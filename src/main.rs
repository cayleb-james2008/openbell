//! openbell-rs — Cayleb's Kalshi Connected Trading & Quant Desk, rebuilt lean in Rust.
//!
//! Venue truth on every number: every figure the app shows comes from
//! Kalshi's own record or is shown as "unavailable". Combines a live Kalshi
//! Trade API v2 RSA-PSS client gated by fail-closed Hard Risk Rails and an
//! Emergency Kill Switch (always disarmed on boot) with a forward 15-minute
//! collector, paper slot simulator, PAV isotonic recalibration, `$500/day`
//! capital scaling engine, and out-of-sample strategy tournament.

mod analytics;
mod auto_allocator;
mod edge_audit;
mod embedded_chunks;
mod execution_audit;
mod gzip_inflate;
mod kalshi_live;
mod open;
mod orderbook;
mod recalibration;
mod risk_rails;
mod scaling;
mod shadow_runner;
mod spot_feed;
mod strategy_v2;
mod synthesizer;
mod tournament;
mod update;

use serde_json::json;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, LazyLock, Mutex};

const INDEX_HTML_GZ: &[u8] = include_bytes!("../assets/index.html.gz");

fn index_html_str() -> &'static str {
    static INDEX_HTML_RAW: LazyLock<String> = LazyLock::new(|| {
        let bytes = gzip_inflate::inflate_gzip(INDEX_HTML_GZ).expect("embedded index.html.gz must inflate cleanly");
        String::from_utf8(bytes).expect("embedded index.html must be UTF-8")
    });
    INDEX_HTML_RAW.as_str()
}

#[inline]
fn index_etag() -> &'static str {
    embedded_chunks::INDEX_HTML_ETAG
}

const SERIES: [&str; 5] = ["KXBTC15M", "KXETH15M", "KXSOL15M", "KXXRP15M", "KXDOGE15M"];
const API_BASE: &str = "https://api.elections.kalshi.com/trade-api/v2";

// ---------------------------------------------------------------------------
// Time helpers (no chrono dependency — Kalshi timestamps are ISO-8601 Zulu)
// ---------------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn parse_iso_z(s: &str) -> Option<i64> {
    let t = s.get(..19)?;
    let (date, time) = t.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
    );
    let mut h = time.split(':');
    let (hh, mm, ss) = (
        h.next()?.parse::<i64>().ok()?,
        h.next()?.parse::<i64>().ok()?,
        h.next()?.parse::<i64>().ok()?,
    );
    // Range-checked: an unchecked parse computes a confident epoch
    // for "2026-13-45T99:99:99Z", and a wrong epoch silently
    // reorders the compounding.
    if !(1..=12).contains(&mo) || !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in_month = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(mo - 1) as usize];
    if da < 1 || da > days_in_month {
        return None;
    }
    Some(days_from_civil(y, mo, da) * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Format an epoch as ISO-8601 Z (for the report's as-of line).
fn iso_z(epoch: i64) -> String {
    let days = epoch.div_euclid(86400);
    let secs = epoch.rem_euclid(86400);
    // civil-from-days (Howard Hinnant's algorithm, inverse of
    // days_from_civil above).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Snapshots and the forward collector (public market data — no auth)
// ---------------------------------------------------------------------------

mod sym_intern {
    use serde::{de::Visitor, Deserializer, Serializer};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ops::Deref;
    use std::sync::{Arc, LazyLock, RwLock};

    #[inline]
    fn fast_id(s: &str) -> Option<u32> {
        Some(match s {
            "" => 0,
            "yes" => 1,
            "no" => 2,
            "scalar" => 3,
            "active" => 4,
            "open" => 5,
            "closed" => 6,
            "finalized" => 7,
            "settled" => 8,
            "initialized" => 9,
            "KXBTC15M" => 10,
            "KXETH15M" => 11,
            "KXSOL15M" => 12,
            "KXXRP15M" => 13,
            "KXDOGE15M" => 14,
            "KXBTC" => 15,
            _ => return None,
        })
    }

    #[inline]
    fn fast_str(id: u32) -> Option<&'static str> {
        Some(match id {
            0 => "",
            1 => "yes",
            2 => "no",
            3 => "scalar",
            4 => "active",
            5 => "open",
            6 => "closed",
            7 => "finalized",
            8 => "settled",
            9 => "initialized",
            10 => "KXBTC15M",
            11 => "KXETH15M",
            12 => "KXSOL15M",
            13 => "KXXRP15M",
            14 => "KXDOGE15M",
            15 => "KXBTC",
            _ => return None,
        })
    }

    struct SymTable {
        vec: Vec<&'static str>,
        map: HashMap<&'static str, u32>,
    }

    static TABLE: LazyLock<RwLock<SymTable>> = LazyLock::new(|| {
        RwLock::new(SymTable {
            vec: Vec::with_capacity(4096),
            map: HashMap::with_capacity(4096),
        })
    });

    thread_local! {
        static LOCAL_POOL: RefCell<Option<HashMap<&'static str, Sym>>> = const { RefCell::new(None) };
    }

    pub struct TapePoolGuard;

    impl TapePoolGuard {
        pub fn enter(cap: usize) -> Self {
            LOCAL_POOL.with(|c| {
                *c.borrow_mut() = Some(HashMap::with_capacity(cap));
            });
            Self
        }
    }

    impl Drop for TapePoolGuard {
        fn drop(&mut self) {
            LOCAL_POOL.with(|c| {
                *c.borrow_mut() = None;
            });
        }
    }

    #[derive(Copy, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct Sym(u32);

    impl Sym {
        #[inline]
        pub(crate) fn new(s: &str) -> Self {
            if let Some(id) = fast_id(s) {
                return Self(id);
            }
            let hit = LOCAL_POOL.with(|c| {
                c.borrow()
                    .as_ref()
                    .and_then(|pool| pool.get(s).copied())
            });
            if let Some(sym) = hit {
                return sym;
            }
            let sym = {
                let r = TABLE.read().unwrap_or_else(|p| p.into_inner());
                if let Some(&id) = r.map.get(s) {
                    Self(id)
                } else {
                    drop(r);
                    let mut w = TABLE.write().unwrap_or_else(|p| p.into_inner());
                    if let Some(&id) = w.map.get(s) {
                        Self(id)
                    } else {
                        let id = 16u32 + (w.vec.len() as u32);
                        let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
                        w.vec.push(leaked);
                        w.map.insert(leaked, id);
                        Self(id)
                    }
                }
            };
            LOCAL_POOL.with(|c| {
                if let Some(pool) = c.borrow_mut().as_mut() {
                    if pool.len() < 32_768 {
                        pool.insert(sym.as_str(), sym);
                    }
                }
            });
            sym
        }

        #[inline]
        pub(crate) fn as_str(&self) -> &'static str {
            if let Some(s) = fast_str(self.0) {
                return s;
            }
            let r = TABLE.read().unwrap_or_else(|p| p.into_inner());
            r.vec[(self.0 - 16) as usize]
        }

        #[inline]
        pub(crate) fn is_empty(&self) -> bool {
            self.0 == 0
        }
    }

    impl Deref for Sym {
        type Target = str;
        #[inline]
        fn deref(&self) -> &str {
            self.as_str()
        }
    }

    impl AsRef<str> for Sym {
        #[inline]
        fn as_ref(&self) -> &str {
            self.as_str()
        }
    }

    impl PartialOrd for Sym {
        #[inline]
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for Sym {
        #[inline]
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            if self.0 == other.0 {
                std::cmp::Ordering::Equal
            } else {
                self.as_str().cmp(other.as_str())
            }
        }
    }

    impl std::fmt::Debug for Sym {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            std::fmt::Debug::fmt(self.as_str(), f)
        }
    }

    impl std::fmt::Display for Sym {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.as_str())
        }
    }

    impl From<&str> for Sym {
        #[inline]
        fn from(s: &str) -> Self {
            Self::new(s)
        }
    }

    impl From<String> for Sym {
        #[inline]
        fn from(s: String) -> Self {
            Self::new(&s)
        }
    }

    impl From<Arc<str>> for Sym {
        #[inline]
        fn from(s: Arc<str>) -> Self {
            Self::new(&s)
        }
    }

    impl PartialEq<str> for Sym {
        #[inline]
        fn eq(&self, other: &str) -> bool {
            self.as_str() == other
        }
    }

    impl PartialEq<&str> for Sym {
        #[inline]
        fn eq(&self, other: &&str) -> bool {
            self.as_str() == *other
        }
    }

    impl PartialEq<String> for Sym {
        #[inline]
        fn eq(&self, other: &String) -> bool {
            self.as_str() == other.as_str()
        }
    }

    impl serde::Serialize for Sym {
        #[inline]
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(self.as_str())
        }
    }

    impl<'de> serde::Deserialize<'de> for Sym {
        #[inline]
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Sym;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a string")
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Sym, E> {
                    Ok(Sym::new(v))
                }
            }
            d.deserialize_str(V)
        }
    }
}
use sym_intern::{Sym, TapePoolGuard};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct OptPrice(u32);

impl OptPrice {
    const NONE: Self = Self(u32::MAX);

    #[inline]
    fn from_opt(v: Option<f64>) -> Self {
        let Some(x) = v else {
            return Self::NONE;
        };
        if !x.is_finite() {
            return Self::NONE;
        }
        if (0.0..=100.0).contains(&x) {
            let c = (x * 100.0).round();
            if (c / 100.0).to_bits() == x.to_bits() {
                return Self(c as u32);
            }
        }
        let bits = (x as f32).to_bits();
        let encoded = (bits >> 1) | 0x8000_0000;
        if encoded == u32::MAX {
            Self(u32::MAX - 1)
        } else {
            Self(encoded)
        }
    }

    #[inline]
    fn get(self) -> Option<f64> {
        if self.0 == u32::MAX {
            None
        } else if self.0 <= 10_000 {
            Some((self.0 as f64) / 100.0)
        } else {
            Some(f32::from_bits(self.0 << 1) as f64)
        }
    }
}

impl Serialize for OptPrice {
    #[inline]
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(s)
    }
}

impl<'de> Deserialize<'de> for OptPrice {
    #[inline]
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Option::<f64>::deserialize(d).map(Self::from_opt)
    }
}

#[inline]
fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    fetched_at: i64,
    ticker: Sym,
    event_ticker: Sym,
    series: Sym,
    close_time: Sym,
    status: Sym,
    result: Sym,
    yes_bid: OptPrice,
    yes_ask: OptPrice,
    no_bid: OptPrice,
    no_ask: OptPrice,
    last_price: OptPrice,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    yes_bid_size: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    no_bid_size: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    floor_strike: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cap_strike: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spot_price: Option<f64>,
}

fn opt_finite_f64(v: &serde_json::Value, key: &str, positive_only: bool) -> Option<f64> {
    v.get(key)
        .and_then(|x| {
            x.as_f64()
                .or_else(|| x.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        })
        .filter(|n| n.is_finite() && (!positive_only || *n > 0.0))
}

fn dollars(v: &serde_json::Value, key: &str) -> OptPrice {
    OptPrice::from_opt(v.get(key).and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()))
}

/// Parse top-of-book contract depth from Kalshi public market JSON (string `*_fp`
/// or numeric fields), returning finite non-negative depth or `0.0` when absent.
fn depth_size(v: &serde_json::Value, keys: &[&str]) -> f32 {
    for &key in keys {
        let parsed = v.get(key).and_then(|x| {
            x.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| x.as_f64())
        });
        if let Some(n) = parsed {
            if n.is_finite() && n >= 0.0 {
                return n as f32;
            }
        }
    }
    0.0
}

#[cfg(test)]
fn median_observed_book_depth(snaps: &[Snapshot]) -> Option<f64> {
    median_observed_book_depth_iter(snaps.iter())
}

fn median_observed_book_depth_grouped(grouped: &BTreeMap<String, Vec<Snapshot>>) -> Option<f64> {
    median_observed_book_depth_iter(grouped.values().flatten())
}

fn median_observed_book_depth_iter<'a, I: Iterator<Item = &'a Snapshot>>(iter: I) -> Option<f64> {
    let mut depths: Vec<f32> = Vec::new();
    for s in iter {
        if s.yes_bid_size.is_finite() && s.yes_bid_size > 0.0 {
            depths.push(s.yes_bid_size);
        }
        if s.no_bid_size.is_finite() && s.no_bid_size > 0.0 {
            depths.push(s.no_bid_size);
        }
    }
    if depths.is_empty() {
        return None;
    }
    depths.sort_unstable_by(|a, b| a.total_cmp(b));
    let mid = depths.len() / 2;
    if depths.len() % 2 == 1 {
        Some(depths[mid] as f64)
    } else {
        Some(((depths[mid - 1] as f64) + (depths[mid] as f64)) * 0.5)
    }
}

fn active_series() -> Vec<String> {
    if let Ok(raw) = std::env::var("OPENBELL_SERIES") {
        let mut parsed: Vec<String> = Vec::new();
        for tok in raw.split(',') {
            let s = tok.trim().to_ascii_uppercase();
            if !s.is_empty()
                && s.len() <= 32
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !parsed.contains(&s)
            {
                parsed.push(s);
            }
        }
        if !parsed.is_empty() {
            return parsed;
        }
    }
    SERIES.iter().map(|s| (*s).to_string()).collect()
}

fn markets_api_base() -> String {
    std::env::var("OPENBELL_API_BASE")
        .ok()
        .and_then(|u| kalshi_live::validate_api_base_url(&u).ok())
        .unwrap_or_else(|| API_BASE.to_string())
}

fn fetch_markets(series: &str, status: &str) -> Result<(Vec<Snapshot>, usize, bool), String> {
    let base = markets_api_base();
    let url = format!("{base}/markets?series_ticker={series}&status={status}&limit=100");
    let mut last_err = "venue response had no markets array".to_string();
    let mut hit_rate_limit = false;
    for attempt in 0..4u32 {
        let out = Command::new("curl")
            // -q first: never read the user's ~/.curlrc.
            .arg("-q")
            .args(["-s", "-m", "25", &url])
            .output()
            .map_err(|e| format!("curl failed: {e}"))?;
        if !out.status.success() {
            return Err(format!("curl exited {}", out.status));
        }
        let body: serde_json::Value = match serde_json::from_slice(&out.stdout) {
            Ok(v) => v,
            Err(e) => {
                let text = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
                if text.contains("429") || text.contains("too many requests") || text.contains("too_many_requests") {
                    hit_rate_limit = true;
                    last_err = "venue returned HTTP 429 (too_many_requests)".to_string();
                    if attempt < 3 {
                        let wait_ms = 750u64 << attempt;
                        std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                        continue;
                    }
                    return Err(last_err);
                }
                return Err(format!("venue returned non-JSON: {e}"));
            }
        };
        let fetched_at = now_unix();
        // A response without a markets array is not market data — the
        // venue's error envelope is valid JSON with an "error" key, and
        // treating it as an empty list would report a dead venue as a
        // successful, empty collection. Rate-limit envelopes retry with
        // bounded backoff so shared outbound IPs still complete a full pass.
        let Some(markets) = body.get("markets").and_then(|m| m.as_array()) else {
            let code = body
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if code == "too_many_requests" {
                hit_rate_limit = true;
                last_err = "venue response had no markets array (too_many_requests)".to_string();
                if attempt < 3 {
                    let wait_ms = 750u64 << attempt;
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                    continue;
                }
            }
            return Err(last_err);
        };
        let mut snaps: Vec<Snapshot> = Vec::new();
        let mut skipped = 0usize;
        for m in markets {
            let get = |k: &str| m.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            // A record that cannot be a snapshot is not written to
            // the permanent tape: no ticker, a close time that does
            // not parse (it would score at epoch 0 as a settled
            // window that never existed), or a result outside the
            // venue's vocabulary. Skips are counted and reported by
            // the caller, never silent.
            let result = get("result");
            if get("ticker").is_empty()
                || parse_iso_z(&get("close_time")).is_none()
                || !matches!(result.as_str(), "" | "yes" | "no")
            {
                skipped += 1;
                continue;
            }
            snaps.push(Snapshot {
                fetched_at,
                ticker: get("ticker").into(),
                event_ticker: get("event_ticker").into(),
                series: Sym::new(series),
                close_time: get("close_time").into(),
                status: get("status").into(),
                result: get("result").into(),
                yes_bid: dollars(m, "yes_bid_dollars"),
                yes_ask: dollars(m, "yes_ask_dollars"),
                no_bid: dollars(m, "no_bid_dollars"),
                no_ask: dollars(m, "no_ask_dollars"),
                last_price: dollars(m, "last_price_dollars"),
                yes_bid_size: depth_size(m, &["yes_bid_size_fp", "yes_bid_size"]),
                no_bid_size: depth_size(m, &["no_bid_size_fp", "no_bid_size", "yes_ask_size_fp"]),
                floor_strike: opt_finite_f64(m, "floor_strike", false),
                cap_strike: opt_finite_f64(m, "cap_strike", false),
                spot_price: opt_finite_f64(m, "spot_price", true),
            });
        }
        return Ok((snaps, skipped, hit_rate_limit));
    }
    Err(last_err)
}

/// Bounded in-process SSE broadcast hub for live collector events.
const MAX_SSE_CLIENTS: usize = 64;

struct TapeHub {
    senders: std::sync::Mutex<Vec<std::sync::mpsc::SyncSender<(String, String)>>>,
}

impl TapeHub {
    fn new() -> Self {
        Self {
            senders: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<(String, String)>> {
        let mut guard = self.senders.lock().unwrap_or_else(|p| p.into_inner());
        // Drop closed subscribers before enforcing the concurrent cap.
        guard.retain(|tx| {
            !matches!(
                tx.try_send((String::new(), String::new())),
                Err(std::sync::mpsc::TrySendError::Disconnected(_))
            )
        });
        if guard.len() >= MAX_SSE_CLIENTS {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        guard.push(tx);
        Some(rx)
    }

    fn publish_event(&self, event_name: &str, payload: String) {
        let ev = event_name.to_string();
        let mut guard = self.senders.lock().unwrap_or_else(|p| p.into_inner());
        guard.retain(|tx| {
            !matches!(
                tx.try_send((ev.clone(), payload.clone())),
                Err(std::sync::mpsc::TrySendError::Disconnected(_))
            )
        });
    }

    fn publish(&self, payload: String) {
        self.publish_event("tape", payload);
    }

    fn publish_runner(&self, payload: String) {
        self.publish_event("runner", payload);
    }
}

static TAPE_HUB: std::sync::OnceLock<TapeHub> = std::sync::OnceLock::new();

fn tape_hub() -> &'static TapeHub {
    TAPE_HUB.get_or_init(TapeHub::new)
}

type SettledKey = (Sym, Sym, Sym);
type SettledKeysCacheEntry = (PathBuf, u64, u64, std::collections::HashSet<SettledKey>);
static SETTLED_KEYS_CACHE: std::sync::Mutex<Option<SettledKeysCacheEntry>> =
    std::sync::Mutex::new(None);

/// Already-recorded settled snapshot keys on disk `(ticker, status, result)` so
/// repeat passes do not duplicate hundreds of immutable historical settlements.
/// Cached by `(path, len, mtime)` so 100k-line tapes are not re-parsed every pass.
fn existing_settled_keys(path: &Path) -> std::collections::HashSet<SettledKey> {
    let stat = tape_stat(path);
    if let Some((len, mtime)) = stat {
        let guard = SETTLED_KEYS_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((ref cp, clen, cmtime, ref cached)) = *guard {
            if cp == path && clen == len && cmtime == mtime {
                return cached.clone();
            }
        }
    }
    let mut keys = std::collections::HashSet::new();
    let Ok(file) = std::fs::File::open(path) else {
        return keys;
    };
    use std::io::BufRead as _;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::with_capacity(512);
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let raw = line.strip_suffix(b"\n").unwrap_or(&line);
                if let Ok(s) = serde_json::from_slice::<Snapshot>(raw) {
                    if !s.result.is_empty() {
                        keys.insert((s.ticker, s.status, s.result));
                    }
                }
            }
        }
    }
    if let Some((len, mtime)) = stat {
        *SETTLED_KEYS_CACHE.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((path.to_path_buf(), len, mtime, keys.clone()));
    }
    keys
}

/// One collector pass over every series the desk trades: open markets (the
/// forward tape) plus settled markets (so windows get their results).
/// Appends JSONL snapshots to disk. Returns how many snapshots were written.
/// One collection pass at a time, across every caller (the
/// background loop and POST /api/collect): concurrent passes each
/// hold their own append handle, and their writes interleave into
/// spliced, unparseable tape lines.
static COLLECT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Series fetches that failed on the most recent pass: the count the
/// pass logs as "series failed", stated on the health block and report.
static SERIES_FAILED_LAST_PASS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
/// The series tickers behind that count, in SERIES order, each once.
static SERIES_FAILED_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

const AUTO_COLLECT_MIN_SECS: u64 = 30;
const AUTO_COLLECT_MAX_SECS: u64 = 900;
const READINESS_TARGET_WINDOWS: usize = 200;

struct CollectPassReport {
    written: usize,
    deduplicated: usize,
    errors: Vec<String>,
    rate_limited: bool,
}

#[derive(Clone, Debug)]
struct CollectorState {
    enabled: bool,
    interval_secs: u64,
    backoff_secs: u64,
    rate_limited: bool,
    consecutive_errors: u32,
    in_flight: bool,
    runs_total: u64,
    last_run_epoch: Option<i64>,
    next_run_epoch: Option<i64>,
    last_written: usize,
    last_deduped: usize,
    last_error: Option<String>,
    generation: u64,
}

impl CollectorState {
    fn new(enabled: bool, interval_secs: u64) -> Self {
        let clamped = interval_secs.clamp(AUTO_COLLECT_MIN_SECS, AUTO_COLLECT_MAX_SECS);
        Self {
            enabled,
            interval_secs: clamped,
            backoff_secs: 0,
            rate_limited: false,
            consecutive_errors: 0,
            in_flight: false,
            runs_total: 0,
            last_run_epoch: None,
            next_run_epoch: if enabled { Some(now_unix()) } else { None },
            last_written: 0,
            last_deduped: 0,
            last_error: None,
            generation: 0,
        }
    }

    fn effective_interval_secs(&self) -> u64 {
        self.interval_secs
            .saturating_add(self.backoff_secs)
            .clamp(AUTO_COLLECT_MIN_SECS, AUTO_COLLECT_MAX_SECS)
    }
}

/// Adaptive backoff policy: doubles the wait on HTTP 429 / `too_many_requests`
/// (up to `AUTO_COLLECT_MAX_SECS`), applies bounded backoff on general errors,
/// and resets immediately to `0` on a clean pass.
fn compute_adaptive_backoff(interval_secs: u64, consecutive_errors: u32, rate_limited: bool) -> u64 {
    if consecutive_errors == 0 {
        return 0;
    }
    let base = interval_secs.clamp(AUTO_COLLECT_MIN_SECS, AUTO_COLLECT_MAX_SECS);
    let max_extra = AUTO_COLLECT_MAX_SECS.saturating_sub(base);
    if max_extra == 0 {
        return 0;
    }
    let step = (consecutive_errors - 1).min(4);
    let extra = if rate_limited {
        // Effective interval: 2x, 4x, 8x... capped at 900s.
        let target = base.saturating_mul(2u64 << step).min(AUTO_COLLECT_MAX_SECS);
        target.saturating_sub(base)
    } else {
        (30u64 << step).min(max_extra)
    };
    extra.min(max_extra)
}

struct CollectorScheduler {
    state: std::sync::Mutex<CollectorState>,
    cv: std::sync::Condvar,
}

impl CollectorScheduler {
    fn new(enabled: bool, interval_secs: u64) -> Self {
        Self {
            state: std::sync::Mutex::new(CollectorState::new(enabled, interval_secs)),
            cv: std::sync::Condvar::new(),
        }
    }

    fn snapshot(&self) -> CollectorState {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn update_schedule(&self, enabled: Option<bool>, interval_secs: Option<u64>) -> CollectorState {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let was_enabled = st.enabled;
        if let Some(iv) = interval_secs {
            st.interval_secs = iv.clamp(AUTO_COLLECT_MIN_SECS, AUTO_COLLECT_MAX_SECS);
            // Re-cap existing backoff under the updated base interval.
            st.backoff_secs = compute_adaptive_backoff(
                st.interval_secs,
                st.consecutive_errors,
                st.rate_limited,
            );
        }
        if let Some(en) = enabled {
            st.enabled = en;
        }
        let now = now_unix();
        if !st.enabled {
            st.next_run_epoch = None;
            st.backoff_secs = 0;
            st.rate_limited = false;
            st.consecutive_errors = 0;
        } else {
            let eff = st.effective_interval_secs() as i64;
            let due_at = match st.last_run_epoch {
                Some(last) if now - last < eff && (!was_enabled || interval_secs.is_some()) => {
                    last + eff
                }
                Some(last) if now - last < eff => last + eff,
                _ => now,
            };
            st.next_run_epoch = Some(due_at);
        }
        st.generation = st.generation.wrapping_add(1);
        let out = st.clone();
        self.cv.notify_all();
        out
    }

    fn mark_in_flight(&self, active: bool) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.in_flight = active;
    }

    fn record_pass_result(&self, res: &Result<CollectPassReport, (String, bool)>) -> CollectorState {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let now = now_unix();
        st.in_flight = false;
        st.last_run_epoch = Some(now);
        st.runs_total = st.runs_total.saturating_add(1);
        match res {
            Ok(rep) => {
                st.last_written = rep.written;
                st.last_deduped = rep.deduplicated;
                if rep.rate_limited {
                    st.rate_limited = true;
                    st.consecutive_errors = st.consecutive_errors.saturating_add(1).min(8);
                    st.backoff_secs =
                        compute_adaptive_backoff(st.interval_secs, st.consecutive_errors, true);
                    st.last_error = if rep.errors.is_empty() {
                        Some("venue rate limited (HTTP 429) — adaptive backoff active".to_string())
                    } else {
                        Some(rep.errors.join("; "))
                    };
                } else {
                    st.rate_limited = false;
                    st.consecutive_errors = 0;
                    st.backoff_secs = 0;
                    st.last_error = if rep.errors.is_empty() {
                        None
                    } else {
                        Some(rep.errors.join("; "))
                    };
                }
            }
            Err((err, rate_limited)) => {
                st.last_written = 0;
                st.last_deduped = 0;
                st.rate_limited = *rate_limited;
                st.consecutive_errors = st.consecutive_errors.saturating_add(1).min(8);
                st.backoff_secs = compute_adaptive_backoff(
                    st.interval_secs,
                    st.consecutive_errors,
                    *rate_limited,
                );
                st.last_error = Some(err.clone());
            }
        }
        st.next_run_epoch = if st.enabled {
            Some(now + st.effective_interval_secs() as i64)
        } else {
            None
        };
        st.clone()
    }
}

fn prioritized_open_tickers(snaps: &[Snapshot]) -> Vec<String> {
    let mut ranked: Vec<(i64, String)> = snaps
        .iter()
        .filter(|s| s.result.is_empty() && matches!(s.status.as_str(), "open" | "active"))
        .map(|s| {
            let close_ep = parse_iso_z(s.close_time.as_str()).unwrap_or(i64::MAX);
            (close_ep, s.ticker.as_str().to_string())
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut out: Vec<String> = Vec::with_capacity(ranked.len());
    for (_, t) in ranked {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Run one pass with rate-limit telemetry. Strictly public read-only GETs only.
fn collect_once_detailed(
    data_dir: &Path,
    orderbook: Option<&orderbook::OrderbookStore>,
    spot_feed: Option<&spot_feed::SpotFeedStore>,
) -> Result<CollectPassReport, (String, bool)> {
    let _guard = COLLECT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _pass = PassGuard::new();
    let _ = std::fs::create_dir_all(data_dir);
    let path = data_dir.join("snapshots.jsonl");
    let series_list = active_series();
    let existing_settled = existing_settled_keys(&path);
    let mut pass_snaps: Vec<Snapshot> = Vec::new();
    let mut seen_in_pass: std::collections::HashSet<SettledKey> =
        std::collections::HashSet::new();
    let mut errors: Vec<String> = Vec::new();
    let mut failed_series: Vec<String> = Vec::new();
    let mut any_rate_limited = false;
    let mut pace_ms: u64 = std::env::var("OPENBELL_COLLECT_PACE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(250)
        .min(5000);
    let mut call_idx = 0usize;
    for series in &series_list {
        for status in ["open", "settled"] {
            if call_idx > 0 && pace_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(pace_ms));
            }
            call_idx += 1;
            match fetch_markets(series, status) {
                Ok((snaps, skipped, rate_limited)) => {
                    if rate_limited {
                        any_rate_limited = true;
                        if pace_ms > 0 {
                            pace_ms = (pace_ms.saturating_mul(2)).clamp(400, 1200);
                        }
                    }
                    if skipped > 0 {
                        errors.push(format!(
                            "{series}/{status}: skipped {skipped} invalid record(s)"
                        ));
                        if !failed_series.iter().any(|f| f == series) {
                            failed_series.push(series.clone());
                        }
                    }
                    for mut snap in snaps {
                        if snap.spot_price.is_none() {
                            if let Some(sf) = spot_feed {
                                if let Some(st) = sf.latest_fresh_for_series(snap.series.as_str()) {
                                    snap.spot_price = st.price;
                                }
                            }
                        }
                        let key = (snap.ticker, snap.status, snap.result);
                        if seen_in_pass.insert(key) {
                            pass_snaps.push(snap);
                        }
                    }
                }
                Err(e) => {
                    if e.contains("too_many_requests") || e.contains("429") {
                        any_rate_limited = true;
                    }
                    eprintln!("collect {series}/{status}: {e}");
                    errors.push(format!("{series}/{status}: {e}"));
                    if !failed_series.iter().any(|f| f == series) {
                        failed_series.push(series.clone());
                    }
                }
            }
        }
    }
    SERIES_FAILED_LAST_PASS.store(errors.len(), std::sync::atomic::Ordering::Relaxed);
    *SERIES_FAILED_NAMES.lock().unwrap_or_else(|p| p.into_inner()) = failed_series;

    // Cross-pass deduplication: skip immutable settled rows already on disk whenever
    // this pass has at least one fresh open or newly-settled record, keeping the tape
    // lean while still ensuring a heartbeat write when only settled records exist.
    let dedup_enabled = std::env::var("OPENBELL_DEDUP_SETTLED").as_deref() != Ok("0");
    let mut fresh_snaps: Vec<&Snapshot> = Vec::new();
    let mut deduplicated = 0usize;
    for snap in &pass_snaps {
        if dedup_enabled
            && !snap.result.is_empty()
            && existing_settled.contains(&(snap.ticker, snap.status, snap.result))
        {
            deduplicated += 1;
        } else {
            fresh_snaps.push(snap);
        }
    }
    let to_write: Vec<&Snapshot> = if !fresh_snaps.is_empty() {
        fresh_snaps
    } else {
        deduplicated = 0;
        pass_snaps.iter().collect()
    };

    // Build the whole pass in memory and append it in one write: a
    // reader of the tape (the report, the health count) then sees the
    // pass whole or not at all, never a torn half-pass.
    let mut buf = String::new();
    let mut written = 0usize;
    for snap in &to_write {
        buf.push_str(&serde_json::to_string(snap).unwrap());
        buf.push('\n');
        written += 1;
    }
    if written == 0 {
        let msg = if errors.is_empty() {
            "collection wrote nothing — every series returned empty".to_string()
        } else {
            format!(
                "collection failed on every series: {}",
                errors.join("; ")
            )
        };
        return Err((msg, any_rate_limited));
    }
    append_tape(&path, &buf).map_err(|e| (e.to_string(), any_rate_limited))?;
    if let Some(ob) = orderbook {
        let open_tickers = prioritized_open_tickers(&pass_snaps);
        if !open_tickers.is_empty() {
            ob.sync_active_markets(&open_tickers, &[]);
        }
    }
    let event = format!(
        "{{\"type\":\"collect\",\"snapshots_written\":{written},\"deduplicated\":{deduplicated},\"series_failed\":{},\"rate_limited\":{any_rate_limited},\"ts\":{}}}",
        errors.len(),
        now_unix(),
    );
    tape_hub().publish(event);
    Ok(CollectPassReport {
        written,
        deduplicated,
        errors,
        rate_limited: any_rate_limited,
    })
}

/// Run one pass. Ok carries (snapshots written, deduplicated count, per-series errors);
/// a pass that wrote nothing BECAUSE everything failed is Err, so
/// the API can say so instead of returning a cheerful 200 with 0.
#[allow(dead_code)]
fn collect_once(data_dir: &Path) -> Result<(usize, usize, Vec<String>), String> {
    collect_once_detailed(data_dir, None, None)
        .map(|r| (r.written, r.deduplicated, r.errors))
        .map_err(|(e, _)| e)
}

/// Append one pass in a single write. A tape that ends in a torn line
/// (no final newline, left by a crash mid-append) gets a newline first,
/// so the torn fragment stays its own line and the pass is not spliced
/// onto it.
fn append_tape(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut torn = false;
    if let Ok(mut f) = std::fs::File::open(path) {
        if f.metadata()?.len() > 0 {
            f.seek(SeekFrom::End(-1))?;
            let mut last = [0u8; 1];
            f.read_exact(&mut last)?;
            torn = last[0] != b'\n';
        }
    }
    let mut out = String::new();
    if torn {
        out.push('\n');
    }
    out.push_str(body);
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(out.as_bytes())
}

/// Parse a tape as bytes, one record per line. A line that is not
/// valid UTF-8 or not a snapshot is dropped and counted in the line
/// total, exactly like any malformed line; it never hides its
/// neighbours. Returns (snapshots, lines in the file).
fn parse_tape_bytes(bytes: &[u8]) -> (Vec<Snapshot>, usize) {
    let trimmed = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if trimmed.is_empty() {
        return (Vec::new(), 0);
    }
    let est = (trimmed.len() / 200).max(16);
    let mut snaps = Vec::with_capacity(est);
    let mut total_lines = 0usize;
    for line in trimmed.split(|&b| b == b'\n') {
        total_lines += 1;
        if let Ok(s) = serde_json::from_slice::<Snapshot>(line) {
            snaps.push(s);
        }
    }
    (snaps, total_lines)
}

// ---------------------------------------------------------------------------
// Shadow scorer — Rune strategies, promotion standard as code (paper only)
// ---------------------------------------------------------------------------

/// The desk's promotion standard. A strategy is proven only on forward
/// evidence: enough settled windows, and BOTH chronological halves of the
/// record positive, at or above the per-contract bar.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Standard {
    min_windows: usize,
    min_mean_cents: f64,
}

impl Default for Standard {
    fn default() -> Self {
        Standard {
            min_windows: 100,
            min_mean_cents: 3.5,
        }
    }
}

/// Kalshi-style fee model used for shadow scoring, in dollars per contract.
fn fee(price: f64) -> f64 {
    0.07 * price * (1.0 - price)
}

struct Strategy {
    name: String,
    side: String,
    /// Content hash of the strategy's source bytes. Two loads of the
    /// same source share it; any edit changes it. The per-window
    /// entry index (window_entry) keys on it, so a mutated strategy
    /// can never inherit entries computed from different code.
    fingerprint: u64,
    unit: Arc<rune::Unit>,
    runtime: Arc<rune::runtime::RuntimeContext>,
    /// Time this strategy's own calls have consumed this pass —
    /// the aggregate deadline meters THIS, never the pass's wall
    /// clock (other strategies' calls interleave in a pass, and
    /// their burn is not this strategy's to spend).
    spent: std::cell::Cell<std::time::Duration>,
    /// Set once the aggregate allowance is spent: later calls in
    /// the pass degrade to no-entry without running the script.
    exhausted: std::cell::Cell<bool>,
    /// allow_entry calls that errored or returned a non-boolean:
    /// counted so a broken strategy reads as broken, not quiet.
    call_failures: std::cell::Cell<usize>,
    /// The strategy's virtual machine, reused across calls: a fresh
    /// Vm per call pays a full stack allocation per call, and over
    /// a full tape (~100k calls per strategy) that overhead — not
    /// the scripts — is what pushes honest strategies into their
    /// aggregate deadline and leaves their tail windows unscored.
    /// One Vm executes every call this strategy makes; the budget
    /// and memory limit wrap each call independently of it.
    vm: std::cell::RefCell<Option<rune::Vm>>,
    /// Consecutive calls that died on the instruction budget (see
    /// budget_open): direct evidence a script does not terminate,
    /// as opposed to a script that is merely slow.
    budget_deaths: std::cell::Cell<usize>,
    /// Cached probe of whether this Rune unit exports `entry_quote`.
    has_entry_quote: std::cell::Cell<Option<bool>>,
    /// Cached probe of whether this Rune unit exports v2 `evaluate(ctx)`.
    has_evaluate: std::cell::Cell<Option<bool>>,
}

static SHARED_RUNTIME: std::sync::Mutex<Option<std::sync::Weak<rune::runtime::RuntimeContext>>> =
    std::sync::Mutex::new(None);

thread_local! {
    static ACTIVE_COMPILE_FLAG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ACTIVE_COMPILE_CTX: std::cell::RefCell<
        Option<(rune::Context, Arc<rune::runtime::RuntimeContext>)>,
    > = const { std::cell::RefCell::new(None) };
}

struct CompileCtxGuard {
    prev: bool,
}

impl CompileCtxGuard {
    fn enter() -> Self {
        let prev = ACTIVE_COMPILE_FLAG.with(|f| f.replace(true));
        Self { prev }
    }
}

impl Drop for CompileCtxGuard {
    fn drop(&mut self) {
        ACTIVE_COMPILE_FLAG.with(|f| f.set(self.prev));
        if !self.prev {
            let had_ctx = ACTIVE_COMPILE_CTX.with(|c| c.borrow_mut().take().is_some());
            if had_ctx {
                trim_heap();
            }
        }
    }
}

impl Strategy {
    fn load(path: &Path) -> Result<Strategy, String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err("refusing symlink or non-regular strategy file".to_string());
        }
        let script = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            script.hash(&mut h);
            h.finish()
        };
        if script.len() > MAX_STRATEGY_BYTES {
            return Err(format!(
                "strategy file is {} bytes; the cap is {MAX_STRATEGY_BYTES}",
                script.len()
            ));
        }
        // Compiled-strategy cache keyed by (path, length, mtime):
        // compiling Rune from source on every request made every
        // surface pay it per hit. The Strategy handed out is always
        // fresh (its budget meter and failure counters are per
        // pass); only the immutable compile is shared.
        let cache_key = Some((
            script.len() as u64,
            meta.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        ));
        if let Some((len, mtime)) = cache_key {
            let guard = STRAT_CACHE.lock().unwrap();
            if let Some((_p, _l, _t, unit, runtime, name, side)) =
                guard.iter().find(|(p, l, t, ..)| p == path && *l == len && *t == mtime)
            {
                let (unit, runtime, name, side) = (
                    unit.clone(),
                    runtime.clone(),
                    name.clone(),
                    side.clone(),
                );
                drop(guard);
                return Ok(Strategy {
                    name,
                    side,
                    fingerprint,
                    unit,
                    runtime,
                    spent: std::cell::Cell::new(std::time::Duration::ZERO),
                    exhausted: std::cell::Cell::new(false),
                    call_failures: std::cell::Cell::new(0),
                    vm: std::cell::RefCell::new(None),
                    budget_deaths: std::cell::Cell::new(0),
                    has_entry_quote: std::cell::Cell::new(None),
                    has_evaluate: std::cell::Cell::new(None),
                });
            }
        }
        let (unit, runtime) = {
            let compile_with = |context: &rune::Context,
                                runtime: Arc<rune::runtime::RuntimeContext>|
             -> Result<(Arc<rune::Unit>, Arc<rune::runtime::RuntimeContext>), String> {
                let mut sources = rune::Sources::new();
                sources
                    .insert(rune::Source::memory(&script).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                let mut diagnostics = rune::Diagnostics::new();
                let unit = rune::prepare(&mut sources)
                    .with_context(context)
                    .with_diagnostics(&mut diagnostics)
                    .build()
                    .map_err(|e| {
                        format!(
                            "strategy {} failed to build: {}",
                            path.display(),
                            e.to_string().replace(" (see diagnostics for details)", "")
                        )
                    })?;
                Ok((Arc::new(unit), runtime))
            };
            let shared_rt = |ctx: &rune::Context| -> Result<Arc<rune::runtime::RuntimeContext>, String> {
                let existing = SHARED_RUNTIME
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                    .and_then(|w| w.upgrade());
                match existing {
                    Some(rt) => Ok(rt),
                    None => {
                        let arc = Arc::new(ctx.runtime().map_err(|e| e.to_string())?);
                        *SHARED_RUNTIME.lock().unwrap_or_else(|p| p.into_inner()) =
                            Some(Arc::downgrade(&arc));
                        Ok(arc)
                    }
                }
            };
            if ACTIVE_COMPILE_FLAG.with(|f| f.get()) {
                ACTIVE_COMPILE_CTX.with(|c| {
                    let mut b = c.borrow_mut();
                    if b.is_none() {
                        let ctx = rune::Context::with_default_modules().map_err(|e| e.to_string())?;
                        let rt = shared_rt(&ctx)?;
                        *b = Some((ctx, rt));
                    }
                    let (context, runtime) = b.as_ref().unwrap();
                    compile_with(context, runtime.clone())
                })?
            } else {
                let context = rune::Context::with_default_modules().map_err(|e| e.to_string())?;
                let runtime = shared_rt(&context)?;
                compile_with(&context, runtime)?
            }
        };
        let mut strat = Strategy {
            name: String::new(),
            side: String::new(),
            fingerprint,
            unit,
            runtime,
            spent: std::cell::Cell::new(std::time::Duration::ZERO),
            exhausted: std::cell::Cell::new(false),
            call_failures: std::cell::Cell::new(0),
            vm: std::cell::RefCell::new(None),
            budget_deaths: std::cell::Cell::new(0),
            has_entry_quote: std::cell::Cell::new(None),
            has_evaluate: std::cell::Cell::new(None),
        };
        strat.name = strat.call_string("name")?;
        strat.side = strat.call_string("side")?;
        if strat.side != "yes" && strat.side != "no" {
            return Err(format!(
                "side() must return \"yes\" or \"no\", got \"{}\" — any other value would silently trade the NO side and score it as the other",
                strat.side
            ));
        }
        if let Some((len, mtime)) = cache_key {
            let mut guard = STRAT_CACHE.lock().unwrap();
            guard.retain(|(p, ..)| p != path);
            guard.push((
                path.to_path_buf(),
                len,
                mtime,
                strat.unit.clone(),
                strat.runtime.clone(),
                strat.name.clone(),
                strat.side.clone(),
            ));
        }
        Ok(strat)
    }

    /// Borrow the strategy's reusable Vm (see the field's note).
    fn with_vm<R>(&self, f: impl FnOnce(&mut rune::Vm) -> R) -> R {
        let mut slot = self.vm.borrow_mut();
        let vm = slot
            .get_or_insert_with(|| rune::Vm::new(self.runtime.clone(), self.unit.clone()));
        f(vm)
    }

    /// Every strategy call runs under an instruction budget AND a
    /// memory limit. A strategy is a small predicate over three
    /// numbers; the instruction budget is generous headroom for any
    /// honest shape, and a script that never terminates (a while
    /// loop in allow_entry) burns it and errors — degrading to
    /// no-entry like any other failed call — instead of wedging
    /// every scoring surface forever. The instruction budget alone
    /// cannot bound bytes: a single native call (a format! with a
    /// huge width, a with_capacity reservation) allocates hundreds
    /// of megabytes inside a handful of instructions, so every call
    /// also runs under rune::alloc::limit — a hard cap on the bytes
    /// the call may allocate through Rune's allocator. A breach
    /// errors the call, degrading exactly like any other failure:
    /// counted, noted, treated as no-entry.
    fn call_budgeted(
        &self,
        name: &str,
        args: (f64, f64, i64),
    ) -> Result<rune::Value, rune::runtime::VmError> {
        let t0 = std::time::Instant::now();
        let out = self.with_vm(|vm| {
            rune::runtime::budget::with(
                STRATEGY_BUDGET,
                rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || vm.call([name], args)),
            )
            .call()
        });
        self.spent.set(self.spent.get() + t0.elapsed());
        match &out {
            Ok(_) => self.budget_deaths.set(0),
            Err(e) if is_budget_halt(e) => {
                let deaths = self.budget_deaths.get() + 1;
                self.budget_deaths.set(deaths);
                if deaths >= BUDGET_DEATH_LIMIT {
                    self.exhausted.set(true);
                }
            }
            Err(_) => {}
        }
        out
    }

    fn call_string(&self, name: &str) -> Result<String, String> {
        let value = self
            .with_vm(|vm| {
                rune::runtime::budget::with(
                    STRATEGY_BUDGET,
                    rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || vm.call([name], ())),
                )
                .call()
            })
            .map_err(|e| format!("strategy call {name} failed: {e}"))?;
        rune::from_value::<String>(value).map_err(|e| e.to_string())
    }

    /// The aggregate bound: a strategy gets STRATEGY_DEADLINE_SECS
    /// of its OWN consumed call time per scoring pass (a pass
    /// builds fresh Strategy objects). The allowance exists so a
    /// pathological script's cost is bounded per surface, not by a
    /// neighbour's burn. It is calibrated so an honest predicate
    /// can always finish a full first computation over the whole
    /// tape: at ~750 settled windows a strategy's first pass is
    /// ~100–200k short calls, a few seconds of burn in the worst
    /// case — the 3s this used to be was set on a 108k-line tape
    /// and truncated honest strategies mid-tape, which also left
    /// the per-window entry index permanently incomplete (a
    /// truncated scan is never cached), so every recompute re-burned
    /// the same allowance and the desk's verdicts were computed on
    /// whatever prefix fit. Thirty seconds is ~10x the honest
    /// need; a script that burns it is named in the pass's notes
    /// and treated as no-entry for the rest of the pass, exactly
    /// as before.
    fn budget_open(&self) -> bool {
        if self.exhausted.get() {
            return false;
        }
        if self.spent.get().as_secs() >= STRATEGY_DEADLINE_SECS {
            self.exhausted.set(true);
            return false;
        }
        // The second trigger is evidence, not elapsed time: a call
        // that dies on the instruction budget is a script that did
        // not terminate within 250k instructions for those inputs.
        // One such death can be an outlier input; a run of them is
        // a non-terminating script, and it is exhausted on the spot
        // rather than after STRATEGY_DEADLINE_SECS of aggregate
        // burn — on a small tape the clock alone would never name
        // it, and the pass would pay full price for every window.
        if self.budget_deaths.get() >= BUDGET_DEATH_LIMIT {
            self.exhausted.set(true);
            return false;
        }
        true
    }

    fn budget_exhausted(&self) -> bool {
        self.exhausted.get()
    }

    fn call_failures(&self) -> usize {
        self.call_failures.get()
    }

    fn allow_entry(&self, yes_ask: f64, no_ask: f64, secs_to_close: i64) -> bool {
        if !self.budget_open() {
            return false;
        }
        match self.call_budgeted("allow_entry", (yes_ask, no_ask, secs_to_close)) {
            Ok(value) => match rune::from_value::<bool>(value) {
                Ok(b) => b,
                Err(_) => {
                    self.call_failures.set(self.call_failures.get() + 1);
                    false
                }
            },
            Err(_) => {
                self.call_failures.set(self.call_failures.get() + 1);
                false
            }
        }
    }

    /// Invoke optional `entry_quote(yes_ask, no_ask, secs_to_close)` returning a
    /// whole-cent limit quote in `0.01..=0.99`, falling back to `allow_entry`
    /// + side ask only when the script does not define `entry_quote`.
    fn entry_quote(&self, yes_ask: f64, no_ask: f64, secs_to_close: i64) -> Option<f64> {
        if !self.budget_open() {
            return None;
        }
        if self.has_entry_quote.get() != Some(false) {
            match self.call_budgeted("entry_quote", (yes_ask, no_ask, secs_to_close)) {
                Ok(value) => {
                    self.has_entry_quote.set(Some(true));
                    match rune::from_value::<Option<f64>>(value) {
                        Ok(Some(price)) => {
                            if price.is_finite() && (0.01..=0.99).contains(&price) {
                                let cents = (price * 100.0).round();
                                if ((price * 100.0) - cents).abs() < 1e-6 {
                                    return Some(cents / 100.0);
                                }
                            }
                            self.call_failures.set(self.call_failures.get() + 1);
                            return None;
                        }
                        Ok(None) => return None,
                        Err(_) => {
                            self.call_failures.set(self.call_failures.get() + 1);
                            return None;
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("Missing function") {
                        self.has_entry_quote.set(Some(false));
                    } else {
                        self.call_failures.set(self.call_failures.get() + 1);
                        return None;
                    }
                }
            }
        }
        if self.allow_entry(yes_ask, no_ask, secs_to_close) {
            let price = if self.side == "yes" { yes_ask } else { no_ask };
            if price.is_finite() && (0.01..=0.99).contains(&price) {
                let cents = (price * 100.0).round();
                if ((price * 100.0) - cents).abs() < 1e-6 {
                    return Some(cents / 100.0);
                }
            }
        }
        if self.is_v2() {
            let yb = (1.0 - no_ask).clamp(0.01, 0.99);
            let nb = (1.0 - yes_ask).clamp(0.01, 0.99);
            let ctx = strategy_v2::TickContext {
                yes_bid: yb,
                yes_ask,
                no_bid: nb,
                no_ask,
                secs_to_close,
                spread_cents: ((yes_ask - yb) * 100.0).max(0.0),
                yes_bid_size: 0.0,
                no_bid_size: 0.0,
                book_imbalance: 0.0,
                tick_index: 0,
                open_yes_ask: yes_ask,
                prev_yes_ask: yes_ask,
                high_yes_ask: yes_ask,
                low_yes_ask: yes_ask,
                delta_60s_cents: 0.0,
                window_vol_cents: 0.0,
                pav_yes_prob: yes_ask.clamp(0.01, 0.99),
                pav_ev_yes_cents: 0.0,
                pav_ev_no_cents: 0.0,
                series: String::new(),
                slot_position_contracts: 0,
                slot_avg_entry_cents: 0.0,
                slot_unrealized_pnl_cents: 0.0,
                slot_resting_cents: 0,
                spot_price: 0.0,
                strike_price: 0.0,
                spot_distance_bps: 0.0,
                spot_delta_30s_bps: 0.0,
                spot_vol_bps: 0.0,
            };
            if let Some(dec) = self.evaluate_v2(&ctx) {
                return Some((dec.limit_cents as f64) / 100.0);
            }
        }
        None
    }

    fn is_v2(&self) -> bool {
        if let Some(cached) = self.has_evaluate.get() {
            return cached;
        }
        let detected = self
            .with_vm(strategy_v2::detect_vm_has_evaluate)
            .unwrap_or(false);
        self.has_evaluate.set(Some(detected));
        detected
    }

    fn evaluate_v2(&self, ctx: &strategy_v2::TickContext) -> Option<strategy_v2::V2Decision> {
        if !self.budget_open() {
            return None;
        }
        let t0 = std::time::Instant::now();
        let eval_hash = rune::Hash::type_hash(["evaluate"]);
        let out = self.with_vm(|vm| {
            strategy_v2::call_evaluate_v2_vm(vm, eval_hash, ctx, &self.side)
        });
        self.spent.set(self.spent.get() + t0.elapsed());
        if self.spent.get().as_secs() >= STRATEGY_DEADLINE_SECS {
            self.exhausted.set(true);
        }
        match out {
            Ok(dec) => {
                self.budget_deaths.set(0);
                dec
            }
            Err((_msg, budget_halt)) => {
                self.call_failures.set(self.call_failures.get() + 1);
                if budget_halt {
                    let deaths = self.budget_deaths.get() + 1;
                    self.budget_deaths.set(deaths);
                    if deaths >= BUDGET_DEATH_LIMIT {
                        self.exhausted.set(true);
                    }
                }
                None
            }
        }
    }

    /// Evaluate either v2 `evaluate(ctx)` (when present) or v1 `entry_quote`/`allow_entry`,
    /// returning `(limit_dollars, side, post_only)`.
    fn entry_quote_with_ctx(
        &self,
        yes_ask: f64,
        no_ask: f64,
        secs_to_close: i64,
        ctx: Option<&strategy_v2::TickContext>,
        default_post_only: bool,
    ) -> Option<(f64, String, bool)> {
        if self.is_v2() {
            let owned_fallback;
            let c = match ctx {
                Some(c) => c,
                None => {
                    let yb = (1.0 - no_ask).clamp(0.01, 0.99);
                    let nb = (1.0 - yes_ask).clamp(0.01, 0.99);
                    owned_fallback = strategy_v2::TickContext {
                        yes_bid: yb,
                        yes_ask,
                        no_bid: nb,
                        no_ask,
                        secs_to_close,
                        spread_cents: ((yes_ask - yb) * 100.0).max(0.0),
                        yes_bid_size: 0.0,
                        no_bid_size: 0.0,
                        book_imbalance: 0.0,
                        tick_index: 0,
                        open_yes_ask: yes_ask,
                        prev_yes_ask: yes_ask,
                        high_yes_ask: yes_ask,
                        low_yes_ask: yes_ask,
                        delta_60s_cents: 0.0,
                        window_vol_cents: 0.0,
                        pav_yes_prob: yes_ask.clamp(0.01, 0.99),
                        pav_ev_yes_cents: 0.0,
                        pav_ev_no_cents: 0.0,
                        series: String::new(),
                        slot_position_contracts: 0,
                        slot_avg_entry_cents: 0.0,
                        slot_unrealized_pnl_cents: 0.0,
                        slot_resting_cents: 0,
                        spot_price: 0.0,
                        strike_price: 0.0,
                        spot_distance_bps: 0.0,
                        spot_delta_30s_bps: 0.0,
                        spot_vol_bps: 0.0,
                    };
                    &owned_fallback
                }
            };
            return self
                .evaluate_v2(c)
                .map(|d| ((d.limit_cents as f64) / 100.0, d.side, d.post_only));
        }
        self.entry_quote(yes_ask, no_ask, secs_to_close)
            .map(|q| (q, self.side.clone(), default_post_only))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScoreReport {
    budget_exhausted: bool,
    call_failures: usize,
    windows_conflicted: usize,
    strategy: String,
    side: String,
    windows_settled: usize,
    windows_traded: usize,
    mean_cents_per_contract: Option<f64>,
    first_half_cents: Option<f64>,
    second_half_cents: Option<f64>,
    standard: Standard,
    verdict: String,
}

/// The per-window entry index. A window's entry for a strategy is a
/// pure function of (the strategy's source, the window's snapshot
/// sequence) — so it is cached under a fingerprint of exactly those
/// inputs: the strategy's source hash and a hash over the snapshot
/// fields window_entry reads (fetch order, asks, close time). The
/// tape grows ~505 lines a minute, but appends only ever change the
/// currently-open windows; every settled window's entry is computed
/// once and then read from the index, so a re-score after a collect
/// pass runs Rune only for the windows that actually changed.
/// Results from a scan the aggregate deadline cut short are never
/// cached — they are a function of the budget state, not of the
/// window, and caching them would poison later passes.
type EntryCache = std::sync::Mutex<std::collections::HashMap<(u64, u64), Option<f64>>>;
static ENTRY_CACHE: std::sync::LazyLock<EntryCache> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// The scan-resume cache: for each (strategy, window) the longest
/// prefix of the window's snapshots a completed scan has covered,
/// that prefix's fingerprint, and the entry the scan found in it.
/// Windows are append-only — a collect pass adds snapshots to the
/// currently-open windows and never rewrites a settled one — so
/// when a window grows, its old prefix is still there verbatim
/// (proven by the prefix fingerprint) and the scan resumes where
/// it stopped instead of re-asking the strategy about every old
/// snapshot. Without this, every collect pass re-scanned every
/// window it touched (~500 windows × ~300 snapshots × 5
/// strategies of Rune calls per recompute). A scan the deadline
/// cut short updates neither cache: its result is a function of
/// the budget state, not of the window.
type PrefixCache =
    std::sync::Mutex<std::collections::HashMap<(u64, Sym), (usize, u64, Option<f64>)>>;
static PREFIX_CACHE: std::sync::LazyLock<PrefixCache> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn window_fingerprint(snaps: &[Snapshot]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    snaps.len().hash(&mut h);
    for s in snaps {
        s.fetched_at.hash(&mut h);
        s.yes_ask.get().map(f64::to_bits).hash(&mut h);
        s.no_ask.get().map(f64::to_bits).hash(&mut h);
        s.yes_bid.get().map(f64::to_bits).hash(&mut h);
        s.no_bid.get().map(f64::to_bits).hash(&mut h);
        s.yes_bid_size.to_bits().hash(&mut h);
        s.no_bid_size.to_bits().hash(&mut h);
        s.floor_strike.map(f64::to_bits).hash(&mut h);
        s.cap_strike.map(f64::to_bits).hash(&mut h);
        s.spot_price.map(f64::to_bits).hash(&mut h);
        s.series.hash(&mut h);
        s.close_time.hash(&mut h);
    }
    h.finish()
}

/// The entry a strategy would have taken in one window: the ask on its
/// side at the first snapshot (in fetch order) where its Rune
/// `allow_entry` (v1) or `evaluate` (v2) says yes. None when it never enters.
#[cfg(test)]
fn window_entry(strat: &Strategy, snaps: &[Snapshot]) -> Option<f64> {
    window_entry_keyed(strat, None, snaps, window_fingerprint(snaps))
}

fn window_entry_keyed(
    strat: &Strategy,
    ticker: Option<&str>,
    snaps: &[Snapshot],
    win_fp: u64,
) -> Option<f64> {
    window_entry_keyed_with_pav(strat, ticker, snaps, win_fp, &[]).map(|(p, _)| p)
}

fn window_entry_keyed_with_pav(
    strat: &Strategy,
    ticker: Option<&str>,
    snaps: &[Snapshot],
    win_fp: u64,
    pav_blocks: &[recalibration::PavBlock],
) -> Option<(f64, String)> {
    // A strategy whose aggregate allowance is spent degrades to
    // no-entry for the rest of the pass — from the index or not.
    if !strat.budget_open() {
        return None;
    }
    let is_v2 = strat.is_v2();
    let pav_fp = if is_v2 && !pav_blocks.is_empty() {
        recalibration::pav_blocks_fingerprint(pav_blocks)
    } else {
        0
    };
    let eff_strat_fp = strat.fingerprint ^ pav_fp;
    let key = (eff_strat_fp, win_fp);
    if !is_v2 {
        if let Some(hit) = ENTRY_CACHE.lock().unwrap().get(&key) {
            return hit.map(|p| (p, strat.side.clone()));
        }
    }
    // Resume a previous scan of this window when its covered
    // prefix is still the window's prefix verbatim (v1 only; v2 multi-bar
    // features depend on ticks from the start of the window).
    let mut resume_from = 0usize;
    let is_settled = snaps.iter().any(|s| !s.result.is_empty());
    let ticker_sym: Option<Sym> = if !is_settled && !is_v2 {
        ticker.map(|t| {
            snaps
                .first()
                .filter(|s| s.ticker.as_str() == t)
                .map(|s| s.ticker)
                .unwrap_or_else(|| Sym::new(t))
        })
    } else {
        None
    };
    if let Some(t_sym) = ticker_sym {
        let prior = PREFIX_CACHE
            .lock()
            .unwrap()
            .get(&(strat.fingerprint, t_sym))
            .copied();
        if let Some((scanned, prefix_fp, prior_entry)) = prior {
            if scanned <= snaps.len()
                && window_fingerprint(&snaps[..scanned]) == prefix_fp
            {
                if let Some(price) = prior_entry {
                    // The entry sits inside the unchanged prefix;
                    // the full-window result is already known.
                    if !strat.budget_exhausted() {
                        ENTRY_CACHE.lock().unwrap().insert(key, Some(price));
                    }
                    return Some((price, strat.side.clone()));
                }
                resume_from = scanned;
            }
        }
    }
    let entry = window_entry_scan_with_pav(strat, &snaps[resume_from..], pav_blocks);
    if !strat.budget_exhausted() && !is_v2 {
        let entry_price = entry.as_ref().map(|(p, _)| *p);
        let mut guard = ENTRY_CACHE.lock().unwrap();
        // Windows stop changing once settled, so the index stays
        // small; the cap is insurance for a months-long process —
        // a cleared index only costs recompute, never correctness.
        if guard.len() > 250_000 {
            guard.clear();
        }
        guard.insert(key, entry_price);
        drop(guard);
        if let Some(t_sym) = ticker_sym {
            let mut guard = PREFIX_CACHE.lock().unwrap();
            if guard.len() > 250_000 {
                guard.clear();
            }
            guard.insert(
                (strat.fingerprint, t_sym),
                (snaps.len(), win_fp, entry_price),
            );
        }
    }
    entry
}

fn window_entry_scan_with_pav(
    strat: &Strategy,
    snaps: &[Snapshot],
    pav_blocks: &[recalibration::PavBlock],
) -> Option<(f64, String)> {
    if strat.is_v2() {
        let mut tracker = strategy_v2::CausalFeatureTracker::new();
        for snap in snaps {
            let Some(ctx) = tracker.push_tick(snap, pav_blocks) else {
                continue;
            };
            if let Some(dec) = strat.evaluate_v2(&ctx) {
                let price = (dec.limit_cents as f64) / 100.0;
                if (0.01..=0.99).contains(&price) {
                    return Some((price, dec.side));
                }
            }
        }
        return None;
    }
    for snap in snaps {
        let (Some(ya), Some(na)) = (snap.yes_ask.get(), snap.no_ask.get()) else {
            continue;
        };
        let Some(close) = parse_iso_z(&snap.close_time) else {
            continue;
        };
        let secs_to_close = close - snap.fetched_at;
        if strat.allow_entry(ya, na, secs_to_close) {
            let price = if strat.side == "yes" { ya } else { na };
            // Only whole cents trade (README): 0.45 yes, never 0.4567.
            let whole_cent = ((price * 100.0) - (price * 100.0).round()).abs() < 1e-9;
            if (0.01..=0.99).contains(&price) && whole_cent {
                return Some((price, strat.side.clone()));
            }
        }
    }
    None
}

/// What a window's snapshots actually establish. The venue's record
/// is the only truth the desk has, so a window whose records
/// disagree with each other — or cannot be placed in time — is not
/// scored at all, and the disagreement is counted, never silently
/// resolved in someone's favour.
#[derive(Clone)]
enum WindowFacts {
    Unsettled,
    Conflict,
    Settled { result: String, close_epoch: i64 },
}

/// The window-facts cache. A window's facts are a pure function of
/// its snapshot sequence (results and close times, in order), and
/// every analysis derives them once per consumer — the simulation
/// plus five scorings re-derived the same facts six times. Keyed by
/// a fingerprint of exactly the fields window_facts reads.
static FACTS_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<u64, WindowFacts>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn facts_fingerprint(snaps: &[Snapshot]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    snaps.len().hash(&mut h);
    for s in snaps {
        s.result.hash(&mut h);
        s.close_time.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
fn window_facts(snaps: &[Snapshot]) -> WindowFacts {
    window_facts_keyed(snaps, facts_fingerprint(snaps))
}

fn window_facts_keyed(snaps: &[Snapshot], key: u64) -> WindowFacts {
    if let Some(hit) = FACTS_CACHE.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let facts = window_facts_scan(snaps);
    let mut guard = FACTS_CACHE.lock().unwrap();
    if guard.len() > 250_000 {
        guard.clear();
    }
    guard.insert(key, facts.clone());
    facts
}

fn window_facts_scan(snaps: &[Snapshot]) -> WindowFacts {
    let mut distinct: Vec<&str> = Vec::new();
    for s in snaps {
        if !s.result.is_empty() && !distinct.contains(&s.result.as_ref()) {
            distinct.push(s.result.as_ref());
        }
    }
    if distinct.len() > 1 {
        return WindowFacts::Conflict;
    }
    let Some(result) = distinct.into_iter().next() else {
        return WindowFacts::Unsettled;
    };
    // The window's moment is the settled record's own close time —
    // not the earliest-fetched snapshot's, whose timestamp one
    // non-entry record could poison. Fall back to the latest
    // parseable close time in the window; a window with none cannot
    // be ordered, and compounding order is money.
    let epoch = snaps
        .iter()
        .rev()
        .find(|s| s.result.as_ref() == result)
        .and_then(|s| parse_iso_z(&s.close_time))
        .or_else(|| snaps.iter().filter_map(|s| parse_iso_z(&s.close_time)).max());
    match epoch {
        Some(close_epoch) => WindowFacts::Settled {
            result: result.to_string(),
            close_epoch,
        },
        None => WindowFacts::Unsettled,
    }
}

/// Group snapshots into market windows keyed by ticker, each window's
/// snapshots in fetch order.
#[cfg(test)]
fn group_windows(snapshots: &[Snapshot]) -> BTreeMap<String, Vec<Snapshot>> {
    let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
    for snap in snapshots {
        windows.entry(snap.ticker.to_string()).or_default().push(snap.clone());
    }
    for snaps in windows.values_mut() {
        snaps.sort_by_key(|s| s.fetched_at);
    }
    windows
}

#[cfg(test)]
fn score_strategy(strat: &Strategy, snapshots: &[Snapshot], standard: Standard) -> ScoreReport {
    score_strategy_windows(strat, &group_windows(snapshots), standard)
}

#[cfg(test)]
fn score_strategy_windows(
    strat: &Strategy,
    windows: &BTreeMap<String, Vec<Snapshot>>,
    standard: Standard,
) -> ScoreReport {
    score_strategy_windows_fps(strat, windows, standard, None)
}

/// `fps`, when given, carries (facts fingerprint, entry
/// fingerprint) per window in the map's iteration order — the
/// analysis computes them once and shares them across the slot
/// simulation and every strategy's score, instead of each pass
/// re-hashing every snapshot of every window.
fn score_strategy_windows_fps(
    strat: &Strategy,
    windows: &BTreeMap<String, Vec<Snapshot>>,
    standard: Standard,
    fps: Option<&[(u64, u64)]>,
) -> ScoreReport {
    let pav_blocks = if strat.is_v2() {
        recalibration::fit_is_market_pav_blocks(windows).0
    } else {
        Vec::new()
    };
    // (close_epoch, pnl per contract) for every settled window we entered.
    let mut trades: Vec<(i64, f64)> = Vec::new();
    let mut settled = 0usize;
    let mut conflicted = 0usize;
    for (wi, (_ticker, snaps)) in windows.iter().enumerate() {
        let (facts_fp, win_fp) = match fps {
            Some(f) => f[wi],
            None => (facts_fingerprint(snaps), window_fingerprint(snaps)),
        };
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict => {
                conflicted += 1;
                continue;
            }
            WindowFacts::Unsettled => continue,
        };
        settled += 1;
        let Some((price, entry_side)) =
            window_entry_keyed_with_pav(strat, Some(_ticker), snaps, win_fp, &pav_blocks)
        else {
            continue;
        };
        if price <= 0.0 || price >= 1.0 {
            continue;
        }
        let payoff = if result == entry_side { 1.0 } else { 0.0 };
        let pnl = payoff - price - fee(price);
        trades.push((close_epoch, pnl));
    }
    trades.sort_by_key(|(close, _)| *close);
    let traded = trades.len();
    let mean = |xs: &[f64]| {
        if xs.is_empty() {
            None
        } else {
            Some(xs.iter().sum::<f64>() / xs.len() as f64 * 100.0)
        }
    };
    let pnls: Vec<f64> = trades.iter().map(|(_, p)| *p).collect();
    let half = traded / 2;
    let first = mean(&pnls[..half]);
    let second = mean(&pnls[half..]);
    let overall = mean(&pnls);
    let verdict = if traded < standard.min_windows {
        format!(
            "INSUFFICIENT EVIDENCE — {traded} forward windows traded, the standard requires {}",
            standard.min_windows
        )
    } else if first.unwrap_or(0.0) > 0.0
        && second.unwrap_or(0.0) > 0.0
        && overall.unwrap_or(0.0) >= standard.min_mean_cents
    {
        "PROVEN on forward evidence (paper only — no orders placed)".to_string()
    } else {
        "NOT PROVEN — at least one half of the forward record is not positive above the bar"
            .to_string()
    };
    ScoreReport {
        budget_exhausted: strat.budget_exhausted(),
        call_failures: strat.call_failures(),
        windows_conflicted: conflicted,
        strategy: strat.name.clone(),
        side: strat.side.clone(),
        windows_settled: settled,
        windows_traded: traded,
        mean_cents_per_contract: overall,
        first_half_cents: first,
        second_half_cents: second,
        standard,
        verdict,
    }
}

// ---------------------------------------------------------------------------
// The slot system, on paper — per-slot state, strikes, equity-split sizing
// ---------------------------------------------------------------------------
//
// The desk the goal describes, expressed over the forward tape. There is
// one slot per strategy in the library; each slot starts with its own
// strategy. Before each window the paper pot is notionally re-split
// evenly across the slots — the split uses 100% of the pot, no reserve —
// and each slot stakes its full current share on a window its strategy
// enters (contracts = floor(share / price), fees charged on top). A
// losing window is a strike against the strategy running in that slot, a
// winning window clears the strikes, and three consecutive strikes
// retire the strategy — the next one in the library takes the slot. A
// slot's reported equity is its attributed equity: its starting share
// plus its own cumulative paper P&L. All of it is paper: the only thing
// at risk is the simulation's own arithmetic.

/// Consecutive losing windows that retire a strategy from its slot.
const STRIKE_LIMIT: i64 = 3;

/// The desk's stage-one target: compound the pot to $1,000. Stage
/// progress is measured from the paper simulation's own record and
/// labelled paper everywhere it appears — it is a statement about
/// the paper pot, never about venue money.
const STAGE_TARGET_DOLLARS: f64 = 1000.0;

/// Where the paper pot stands against the stage-one target,
/// computed from the slot simulation over the collected tape.
#[derive(Clone, Serialize)]
struct StageProgress {
    target_dollars: f64,
    start_dollars: f64,
    paper_equity_dollars: f64,
    progress_pct: f64,
    remaining_dollars: f64,
    reached: bool,
}

fn stage_progress(sim: &SlotSimulation) -> StageProgress {
    let equity = sim.paper_equity_dollars;
    StageProgress {
        target_dollars: STAGE_TARGET_DOLLARS,
        start_dollars: sim.paper_pot_start,
        paper_equity_dollars: equity,
        progress_pct: equity / STAGE_TARGET_DOLLARS * 100.0,
        remaining_dollars: if equity >= STAGE_TARGET_DOLLARS {
            0.0
        } else {
            STAGE_TARGET_DOLLARS - equity
        },
        reached: equity >= STAGE_TARGET_DOLLARS,
    }
}

#[derive(Clone, Serialize)]
struct Trade {
    close_epoch: i64,
    slot: usize,
    strategy: String,
    price: f64,
    contracts: i64,
    pnl_dollars: f64,
}

#[derive(Clone, Serialize)]
struct SlotReport {
    slot: usize,
    strategy: String,
    /// A slot is always running a strategy (rotation is instant);
    /// retired occupants live in the simulation's retired list.
    status: String,
    strikes: i64,
    /// Cumulative across every occupant of this slot.
    windows_traded: usize,
    rotations: usize,
    /// Cumulative across every occupant of this slot.
    paper_pnl_dollars: f64,
    equity_dollars: f64,
    /// The current occupant's own record since it took the slot —
    /// a slot line must never credit a strategy with a
    /// predecessor's windows or money.
    stint_windows: usize,
    stint_pnl_dollars: f64,
}

#[derive(Clone, Serialize)]
struct RetiredReport {
    slot: usize,
    strategy: String,
    strikes: i64,
    windows_traded: usize,
    paper_pnl_dollars: f64,
    /// The strategy that actually took the slot at that moment —
    /// not whatever the slot happens to run at the end.
    rotated_to: String,
}

#[derive(Clone, Serialize)]
struct StrategyTotal {
    strategy: String,
    windows_traded: usize,
    paper_pnl_dollars: f64,
}

#[derive(Clone, Serialize)]
struct SlotSimulation {
    conflicted_windows: usize,
    paper_pot_start: f64,
    paper_equity_dollars: f64,
    slots: Vec<SlotReport>,
    retired: Vec<RetiredReport>,
    strategy_totals: Vec<StrategyTotal>,
    /// Disclosures the surfaces must not bury — e.g. a strategy
    /// running in several slots at once (its pot weight multiplies).
    notes: Vec<String>,
    #[serde(skip)]
    trades: Vec<Trade>,
}

#[cfg(test)]
fn simulate_slots(strategies: &[Strategy], snapshots: &[Snapshot], paper_pot: f64) -> SlotSimulation {
    simulate_slots_windows(strategies, &group_windows(snapshots), paper_pot)
}

#[cfg(test)]
fn simulate_slots_windows(
    strategies: &[Strategy],
    grouped: &BTreeMap<String, Vec<Snapshot>>,
    paper_pot: f64,
) -> SlotSimulation {
    simulate_slots_windows_fps(strategies, grouped, paper_pot, None)
}

/// `fps`: see score_strategy_windows_fps.
fn simulate_slots_windows_fps(
    strategies: &[Strategy],
    grouped: &BTreeMap<String, Vec<Snapshot>>,
    paper_pot: f64,
    fps: Option<&[(u64, u64)]>,
) -> SlotSimulation {
    let n = strategies.len();
    if n == 0 {
        return SlotSimulation {
            conflicted_windows: 0,
            paper_pot_start: paper_pot,
            paper_equity_dollars: paper_pot,
            slots: Vec::new(),
            retired: Vec::new(),
            strategy_totals: Vec::new(),
            notes: Vec::new(),
            trades: Vec::new(),
        };
    }
    let pav_blocks = if strategies.iter().any(|s| s.is_v2()) {
        recalibration::fit_is_market_pav_blocks(grouped).0
    } else {
        Vec::new()
    };
    // Settled windows in chronological order, with every strategy's
    // entry price precomputed.
    struct Window {
        close_epoch: i64,
        result: String,
        entries: Vec<Option<(f64, String)>>,
    }
    let mut windows: Vec<Window> = Vec::new();
    let mut conflicted_windows = 0usize;
    for (wi, (_ticker, snaps)) in grouped.iter().enumerate() {
        let (facts_fp, win_fp) = match fps {
            Some(f) => f[wi],
            None => (facts_fingerprint(snaps), window_fingerprint(snaps)),
        };
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict => {
                conflicted_windows += 1;
                continue;
            }
            WindowFacts::Unsettled => continue,
        };
        // One fingerprint per window serves every strategy's
        // entry lookup in this window.
        let entries = strategies
            .iter()
            .map(|s| window_entry_keyed_with_pav(s, Some(_ticker), snaps, win_fp, &pav_blocks))
            .collect();
        windows.push(Window {
            close_epoch,
            result,
            entries,
        });
    }
    windows.sort_by_key(|w| w.close_epoch);

    let mut total_equity = paper_pot;
    let mut slot_strat: Vec<usize> = (0..n).collect();
    let mut slot_strikes = vec![0i64; n];
    let mut strat_traded = vec![0usize; n]; // by the strategy currently in the slot
    let mut strat_pnl = vec![0.0f64; n];
    let mut slot_traded = vec![0usize; n];
    let mut slot_pnl = vec![0.0f64; n];
    let mut rotations = vec![0usize; n];
    let mut retired: Vec<RetiredReport> = Vec::new();
    let mut totals: Vec<(usize, f64)> = vec![(0, 0.0); n]; // per library strategy
    let mut trades: Vec<Trade> = Vec::new();

    for window in &windows {
        // The split for this window is taken from the pot as it stands
        // at the window's start; every slot in the window uses it.
        let share = if total_equity > 0.0 {
            total_equity / n as f64
        } else {
            0.0
        };
        for slot in 0..n {
            let si = slot_strat[slot];
            let strat = &strategies[si];
            let Some((price, ref entry_side)) = window.entries[si] else {
                continue;
            };
            if price <= 0.0 || price >= 1.0 {
                continue;
            }
            let contracts = (share / price).floor() as i64;
            if contracts <= 0 {
                continue;
            }
            let payoff = if &window.result == entry_side {
                contracts as f64
            } else {
                0.0
            };
            let pnl = payoff - contracts as f64 * price - contracts as f64 * fee(price);
            total_equity += pnl;
            slot_pnl[slot] += pnl;
            slot_traded[slot] += 1;
            strat_traded[slot] += 1;
            strat_pnl[slot] += pnl;
            totals[si].0 += 1;
            totals[si].1 += pnl;
            trades.push(Trade {
                close_epoch: window.close_epoch,
                slot,
                strategy: strat.name.clone(),
                price,
                contracts,
                pnl_dollars: pnl,
            });
            if pnl < 0.0 {
                slot_strikes[slot] += 1;
            } else {
                slot_strikes[slot] = 0;
            }
            if slot_strikes[slot] >= STRIKE_LIMIT {
                // Successor: the next library strategy in order that
                // is not currently running in another slot — a blind
                // next-in-order can stack one strategy across slots
                // and silently multiply its weight in the pot. Only
                // when every other strategy is already running (slots
                // == library size) does the next in order take it,
                // and the end-state notes disclose the duplication.
                let mut chosen = (si + 1) % n;
                if n > 1 {
                    let running: std::collections::HashSet<usize> = slot_strat
                        .iter()
                        .enumerate()
                        .filter(|(idx, _)| *idx != slot)
                        .map(|(_, s)| *s)
                        .collect();
                    for k in 1..n {
                        let cand = (si + k) % n;
                        if cand != si && !running.contains(&cand) {
                            chosen = cand;
                            break;
                        }
                    }
                }
                retired.push(RetiredReport {
                    slot,
                    strategy: strat.name.clone(),
                    strikes: slot_strikes[slot],
                    windows_traded: strat_traded[slot],
                    paper_pnl_dollars: strat_pnl[slot],
                    rotated_to: strategies[chosen].name.clone(),
                });
                slot_strat[slot] = chosen;
                slot_strikes[slot] = 0;
                strat_traded[slot] = 0;
                strat_pnl[slot] = 0.0;
                rotations[slot] += 1;
            }
        }
    }

    // End-state disclosures: a strategy running in several slots at
    // once stakes several shares on the same windows — say so.
    let mut notes: Vec<String> = Vec::new();
    for strat in strategies {
        if strat.budget_exhausted() {
            notes.push(format!(
                "strategy {} exhausted its scoring budget and was treated as no-entry for the remaining windows",
                strat.name
            ));
        }
        if strat.call_failures() > 0 {
            notes.push(format!(
                "strategy {}'s allow_entry failed {} time(s) (runtime error or a non-boolean return) and was treated as no-entry for those windows",
                strat.name,
                strat.call_failures()
            ));
        }
    }
    if conflicted_windows > 0 {
        notes.push(format!(
            "{conflicted_windows} window(s) carry conflicting settlements from the venue and were not scored"
        ));
    }
    if total_equity <= 0.0 {
        notes.push(
            "the paper pot is exhausted — at zero or below, no slot can stake, so the simulation cannot trade again on this tape"
                .to_string(),
        );
    }
    for (i, s) in strategies.iter().enumerate() {
        let count = slot_strat.iter().filter(|si| **si == i).count();
        if count > 1 {
            notes.push(format!(
                "{} is running in {} slots at once — its weight in the pot is multiplied accordingly",
                s.name, count
            ));
        }
    }
    SlotSimulation {
        conflicted_windows,
        paper_pot_start: paper_pot,
        paper_equity_dollars: total_equity,
        slots: (0..n)
            .map(|slot| SlotReport {
                slot,
                strategy: strategies[slot_strat[slot]].name.clone(),
                status: "active".to_string(),
                strikes: slot_strikes[slot],
                windows_traded: slot_traded[slot],
                rotations: rotations[slot],
                paper_pnl_dollars: slot_pnl[slot],
                equity_dollars: paper_pot / n as f64 + slot_pnl[slot],
                stint_windows: strat_traded[slot],
                stint_pnl_dollars: strat_pnl[slot],
            })
            .collect(),
        retired,
        strategy_totals: strategies
            .iter()
            .enumerate()
            .map(|(i, s)| StrategyTotal {
                strategy: s.name.clone(),
                windows_traded: totals[i].0,
                paper_pnl_dollars: totals[i].1,
            })
            .collect(),
        notes,
        trades,
    }
}

/// Cumulative paper P&L per strategy over time, plus the whole pot's
/// equity curve — the data behind the chart. Points are (epoch, value)
/// in chronological order; each strategy series starts at 0 and the pot
/// series starts at the paper pot.
type EquitySeries = Vec<(String, Vec<(i64, f64)>)>;
fn equity_curves(sim: &SlotSimulation) -> (Vec<(i64, f64)>, EquitySeries) {
    let mut pot: Vec<(i64, f64)> = Vec::new();
    let mut per: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
    let mut pot_now = sim.paper_pot_start;
    let mut cum: BTreeMap<String, f64> = BTreeMap::new();
    // Trades at the same close epoch land together on the pot curve.
    let mut trades: Vec<&Trade> = sim.trades.iter().collect();
    trades.sort_by_key(|t| t.close_epoch);
    let mut i = 0;
    while i < trades.len() {
        let epoch = trades[i].close_epoch;
        let mut epoch_pnl: BTreeMap<String, f64> = BTreeMap::new();
        while i < trades.len() && trades[i].close_epoch == epoch {
            pot_now += trades[i].pnl_dollars;
            *epoch_pnl.entry(trades[i].strategy.clone()).or_default() += trades[i].pnl_dollars;
            i += 1;
        }
        pot.push((epoch, pot_now));
        for (name, pnl) in epoch_pnl {
            let c = cum.entry(name.clone()).or_default();
            *c += pnl;
            per.entry(name).or_default().push((epoch, *c));
        }
    }
    // The starting points the doc comment promises: the pot series
    // opens at the paper pot and each strategy series at 0, at the
    // first traded epoch.
    if let Some((first_epoch, _)) = pot.first().copied() {
        pot.insert(0, (first_epoch, sim.paper_pot_start));
    }
    for series in per.values_mut() {
        if let Some((first_epoch, _)) = series.first().copied() {
            series.insert(0, (first_epoch, 0.0));
        }
    }
    (pot, per.into_iter().collect())
}

/// VM instruction budget per strategy call (see call_budgeted).
const STRATEGY_BUDGET: usize = 250_000;
/// Memory cap per strategy call, in bytes allocated through Rune's
/// allocator (see call_budgeted). A predicate over three numbers
/// needs kilobytes; 8 MiB is wide headroom for honest shapes and
/// ~75x below the ~600 MB a single format!/with_capacity call can
/// otherwise allocate inside a few instructions.
const STRATEGY_MEMORY_BYTES: usize = 8 * 1024 * 1024;
/// Consumed-call-time allowance per strategy per scoring pass
/// (see Strategy::budget_open for the calibration).
const STRATEGY_DEADLINE_SECS: u64 = 30;

/// Consecutive instruction-budget deaths that mark a strategy
/// exhausted (see Strategy::budget_open).
const BUDGET_DEATH_LIMIT: usize = 32;

/// Whether a call error is the instruction budget halting the VM,
/// as opposed to any other failure (a memory-limit breach reads
/// "Failed to allocate …", a type error names itself). The wording
/// is the pinned rune version's; the looper unit test fails loudly
/// if a rune bump ever changes it.
fn is_budget_halt(e: &rune::runtime::VmError) -> bool {
    e.to_string().starts_with("Halted for unexpected reason `limited`")
}
/// A strategy is a small predicate; a file past this size is a
/// config error, and it is recompiled often enough that its size
/// is a per-request cost.
const MAX_STRATEGY_BYTES: usize = 256 * 1024;

/// (path, byte length, mtime-millis, unit, runtime, name, side) —
/// see Strategy::load.
#[allow(clippy::type_complexity)]
static STRAT_CACHE: std::sync::Mutex<
    Vec<(
        PathBuf,
        u64,
        u64,
        Arc<rune::Unit>,
        Arc<rune::runtime::RuntimeContext>,
        String,
        String,
    )>,
> = std::sync::Mutex::new(Vec::new());

#[allow(dead_code)]
fn load_strategies(dir: &Path) -> Vec<Strategy> {
    load_strategies_with_errors(dir).0
}

/// Load the library, keeping the failures: a broken .rn file must
/// be visible on the surfaces, not silently dropped while every
/// surface presents the surviving subset as the whole library.
fn load_strategies_with_errors(dir: &Path) -> (Vec<Strategy>, Vec<(String, String)>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "rn").unwrap_or(false))
            .filter(|p| is_confined_regular_file(dir, p))
            .collect();
        paths.sort();
        // Compile all uncached strategies under one transient Context, then drop it.
        let _compile_guard = CompileCtxGuard::enter();
        for path in paths {
            let fname = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            match Strategy::load(&path) {
                Ok(s) => {
                    // Names key the equity curves and the totals:
                    // two strategies sharing one would merge into a
                    // single series matching neither's record.
                    if out.iter().any(|o: &Strategy| o.name == s.name) {
                        errors.push((
                            fname,
                            format!(
                                "duplicate strategy name \"{}\" — names must be unique across the library",
                                s.name
                            ),
                        ));
                    } else {
                        out.push(s);
                    }
                }
                Err(e) => {
                    eprintln!("strategy load failed: {e}");
                    errors.push((fname, e));
                }
            }
        }
        drop(_compile_guard);
    }
    (out, errors)
}

// ---------------------------------------------------------------------------
// Venue account state — read-only, and only if a credential is already in
// the environment. This build never hunts for one.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AccountState {
    status: String,
    balance: Option<f64>,
    /// None = the venue has not told us: unknown is unavailable,
    /// never an empty list that renders as a fabricated zero.
    open_positions: Option<Vec<serde_json::Value>>,
    day_pnl: Option<f64>,
    note: String,
}

fn account_state() -> AccountState {
    let key_id = std::env::var("KALSHI_API_KEY_ID").ok();
    let key_path = std::env::var("KALSHI_PRIVATE_KEY_PATH").ok();
    let note = match (key_id, key_path) {
        (Some(_), Some(p)) if Path::new(&p).exists() => {
            "A credential is present in the environment, but signed venue reads are not enabled in this build — figures are shown as unavailable rather than guessed."
        }
        (Some(_), Some(_)) => {
            "A venue credential ID is present in the environment, but its private key file is not — signed venue reads are not enabled in this build, and figures are shown as unavailable rather than guessed."
        }
        (Some(_), None) => {
            "A venue credential ID is present in the environment, but no private key path is set — signed venue reads are not enabled in this build, and figures are shown as unavailable rather than guessed."
        }
        (None, Some(_)) => {
            "A venue private key path is set in the environment, but no credential ID — signed venue reads are not enabled in this build, and figures are shown as unavailable rather than guessed."
        }
        (None, None) => {
            "No venue credential in the environment — account figures are unavailable. Market data below is public venue data and needs no credential."
        }
    };
    AccountState {
        status: "unavailable".to_string(),
        balance: None,
        open_positions: None,
        day_pnl: None,
        note: note.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Daily report — plain words, venue truth, signed P&L in line one
// ---------------------------------------------------------------------------

/// Money as a reader expects it: -$42.46, not $-42.46.
fn fmt_money(v: f64) -> String {
    if v < 0.0 {
        format!("-${:.2}", v.abs())
    } else {
        format!("${v:.2}")
    }
}

fn daily_report(app: &App) -> String {
    let account = account_state();
    // DIRECTION's signature: line 1 is the day's venue truth — the
    // signed P&L, or the honest unavailable line. Never a title, never
    // an invented figure.
    let pnl_line = match account.day_pnl {
        Some(p) => format!("Day P&L (venue): {}${:.2}", if p < 0.0 { "-" } else { "+" }, p.abs()),
        // The reason comes from account_state itself — the report must
        // not claim "no credential" when one is present but unread.
        None => format!(
            "Day P&L (venue): unavailable — {} Nothing below invents one.",
            account.note
        ),
    };
    let health = collector_health(app);
    let collecting = health.collecting;
    let age = health.newest_snapshot_age_secs;
    let age_text = match age {
        Some(a) if a < 90 => format!("{a}s old"),
        Some(a) if a < 5400 => format!("{}m old", a / 60),
        Some(a) => format!("{:.1}h old", a as f64 / 3600.0),
        None => "no snapshots yet".to_string(),
    };
    let mut lines = vec![
        pnl_line,
        format!(
            "As of {} — everything below is cumulative over the collected tape ({} snapshots, {} distinct markets).",
            iso_z(now_unix()),
            health.snapshots_on_disk,
            health.markets_seen
        ),
        format!(
            "Balance (venue): {} · Open positions (venue): {}",
            account
                .balance
                .map(|b| format!("${b:.2}"))
                .unwrap_or_else(|| "unavailable".to_string()),
            match &account.open_positions {
                Some(v) => v.len().to_string(),
                None => "unavailable".to_string(),
            }
        ),
        format!(
            "Collector: {} — {} snapshots on disk across {} markets, {} distinct settled windows; newest snapshot {}.{}",
            if collecting { "collecting" } else { "NOT collecting" },
            health.snapshots_on_disk,
            health.markets_seen,
            health.settled_windows,
            age_text,
            if health.series_failed_last_pass > 0 {
                let named = if health.series_failed_names.is_empty() {
                    String::new()
                } else {
                    format!(": {}", health.series_failed_names.join(", "))
                };
                format!(
                    " {} of {} market reads failed on the last pass ({} series, open and settled){named}.",
                    health.series_failed_last_pass,
                    health.series_total,
                    SERIES.len()
                )
            } else {
                String::new()
            }
        ),
    ];
    if !collecting {
        lines.push("  Uncollected windows cannot be backfilled — a stopped collector is lost evidence.".to_string());
    }
    // The slot system over the tape, on paper — from the analysis
    // cache, so a report against an unchanged tape does not re-run
    // the whole simulation and scoring per request.
    let (sim, scores) = app.analysis(&app.data_dir.join("snapshots.jsonl"));
    lines.push(format!(
        "Slots (paper — starting pot ${:.2}, re-split evenly across slots each window):",
        sim.paper_pot_start
    ));
    let (_, lib_errors) = load_strategies_with_errors(&app.strategies_dir);
    if !lib_errors.is_empty() {
        let named: Vec<String> = lib_errors
            .iter()
            .map(|(f, e)| format!("{f}: {e}"))
            .collect();
        lines.push(format!(
            "Strategy library: {} file(s) failed to load and are NOT being scored — {}",
            lib_errors.len(),
            named.join("; ")
        ));
    }
    for slot in &sim.slots {
        lines.push(format!(
            "  Slot {}: {} ({}) — strikes {}/{} · this stint: {} windows, P&L {} · slot total: {} windows, P&L {}, equity {}",
            slot.slot + 1,
            slot.strategy,
            slot.status,
            slot.strikes,
            STRIKE_LIMIT,
            slot.stint_windows,
            fmt_money(slot.stint_pnl_dollars),
            slot.windows_traded,
            fmt_money(slot.paper_pnl_dollars),
            fmt_money(slot.equity_dollars)
        ));
    }
    for r in &sim.retired {
        lines.push(format!(
            "  Retired: {} from slot {} after {} consecutive losses ({} windows, P&L {}) — slot rotated to {}",
            r.strategy,
            r.slot + 1,
            r.strikes,
            r.windows_traded,
            fmt_money(r.paper_pnl_dollars),
            r.rotated_to
        ));
    }
    for note in &sim.notes {
        lines.push(format!("  Note: {note}"));
    }
    lines.push(format!(
        "Paper pot: {} (started {}) — paper only, not venue money.",
        fmt_money(sim.paper_equity_dollars),
        fmt_money(sim.paper_pot_start)
    ));
    let stage = stage_progress(&sim);
    if stage.reached {
        lines.push(format!(
            "Stage target (paper): {} — the {} stage-one target is reached on paper. Paper only, not venue money.",
            fmt_money(stage.paper_equity_dollars),
            fmt_money(stage.target_dollars)
        ));
    } else {
        lines.push(format!(
            "Stage target (paper): {} of the {} stage-one target — {:.1}% of the way, {} to go. Paper only, not venue money.",
            fmt_money(stage.paper_equity_dollars),
            fmt_money(stage.target_dollars),
            stage.progress_pct,
            fmt_money(stage.remaining_dollars)
        ));
    }
    lines.push(
        "Strategy verdicts (paper, forward tape; promotion needs 100+ windows, both halves positive, +3.5c/contract after fees):"
            .to_string(),
    );
    for report in scores.iter() {
        lines.push(format!(
            "  {} ({}): {} windows traded, verdict — {}",
            report.strategy, report.side, report.windows_traded, report.verdict
        ));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// HTTP layer (std-only)
// ---------------------------------------------------------------------------

/// One parsed tape, held across requests. The collector only ever
/// appends whole lines, so when the file has grown the new state is
/// the old parse plus the tail — verified at the seam: the last line
/// of the old parse must still sit at its recorded offset in the
/// file, byte for byte, before any tail byte is trusted. A file
/// that shrank, moved backwards, or fails the seam check is parsed
/// whole. Nothing is ever keyed by less than the content it claims
#[derive(Clone, Copy)]
enum ResultTrack {
    One(Sym),
    Conflict,
}

/// to summarise.
struct TapeEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    /// Snapshots grouped into market windows (each in fetch order) —
    /// built once via streaming BufReader (zero duplicate flat Vec<Snapshot>)
    /// and folded incrementally over appends.
    grouped: Arc<BTreeMap<String, Vec<Snapshot>>>,
    /// Pre-serialized `/api/markets` JSON body built once per parse/append
    /// from the latest snapshot of each grouped ticker, sorted by close_time.
    latest_json: Arc<String>,
    /// Total valid parsed snapshots in `grouped`.
    snaps_count: usize,
    /// Timestamp (`fetched_at`) of the newest parsed snapshot in append order.
    newest_fetched_at: Option<i64>,
    /// Every line in the file, parseable or not — the health block's
    /// dropped-line count is total minus parsed, so both numbers
    /// must come from the same read.
    total_lines: usize,
    /// Collector-health aggregates over the parsed tape.
    results_by_ticker: std::collections::HashMap<Sym, ResultTrack>,
    conflicting_settlements: usize,
    /// Byte offset at which the last line starts, and its raw text:
    /// the seam an append must continue.
    seam_offset: u64,
    seam_line: String,
    /// Whether the parsed bytes end at a line boundary. A torn last
    /// line cannot be extended: the next pass's first byte would
    /// finish it, and the line count would be off by one.
    ends_newline: bool,
}

fn build_latest_markets_json(grouped: &BTreeMap<String, Vec<Snapshot>>) -> Arc<String> {
    let mut latest: Vec<&Snapshot> = grouped.values().filter_map(|w| w.last()).collect();
    latest.sort_by(|a, b| a.close_time.cmp(&b.close_time));
    let mut s = serde_json::to_string(&latest).unwrap_or_else(|_| "[]".to_string());
    s.shrink_to_fit();
    Arc::new(s)
}

/// Fold health aggregates from one parsed snapshot.
#[inline]
fn fold_one_health(
    s: &Snapshot,
    results_by_ticker: &mut std::collections::HashMap<Sym, ResultTrack>,
) {
    if !s.result.is_empty() {
        match results_by_ticker.get_mut(&s.ticker) {
            Some(entry @ ResultTrack::One(_)) => {
                if let ResultTrack::One(prev) = entry {
                    if *prev != s.result {
                        *entry = ResultTrack::Conflict;
                    }
                }
            }
            Some(ResultTrack::Conflict) => {}
            None => {
                results_by_ticker.insert(s.ticker, ResultTrack::One(s.result));
            }
        }
    }
}

/// The seam an append must continue, plus the health aggregates:
/// see TapeEntry.
fn fold_health(
    snaps: &[Snapshot],
    results_by_ticker: &mut std::collections::HashMap<Sym, ResultTrack>,
) {
    for s in snaps {
        fold_one_health(s, results_by_ticker);
    }
}

/// One consistent read of a tape's current state: the parsed
/// snapshots plus everything the health block states about them.
struct TapeView {
    grouped: Arc<BTreeMap<String, Vec<Snapshot>>>,
    latest_json: Arc<String>,
    snaps_count: usize,
    total_lines: usize,
    markets: usize,
    settled_windows: usize,
    conflicting_settlements: usize,
    newest_fetched_at: Option<i64>,
}
/// Byte-level seam finder avoiding full-buffer UTF-8 conversion on large tapes.
fn seam_of_bytes(bytes: &[u8], base: u64) -> (u64, String) {
    let t = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    match t.iter().rposition(|&b| b == b'\n') {
        Some(i) => (
            base + i as u64 + 1,
            String::from_utf8_lossy(&t[i + 1..]).to_string(),
        ),
        None => (base, String::from_utf8_lossy(t).to_string()),
    }
}

type AnalysisTuple = (
    Arc<SlotSimulation>,
    Arc<Vec<ScoreReport>>,
    Arc<Vec<(String, String)>>,
    Arc<Vec<String>>,
);

type AnalyticsTuple = (
    Arc<String>,
    Arc<String>,
    Arc<Vec<f64>>,
    Arc<std::collections::HashMap<String, Vec<f64>>>,
    f64,
);

struct AnalysisEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    /// Hash of everything besides the tape that shapes the result:
    /// the paper pot and each strategy file's metadata/content.
    key: u64,
    sim: Arc<SlotSimulation>,
    scores: Arc<Vec<ScoreReport>>,
    lib_errors: Arc<Vec<(String, String)>>,
    strategy_names: Arc<Vec<String>>,
}

struct AnalyticsCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    dataset: String,
    calibration_json: Arc<String>,
    walkforward_json: Arc<String>,
    portfolio_returns: Arc<Vec<f64>>,
    strategy_returns: Arc<std::collections::HashMap<String, Vec<f64>>>,
    paper_pot_start: f64,
}

struct RecalibrationCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    dataset: String,
    slippage_bits: u64,
    json: Arc<String>,
}

struct TournamentCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    dataset: String,
    slippage_bits: u64,
    json: Arc<String>,
}

struct TournamentCtxCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    ctx: Arc<tournament::TournamentTapeContext>,
}

struct ScaleCtxCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    ctx: Arc<scaling::ScaleTapeContext>,
}

struct EdgeAuditCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    dataset: String,
    slippage_bits: u64,
    json: Arc<String>,
}

struct DeskJsonCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    dataset: String,
    strategies_json: Arc<String>,
    slots_json: Arc<String>,
    equity_json: Arc<String>,
}

struct AllocatorCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    key: u64,
    req_hash: u64,
    json: Arc<String>,
}

const BACKTEST_CACHE_CAP: usize = 16;
const SCALE_PLAN_CACHE_CAP: usize = 32;
const SLIPPAGE_CACHE_CAP: usize = 16;

struct LiveDeskManager {
    config: kalshi_live::PersistedLiveConfig,
    rails: risk_rails::RiskRailsState,
    connected: bool,
    executions: risk_rails::LiveExecutionLedger,
    reserved_order_exposure: std::collections::HashMap<String, f64>,
    last_discrepancies: Vec<risk_rails::ReconcileDiscrepancy>,
}

#[derive(Default)]
struct LiveHotSnapshot {
    until: Option<std::time::Instant>,
    markets_json: Option<Arc<String>>,
    home_http_close: Option<Arc<Vec<u8>>>,
    strategies_http_close: Option<Arc<Vec<u8>>>,
    slots_http_close: Option<Arc<Vec<u8>>>,
    equity_http_close: Option<Arc<Vec<u8>>>,
    home_json: Option<Arc<String>>,
    strategies_json: Option<Arc<String>>,
    slots_json: Option<Arc<String>>,
    equity_json: Option<Arc<String>>,
}

#[inline]
fn build_http_close_json(body: &[u8]) -> Arc<Vec<u8>> {
    use std::io::Write as _;
    let mut out = Vec::with_capacity(body.len() + 192);
    let _ = write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{SEC_HEADERS}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    out.extend_from_slice(body);
    Arc::new(out)
}

struct App {
    data_dir: PathBuf,
    live_snapshots_path: PathBuf,
    strategies_dir: PathBuf,
    /// The paper pot the slot simulation splits across slots. A
    /// simulation parameter, stated on every surface that shows it —
    /// never a venue figure.
    paper_pot: f64,
    /// The collector's pass interval (seconds), so the home screen can
    /// say honestly whether the tape is fresh: a newest snapshot older
    /// than two intervals means collection has stopped, and
    /// uncollected windows cannot be backfilled.
    collect_secs: u64,
    /// Toggleable background public tape collector scheduler + adaptive 429 backoff.
    collector_scheduler: Arc<CollectorScheduler>,
    /// Live Kalshi Trade API v2 connection config & fail-closed risk rails (always boots disarmed).
    live_desk: std::sync::Mutex<LiveDeskManager>,
    /// Serializes live venue reconciliation passes across HTTP calls and background polling.
    reconcile_sync_lock: std::sync::Mutex<()>,
    /// Wakeup signal for the background live venue reconciler thread.
    reconcile_wake: Arc<(Mutex<u64>, Condvar)>,
    /// Tick-driven Paper/Shadow Orderbook Matching Engine & Fail-Closed Automated Runner.
    runner: Arc<shadow_runner::ShadowRunner>,
    /// Live L2 Orderbook Depth Store, WebSocket/REST stream & book-walking engine.
    orderbook: Arc<orderbook::OrderbookStore>,
    /// External public crypto spot price & strike oracle feed (defaults to offline).
    spot_feed: Arc<spot_feed::SpotFeedStore>,
    tape_cache: std::sync::RwLock<Option<TapeEntry>>,
    /// Serializes cold tape parses so 30+ concurrent readers share one 100k-line parse.
    tape_compute_lock: std::sync::Mutex<()>,
    /// The computed analysis (slot simulation + per-strategy scores)
    /// for one exact tape state. Shared via Arc under RwLock so 30+
    /// concurrent readers never contend or clone thousands of trades.
    analysis_cache: std::sync::RwLock<Option<AnalysisEntry>>,
    /// Serialises cold analysis misses so 30 concurrent readers share
    /// one computation instead of piling onto the CPU simultaneously.
    analysis_compute_lock: std::sync::Mutex<()>,
    /// Cached calibration + walk-forward JSON payloads and extracted
    /// chronological returns for instant Quant Lab & Monte Carlo reads.
    analytics_cache: std::sync::RwLock<Option<AnalyticsCacheEntry>>,
    analytics_compute_lock: std::sync::Mutex<()>,
    /// Cached PAV isotonic recalibration JSON payloads (multi-slippage) under RwLock + singleflight.
    recalibration_cache: std::sync::RwLock<Vec<RecalibrationCacheEntry>>,
    recalibration_compute_lock: std::sync::Mutex<()>,
    /// Pre-scanned tape observations for OOS strategy tournament, 12x12
    /// correlation matrix & 2D regime heatmap (`GET /api/analytics/tournament`).
    tournament_ctx_cache: std::sync::RwLock<Option<TournamentCtxCacheEntry>>,
    tournament_ctx_compute_lock: std::sync::Mutex<()>,
    /// Cached OOS strategy tournament, 12x12 correlation & regime heatmap payloads
    /// (multi-slippage) under RwLock + singleflight (`GET /api/analytics/tournament`).
    tournament_cache: std::sync::RwLock<Vec<TournamentCacheEntry>>,
    tournament_compute_locks: [std::sync::Mutex<()>; 8],
    /// Pre-extracted tape observations & depth for `$500/day` scale planner so
    /// unique slider/seed requests never reload strategies or re-scan 100k ticks.
    scale_ctx_cache: std::sync::RwLock<Option<ScaleCtxCacheEntry>>,
    scale_ctx_compute_lock: std::sync::Mutex<()>,
    /// Bounded cache for repeated `$500/day` capital scale-plan requests.
    scale_plan_cache: std::sync::RwLock<Vec<(u64, Arc<String>)>>,
    /// Cached 1-click Full Quant Edge Audit scorecard payloads under RwLock + singleflight.
    edge_audit_cache: std::sync::RwLock<Vec<EdgeAuditCacheEntry>>,
    edge_audit_compute_lock: std::sync::Mutex<()>,
    /// Pre-serialized `/api/strategies`, `/api/slots`, and `/api/equity` JSON strings
    /// cached under `(tape, strategy-dir, dataset)` so concurrent readers pay 0 serialization.
    desk_json_cache: std::sync::RwLock<Vec<DeskJsonCacheEntry>>,
    desk_json_compute_lock: std::sync::Mutex<()>,
    /// Bounded cache for `GET /api/allocator` and `POST /api/allocator/plan` responses.
    allocator_cache: std::sync::RwLock<Vec<AllocatorCacheEntry>>,
    /// Small bounded cache for repeated in-memory sandbox backtests over
    /// an unchanged tape + identical source/config hash.
    backtest_cache: std::sync::RwLock<Vec<(u64, Arc<String>)>>,
    home_cache: std::sync::RwLock<Option<HomeCacheEntry>>,
    live_hot_cache: std::sync::RwLock<LiveHotSnapshot>,
}

type HomeCacheEntry = (i64, u64, Arc<String>);
type DeskJsonTriple = (Arc<String>, Arc<String>, Arc<String>);

impl App {
    #[cfg(test)]
    fn new(data_dir: PathBuf, strategies_dir: PathBuf, paper_pot: f64, collect_secs: u64) -> Self {
        Self::new_with_auto_collect(data_dir, strategies_dir, paper_pot, collect_secs, false, collect_secs)
    }

    #[inline]
    fn invalidate_live_hot(&self) {
        if let Ok(mut guard) = self.live_hot_cache.write() {
            *guard = LiveHotSnapshot::default();
        }
    }

    fn notify_reconcile(&self) {
        let (lock, cv) = &*self.reconcile_wake;
        let mut seq = lock.lock().unwrap_or_else(|p| p.into_inner());
        *seq = seq.saturating_add(1);
        drop(seq);
        cv.notify_all();
    }

    fn new_with_auto_collect(
        data_dir: PathBuf,
        strategies_dir: PathBuf,
        paper_pot: f64,
        collect_secs: u64,
        auto_collect_enabled: bool,
        auto_collect_interval: u64,
    ) -> Self {
        let live_cfg = kalshi_live::load_live_config(&data_dir);
        let live_rails = risk_rails::RiskRailsState::load(&data_dir);
        let live_executions = risk_rails::LiveExecutionLedger::load(&data_dir);
        let runner = Arc::new(shadow_runner::ShadowRunner::load(&data_dir, paper_pot));
        let orderbook = Arc::new(orderbook::OrderbookStore::new(data_dir.clone()));
        let runner_for_ob = runner.clone();
        orderbook.set_runner_notify(move || {
            runner_for_ob.notify_tick();
        });
        let spot_feed = Arc::new(spot_feed::SpotFeedStore::new(data_dir.clone()));
        let runner_for_spot = runner.clone();
        spot_feed.set_runner_notify(move || {
            runner_for_spot.notify_tick();
        });
        let live_snapshots_path = data_dir.join("snapshots.jsonl");
        Self {
            data_dir,
            live_snapshots_path,
            strategies_dir,
            paper_pot,
            collect_secs,
            collector_scheduler: Arc::new(CollectorScheduler::new(
                auto_collect_enabled,
                auto_collect_interval,
            )),
            live_desk: std::sync::Mutex::new(LiveDeskManager {
                config: live_cfg,
                rails: live_rails,
                connected: false,
                executions: live_executions,
                reserved_order_exposure: std::collections::HashMap::new(),
                last_discrepancies: Vec::new(),
            }),
            reconcile_sync_lock: std::sync::Mutex::new(()),
            reconcile_wake: Arc::new((Mutex::new(0), Condvar::new())),
            runner,
            orderbook,
            spot_feed,
            tape_cache: std::sync::RwLock::new(None),
            tape_compute_lock: std::sync::Mutex::new(()),
            analysis_cache: std::sync::RwLock::new(None),
            analysis_compute_lock: std::sync::Mutex::new(()),
            analytics_cache: std::sync::RwLock::new(None),
            analytics_compute_lock: std::sync::Mutex::new(()),
            recalibration_cache: std::sync::RwLock::new(Vec::new()),
            recalibration_compute_lock: std::sync::Mutex::new(()),
            tournament_ctx_cache: std::sync::RwLock::new(None),
            tournament_ctx_compute_lock: std::sync::Mutex::new(()),
            tournament_cache: std::sync::RwLock::new(Vec::new()),
            tournament_compute_locks: std::array::from_fn(|_| std::sync::Mutex::new(())),
            scale_ctx_cache: std::sync::RwLock::new(None),
            scale_ctx_compute_lock: std::sync::Mutex::new(()),
            scale_plan_cache: std::sync::RwLock::new(Vec::new()),
            edge_audit_cache: std::sync::RwLock::new(Vec::new()),
            edge_audit_compute_lock: std::sync::Mutex::new(()),
            desk_json_cache: std::sync::RwLock::new(Vec::new()),
            desk_json_compute_lock: std::sync::Mutex::new(()),
            allocator_cache: std::sync::RwLock::new(Vec::new()),
            backtest_cache: std::sync::RwLock::new(Vec::new()),
            home_cache: std::sync::RwLock::new(None),
            live_hot_cache: std::sync::RwLock::new(LiveHotSnapshot::default()),
        }
    }
}

fn is_confined_regular_file(root: &Path, path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return false;
    }
    // Direct non-symlink regular child of an existing directory cannot escape `root`.
    if path.parent() == Some(root) && root.is_dir() {
        return true;
    }
    let Ok(canon_root) = root.canonicalize() else {
        return false;
    };
    let Ok(canon_path) = path.canonicalize() else {
        return false;
    };
    canon_path.starts_with(&canon_root)
}

fn tape_stat(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::symlink_metadata(path).ok()?;
    if m.file_type().is_symlink() || !m.file_type().is_file() {
        return None;
    }
    Some((
        m.len(),
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    ))
}

static STRAT_DIR_KEY_INVALIDATED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn invalidate_strategies_dir_caches() {
    STRAT_DIR_KEY_INVALIDATED.store(true, std::sync::atomic::Ordering::Release);
    if let Ok(mut g) = STRAT_CACHE.lock() {
        g.clear();
    }
    if let Ok(mut g) = ENTRY_CACHE.lock() {
        g.clear();
    }
    if let Ok(mut g) = PREFIX_CACHE.lock() {
        g.clear();
    }
}

/// Fast fingerprint of `strategies_dir` + `paper_pot` so concurrent HTTP readers
/// hit `analysis_cache` without compiling or cloning strategies on every request.
fn strategies_dir_key(dir: &Path, paper_pot: f64) -> u64 {
    #[cfg(not(test))]
    {
        static LAST_KEY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static LAST_KEY_VAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static LAST_POT_BITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let invalidated =
            STRAT_DIR_KEY_INVALIDATED.swap(false, std::sync::atomic::Ordering::AcqRel);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let pot_bits = paper_pot.to_bits();
        let prev_ms = LAST_KEY_MS.load(std::sync::atomic::Ordering::Relaxed);
        if !invalidated
            && now_ms.saturating_sub(prev_ms) < 150
            && LAST_POT_BITS.load(std::sync::atomic::Ordering::Relaxed) == pot_bits
        {
            let v = LAST_KEY_VAL.load(std::sync::atomic::Ordering::Relaxed);
            if v != 0 {
                return v;
            }
        }
        let computed = strategies_dir_key_uncached(dir, paper_pot);
        LAST_POT_BITS.store(pot_bits, std::sync::atomic::Ordering::Relaxed);
        LAST_KEY_VAL.store(computed, std::sync::atomic::Ordering::Relaxed);
        LAST_KEY_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
        computed
    }
    #[cfg(test)]
    {
        let _ = STRAT_DIR_KEY_INVALIDATED.swap(false, std::sync::atomic::Ordering::AcqRel);
        strategies_dir_key_uncached(dir, paper_pot)
    }
}

fn strategies_dir_key_uncached(dir: &Path, paper_pot: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    paper_pot.to_bits().hash(&mut h);
    let Ok(dir_meta) = std::fs::symlink_metadata(dir) else {
        return h.finish();
    };
    if dir_meta.file_type().is_symlink() || !dir_meta.file_type().is_dir() {
        return h.finish();
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut names: Vec<std::ffi::OsString> = Vec::with_capacity(20);
        for entry in entries.flatten() {
            let fname = entry.file_name();
            if Path::new(&fname).extension().is_some_and(|x| x == "rn") {
                names.push(fname);
            }
        }
        names.sort_unstable();
        for fname in names {
            let p = dir.join(&fname);
            if let Ok(m) = std::fs::symlink_metadata(&p) {
                if !m.file_type().is_symlink() && m.file_type().is_file() {
                    fname.hash(&mut h);
                    m.len().hash(&mut h);
                    let mt = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    mt.hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

impl App {
    /// The tape's parsed snapshots and its total line count, from
    /// one consistent read (see TapeEntry).
    fn tape(&self, path: &Path) -> TapeView {
        fn view(e: &TapeEntry) -> TapeView {
            TapeView {
                grouped: e.grouped.clone(),
                latest_json: e.latest_json.clone(),
                snaps_count: e.snaps_count,
                total_lines: e.total_lines,
                markets: e.grouped.len(),
                settled_windows: e.results_by_ticker.len(),
                conflicting_settlements: e.conflicting_settlements,
                newest_fetched_at: e.newest_fetched_at,
            }
        }
        let Some((len, mtime)) = is_confined_regular_file(&self.data_dir, path)
            .then(|| tape_stat(path))
            .flatten()
        else {
            return TapeView {
                grouped: Arc::new(BTreeMap::new()),
                latest_json: Arc::new("[]".to_string()),
                snaps_count: 0,
                total_lines: 0,
                markets: 0,
                settled_windows: 0,
                conflicting_settlements: 0,
                newest_fetched_at: None,
            };
        };
        {
            let guard = self.tape_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == path && e.len == len && e.mtime == mtime {
                    return view(e);
                }
            }
        }
        let _tape_compute = self.tape_compute_lock.lock().unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.tape_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == path && e.len == len && e.mtime == mtime {
                    return view(e);
                }
            }
        }
        // Incremental path: the file grew past a parse we hold, and
        // the seam line is still exactly where the old parse left
        // it — parse only the appended tail.
        let prev = {
            let mut guard = self.tape_cache.write().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == path && e.len == len && e.mtime == mtime {
                    return view(e);
                }
            }
            match &*guard {
                Some(e) if e.path == path && len > e.len && e.len > 0 => guard.take(),
                _ => None,
            }
        };
        if let Some(e) = prev {
            if let Some(extended) = extend_tape(e, path, len, mtime) {
                let out = view(&extended);
                *self.tape_cache.write().unwrap_or_else(|p| p.into_inner()) = Some(extended);
                return out;
            }
            // Seam check failed: the file was replaced, not appended
            // to. Fall through to a full parse of the current bytes.
        }

        let mut grouped_map: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        let mut results_by_ticker = std::collections::HashMap::new();
        let mut total_lines = 0usize;
        let mut snaps_count = 0usize;
        let mut newest_fetched_at: Option<i64> = None;
        let mut seam_offset = 0u64;
        let mut seam_bytes = Vec::with_capacity(512);
        let mut ends_newline = true;

        {
            let _pool_guard = TapePoolGuard::enter(4096);
            if let Ok(file) = std::fs::File::open(path) {
                use std::io::BufRead as _;
                let mut reader = std::io::BufReader::with_capacity(32 * 1024, file);
                let mut line = Vec::with_capacity(512);
                let mut offset = 0u64;
                loop {
                    line.clear();
                    match reader.read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            total_lines += 1;
                            let has_nl = line.last() == Some(&b'\n');
                            ends_newline = has_nl;
                            let raw = if has_nl { &line[..line.len() - 1] } else { &line[..] };
                            seam_offset = offset;
                            seam_bytes.clear();
                            seam_bytes.extend_from_slice(raw);
                            offset += n as u64;
                            if let Ok(s) = serde_json::from_slice::<Snapshot>(raw) {
                                snaps_count += 1;
                                newest_fetched_at = Some(s.fetched_at);
                                fold_one_health(&s, &mut results_by_ticker);
                                if let Some(vec) = grouped_map.get_mut(s.ticker.as_ref()) {
                                    vec.push(s);
                                } else {
                                    let key = s.ticker.to_string();
                                    let mut vec = Vec::with_capacity(16);
                                    vec.push(s);
                                    grouped_map.insert(key, vec);
                                }
                            }
                        }
                    }
                }
            }
        }
        let seam_line = String::from_utf8_lossy(&seam_bytes).into_owned();
        drop(seam_bytes);
        for (idx, snaps) in grouped_map.values_mut().enumerate() {
            snaps.sort_by_key(|s| s.fetched_at);
            snaps.shrink_to_fit();
            if idx & 255 == 255 {
                trim_heap();
            }
        }
        trim_heap();
        let grouped = Arc::new(grouped_map);
        let latest_json = build_latest_markets_json(&grouped);
        results_by_ticker.shrink_to_fit();
        trim_heap();
        let conflicting_settlements = results_by_ticker
            .values()
            .filter(|r| matches!(r, ResultTrack::Conflict))
            .count();
        let entry = TapeEntry {
            path: path.to_path_buf(),
            len,
            mtime,
            grouped,
            latest_json,
            snaps_count,
            newest_fetched_at,
            total_lines,
            results_by_ticker,
            conflicting_settlements,
            seam_offset,
            seam_line,
            ends_newline,
        };
        let out = view(&entry);
        *self.tape_cache.write().unwrap_or_else(|p| p.into_inner()) = Some(entry);
        out
    }

    /// Zero-scan `/api/markets` JSON cache lookup once `dpath` is security-checked.
    fn tape_latest_json(&self, path: &Path) -> Arc<String> {
        if let Some((len, mtime)) = tape_stat(path) {
            let guard = self.tape_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == path && e.len == len && e.mtime == mtime {
                    return e.latest_json.clone();
                }
            }
        }
        self.tape(path).latest_json
    }

    /// The slot simulation, per-strategy scores, and library load errors over one
    /// dataset, cached by the exact tape + strategy-dir state under RwLock.
    fn analysis_shared(&self, dpath: &Path) -> AnalysisTuple {
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.analysis_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath && e.len == len && e.mtime == mtime && e.key == key {
                    return (
                        e.sim.clone(),
                        e.scores.clone(),
                        e.lib_errors.clone(),
                        e.strategy_names.clone(),
                    );
                }
            }
        }
        let _compute_guard = self
            .analysis_compute_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.analysis_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath && e.len == len && e.mtime == mtime && e.key == key {
                    return (
                        e.sim.clone(),
                        e.scores.clone(),
                        e.lib_errors.clone(),
                        e.strategy_names.clone(),
                    );
                }
            }
        }
        let (strategies, lib_errors) = load_strategies_with_errors(&self.strategies_dir);
        let view = self.tape(dpath);
        // The tape entry carries the windows already grouped —
        // built once per parse and folded over appends — so a
        // recompute never pays a full-tape grouping again; the
        // per-window entry index does the rest.
        let fps: Vec<(u64, u64)> = view
            .grouped
            .values()
            .map(|s| (facts_fingerprint(s), window_fingerprint(s)))
            .collect();
        let sim = Arc::new(simulate_slots_windows_fps(
            &strategies,
            &view.grouped,
            self.paper_pot,
            Some(&fps),
        ));
        let scores = Arc::new(
            strategies
                .iter()
                .map(|s| score_strategy_windows_fps(s, &view.grouped, Standard::default(), Some(&fps)))
                .collect::<Vec<_>>(),
        );
        let strategy_names = Arc::new(strategies.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
        let lib_errors = Arc::new(lib_errors);
        let out = (
            sim.clone(),
            scores.clone(),
            lib_errors.clone(),
            strategy_names.clone(),
        );
        let entry = AnalysisEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            sim,
            scores,
            lib_errors,
            strategy_names,
        };
        drop(fps);
        drop(strategies);
        let mut guard = self.analysis_cache.write().unwrap_or_else(|p| p.into_inner());
        *guard = Some(entry);
        drop(guard);
        trim_heap();
        out
    }

    fn analysis(&self, dpath: &Path) -> (Arc<SlotSimulation>, Arc<Vec<ScoreReport>>) {
        let (sim, scores, _, _) = self.analysis_shared(dpath);
        (sim, scores)
    }

    /// Cached Calibration + Walk-Forward JSON payloads plus extracted chronological
    /// window returns so Quant Lab & Monte Carlo stay sub-millisecond under 30+ clients.
    fn analytics_shared(&self, dpath: &Path, dataset: &str) -> AnalyticsTuple {
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.analytics_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                {
                    return (
                        e.calibration_json.clone(),
                        e.walkforward_json.clone(),
                        e.portfolio_returns.clone(),
                        e.strategy_returns.clone(),
                        e.paper_pot_start,
                    );
                }
            }
        }
        let _compute_guard = self
            .analytics_compute_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.analytics_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                {
                    return (
                        e.calibration_json.clone(),
                        e.walkforward_json.clone(),
                        e.portfolio_returns.clone(),
                        e.strategy_returns.clone(),
                        e.paper_pot_start,
                    );
                }
            }
        }

        let (sim, _, _, _) = self.analysis_shared(dpath);
        let view = self.tape(dpath);
        let (strategies, _) = load_strategies_with_errors(&self.strategies_dir);

        let cal = analytics::calibration_from_windows(&view.grouped, &strategies);
        let calibration_json = Arc::new(json_with_dataset(&cal, dataset));

        let wf = analytics::walk_forward_from_desk(&view.grouped, &strategies, &sim);
        let portfolio_returns = Arc::new(wf.portfolio.window_returns.clone());
        let mut strat_map = std::collections::HashMap::with_capacity(wf.strategies.len());
        for s in &wf.strategies {
            strat_map.insert(s.strategy.clone(), s.split.window_returns.clone());
        }
        let strategy_returns = Arc::new(strat_map);
        let walkforward_json = Arc::new(json_with_dataset(&wf, dataset));
        let paper_pot_start = sim.paper_pot_start;

        let out = (
            calibration_json.clone(),
            walkforward_json.clone(),
            portfolio_returns.clone(),
            strategy_returns.clone(),
            paper_pot_start,
        );
        let entry = AnalyticsCacheEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            dataset: dataset.to_string(),
            calibration_json,
            walkforward_json,
            portfolio_returns,
            strategy_returns,
            paper_pot_start,
        };
        *self.analytics_cache.write().unwrap_or_else(|p| p.into_inner()) = Some(entry);
        out
    }

    /// Run an in-memory sandbox backtest with bounded caching keyed on
    /// `(tape_stat, dataset, source, config)` so repeated requests stay fast
    /// while edits compile & score fresh immediately.
    fn sandbox_backtest_cached(
        &self,
        dpath: &Path,
        dataset: &str,
        source: &str,
        cfg: analytics::BacktestConfig,
    ) -> Arc<String> {
        use std::hash::{Hash, Hasher};
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dpath.hash(&mut h);
        len.hash(&mut h);
        mtime.hash(&mut h);
        dataset.hash(&mut h);
        source.hash(&mut h);
        cfg.paper_pot_start.to_bits().hash(&mut h);
        cfg.risk_fraction.to_bits().hash(&mut h);
        cfg.min_windows.hash(&mut h);
        cfg.min_mean_cents.to_bits().hash(&mut h);
        cfg.monte_carlo_paths.hash(&mut h);
        cfg.monte_carlo_seed.hash(&mut h);
        cfg.max_trades_returned.hash(&mut h);
        let cache_key = h.finish();

        {
            let guard = self.backtest_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some((_, cached)) = guard.iter().find(|(k, _)| *k == cache_key) {
                return cached.clone();
            }
        }

        let view = self.tape(dpath);
        let mut res =
            analytics::validate_and_backtest_rune_source_with_config(source, &view.grouped, cfg);
        if let Some(score) = res.score.as_mut() {
            score.verdict = dataset_verdict(&score.verdict, dataset);
        }
        let payload = Arc::new(json_with_dataset(&res, dataset));

        if res.validation.valid {
            let mut guard = self.backtest_cache.write().unwrap_or_else(|p| p.into_inner());
            if !guard.iter().any(|(k, _)| *k == cache_key) {
                if guard.len() >= BACKTEST_CACHE_CAP {
                    guard.remove(0);
                }
                guard.push((cache_key, payload.clone()));
            }
        }
        payload
    }

    /// Cached PAV isotonic recalibration study (`GET /api/analytics/recalibration`)
    /// under RwLock + singleflight guard so 30 concurrent clients stay sub-5ms p95.
    fn recalibration_shared(&self, dpath: &Path, dataset: &str, slippage_cents: f64) -> Arc<String> {
        let slippage = if slippage_cents.is_finite() {
            slippage_cents.clamp(0.0, 5.0)
        } else {
            1.0
        };
        let slippage_bits = slippage.to_bits();
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.recalibration_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                    && e.slippage_bits == slippage_bits
            }) {
                return e.json.clone();
            }
        }
        let _compute_guard = self
            .recalibration_compute_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.recalibration_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                    && e.slippage_bits == slippage_bits
            }) {
                return e.json.clone();
            }
        }

        let _ = self.analysis_shared(dpath);
        let view = self.tape(dpath);
        let (strategies, _) = load_strategies_with_errors(&self.strategies_dir);
        let rep = recalibration::recalibration_from_windows(&view.grouped, &strategies, slippage);
        let payload = Arc::new(json_with_dataset(&rep, dataset));

        let entry = RecalibrationCacheEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            dataset: dataset.to_string(),
            slippage_bits,
            json: payload.clone(),
        };
        let mut guard = self.recalibration_cache.write().unwrap_or_else(|p| p.into_inner());
        guard.retain(|e| {
            !(e.path == dpath && (e.len != len || e.mtime != mtime || e.key != key))
        });
        if guard.len() >= SLIPPAGE_CACHE_CAP {
            guard.remove(0);
        }
        guard.push(entry);
        drop(guard);
        trim_heap();
        payload
    }

    /// Singleflight cached `TournamentTapeContext` built once per `(tape, strategies)`
    /// state so switching `slippage_cents` skips re-scanning 100k snapshots.
    fn tournament_ctx_shared(&self, dpath: &Path) -> Arc<tournament::TournamentTapeContext> {
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self
                .tournament_ctx_cache
                .read()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath && e.len == len && e.mtime == mtime && e.key == key {
                    return e.ctx.clone();
                }
            }
        }
        let _compute_guard = self
            .tournament_ctx_compute_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let guard = self
                .tournament_ctx_cache
                .read()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath && e.len == len && e.mtime == mtime && e.key == key {
                    return e.ctx.clone();
                }
            }
        }

        let _ = self.analysis_shared(dpath);
        let view = self.tape(dpath);
        let (strategies, _) = load_strategies_with_errors(&self.strategies_dir);
        let ctx = Arc::new(tournament::build_tournament_context(&view.grouped, &strategies));
        *self
            .tournament_ctx_cache
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(TournamentCtxCacheEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            ctx: ctx.clone(),
        });
        ctx
    }

    /// Cached OOS strategy tournament, 12x12 correlation & regime heatmap
    /// (`GET /api/analytics/tournament`) under RwLock + singleflight.
    fn tournament_shared(&self, dpath: &Path, dataset: &str, slippage_cents: f64) -> Arc<String> {
        let slippage = if slippage_cents.is_finite() {
            slippage_cents.clamp(0.0, 5.0)
        } else {
            1.0
        };
        let slippage_bits = slippage.to_bits();
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.tournament_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                    && e.slippage_bits == slippage_bits
            }) {
                return e.json.clone();
            }
        }
        let stripe = (slippage.round() as usize) & 7;
        let _compute_guard = self.tournament_compute_locks[stripe]
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.tournament_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                    && e.slippage_bits == slippage_bits
            }) {
                return e.json.clone();
            }
        }

        let ctx = self.tournament_ctx_shared(dpath);
        let rep = tournament::compute_tournament_from_context(&ctx, slippage);
        // Provide a canonical `regime_heatmap` alias alongside `regime_heatmap_oos` / `regime_heatmap_full`.
        let primary_heatmap = if rep.regime_heatmap_oos.total_entries_in_grid > 0 {
            &rep.regime_heatmap_oos
        } else {
            &rep.regime_heatmap_full
        };
        let payload = Arc::new(json_with_dataset_pair(
            &rep,
            dataset,
            "regime_heatmap",
            primary_heatmap,
        ));

        let entry = TournamentCacheEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            dataset: dataset.to_string(),
            slippage_bits,
            json: payload.clone(),
        };
        let mut guard = self.tournament_cache.write().unwrap_or_else(|p| p.into_inner());
        guard.retain(|e| {
            !(e.path == dpath
                && (e.len != len
                    || e.mtime != mtime
                    || e.key != key
                    || (e.dataset == dataset && e.slippage_bits == slippage_bits)))
        });
        if guard.len() >= SLIPPAGE_CACHE_CAP {
            guard.remove(0);
        }
        guard.push(entry);
        drop(guard);
        trim_heap();
        payload
    }

    /// Snapshot of auto-collector schedule, adaptive 429 backoff & 200-window readiness progress.
    fn collector_status_json(&self) -> String {
        let tape_path = self.data_dir.join("snapshots.jsonl");
        let view = self.tape(&tape_path);
        let st = self.collector_scheduler.snapshot();
        let now = now_unix();
        let next_run_in_secs = st
            .next_run_epoch
            .map(|ts| (ts - now).max(0) as u64);
        let progress_pct =
            ((view.settled_windows as f64 / READINESS_TARGET_WINDOWS as f64) * 100.0).min(100.0);
        let progress_pct = (progress_pct * 10.0).round() / 10.0;
        #[derive(serde::Serialize)]
        struct CollectorStatus {
            enabled: bool,
            interval_secs: u64,
            effective_interval_secs: u64,
            min_interval_secs: u64,
            max_interval_secs: u64,
            backoff_secs: u64,
            rate_limited: bool,
            consecutive_errors: u32,
            in_flight: bool,
            runs_total: u64,
            last_run_epoch: Option<i64>,
            next_run_epoch: Option<i64>,
            next_run_in_secs: Option<u64>,
            last_written: usize,
            last_deduped: usize,
            last_error: Option<String>,
            settled_windows: usize,
            readiness_target_windows: usize,
            readiness_progress_pct: f64,
            snapshots_on_disk: usize,
            markets_seen: usize,
            series: Vec<String>,
            mode: &'static str,
        }
        serde_json::to_string(&CollectorStatus {
            enabled: st.enabled,
            interval_secs: st.interval_secs,
            effective_interval_secs: st.effective_interval_secs(),
            min_interval_secs: AUTO_COLLECT_MIN_SECS,
            max_interval_secs: AUTO_COLLECT_MAX_SECS,
            backoff_secs: st.backoff_secs,
            rate_limited: st.rate_limited,
            consecutive_errors: st.consecutive_errors,
            in_flight: st.in_flight || PASSES_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst) > 0,
            runs_total: st.runs_total,
            last_run_epoch: st.last_run_epoch,
            next_run_epoch: st.next_run_epoch,
            next_run_in_secs,
            last_written: st.last_written,
            last_deduped: st.last_deduped,
            last_error: st.last_error,
            settled_windows: view.settled_windows,
            readiness_target_windows: READINESS_TARGET_WINDOWS,
            readiness_progress_pct: progress_pct,
            snapshots_on_disk: view.snaps_count,
            markets_seen: view.markets,
            series: active_series(),
            mode: "public_readonly_get",
        })
        .unwrap_or_else(|_| "{}".to_string())
    }

    /// Singleflight cached `ScaleTapeContext` built once per `(tape, strategies)`
    /// state so unique parameter/seed scale-plan requests stay sub-25ms p95.
    fn scale_ctx_shared(&self, dpath: &Path) -> Arc<scaling::ScaleTapeContext> {
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.scale_ctx_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath && e.len == len && e.mtime == mtime && e.key == key {
                    return e.ctx.clone();
                }
            }
        }
        let _compute_guard = self
            .scale_ctx_compute_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.scale_ctx_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = &*guard {
                if e.path == dpath && e.len == len && e.mtime == mtime && e.key == key {
                    return e.ctx.clone();
                }
            }
        }

        let (sim, _, _, _) = self.analysis_shared(dpath);
        let view = self.tape(dpath);
        let (strategies, _) = load_strategies_with_errors(&self.strategies_dir);
        let median_depth = median_observed_book_depth_grouped(&view.grouped);
        let ctx = Arc::new(scaling::build_scale_tape_context(
            &view.grouped,
            &strategies,
            &sim,
            median_depth,
        ));
        *self.scale_ctx_cache.write().unwrap_or_else(|p| p.into_inner()) =
            Some(ScaleCtxCacheEntry {
                path: dpath.to_path_buf(),
                len,
                mtime,
                key,
                ctx: ctx.clone(),
            });
        ctx
    }

    /// Cached `$500/day` paper capital scaling & liquidity capacity planner
    /// (`POST /api/analytics/scale-plan`), evaluating over the shared
    /// `ScaleTapeContext` so unique slider/seed calls never reload strategies
    /// or re-scan 100k snapshots.
    fn scale_plan_cached(
        &self,
        dpath: &Path,
        dataset: &str,
        req: &scaling::ScalePlanRequest,
    ) -> Arc<String> {
        use std::hash::{Hash, Hasher};
        let sanitized = req.sanitized();
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        let req_json = serde_json::to_string(&sanitized).unwrap_or_default();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dpath.hash(&mut h);
        len.hash(&mut h);
        mtime.hash(&mut h);
        key.hash(&mut h);
        dataset.hash(&mut h);
        req_json.hash(&mut h);
        let cache_key = h.finish();

        {
            let guard = self.scale_plan_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some((_, cached)) = guard.iter().find(|(k, _)| *k == cache_key) {
                return cached.clone();
            }
        }

        let ctx = self.scale_ctx_shared(dpath);
        let plan = scaling::compute_scale_plan_from_context(&ctx, &sanitized);
        let payload = Arc::new(json_with_dataset(&plan, dataset));

        let mut guard = self.scale_plan_cache.write().unwrap_or_else(|p| p.into_inner());
        if !guard.iter().any(|(k, _)| *k == cache_key) {
            if guard.len() >= SCALE_PLAN_CACHE_CAP {
                guard.remove(0);
            }
            guard.push((cache_key, payload.clone()));
        }
        payload
    }

    /// Cached 1-click Full Quant Edge Audit scorecard (`GET /api/analytics/edge-audit`)
    /// combining all 12 strategies, OOS tournament, 4-slot lineup, PAV recalibration,
    /// Kalshi fee & slippage net EV, Monte Carlo DD, and 5-gate readiness.
    fn edge_audit_shared(&self, dpath: &Path, dataset: &str, slippage_cents: f64) -> Arc<String> {
        let slippage = if slippage_cents.is_finite() {
            slippage_cents.clamp(0.0, 5.0)
        } else {
            1.0
        };
        let slippage_bits = slippage.to_bits();
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.edge_audit_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                    && e.slippage_bits == slippage_bits
            }) {
                return e.json.clone();
            }
        }
        let _compute_guard = self
            .edge_audit_compute_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.edge_audit_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.dataset == dataset
                    && e.slippage_bits == slippage_bits
            }) {
                return e.json.clone();
            }
        }

        let (_sim, scores, _, _) = self.analysis_shared(dpath);
        let view = self.tape(dpath);
        let (strategies, _) = load_strategies_with_errors(&self.strategies_dir);
        let tour_ctx = self.tournament_ctx_shared(dpath);
        let tour = tournament::compute_tournament_from_context(&tour_ctx, slippage);
        let recal = recalibration::recalibration_from_windows(&view.grouped, &strategies, slippage);
        let scale_ctx = self.scale_ctx_shared(dpath);
        let scale_req = scaling::ScalePlanRequest {
            slippage_cents: slippage,
            ..scaling::ScalePlanRequest::default()
        };
        let scale_plan = scaling::compute_scale_plan_from_context(&scale_ctx, &scale_req);
        let (_, _, portfolio_returns, _, default_pot) = self.analytics_shared(dpath, dataset);
        let mc = analytics::run_monte_carlo_with_equity(&portfolio_returns, default_pot, 1_000, 42);
        let report =
            edge_audit::build_edge_audit_report(dataset, &scores, &tour, &recal, &scale_plan, &mc);
        let payload = Arc::new(serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()));

        let entry = EdgeAuditCacheEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            dataset: dataset.to_string(),
            slippage_bits,
            json: payload.clone(),
        };
        let mut guard = self.edge_audit_cache.write().unwrap_or_else(|p| p.into_inner());
        guard.retain(|e| {
            !(e.path == dpath && (e.len != len || e.mtime != mtime || e.key != key))
        });
        if guard.len() >= SLIPPAGE_CACHE_CAP {
            guard.remove(0);
        }
        guard.push(entry);
        drop(guard);
        trim_heap();
        payload
    }

    /// Cached pre-serialized `/api/strategies`, `/api/slots`, and `/api/equity` JSON strings
    /// keyed on `(path, len, mtime, strategy_key, dataset)`.
    fn desk_json_shared(&self, dpath: &Path, dataset: &str) -> DeskJsonTriple {
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.desk_json_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath && e.len == len && e.mtime == mtime && e.key == key && e.dataset == dataset
            }) {
                return (e.strategies_json.clone(), e.slots_json.clone(), e.equity_json.clone());
            }
        }
        let _compute_guard = self.desk_json_compute_lock.lock().unwrap_or_else(|p| p.into_inner());
        {
            let guard = self.desk_json_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath && e.len == len && e.mtime == mtime && e.key == key && e.dataset == dataset
            }) {
                return (e.strategies_json.clone(), e.slots_json.clone(), e.equity_json.clone());
            }
        }

        let (sim, scores_arc, lib_errors, strategy_names) = self.analysis_shared(dpath);

        let mut reports: Vec<ScoreReport> = (*scores_arc).clone();
        for report in &mut reports {
            report.verdict = dataset_verdict(&report.verdict, dataset);
        }
        let strategies_json = Arc::new(serde_json::to_string(&reports).unwrap_or_else(|_| "[]".to_string()));
        drop(reports);

        #[derive(Serialize)]
        struct LibErrItem<'a> {
            file: &'a str,
            error: &'a str,
        }
        let lib_err_rows: Vec<LibErrItem<'_>> = lib_errors
            .iter()
            .map(|(file, error)| LibErrItem { file, error })
            .collect();

        #[derive(Serialize)]
        struct SlotsJsonBody<'a> {
            conflicted_windows: usize,
            paper_pot_start: f64,
            paper_equity_dollars: f64,
            slots: &'a [SlotReport],
            retired: &'a [RetiredReport],
            strategy_totals: &'a [StrategyTotal],
            notes: &'a [String],
            stage_target: StageProgress,
            trades: &'a [Trade],
            trades_truncated: bool,
            library_errors: &'a [LibErrItem<'a>],
            dataset: &'a str,
        }
        let total = sim.trades.len();
        let shown = &sim.trades[total.saturating_sub(5000)..];
        let slots_body = SlotsJsonBody {
            conflicted_windows: sim.conflicted_windows,
            paper_pot_start: sim.paper_pot_start,
            paper_equity_dollars: sim.paper_equity_dollars,
            slots: &sim.slots,
            retired: &sim.retired,
            strategy_totals: &sim.strategy_totals,
            notes: &sim.notes,
            stage_target: stage_progress(&sim),
            trades: shown,
            trades_truncated: total > 5000,
            library_errors: &lib_err_rows,
            dataset,
        };
        let slots_json = Arc::new(serde_json::to_string(&slots_body).unwrap_or_else(|_| "{}".to_string()));

        #[derive(Serialize)]
        struct EquitySeriesItem<'a> {
            strategy: &'a str,
            points: &'a [(i64, f64)],
        }
        #[derive(Serialize)]
        struct EquityJsonBody<'a> {
            paper_pot_start: f64,
            pot: &'a [(i64, f64)],
            series: Vec<EquitySeriesItem<'a>>,
            library_errors: &'a [LibErrItem<'a>],
            dataset: &'a str,
        }
        let (pot, per) = equity_curves(&sim);
        let mut series: Vec<EquitySeriesItem<'_>> = strategy_names
            .iter()
            .map(|name| {
                let points = per
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, pts)| pts.as_slice())
                    .unwrap_or(&[]);
                EquitySeriesItem { strategy: name, points }
            })
            .collect();
        series.sort_by(|a, b| a.strategy.cmp(b.strategy));
        let equity_body = EquityJsonBody {
            paper_pot_start: sim.paper_pot_start,
            pot: &pot,
            series,
            library_errors: &lib_err_rows,
            dataset,
        };
        let equity_json = Arc::new(serde_json::to_string(&equity_body).unwrap_or_else(|_| "{}".to_string()));
        drop(pot);
        drop(per);

        let entry = DeskJsonCacheEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            dataset: dataset.to_string(),
            strategies_json: strategies_json.clone(),
            slots_json: slots_json.clone(),
            equity_json: equity_json.clone(),
        };
        let mut guard = self.desk_json_cache.write().unwrap_or_else(|p| p.into_inner());
        guard.retain(|e| !(e.path == dpath && (e.len != len || e.mtime != mtime || e.key != key)));
        if guard.len() >= SLIPPAGE_CACHE_CAP {
            guard.remove(0);
        }
        guard.push(entry);
        drop(guard);
        trim_heap();
        (strategies_json, slots_json, equity_json)
    }

    fn execution_health_for_tape(
        &self,
        dpath: &Path,
        persist_quarantine: bool,
    ) -> execution_audit::ExecutionHealthReport {
        let tape = self.tape(dpath);
        let mut tape_mids: std::collections::HashMap<String, Vec<execution_audit::MidpointObs>> =
            std::collections::HashMap::new();
        let mut tape_settle: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (t, snaps) in tape.grouped.iter() {
            for s in snaps {
                let res = s.result.as_str().trim().to_ascii_lowercase();
                if res == "yes" || res == "no" {
                    tape_settle.insert(t.clone(), res);
                } else if let (Some(yb), Some(ya)) = (s.yes_bid.get(), s.yes_ask.get()) {
                    tape_mids
                        .entry(t.clone())
                        .or_default()
                        .push(execution_audit::MidpointObs {
                            ts_secs: s.fetched_at as f64,
                            yes_mid_cents: (yb + ya) * 50.0,
                        });
                }
            }
        }
        execution_audit::evaluate_execution_health(
            &self.data_dir,
            Some(&self.orderbook),
            Some(&tape_mids),
            Some(&tape_settle),
            persist_quarantine,
        )
    }

    /// Cached Autonomous Edge Allocator JSON payload keyed on tape, strategy dir,
    /// request parameters, execution audit files, and current Hard Risk Rails state.
    fn allocator_shared(
        &self,
        dpath: &Path,
        dataset: &str,
        req: &auto_allocator::AutoAllocatorRequest,
    ) -> Arc<String> {
        use std::hash::{Hash, Hasher};
        let san = req.sanitized();
        let (live_armed, killed, rails) = {
            let st = self.live_desk.lock().unwrap_or_else(|p| p.into_inner());
            (st.rails.live_armed, st.rails.killed, st.rails.rails.clone())
        };
        let key = strategies_dir_key(&self.strategies_dir, self.paper_pot);
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));

        let mut h = std::collections::hash_map::DefaultHasher::new();
        dataset.hash(&mut h);
        san.sizing_mode.hash(&mut h);
        san.execution_mode.hash(&mut h);
        san.maker_fill_rate.to_bits().hash(&mut h);
        san.min_oos_trades.hash(&mut h);
        san.max_slot_risk_pct.to_bits().hash(&mut h);
        san.max_total_heat_pct.to_bits().hash(&mut h);
        san.slippage_cents.to_bits().hash(&mut h);
        san.starting_bankroll.to_bits().hash(&mut h);
        san.require_stress_resilience.hash(&mut h);
        san.enable_regime_throttle.hash(&mut h);
        san.enable_diversification_guard.hash(&mut h);
        san.min_fill_rate_pct.to_bits().hash(&mut h);
        live_armed.hash(&mut h);
        killed.hash(&mut h);
        rails.max_order_contracts.hash(&mut h);
        rails.max_order_dollars.to_bits().hash(&mut h);
        rails.max_daily_loss_dollars.to_bits().hash(&mut h);
        rails.max_open_exposure_dollars.to_bits().hash(&mut h);
        rails.allowed_ticker_prefix.hash(&mut h);
        self.orderbook.revision().hash(&mut h);
        for fname in [
            "quarantine_state.json",
            "shadow_ledger.jsonl",
            "live_executions.jsonl",
            "orderbooks.jsonl",
            "markout_samples.jsonl",
        ] {
            let fp = self.data_dir.join(fname);
            tape_stat(&fp).unwrap_or((0, 0)).hash(&mut h);
        }
        let req_hash = h.finish();

        {
            let guard = self.allocator_cache.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = guard.iter().find(|e| {
                e.path == dpath
                    && e.len == len
                    && e.mtime == mtime
                    && e.key == key
                    && e.req_hash == req_hash
            }) {
                return e.json.clone();
            }
        }

        let ctx = self.tournament_ctx_shared(dpath);
        let health = self.execution_health_for_tape(dpath, false);
        let rep = auto_allocator::compute_auto_allocator_report_with_audit(
            &ctx,
            &san,
            &rails,
            live_armed,
            killed,
            Some(&self.orderbook),
            Some(&health),
        );
        let json = Arc::new(serde_json::to_string(&rep).unwrap_or_else(|_| "{}".into()));

        let mut guard = self.allocator_cache.write().unwrap_or_else(|p| p.into_inner());
        guard.retain(|e| !(e.path == dpath && (e.len != len || e.mtime != mtime || e.key != key)));
        if guard.len() >= SLIPPAGE_CACHE_CAP {
            guard.remove(0);
        }
        guard.push(AllocatorCacheEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            req_hash,
            json: json.clone(),
        });
        json
    }
}

/// Grow a held parse by the file's appended tail, if the append is
/// real: the seam line must still sit at its recorded offset. The
/// tail is parsed on its own — it starts at a line boundary (the
/// old file length), so its line count and seam are exact.
fn extend_tape(mut e: TapeEntry, path: &Path, len: u64, mtime: u64) -> Option<TapeEntry> {
    use std::io::{Read, Seek, SeekFrom};
    if !e.ends_newline {
        return None;
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut seam_buf = Vec::new();
    f.seek(SeekFrom::Start(e.seam_offset)).ok()?;
    std::io::Read::by_ref(&mut f)
        .take(len - e.seam_offset)
        .read_to_end(&mut seam_buf)
        .ok()?;
    let seam_text = String::from_utf8_lossy(&seam_buf);
    let seam_first = seam_text.lines().next().unwrap_or("");
    if seam_first != e.seam_line {
        return None;
    }
    // Read exactly the statted growth, never to real EOF: a pass
    // landing mid-read would otherwise be parsed now and parsed
    // AGAIN by the next extension (which trusts e.len as its
    // starting offset), duplicating its snapshots in every window.
    let mut tail_buf = Vec::new();
    f.seek(SeekFrom::Start(e.len)).ok()?;
    std::io::Read::by_ref(&mut f)
        .take(len - e.len)
        .read_to_end(&mut tail_buf)
        .ok()?;
    let (new_snaps, tail_lines) = {
        let _pool_guard = TapePoolGuard::enter(64);
        parse_tape_bytes(&tail_buf)
    };
    e.total_lines += tail_lines;
    // The held parse is shared with readers; extending it mutates
    // in place when this entry is the only holder (the common
    // case), and every fold touches only the new snapshots.
    fold_health(&new_snaps, &mut e.results_by_ticker);
    e.snaps_count += new_snaps.len();
    if let Some(last) = new_snaps.last() {
        e.newest_fetched_at = Some(last.fetched_at);
    }
    {
        let grouped = Arc::make_mut(&mut e.grouped);
        for s in new_snaps {
            let w = grouped.entry(s.ticker.to_string()).or_default();
            // Appends arrive in fetch order, so the window stays
            // sorted; an out-of-order tail re-sorts its window
            // instead of silently corrupting the order.
            let out_of_order = w
                .last()
                .is_some_and(|last| s.fetched_at < last.fetched_at);
            w.push(s);
            if out_of_order {
                w.sort_by_key(|x| x.fetched_at);
            }
        }
    }
    e.latest_json = build_latest_markets_json(&e.grouped);
    trim_heap();
    e.conflicting_settlements = e
        .results_by_ticker
        .values()
        .filter(|r| matches!(r, ResultTrack::Conflict))
        .count();
    if !tail_buf.is_empty() {
        let (off, line) = seam_of_bytes(&tail_buf, e.len);
        e.seam_offset = off;
        e.seam_line = line;
        e.ends_newline = tail_buf.last() == Some(&b'\n');
    }
    e.len = len;
    e.mtime = mtime;
    Some(e)
}

#[derive(serde::Serialize)]
struct CollectorHealth {
    snapshots_on_disk: usize,
    lines_on_disk: usize,
    dropped_lines: usize,
    markets_seen: usize,
    settled_windows: usize,
    conflicting_settlements: usize,
    newest_snapshot_age_secs: Option<i64>,
    collect_interval_secs: u64,
    collecting: bool,
    series_failed_last_pass: usize,
    series_failed_names: Vec<String>,
    series_total: usize,
    auto_collect_enabled: bool,
    auto_collect_interval_secs: u64,
    auto_collect_backoff_secs: u64,
    readiness_target_windows: usize,
    readiness_progress_pct: f64,
}

/// Collector health, computed from the tape itself — the same numbers
/// an independent count of the file gives.
fn collector_health(app: &App) -> CollectorHealth {
    let tape_path = app.data_dir.join("snapshots.jsonl");
    // One consistent view of the tape's current state: total lines,
    // parsed snapshots and the health aggregates all come from the
    // same read — the dropped-line count is the difference, and it
    // must never mix two different reads. The aggregates are folded
    // in as the tape is parsed, so stating them costs nothing here.
    let view = app.tape(&tape_path);
    // The tape is append-ordered: the last line is the newest
    // pass's stamp. (A max() over fetched_at would let one
    // future-dated line anywhere in the tape certify a dead
    // collector as collecting, indefinitely.)
    let newest = view.newest_fetched_at;
    let now = now_unix();
    let age = newest.map(|n| (now - n).max(0));
    let future_dated = newest.is_some_and(|n| n > now + 300);
    let collecting = !future_dated
        && age.is_some_and(|a| a <= (app.collect_secs.max(60) * 2) as i64);
    let sched = app.collector_scheduler.snapshot();
    let progress_pct =
        ((view.settled_windows as f64 / READINESS_TARGET_WINDOWS as f64) * 100.0).min(100.0);
    let progress_pct = (progress_pct * 10.0).round() / 10.0;
    CollectorHealth {
        snapshots_on_disk: view.snaps_count,
        lines_on_disk: view.total_lines,
        dropped_lines: view.total_lines.saturating_sub(view.snaps_count),
        markets_seen: view.markets,
        settled_windows: view.settled_windows,
        conflicting_settlements: view.conflicting_settlements,
        newest_snapshot_age_secs: age,
        collect_interval_secs: app.collect_secs,
        collecting,
        series_failed_last_pass: SERIES_FAILED_LAST_PASS.load(std::sync::atomic::Ordering::Relaxed),
        series_failed_names: SERIES_FAILED_NAMES.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        series_total: active_series().len() * 2,
        auto_collect_enabled: sched.enabled,
        auto_collect_interval_secs: sched.interval_secs,
        auto_collect_backoff_secs: sched.backoff_secs,
        readiness_target_windows: READINESS_TARGET_WINDOWS,
        readiness_progress_pct: progress_pct,
    }
}

/// Resolve the `dataset=` query parameter to a tape file in the data
/// dir. Names are restricted to [a-z0-9-] so the parameter can never
/// walk out of the data dir. No parameter means the live collected tape.
/// The dataset a query names: "live" for the collected tape, or
/// the sanitised dataset= value. Responses carry the name so a
/// capture of fixture figures can never pass as live ones.
fn dataset_name(query: &str) -> String {
    if let Some(raw) = query_param(query, "dataset") {
        let name: String = raw
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if !name.is_empty() {
            return name;
        }
    }
    "live".to_string()
}

fn dataset_path(app: &App, query: &str) -> PathBuf {
    let name = dataset_name(query);
    if name != "live" {
        return app.data_dir.join(format!("{name}-snapshots.jsonl"));
    }
    app.data_dir.join("snapshots.jsonl")
}

/// The verdict as the named dataset may state it. The fixture is the
/// labelled example tape written for tests, never forward evidence.
fn dataset_verdict(verdict: &str, dataset: &str) -> String {
    if dataset == "fixture" && verdict.starts_with("PROVEN on forward evidence") {
        return "PROVEN on the example tape (not forward evidence, paper only - no orders placed)"
            .to_string();
    }
    verdict.to_string()
}

const SEC_HEADERS: &str = "X-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n";

fn write_all_vectored<W: std::io::Write>(
    stream: &mut W,
    mut head: &[u8],
    mut body: &[u8],
) -> std::io::Result<()> {
    use std::io::IoSlice;
    while !head.is_empty() || !body.is_empty() {
        let slices = [IoSlice::new(head), IoSlice::new(body)];
        let n = stream.write_vectored(&slices)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "failed to write response",
            ));
        }
        if n >= head.len() {
            let rest = n - head.len();
            head = &[];
            body = &body[rest.min(body.len())..];
        } else {
            head = &head[n..];
        }
    }
    Ok(())
}

#[inline(never)]
fn json_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{SEC_HEADERS}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[derive(serde::Serialize)]
struct JsonErr<'a> {
    error: &'a str,
}

#[derive(serde::Serialize)]
struct JsonOkErr<'a> {
    ok: bool,
    error: &'a str,
}

#[derive(serde::Serialize)]
struct LiveErrPayload<'a> {
    configured: bool,
    connected: bool,
    error: &'a str,
}

#[inline(never)]
fn json_err_response(status: u16, msg: &str) -> String {
    let body = serde_json::to_string(&JsonErr { error: msg }).unwrap_or_else(|_| "{}".to_string());
    json_response(status, &body)
}

#[inline(never)]
fn json_ok_err_response(status: u16, msg: &str) -> String {
    let body =
        serde_json::to_string(&JsonOkErr { ok: false, error: msg }).unwrap_or_else(|_| "{}".to_string());
    json_response(status, &body)
}

#[inline(never)]
fn publish_live_status_json(snap: &risk_rails::LiveStatusSnapshot) -> String {
    let ev = format!(
        "{{\"type\":\"live_status\",\"configured\":{},\"live_armed\":{},\"environment\":\"{}\",\"ts\":{}}}",
        snap.configured,
        snap.live_armed,
        if snap.environment == "prod" { "prod" } else { "demo" },
        now_unix(),
    );
    tape_hub().publish(ev);
    json_response(200, &serde_json::to_string(snap).unwrap_or_else(|_| "{}".into()))
}

#[inline(never)]
fn parse_json_object_body(
    body: &str,
    allow_empty: bool,
    non_object_msg: &str,
) -> Result<serde_json::Value, String> {
    if allow_empty && body.trim().is_empty() {
        return Ok(serde_json::Value::Object(Default::default()));
    }
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err(json_err_response(400, non_object_msg)),
        Err(e) => Err(json_err_response(400, &format!("invalid JSON body: {e}"))),
    }
}

#[inline(never)]
fn live_err_response(status: u16, list_key: Option<&str>, msg: &str) -> String {
    let err_json = serde_json::to_string(msg).unwrap_or_else(|_| "\"error\"".to_string());
    let body = match list_key {
        Some(k) => format!(
            "{{\"configured\":true,\"connected\":false,\"{k}\":[],\"error\":{err_json}}}"
        ),
        None => serde_json::to_string(&LiveErrPayload {
            configured: true,
            connected: false,
            error: msg,
        })
        .unwrap_or_else(|_| "{}".to_string()),
    };
    json_response(status, &body)
}

#[inline(never)]
fn json_with_dataset<T: serde::Serialize>(val: &T, dataset: &str) -> String {
    let raw = serde_json::to_string(val).unwrap_or_else(|_| "{}".to_string());
    let ds = serde_json::to_string(dataset).unwrap_or_else(|_| "\"\"".to_string());
    match raw.strip_prefix('{') {
        Some("}") => format!("{{\"dataset\":{ds}}}"),
        Some(rest) => format!("{{\"dataset\":{ds},{rest}"),
        None => raw,
    }
}

#[inline(never)]
fn json_with_dataset_pair<T: serde::Serialize, E: serde::Serialize>(
    val: &T,
    dataset: &str,
    extra_key: &str,
    extra_val: &E,
) -> String {
    let raw = serde_json::to_string(val).unwrap_or_else(|_| "{}".to_string());
    let ds = serde_json::to_string(dataset).unwrap_or_else(|_| "\"\"".to_string());
    let ev = serde_json::to_string(extra_val).unwrap_or_else(|_| "null".to_string());
    match raw.strip_prefix('{') {
        Some("}") => format!("{{\"dataset\":{ds},\"{extra_key}\":{ev}}}"),
        Some(rest) => format!("{{\"dataset\":{ds},\"{extra_key}\":{ev},{rest}"),
        None => raw,
    }
}

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n{SEC_HEADERS}ETag: {}\r\nVary: Accept-Encoding\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        index_etag(),
        body.len()
    )
}

#[inline]
fn accepts_gzip_header(raw: &str) -> bool {
    for part in raw.split(',') {
        let mut pieces = part.trim().split(';');
        let enc = pieces.next().unwrap_or("").trim();
        if enc.eq_ignore_ascii_case("gzip") {
            let zero_q = pieces.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .and_then(|q| q.trim().parse::<f64>().ok())
                    .is_some_and(|v| v <= 0.0)
            });
            if !zero_q {
                return true;
            }
        }
    }
    false
}

fn accepts_gzip(headers: &[(String, String)]) -> bool {
    let raw = headers
        .iter()
        .find(|(n, _)| n == "accept-encoding")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    accepts_gzip_header(raw)
}

fn etag_matches(headers: &[(String, String)], etag: &str) -> bool {
    let raw = headers
        .iter()
        .find(|(n, _)| n == "if-none-match")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    if raw.is_empty() {
        return false;
    }
    raw.split(',').any(|tok| {
        let t = tok.trim();
        t == "*" || t == etag || t.strip_prefix("W/").is_some_and(|w| w.trim() == etag)
    })
}

#[allow(clippy::too_many_arguments)]
fn write_cached_asset<W: std::io::Write>(
    stream: &mut W,
    method: &str,
    headers: &[(String, String)],
    content_type: &str,
    cache_control: &str,
    etag: &str,
    bytes: &[u8],
    is_gzip: bool,
) -> std::io::Result<()> {
    if etag_matches(headers, etag) {
        let hdr = format!(
            "HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nVary: Accept-Encoding\r\nCache-Control: {cache_control}\r\n{SEC_HEADERS}Connection: close\r\n\r\n"
        );
        return stream.write_all(hdr.as_bytes());
    }
    let is_head = method == "HEAD";
    let send_gzip = is_gzip && accepts_gzip(headers);
    let owned_raw: Option<Vec<u8>> = if is_gzip && !send_gzip {
        gzip_inflate::inflate_gzip(bytes)
    } else {
        None
    };
    let body: &[u8] = match owned_raw.as_deref() {
        Some(raw) => raw,
        None => bytes,
    };
    let enc_hdr = if send_gzip {
        "Content-Encoding: gzip\r\n"
    } else {
        ""
    };
    let hdr = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{enc_hdr}Vary: Accept-Encoding\r\nETag: {etag}\r\nCache-Control: {cache_control}\r\n{SEC_HEADERS}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if is_head {
        stream.write_all(hdr.as_bytes())
    } else {
        write_all_vectored(stream, hdr.as_bytes(), body)
    }
}

fn write_index_response<W: std::io::Write>(
    stream: &mut W,
    method: &str,
    headers: &[(String, String)],
) -> std::io::Result<()> {
    if accepts_gzip(headers) {
        write_cached_asset(
            stream,
            method,
            headers,
            "text/html; charset=utf-8",
            "no-cache",
            index_etag(),
            INDEX_HTML_GZ,
            true,
        )
    } else {
        write_cached_asset(
            stream,
            method,
            headers,
            "text/html; charset=utf-8",
            "no-cache",
            index_etag(),
            index_html_str().as_bytes(),
            false,
        )
    }
}

fn write_embedded_chunk<W: std::io::Write>(
    stream: &mut W,
    method: &str,
    headers: &[(String, String)],
    chunk: &embedded_chunks::EmbeddedChunk,
) -> std::io::Result<()> {
    write_cached_asset(
        stream,
        method,
        headers,
        chunk.content_type,
        "public, max-age=31536000, immutable",
        chunk.etag,
        chunk.bytes,
        chunk.is_gzip,
    )
}

/// Declared-body cap. Anything larger is refused without being
/// read: the allocation itself is the attack (a 1TB Content-Length
/// aborted this process, collector and all, before this cap).
const MAX_BODY_BYTES: usize = 1_000_000;

/// The hostname of a Host or Origin authority: "[::1]" for an IPv6
/// literal, otherwise the part before the port.
fn authority_host(authority: &str) -> &str {
    if authority.starts_with('[') {
        authority.split_inclusive(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    }
}

/// The same-site rule, shared with Fleet. Every request must name a
/// local Host unless OPENBELL_ALLOWED_HOSTS lists it. A state-changing
/// request (anything but GET or HEAD) is refused when a browser says
/// it came from another site, when its Origin names another authority
/// than its Host, or when it carries a body that is not JSON. Returns
/// the status and message of the refusal, or None to serve it.
fn refuse_request(
    method: &str,
    headers: &[(String, String)],
    body_len: usize,
    allowed: &[String],
) -> Option<(u16, &'static str)> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let host = header("host");
    let host_name = authority_host(host);
    let host_ok = host_name.eq_ignore_ascii_case("localhost")
        || matches!(host_name, "127.0.0.1" | "[::1]")
        || allowed.iter().any(|a| a.eq_ignore_ascii_case(host_name));
    if !host_ok {
        return Some((403, "this desk answers only on localhost unless OPENBELL_ALLOWED_HOSTS names this host"));
    }
    if matches!(method, "GET" | "HEAD") {
        return None;
    }
    if header("sec-fetch-site").eq_ignore_ascii_case("cross-site") {
        return Some((403, "cross-site requests are refused"));
    }
    let origin = header("origin");
    if !origin.is_empty() {
        let origin_authority = origin.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
        if !origin_authority.eq_ignore_ascii_case(host) {
            return Some((403, "cross-origin requests are refused"));
        }
    }
    let ct = header("content-type");
    if body_len > 0
        && !(ct.len() >= 16 && ct[..16].eq_ignore_ascii_case("application/json"))
    {
        return Some((415, "requests with a body must be Content-Type: application/json"));
    }
    None
}

/// Hard transport boundary for `/api/live/*`:
/// - Strictly loopback-only (`localhost` / `127.0.0.1` / `[::1]`), even if
///   `OPENBELL_ALLOWED_HOSTS` grants LAN read access.
/// - Every live mutation (`POST`/`DELETE`/etc.) requires explicit `X-OpenBell-CSRF` header.
fn refuse_live_request(
    method: &str,
    host: &str,
    headers: &[(String, String)],
) -> Option<(u16, &'static str)> {
    if !is_loopback_host(host) {
        return Some((
            403,
            "live Kalshi trading endpoints are restricted to localhost (loopback) only",
        ));
    }
    if !matches!(method, "GET" | "HEAD") {
        let csrf = headers
            .iter()
            .find(|(n, _)| n == "x-openbell-csrf")
            .map(|(_, v)| v.trim())
            .unwrap_or("");
        if csrf.is_empty() {
            return Some((
                403,
                "live Kalshi mutations require the X-OpenBell-CSRF header",
            ));
        }
    }
    None
}

#[inline]
fn find_crlfcrlf(haystack: &[u8]) -> Option<usize> {
    haystack.windows(4).position(|w| w == b"\r\n\r\n")
}

type RawRequest = (String, String, String, Vec<(String, String)>);
fn read_retry(stream: &mut std::net::TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn read_request_with_prefix(
    stream: &mut std::net::TcpStream,
    stack_buf: &mut [u8; 4096],
    mut stack_len: usize,
) -> Option<RawRequest> {
    const MAX_HEADER_BYTES: usize = 32_768;
    // One deadline for the whole request: a per-read timeout alone
    // is defeated by a client dripping a byte just under it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut overflow: Option<Vec<u8>> = None;
    let mut scanned = 0usize;

    let hdr_end = loop {
        let cur: &[u8] = match &overflow {
            Some(v) => v.as_slice(),
            None => &stack_buf[..stack_len],
        };
        let search_from = scanned.saturating_sub(3);
        if let Some(rel) = find_crlfcrlf(&cur[search_from..]) {
            break search_from + rel;
        }
        scanned = cur.len();
        if scanned > MAX_HEADER_BYTES {
            return Some((
                "__bad_request__".to_string(),
                String::new(),
                "HTTP headers too large".to_string(),
                Vec::new(),
            ));
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        if let Some(v) = &mut overflow {
            let mut chunk = [0u8; 4096];
            let n = read_retry(stream, &mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            v.extend_from_slice(&chunk[..n]);
        } else if stack_len < stack_buf.len() {
            let n = read_retry(stream, &mut stack_buf[stack_len..]).ok()?;
            if n == 0 {
                return None;
            }
            stack_len += n;
        } else {
            let mut v = Vec::with_capacity(8192);
            v.extend_from_slice(&stack_buf[..stack_len]);
            let mut chunk = [0u8; 4096];
            let n = read_retry(stream, &mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            v.extend_from_slice(&chunk[..n]);
            overflow = Some(v);
        }
    };

    let buf: &[u8] = match &overflow {
        Some(v) => v.as_slice(),
        None => &stack_buf[..stack_len],
    };

    let hdr_str = String::from_utf8_lossy(&buf[..hdr_end]);
    let mut lines = hdr_str.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length: Option<usize> = None;
    let mut headers: Vec<(String, String)> = Vec::with_capacity(8);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((raw_name, raw_value)) = line.split_once(':') {
            let trimmed_name = raw_name.trim();
            let canonical_name: Option<&'static str> = if trimmed_name.eq_ignore_ascii_case("host") {
                Some("host")
            } else if trimmed_name.eq_ignore_ascii_case("content-length") {
                Some("content-length")
            } else if trimmed_name.eq_ignore_ascii_case("content-type") {
                Some("content-type")
            } else if trimmed_name.eq_ignore_ascii_case("origin") {
                Some("origin")
            } else if trimmed_name.eq_ignore_ascii_case("sec-fetch-site") {
                Some("sec-fetch-site")
            } else if trimmed_name.eq_ignore_ascii_case("accept-encoding") {
                Some("accept-encoding")
            } else if trimmed_name.eq_ignore_ascii_case("if-none-match") {
                Some("if-none-match")
            } else if trimmed_name.eq_ignore_ascii_case("x-openbell-csrf") {
                Some("x-openbell-csrf")
            } else {
                None
            };
            let Some(name_str) = canonical_name else {
                continue;
            };
            let value = raw_value.trim().to_string();
            if name_str == "content-length" {
                let parsed = match value.parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => {
                        return Some((
                            "__bad_request__".to_string(),
                            String::new(),
                            "invalid Content-Length header".to_string(),
                            Vec::new(),
                        ));
                    }
                };
                if content_length.is_some_and(|prev| prev != parsed) {
                    return Some((
                        "__bad_request__".to_string(),
                        String::new(),
                        "conflicting Content-Length headers".to_string(),
                        Vec::new(),
                    ));
                }
                content_length = Some(parsed);
            }
            headers.push((name_str.to_string(), value));
        }
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Some((
            "__too_large__".to_string(),
            String::new(),
            String::new(),
            Vec::new(),
        ));
    }
    if content_length == 0 {
        return Some((method, path, String::new(), headers));
    }
    let body_start = hdr_end + 4;
    let mut body: Vec<u8> = Vec::with_capacity(content_length);
    let already = &buf[body_start..];
    if !already.is_empty() {
        let take = already.len().min(content_length);
        body.extend_from_slice(&already[..take]);
    }
    if body.len() < content_length {
        let mut rbuf = [0u8; 8192];
        while body.len() < content_length {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            let want = (content_length - body.len()).min(rbuf.len());
            match read_retry(stream, &mut rbuf[..want]) {
                Ok(0) => return None,
                Ok(n) => body.extend_from_slice(&rbuf[..n]),
                Err(_) => return None,
            }
        }
    }
    Some((method, path, String::from_utf8_lossy(&body).to_string(), headers))
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                let mut out = String::with_capacity(v.len());
                let bytes = v.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'%' && i + 2 < bytes.len() {
                        if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                                out.push(byte as char);
                                i += 3;
                                continue;
                            }
                        }
                    } else if bytes[i] == b'+' {
                        out.push(' ');
                        i += 1;
                        continue;
                    }
                    out.push(bytes[i] as char);
                    i += 1;
                }
                if !out.is_empty() {
                    return Some(out);
                }
            }
        }
    }
    None
}

fn text_download_response(content_type: &str, filename: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Disposition: attachment; filename=\"{filename}\"\r\n{SEC_HEADERS}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Markdown export of the daily report. Line 1 is strictly the venue's own
/// Day P&L line verbatim (never invented, never pushed below a heading).
fn export_report_md(app: &App, dpath: &Path, dataset: &str) -> String {
    let raw = daily_report(app);
    let line1 = raw
        .lines()
        .next()
        .unwrap_or("Day P&L (venue): unavailable");
    let health = collector_health(app);
    let (sim, scores, _, _) = app.analysis_shared(dpath);
    let mut out = String::new();
    out.push_str(line1);
    out.push_str("\n\n## OpenBell Daily Paper Report\n\n");
    out.push_str(&format!(
        "- **Mode:** Shadow paper-only desk (zero order-placement paths)\n\
         - **Dataset:** {dataset}\n\
         - **Paper pot:** {} (started {})\n\
         - **Collector:** {} ({} snapshots on disk, {} markets, {} settled windows)\n\n",
        fmt_money(sim.paper_equity_dollars),
        fmt_money(sim.paper_pot_start),
        if health.collecting {
            "collecting"
        } else {
            "NOT collecting"
        },
        health.snapshots_on_disk,
        health.markets_seen,
        health.settled_windows,
    ));
    out.push_str("## Slots (Paper)\n\n| Slot | Strategy | Status | Strikes | Stint Windows | Stint P&L | Slot P&L | Equity |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for s in &sim.slots {
        out.push_str(&format!(
            "| {} | {} | {} | {}/{} | {} | {} | {} | {} |\n",
            s.slot + 1,
            s.strategy,
            s.status,
            s.strikes,
            STRIKE_LIMIT,
            s.stint_windows,
            fmt_money(s.stint_pnl_dollars),
            fmt_money(s.paper_pnl_dollars),
            fmt_money(s.equity_dollars),
        ));
    }
    out.push_str("\n## Strategy Verdicts (Paper)\n\n| Strategy | Side | Windows Traded | Mean c/Contract | Verdict |\n| --- | --- | --- | --- | --- |\n");
    for r in scores.iter() {
        let mean = r
            .mean_cents_per_contract
            .map(|m| format!("{m:+.2}c"))
            .unwrap_or_else(|| "n/a".to_string());
        let verdict = dataset_verdict(&r.verdict, dataset);
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            r.strategy, r.side, r.windows_traded, mean, verdict
        ));
    }
    out.push_str("\n## Verbatim Report\n\n```text\n");
    out.push_str(&raw);
    out.push_str("\n```\n");
    out
}

/// CSV export of the daily report. Line 1 preserves the exact venue Day P&L
/// line verbatim before the RFC-4180 CSV header and structured rows.
fn export_report_csv(app: &App, dpath: &Path, dataset: &str) -> String {
    let raw = daily_report(app);
    let line1 = raw
        .lines()
        .next()
        .unwrap_or("Day P&L (venue): unavailable");
    let health = collector_health(app);
    let (sim, scores, _, _) = app.analysis_shared(dpath);
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(line1);
    out.push('\n');
    out.push_str("section,name,side,windows_traded,mean_cents,paper_pnl_dollars,equity_dollars,strikes,status_or_verdict\n");
    out.push_str(&format!(
        "venue,day_pnl,,,,,,,{}\n",
        csv_escape(line1)
    ));
    out.push_str(&format!(
        "collector,tape,,{},,,,,{}\n",
        health.settled_windows,
        csv_escape(&format!(
            "collecting={} snapshots={} markets={}",
            health.collecting,
            health.snapshots_on_disk,
            health.markets_seen,
        ))
    ));
    out.push_str(&format!(
        "paper_pot,total,,,,{:.2},{:.2},,paper_only\n",
        sim.paper_equity_dollars - sim.paper_pot_start,
        sim.paper_equity_dollars
    ));
    for s in &sim.slots {
        out.push_str(&format!(
            "slot,{},{},{},,{:.2},{:.2},{},{}\n",
            csv_escape(&format!("slot-{}", s.slot + 1)),
            csv_escape(&s.strategy),
            s.windows_traded,
            s.paper_pnl_dollars,
            s.equity_dollars,
            s.strikes,
            csv_escape(&s.status),
        ));
    }
    for r in &sim.retired {
        out.push_str(&format!(
            "retired,{},{},{},,{:.2},,{},{}\n",
            csv_escape(&format!("slot-{}", r.slot + 1)),
            csv_escape(&r.strategy),
            r.windows_traded,
            r.paper_pnl_dollars,
            r.strikes,
            csv_escape(&format!("rotated_to:{}", r.rotated_to)),
        ));
    }
    for st in scores.iter() {
        let mean = st
            .mean_cents_per_contract
            .map(|m| format!("{m:.4}"))
            .unwrap_or_default();
        let verdict = dataset_verdict(&st.verdict, dataset);
        out.push_str(&format!(
            "strategy,{},{},{},{},,,,{}\n",
            csv_escape(&st.strategy),
            csv_escape(&st.side),
            st.windows_traded,
            mean,
            csv_escape(&verdict),
        ));
    }
    out
}

/// True Server-Sent Events stream for `/api/tape/stream`. Emits an initial
/// snapshot frame, then real-time `tape` frames on every collector pass plus
/// periodic keepalive comments until the client disconnects.
fn serve_sse_stream(stream: &mut std::net::TcpStream, app: &App) {
    let Some(rx) = tape_hub().subscribe() else {
        let resp = json_response(503, "{\"error\":\"too many active tape stream subscribers\"}");
        let _ = stream.write_all(resp.as_bytes());
        return;
    };
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
    let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n";
    if stream.write_all(headers.as_bytes()).is_err() {
        return;
    }
    let health = collector_health(app);
    let view = app.tape(&app.data_dir.join("snapshots.jsonl"));
    let open_markets: Vec<&Snapshot> = view
        .grouped
        .values()
        .filter_map(|w| w.last())
        .filter(|m| m.result.is_empty() && matches!(m.status.as_ref(), "open" | "active"))
        .take(25)
        .collect();
    #[derive(serde::Serialize)]
    struct SseSnapshot<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        version: &'static str,
        collector: &'a CollectorHealth,
        open_markets: &'a [&'a Snapshot],
        series: Vec<String>,
        ts: i64,
    }
    let initial = serde_json::to_string(&SseSnapshot {
        kind: "snapshot",
        version: env!("CARGO_PKG_VERSION"),
        collector: &health,
        open_markets: &open_markets,
        series: active_series(),
        ts: now_unix(),
    })
    .unwrap_or_else(|_| "{}".to_string());
    if stream
        .write_all(format!("event: snapshot\ndata: {initial}\n\n").as_bytes())
        .is_err()
        || stream.flush().is_err()
    {
        return;
    }
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok((ev_name, payload)) => {
                if payload.is_empty() {
                    continue;
                }
                let ev = if ev_name.is_empty() { "tape" } else { ev_name.as_str() };
                if stream
                    .write_all(format!("event: {ev}\ndata: {payload}\n\n").as_bytes())
                    .is_err()
                    || stream.flush().is_err()
                {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if stream.write_all(b": keepalive\n\n").is_err() || stream.flush().is_err() {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn market_history_from(app: &App, path: &Path, ticker: &str) -> Vec<Snapshot> {
    let view = app.tape(path);
    let Some(pts) = view.grouped.get(ticker) else {
        return Vec::new();
    };
    let start = pts.len().saturating_sub(240);
    pts[start..].to_vec()
}

fn strategy_source_for(dir: &Path, name: &str) -> Option<(String, String, String)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "rn").unwrap_or(false))
        .filter(|p| is_confined_regular_file(dir, p))
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(s) = Strategy::load(&path) {
            if s.name == name {
                let file = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                let source = std::fs::read_to_string(&path).unwrap_or_default();
                return Some((file, s.side, source));
            }
        }
    }
    None
}

fn resolve_dataset_checked(
    app: &App,
    query: &str,
    body_dataset: Option<&str>,
) -> Result<(String, PathBuf), String> {
    let (dataset, dpath) = resolve_dataset(app, query, body_dataset);
    match std::fs::symlink_metadata(&dpath) {
        Ok(meta) => {
            if meta.file_type().is_symlink()
                || !meta.file_type().is_file()
                || !is_confined_regular_file(&app.data_dir, &dpath)
            {
                return Err(json_err_response(
                    403,
                    &format!("dataset path is a symlink or escapes data_dir: {dataset}"),
                ));
            }
        }
        Err(_) => {
            if dataset != "live" {
                return Err(json_err_response(
                    404,
                    &format!("no such dataset: {dataset}"),
                ));
            }
        }
    }
    Ok((dataset, dpath))
}

fn resolve_dataset(app: &App, query: &str, body_dataset: Option<&str>) -> (String, PathBuf) {
    let qname = dataset_name(query);
    if qname != "live" {
        return (qname, dataset_path(app, query));
    }
    if let Some(raw) = body_dataset {
        let name: String = raw
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if !name.is_empty() && name != "live" {
            return (name.clone(), app.data_dir.join(format!("{name}-snapshots.jsonl")));
        }
    }
    ("live".to_string(), app.data_dir.join("snapshots.jsonl"))
}

#[allow(dead_code)]
fn handle(app: &App, method: &str, path: &str) -> String {
    handle_with_body(app, method, path, "")
}

fn home_json_body(app: &App) -> String {
    let (sim, _, lib_errors, _) = app.analysis_shared(&app.data_dir.join("snapshots.jsonl"));
    let live_snap = {
        let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        st.rails.status_snapshot(&st.config, st.connected)
    };
    let mode_str = if live_snap.live_armed && live_snap.configured {
        "live — Kalshi v2 armed under hard risk rails"
    } else if live_snap.configured {
        "live_disarmed — Kalshi v2 configured (disarmed; arm explicitly to route orders)"
    } else {
        "paper — Kalshi v2 ready (configure credentials in Settings to connect live)"
    };
    #[derive(Serialize)]
    struct LibErrRef<'a> {
        file: &'a str,
        error: &'a str,
    }
    #[derive(Serialize)]
    struct HomeBody<'a> {
        account: AccountState,
        collector: CollectorHealth,
        live: risk_rails::LiveStatusSnapshot,
        stage_target: StageProgress,
        library_errors: Vec<LibErrRef<'a>>,
        version: &'static str,
        mode: &'a str,
        series: Vec<String>,
        sse: bool,
        orderbook_l2: bool,
        capabilities: &'static [&'static str],
    }
    let body = HomeBody {
        account: account_state(),
        collector: collector_health(app),
        live: live_snap,
        stage_target: stage_progress(&sim),
        library_errors: lib_errors
            .iter()
            .map(|(file, error)| LibErrRef { file, error })
            .collect(),
        version: env!("CARGO_PKG_VERSION"),
        mode: mode_str,
        series: active_series(),
        sse: true,
        orderbook_l2: true,
        capabilities: &[
            "sse",
            "calibration",
            "walkforward",
            "montecarlo",
            "sandbox",
            "export",
            "recalibration",
            "scale_plan",
            "tournament",
            "edge_audit",
            "auto_allocator",
            "auto_collector",
            "live_kalshi",
            "risk_rails",
            "shadow_runner",
            "orderbook_l2",
            "strategy_v2",
            "synthesizer",
            "live_reconcile",
            "execution_health",
        ],
    };
    serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string())
}

fn home_json_shared(app: &App) -> Arc<String> {
    use std::hash::{Hash, Hasher};
    let now = now_unix();
    let dpath = app.data_dir.join("snapshots.jsonl");
    let (len, mtime) = tape_stat(&dpath).unwrap_or((0, 0));
    let key = strategies_dir_key(&app.strategies_dir, app.paper_pot);
    let (live_armed, configured, killed, last_sync, sync_stale) = {
        let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        let cfg_ok = st.config.is_configured();
        (
            st.rails.live_armed,
            cfg_ok,
            st.rails.killed,
            st.rails.last_venue_sync_epoch,
            cfg_ok && st.rails.is_venue_sync_stale(now),
        )
    };
    let sched = app.collector_scheduler.snapshot();
    let in_flight = PASSES_IN_FLIGHT.load(std::sync::atomic::Ordering::Relaxed);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    len.hash(&mut h);
    mtime.hash(&mut h);
    key.hash(&mut h);
    live_armed.hash(&mut h);
    configured.hash(&mut h);
    killed.hash(&mut h);
    last_sync.hash(&mut h);
    sync_stale.hash(&mut h);
    in_flight.hash(&mut h);
    sched.enabled.hash(&mut h);
    sched.runs_total.hash(&mut h);
    sched.interval_secs.hash(&mut h);
    sched.backoff_secs.hash(&mut h);
    let state_hash = h.finish();

    {
        let guard = app.home_cache.read().unwrap_or_else(|p| p.into_inner());
        if let Some((sec, sh, body)) = &*guard {
            if *sec == now && *sh == state_hash {
                return body.clone();
            }
        }
    }

    let body = Arc::new(home_json_body(app));
    *app.home_cache.write().unwrap_or_else(|p| p.into_inner()) =
        Some((now, state_hash, body.clone()));
    body
}

#[inline]
fn write_json_bytes_inline<W: std::io::Write>(
    stream: &mut W,
    status: u16,
    body: &[u8],
    set_blocking_on_partial: Option<&std::net::TcpStream>,
    keep_alive: bool,
) -> Option<()> {
    use std::io::{IoSlice, Write as _};
    let mut hdr_buf = [0u8; 256];
    let mut cursor = std::io::Cursor::new(&mut hdr_buf[..]);
    let conn = if keep_alive { "keep-alive" } else { "close" };
    let _ = write!(
        cursor,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{SEC_HEADERS}Content-Length: {}\r\nConnection: {conn}\r\n\r\n",
        body.len()
    );
    let hdr_len = cursor.position() as usize;
    let mut head = &hdr_buf[..hdr_len];
    let mut rem_body = body;
    let mut wrote_any = false;
    while !head.is_empty() || !rem_body.is_empty() {
        let slices = [IoSlice::new(head), IoSlice::new(rem_body)];
        match stream.write_vectored(&slices) {
            Ok(0) => return None,
            Ok(n) => {
                wrote_any = true;
                if n >= head.len() {
                    let rest = n - head.len();
                    head = &[];
                    rem_body = &rem_body[rest.min(rem_body.len())..];
                } else {
                    head = &head[n..];
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if !wrote_any {
                    return None;
                }
                if let Some(sock) = set_blocking_on_partial {
                    let _ = sock.set_nonblocking(false);
                    return write_all_vectored(stream, head, rem_body).ok();
                }
                return None;
            }
            Err(_) => return None,
        }
    }
    Some(())
}

#[inline]
fn hot_ttl() -> std::time::Duration {
    #[cfg(test)]
    {
        std::time::Duration::from_millis(0)
    }
    #[cfg(not(test))]
    {
        std::time::Duration::from_millis(1000)
    }
}

#[inline]
fn try_serve_fast_get(
    stream: &mut std::net::TcpStream,
    app: &App,
    path: &str,
    warm_only: bool,
    keep_alive: bool,
) -> bool {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let sock_ref: Option<&std::net::TcpStream> = if warm_only {
        Some(unsafe { &*(stream as *const std::net::TcpStream) })
    } else {
        None
    };
    let ttl = hot_ttl();
    let now_inst = if !ttl.is_zero() && !warm_only && query.is_empty() {
        Some(std::time::Instant::now())
    } else {
        None
    };
    match route {
        "/api/version" => {
            if !keep_alive {
                static VERSION_HTTP_CLOSE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
                let raw = VERSION_HTTP_CLOSE.get_or_init(|| {
                    let up = updater();
                    let body = format!(
                        "{{\"name\":\"openbell-rs\",\"version\":\"{}\",\"release_url\":\"https://github.com/{}/releases/tag/v{}\"}}",
                        env!("CARGO_PKG_VERSION"),
                        up.cfg.repo,
                        env!("CARGO_PKG_VERSION")
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{SEC_HEADERS}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .into_bytes()
                });
                return stream.write_all(raw).is_ok();
            }
            static VERSION_JSON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let body = VERSION_JSON.get_or_init(|| {
                let up = updater();
                format!(
                    "{{\"name\":\"openbell-rs\",\"version\":\"{}\",\"release_url\":\"https://github.com/{}/releases/tag/v{}\"}}",
                    env!("CARGO_PKG_VERSION"),
                    up.cfg.repo,
                    env!("CARGO_PKG_VERSION")
                )
            });
            write_json_bytes_inline(stream, 200, body.as_bytes(), sock_ref, keep_alive).is_some()
        }
        "/api/home" => {
            if let Some(now) = now_inst {
                if let Ok(guard) = app.live_hot_cache.try_read() {
                    if guard.until.is_some_and(|u| now <= u) {
                        if !keep_alive {
                            if let Some(raw) = &guard.home_http_close {
                                let raw = raw.clone();
                                drop(guard);
                                return stream.write_all(&raw).is_ok();
                            }
                        } else if let Some(b) = &guard.home_json {
                            let body = b.clone();
                            drop(guard);
                            return write_json_bytes_inline(
                                stream,
                                200,
                                body.as_bytes(),
                                sock_ref,
                                keep_alive,
                            )
                            .is_some();
                        }
                    }
                }
            }
            let body = if warm_only {
                let now = now_unix();
                let Ok(guard) = app.home_cache.try_read() else {
                    return false;
                };
                match &*guard {
                    Some((sec, _, b)) if *sec == now => b.clone(),
                    _ => return false,
                }
            } else {
                let b = home_json_shared(app);
                if let Some(now) = now_inst {
                    let close_raw = build_http_close_json(b.as_bytes());
                    if let Ok(mut guard) = app.live_hot_cache.write() {
                        if guard.until.is_none_or(|u| now > u) {
                            *guard = LiveHotSnapshot::default();
                            guard.until = Some(now + ttl);
                        }
                        guard.home_http_close = Some(close_raw);
                        guard.home_json = Some(b.clone());
                    }
                }
                b
            };
            write_json_bytes_inline(stream, 200, body.as_bytes(), sock_ref, keep_alive).is_some()
        }
        "/api/markets" => {
            if let Some(now) = now_inst {
                if let Ok(guard) = app.live_hot_cache.try_read() {
                    if guard.until.is_some_and(|u| now <= u) {
                        if let Some(b) = &guard.markets_json {
                            let body = b.clone();
                            drop(guard);
                            return write_json_bytes_inline(
                                stream,
                                200,
                                body.as_bytes(),
                                sock_ref,
                                keep_alive,
                            )
                            .is_some();
                        }
                    }
                }
            }
            if warm_only && query.is_empty() {
                let Ok(guard) = app.tape_cache.try_read() else {
                    return false;
                };
                let Some(e) = &*guard else {
                    return false;
                };
                let Some((len, mtime)) = tape_stat(&app.live_snapshots_path) else {
                    return false;
                };
                if e.path != app.live_snapshots_path || e.len != len || e.mtime != mtime {
                    return false;
                }
                let latest = e.latest_json.clone();
                drop(guard);
                return write_json_bytes_inline(
                    stream,
                    200,
                    latest.as_bytes(),
                    sock_ref,
                    keep_alive,
                )
                .is_some();
            }
            if warm_only {
                return false;
            }
            match resolve_dataset_checked(app, query, None) {
                Ok((_, dpath)) => {
                    let latest = app.tape_latest_json(&dpath);
                    if let Some(now) = now_inst {
                        if let Ok(mut guard) = app.live_hot_cache.write() {
                            if guard.until.is_none_or(|u| now > u) {
                                *guard = LiveHotSnapshot::default();
                                guard.until = Some(now + ttl);
                            }
                            guard.markets_json = Some(latest.clone());
                        }
                    }
                    let _ = write_json_bytes_inline(
                        stream,
                        200,
                        latest.as_bytes(),
                        None,
                        keep_alive,
                    );
                }
                Err(resp) => {
                    let _ = stream.write_all(resp.as_bytes());
                    return false;
                }
            }
            true
        }
        "/api/strategies" | "/api/slots" | "/api/equity" => {
            if let Some(now) = now_inst {
                if let Ok(guard) = app.live_hot_cache.try_read() {
                    if guard.until.is_some_and(|u| now <= u) {
                        if !keep_alive {
                            let hit_raw = match route {
                                "/api/strategies" => &guard.strategies_http_close,
                                "/api/slots" => &guard.slots_http_close,
                                _ => &guard.equity_http_close,
                            };
                            if let Some(raw) = hit_raw {
                                let raw = raw.clone();
                                drop(guard);
                                return stream.write_all(&raw).is_ok();
                            }
                        } else {
                            let hit = match route {
                                "/api/strategies" => &guard.strategies_json,
                                "/api/slots" => &guard.slots_json,
                                _ => &guard.equity_json,
                            };
                            if let Some(b) = hit {
                                let body = b.clone();
                                drop(guard);
                                return write_json_bytes_inline(
                                    stream,
                                    200,
                                    body.as_bytes(),
                                    sock_ref,
                                    keep_alive,
                                )
                                .is_some();
                            }
                        }
                    }
                }
            }
            if warm_only && query.is_empty() {
                let Ok(guard) = app.desk_json_cache.try_read() else {
                    return false;
                };
                let key = strategies_dir_key(&app.strategies_dir, app.paper_pot);
                let Some((len, mtime)) = tape_stat(&app.live_snapshots_path) else {
                    return false;
                };
                let Some(e) = guard.iter().find(|e| {
                    e.path == app.live_snapshots_path
                        && e.len == len
                        && e.mtime == mtime
                        && e.key == key
                        && e.dataset == "live"
                }) else {
                    return false;
                };
                let body = match route {
                    "/api/strategies" => e.strategies_json.clone(),
                    "/api/slots" => e.slots_json.clone(),
                    _ => e.equity_json.clone(),
                };
                drop(guard);
                return write_json_bytes_inline(
                    stream,
                    200,
                    body.as_bytes(),
                    sock_ref,
                    keep_alive,
                )
                .is_some();
            }
            if warm_only {
                return false;
            }
            match resolve_dataset_checked(app, query, None) {
                Ok((dataset, dpath)) => {
                    let (s_json, sl_json, eq_json) = app.desk_json_shared(&dpath, &dataset);
                    if let Some(now) = now_inst {
                        let s_raw = build_http_close_json(s_json.as_bytes());
                        let sl_raw = build_http_close_json(sl_json.as_bytes());
                        let eq_raw = build_http_close_json(eq_json.as_bytes());
                        if let Ok(mut guard) = app.live_hot_cache.write() {
                            if guard.until.is_none_or(|u| now > u) {
                                *guard = LiveHotSnapshot::default();
                            }
                            guard.until = Some(now + ttl);
                            guard.strategies_http_close = Some(s_raw);
                            guard.slots_http_close = Some(sl_raw);
                            guard.equity_http_close = Some(eq_raw);
                            guard.strategies_json = Some(s_json.clone());
                            guard.slots_json = Some(sl_json.clone());
                            guard.equity_json = Some(eq_json.clone());
                        }
                    }
                    let body = match route {
                        "/api/strategies" => &s_json,
                        "/api/slots" => &sl_json,
                        _ => &eq_json,
                    };
                    let _ = write_json_bytes_inline(stream, 200, body.as_bytes(), None, keep_alive);
                }
                Err(resp) => {
                    let _ = stream.write_all(resp.as_bytes());
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

#[inline]
fn try_handle_inline_get(
    stream: &mut std::net::TcpStream,
    buf: &[u8],
    app: &App,
    allowed_hosts: &[String],
    allow_keep_alive: bool,
) -> Option<bool> {
    if !buf.starts_with(b"GET /api/") {
        return None;
    }
    let hdr_end = find_crlfcrlf(buf)?;
    let hdr_str = std::str::from_utf8(&buf[..hdr_end]).ok()?;
    let mut lines = hdr_str.split("\r\n");
    let req_line = lines.next()?;
    let mut parts = req_line.split_whitespace();
    let (Some("GET"), Some(path), Some(_ver)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    if path.starts_with("/api/live")
        || path.starts_with("/api/runner")
        || path.starts_with("/api/tape/stream")
        || path.starts_with("/api/stream")
        || path.starts_with("/api/orderbook")
        || path.starts_with("/api/spot")
        || path.starts_with("/api/analytics/execution-health")
        || path.starts_with("/api/analytics/quarantine")
    {
        return None;
    }
    let mut host_ok = false;
    let mut want_keep_alive = false;
    for l in lines {
        if let Some((n, v)) = l.split_once(':') {
            let tn = n.trim();
            if tn.eq_ignore_ascii_case("host") {
                let hn = authority_host(v.trim());
                host_ok = hn.eq_ignore_ascii_case("localhost")
                    || matches!(hn, "127.0.0.1" | "[::1]")
                    || allowed_hosts.iter().any(|a| a.eq_ignore_ascii_case(hn));
            } else if tn.eq_ignore_ascii_case("connection") {
                let tv = v.trim();
                if tv.eq_ignore_ascii_case("keep-alive") {
                    want_keep_alive = true;
                } else if tv.eq_ignore_ascii_case("close") {
                    want_keep_alive = false;
                }
            } else if tn.eq_ignore_ascii_case("content-length") {
                return None;
            }
        }
    }
    if !host_ok {
        return None;
    }
    let keep_alive = allow_keep_alive && want_keep_alive && buf.len() == hdr_end + 4;
    if try_serve_fast_get(stream, app, path, false, keep_alive) {
        Some(keep_alive)
    } else {
        None
    }
}

fn handle_with_body(app: &App, method: &str, path: &str, body: &str) -> String {
    let (route, query) = match path.split_once('?') {
        Some((r, q)) => (r, q),
        None => (path, ""),
    };
    match (method, route) {
        ("GET", "/") => html_response(index_html_str()),
        ("GET", "/api/home") => json_response(200, &home_json_body(app)),
        ("GET", "/api/markets") => {
            let (_dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let view = app.tape(&dpath);
            json_response(200, &view.latest_json)
        }
        ("GET", "/api/markets/history") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let Some(ticker) = query_param(query, "ticker") else {
                return json_response(400, "{\"error\":\"missing ticker parameter\"}");
            };
            let points = market_history_from(app, &dpath, &ticker);
            let body = format!(
                "{{\"ticker\":{},\"dataset\":{},\"points\":{}}}",
                serde_json::to_string(&ticker).unwrap_or_else(|_| "\"\"".into()),
                serde_json::to_string(&dataset).unwrap_or_else(|_| "\"\"".into()),
                serde_json::to_string(&points).unwrap_or_else(|_| "[]".into()),
            );
            json_response(200, &body)
        }
        ("GET", "/api/strategy/source") => {
            let Some(name) = query_param(query, "name") else {
                return json_response(400, "{\"error\":\"missing name parameter\"}");
            };
            match strategy_source_for(&app.strategies_dir, &name) {
                Some((file, side, source)) => {
                    let body = format!(
                        "{{\"strategy\":{},\"file\":{},\"side\":{},\"source\":{}}}",
                        serde_json::to_string(&name).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&file).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&side).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&source).unwrap_or_else(|_| "\"\"".into()),
                    );
                    json_response(200, &body)
                }
                None => json_err_response(404, &format!("no such strategy: {name}")),
            }
        }
        ("POST", "/api/collect") => {
            app.collector_scheduler.mark_in_flight(true);
            let res = collect_once_detailed(
                &app.data_dir,
                Some(&app.orderbook),
                Some(&app.spot_feed),
            );
            let _ = app.collector_scheduler.record_pass_result(&res);
            match res {
                Ok(rep) => {
                    if rep.written > 0 {
                        app.runner.notify_tick();
                    }
                    #[derive(serde::Serialize)]
                    struct CollectResp {
                        snapshots_written: usize,
                        #[serde(skip_serializing_if = "is_zero_usize")]
                        deduplicated: usize,
                        #[serde(skip_serializing_if = "Vec::is_empty")]
                        errors: Vec<String>,
                        #[serde(skip_serializing_if = "std::ops::Not::not")]
                        rate_limited: bool,
                    }
                    fn is_zero_usize(n: &usize) -> bool {
                        *n == 0
                    }
                    let body = serde_json::to_string(&CollectResp {
                        snapshots_written: rep.written,
                        deduplicated: rep.deduplicated,
                        errors: rep.errors,
                        rate_limited: rep.rate_limited,
                    })
                    .unwrap_or_else(|_| "{}".to_string());
                    json_response(200, &body)
                }
                Err((e, _)) => json_err_response(502, &e),
            }
        }
        ("GET", "/api/collector/status") => {
            json_response(200, &app.collector_status_json())
        }
        ("POST", "/api/collector/schedule") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Collector schedule request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let enabled = match parsed.get("enabled") {
                None => None,
                Some(v) => match v.as_bool() {
                    Some(b) => Some(b),
                    None => {
                        return json_response(
                            400,
                            "{\"error\":\"enabled must be a boolean\"}",
                        );
                    }
                },
            };
            let interval_secs = match parsed.get("interval_secs") {
                None => None,
                Some(v) => match v.as_u64().or_else(|| {
                    v.as_i64().map(|n| n.max(0) as u64).or_else(|| {
                        v.as_f64()
                            .filter(|f| f.is_finite())
                            .map(|f| f.round().max(0.0).min(AUTO_COLLECT_MAX_SECS as f64) as u64)
                    })
                }) {
                    Some(secs) => Some(secs.clamp(AUTO_COLLECT_MIN_SECS, AUTO_COLLECT_MAX_SECS)),
                    None => {
                        return json_response(
                            400,
                            "{\"error\":\"interval_secs must be a finite number (30..=900)\"}",
                        );
                    }
                },
            };
            if enabled.is_none() && interval_secs.is_none() {
                return json_response(
                    400,
                    "{\"error\":\"provide enabled (bool) and/or interval_secs (30..=900)\"}",
                );
            }
            let st = app.collector_scheduler.update_schedule(enabled, interval_secs);
            let event = format!(
                "{{\"type\":\"schedule\",\"enabled\":{},\"interval_secs\":{},\"effective_interval_secs\":{},\"backoff_secs\":{},\"ts\":{}}}",
                st.enabled,
                st.interval_secs,
                st.effective_interval_secs(),
                st.backoff_secs,
                now_unix(),
            );
            tape_hub().publish(event);
            json_response(200, &app.collector_status_json())
        }
        ("GET", "/api/strategies") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (strategies_json, _, _) = app.desk_json_shared(&dpath, &dataset);
            json_response(200, &strategies_json)
        }
        ("GET", "/api/slots") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (_, slots_json, _) = app.desk_json_shared(&dpath, &dataset);
            json_response(200, &slots_json)
        }
        ("GET", "/api/equity") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (_, _, equity_json) = app.desk_json_shared(&dpath, &dataset);
            json_response(200, &equity_json)
        }
        ("GET", "/api/report") => {
            let report = daily_report(app);
            let body = format!(
                "{{\"report\":{}}}",
                serde_json::to_string(&report).unwrap_or_else(|_| "\"\"".into())
            );
            json_response(200, &body)
        }
        ("GET", "/api/export/report") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let fmt = query_param(query, "format").unwrap_or_else(|| "md".to_string());
            match fmt.to_ascii_lowercase().as_str() {
                "md" | "markdown" => text_download_response(
                    "text/markdown; charset=utf-8",
                    "openbell-daily-report.md",
                    &export_report_md(app, &dpath, &dataset),
                ),
                "csv" => text_download_response(
                    "text/csv; charset=utf-8",
                    "openbell-daily-report.csv",
                    &export_report_csv(app, &dpath, &dataset),
                ),
                other => json_err_response(
                    400,
                    &format!("unsupported export format: {other} (use md or csv)"),
                ),
            }
        }
        ("GET", "/api/analytics/calibration") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (cal_json, _, _, _, _) = app.analytics_shared(&dpath, &dataset);
            json_response(200, &cal_json)
        }
        ("GET", "/api/analytics/walkforward") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (_, wf_json, _, _, _) = app.analytics_shared(&dpath, &dataset);
            json_response(200, &wf_json)
        }
        ("GET", "/api/analytics/recalibration") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let slippage = query_param(query, "slippage_cents")
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 5.0))
                .unwrap_or(1.0);
            let payload = app.recalibration_shared(&dpath, &dataset, slippage);
            json_response(200, &payload)
        }
        ("GET", "/api/allocator") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let payload =
                app.allocator_shared(&dpath, &dataset, &auto_allocator::AutoAllocatorRequest::default());
            json_response(200, &payload)
        }
        ("POST", "/api/allocator/plan") => {
            let parsed = match parse_json_object_body(
                body,
                true,
                "Allocator request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (dataset, dpath) = match resolve_dataset_checked(
                app,
                query,
                parsed.get("dataset").and_then(|v| v.as_str()),
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let req: auto_allocator::AutoAllocatorRequest = if body.trim().is_empty() {
                Default::default()
            } else {
                match serde_json::from_str(body) {
                    Ok(r) => r,
                    Err(e) => {
                        return json_err_response(400, &format!("invalid allocator parameters: {e}"));
                    }
                }
            };
            let payload = app.allocator_shared(&dpath, &dataset, &req);
            json_response(200, &payload)
        }
        ("GET", "/api/analytics/tournament") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let slippage = query_param(query, "slippage_cents")
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 5.0))
                .unwrap_or(1.0);
            let payload = app.tournament_shared(&dpath, &dataset, slippage);
            json_response(200, &payload)
        }
        ("GET", "/api/analytics/edge-audit") => {
            let (dataset, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let slippage = query_param(query, "slippage_cents")
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 5.0))
                .unwrap_or(1.0);
            let payload = app.edge_audit_shared(&dpath, &dataset, slippage);
            json_response(200, &payload)
        }
        ("GET", "/api/analytics/execution-health") => {
            let (_, dpath) = match resolve_dataset_checked(app, query, None) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let rep = app.execution_health_for_tape(&dpath, true);
            app.runner.set_quarantine_state(rep.quarantine_state.clone());
            json_response(200, &serde_json::to_string(&rep).unwrap_or_else(|_| "{}".into()))
        }
        ("POST", "/api/analytics/quarantine/override") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Quarantine override request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let Some(strategy) = parsed.get("strategy").and_then(|v| v.as_str()) else {
                return json_ok_err_response(400, "missing string field \"strategy\"");
            };
            let Some(action) = parsed.get("action").and_then(|v| v.as_str()) else {
                return json_ok_err_response(400, "missing string field \"action\"");
            };
            let reason = match parsed.get("reason") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(s)) => Some(s.as_str()),
                Some(_) => {
                    return json_ok_err_response(400, "optional field \"reason\" must be a string");
                }
            };
            let clean_strat = strategy.trim();
            let (strats, _) = load_strategies_with_errors(&app.strategies_dir);
            let known_strat = strats.iter().any(|s| {
                s.name == clean_strat
                    || s.name == clean_strat.replace('-', "_")
                    || s.name == clean_strat.replace('_', "-")
            }) || execution_audit::strategy_exists_in_audit(&app.data_dir, clean_strat);
            if !known_strat {
                return json_ok_err_response(
                    400,
                    &format!("unknown strategy \"{clean_strat}\""),
                );
            }
            match execution_audit::apply_quarantine_override(
                &app.data_dir,
                clean_strat,
                action,
                reason,
            ) {
                Ok((entry, q_state)) => {
                    app.runner.set_quarantine_state(q_state.clone());
                    {
                        let mut guard = app.allocator_cache.write().unwrap_or_else(|p| p.into_inner());
                        guard.clear();
                    }
                    let out = serde_json::json!({
                        "ok": true,
                        "strategy": clean_strat,
                        "entry": entry,
                        "quarantine_state": q_state,
                    });
                    json_response(200, &out.to_string())
                }
                Err(e) => json_ok_err_response(400, &e),
            }
        }
        ("POST", "/api/analytics/scale-plan") => {
            let parsed = match parse_json_object_body(
                body,
                true,
                "Scale plan request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (dataset, dpath) = match resolve_dataset_checked(
                app,
                query,
                parsed.get("dataset").and_then(|v| v.as_str()),
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let req: scaling::ScalePlanRequest = if body.trim().is_empty() {
                Default::default()
            } else {
                match serde_json::from_str(body) {
                    Ok(r) => r,
                    Err(e) => {
                        return json_err_response(
                            400,
                            &format!("invalid scale-plan parameters: {e}"),
                        );
                    }
                }
            };
            let payload = app.scale_plan_cached(&dpath, &dataset, &req);
            json_response(200, &payload)
        }
        ("POST", "/api/analytics/montecarlo") => {
            let parsed = match parse_json_object_body(
                body,
                true,
                "Monte Carlo request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (dataset, dpath) = match resolve_dataset_checked(
                app,
                query,
                parsed.get("dataset").and_then(|v| v.as_str()),
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (_, _, portfolio_returns, strategy_returns, default_pot) =
                app.analytics_shared(&dpath, &dataset);
            let paths = parsed
                .get("paths")
                .and_then(|v| v.as_u64())
                .map(|p| (p as usize).clamp(10, 5_000))
                .unwrap_or(1_000);
            let seed = parsed
                .get("seed")
                .and_then(|v| v.as_u64())
                .unwrap_or(20_261_122);
            let start_equity = parsed
                .get("start_equity")
                .or_else(|| parsed.get("paper_pot_start"))
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1.0e9)
                .unwrap_or(default_pot);
            let strat_req = parsed
                .get("strategy")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != "portfolio" && *s != "all");
            let (strategy_label, returns_slice): (&str, &[f64]) = match strat_req {
                None => ("portfolio", portfolio_returns.as_slice()),
                Some(name) => match strategy_returns.get(name) {
                    Some(v) => (name, v.as_slice()),
                    None => {
                        return json_err_response(404, &format!("no such strategy: {name}"));
                    }
                },
            };
            let mc =
                analytics::run_monte_carlo_with_equity(returns_slice, start_equity, paths, seed);
            json_response(
                200,
                &json_with_dataset_pair(&mc, &dataset, "strategy", &strategy_label),
            )
        }
        ("POST", "/api/strategy/validate") => {
            let parsed: serde_json::Value = match serde_json::from_str::<serde_json::Value>(body) {
                Ok(v) if v.is_object() => v,
                _ => {
                    return json_response(
                        400,
                        "{\"error\":\"expected JSON body with a string \\\"source\\\" field\"}",
                    );
                }
            };
            let Some(source) = parsed.get("source").and_then(|v| v.as_str()) else {
                return json_response(
                    400,
                    "{\"error\":\"expected JSON body with a string \\\"source\\\" field\"}",
                );
            };
            let res = analytics::validate_rune_source(source);
            json_response(
                200,
                &serde_json::to_string(&res).unwrap_or_else(|_| "{}".to_string()),
            )
        }
        ("POST", "/api/backtest") => {
            let parsed: serde_json::Value = match serde_json::from_str::<serde_json::Value>(body) {
                Ok(v) if v.is_object() => v,
                _ => {
                    return json_response(
                        400,
                        "{\"error\":\"expected JSON body with a string \\\"source\\\" field\"}",
                    );
                }
            };
            let Some(source) = parsed.get("source").and_then(|v| v.as_str()) else {
                return json_response(
                    400,
                    "{\"error\":\"expected JSON body with a string \\\"source\\\" field\"}",
                );
            };
            let (dataset, dpath) = match resolve_dataset_checked(
                app,
                query,
                parsed.get("dataset").and_then(|v| v.as_str()),
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let mut cfg = analytics::BacktestConfig {
                paper_pot_start: app.paper_pot,
                ..analytics::BacktestConfig::default()
            };
            if let Some(pot) = parsed
                .get("paper_pot_start")
                .or_else(|| parsed.get("start_equity"))
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1.0e9)
            {
                cfg.paper_pot_start = pot;
            }
            if let Some(rf) = parsed
                .get("risk_fraction")
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite())
            {
                cfg.risk_fraction = rf.clamp(0.01, 1.0);
            }
            if let Some(paths) = parsed
                .get("monte_carlo_paths")
                .or_else(|| parsed.get("paths"))
                .and_then(|v| v.as_u64())
            {
                cfg.monte_carlo_paths = (paths as usize).min(5_000);
            }
            if let Some(seed) = parsed
                .get("monte_carlo_seed")
                .or_else(|| parsed.get("seed"))
                .and_then(|v| v.as_u64())
            {
                cfg.monte_carlo_seed = seed;
            }
            if let Some(max_t) = parsed.get("max_trades_returned").and_then(|v| v.as_u64()) {
                cfg.max_trades_returned = (max_t as usize).min(5_000);
            }
            let payload = app.sandbox_backtest_cached(&dpath, &dataset, source, cfg);
            json_response(200, &payload)
        }
        ("POST", "/api/strategies/save") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Strategy save request body must be a JSON object with a string \"source\" field",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let Some(source) = parsed.get("source").and_then(|v| v.as_str()) else {
                return json_ok_err_response(400, "missing string field \"source\"");
            };
            let requested = match parsed.get("file").or_else(|| parsed.get("name")) {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v.as_str() {
                    Some(s) => Some(s),
                    None => {
                        return json_ok_err_response(400, "strategy name/file must be a string");
                    }
                },
            };
            let overwrite = match parsed.get("overwrite") {
                None | Some(serde_json::Value::Null) => true,
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => {
                        return json_ok_err_response(400, "overwrite must be a boolean");
                    }
                },
            };
            match strategy_v2::save_strategy_atomic_with_overwrite(
                &app.strategies_dir,
                requested,
                source,
                overwrite,
            ) {
                Ok(saved) => {
                    invalidate_strategies_dir_caches();
                    app.invalidate_live_hot();
                    json_response(
                        200,
                        &serde_json::to_string(&saved).unwrap_or_else(|_| "{}".to_string()),
                    )
                }
                Err(err) => {
                    let payload = serde_json::json!({
                        "ok": false,
                        "error": err.message(),
                        "errors": err.errors(),
                    });
                    json_response(err.status(), &payload.to_string())
                }
            }
        }
        ("DELETE", r) if r.starts_with("/api/strategies/") => {
            let raw_name = r.strip_prefix("/api/strategies/").unwrap_or("");
            match strategy_v2::delete_strategy_confined(&app.strategies_dir, raw_name) {
                Ok(deleted) => {
                    invalidate_strategies_dir_caches();
                    app.invalidate_live_hot();
                    json_response(
                        200,
                        &serde_json::to_string(&deleted).unwrap_or_else(|_| "{}".to_string()),
                    )
                }
                Err(err) => {
                    let payload = serde_json::json!({
                        "ok": false,
                        "error": err.message(),
                        "errors": err.errors(),
                    });
                    json_response(err.status(), &payload.to_string())
                }
            }
        }
        ("POST", "/api/research/synthesize") => {
            let parsed = match parse_json_object_body(
                body,
                true,
                "Synthesizer request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let req: synthesizer::SynthesizeRequest = if body.trim().is_empty() {
                Default::default()
            } else {
                match serde_json::from_str(body) {
                    Ok(r) => r,
                    Err(e) => {
                        return json_ok_err_response(
                            400,
                            &format!("invalid synthesize parameters: {e}"),
                        );
                    }
                }
            };
            let (dataset, dpath) = match resolve_dataset_checked(
                app,
                query,
                parsed
                    .get("dataset")
                    .and_then(|v| v.as_str())
                    .or(req.dataset.as_deref()),
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let view = app.tape(&dpath);
            let (strategies, _) = load_strategies_with_errors(&app.strategies_dir);
            match synthesizer::synthesize_strategies(&view.grouped, &strategies, &dataset, &req) {
                Ok(rep) => json_response(
                    200,
                    &serde_json::to_string(&rep).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(e) => json_ok_err_response(400, &e),
            }
        }
        ("GET", "/api/live/status") => {
            let snap = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.rails.status_snapshot(&st.config, st.connected)
            };
            json_response(200, &serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()))
        }
        ("GET", "/api/live/account") => {
            let cfg = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.config.clone()
            };
            if !cfg.is_configured() {
                return json_response(
                    200,
                    "{\"configured\":false,\"connected\":false,\"balance_cents\":null,\"balance_dollars\":null,\"portfolio_value_cents\":null,\"portfolio_value_dollars\":null,\"daily_pnl_dollars\":null,\"note\":\"Configure Kalshi v2 API credentials in Desk Settings to connect your account.\"}",
                );
            }
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                    st.connected = false;
                    st.rails.last_error = Some(e.clone());
                    return live_err_response(502, None, &e);
                }
            };
            match client.get_balance() {
                Ok(bal) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = true;
                        st.rails.last_error = None;
                    }
                    let payout = bal
                        .payout_cents
                        .map_or_else(|| "null".to_string(), |v| v.to_string());
                    let body = format!(
                        "{{\"configured\":true,\"connected\":true,\"balance_cents\":{},\"balance_dollars\":{},\"portfolio_value_cents\":{},\"portfolio_value_dollars\":{},\"payout_cents\":{},\"note\":\"Connected to Kalshi v2 REST portfolio.\"}}",
                        bal.balance_cents,
                        bal.balance_dollars,
                        bal.portfolio_value_cents,
                        bal.portfolio_value_dollars,
                        payout
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = false;
                        st.rails.last_error = Some(e.clone());
                    }
                    live_err_response(502, None, &e)
                }
            }
        }
        ("GET", "/api/live/positions") => {
            let cfg = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.config.clone()
            };
            if !cfg.is_configured() {
                return json_response(
                    200,
                    "{\"configured\":false,\"connected\":false,\"positions\":[],\"total_exposure_dollars\":0.0,\"note\":\"Configure Kalshi v2 API credentials in Desk Settings to load positions.\"}",
                );
            }
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => return live_err_response(502, Some("positions"), &e),
            };
            match client.get_positions() {
                Ok(positions) => {
                    let total_exposure: f64 = positions
                        .iter()
                        .map(|p| p.market_exposure_dollars.abs())
                        .sum();
                    let realized_loss: f64 = positions
                        .iter()
                        .filter(|p| p.realized_pnl_dollars < 0.0)
                        .map(|p| -p.realized_pnl_dollars)
                        .sum();
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = true;
                        st.rails.last_error = None;
                        st.rails.sync_venue_metrics(total_exposure, realized_loss);
                    }
                    let body = format!(
                        "{{\"configured\":true,\"connected\":true,\"positions\":{},\"total_exposure_dollars\":{total_exposure}}}",
                        serde_json::to_string(&positions).unwrap_or_else(|_| "[]".into()),
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = false;
                        st.rails.last_error = Some(e.clone());
                    }
                    live_err_response(502, Some("positions"), &e)
                }
            }
        }
        ("GET", "/api/live/orders") => {
            let cfg = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.config.clone()
            };
            if !cfg.is_configured() {
                return json_response(
                    200,
                    "{\"configured\":false,\"connected\":false,\"orders\":[],\"note\":\"Configure Kalshi v2 API credentials in Desk Settings to load orders.\"}",
                );
            }
            let status_filter = query_param(query, "status");
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => return live_err_response(502, Some("orders"), &e),
            };
            match client.get_orders(status_filter.as_deref()) {
                Ok(orders) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = true;
                        st.rails.last_error = None;
                    }
                    let body = format!(
                        "{{\"configured\":true,\"connected\":true,\"orders\":{}}}",
                        serde_json::to_string(&orders).unwrap_or_else(|_| "[]".into()),
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = false;
                        st.rails.last_error = Some(e.clone());
                    }
                    live_err_response(502, Some("orders"), &e)
                }
            }
        }
        ("GET", "/api/live/fills") => {
            let cfg = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.config.clone()
            };
            if !cfg.is_configured() {
                return json_response(
                    200,
                    "{\"configured\":false,\"connected\":false,\"fills\":[],\"note\":\"Configure Kalshi v2 API credentials in Desk Settings to load fills.\"}",
                );
            }
            let limit = query_param(query, "limit")
                .and_then(|s| s.parse::<usize>().ok())
                .map(|n| n.clamp(1, 200))
                .unwrap_or(50);
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => return live_err_response(502, Some("fills"), &e),
            };
            match client.get_fills(Some(limit)) {
                Ok(fills) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = true;
                        st.rails.last_error = None;
                    }
                    let body = format!(
                        "{{\"configured\":true,\"connected\":true,\"fills\":{}}}",
                        serde_json::to_string(&fills).unwrap_or_else(|_| "[]".into()),
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = false;
                        st.rails.last_error = Some(e.clone());
                    }
                    live_err_response(502, Some("fills"), &e)
                }
            }
        }
        ("POST", "/api/live/config") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Live config request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (mut next_cfg, mut next_rails) = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                (st.config.clone(), st.rails.rails.clone())
            };
            let mut creds_changed = false;
            if let Some(v) = parsed.get("api_key_id") {
                let Some(s) = v.as_str() else {
                    return json_response(400, "{\"error\":\"api_key_id must be a string\"}");
                };
                next_cfg.api_key_id = s.trim().to_string();
                creds_changed = true;
            }
            if let Some(v) = parsed.get("private_key_path") {
                let Some(s) = v.as_str() else {
                    return json_response(400, "{\"error\":\"private_key_path must be a string\"}");
                };
                next_cfg.private_key_path = s.trim().to_string();
                creds_changed = true;
            }
            let private_key_pem: Option<String> = match parsed.get("private_key_pem") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v.as_str() {
                    Some(s) if !s.trim().is_empty() => {
                        creds_changed = true;
                        Some(s.to_string())
                    }
                    Some(_) => None,
                    None => {
                        return json_response(400, "{\"error\":\"private_key_pem must be a string\"}");
                    }
                },
            };
            if let Some(v) = parsed.get("environment") {
                let Some(s) = v.as_str().and_then(kalshi_live::KalshiEnv::parse_loose) else {
                    return json_response(
                        400,
                        "{\"error\":\"environment must be \\\"demo\\\" or \\\"prod\\\"\"}",
                    );
                };
                if next_cfg.environment != s {
                    next_cfg.environment = s;
                    creds_changed = true;
                }
            }
            if let Some(v) = parsed.get("api_base_url") {
                if v.is_null() {
                    next_cfg.api_base_url = None;
                    creds_changed = true;
                } else if let Some(s) = v.as_str() {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        next_cfg.api_base_url = None;
                    } else {
                        match kalshi_live::validate_api_base_url(trimmed) {
                            Ok(valid) => next_cfg.api_base_url = Some(valid),
                            Err(e) => {
                                return json_err_response(400, &e);
                            }
                        }
                    }
                    creds_changed = true;
                } else {
                    return json_response(400, "{\"error\":\"api_base_url must be a string or null\"}");
                }
            }
            if let Some(rv) = parsed.get("rails") {
                let Some(ro) = rv.as_object() else {
                    return json_response(400, "{\"error\":\"rails must be a JSON object\"}");
                };
                if let Some(v) = ro.get("max_order_contracts") {
                    let Some(n) = v.as_u64().and_then(|u| u32::try_from(u).ok()) else {
                        return json_response(400, "{\"error\":\"max_order_contracts must be a positive integer\"}");
                    };
                    next_rails.max_order_contracts = n;
                }
                if let Some(v) = ro.get("max_order_dollars") {
                    let Some(f) = v.as_f64() else {
                        return json_response(400, "{\"error\":\"max_order_dollars must be a number\"}");
                    };
                    next_rails.max_order_dollars = f;
                }
                if let Some(v) = ro.get("max_daily_loss_dollars") {
                    let Some(f) = v.as_f64() else {
                        return json_response(400, "{\"error\":\"max_daily_loss_dollars must be a number\"}");
                    };
                    next_rails.max_daily_loss_dollars = f;
                }
                if let Some(v) = ro.get("max_open_exposure_dollars") {
                    let Some(f) = v.as_f64() else {
                        return json_response(400, "{\"error\":\"max_open_exposure_dollars must be a number\"}");
                    };
                    next_rails.max_open_exposure_dollars = f;
                }
                if let Some(v) = ro.get("allowed_ticker_prefix") {
                    let Some(s) = v.as_str() else {
                        return json_response(400, "{\"error\":\"allowed_ticker_prefix must be a string\"}");
                    };
                    next_rails.allowed_ticker_prefix = s.to_string();
                }
            }

            let validated_rails = match next_rails.validate() {
                Ok(r) => r,
                Err(errs) => {
                    let body = format!(
                        "{{\"error\":{},\"rail_violations\":{}}}",
                        serde_json::to_string(&errs.join("; ")).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&errs).unwrap_or_else(|_| "[]".into()),
                    );
                    return json_response(400, &body);
                }
            };
            let saved_cfg = match kalshi_live::save_live_config(
                &app.data_dir,
                next_cfg,
                private_key_pem.as_deref(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    return json_err_response(400, &e);
                }
            };
            if let Err(e) = risk_rails::save_rails_config(&app.data_dir, &validated_rails) {
                return json_err_response(500, &e);
            }
            let snap = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.config = saved_cfg;
                let _ = st.rails.update_rails(&validated_rails);
                if creds_changed {
                    st.connected = false;
                    st.rails.last_venue_sync_epoch = None;
                    let _ = st.rails.save_runtime(&app.data_dir);
                    if !st.config.is_configured() {
                        st.rails.disarm();
                    }
                }
                st.rails.last_error = None;
                st.rails.status_snapshot(&st.config, st.connected)
            };
            app.invalidate_live_hot();
            if snap.configured {
                app.notify_reconcile();
            }
            publish_live_status_json(&snap)
        }
        ("POST", "/api/live/arm") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Arm request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let confirm_arm = match parsed.get("confirm_arm") {
                None => false,
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => {
                        return json_response(400, "{\"error\":\"confirm_arm must be a boolean\"}");
                    }
                },
            };
            let (res, configured, snap) = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                let configured = st.config.is_configured();
                let res = st.rails.arm(confirm_arm, configured);
                let snap = st.rails.status_snapshot(&st.config, st.connected);
                (res, configured, snap)
            };
            match res {
                Ok(()) => publish_live_status_json(&snap),
                Err(e) => {
                    let body = format!(
                        "{{\"error\":{},\"status\":{}}}",
                        serde_json::to_string(&e).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
                    );
                    let code = if !configured { 412 } else { 400 };
                    json_response(code, &body)
                }
            }
        }
        ("POST", "/api/live/disarm") => {
            let snap = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.rails.disarm();
                st.rails.status_snapshot(&st.config, st.connected)
            };
            publish_live_status_json(&snap)
        }
        ("POST", "/api/live/order") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Order request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let post_only = match parsed.get("post_only") {
                None | Some(serde_json::Value::Null) => false,
                Some(serde_json::Value::Bool(b)) => *b,
                Some(_) => {
                    return json_response(
                        400,
                        "{\"ok\":false,\"error\":\"post_only must be a boolean\"}",
                    );
                }
            };
            let intent: risk_rails::LiveOrderIntent = match serde_json::from_str(body) {
                Ok(i) => i,
                Err(e) => {
                    return json_ok_err_response(400, &format!("invalid order intent: {e}"));
                }
            };
            let (auth_res, cfg) = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                (st.rails.authorize_order(&intent), st.config.clone())
            };
            let (kalshi_req, check) = match auth_res {
                Ok((base_req, check)) => {
                    if post_only {
                        match kalshi_live::KalshiOrderRequest::maker_limit(
                            &intent.ticker,
                            &intent.action,
                            &intent.side,
                            intent.count,
                            intent.price_cents,
                            Some(base_req.client_order_id),
                        ) {
                            Ok(maker_req) => (maker_req, check),
                            Err(e) => {
                                return json_ok_err_response(400, &e);
                            }
                        }
                    } else {
                        (base_req, check)
                    }
                }
                Err(check) => {
                    let body = format!(
                        "{{\"ok\":false,\"error\":{},\"rail_violations\":{},\"order_notional_dollars\":{},\"projected_exposure_dollars\":{}}}",
                        serde_json::to_string(&check.rail_violations.join("; ")).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&check.rail_violations).unwrap_or_else(|_| "[]".into()),
                        check.order_notional_dollars,
                        check.projected_exposure_dollars,
                    );
                    return json_response(403, &body);
                }
            };
            if intent.dry_run {
                let body = format!(
                    "{{\"ok\":true,\"dry_run\":true,\"order_request\":{},\"risk_check\":{}}}",
                    serde_json::to_string(&kalshi_req).unwrap_or_else(|_| "{}".into()),
                    serde_json::to_string(&check).unwrap_or_else(|_| "{}".into()),
                );
                return json_response(200, &body);
            }
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    return json_ok_err_response(412, &e);
                }
            };
            match client.place_order(&kalshi_req) {
                Ok(order) => {
                    let now_ts = now_unix();
                    let snap = {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = true;
                        st.rails.last_error = None;
                        st.rails
                            .record_submitted_order(&intent.action, check.order_notional_dollars);
                        if intent.action.eq_ignore_ascii_case("buy") {
                            st.reserved_order_exposure
                                .insert(order.order_id.clone(), check.order_notional_dollars);
                        }
                        st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                            kind: "order_submitted".to_string(),
                            venue_id: order.order_id.clone(),
                            ts: now_ts,
                            order_id: order.order_id.clone(),
                            client_order_id: order
                                .client_order_id
                                .clone()
                                .or_else(|| Some(kalshi_req.client_order_id.clone())),
                            trade_id: None,
                            ticker: order.ticker.clone(),
                            side: intent.side.clone(),
                            action: intent.action.clone(),
                            count: intent.count as i64,
                            price_cents: intent.price_cents,
                            is_taker: Some(!post_only),
                            fee_dollars: 0.0,
                            revenue_cents: None,
                            realized_pnl_dollars: None,
                        });
                        st.rails.status_snapshot(&st.config, st.connected)
                    };
                    app.invalidate_live_hot();
                    let ev = format!(
                        "{{\"type\":\"live_order\",\"action\":\"placed\",\"order_id\":{},\"ticker\":{},\"ts\":{}}}",
                        serde_json::to_string(&order.order_id).unwrap_or_else(|_| "\"\"".to_string()),
                        serde_json::to_string(&order.ticker).unwrap_or_else(|_| "\"\"".to_string()),
                        now_ts,
                    );
                    tape_hub().publish(ev);
                    let body = format!(
                        "{{\"ok\":true,\"order\":{},\"risk_check\":{},\"status\":{}}}",
                        serde_json::to_string(&order).unwrap_or_else(|_| "{}".into()),
                        serde_json::to_string(&check).unwrap_or_else(|_| "{}".into()),
                        serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.rails.last_error = Some(e.clone());
                    }
                    json_ok_err_response(502, &e)
                }
            }
        }
        ("POST", r) if r.starts_with("/api/live/orders/") && r.ends_with("/amend") => {
            let order_id = &r["/api/live/orders/".len()..r.len() - "/amend".len()];
            if let Err(e) = kalshi_live::validate_order_id(order_id) {
                return json_ok_err_response(400, &e);
            }
            let parsed = match parse_json_object_body(
                body,
                false,
                "Amend request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let Some(price_u64) = parsed.get("price_cents").and_then(|v| v.as_u64()) else {
                return json_ok_err_response(400, "price_cents must be an integer in 1..=99");
            };
            if !(1..=99).contains(&price_u64) {
                return json_ok_err_response(
                    400,
                    &format!("price_cents must be whole-cent 1..=99, got {price_u64}"),
                );
            }
            let price_cents = price_u64 as u8;
            let count_opt: Option<u32> = match parsed.get("count") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v.as_u64() {
                    Some(c) if c >= 1 && c <= u32::MAX as u64 => Some(c as u32),
                    _ => {
                        return json_ok_err_response(400, "count must be a positive integer >= 1");
                    }
                },
            };

            let (cfg, local_info, local_reserved) = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if st.rails.killed {
                    let msg = format!(
                        "emergency kill switch is engaged ({})",
                        st.rails.kill_reason.as_deref().unwrap_or("operator halt")
                    );
                    return json_ok_err_response(403, &msg);
                }
                if !st.rails.live_armed {
                    return json_ok_err_response(
                        403,
                        "live trading is disarmed (live_armed=false); arm explicitly before amending orders",
                    );
                }
                (
                    st.config.clone(),
                    st.executions.latest_order_info(order_id),
                    st.reserved_order_exposure.get(order_id).copied(),
                )
            };
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => return json_ok_err_response(412, &e),
            };

            let (ticker, side, action, old_price, old_count) = match local_info {
                Some((t, s, a, p, c)) => (t, s, a, p, u32::try_from(c.max(1)).unwrap_or(1)),
                None => {
                    let orders = match client.get_orders(None) {
                        Ok(list) => list,
                        Err(e) => {
                            let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                            st.rails.last_error = Some(e.clone());
                            return json_ok_err_response(502, &e);
                        }
                    };
                    let Some(ord) = orders.into_iter().find(|o| o.order_id == order_id) else {
                        return json_ok_err_response(404, &format!("order {order_id} not found"));
                    };
                    if ord.status.eq_ignore_ascii_case("canceled")
                        || ord.status.eq_ignore_ascii_case("cancelled")
                        || ord.status.eq_ignore_ascii_case("executed")
                    {
                        return json_ok_err_response(
                            409,
                            &format!("order {order_id} is {}, cannot amend", ord.status),
                        );
                    }
                    let old_p = if ord.side.eq_ignore_ascii_case("no") {
                        ord.no_price.or_else(|| ord.yes_price.map(|p| 100 - p))
                    } else {
                        ord.yes_price.or_else(|| ord.no_price.map(|p| 100 - p))
                    }
                    .unwrap_or(50)
                    .clamp(1, 99) as u8;
                    let old_c = if ord.remaining_count > 0 {
                        ord.remaining_count
                    } else {
                        ord.initial_count.max(1)
                    };
                    (
                        ord.ticker,
                        ord.side,
                        if ord.action.is_empty() {
                            "buy".to_string()
                        } else {
                            ord.action
                        },
                        old_p,
                        u32::try_from(old_c.max(1)).unwrap_or(1),
                    )
                }
            };

            let target_count = count_opt.unwrap_or(old_count);
            let old_reserved_eff = local_reserved.unwrap_or_else(|| {
                if action.eq_ignore_ascii_case("buy") {
                    (old_count as f64) * (old_price as f64 / 100.0)
                } else {
                    0.0
                }
            });
            let is_upward = price_cents > old_price || target_count > old_count;

            let check = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if st.rails.killed {
                    let msg = format!(
                        "emergency kill switch is engaged ({})",
                        st.rails.kill_reason.as_deref().unwrap_or("operator halt")
                    );
                    return json_ok_err_response(403, &msg);
                }
                if !st.rails.live_armed {
                    return json_ok_err_response(
                        403,
                        "live trading is disarmed (live_armed=false); arm explicitly before amending orders",
                    );
                }
                let intent = risk_rails::LiveOrderIntent {
                    ticker: ticker.clone(),
                    action: action.clone(),
                    side: side.clone(),
                    count: target_count,
                    price_cents,
                    client_order_id: None,
                    dry_run: false,
                };
                if is_upward {
                    let res = st.rails.check_amend_order(&intent, old_reserved_eff);
                    if !res.allowed {
                        let body = format!(
                            "{{\"ok\":false,\"error\":{},\"rail_violations\":{},\"order_notional_dollars\":{},\"projected_exposure_dollars\":{}}}",
                            serde_json::to_string(&res.rail_violations.join("; ")).unwrap_or_else(|_| "\"\"".into()),
                            serde_json::to_string(&res.rail_violations).unwrap_or_else(|_| "[]".into()),
                            res.order_notional_dollars,
                            res.projected_exposure_dollars,
                        );
                        return json_response(403, &body);
                    }
                    res
                } else {
                    let new_notional = (target_count as f64) * (price_cents as f64 / 100.0);
                    let projected = if action.eq_ignore_ascii_case("buy") {
                        (st.rails.open_exposure_dollars - old_reserved_eff + new_notional).max(0.0)
                    } else {
                        st.rails.open_exposure_dollars
                    };
                    risk_rails::RiskCheckResult {
                        allowed: true,
                        order_notional_dollars: new_notional,
                        projected_exposure_dollars: projected,
                        rail_violations: Vec::new(),
                    }
                }
            };

            match client.amend_order_with_side(order_id, Some(&side), price_cents, count_opt) {
                Ok(amended) => {
                    let now_ts = now_unix();
                    let final_price = if side.eq_ignore_ascii_case("no") {
                        amended.no_price.or_else(|| amended.yes_price.map(|p| 100 - p))
                    } else {
                        amended.yes_price.or_else(|| amended.no_price.map(|p| 100 - p))
                    }
                    .unwrap_or(price_cents as i64)
                    .clamp(1, 99) as u8;
                    let final_count = if amended.remaining_count > 0 {
                        u32::try_from(amended.remaining_count).unwrap_or(target_count)
                    } else if amended.initial_count > 0 {
                        u32::try_from(amended.initial_count).unwrap_or(target_count)
                    } else {
                        target_count
                    };
                    let snap = {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = true;
                        st.rails.last_error = None;
                        if action.eq_ignore_ascii_case("buy") {
                            let new_reserved =
                                (final_count as f64) * (final_price as f64 / 100.0);
                            st.rails
                                .apply_amended_exposure(old_reserved_eff, new_reserved);
                            st.reserved_order_exposure
                                .insert(order_id.to_string(), new_reserved);
                        }
                        let cid = amended
                            .client_order_id
                            .clone()
                            .or_else(|| st.executions.client_order_id_for(order_id));
                        st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                            kind: "order_amended".to_string(),
                            venue_id: format!(
                                "{}:{}:{}:{}",
                                order_id,
                                kalshi_live::now_millis_string(),
                                final_price,
                                final_count
                            ),
                            ts: now_ts,
                            order_id: order_id.to_string(),
                            client_order_id: cid,
                            trade_id: None,
                            ticker: if amended.ticker.is_empty() {
                                ticker.clone()
                            } else {
                                amended.ticker.clone()
                            },
                            side: side.clone(),
                            action: action.clone(),
                            count: final_count as i64,
                            price_cents: final_price,
                            is_taker: None,
                            fee_dollars: 0.0,
                            revenue_cents: None,
                            realized_pnl_dollars: None,
                        });
                        st.rails.status_snapshot(&st.config, st.connected)
                    };
                    app.runner
                        .mark_live_order_amended(order_id, final_price, final_count, now_ts);
                    app.invalidate_live_hot();
                    let ev = format!(
                        "{{\"type\":\"live_order\",\"action\":\"amended\",\"order_id\":{},\"ticker\":{},\"price_cents\":{},\"ts\":{}}}",
                        serde_json::to_string(order_id).unwrap_or_else(|_| "\"\"".to_string()),
                        serde_json::to_string(&ticker).unwrap_or_else(|_| "\"\"".to_string()),
                        final_price,
                        now_ts,
                    );
                    tape_hub().publish(ev);
                    let body = format!(
                        "{{\"ok\":true,\"order\":{},\"risk_check\":{},\"status\":{}}}",
                        serde_json::to_string(&amended).unwrap_or_else(|_| "{}".into()),
                        serde_json::to_string(&check).unwrap_or_else(|_| "{}".into()),
                        serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.rails.last_error = Some(e.clone());
                    }
                    let code = if e.starts_with("Kalshi HTTP 409:") {
                        409
                    } else if e.starts_with("Kalshi HTTP 400:") {
                        400
                    } else {
                        502
                    };
                    json_ok_err_response(code, &e)
                }
            }
        }
        ("POST", r) if r.starts_with("/api/live/positions/") && r.ends_with("/flatten") => {
            let ticker_raw = &r["/api/live/positions/".len()..r.len() - "/flatten".len()];
            let ticker = match kalshi_live::validate_ticker(ticker_raw) {
                Ok(t) => t.to_string(),
                Err(e) => return json_ok_err_response(400, &e),
            };
            let parsed = match parse_json_object_body(
                body,
                true,
                "Flatten request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let req_side: Option<String> = match parsed.get("side") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v.as_str().map(|s| s.trim().to_ascii_lowercase()) {
                    Some(s) if s == "yes" || s == "no" => Some(s),
                    _ => return json_ok_err_response(400, "side must be \"yes\" or \"no\""),
                },
            };
            let req_price_cents: Option<u8> = match parsed.get("price_cents") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v.as_u64() {
                    Some(p) if (1..=99).contains(&p) => Some(p as u8),
                    _ => {
                        return json_ok_err_response(400, "price_cents must be whole-cent 1..=99");
                    }
                },
            };
            let post_only = match parsed.get("post_only") {
                None | Some(serde_json::Value::Null) => false,
                Some(serde_json::Value::Bool(b)) => *b,
                Some(_) => {
                    return json_ok_err_response(400, "post_only must be a boolean");
                }
            };

            let cfg = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if st.rails.killed {
                    let msg = format!(
                        "emergency kill switch is engaged ({})",
                        st.rails.kill_reason.as_deref().unwrap_or("operator halt")
                    );
                    return json_ok_err_response(403, &msg);
                }
                if !st.rails.live_armed {
                    return json_ok_err_response(
                        403,
                        "live trading is disarmed (live_armed=false); arm explicitly before flattening positions",
                    );
                }
                if st.rails.is_venue_sync_stale(now_unix()) {
                    return json_ok_err_response(
                        403,
                        "refusing position flatten: venue sync is stale (>30s) or pending; reconcile first",
                    );
                }
                st.config.clone()
            };
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => return json_ok_err_response(412, &e),
            };
            let positions = match client.get_positions() {
                Ok(p) => p,
                Err(e) => {
                    let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                    st.rails.last_error = Some(e.clone());
                    return json_ok_err_response(502, &e);
                }
            };
            let net_pos = positions
                .iter()
                .find(|p| p.ticker == ticker)
                .map(|p| p.position)
                .unwrap_or(0);
            if net_pos == 0 {
                return json_ok_err_response(
                    404,
                    &format!("no open venue position for {ticker}"),
                );
            }
            let (open_side, open_count) = if net_pos > 0 {
                ("yes".to_string(), u32::try_from(net_pos).unwrap_or(u32::MAX))
            } else {
                (
                    "no".to_string(),
                    u32::try_from(net_pos.saturating_neg()).unwrap_or(u32::MAX),
                )
            };
            if let Some(ref wanted_side) = req_side {
                if wanted_side != &open_side {
                    return json_ok_err_response(
                        400,
                        &format!(
                            "open position for {ticker} is {open_side}, cannot flatten {wanted_side}"
                        ),
                    );
                }
            }

            let limit_cents = match req_price_cents {
                Some(p) => p,
                None => {
                    let ob = app.orderbook.snapshot(&ticker);
                    let ob_bid = if !ob.stale && !ob.needs_resync && ob.source != "empty" {
                        if open_side == "yes" {
                            ob.best_yes_bid
                        } else {
                            ob.no_bids.first().map(|l| l.price_cents)
                        }
                    } else {
                        None
                    };
                    ob_bid
                        .or_else(|| {
                            client.get_market_orderbook(&ticker).ok().and_then(|book| {
                                let levels = if open_side == "yes" {
                                    &book.yes
                                } else {
                                    &book.no
                                };
                                levels
                                    .iter()
                                    .map(|l| l[0])
                                    .max()
                                    .and_then(|c| u8::try_from(c).ok())
                                    .filter(|c| (1..=99).contains(c))
                            })
                        })
                        .or_else(|| {
                            let tape = app.tape(&app.live_snapshots_path);
                            tape.grouped.get(ticker.as_str()).and_then(|snaps| {
                                snaps.last().and_then(|s| {
                                    let p = if open_side == "yes" {
                                        s.yes_bid.get()
                                    } else {
                                        s.no_bid.get()
                                    }?;
                                    let c = (p * 100.0).round() as i64;
                                    if (1..=99).contains(&c) {
                                        Some(c as u8)
                                    } else {
                                        None
                                    }
                                })
                            })
                        })
                        .unwrap_or(if post_only { 50 } else { 1 })
                }
            };

            let intent = risk_rails::LiveOrderIntent {
                ticker: ticker.clone(),
                action: "sell".to_string(),
                side: open_side.clone(),
                count: open_count,
                price_cents: limit_cents,
                client_order_id: None,
                dry_run: false,
            };
            let auth_res = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if st.rails.killed {
                    let msg = format!(
                        "emergency kill switch is engaged ({})",
                        st.rails.kill_reason.as_deref().unwrap_or("operator halt")
                    );
                    return json_ok_err_response(403, &msg);
                }
                if !st.rails.live_armed {
                    return json_ok_err_response(
                        403,
                        "live trading is disarmed (live_armed=false); arm explicitly before flattening positions",
                    );
                }
                if st.rails.is_venue_sync_stale(now_unix()) {
                    return json_ok_err_response(
                        403,
                        "refusing position flatten: venue sync is stale (>30s) or pending; reconcile first",
                    );
                }
                st.rails.authorize_order(&intent)
            };
            let (kalshi_req, check) = match auth_res {
                Ok((base_req, check)) => {
                    if post_only {
                        match kalshi_live::KalshiOrderRequest::maker_limit(
                            &intent.ticker,
                            &intent.action,
                            &intent.side,
                            intent.count,
                            intent.price_cents,
                            Some(base_req.client_order_id),
                        ) {
                            Ok(maker_req) => (maker_req, check),
                            Err(e) => return json_ok_err_response(400, &e),
                        }
                    } else {
                        (base_req, check)
                    }
                }
                Err(check) => {
                    let body = format!(
                        "{{\"ok\":false,\"error\":{},\"rail_violations\":{},\"order_notional_dollars\":{},\"projected_exposure_dollars\":{}}}",
                        serde_json::to_string(&check.rail_violations.join("; ")).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&check.rail_violations).unwrap_or_else(|_| "[]".into()),
                        check.order_notional_dollars,
                        check.projected_exposure_dollars,
                    );
                    return json_response(403, &body);
                }
            };

            match client.place_order(&kalshi_req) {
                Ok(order) => {
                    let now_ts = now_unix();
                    let snap = {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.connected = true;
                        st.rails.last_error = None;
                        st.rails
                            .record_submitted_order("sell", check.order_notional_dollars);
                        st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                            kind: "order_submitted".to_string(),
                            venue_id: order.order_id.clone(),
                            ts: now_ts,
                            order_id: order.order_id.clone(),
                            client_order_id: order
                                .client_order_id
                                .clone()
                                .or_else(|| Some(kalshi_req.client_order_id.clone())),
                            trade_id: None,
                            ticker: ticker.clone(),
                            side: open_side.clone(),
                            action: "sell".to_string(),
                            count: open_count as i64,
                            price_cents: limit_cents,
                            is_taker: Some(!post_only),
                            fee_dollars: 0.0,
                            revenue_cents: None,
                            realized_pnl_dollars: None,
                        });
                        st.rails.status_snapshot(&st.config, st.connected)
                    };
                    app.invalidate_live_hot();
                    let ev = format!(
                        "{{\"type\":\"live_order\",\"action\":\"flattened\",\"order_id\":{},\"ticker\":{},\"side\":{},\"count\":{},\"ts\":{}}}",
                        serde_json::to_string(&order.order_id).unwrap_or_else(|_| "\"\"".to_string()),
                        serde_json::to_string(&ticker).unwrap_or_else(|_| "\"\"".to_string()),
                        serde_json::to_string(&open_side).unwrap_or_else(|_| "\"\"".to_string()),
                        open_count,
                        now_ts,
                    );
                    tape_hub().publish(ev);
                    let body = format!(
                        "{{\"ok\":true,\"order\":{},\"risk_check\":{},\"status\":{},\"flattened_contracts\":{},\"side\":{}}}",
                        serde_json::to_string(&order).unwrap_or_else(|_| "{}".into()),
                        serde_json::to_string(&check).unwrap_or_else(|_| "{}".into()),
                        serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
                        open_count,
                        serde_json::to_string(&open_side).unwrap_or_else(|_| "\"\"".into()),
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.rails.last_error = Some(e.clone());
                    }
                    json_ok_err_response(502, &e)
                }
            }
        }
        ("DELETE", r) if r.starts_with("/api/live/orders/") => {
            let order_id = &r["/api/live/orders/".len()..];
            if let Err(e) = kalshi_live::validate_order_id(order_id) {
                return json_ok_err_response(400, &e);
            }
            let cfg = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.config.clone()
            };
            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    return json_ok_err_response(412, &e);
                }
            };
            match client.cancel_order(order_id) {
                Ok(venue_resp) => {
                    let now_ts = now_unix();
                    let snap = {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        let released = st
                            .reserved_order_exposure
                            .remove(order_id)
                            .or_else(|| st.executions.submitted_notional_for(order_id))
                            .or_else(|| {
                                let o = venue_resp.get("order").unwrap_or(&venue_resp);
                                let rem = o.get("remaining_count").and_then(|v| v.as_i64()).unwrap_or(0);
                                let px = o
                                    .get("yes_price")
                                    .or_else(|| o.get("no_price"))
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(0);
                                if rem > 0 && px > 0 {
                                    Some((rem as f64) * (px as f64 / 100.0))
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0.0);
                        st.rails.release_exposure(released);
                        let sub_rec = st
                            .executions
                            .records()
                            .iter()
                            .find(|rec| rec.kind == "order_submitted" && rec.order_id == order_id)
                            .cloned();
                        st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                            kind: "order_canceled".to_string(),
                            venue_id: order_id.to_string(),
                            ts: now_ts,
                            order_id: order_id.to_string(),
                            client_order_id: sub_rec.as_ref().and_then(|s| s.client_order_id.clone()),
                            trade_id: None,
                            ticker: sub_rec.as_ref().map(|s| s.ticker.clone()).unwrap_or_default(),
                            side: sub_rec.as_ref().map(|s| s.side.clone()).unwrap_or_default(),
                            action: sub_rec.as_ref().map(|s| s.action.clone()).unwrap_or_default(),
                            count: sub_rec.as_ref().map(|s| s.count).unwrap_or(0),
                            price_cents: sub_rec.as_ref().map(|s| s.price_cents).unwrap_or(0),
                            is_taker: None,
                            fee_dollars: 0.0,
                            revenue_cents: None,
                            realized_pnl_dollars: None,
                        });
                        st.rails.status_snapshot(&st.config, st.connected)
                    };
                    app.runner
                        .mark_live_order_canceled(order_id, now_ts, "operator_canceled");
                    app.invalidate_live_hot();
                    let ev = format!(
                        "{{\"type\":\"live_order\",\"action\":\"cancelled\",\"order_id\":{},\"ts\":{}}}",
                        serde_json::to_string(order_id).unwrap_or_else(|_| "\"\"".to_string()),
                        now_ts,
                    );
                    tape_hub().publish(ev);
                    let body = format!(
                        "{{\"ok\":true,\"order_id\":{},\"cancelled\":true,\"venue_response\":{},\"status\":{}}}",
                        serde_json::to_string(order_id).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&venue_resp).unwrap_or_else(|_| "{}".into()),
                        serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
                    );
                    json_response(200, &body)
                }
                Err(e) => {
                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.rails.last_error = Some(e.clone());
                    }
                    json_ok_err_response(502, &e)
                }
            }
        }
        ("GET", "/api/live/reconcile") => {
            let snap = build_reconcile_snapshot(app);
            json_response(200, &serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()))
        }
        ("POST", "/api/live/reconcile") => {
            if let Err(resp) = parse_json_object_body(
                body,
                true,
                "Reconcile request body must be a JSON object",
            ) {
                return resp;
            }
            match reconcile_live_venue(app) {
                Ok(snap) => json_response(
                    200,
                    &serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
                ),
                Err((code, msg)) => json_ok_err_response(code, &msg),
            }
        }
        ("POST", "/api/live/reset-kill") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Reset-kill request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let confirm = parsed
                .get("confirm_reset_kill")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !confirm {
                return json_ok_err_response(
                    400,
                    "Explicit confirmation (confirm_reset_kill: true) is required to reset kill latch.",
                );
            }
            let snap = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if let Err(e) = st.rails.reset_kill(true) {
                    return json_ok_err_response(400, &e);
                }
                st.rails.last_error = None;
                let _ = st.rails.save_runtime(&app.data_dir);
                st.rails.status_snapshot(&st.config, st.connected)
            };
            app.invalidate_live_hot();
            let ev = format!(
                "{{\"type\":\"live_status\",\"configured\":{},\"live_armed\":{},\"killed\":false,\"environment\":\"{}\",\"ts\":{}}}",
                snap.configured,
                snap.live_armed,
                if snap.environment == "prod" { "prod" } else { "demo" },
                now_unix(),
            );
            tape_hub().publish(ev);
            let mut val = serde_json::to_value(&snap).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = val.as_object_mut() {
                obj.insert("ok".to_string(), serde_json::Value::Bool(true));
                obj.insert(
                    "status".to_string(),
                    serde_json::to_value(&snap).unwrap_or_default(),
                );
            }
            json_response(200, &val.to_string())
        }
        ("POST", "/api/live/kill-switch") => {
            let reason: Option<String> = if body.trim().is_empty() {
                None
            } else {
                serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
            };
            // Fail-closed FIRST under lock: disarm, latch killed, and persist runtime BEFORE network calls!
            let (why, client_opt) = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                let why = reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("operator emergency kill switch")
                    .to_string();
                st.rails.live_armed = false;
                st.rails.killed = true;
                st.rails.kill_reason = Some(why.clone());
                let _ = st.rails.save_runtime(&app.data_dir);
                let client_opt = if st.config.is_configured() {
                    kalshi_live::KalshiLiveClient::from_config(&st.config).ok()
                } else {
                    None
                };
                (why, client_opt)
            };
            // Atomically halt automated runner BEFORE calling venue cancel_all_orders().
            app.runner.emergency_halt(&why);
            let report = match client_opt {
                Some(c) => c.cancel_all_orders(),
                None => kalshi_live::CancelAllReport {
                    attempted: 0,
                    cancelled_count: 0,
                    cancelled_ids: Vec::new(),
                    errors: Vec::new(),
                },
            };
            let now_ts = now_unix();
            let snap = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                for cid in &report.cancelled_ids {
                    if let Some(released) = st
                        .reserved_order_exposure
                        .remove(cid)
                        .or_else(|| st.executions.submitted_notional_for(cid))
                    {
                        st.rails.release_exposure(released);
                    }
                    let sub_rec = st
                        .executions
                        .records()
                        .iter()
                        .find(|rec| rec.kind == "order_submitted" && rec.order_id == *cid)
                        .cloned();
                    st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                        kind: "order_canceled".to_string(),
                        venue_id: cid.clone(),
                        ts: now_ts,
                        order_id: cid.clone(),
                        client_order_id: sub_rec.as_ref().and_then(|s| s.client_order_id.clone()),
                        trade_id: None,
                        ticker: sub_rec.as_ref().map(|s| s.ticker.clone()).unwrap_or_default(),
                        side: sub_rec.as_ref().map(|s| s.side.clone()).unwrap_or_default(),
                        action: sub_rec.as_ref().map(|s| s.action.clone()).unwrap_or_default(),
                        count: sub_rec.as_ref().map(|s| s.count).unwrap_or(0),
                        price_cents: sub_rec.as_ref().map(|s| s.price_cents).unwrap_or(0),
                        is_taker: None,
                        fee_dollars: 0.0,
                        revenue_cents: None,
                        realized_pnl_dollars: None,
                    });
                }
                if !report.errors.is_empty() {
                    st.rails.last_error = Some(report.errors.join("; "));
                }
                let _ = st.rails.save_runtime(&app.data_dir);
                st.rails.status_snapshot(&st.config, st.connected)
            };
            app.invalidate_live_hot();
            let ev = format!(
                "{{\"type\":\"kill_switch\",\"disarmed\":true,\"killed\":true,\"cancelled_count\":{},\"ts\":{}}}",
                report.cancelled_count,
                now_unix()
            );
            tape_hub().publish(ev);
            let body = format!(
                "{{\"ok\":true,\"disarmed\":true,\"killed\":true,\"kill_reason\":{},\"attempted\":{},\"cancelled_count\":{},\"cancelled_ids\":{},\"errors\":{},\"status\":{}}}",
                serde_json::to_string(&why).unwrap_or_else(|_| "\"\"".into()),
                report.attempted,
                report.cancelled_count,
                serde_json::to_string(&report.cancelled_ids).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&report.errors).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()),
            );
            json_response(200, &body)
        }
        ("GET", "/api/runner/status") => {
            let (live_armed, killed) = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                (st.rails.live_armed, st.rails.killed)
            };
            let snap = app.runner.status(live_armed, killed);
            json_response(200, &serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()))
        }
        ("POST", "/api/runner/config") => {
            if let Err(resp) = parse_json_object_body(
                body,
                false,
                "Runner config request body must be a JSON object",
            ) {
                return resp;
            }
            let req: shadow_runner::RunnerConfigRequest = match serde_json::from_str(body) {
                Ok(r) => r,
                Err(e) => {
                    return json_ok_err_response(400, &format!("invalid runner config: {e}"));
                }
            };
            let (live_armed, killed) = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                (st.rails.live_armed, st.rails.killed)
            };
            match app.runner.configure(req, live_armed, killed) {
                Ok(status) => {
                    let body = serde_json::json!({
                        "ok": true,
                        "status": status,
                    });
                    json_response(200, &body.to_string())
                }
                Err(e) => json_ok_err_response(400, &e),
            }
        }
        ("POST", "/api/runner/start") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Runner start request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let Some(mode) = parsed.get("mode").and_then(|v| v.as_str()) else {
                return json_ok_err_response(400, "missing string field \"mode\"");
            };
            let confirm_auto_live = match parsed.get("confirm_auto_live") {
                None | Some(serde_json::Value::Null) => false,
                Some(serde_json::Value::Bool(b)) => *b,
                Some(_) => {
                    return json_ok_err_response(400, "confirm_auto_live must be a boolean");
                }
            };
            let (live_armed, killed) = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                (st.rails.live_armed, st.rails.killed)
            };
            match app.runner.start(mode, confirm_auto_live, live_armed, killed) {
                Ok(status) => {
                    let body = serde_json::json!({
                        "ok": true,
                        "status": status,
                    });
                    json_response(200, &body.to_string())
                }
                Err((code, msg)) => json_ok_err_response(code, &msg),
            }
        }
        ("POST", "/api/runner/stop") => {
            let parsed = match parse_json_object_body(
                body,
                true,
                "Runner stop request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let cancel_resting = match parsed.get("cancel_resting") {
                None | Some(serde_json::Value::Null) => true,
                Some(serde_json::Value::Bool(b)) => *b,
                Some(_) => {
                    return json_ok_err_response(400, "cancel_resting must be a boolean");
                }
            };
            let (live_armed, killed, live_cfg) = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                (st.rails.live_armed, st.rails.killed, st.config.clone())
            };
            match app.runner.stop(cancel_resting, Some(&live_cfg), live_armed, killed) {
                Ok((status, cancelled_quotes, live_cancel_errors)) => {
                    let body = serde_json::json!({
                        "ok": true,
                        "cancelled_quotes": cancelled_quotes,
                        "live_cancel_errors": live_cancel_errors,
                        "status": status,
                    });
                    json_response(200, &body.to_string())
                }
                Err(e) => json_ok_err_response(500, &e),
            }
        }
        ("GET", "/api/runner/ledger") => {
            let limit = query_param(query, "limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(200);
            let strategy = query_param(query, "strategy");
            let mode = query_param(query, "mode");
            let resp = app
                .runner
                .ledger_query(limit, strategy.as_deref(), mode.as_deref());
            json_response(200, &serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into()))
        }
        ("POST", "/api/runner/sync-allocator") => {
            let parsed = match parse_json_object_body(
                body,
                true,
                "Runner sync-allocator request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let (_, dpath) = match resolve_dataset_checked(
                app,
                query,
                parsed.get("dataset").and_then(|v| v.as_str()),
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let alloc_req: auto_allocator::AutoAllocatorRequest = if body.trim().is_empty() {
                Default::default()
            } else {
                match serde_json::from_str(body) {
                    Ok(r) => r,
                    Err(e) => {
                        return json_ok_err_response(
                            400,
                            &format!("invalid allocator parameters: {e}"),
                        );
                    }
                }
            };
            let bankroll_override = match parsed.get("bankroll_dollars") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v.as_f64() {
                    Some(b) if b.is_finite() && b > 0.0 && b <= 10_000_000.0 => Some(b),
                    _ => {
                        return json_ok_err_response(
                            400,
                            "bankroll_dollars must be finite between $0.01 and $10,000,000",
                        );
                    }
                },
            };
            let exec_override = match parsed.get("execution_mode") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(s)) => {
                    let m = s.trim().to_ascii_lowercase();
                    if m != "maker" && m != "taker" {
                        return json_ok_err_response(
                            400,
                            "execution_mode must be \"maker\" or \"taker\"",
                        );
                    }
                    Some(m)
                }
                Some(_) => {
                    return json_ok_err_response(400, "execution_mode must be a string");
                }
            };
            let (live_armed, killed, rails) = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                (st.rails.live_armed, st.rails.killed, st.rails.rails.clone())
            };
            let ctx = app.tournament_ctx_shared(&dpath);
            let health = app.execution_health_for_tape(&dpath, true);
            let san = alloc_req.sanitized();
            let rep = auto_allocator::compute_auto_allocator_report_with_audit(
                &ctx,
                &san,
                &rails,
                live_armed,
                killed,
                Some(&app.orderbook),
                Some(&health),
            );
            match app.runner.sync_from_allocator(
                &rep,
                &rails,
                &health.quarantine_state,
                bankroll_override,
                exec_override.as_deref(),
                live_armed,
                killed,
            ) {
                Ok((slots, excluded_quarantined, status)) => {
                    let out = serde_json::json!({
                        "ok": true,
                        "slots": slots,
                        "excluded_quarantined": excluded_quarantined,
                        "status": status,
                    });
                    json_response(200, &out.to_string())
                }
                Err(e) => json_ok_err_response(400, &e),
            }
        }
        ("GET", "/api/orderbook") => {
            let Some(raw_ticker) = query_param(query, "ticker") else {
                return json_ok_err_response(400, "missing ticker parameter");
            };
            let clean = match kalshi_live::validate_ticker(&raw_ticker) {
                Ok(t) => t.to_string(),
                Err(e) => return json_ok_err_response(400, &e),
            };
            let snap = app.orderbook.snapshot(&clean);
            json_response(200, &serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into()))
        }
        ("GET", "/api/orderbook/history") => {
            let Some(raw_ticker) = query_param(query, "ticker") else {
                return json_ok_err_response(400, "missing ticker parameter");
            };
            let clean = match kalshi_live::validate_ticker(&raw_ticker) {
                Ok(t) => t.to_string(),
                Err(e) => return json_ok_err_response(400, &e),
            };
            let limit: usize = query_param(query, "limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(100)
                .clamp(1, 500);
            match app.orderbook.read_history(&clean, limit) {
                Ok(records) => {
                    let payload = serde_json::json!({
                        "ok": true,
                        "ticker": clean,
                        "records": records,
                    });
                    json_response(200, &payload.to_string())
                }
                Err(e) => json_ok_err_response(400, &e),
            }
        }
        ("GET", "/api/orderbook/stream-status") => {
            let status = app.orderbook.stream_status();
            json_response(200, &serde_json::to_string(&status).unwrap_or_else(|_| "{}".into()))
        }
        ("GET", "/api/spot/status") => {
            let status = app.spot_feed.status_snapshot();
            json_response(200, &serde_json::to_string(&status).unwrap_or_else(|_| "{}".into()))
        }
        ("GET", "/api/spot/history") => {
            let asset_raw = query_param(query, "asset").unwrap_or_else(|| "BTC".to_string());
            let clean_asset = match spot_feed::validate_spot_asset_param(&asset_raw) {
                Ok(a) => a,
                Err(e) => return json_ok_err_response(400, &e),
            };
            let limit: usize = query_param(query, "limit")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(200)
                .clamp(1, 500) as usize;
            match app.spot_feed.read_history(clean_asset, limit) {
                Ok(ticks) => {
                    let payload = spot_feed::SpotHistoryResponse {
                        ok: true,
                        asset: clean_asset.to_string(),
                        ticks,
                    };
                    json_response(200, &serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()))
                }
                Err((status, msg)) => json_ok_err_response(status, &msg),
            }
        }
        ("POST", "/api/orderbook/subscribe") => {
            let parsed = match parse_json_object_body(
                body,
                false,
                "Orderbook subscribe request body must be a JSON object",
            ) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let res = if let Some(arr_v) = parsed.get("tickers") {
                let Some(arr) = arr_v.as_array() else {
                    return json_ok_err_response(400, "\"tickers\" must be an array of strings");
                };
                let mut list = Vec::with_capacity(arr.len());
                for item in arr {
                    let Some(s) = item.as_str() else {
                        return json_ok_err_response(400, "each entry in \"tickers\" must be a string");
                    };
                    list.push(s.to_string());
                }
                app.orderbook.subscribe_tickers(&list, true)
            } else if let Some(t_v) = parsed.get("ticker") {
                let Some(t) = t_v.as_str() else {
                    return json_ok_err_response(400, "\"ticker\" must be a string");
                };
                app.orderbook.subscribe_tickers(&[t.to_string()], false)
            } else {
                return json_ok_err_response(400, "missing \"ticker\" or \"tickers\" field");
            };
            match res {
                Ok(subscribed_tickers) => {
                    let payload = serde_json::json!({
                        "ok": true,
                        "subscribed_tickers": subscribed_tickers,
                    });
                    json_response(200, &payload.to_string())
                }
                Err(e) => json_ok_err_response(400, &e),
            }
        }
        _ => json_response(404, "{\"error\":\"not found\"}"),
    }
}

/// The default desk port, and how many ports above it a busy default may use.
const DEFAULT_PORT: u16 = 8472;
const DEFAULT_PORT_SPAN: u16 = 20;

/// The strategy library built into the binary, so a first run has all sixteen paper shapes without a checkout.
const BUILT_IN_STRATEGIES: [(&str, &str); 16] = [
    ("bid_ask_imbalance.rn", include_str!("../strategies/bid_ask_imbalance.rn")),
    ("favourite_floor.rn", include_str!("../strategies/favourite_floor.rn")),
    ("favourite_late.rn", include_str!("../strategies/favourite_late.rn")),
    ("late_convergence_cap.rn", include_str!("../strategies/late_convergence_cap.rn")),
    ("longshot_late.rn", include_str!("../strategies/longshot_late.rn")),
    ("maker_late_theta_no.rn", include_str!("../strategies/maker_late_theta_no.rn")),
    ("maker_spread_capture.rn", include_str!("../strategies/maker_spread_capture.rn")),
    ("mean_reversion_fade.rn", include_str!("../strategies/mean_reversion_fade.rn")),
    ("mid_window_momentum.rn", include_str!("../strategies/mid_window_momentum.rn")),
    ("no_favourite.rn", include_str!("../strategies/no_favourite.rn")),
    ("opening_range_break.rn", include_str!("../strategies/opening_range_break.rn")),
    ("spot_lead_momentum_yes.rn", include_str!("../strategies/spot_lead_momentum_yes.rn")),
    ("spread_compression.rn", include_str!("../strategies/spread_compression.rn")),
    ("tail_overreaction_fade.rn", include_str!("../strategies/tail_overreaction_fade.rn")),
    ("time_decay_harvest.rn", include_str!("../strategies/time_decay_harvest.rn")),
    ("volatility_squeeze.rn", include_str!("../strategies/volatility_squeeze.rn")),
];

/// The platform's standard data folder for OpenBell, or None when the home folder is unknown.
fn platform_data_dir(os: &str, get: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    const APP: &str = "openbell";
    match os {
        "macos" => get("HOME").map(|h| PathBuf::from(h).join("Library/Application Support").join(APP)),
        "windows" => get("APPDATA").filter(|a| !a.is_empty()).map(|a| PathBuf::from(a).join(APP)),
        _ => get("XDG_DATA_HOME")
            .filter(|x| x.starts_with('/'))
            .or_else(|| get("HOME").map(|h| format!("{h}/.local/share")))
            .map(|base| PathBuf::from(base).join(APP)),
    }
}

/// Where the tape goes when OPENBELL_DATA is unset: the folder this desk already
/// uses in the current directory when there is one, else the platform's folder.
fn default_data_dir(os: &str, get: &dyn Fn(&str) -> Option<String>, existing_here: bool) -> PathBuf {
    if existing_here {
        return PathBuf::from("./openbell-data");
    }
    platform_data_dir(os, get).unwrap_or_else(|| PathBuf::from("./openbell-data"))
}

/// Where the strategy library goes when OPENBELL_STRATEGIES is unset.
fn default_strategies_dir(data_dir: &Path, existing_here: bool) -> PathBuf {
    if existing_here {
        PathBuf::from("./strategies")
    } else {
        data_dir.join("strategies")
    }
}

/// Writes the built-in library into `dir`, only when `dir` does not exist yet.
fn seed_strategy_library(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    for (name, source) in BUILT_IN_STRATEGIES {
        std::fs::write(dir.join(name), source)?;
    }
    Ok(())
}

/// The one friendly start-up message: where the desk is, where its tape lives, and what updates do.
fn start_banner(url: &str, data_dir: &Path, updates: update::Mode, opened: bool) -> String {
    let updates = match updates {
        update::Mode::Off => "off",
        update::Mode::Notify => "notify (tells you when a release is ready)",
        update::Mode::Auto => "auto (installs a release and restarts when idle)",
    };
    let browser = if opened { "opening it in your browser" } else { "open that address in a browser" };
    format!(
        "\n  OpenBell {}\n  Your desk:  {url}  ({browser})\n  Tape:       {}\n  Updates:    {updates}\n  Live mode:  Kalshi v2 ready (disarmed by default under hard risk rails).\n",
        env!("CARGO_PKG_VERSION"),
        data_dir.display()
    )
}

/// The first port from `start` (at most DEFAULT_PORT_SPAN above it) that `bind` accepts.
fn bind_free_port<F>(start: u16, bind: F) -> Option<(TcpListener, u16)>
where
    F: Fn(u16) -> std::io::Result<TcpListener>,
{
    (start..=start.saturating_add(DEFAULT_PORT_SPAN)).find_map(|p| bind(p).ok().map(|l| (l, p)))
}

fn parse_auto_collect_config(
    args: &[String],
    get_env: &dyn Fn(&str) -> Option<String>,
    collect_secs: u64,
) -> (bool, u64) {
    let mut enabled = get_env("OPENBELL_AUTO_COLLECT")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    let mut interval_opt: Option<u64> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--auto-collect" => enabled = true,
            "--no-auto-collect" => enabled = false,
            "--collect-interval" => {
                if let Some(next) = args.get(i + 1) {
                    if let Ok(v) = next.parse::<u64>() {
                        interval_opt = Some(v);
                    }
                    i += 1;
                }
            }
            other => {
                if let Some(rest) = other.strip_prefix("--collect-interval=") {
                    if let Ok(v) = rest.parse::<u64>() {
                        interval_opt = Some(v);
                    }
                }
            }
        }
        i += 1;
    }
    let default_iv = if (AUTO_COLLECT_MIN_SECS..=AUTO_COLLECT_MAX_SECS).contains(&collect_secs) {
        collect_secs
    } else {
        60
    };
    let interval = interval_opt
        .unwrap_or(default_iv)
        .clamp(AUTO_COLLECT_MIN_SECS, AUTO_COLLECT_MAX_SECS);
    (enabled, interval)
}

/// Safe toggleable background public tape collector thread. Parks idle on the
/// Condvar when disabled (`0` CPU / `0` network), wakes immediately when
/// `POST /api/collector/schedule` changes enabled/interval, applies adaptive
/// 429 backoff, and broadcasts live SSE `tape` frames via `collect_once_detailed`.
fn start_background_collector(app: Arc<App>) {
    std::thread::spawn(move || loop {
        {
            let mut st = app
                .collector_scheduler
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            while !st.enabled {
                st = app
                    .collector_scheduler
                    .cv
                    .wait(st)
                    .unwrap_or_else(|p| p.into_inner());
            }
            let now = now_unix();
            let due = st.next_run_epoch.unwrap_or(now);
            if due > now {
                let wait_secs = (due - now).min(AUTO_COLLECT_MAX_SECS as i64) as u64;
                let (next_st, _) = app
                    .collector_scheduler
                    .cv
                    .wait_timeout(st, std::time::Duration::from_secs(wait_secs))
                    .unwrap_or_else(|p| p.into_inner());
                drop(next_st);
                continue;
            }
            st.in_flight = true;
        }
        let res = collect_once_detailed(
            &app.data_dir,
            Some(&app.orderbook),
            Some(&app.spot_feed),
        );
        let updated = app.collector_scheduler.record_pass_result(&res);
        match res {
            Ok(rep) => {
                if rep.written > 0 {
                    app.invalidate_live_hot();
                    app.runner.notify_tick();
                }
                if rep.errors.is_empty() {
                    if rep.deduplicated > 0 {
                        println!(
                            "collector: {} snapshots written ({} settled deduplicated)",
                            rep.written, rep.deduplicated
                        );
                    } else {
                        println!("collector: {} snapshots written", rep.written);
                    }
                } else {
                    println!(
                        "collector: {} snapshots written, {} series failed (effective interval {}s)",
                        rep.written,
                        rep.errors.len(),
                        updated.effective_interval_secs()
                    );
                }
            }
            Err((e, rl)) => eprintln!(
                "collector: {e}{} (next interval {}s)",
                if rl { " [429 backoff]" } else { "" },
                updated.effective_interval_secs()
            ),
        }
    });
}

fn build_reconcile_snapshot(app: &App) -> risk_rails::LiveReconcileResponse {
    let now = now_unix();
    let today = risk_rails::current_utc_day();
    let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
    let configured = st.config.is_configured();
    let sync_stale = configured && st.rails.is_venue_sync_stale(now);
    let bd = st.executions.utc_day_breakdown(today);
    let persisted_same_day_loss = if st.rails.utc_day_num == today {
        st.rails.daily_realized_loss_dollars
    } else {
        0.0
    };
    let utc_day_loss = bd.realized_loss_dollars.max(persisted_same_day_loss);
    risk_rails::LiveReconcileResponse {
        ok: true,
        configured,
        connected: st.connected,
        last_sync_epoch: st.rails.last_venue_sync_epoch,
        sync_stale,
        utc_day_loss_dollars: utc_day_loss,
        utc_day_realized_pnl_dollars: bd.realized_pnl_dollars,
        utc_day_fees_dollars: bd.fees_dollars,
        utc_day_breakdown: bd,
        open_exposure_dollars: st.rails.open_exposure_dollars,
        executions: st.executions.records().to_vec(),
        discrepancies: st.last_discrepancies.clone(),
        last_error: st.rails.last_error.clone(),
    }
}

pub(crate) fn reconcile_live_venue(
    app: &App,
) -> Result<risk_rails::LiveReconcileResponse, (u16, String)> {
    let _sync_guard = app
        .reconcile_sync_lock
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (cfg, min_ts) = {
        let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        let min_ts = st
            .executions
            .last_fill_ts()
            .map(|t| t.saturating_sub(300));
        (st.config.clone(), min_ts)
    };
    if !cfg.is_configured() {
        return Err((
            412,
            "Kalshi API key ID and private key PEM are not configured.".to_string(),
        ));
    }
    let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
            st.connected = false;
            st.rails.last_error = Some(e.clone());
            return Err((502, e));
        }
    };

    struct VenueFetch {
        positions: Vec<kalshi_live::KalshiPosition>,
        orders: Vec<kalshi_live::KalshiOrder>,
        fills: Vec<kalshi_live::KalshiFill>,
        settlements: Vec<kalshi_live::KalshiSettlement>,
    }

    let fetch_res: Result<VenueFetch, String> = (|| {
        let positions = client.get_positions()?;
        let orders = client.get_orders(None)?;
        let fills = client.get_fills_since(min_ts, None)?;
        let settlements = client.get_settlements(None, Some(100))?;
        Ok(VenueFetch {
            positions,
            orders,
            fills,
            settlements,
        })
    })();

    let VenueFetch {
        positions,
        orders,
        fills,
        settlements,
    } = match fetch_res {
        Ok(data) => data,
        Err(e) => {
            let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
            st.connected = false;
            st.rails.last_error = Some(e.clone());
            st.last_discrepancies = vec![risk_rails::ReconcileDiscrepancy {
                kind: "sync_error".to_string(),
                message: e.clone(),
                ticker: None,
                order_id: None,
            }];
            return Err((502, e));
        }
    };

    let now = now_unix();
    let open_live_quotes = app
        .runner
        .reconcile_live_data(&orders, &fills, &settlements, now);

    let resp = {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        let mut order_cid_map: std::collections::HashMap<&str, Option<String>> =
            std::collections::HashMap::new();
        for o in &orders {
            order_cid_map.insert(o.order_id.as_str(), o.client_order_id.clone());
        }

        // 1. Append fills idempotently by trade_id.
        for f in &fills {
            let ts = f
                .ts
                .or_else(|| f.created_time.as_deref().and_then(parse_iso_z))
                .unwrap_or(now);
            let price_c = if f.side.eq_ignore_ascii_case("no") {
                f.no_price.or_else(|| f.yes_price.map(|p| 100 - p))
            } else {
                f.yes_price.or_else(|| f.no_price.map(|p| 100 - p))
            }
            .unwrap_or(50)
            .clamp(1, 99) as u8;
            let fee_dollars = f
                .fee_cost_dollars
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or_else(|| {
                    let p = price_c as f64 / 100.0;
                    let rate = if f.is_taker == Some(false) {
                        0.0175
                    } else {
                        0.07
                    };
                    rate * p * (1.0 - p) * (f.count.max(0) as f64)
                });
            let venue_id = if !f.trade_id.trim().is_empty() {
                f.trade_id.clone()
            } else {
                format!("{}:{}:{}", f.order_id, ts, f.count)
            };
            let cid = order_cid_map
                .get(f.order_id.as_str())
                .cloned()
                .flatten()
                .or_else(|| st.executions.client_order_id_for(&f.order_id));
            st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                kind: "fill".to_string(),
                venue_id,
                ts,
                order_id: f.order_id.clone(),
                client_order_id: cid,
                trade_id: if f.trade_id.trim().is_empty() {
                    None
                } else {
                    Some(f.trade_id.clone())
                },
                ticker: f.ticker.clone(),
                side: f.side.clone(),
                action: f.action.clone(),
                count: f.count,
                price_cents: price_c,
                is_taker: f.is_taker,
                fee_dollars,
                revenue_cents: None,
                realized_pnl_dollars: None,
            });
        }

        // 2. Append canceled orders & prune reserved exposure for filled/canceled orders.
        for o in &orders {
            if o.status.eq_ignore_ascii_case("canceled")
                || o.status.eq_ignore_ascii_case("cancelled")
            {
                st.reserved_order_exposure.remove(&o.order_id);
                let price_c = if o.side.eq_ignore_ascii_case("no") {
                    o.no_price.or_else(|| o.yes_price.map(|p| 100 - p))
                } else {
                    o.yes_price.or_else(|| o.no_price.map(|p| 100 - p))
                }
                .unwrap_or(50)
                .clamp(1, 99) as u8;
                let ts = o
                    .created_time
                    .as_deref()
                    .and_then(parse_iso_z)
                    .unwrap_or(now);
                st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                    kind: "order_canceled".to_string(),
                    venue_id: o.order_id.clone(),
                    ts,
                    order_id: o.order_id.clone(),
                    client_order_id: o.client_order_id.clone(),
                    trade_id: None,
                    ticker: o.ticker.clone(),
                    side: o.side.clone(),
                    action: o.action.clone(),
                    count: o.remaining_count.max(0),
                    price_cents: price_c,
                    is_taker: None,
                    fee_dollars: 0.0,
                    revenue_cents: None,
                    realized_pnl_dollars: None,
                });
            } else if o.status.eq_ignore_ascii_case("executed") || o.remaining_count <= 0 {
                st.reserved_order_exposure.remove(&o.order_id);
            }
        }

        // 3. Append settlements idempotently by ticker.
        for s in &settlements {
            let ts = s
                .settled_time
                .as_deref()
                .and_then(parse_iso_z)
                .unwrap_or(now);
            let side = if s.yes_count >= s.no_count {
                "yes".to_string()
            } else {
                "no".to_string()
            };
            let count = s.yes_count.saturating_add(s.no_count).max(0);
            let pnl = s.realized_pnl_dollars();
            st.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                kind: "settlement".to_string(),
                venue_id: s.ticker.clone(),
                ts,
                order_id: String::new(),
                client_order_id: None,
                trade_id: None,
                ticker: s.ticker.clone(),
                side,
                action: "settle".to_string(),
                count,
                price_cents: 0,
                is_taker: None,
                fee_dollars: s.fee_cost_dollars,
                revenue_cents: Some(s.revenue),
                realized_pnl_dollars: Some(pnl),
            });
        }

        // 4. Authoritative open exposure = filled position exposure + resting buy orders notional.
        let pos_exposure: f64 = positions
            .iter()
            .map(|p| p.market_exposure_dollars.abs())
            .filter(|v| v.is_finite())
            .sum();
        let resting_buy_exposure: f64 = orders
            .iter()
            .filter(|o| {
                o.status.eq_ignore_ascii_case("resting")
                    && o.action.eq_ignore_ascii_case("buy")
                    && o.remaining_count > 0
            })
            .map(|o| {
                let px = if o.side.eq_ignore_ascii_case("no") {
                    o.no_price.unwrap_or(0)
                } else {
                    o.yes_price.unwrap_or(0)
                };
                (o.remaining_count as f64) * (px.clamp(0, 99) as f64 / 100.0)
            })
            .sum();
        let open_exposure = pos_exposure + resting_buy_exposure;

        let today = risk_rails::current_utc_day();
        let bd = st.executions.utc_day_breakdown(today);
        let pos_loss: f64 = positions
            .iter()
            .filter(|p| p.realized_pnl_dollars.is_finite() && p.realized_pnl_dollars < 0.0)
            .map(|p| -p.realized_pnl_dollars)
            .sum();
        let prev_same_day_loss = if st.rails.utc_day_num == today {
            st.rails.daily_realized_loss_dollars
        } else {
            0.0
        };
        let effective_loss = bd.realized_loss_dollars.max(pos_loss).max(prev_same_day_loss);

        // 5. Cross-check local vs venue discrepancies.
        let mut discrepancies = Vec::new();
        let runner_ids: std::collections::HashSet<&str> = open_live_quotes
            .iter()
            .map(|(lid, _)| lid.as_str())
            .collect();
        let fill_order_ids: std::collections::HashSet<&str> =
            fills.iter().map(|f| f.order_id.as_str()).collect();
        for (lid, ticker) in &open_live_quotes {
            if !order_cid_map.contains_key(lid.as_str()) && !fill_order_ids.contains(lid.as_str()) {
                discrepancies.push(risk_rails::ReconcileDiscrepancy {
                    kind: "missing_on_venue".to_string(),
                    message: format!("runner live order {lid} on {ticker} not found on venue"),
                    ticker: Some(ticker.clone()),
                    order_id: Some(lid.clone()),
                });
            }
        }
        for o in &orders {
            if o.status.eq_ignore_ascii_case("resting")
                && !runner_ids.contains(o.order_id.as_str())
                && !st.executions.has_order_submitted(&o.order_id)
            {
                discrepancies.push(risk_rails::ReconcileDiscrepancy {
                    kind: "untracked_venue_order".to_string(),
                    message: format!(
                        "resting venue order {} on {} is not in local execution ledger",
                        o.order_id, o.ticker
                    ),
                    ticker: Some(o.ticker.clone()),
                    order_id: Some(o.order_id.clone()),
                });
            }
        }
        for p in &positions {
            if p.position != 0 && !st.executions.has_fill_for_ticker(&p.ticker) {
                discrepancies.push(risk_rails::ReconcileDiscrepancy {
                    kind: "unreconciled_position".to_string(),
                    message: format!(
                        "venue position {} ({}) on {} has no matching fill in execution ledger",
                        p.position, p.ticker, p.ticker
                    ),
                    ticker: Some(p.ticker.clone()),
                    order_id: None,
                });
            }
        }

        st.connected = true;
        st.rails.last_error = None;
        st.last_discrepancies = discrepancies.clone();
        st.rails.record_venue_sync(now, open_exposure, effective_loss);

        risk_rails::LiveReconcileResponse {
            ok: true,
            configured: true,
            connected: true,
            last_sync_epoch: Some(now),
            sync_stale: false,
            utc_day_loss_dollars: effective_loss,
            utc_day_realized_pnl_dollars: bd.realized_pnl_dollars,
            utc_day_fees_dollars: bd.fees_dollars,
            utc_day_breakdown: bd,
            open_exposure_dollars: open_exposure,
            executions: st.executions.records().to_vec(),
            discrepancies,
            last_error: None,
        }
    };

    app.invalidate_live_hot();
    let ev = format!(
        "{{\"type\":\"live_reconcile\",\"last_sync_epoch\":{},\"sync_stale\":false,\"utc_day_loss_dollars\":{},\"open_exposure_dollars\":{},\"executions_count\":{},\"ts\":{}}}",
        now,
        resp.utc_day_loss_dollars,
        resp.open_exposure_dollars,
        resp.executions.len(),
        now,
    );
    tape_hub().publish(ev);
    Ok(resp)
}

/// Background reconciler thread polling Kalshi when live credentials are configured,
/// keeping `last_venue_sync_epoch` < 30s and reconciling fills/settlements/exposure.
/// Parks idle on `Condvar` (0% CPU / 0 network) when live credentials are absent.
fn start_background_reconciler(app: Arc<App>) {
    let poll_secs = std::env::var("OPENBELL_RECONCILE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10)
        .clamp(1, 25);
    std::thread::spawn(move || {
        let mut last_seen_seq = 0u64;
        loop {
            let configured = {
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.config.is_configured()
            };
            if !configured {
                let (lock, cv) = &*app.reconcile_wake;
                let mut seq = lock.lock().unwrap_or_else(|p| p.into_inner());
                while *seq == last_seen_seq {
                    seq = cv.wait(seq).unwrap_or_else(|p| p.into_inner());
                }
                last_seen_seq = *seq;
                continue;
            }

            let _ = reconcile_live_venue(&app);

            let (lock, cv) = &*app.reconcile_wake;
            let seq = lock.lock().unwrap_or_else(|p| p.into_inner());
            if *seq == last_seen_seq {
                let (next_seq, _) = cv
                    .wait_timeout(seq, std::time::Duration::from_secs(poll_secs))
                    .unwrap_or_else(|p| p.into_inner());
                last_seen_seq = *next_seq;
            } else {
                last_seen_seq = *seq;
            }
        }
    });
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
unsafe extern "C" {
    fn mallopt(param: std::ffi::c_int, value: std::ffi::c_int) -> std::ffi::c_int;
    fn malloc_trim(pad: usize) -> std::ffi::c_int;
}

#[inline]
fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        let _ = malloc_trim(0);
    }
}

fn main() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: M_ARENA_MAX (-8), M_TRIM_THRESHOLD (-1), and M_MMAP_THRESHOLD (-3) bound heap arenas and return freed pages promptly.
    unsafe {
        mallopt(-8, 1);
        mallopt(-1, 16 * 1024);
        mallopt(-3, 16 * 1024);
    }
    if std::env::args().nth(1).as_deref().is_some_and(|a| a == "--version" || a == "-V") {
        println!("openbell-rs {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    // An explicit PORT is a promise: it is used exactly, or the start fails loudly.
    let explicit_port: Option<u16> = std::env::var("PORT").ok().map(|p| {
        p.parse().unwrap_or_else(|_| {
            panic!("PORT is set to '{p}', which is not a valid port — refusing to silently fall back to 8472")
        })
    });
    let listener = match explicit_port {
        Some(p) => TcpListener::bind(("127.0.0.1", p))
            .unwrap_or_else(|e| panic!("PORT {p} is not free ({e}); choose another PORT")),
        None => bind_free_port(DEFAULT_PORT, |p| TcpListener::bind(("127.0.0.1", p)))
            .map(|(l, _)| l)
            .unwrap_or_else(|| {
                panic!("ports {DEFAULT_PORT} to {} are all busy; set PORT to a free port", DEFAULT_PORT + DEFAULT_PORT_SPAN)
            }),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(DEFAULT_PORT);
    let get_env = |k: &str| std::env::var(k).ok();
    let data_dir = std::env::var("OPENBELL_DATA").map(PathBuf::from).unwrap_or_else(|_| {
        default_data_dir(std::env::consts::OS, &get_env, Path::new("openbell-data").is_dir())
    });
    let strategies_dir = std::env::var("OPENBELL_STRATEGIES").map(PathBuf::from).unwrap_or_else(|_| {
        let here = Path::new("strategies").is_dir();
        let dir = default_strategies_dir(&data_dir, here);
        if !here {
            if let Err(e) = seed_strategy_library(&dir) {
                eprintln!("openbell-rs: cannot write the strategy library to {}: {e}", dir.display());
            }
        }
        dir
    });
    let paper_pot: f64 = std::env::var("OPENBELL_PAPER_POT")
        .ok()
        .and_then(|p| p.parse().ok())
        .filter(|p: &f64| *p > 0.0 && p.is_finite() && *p <= 1.0e9)
        .unwrap_or(100.0);
    let _ = std::fs::create_dir_all(&data_dir);

    let collect_secs: u64 = std::env::var("OPENBELL_COLLECT_SECS")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(60);
    let collect_secs = if collect_secs == 0 {
        eprintln!(
            "OPENBELL_COLLECT_SECS=0 would busy-loop the collector against the venue; using 5s"
        );
        5
    } else if collect_secs > 3600 {
        eprintln!(
            "OPENBELL_COLLECT_SECS={collect_secs} would leave the tape stale for hours and make the health threshold meaningless; using 3600s"
        );
        3600
    } else {
        collect_secs
    };
    let (auto_collect_enabled, auto_collect_interval) =
        parse_auto_collect_config(&args, &get_env, collect_secs);

    let app = Arc::new(App::new_with_auto_collect(
        data_dir.clone(),
        strategies_dir,
        paper_pot,
        collect_secs,
        auto_collect_enabled,
        auto_collect_interval,
    ));
    let allowed_hosts: Vec<String> = std::env::var("OPENBELL_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    let url = format!("http://127.0.0.1:{port}");
    let opening = open::wanted(&args, &get_env, std::env::consts::OS);
    if opening {
        open::open_in_browser(&url);
    }
    static ACTIVE_HTTP_CONNS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    const MAX_CONCURRENT_HTTP_CONNS: usize = 128;
    const HTTP_POOL_WORKERS: usize = 16;
    const MAX_HTTP_POOL_WORKERS: usize = 24;

    struct ConnGuard;
    impl Drop for ConnGuard {
        fn drop(&mut self) {
            ACTIVE_HTTP_CONNS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    struct QueuedConn {
        stream: std::net::TcpStream,
        _guard: ConnGuard,
    }

    struct HttpPoolInner {
        queue: VecDeque<QueuedConn>,
        idle_workers: usize,
        total_workers: usize,
    }

    let pool: Arc<(Mutex<HttpPoolInner>, Condvar)> = Arc::new((
        Mutex::new(HttpPoolInner {
            queue: VecDeque::with_capacity(64),
            idle_workers: 0,
            total_workers: HTTP_POOL_WORKERS,
        }),
        Condvar::new(),
    ));
    let allowed_hosts = Arc::new(allowed_hosts);
    let spawn_worker = |pool: Arc<(Mutex<HttpPoolInner>, Condvar)>,
                        app: Arc<App>,
                        allowed_hosts: Arc<Vec<String>>,
                        burst: bool| {
        let _ = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    let QueuedConn {
                        mut stream,
                        _guard: conn_guard,
                    } = {
                        let (lock, cv) = &*pool;
                        let mut st = lock.lock().unwrap_or_else(|p| p.into_inner());
                        st.idle_workers += 1;
                        while st.queue.is_empty() {
                            if burst && st.total_workers > HTTP_POOL_WORKERS {
                                let (next_st, res) = cv
                                    .wait_timeout(st, std::time::Duration::from_secs(5))
                                    .unwrap_or_else(|p| p.into_inner());
                                st = next_st;
                                if res.timed_out() && st.queue.is_empty() && st.total_workers > HTTP_POOL_WORKERS {
                                    st.idle_workers = st.idle_workers.saturating_sub(1);
                                    st.total_workers = st.total_workers.saturating_sub(1);
                                    return;
                                }
                            } else {
                                st = cv.wait(st).unwrap_or_else(|p| p.into_inner());
                            }
                        }
                        st.idle_workers = st.idle_workers.saturating_sub(1);
                        st.queue.pop_front().unwrap()
                    };
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                    let mut in_keep_alive = false;
                    let mut sse_handed_off = false;
                    loop {
                        let first_n = match read_retry(&mut stream, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let allow_ka =
                            ACTIVE_HTTP_CONNS.load(std::sync::atomic::Ordering::Relaxed)
                                < (HTTP_POOL_WORKERS - 4);
                        if let Some(keep_alive) = try_handle_inline_get(
                            &mut stream,
                            &buf[..first_n],
                            &app,
                            &allowed_hosts,
                            allow_ka,
                        ) {
                            if keep_alive {
                                if !in_keep_alive {
                                    in_keep_alive = true;
                                    let _ = stream.set_read_timeout(Some(
                                        std::time::Duration::from_millis(500),
                                    ));
                                }
                                continue;
                            }
                            break;
                        }
                        if let Some((method, path, body, headers)) =
                            read_request_with_prefix(&mut stream, &mut buf, first_n)
                        {
                            if method == "__too_large__" {
                                let resp = "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"request body too large\"}";
                                let _ = stream.write_all(resp.as_bytes());
                                let _ = stream.flush();
                                break;
                            }
                            if method == "__bad_request__" {
                                let resp = json_err_response(400, &body);
                                let _ = stream.write_all(resp.as_bytes());
                                break;
                            }
                            let host = headers
                                .iter()
                                .find(|(n, _)| n == "host")
                                .map(|(_, v)| v.as_str())
                                .unwrap_or("");
                            if let Some((status, message)) =
                                refuse_request(&method, &headers, body.len(), &allowed_hosts)
                            {
                                let response = json_err_response(status, message);
                                let _ = stream.write_all(response.as_bytes());
                                break;
                            }
                            let route = path.split_once('?').map(|(r, _)| r).unwrap_or(&path);
                            let is_strict_mutation = (route == "/api/strategies/save"
                                && method == "POST")
                                || (route.starts_with("/api/strategies/") && method == "DELETE")
                                || (route == "/api/research/synthesize" && method == "POST")
                                || (route == "/api/analytics/quarantine/override"
                                    && method == "POST");
                            let is_strict_live = route == "/api/live"
                                || route.starts_with("/api/live/")
                                || route == "/api/runner"
                                || route.starts_with("/api/runner/")
                                || route == "/api/orderbook/subscribe"
                                || route == "/api/analytics/execution-health";
                            if is_strict_live || is_strict_mutation {
                                if is_strict_mutation {
                                    if !is_loopback_host(host) {
                                        let response = json_err_response(
                                            403,
                                            "strategy and research mutations are restricted to localhost (loopback) only",
                                        );
                                        let _ = stream.write_all(response.as_bytes());
                                        break;
                                    }
                                    let csrf = headers
                                        .iter()
                                        .find(|(n, _)| n == "x-openbell-csrf")
                                        .map(|(_, v)| v.trim())
                                        .unwrap_or("");
                                    if csrf != "1" {
                                        let response = json_err_response(
                                            403,
                                            "mutations require X-OpenBell-CSRF: 1",
                                        );
                                        let _ = stream.write_all(response.as_bytes());
                                        break;
                                    }
                                    let ct = headers
                                        .iter()
                                        .find(|(n, _)| n == "content-type")
                                        .map(|(_, v)| v.as_str())
                                        .unwrap_or("");
                                    if !(ct.len() >= 16
                                        && ct[..16].eq_ignore_ascii_case("application/json"))
                                    {
                                        let response = json_err_response(
                                            415,
                                            "mutations require Content-Type: application/json",
                                        );
                                        let _ = stream.write_all(response.as_bytes());
                                        break;
                                    }
                                } else if let Some((status, message)) =
                                    refuse_live_request(&method, host, &headers)
                                {
                                    let response = json_err_response(status, message);
                                    let _ = stream.write_all(response.as_bytes());
                                    break;
                                }
                                let is_e1_live_mutation = !matches!(method.as_str(), "GET" | "HEAD")
                                    && ((route.starts_with("/api/live/orders/")
                                        && route.ends_with("/amend"))
                                        || (route.starts_with("/api/live/positions/")
                                            && route.ends_with("/flatten")));
                                if is_e1_live_mutation {
                                    let csrf = headers
                                        .iter()
                                        .find(|(n, _)| n == "x-openbell-csrf")
                                        .map(|(_, v)| v.trim())
                                        .unwrap_or("");
                                    if csrf != "1" {
                                        let response = json_err_response(
                                            403,
                                            "mutations require X-OpenBell-CSRF: 1",
                                        );
                                        let _ = stream.write_all(response.as_bytes());
                                        break;
                                    }
                                }
                                if (route == "/api/runner"
                                    || route.starts_with("/api/runner/")
                                    || route == "/api/orderbook/subscribe"
                                    || route == "/api/live/reconcile"
                                    || route == "/api/live/reset-kill"
                                    || is_e1_live_mutation)
                                    && !matches!(method.as_str(), "GET" | "HEAD")
                                {
                                    let ct = headers
                                        .iter()
                                        .find(|(n, _)| n == "content-type")
                                        .map(|(_, v)| v.as_str())
                                        .unwrap_or("");
                                    if !(ct.len() >= 16
                                        && ct[..16].eq_ignore_ascii_case("application/json"))
                                    {
                                        let response = json_err_response(
                                            415,
                                            "mutations require Content-Type: application/json",
                                        );
                                        let _ = stream.write_all(response.as_bytes());
                                        break;
                                    }
                                }
                            }
                            if method == "GET" && (route == "/api/tape/stream" || route == "/api/stream") {
                                let sse_app = app.clone();
                                sse_handed_off = true;
                                std::thread::spawn(move || {
                                    let _sse_guard = conn_guard;
                                    serve_sse_stream(&mut stream, &sse_app);
                                });
                                break;
                            }
                            let _ = &conn_guard;
                            if matches!(method.as_str(), "GET" | "HEAD") && route == "/" {
                                let _ = write_index_response(&mut stream, &method, &headers);
                                break;
                            }
                            if matches!(method.as_str(), "GET" | "HEAD") {
                                if let Some(chunk_name) = route.strip_prefix("/chunks/") {
                                    if !chunk_name.contains('/') && !chunk_name.contains("..") {
                                        if let Some(chunk) = embedded_chunks::lookup(chunk_name) {
                                            let _ = write_embedded_chunk(
                                                &mut stream,
                                                &method,
                                                &headers,
                                                chunk,
                                            );
                                            break;
                                        }
                                    }
                                    let resp =
                                        json_response(404, "{\"error\":\"chunk not found\"}");
                                    let _ = stream.write_all(resp.as_bytes());
                                    break;
                                }
                            }
                            if method == "GET"
                                && try_serve_fast_get(&mut stream, &app, &path, false, false)
                            {
                                break;
                            }
                            let response = update_response(&method, &path, host)
                                .unwrap_or_else(|| handle_with_body(&app, &method, &path, &body));
                            if method != "GET" && method != "HEAD" {
                                app.invalidate_live_hot();
                            }
                            let _ = stream.write_all(response.as_bytes());
                        }
                        break;
                    }
                    let _ = sse_handed_off;
                }
            });
    };
    for _ in 0..HTTP_POOL_WORKERS {
        spawn_worker(pool.clone(), app.clone(), allowed_hosts.clone(), false);
    }
    start_background_collector(app.clone());
    shadow_runner::start_background_runner(app.clone());
    start_background_reconciler(app.clone());
    orderbook::start_background_orderbook(app.clone());
    spot_feed::start_background_spot_feed(app.clone());
    startup_discover_open_markets(app.clone());
    updater().start_background();
    println!("{}", start_banner(&url, &data_dir, updater().cfg.mode, opening));

    for mut stream in listener.incoming().flatten() {
        if ACTIVE_HTTP_CONNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            >= MAX_CONCURRENT_HTTP_CONNS
        {
            ACTIVE_HTTP_CONNS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            let resp = json_response(503, "{\"error\":\"server busy\"}");
            let _ = stream.write_all(resp.as_bytes());
            continue;
        }
        let (lock, cv) = &*pool;
        let mut st = lock.lock().unwrap_or_else(|p| p.into_inner());
        st.queue.push_back(QueuedConn {
            stream,
            _guard: ConnGuard,
        });
        let need_burst = st.idle_workers == 0 && st.total_workers < MAX_HTTP_POOL_WORKERS;
        if need_burst {
            st.total_workers += 1;
        }
        drop(st);
        cv.notify_one();
        if need_burst {
            spawn_worker(pool.clone(), app.clone(), allowed_hosts.clone(), true);
        }
    }
}

/// Discover active open markets across `active_series()` at startup (non-blocking)
/// and sync them into `OrderbookStore` so L2 WS/REST begins streaming immediately.
fn startup_discover_open_markets(app: Arc<App>) {
    std::thread::spawn(move || {
        if std::env::var("OPENBELL_ORDERBOOK_OFFLINE").as_deref() == Ok("1") {
            return;
        }

        // Public REST discovery of currently open markets across active_series().
        let pace_ms: u64 = std::env::var("OPENBELL_COLLECT_PACE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50)
            .min(500);
        let mut live_open: Vec<Snapshot> = Vec::new();
        for (idx, series) in active_series().iter().enumerate() {
            if idx > 0 && pace_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(pace_ms));
            }
            if let Ok((snaps, _, _)) = fetch_markets(series, "open") {
                live_open.extend(snaps);
            }
        }
        let open_tickers = prioritized_open_tickers(&live_open);
        if !open_tickers.is_empty() {
            app.orderbook.sync_active_markets(&open_tickers, &[]);
        }
    });
}

/// Self-update state, one per process: the production entry starts its
/// background checks; tests get a quiet one that never calls out.
static UPDATER: std::sync::OnceLock<std::sync::Arc<update::Updater>> = std::sync::OnceLock::new();

fn updater() -> &'static std::sync::Arc<update::Updater> {
    UPDATER.get_or_init(|| {
        update::Updater::from_env("OPENBELL", "openbell-rs", env!("CARGO_PKG_VERSION"), "cayleb-james2008/openbell", true)
    })
}

/// Collection passes in progress, so an update restart never lands mid-pass.
static PASSES_IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct PassGuard;

impl PassGuard {
    fn new() -> PassGuard {
        PASSES_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        PassGuard
    }
}

impl Drop for PassGuard {
    fn drop(&mut self) {
        PASSES_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Only this machine may replace the program: its Host must be a loopback
/// name. A name listed in OPENBELL_ALLOWED_HOSTS may read the desk, not update it.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    matches!(authority_host(&host), "localhost" | "127.0.0.1" | "[::1]")
}

/// Why a restart is refused, if it is; `None` means it may go ahead.
fn restart_refusal(staged: bool, pass_running: bool) -> Option<&'static str> {
    if !staged {
        Some("no update is waiting for a restart")
    } else if pass_running {
        Some("a collection pass is still running; restart when it has finished")
    } else {
        None
    }
}

/// The version and self-update routes. Returns None for any other path.
fn update_response(method: &str, path: &str, host: &str) -> Option<String> {
    let up = updater();
    Some(match (method, path) {
        ("GET", "/api/version") => json_response(
            200,
            &json!({
                "name": "openbell-rs",
                "version": env!("CARGO_PKG_VERSION"),
                "release_url": format!("https://github.com/{}/releases/tag/v{}", up.cfg.repo, env!("CARGO_PKG_VERSION")),
            })
            .to_string(),
        ),
        ("GET", "/api/update") => json_response(200, &up.snapshot().to_string()),
        ("POST", "/api/update/check") => json_response(200, &up.check_now().to_string()),
        ("POST", "/api/update/apply") | ("POST", "/api/update/restart") => {
            if !is_loopback_host(host) {
                return Some(json_err_response(
                    403,
                    "updating is only allowed from this machine, not through a shared address",
                ));
            }
            if path == "/api/update/apply" {
                match up.apply() {
                    Ok(v) => {
                        let mut snap = up.snapshot();
                        snap["applied"] = json!(v);
                        json_response(200, &snap.to_string())
                    }
                    Err(e) => json_err_response(409, &e),
                }
            } else if let Some(e) = restart_refusal(
                up.snapshot()["staged"].is_string(),
                PASSES_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst) > 0,
            ) {
                json_err_response(409, e)
            } else {
                let restarter = up.clone();
                std::thread::spawn(move || {
                    // Give the answer below time to leave before this process is replaced.
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    if let Err(e) = restarter.restart() {
                        eprintln!("openbell-rs: restart failed: {e}");
                    }
                });
                json_response(200, "{\"restarting\":true}")
            }
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn extension_keeps_untouched_window_fingerprints() {
        // The entry index is keyed by window fingerprints; tape
        // extension must never change the fingerprint of a window
        // the append did not touch, or every recompute would
        // re-scan the whole tape.
        let dir = std::env::temp_dir().join(format!("ob-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snapshots.jsonl");
        let line = |ticker: &str, at: i64, result: &str| {
            let s = snap(ticker, at, "2026-11-18T00:15:00Z", result, Some(0.62), Some(0.40));
            format!("{}\n", serde_json::to_string(&s).unwrap())
        };
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&line("KXAAA-1", 1000 + i, if i == 49 { "yes" } else { "" }));
            text.push_str(&line("KXBBB-1", 1000 + i, if i == 49 { "no" } else { "" }));
        }
        std::fs::write(&path, &text).unwrap();
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        let v1 = app.tape(&path);
        let fp_b1 = window_fingerprint(&v1.grouped["KXBBB-1"]);
        let fp_a1 = window_fingerprint(&v1.grouped["KXAAA-1"]);
        let mut extra = String::new();
        for i in 50..60 {
            extra.push_str(&line("KXAAA-1", 1000 + i, "yes"));
        }
        std::fs::write(&path, format!("{text}{extra}")).unwrap();
        let v2 = app.tape(&path);
        assert_ne!(fp_a1, window_fingerprint(&v2.grouped["KXAAA-1"]));
        assert_eq!(
            fp_b1,
            window_fingerprint(&v2.grouped["KXBBB-1"]),
            "untouched window's fingerprint changed across extension"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn entry_scan_resumes_over_a_grown_window() {
        // A strategy that only enters on a snapshot that arrives
        // in a later collect pass: the grown window's entry must
        // be the tail entry (found by resuming the prefix scan),
        // and a window whose entry sits in the prefix must keep
        // that entry after growing.
        let dir = std::env::temp_dir().join(format!("ob-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("late.rn"),
            "pub fn name() { \"late\" }\npub fn side() { \"yes\" }\n\
             pub fn allow_entry(yes_ask, no_ask, secs_to_close) { yes_ask >= 0.60 }\n",
        )
        .unwrap();
        let strat = load_strategies(&dir).remove(0);
        let close = "2030-01-01T01:00:00Z";
        let prefix: Vec<Snapshot> = (0..5)
            .map(|i| snap("KXRES-1", 100 + i, close, "", Some(0.50), Some(0.52)))
            .collect();
        assert_eq!(window_entry_keyed(&strat, Some("KXRES-1"), &prefix, window_fingerprint(&prefix)), None);
        let mut grown = prefix.clone();
        grown.push(snap("KXRES-1", 200, close, "yes", Some(0.62), Some(0.40)));
        assert_eq!(
            window_entry_keyed(&strat, Some("KXRES-1"), &grown, window_fingerprint(&grown)),
            Some(0.62),
            "resumed scan missed the tail entry"
        );
        // Entry inside the prefix survives growth verbatim.
        let early: Vec<Snapshot> = (0..5)
            .map(|i| snap("KXRES-2", 100 + i, close, "", Some(0.62), Some(0.40)))
            .collect();
        assert_eq!(window_entry_keyed(&strat, Some("KXRES-2"), &early, window_fingerprint(&early)), Some(0.62));
        let mut grown2 = early.clone();
        grown2.push(snap("KXRES-2", 200, close, "yes", Some(0.50), Some(0.52)));
        assert_eq!(
            window_entry_keyed(&strat, Some("KXRES-2"), &grown2, window_fingerprint(&grown2)),
            Some(0.62)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn snap(ticker: &str, fetched_at: i64, close: &str, result: &str, ya: Option<f64>, na: Option<f64>) -> Snapshot {
        Snapshot {
            fetched_at,
            ticker: ticker.into(),
            event_ticker: "EV".into(),
            series: "KXBTC15M".into(),
            close_time: close.into(),
            status: if result.is_empty() { "active".into() } else { "settled".into() },
            result: result.into(),
            yes_bid: OptPrice::from_opt(ya.map(|v| v - 0.02)),
            yes_ask: OptPrice::from_opt(ya),
            no_bid: OptPrice::from_opt(na.map(|v| v - 0.02)),
            no_ask: OptPrice::from_opt(na),
            last_price: OptPrice::NONE,
            yes_bid_size: 0.0,
            no_bid_size: 0.0,
            floor_strike: None,
            cap_strike: None,
            spot_price: None,
        }
    }

    #[test]
    fn iso_parse_rejects_out_of_range_fields() {
        // Round-2 adversarial: an unchecked parse computed a
        // confident epoch for month 13 / day 45 / hour 99.
        assert!(parse_iso_z("2026-13-45T99:99:99Z").is_none());
        assert!(parse_iso_z("2026-02-30T00:00:00Z").is_none());
        assert!(parse_iso_z("2025-02-29T00:00:00Z").is_none());
        assert!(parse_iso_z("2024-02-29T00:00:00Z").is_some());
        assert!(parse_iso_z("not-a-date").is_none());
        assert_eq!(parse_iso_z("2030-01-01T01:00:00Z"), Some(1893459600));
        assert_eq!(iso_z(1893459600), "2030-01-01T01:00:00Z");
    }

    #[test]
    fn window_facts_conflict_and_epoch() {
        let snap = |close: &str, result: &str, fetched: i64| Snapshot {
            fetched_at: fetched,
            ticker: "T".into(),
            event_ticker: "T".into(),
            series: "S".into(),
            close_time: close.into(),
            status: "active".into(),
            result: result.into(),
            yes_bid: OptPrice::NONE,
            yes_ask: OptPrice::from_opt(Some(0.5)),
            no_bid: OptPrice::NONE,
            no_ask: OptPrice::from_opt(Some(0.5)),
            last_price: OptPrice::NONE,
            yes_bid_size: 0.0,
            no_bid_size: 0.0,
            floor_strike: None,
            cap_strike: None,
            spot_price: None,
        };
        // Conflicting settlements: not scored, ever.
        let conflict = vec![
            snap("2030-01-01T00:15:00Z", "yes", 100),
            snap("2030-01-01T00:15:00Z", "no", 110),
        ];
        assert!(matches!(window_facts(&conflict), WindowFacts::Conflict));
        // The epoch is the settled record's own close time, not
        // the earliest snapshot's (whose close_time is garbage).
        let poisoned = vec![
            snap("not-a-date", "", 100),
            snap("2030-01-01T01:00:00Z", "no", 110),
        ];
        match window_facts(&poisoned) {
            WindowFacts::Settled { result, close_epoch } => {
                assert_eq!(result, "no");
                assert_eq!(close_epoch, 1893459600);
            }
            _ => panic!("expected a settled window"),
        }
        // Unsettled stays unsettled.
        let open = vec![snap("2030-01-01T01:00:00Z", "", 100)];
        assert!(matches!(window_facts(&open), WindowFacts::Unsettled));
    }

    #[test]
    fn iso_parse_round_trip() {
        assert_eq!(parse_iso_z("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_z("1970-01-02T00:00:00Z"), Some(86400));
        assert!(parse_iso_z("2026-11-17T23:00:00Z").unwrap() > 1_700_000_000);
    }

    #[test]
    fn fee_model() {
        assert!((fee(0.5) - 0.0175).abs() < 1e-9);
        assert!(fee(0.9) < fee(0.5));
    }

    #[test]
    fn insufficient_evidence_is_the_honest_default() {
        let strat = Strategy::load(Path::new("strategies/favourite_late.rn")).unwrap();
        let report = score_strategy(&strat, &[], Standard::default());
        assert_eq!(report.windows_traded, 0);
        assert!(report.verdict.contains("INSUFFICIENT EVIDENCE"));
    }

    #[test]
    fn scorer_rewards_a_winning_shape_and_kills_a_losing_one() {
        // 6 windows: YES settles in 5. favourite-late buys YES at 0.70;
        // longshot-late buys NO at 0.32 and loses 5 of 6.
        let mut snaps = Vec::new();
        for i in 0..6 {
            let close = format!("2026-11-17T{:02}:00:00Z", 10 + i);
            let close_epoch = parse_iso_z(&close).unwrap();
            let result = if i == 5 { "no" } else { "yes" };
            snaps.push(snap(&format!("T{i}"), close_epoch - 240, &close, "", Some(0.70), Some(0.32)));
            snaps.push(snap(&format!("T{i}"), close_epoch + 60, &close, result, None, None));
        }
        let standard = Standard { min_windows: 4, min_mean_cents: 3.5 };
        let fav = Strategy::load(Path::new("strategies/favourite_late.rn")).unwrap();
        let report = score_strategy(&fav, &snaps, standard);
        assert_eq!(report.windows_traded, 6);
        assert!(report.verdict.contains("PROVEN"), "verdict: {}", report.verdict);
        let long = Strategy::load(Path::new("strategies/longshot_late.rn")).unwrap();
        let report = score_strategy(&long, &snaps, standard);
        assert_eq!(report.windows_traded, 6);
        assert!(report.verdict.contains("NOT PROVEN"), "verdict: {}", report.verdict);
    }

    fn losing_windows(n: usize, results: &[&str]) -> Vec<Snapshot> {
        // Windows with a tradeable snapshot 240s before close
        // (yes_ask 0.70 / no_ask 0.32), settled per `results`.
        let mut snaps = Vec::new();
        for (i, &res) in results.iter().enumerate().take(n) {
            let close = format!("2026-11-17T{:02}:00:00Z", 10 + i);
            let close_epoch = parse_iso_z(&close).unwrap();
            snaps.push(snap(&format!("W{i}"), close_epoch - 240, &close, "", Some(0.70), Some(0.32)));
            snaps.push(snap(&format!("W{i}"), close_epoch + 60, &close, res, None, None));
        }
        snaps
    }

    #[test]
    fn sizing_compounds_the_pot_across_windows() {
        // One strategy in a scratch library, so N=1 and the arithmetic
        // is checkable by hand. Two winning windows at yes 0.70:
        //   W1: share = 100.00 -> 142 contracts; P&L = 142 - 99.40
        //       - 142*fee(0.70) = +40.5126; equity 140.5126.
        //   W2: share = 140.5126 (the COMPOUNDED equity, not the
        //       starting pot) -> 200 contracts; P&L = +57.06.
        // Under a fixed starting-pot rule W2 would repeat W1 and the
        // total would be 81.03; compounding gives 97.57.
        let dir = std::env::temp_dir().join(format!("openbell-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rn = std::fs::read_to_string("strategies/favourite_late.rn").unwrap();
        std::fs::write(dir.join("favourite_late.rn"), rn).unwrap();
        let strategies = load_strategies(&dir);
        assert_eq!(strategies.len(), 1);
        let mut snaps = Vec::new();
        for i in 0..2 {
            let close = format!("2026-11-17T1{i}:00:00Z");
            let close_epoch = parse_iso_z(&close).unwrap();
            snaps.push(snap(&format!("W{i}"), close_epoch - 240, &close, "", Some(0.70), Some(0.32)));
            snaps.push(snap(&format!("W{i}"), close_epoch + 60, &close, "yes", None, None));
        }
        let sim = simulate_slots(&strategies, &snaps, 100.0);
        let total = &sim.strategy_totals[0];
        assert_eq!(total.windows_traded, 2);
        assert!(
            (total.paper_pnl_dollars - 97.57).abs() < 0.01,
            "compounded P&L should be ~97.57, got {}",
            total.paper_pnl_dollars
        );
        assert!((sim.paper_equity_dollars - 197.57).abs() < 0.01);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slot_rotates_after_three_consecutive_strikes() {
        let strategies = load_strategies(Path::new("strategies"));
        let fav_idx = strategies
            .iter()
            .position(|s| s.name == "favourite-late")
            .expect("library holds favourite-late");
        // YES loses every window: favourite-late (buys YES at 0.70)
        // strikes on windows 1-3 and must be retired from its slot.
        // Exactly three windows: enough to retire, not enough for the
        // book to cycle a strategy back into the slot.
        let snaps = losing_windows(3, &["no", "no", "no"]);
        let sim = simulate_slots(&strategies, &snaps, 100.0);
        let retired = sim
            .retired
            .iter()
            .find(|r| r.strategy == "favourite-late")
            .expect("favourite-late was retired");
        assert_eq!(retired.slot, fav_idx);
        assert_eq!(retired.strikes, 3);
        assert_eq!(retired.windows_traded, 3);
        // The successor is the strategy freed when favourite-floor's
        // slot retired moments earlier in the same window — recorded
        // on the retirement itself, not read off the end state.
        assert_eq!(retired.rotated_to, "favourite-floor");
        assert_eq!(sim.slots[fav_idx].strategy, "favourite-floor");
        assert_eq!(sim.slots[fav_idx].rotations, 1);
    }

    #[test]
    fn strikes_are_consecutive_a_win_clears_them() {
        let strategies = load_strategies(Path::new("strategies"));
        let fav_idx = strategies
            .iter()
            .position(|s| s.name == "favourite-late")
            .expect("library holds favourite-late");
        // YES record L L W L L — favourite-late never takes three
        // strikes in a row, so it is not retired and ends on 2 strikes.
        let snaps = losing_windows(5, &["no", "no", "yes", "no", "no"]);
        let sim = simulate_slots(&strategies, &snaps, 100.0);
        assert!(
            !sim.retired.iter().any(|r| r.strategy == "favourite-late"),
            "favourite-late must not be retired"
        );
        assert_eq!(sim.slots[fav_idx].strategy, "favourite-late");
        assert_eq!(sim.slots[fav_idx].strikes, 2);
        assert_eq!(sim.slots[fav_idx].windows_traded, 5);
    }

    #[test]
    fn paper_sizing_is_equity_split_and_recomputes() {
        let fav = Strategy::load(Path::new("strategies/favourite_late.rn")).unwrap();
        // One slot, $100 pot: the slot stakes its full share.
        // contracts = floor(100 / 0.70) = 142; a win pays 142 back
        // against 142*0.70 cost and 142*fee(0.70) fees.
        let snaps = losing_windows(1, &["yes"]);
        let sim = simulate_slots(&[fav], &snaps, 100.0);
        assert_eq!(sim.trades.len(), 1);
        let t = &sim.trades[0];
        assert_eq!(t.contracts, 142);
        let expected = 142.0 - 142.0 * 0.70 - 142.0 * fee(0.70);
        assert!((t.pnl_dollars - expected).abs() < 1e-9, "pnl {} vs {expected}", t.pnl_dollars);
        assert!((sim.paper_equity_dollars - (100.0 + expected)).abs() < 1e-9);
        // Two strategies -> two slots -> $50 share each:
        // contracts = floor(50 / 0.70) = 71 for the favourite's slot.
        let strategies = load_strategies(Path::new("strategies"));
        let two: Vec<Strategy> = strategies
            .into_iter()
            .filter(|s| s.name == "favourite-late" || s.name == "longshot-late")
            .collect();
        assert_eq!(two.len(), 2);
        let sim = simulate_slots(&two, &snaps, 100.0);
        let fav_trade = sim
            .trades
            .iter()
            .find(|t| t.strategy == "favourite-late")
            .expect("favourite traded");
        assert_eq!(fav_trade.contracts, 71);
    }

    #[test]
    fn equity_curve_final_points_match_strategy_totals() {
        let strategies = load_strategies(Path::new("strategies"));
        let snaps = losing_windows(6, &["no", "yes", "no", "yes", "no", "yes"]);
        let sim = simulate_slots(&strategies, &snaps, 100.0);
        let (pot, per) = equity_curves(&sim);
        // Pot curve ends at the simulation's final paper equity.
        assert!((pot.last().unwrap().1 - sim.paper_equity_dollars).abs() < 1e-9);
        // Each strategy's curve ends at its strategy total.
        for (name, points) in &per {
            let total = sim
                .strategy_totals
                .iter()
                .find(|t| &t.strategy == name)
                .unwrap();
            assert!(
                (points.last().unwrap().1 - total.paper_pnl_dollars).abs() < 1e-9,
                "{name}: curve {} vs total {}",
                points.last().unwrap().1,
                total.paper_pnl_dollars
            );
        }
    }

    fn write_strategy(dir: &Path, file: &str, source: &str) -> PathBuf {
        let path = dir.join(file);
        std::fs::write(&path, source).unwrap();
        path
    }

    #[test]
    fn memory_limit_bounds_a_hostile_strategy() {
        // The proven attack: one format! call writes ~600 MB inside
        // ~24 VM instructions — far inside the instruction budget.
        // Under the memory limit the call must error and degrade to
        // no-entry, counted — never allocate its way out.
        let dir = std::env::temp_dir().join(format!("ob-mem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hostile = write_strategy(
            &dir,
            "hostile.rn",
            "pub fn name() { \"hostile-hoard\" }\n\
             pub fn side() { \"yes\" }\n\
             pub fn allow_entry(yes_ask, no_ask, secs_to_close) {\n\
                 let s = format!(\"{:>600000000}\", \"x\");\n\
                 s.len() > 0 && yes_ask <= 0.99\n\
             }\n",
        );
        let strat = Strategy::load(&hostile).expect("hostile strategy loads");
        assert!(!strat.allow_entry(0.50, 0.50, 120));
        assert!(
            strat.call_failures() >= 1,
            "the memory breach must be counted as a failed call"
        );
        // An honest predicate over the same numbers is unaffected.
        let honest = write_strategy(
            &dir,
            "honest.rn",
            "pub fn name() { \"honest\" }\n\
             pub fn side() { \"yes\" }\n\
             pub fn allow_entry(yes_ask, no_ask, secs_to_close) { yes_ask <= 0.85 }\n",
        );
        let strat = Strategy::load(&honest).expect("honest strategy loads");
        assert!(strat.allow_entry(0.50, 0.50, 120));
        assert_eq!(strat.call_failures(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn entry_index_recomputes_a_window_that_grew() {
        // A window whose snapshots give no entry, then grows by
        // append (the collector's only write mode) to include an
        // entering snapshot: the index must serve the NEW window's
        // entry, never the stale no-entry.
        let dir = std::env::temp_dir().join(format!("ob-idx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_strategy(
            &dir,
            "late.rn",
            "pub fn name() { \"late\" }\n\
             pub fn side() { \"yes\" }\n\
             pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close <= 300 }\n",
        );
        let close = "2030-01-01T01:00:00Z"; // epoch 1893459600
        let early = vec![snap("IDX", 1893459100, close, "yes", Some(0.60), Some(0.42))];
        let strat = Strategy::load(&path).unwrap();
        assert_eq!(window_entry(&strat, &early), None);
        let mut grown = early.clone();
        grown.push(snap("IDX", 1893459400, close, "yes", Some(0.62), Some(0.40)));
        let strat = Strategy::load(&path).unwrap();
        assert_eq!(window_entry(&strat, &grown), Some(0.62));
        // And the unchanged window still resolves identically.
        let strat = Strategy::load(&path).unwrap();
        assert_eq!(window_entry(&strat, &grown), Some(0.62));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_terminating_strategy_exhausts_on_budget_deaths() {
        // A while-true allow_entry dies on the instruction budget
        // every call. The aggregate clock alone would take the full
        // deadline to name it (and on a small tape, never within
        // the pass); the death-run trigger must name it exhausted
        // after BUDGET_DEATH_LIMIT consecutive budget halts — and
        // this also pins is_budget_halt to the rune version's
        // wording: if a rune bump changes the halt text, the deaths
        // stop counting and this test fails loudly.
        let dir = std::env::temp_dir().join(format!("ob-loop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_strategy(
            &dir,
            "looper.rn",
            "pub fn name() { \"looper\" }\n\
             pub fn side() { \"yes\" }\n\
             pub fn allow_entry(yes_ask, no_ask, secs_to_close) {\n\
                 let x = 0;\n\
                 while true { x += 1; }\n\
                 x > 0\n\
             }\n",
        );
        let strat = Strategy::load(&path).unwrap();
        for _ in 0..BUDGET_DEATH_LIMIT {
            assert!(!strat.allow_entry(0.5, 0.5, 120));
        }
        assert!(
            strat.budget_exhausted(),
            "a non-terminating strategy was not named exhausted after {BUDGET_DEATH_LIMIT} budget deaths"
        );
        assert_eq!(strat.call_failures(), BUDGET_DEATH_LIMIT);
        // Once exhausted, calls stop burning: no more failures accrue.
        assert!(!strat.allow_entry(0.5, 0.5, 120));
        assert_eq!(strat.call_failures(), BUDGET_DEATH_LIMIT);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stage_progress_math() {
        let mk = |equity: f64| SlotSimulation {
            conflicted_windows: 0,
            paper_pot_start: 100.0,
            paper_equity_dollars: equity,
            slots: Vec::new(),
            retired: Vec::new(),
            strategy_totals: Vec::new(),
            notes: Vec::new(),
            trades: Vec::new(),
        };
        let st = stage_progress(&mk(250.0));
        assert_eq!(st.target_dollars, 1000.0);
        assert_eq!(st.start_dollars, 100.0);
        assert!((st.progress_pct - 25.0).abs() < 1e-9);
        assert_eq!(st.remaining_dollars, 750.0);
        assert!(!st.reached);
        let st = stage_progress(&mk(1250.0));
        assert!(st.reached);
        assert_eq!(st.remaining_dollars, 0.0);
        assert!((st.progress_pct - 125.0).abs() < 1e-9);
        // A losing pot reads as a losing pot: negative progress,
        // more than the whole target still to go.
        let st = stage_progress(&mk(-50.0));
        assert!(!st.reached);
        assert!((st.progress_pct + 5.0).abs() < 1e-9);
        assert_eq!(st.remaining_dollars, 1050.0);
    }

    #[test]
    fn tape_grows_by_append_and_survives_replacement() {
        let dir = std::env::temp_dir().join(format!("ob-tape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tape = dir.join("snapshots.jsonl");
        let line = |t: &str, at: i64| {
            serde_json::to_string(&snap(t, at, "2030-01-01T01:00:00Z", "", Some(0.5), Some(0.5)))
                .unwrap()
        };
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        std::fs::write(&tape, format!("{}\n{}\nnot json\n", line("A", 100), line("B", 110)))
            .unwrap();
        let view = app.tape(&tape);
        assert_eq!((view.snaps_count, view.total_lines), (2, 3), "garbage line counted, not parsed");
        // Append-only growth: the held parse extends by the tail.
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(&tape).unwrap();
        writeln!(f, "{}", line("C", 120)).unwrap();
        drop(f);
        let view = app.tape(&tape);
        assert_eq!((view.snaps_count, view.total_lines), (3, 4));
        assert!(view.grouped.contains_key("C"));
        // Wholesale replacement with different content (longer, and
        // the seam line differs): the seam check must refuse the
        // incremental path and the new content must be what is read.
        let mut replacement = String::new();
        for i in 0..6 {
            replacement.push_str(&line("ZZ", 200 + i));
            replacement.push('\n');
        }
        std::fs::write(&tape, &replacement).unwrap();
        let view = app.tape(&tape);
        assert_eq!((view.snaps_count, view.total_lines), (6, 6));
        assert!(view.grouped.keys().all(|k| k == "ZZ"));
        // A shorter replacement parses whole too.
        std::fs::write(&tape, format!("{}\n", line("Q", 300))).unwrap();
        let view = app.tape(&tape);
        assert_eq!((view.snaps_count, view.total_lines), (1, 1));
        assert!(view.grouped.contains_key("Q"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_non_utf8_line_does_not_hide_the_tape() {
        let dir = std::env::temp_dir().join(format!("ob-utf8-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tape = dir.join("snapshots.jsonl");
        let line = |t: &str, at: i64| {
            serde_json::to_string(&snap(t, at, "2030-01-01T01:00:00Z", "", Some(0.5), Some(0.5)))
                .unwrap()
        };
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        // A valid snapshot whose ticker holds a byte that is not UTF-8.
        let mut bad = line("Z", 105).into_bytes();
        let z = bad.iter().position(|c| *c == b'Z').unwrap();
        bad[z] = 0xff;
        let mut bytes = format!("{}\n", line("A", 100)).into_bytes();
        bytes.extend_from_slice(&bad);
        bytes.push(b'\n');
        bytes.extend_from_slice(format!("{}\n", line("B", 110)).as_bytes());
        std::fs::write(&tape, &bytes).unwrap();
        let view = app.tape(&tape);
        assert_eq!(
            (view.snaps_count, view.total_lines),
            (2, 3),
            "the invalid line is dropped and counted; its neighbours still parse"
        );
        // The same holds for a line that arrives by append.
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(&tape).unwrap();
        f.write_all(&bad).unwrap();
        f.write_all(b"\n").unwrap();
        writeln!(f, "{}", line("C", 120)).unwrap();
        drop(f);
        let view = app.tape(&tape);
        assert_eq!((view.snaps_count, view.total_lines), (3, 5));
        assert!(view.grouped.contains_key("C"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pass_after_a_torn_tape_line_is_not_spliced_onto_it() {
        let dir = std::env::temp_dir().join(format!("ob-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tape = dir.join("snapshots.jsonl");
        let line = |t: &str, at: i64| {
            serde_json::to_string(&snap(t, at, "2030-01-01T01:00:00Z", "", Some(0.5), Some(0.5)))
                .unwrap()
        };
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        // A crash left a torn last line with no newline.
        std::fs::write(&tape, format!("{}\n{{\"ticker\":\"TO", line("A", 100))).unwrap();
        let view = app.tape(&tape);
        assert_eq!((view.snaps_count, view.total_lines), (1, 2));
        // The next pass lands after the torn line.
        append_tape(&tape, &format!("{}\n{}\n", line("B", 110), line("C", 120))).unwrap();
        let view = app.tape(&tape);
        let tickers: Vec<&str> = view.grouped.keys().map(|s| s.as_str()).collect();
        assert_eq!(tickers, ["A", "B", "C"], "the pass must not be lost inside the torn line");
        assert_eq!(view.total_lines, 4, "the torn fragment is its own counted line");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_site_rule_refuses_foreign_writes() {
        let headers = |host: &str, extra: &[(&str, &str)]| {
            let mut h = vec![("host".to_string(), host.to_string())];
            h.extend(extra.iter().map(|(n, v)| (n.to_string(), v.to_string())));
            h
        };
        let json_foreign = [("content-type", "application/json"), ("origin", "http://evil.example")];
        let json_same = [("content-type", "application/json"), ("origin", "http://127.0.0.1:9820")];
        // A foreign page's POST is refused; the same origin is served.
        assert_eq!(
            refuse_request("POST", &headers("127.0.0.1:9820", &json_foreign), 2, &[]).map(|r| r.0),
            Some(403),
            "foreign Origin refused"
        );
        assert_eq!(refuse_request("POST", &headers("127.0.0.1:9820", &json_same), 2, &[]), None);
        // A shell POST without a body is served; a text/plain body is not.
        assert_eq!(refuse_request("POST", &headers("localhost:9820", &[]), 0, &[]), None);
        assert_eq!(
            refuse_request("POST", &headers("127.0.0.1:9820", &[("content-type", "text/plain")]), 20, &[])
                .map(|r| r.0),
            Some(415),
            "text/plain POST refused"
        );
        assert_eq!(
            refuse_request("POST", &headers("127.0.0.1:9820", &[("sec-fetch-site", "cross-site")]), 0, &[])
                .map(|r| r.0),
            Some(403)
        );
        // A foreign Host is refused on every method unless allowed; reads stay open locally.
        assert_eq!(
            refuse_request("GET", &headers("openbell.example", &[]), 0, &[]).map(|r| r.0),
            Some(403),
            "foreign Host refused"
        );
        assert_eq!(
            refuse_request("GET", &headers("openbell.example", &[]), 0, &["openbell.example".to_string()]),
            None
        );
        assert_eq!(refuse_request("GET", &headers("127.0.0.1:9820", &json_foreign), 0, &[]), None);
    }

    #[test]
    fn example_tape_verdict_never_claims_forward_evidence() {
        let live = "PROVEN on forward evidence (paper only — no orders placed)";
        assert_eq!(dataset_verdict(live, "live"), live);
        assert_eq!(
            dataset_verdict(live, "fixture"),
            "PROVEN on the example tape (not forward evidence, paper only - no orders placed)"
        );
    }

    #[test]
    fn only_whole_cent_prices_are_traded() {
        // README: only whole-cent prices size a trade. An odd quote
        // (0.4567) or a sub-cent quote (0.004) is no market.
        let dir = std::env::temp_dir().join(format!("ob-cents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("any.rn"),
            "pub fn name() { \"any\" }\npub fn side() { \"yes\" }\n\
             pub fn allow_entry(yes_ask, no_ask, secs_to_close) { true }\n",
        )
        .unwrap();
        let strat = load_strategies(&dir).remove(0);
        let close = "2030-01-01T01:00:00Z";
        let scan = |ticker: &str, ask: f64| {
            let w = [snap(ticker, 100, close, "", Some(ask), Some(0.52))];
            window_entry_keyed(&strat, Some(ticker), &w, window_fingerprint(&w))
        };
        assert_eq!(scan("KXCENT-ODD", 0.4567), None, "0.4567 is not a whole cent");
        assert_eq!(scan("KXCENT-SUB", 0.004), None, "0.004 is sub-cent");
        assert_eq!(scan("KXCENT-OK", 0.45), Some(0.45), "0.45 trades at its price");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn broken_strategy_file_is_named_without_a_dangling_pointer() {
        // The app keeps no diagnostics output, so the error must not
        // point the reader at any.
        let dir = std::env::temp_dir().join(format!("ob-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.rn"), "pub fn name() {\n").unwrap();
        let (strats, errors) = load_strategies_with_errors(&dir);
        assert!(strats.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "broken.rn");
        assert!(errors[0].1.contains("failed to build"), "error: {}", errors[0].1);
        assert!(!errors[0].1.contains("diagnostics"), "error: {}", errors[0].1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_lines_are_dropped_not_counted_as_snapshots() {
        let dir = std::env::temp_dir().join(format!("ob-count-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let line = |t: &str, at: i64| {
            serde_json::to_string(&snap(t, at, "2030-01-01T01:00:00Z", "", Some(0.5), Some(0.5)))
                .unwrap()
        };
        let now = now_unix();
        let mut bytes = format!("{}\n", line("A", now)).into_bytes();
        bytes.extend_from_slice(b"{\"ticker\":\"broken\n");
        bytes.extend_from_slice(format!("{}\n", line("B", now)).as_bytes());
        std::fs::write(dir.join("snapshots.jsonl"), &bytes).unwrap();
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        let (_, body) = handle(&app, "GET", "/api/home").split_once("\r\n\r\n").map(|(h, b)| (h, b.to_string())).unwrap();
        let home: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(home["collector"]["snapshots_on_disk"], 2, "only valid snapshots are counted");
        assert_eq!(home["collector"]["dropped_lines"], 1, "the invalid line is still reported as dropped");
        assert!(
            daily_report(&app).contains(" — 2 snapshots on disk across"),
            "report must use the same valid count"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_series_are_named_on_the_home_block() {
        let dir = std::env::temp_dir().join(format!("ob-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        SERIES_FAILED_LAST_PASS.store(4, std::sync::atomic::Ordering::Relaxed);
        *SERIES_FAILED_NAMES.lock().unwrap() = vec!["KXBTC15M".to_string(), "KXSOL15M".to_string()];
        let (_, body) = handle(&app, "GET", "/api/home").split_once("\r\n\r\n").map(|(h, b)| (h, b.to_string())).unwrap();
        let home: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(home["collector"]["series_failed_names"], serde_json::json!(["KXBTC15M", "KXSOL15M"]));
        assert_eq!(home["collector"]["series_failed_last_pass"], 4);
        assert_eq!(home["collector"]["series_total"], 10);
        assert!(
            daily_report(&app).contains(
                "4 of 10 market reads failed on the last pass (5 series, open and settled): KXBTC15M, KXSOL15M."
            ),
            "report: {}",
            daily_report(&app)
        );
        SERIES_FAILED_LAST_PASS.store(0, std::sync::atomic::Ordering::Relaxed);
        SERIES_FAILED_NAMES.lock().unwrap().clear();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn allowed_hosts_are_accepted_only_when_listed() {
        let host = |h: &str| vec![("host".to_string(), h.to_string())];
        let allowed = vec!["openbell.lan".to_string()];
        // A listed name is accepted with or without a port.
        assert_eq!(refuse_request("GET", &host("openbell.lan"), 0, &allowed), None);
        assert_eq!(refuse_request("GET", &host("openbell.lan:9820"), 0, &allowed), None);
        assert_eq!(refuse_request("GET", &host("OpenBell.LAN:9820"), 0, &allowed), None);
        // Loopback names are always accepted, even with nothing listed.
        for h in ["localhost:9820", "127.0.0.1:9820", "[::1]:9820", "[::1]"] {
            assert_eq!(refuse_request("GET", &host(h), 0, &[]), None, "{h}");
        }
        // Any other Host is refused, listed or not, and a name that merely
        // contains a listed one is not a match.
        assert_eq!(refuse_request("GET", &host("evil.example:9820"), 0, &allowed).map(|r| r.0), Some(403));
        assert_eq!(refuse_request("GET", &host("openbell.lan.evil.example"), 0, &allowed).map(|r| r.0), Some(403));
        assert_eq!(refuse_request("GET", &host("evil.example"), 0, &[]).map(|r| r.0), Some(403));
        assert_eq!(refuse_request("GET", &[], 0, &allowed).map(|r| r.0), Some(403), "no Host header");
    }

    #[test]
    fn lines_on_disk_is_valid_plus_dropped() {
        let dir = std::env::temp_dir().join(format!("ob-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let line = |t: &str, at: i64| {
            serde_json::to_string(&snap(t, at, "2030-01-01T01:00:00Z", "", Some(0.5), Some(0.5)))
                .unwrap()
        };
        let now = now_unix();
        // Valid, invalid UTF-8, malformed JSON, valid: four lines on disk.
        let mut bytes = format!("{}\n", line("A", now)).into_bytes();
        let mut bad = line("Z", now).into_bytes();
        let z = bad.iter().position(|c| *c == b'Z').unwrap();
        bad[z] = 0xff;
        bytes.extend_from_slice(&bad);
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{\"ticker\": \"malformed\"\n");
        bytes.extend_from_slice(format!("{}\n", line("B", now)).as_bytes());
        std::fs::write(dir.join("snapshots.jsonl"), &bytes).unwrap();
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        let (_, body) = handle(&app, "GET", "/api/home").split_once("\r\n\r\n").map(|(h, b)| (h, b.to_string())).unwrap();
        let home: serde_json::Value = serde_json::from_str(&body).unwrap();
        let c = &home["collector"];
        assert_eq!(c["snapshots_on_disk"], 2, "valid snapshots only");
        assert_eq!(c["dropped_lines"], 2);
        assert_eq!(c["lines_on_disk"], 4, "every line in the file");
        assert_eq!(
            c["lines_on_disk"].as_u64().unwrap(),
            c["snapshots_on_disk"].as_u64().unwrap() + c["dropped_lines"].as_u64().unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn body_json(resp: &str) -> (u16, serde_json::Value) {
        let status: u16 = resp.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = resp.split_once("\r\n\r\n").unwrap().1;
        (status, serde_json::from_str(body).unwrap())
    }

    #[test]
    fn version_endpoint_names_the_running_build() {
        let (status, v) = body_json(&update_response("GET", "/api/version", "127.0.0.1:8472").unwrap());
        assert_eq!(status, 200);
        assert_eq!((v["name"].clone(), v["version"].clone()), (json!("openbell-rs"), json!(env!("CARGO_PKG_VERSION"))));
        assert_eq!(v["release_url"], json!(format!("https://github.com/cayleb-james2008/openbell/releases/tag/v{}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn update_snapshot_has_its_shape_before_anything_is_fetched() {
        let (status, u) = body_json(&update_response("GET", "/api/update", "localhost:8472").unwrap());
        assert_eq!(status, 200);
        for key in ["name", "current", "mode", "repo", "platform", "checked_at", "available", "latest", "notes", "release_url", "error", "staged", "can_apply", "dev_build"] {
            assert!(u.get(key).is_some(), "missing {key}: {u}");
        }
        assert_eq!((u["name"].clone(), u["current"].clone(), u["available"].clone()), (json!("openbell-rs"), json!(env!("CARGO_PKG_VERSION")), json!(false)));
        assert!(u["checked_at"].is_null(), "nothing is fetched until a check is asked for");
        assert_eq!(u["staged"], Value::Null);
    }

    #[test]
    fn apply_and_restart_are_refused_from_any_host_but_this_machine() {
        for path in ["/api/update/apply", "/api/update/restart"] {
            for host in ["lan-desk.local:8472", "openbell.example", ""] {
                let (status, body) = body_json(&update_response("POST", path, host).unwrap());
                assert_eq!(status, 403, "{path} via {host:?}: {body}");
            }
        }
        // A name listed in OPENBELL_ALLOWED_HOSTS may read, but never update.
        assert!(!is_loopback_host("lan-desk.local"));
        assert!(is_loopback_host("127.0.0.1:8472") && is_loopback_host("[::1]:8472") && is_loopback_host("LOCALHOST"));
    }

    #[test]
    fn restart_needs_a_staged_update_and_no_running_pass() {
        let (status, body) = body_json(&update_response("POST", "/api/update/restart", "127.0.0.1:8472").unwrap());
        assert_eq!(status, 409, "{body}");
        assert_eq!(restart_refusal(false, false), Some("no update is waiting for a restart"));
        assert!(restart_refusal(true, true).unwrap().contains("collection pass"));
        assert_eq!(restart_refusal(true, false), None);
    }

    #[test]
    fn home_reports_the_version() {
        let dir = std::env::temp_dir().join(format!("ob-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = App::new(dir.clone(), dir.clone(), 100.0, 60);
        let (_, home) = body_json(&handle(&app, "GET", "/api/home"));
        assert_eq!(home["version"], json!(env!("CARGO_PKG_VERSION")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn market_history_and_strategy_source_endpoints_return_read_only_data() {
        let dir = std::env::temp_dir().join(format!("ob-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s1 = Snapshot {
            fetched_at: 1000,
            ticker: "KXBTC15M-TEST".into(),
            event_ticker: "KXBTC15M".into(),
            series: "KXBTC15M".into(),
            close_time: "2026-11-21T23:00:00Z".into(),
            status: "open".into(),
            result: "".into(),
            yes_bid: OptPrice::from_opt(Some(0.51)),
            yes_ask: OptPrice::from_opt(Some(0.54)),
            no_bid: OptPrice::from_opt(Some(0.46)),
            no_ask: OptPrice::from_opt(Some(0.49)),
            last_price: OptPrice::from_opt(Some(0.52)),
            yes_bid_size: 120.0,
            no_bid_size: 95.0,
            floor_strike: None,
            cap_strike: None,
            spot_price: None,
        };
        let mut s2 = s1.clone();
        s2.fetched_at = 1060;
        s2.yes_ask = OptPrice::from_opt(Some(0.58));
        std::fs::write(
            dir.join("snapshots.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&s2).unwrap(),
                serde_json::to_string(&s1).unwrap()
            ),
        )
        .unwrap();
        let strat_dir = dir.join("strategies");
        seed_strategy_library(&strat_dir).unwrap();
        let app = App::new(dir.clone(), strat_dir, 100.0, 60);
        let (st, body) = body_json(&handle(&app, "GET", "/api/markets/history?ticker=KXBTC15M-TEST"));
        assert_eq!(st, 200);
        let pts = body["points"].as_array().unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0]["fetched_at"], json!(1000));
        assert_eq!(pts[1]["fetched_at"], json!(1060));

        let (st2, src) = body_json(&handle(&app, "GET", "/api/strategy/source?name=favourite-late"));
        assert_eq!(st2, 200);
        assert_eq!(src["file"], json!("favourite_late.rn"));
        assert!(src["source"].as_str().unwrap().contains("allow_entry"));

        let (st3, _) = body_json(&handle(&app, "GET", "/api/strategy/source?name=../etc/passwd"));
        assert_eq!(st3, 404);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| owned.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn data_dir_follows_the_platform_standard_when_unset() {
        assert_eq!(platform_data_dir("linux", &env_of(&[("HOME", "/home/u")])), Some(PathBuf::from("/home/u/.local/share/openbell")));
        assert_eq!(platform_data_dir("linux", &env_of(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")])), Some(PathBuf::from("/data/openbell")));
        // a relative XDG_DATA_HOME is ignored, as the XDG spec says
        assert_eq!(platform_data_dir("linux", &env_of(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "rel")])), Some(PathBuf::from("/home/u/.local/share/openbell")));
        assert_eq!(platform_data_dir("macos", &env_of(&[("HOME", "/Users/u")])), Some(PathBuf::from("/Users/u/Library/Application Support/openbell")));
        assert_eq!(platform_data_dir("windows", &env_of(&[("APPDATA", r"C:\Users\u\AppData\Roaming")])), Some(PathBuf::from(r"C:\Users\u\AppData\Roaming").join("openbell")));
        assert_eq!(platform_data_dir("linux", &env_of(&[])), None);
    }

    #[test]
    fn an_existing_repo_folder_keeps_its_tape_and_env_wins() {
        let get = env_of(&[("HOME", "/home/u")]);
        assert_eq!(default_data_dir("linux", &get, true), PathBuf::from("./openbell-data"), "a tape already in this folder is not orphaned");
        assert_eq!(default_data_dir("linux", &get, false), PathBuf::from("/home/u/.local/share/openbell"));
        assert_eq!(default_data_dir("linux", &env_of(&[]), false), PathBuf::from("./openbell-data"), "no home at all: the old default");
        assert_eq!(default_strategies_dir(Path::new("/d"), true), PathBuf::from("./strategies"));
        assert_eq!(default_strategies_dir(Path::new("/d"), false), PathBuf::from("/d/strategies"));
    }

    #[test]
    fn a_busy_default_port_falls_back_to_the_next_free_one() {
        let (_held, p) = bind_free_port(8472, |p| {
            if p < 8475 {
                Err(std::io::Error::from(std::io::ErrorKind::AddrInUse))
            } else {
                TcpListener::bind(("127.0.0.1", 0))
            }
        })
        .expect("a free port within the span");
        assert_eq!(p, 8475, "the first free port from the default");
        assert!(bind_free_port(8472, |_| Err(std::io::Error::from(std::io::ErrorKind::AddrInUse))).is_none(), "every port in the span busy");
        assert_eq!(DEFAULT_PORT_SPAN, 20, "the fallback reaches at most +20");
    }

    #[test]
    fn the_built_in_strategy_library_seeds_an_empty_folder_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("ob-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        seed_strategy_library(&dir).unwrap();
        assert_eq!(load_strategies(&dir).len(), 16, "all sixteen shapes load from the seeded folder");
        std::fs::write(dir.join("favourite_late.rn"), "pub fn name() { \"mine\" }\n").unwrap();
        seed_strategy_library(&dir).unwrap();
        assert!(std::fs::read_to_string(dir.join("favourite_late.rn")).unwrap().contains("mine"), "an existing folder is left alone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_start_banner_names_the_address_version_tape_and_updates() {
        let b = start_banner("http://127.0.0.1:8475", Path::new("/data/ob"), update::Mode::Auto, true);
        assert!(b.contains("OpenBell ") && b.contains(env!("CARGO_PKG_VERSION")));
        assert!(b.contains("http://127.0.0.1:8475") && b.contains("/data/ob"));
        assert!(b.contains("auto (installs") && b.contains("opening it in your browser") && b.contains("hard risk rails"));
    }

    #[test]
    fn top_of_book_depth_parses_live_kalshi_fields_with_zero_fallback() {
        let live_kalshi = json!({
            "yes_bid_size_fp": "1304.28",
            "yes_ask_size_fp": "42.50"
        });
        assert!((depth_size(&live_kalshi, &["yes_bid_size_fp", "yes_bid_size"]) - 1304.28).abs() < 1e-9);
        assert!((depth_size(&live_kalshi, &["no_bid_size_fp", "no_bid_size", "yes_ask_size_fp"]) - 42.50).abs() < 1e-9);

        let explicit_no = json!({ "yes_bid_size": 250, "no_bid_size": 180.5 });
        assert!((depth_size(&explicit_no, &["yes_bid_size_fp", "yes_bid_size"]) - 250.0).abs() < 1e-9);
        assert!((depth_size(&explicit_no, &["no_bid_size_fp", "no_bid_size", "yes_ask_size_fp"]) - 180.5).abs() < 1e-9);

        let missing_or_bad = json!({ "yes_bid_size_fp": "-10", "no_bid_size_fp": "NaN" });
        assert_eq!(depth_size(&missing_or_bad, &["yes_bid_size_fp", "yes_bid_size"]), 0.0);
        assert_eq!(depth_size(&missing_or_bad, &["no_bid_size_fp", "no_bid_size", "yes_ask_size_fp"]), 0.0);

        // Legacy snapshots without depth fields deserialize cleanly with 0.0 fallback.
        let legacy = r#"{"fetched_at":1000,"ticker":"KXBTC15M-1","event_ticker":"E","series":"KXBTC15M","close_time":"2026-11-22T08:00:00Z","status":"open","result":"","yes_bid":0.5,"yes_ask":0.52,"no_bid":0.48,"no_ask":0.5,"last_price":0.51}"#;
        let parsed: Snapshot = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.yes_bid_size, 0.0);
        assert_eq!(parsed.no_bid_size, 0.0);

        let mut s1 = parsed.clone();
        s1.yes_bid_size = 100.0;
        s1.no_bid_size = 300.0;
        assert_eq!(median_observed_book_depth(&[parsed, s1]), Some(200.0));
    }

    #[test]
    fn auto_collect_cli_and_env_parsing() {
        let no_env = |_k: &str| None;
        assert_eq!(parse_auto_collect_config(&[], &no_env, 60), (false, 60));
        assert_eq!(
            parse_auto_collect_config(&["--auto-collect".into()], &no_env, 60),
            (true, 60)
        );
        assert_eq!(
            parse_auto_collect_config(
                &["--auto-collect".into(), "--collect-interval".into(), "180".into()],
                &no_env,
                60
            ),
            (true, 180)
        );
        assert_eq!(
            parse_auto_collect_config(
                &["--auto-collect".into(), "--collect-interval=5".into()],
                &no_env,
                60
            ),
            (true, 30)
        );
        assert_eq!(
            parse_auto_collect_config(
                &["--auto-collect".into(), "--collect-interval=9999".into()],
                &no_env,
                60
            ),
            (true, 900)
        );
        let yes_env = |k: &str| (k == "OPENBELL_AUTO_COLLECT").then(|| "1".to_string());
        assert_eq!(parse_auto_collect_config(&[], &yes_env, 3600), (true, 60));
        assert_eq!(
            parse_auto_collect_config(&["--no-auto-collect".into()], &yes_env, 120),
            (false, 120)
        );
    }

    #[test]
    fn adaptive_backoff_doubles_on_429_and_resets_on_clean_pass() {
        let sched = CollectorScheduler::new(true, 60);
        assert_eq!(sched.snapshot().effective_interval_secs(), 60);

        let st1 = sched.record_pass_result(&Err(("too_many_requests".into(), true)));
        assert!(st1.rate_limited);
        assert_eq!(st1.backoff_secs, 60);
        assert_eq!(st1.effective_interval_secs(), 120);

        let st2 = sched.record_pass_result(&Err(("too_many_requests".into(), true)));
        assert_eq!(st2.effective_interval_secs(), 240);

        let st3 = sched.record_pass_result(&Err(("too_many_requests".into(), true)));
        assert_eq!(st3.effective_interval_secs(), 480);

        let st4 = sched.record_pass_result(&Err(("too_many_requests".into(), true)));
        assert_eq!(st4.effective_interval_secs(), 900);

        let st_ok = sched.record_pass_result(&Ok(CollectPassReport {
            written: 5,
            deduplicated: 100,
            errors: vec![],
            rate_limited: false,
        }));
        assert!(!st_ok.rate_limited);
        assert_eq!(st_ok.backoff_secs, 0);
        assert_eq!(st_ok.consecutive_errors, 0);
        assert_eq!(st_ok.effective_interval_secs(), 60);
        assert_eq!(st_ok.last_written, 5);
        assert_eq!(st_ok.last_deduped, 100);
        assert_eq!(st_ok.runs_total, 5);
    }

    #[test]
    fn collector_schedule_endpoint_validates_and_clamps() {
        let dir = std::env::temp_dir().join(format!("ob-sched-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let app = App::new(dir.clone(), PathBuf::from("strategies"), 100.0, 60);

        let st0 = handle_with_body(&app, "GET", "/api/collector/status", "");
        assert!(st0.starts_with("HTTP/1.1 200"));
        let j0: serde_json::Value = serde_json::from_str(st0.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(j0["enabled"], false);
        assert_eq!(j0["interval_secs"], 60);
        assert_eq!(j0["readiness_target_windows"], 200);
        assert_eq!(j0["mode"], "public_readonly_get");

        // Enable with below-floor interval -> clamped to 30s
        let r1 = handle_with_body(
            &app,
            "POST",
            "/api/collector/schedule",
            r#"{"enabled":true,"interval_secs":5}"#,
        );
        assert!(r1.starts_with("HTTP/1.1 200"));
        let j1: serde_json::Value = serde_json::from_str(r1.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(j1["enabled"], true);
        assert_eq!(j1["interval_secs"], 30);
        assert!(j1["next_run_epoch"].as_i64().is_some());

        // Above-ceiling interval -> clamped to 900s
        let r2 = handle_with_body(
            &app,
            "POST",
            "/api/collector/schedule",
            r#"{"interval_secs":5000}"#,
        );
        assert!(r2.starts_with("HTTP/1.1 200"));
        let j2: serde_json::Value = serde_json::from_str(r2.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(j2["enabled"], true);
        assert_eq!(j2["interval_secs"], 900);

        // Disable cleanly
        let r3 = handle_with_body(&app, "POST", "/api/collector/schedule", r#"{"enabled":false}"#);
        assert!(r3.starts_with("HTTP/1.1 200"));
        let j3: serde_json::Value = serde_json::from_str(r3.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(j3["enabled"], false);
        assert!(j3["next_run_epoch"].is_null());

        // Malformed / invalid bodies -> 400
        for bad in ["not-json", "[]", "{}", r#"{"enabled":"yes"}"#, r#"{"interval_secs":"fast"}"#] {
            let rb = handle_with_body(&app, "POST", "/api/collector/schedule", bad);
            assert!(rb.starts_with("HTTP/1.1 400"), "expected 400 for {bad}, got {rb}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_endpoints_enforce_loopback_and_csrf_header() {
        let no_hdrs: Vec<(String, String)> = vec![];
        let with_csrf = vec![("x-openbell-csrf".to_string(), "1".to_string())];

        // Non-loopback host refused for BOTH GET and POST even with CSRF header.
        assert_eq!(refuse_live_request("GET", "desk.lan:8472", &no_hdrs).map(|x| x.0), Some(403));
        assert_eq!(refuse_live_request("POST", "192.168.1.10:8472", &with_csrf).map(|x| x.0), Some(403));

        // Loopback GET allowed without CSRF.
        assert!(refuse_live_request("GET", "127.0.0.1:8472", &no_hdrs).is_none());
        assert!(refuse_live_request("GET", "localhost:8472", &no_hdrs).is_none());

        // Loopback mutations refused without CSRF, allowed with CSRF.
        assert_eq!(refuse_live_request("POST", "127.0.0.1:8472", &no_hdrs).map(|x| x.0), Some(403));
        assert_eq!(refuse_live_request("DELETE", "localhost:8472", &no_hdrs).map(|x| x.0), Some(403));
        assert!(refuse_live_request("POST", "127.0.0.1:8472", &with_csrf).is_none());
        assert!(refuse_live_request("DELETE", "127.0.0.1:8472", &with_csrf).is_none());
    }

    #[test]
    fn embedded_chunks_serve_gzip_etag_304_immutable_and_block_woff2_recompress() {
        use sha2::{Digest, Sha256};
        // Verify pure-Rust inflator reproduces assets/index.html byte-for-byte.
        let disk_html = include_bytes!("../assets/index.html");
        assert_eq!(index_html_str().as_bytes(), disk_html);

        let js_chunk = embedded_chunks::CHUNKS
            .iter()
            .find(|c| c.name.ends_with(".js") && c.is_gzip)
            .expect("at least one embedded gzip js chunk");
        let mut buf = Vec::new();
        let gzip_hdrs = vec![("accept-encoding".to_string(), "gzip, deflate".to_string())];
        write_embedded_chunk(&mut buf, "GET", &gzip_hdrs, js_chunk).unwrap();
        let split_pos = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let hdr = std::str::from_utf8(&buf[..split_pos]).unwrap();
        assert!(hdr.starts_with("HTTP/1.1 200 OK"));
        assert!(hdr.contains("Content-Encoding: gzip"));
        assert!(hdr.contains("Cache-Control: public, max-age=31536000, immutable"));
        assert!(hdr.contains(&format!("ETag: {}", js_chunk.etag)));
        assert_eq!(&buf[split_pos + 4..], js_chunk.bytes);

        // Verify non-gzip identity clients receive exact SHA-256 inflated JS.
        let mut id_buf = Vec::new();
        write_embedded_chunk(&mut id_buf, "GET", &[], js_chunk).unwrap();
        let id_split = id_buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let id_hdr = std::str::from_utf8(&id_buf[..id_split]).unwrap();
        assert!(!id_hdr.contains("Content-Encoding: gzip"));
        let digest = Sha256::digest(&id_buf[id_split + 4..]);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(format!("\"{hex}\""), js_chunk.etag);

        let mut buf_304 = Vec::new();
        let inm_hdrs = vec![("if-none-match".to_string(), js_chunk.etag.to_string())];
        write_embedded_chunk(&mut buf_304, "GET", &inm_hdrs, js_chunk).unwrap();
        let resp_304 = std::str::from_utf8(&buf_304).unwrap();
        assert!(resp_304.starts_with("HTTP/1.1 304 Not Modified"));
        assert!(resp_304.contains("Cache-Control: public, max-age=31536000, immutable"));

        if let Some(font) = embedded_chunks::CHUNKS.iter().find(|c| c.name.ends_with(".woff2")) {
            let mut font_buf = Vec::new();
            write_embedded_chunk(&mut font_buf, "GET", &gzip_hdrs, font).unwrap();
            let f_split = font_buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let f_hdr = std::str::from_utf8(&font_buf[..f_split]).unwrap();
            assert!(!f_hdr.contains("Content-Encoding: gzip"));
            assert!(f_hdr.contains("Content-Type: font/woff2"));
            assert_eq!(&font_buf[f_split + 4..], font.bytes);
        }
    }
}
