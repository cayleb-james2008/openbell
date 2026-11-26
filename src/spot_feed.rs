//! Real-Time External Crypto Spot Price & Strike Oracle (Cycle 4 Wave E2).
//!
//! Maintains a thread-safe store of external spot ticks for `BTC`, `ETH`, `SOL`,
//! `XRP`, and `DOGE`, computes rolling 30-second delta (bps) and realized
//! volatility (bps), persists ticks to `<OPENBELL_DATA>/spot_ticks.jsonl` with
//! 8 MiB rotation and CWE-59 non-symlink confinement, and emits throttled SSE
//! `event: spot` frames.
//!
//! Permanent safety invariants:
//! - Connects ONLY to public unauthenticated market-data feeds (never places
//!   trades on external venues and never attaches Kalshi auth credentials).
//! - Defaults to OFFLINE (`OPENBELL_SPOT_OFFLINE=1` or unset without
//!   `OPENBELL_SPOT_URL`), producing zero outbound network traffic in fixture
//!   and default test runs.

#![allow(dead_code)]

use crate::{kalshi_live, orderbook, tape_hub, App};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SPOT_ASSETS: [&str; 5] = ["BTC", "ETH", "SOL", "XRP", "DOGE"];
pub const DEFAULT_STALE_AFTER_MS: i64 = 15_000;
pub const MAX_SPOT_TICKS_JSONL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HISTORY_LIMIT: usize = 500;
const MAX_HISTORY_LINE_BYTES: usize = 64 * 1024;
const MAX_RING_TICKS_PER_ASSET: usize = 1024;
const MAX_TICK_WINDOW_MS: i64 = 300_000;
const DELTA_WINDOW_MS: i64 = 30_000;
const VOL_WINDOW_MS: i64 = 60_000;
const SSE_THROTTLE_MS: i64 = 250;
const RUNNER_NOTIFY_THROTTLE_MS: i64 = 250;
const REST_POLL_INTERVAL_MS: u64 = 2_000;
const WS_RECONNECT_INITIAL_MS: u64 = 1_000;
const WS_RECONNECT_MAX_MS: u64 = 30_000;
const MAX_WS_FRAME_BYTES: usize = 1024 * 1024;
const MAX_WS_CONTINUATION_FRAMES: usize = 64;
const COINBASE_PUBLIC_WS: &str = "wss://ws-feed.exchange.coinbase.com";
const COINBASE_PUBLIC_REST_BASE: &str = "https://api.exchange.coinbase.com";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[inline]
fn finite_opt(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite())
}

#[inline]
fn finite_pos_opt(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x > 0.0)
}

/// Normalize a spot symbol, product ID, or Kalshi series name into one of the
/// five canonical assets (`BTC`, `ETH`, `SOL`, `XRP`, `DOGE`).
pub fn normalize_spot_asset(raw: &str) -> Option<&'static str> {
    let s = raw.trim().to_ascii_uppercase();
    if s.is_empty() {
        return None;
    }
    if s == "BTC"
        || s.starts_with("BTC-")
        || s.starts_with("BTC/")
        || s.starts_with("BTC_")
        || s == "BTCUSD"
        || s == "BTCUSDT"
        || s.starts_with("KXBTC")
    {
        return Some("BTC");
    }
    if s == "ETH"
        || s.starts_with("ETH-")
        || s.starts_with("ETH/")
        || s.starts_with("ETH_")
        || s == "ETHUSD"
        || s == "ETHUSDT"
        || s.starts_with("KXETH")
    {
        return Some("ETH");
    }
    if s == "SOL"
        || s.starts_with("SOL-")
        || s.starts_with("SOL/")
        || s.starts_with("SOL_")
        || s == "SOLUSD"
        || s == "SOLUSDT"
        || s.starts_with("KXSOL")
    {
        return Some("SOL");
    }
    if s == "XRP"
        || s.starts_with("XRP-")
        || s.starts_with("XRP/")
        || s.starts_with("XRP_")
        || s == "XRPUSD"
        || s == "XRPUSDT"
        || s.starts_with("KXXRP")
    {
        return Some("XRP");
    }
    if s == "DOGE"
        || s.starts_with("DOGE-")
        || s.starts_with("DOGE/")
        || s.starts_with("DOGE_")
        || s == "DOGEUSD"
        || s == "DOGEUSDT"
        || s.starts_with("KXDOGE")
    {
        return Some("DOGE");
    }
    None
}

/// Strict query parameter validator for `/api/spot/history?asset=...`.
pub fn validate_spot_asset_param(raw: &str) -> Result<&'static str, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 16 {
        return Err("invalid spot asset; expected BTC, ETH, SOL, XRP, or DOGE".to_string());
    }
    for &canonical in &SPOT_ASSETS {
        if trimmed.eq_ignore_ascii_case(canonical) {
            return Ok(canonical);
        }
    }
    Err("invalid spot asset; expected BTC, ETH, SOL, XRP, or DOGE".to_string())
}

/// Returns `true` when outbound spot feed networking is disabled.
///
/// Defaults to offline (`true`) unless `OPENBELL_SPOT_OFFLINE=0`/`false`, or an
/// explicit `OPENBELL_SPOT_URL` is configured without `OPENBELL_SPOT_OFFLINE=1`.
pub fn spot_feed_offline() -> bool {
    if let Ok(v) = std::env::var("OPENBELL_SPOT_OFFLINE") {
        let t = v.trim();
        if t == "1" || t.eq_ignore_ascii_case("true") {
            return true;
        }
        if t == "0" || t.eq_ignore_ascii_case("false") {
            return false;
        }
    }
    std::env::var("OPENBELL_SPOT_URL")
        .ok()
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpotAssetStatus {
    pub asset: String,
    pub price: Option<f64>,
    pub delta_30s_bps: Option<f64>,
    pub vol_bps: Option<f64>,
    pub updated_ms: Option<i64>,
    pub source: String,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpotStatusResponse {
    pub assets: Vec<SpotAssetStatus>,
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpotTickRecord {
    pub ts_ms: i64,
    pub price: f64,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpotHistoryResponse {
    pub ok: bool,
    pub asset: String,
    pub ticks: Vec<SpotTickRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpotDiskLine {
    ts_ms: i64,
    asset: String,
    price: f64,
    source: String,
}

#[derive(Debug, Clone, Default)]
struct AssetState {
    ticks: VecDeque<(i64, f64)>,
    last_price: Option<f64>,
    updated_ms: Option<i64>,
    source: String,
    last_sse_emit_ms: i64,
}

impl AssetState {
    fn push_sample(&mut self, ts_ms: i64, price: f64, source: &str) {
        if let Some(&(last_ts, _)) = self.ticks.back() {
            if ts_ms >= last_ts {
                self.ticks.push_back((ts_ms, price));
            } else {
                // Keep chronological order for rolling delta and vol calculations.
                let pos = self
                    .ticks
                    .iter()
                    .position(|&(t, _)| t > ts_ms)
                    .unwrap_or(self.ticks.len());
                self.ticks.insert(pos, (ts_ms, price));
            }
        } else {
            self.ticks.push_back((ts_ms, price));
        }

        let cutoff = ts_ms.saturating_sub(MAX_TICK_WINDOW_MS);
        while self.ticks.len() > 2 && self.ticks.front().is_some_and(|&(t, _)| t < cutoff) {
            self.ticks.pop_front();
        }
        while self.ticks.len() > MAX_RING_TICKS_PER_ASSET {
            self.ticks.pop_front();
        }

        if self.updated_ms.is_none_or(|prev| ts_ms >= prev) {
            self.last_price = Some(price);
            self.updated_ms = Some(ts_ms);
            self.source = source.to_string();
        }
    }

    fn compute_delta_30s_bps(&self) -> Option<f64> {
        let cur = finite_pos_opt(self.last_price)?;
        let cur_ts = self.updated_ms?;
        if self.ticks.len() < 2 {
            return Some(0.0);
        }
        let target_ts = cur_ts.saturating_sub(DELTA_WINDOW_MS);
        let mut ref_price = self.ticks.front().map(|x| x.1).unwrap_or(cur);
        for &(ts, px) in &self.ticks {
            if ts <= target_ts {
                ref_price = px;
            } else {
                break;
            }
        }
        if !ref_price.is_finite() || ref_price <= 0.0 {
            return None;
        }
        let bps = ((cur - ref_price) / ref_price) * 10_000.0;
        if bps.is_finite() {
            Some((bps * 100.0).round() / 100.0)
        } else {
            None
        }
    }

    fn compute_vol_bps(&self) -> Option<f64> {
        let _cur = finite_pos_opt(self.last_price)?;
        let cur_ts = self.updated_ms?;
        if self.ticks.len() < 2 {
            return Some(0.0);
        }
        let cutoff = cur_ts.saturating_sub(VOL_WINDOW_MS);
        let mut prev_px: Option<f64> = None;
        let mut returns_bps: Vec<f64> = Vec::new();
        for &(ts, px) in &self.ticks {
            if ts < cutoff && self.ticks.len() > 2 {
                prev_px = Some(px);
                continue;
            }
            if let Some(p0) = prev_px {
                if p0.is_finite() && p0 > 0.0 && px.is_finite() && px > 0.0 {
                    let r = ((px - p0) / p0) * 10_000.0;
                    if r.is_finite() {
                        returns_bps.push(r);
                    }
                }
            }
            prev_px = Some(px);
        }
        if returns_bps.is_empty() {
            return Some(0.0);
        }
        let n = returns_bps.len() as f64;
        let mean = returns_bps.iter().sum::<f64>() / n;
        let var = returns_bps
            .iter()
            .map(|r| {
                let d = *r - mean;
                d * d
            })
            .sum::<f64>()
            / n;
        let sd = if returns_bps.len() >= 2 {
            var.max(0.0).sqrt()
        } else {
            // Single return sample: report absolute move magnitude as initial dispersion.
            returns_bps[0].abs()
        };
        if sd.is_finite() {
            Some((sd * 100.0).round() / 100.0)
        } else {
            None
        }
    }

    fn to_status(&self, asset: &str, now: i64, stale_after_ms: i64, offline: bool) -> SpotAssetStatus {
        let price = finite_pos_opt(self.last_price);
        let stale = match self.updated_ms {
            Some(u) if price.is_some() => now.saturating_sub(u) > stale_after_ms,
            _ => true,
        };
        let source = if !self.source.is_empty() {
            self.source.clone()
        } else if offline {
            "offline".to_string()
        } else {
            "pending".to_string()
        };
        SpotAssetStatus {
            asset: asset.to_string(),
            price,
            delta_30s_bps: if price.is_some() {
                finite_opt(self.compute_delta_30s_bps())
            } else {
                None
            },
            vol_bps: if price.is_some() {
                finite_opt(self.compute_vol_bps())
            } else {
                None
            },
            updated_ms: self.updated_ms,
            source,
            stale,
        }
    }
}

struct SpotFeedInner {
    assets: HashMap<&'static str, AssetState>,
    stale_after_ms: i64,
    last_error: Option<String>,
    last_runner_notify_ms: i64,
    runner_notify: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for SpotFeedInner {
    fn default() -> Self {
        let mut assets = HashMap::with_capacity(SPOT_ASSETS.len());
        for &a in &SPOT_ASSETS {
            assets.insert(a, AssetState::default());
        }
        Self {
            assets,
            stale_after_ms: DEFAULT_STALE_AFTER_MS,
            last_error: None,
            last_runner_notify_ms: 0,
            runner_notify: None,
        }
    }
}

pub struct SpotFeedStore {
    data_dir: PathBuf,
    inner: Mutex<SpotFeedInner>,
    disk_lock: Mutex<()>,
}

impl SpotFeedStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            inner: Mutex::new(SpotFeedInner::default()),
            disk_lock: Mutex::new(()),
        }
    }

    pub fn set_stale_after_ms(&self, ms: i64) {
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.stale_after_ms = ms.max(1_000);
    }

    pub fn set_runner_notify<F: Fn() + Send + Sync + 'static>(&self, cb: F) {
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.runner_notify = Some(Arc::new(cb));
    }

    pub fn is_offline(&self) -> bool {
        spot_feed_offline()
    }

    fn set_last_error(&self, err: Option<String>) {
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.last_error = err;
    }

    /// Ingest a single external spot tick (`asset`, positive finite `price`, `ts_ms`, `source`).
    /// Persists to `<data_dir>/spot_ticks.jsonl` (8 MiB rotation, CWE-59 symlink check) and
    /// emits a throttled SSE `event: spot` payload when appropriate.
    pub fn ingest_tick(&self, asset: &str, price: f64, ts_ms: i64, source: &str) -> bool {
        let Some(canonical) = normalize_spot_asset(asset) else {
            return false;
        };
        if !price.is_finite() || price <= 0.0 {
            return false;
        }
        let now = now_ms();
        let eff_ts = if ts_ms > 0 { ts_ms } else { now };
        let clean_source: String = source
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            .take(24)
            .collect();
        let src = if clean_source.is_empty() {
            "manual".to_string()
        } else {
            clean_source
        };
        let offline = self.is_offline();

        let (sse_payload, notify_cb) = {
            let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let stale_after = st.stale_after_ms;
            let entry = st.assets.entry(canonical).or_default();
            entry.push_sample(eff_ts, price, &src);
            let sse = if now.saturating_sub(entry.last_sse_emit_ms) >= SSE_THROTTLE_MS {
                entry.last_sse_emit_ms = now;
                Some(entry.to_status(canonical, now, stale_after, offline))
            } else {
                None
            };
            let cb = if now.saturating_sub(st.last_runner_notify_ms) >= RUNNER_NOTIFY_THROTTLE_MS {
                st.last_runner_notify_ms = now;
                st.runner_notify.clone()
            } else {
                None
            };
            (sse, cb)
        };

        let _ = self.append_tick_record(canonical, price, eff_ts, &src);

        if let Some(status_payload) = sse_payload {
            if let Ok(json) = serde_json::to_string(&status_payload) {
                tape_hub().publish_event("spot", json);
            }
        }
        if let Some(cb) = notify_cb {
            cb();
        }
        true
    }

    fn append_tick_record(
        &self,
        asset: &str,
        price: f64,
        ts_ms: i64,
        source: &str,
    ) -> Result<(), String> {
        if self.data_dir.as_os_str().is_empty() {
            return Ok(());
        }
        let _disk_guard = self.disk_lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = std::fs::create_dir_all(&self.data_dir) {
            return Err(format!("create data_dir failed: {e}"));
        }
        let dir_meta = std::fs::symlink_metadata(&self.data_dir)
            .map_err(|e| format!("cannot stat data_dir: {e}"))?;
        if dir_meta.file_type().is_symlink() || !dir_meta.file_type().is_dir() {
            return Err("data_dir must be a regular directory".to_string());
        }

        let path = self.data_dir.join("spot_ticks.jsonl");
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                return Err("spot_ticks.jsonl must be a confined regular file".to_string());
            }
            if !crate::is_confined_regular_file(&self.data_dir, &path) {
                return Err("spot_ticks.jsonl failed confinement check".to_string());
            }
            if meta.len() >= MAX_SPOT_TICKS_JSONL_BYTES {
                let backup = self.data_dir.join("spot_ticks.jsonl.1");
                let backup_is_symlink = std::fs::symlink_metadata(&backup)
                    .is_ok_and(|m| m.file_type().is_symlink());
                if backup_is_symlink {
                    return Err("spot_ticks.jsonl.1 must not be a symlink".to_string());
                }
                let _ = std::fs::rename(&path, &backup);
            }
        }

        let line = serde_json::to_string(&SpotDiskLine {
            ts_ms,
            asset: asset.to_string(),
            price,
            source: source.to_string(),
        })
        .map_err(|e| format!("serialize spot tick failed: {e}"))?;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("open spot_ticks.jsonl failed: {e}"))?;
        writeln!(file, "{line}").map_err(|e| format!("write spot_ticks.jsonl failed: {e}"))?;
        Ok(())
    }

    pub fn status_snapshot(&self) -> SpotStatusResponse {
        let now = now_ms();
        let offline = self.is_offline();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let stale_after = st.stale_after_ms;
        let assets = SPOT_ASSETS
            .iter()
            .map(|&asset| match st.assets.get(asset) {
                Some(entry) => entry.to_status(asset, now, stale_after, offline),
                None => AssetState::default().to_status(asset, now, stale_after, offline),
            })
            .collect();
        SpotStatusResponse { assets, offline }
    }

    /// Return the latest non-stale spot status for a Kalshi series or asset name
    /// (e.g. `"KXBTC15M"` or `"BTC"`). Returns `None` if missing, offline/empty, or stale.
    pub fn latest_fresh_for_series(&self, series_or_asset: &str) -> Option<SpotAssetStatus> {
        let canonical = normalize_spot_asset(series_or_asset)?;
        let now = now_ms();
        let offline = self.is_offline();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let stale_after = st.stale_after_ms;
        let entry = st.assets.get(canonical)?;
        let status = entry.to_status(canonical, now, stale_after, offline);
        if status.price.is_some() && !status.stale {
            Some(status)
        } else {
            None
        }
    }

    /// Read up to `limit` (clamped `1..=500`) persisted spot ticks for `asset` from
    /// `<data_dir>/spot_ticks.jsonl` with strict CWE-59 symlink/confinement checks and
    /// corrupt-line tolerance.
    pub fn read_history(
        &self,
        asset: &str,
        limit: usize,
    ) -> Result<Vec<SpotTickRecord>, (u16, String)> {
        let canonical = validate_spot_asset_param(asset).map_err(|e| (400, e))?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let eff_limit = limit.clamp(1, MAX_HISTORY_LIMIT);

        if self.data_dir.as_os_str().is_empty() {
            let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let Some(entry) = st.assets.get(canonical) else {
                return Ok(Vec::new());
            };
            let start = entry.ticks.len().saturating_sub(eff_limit);
            return Ok(entry
                .ticks
                .iter()
                .skip(start)
                .filter(|(_, p)| p.is_finite() && *p > 0.0)
                .map(|&(ts_ms, price)| SpotTickRecord {
                    ts_ms,
                    price,
                    source: entry.source.clone(),
                })
                .collect());
        }

        let dir_meta = match std::fs::symlink_metadata(&self.data_dir) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err((500, format!("cannot stat data directory: {e}"))),
        };
        if dir_meta.file_type().is_symlink() || !dir_meta.file_type().is_dir() {
            return Err((403, "data_dir must be a regular directory".to_string()));
        }

        let path = self.data_dir.join("spot_ticks.jsonl");
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err((500, format!("cannot stat spot_ticks.jsonl: {e}"))),
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err((
                403,
                "spot_ticks.jsonl must be a confined regular file".to_string(),
            ));
        }
        if !crate::is_confined_regular_file(&self.data_dir, &path) {
            return Err((
                403,
                "spot_ticks.jsonl failed directory confinement check".to_string(),
            ));
        }

        let _disk_guard = self.disk_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut file = std::fs::File::open(&path)
            .map_err(|e| (500, format!("open spot_ticks.jsonl failed: {e}")))?;
        let mut skip_first_partial = false;
        if meta.len() > MAX_SPOT_TICKS_JSONL_BYTES {
            let start = meta.len().saturating_sub(MAX_SPOT_TICKS_JSONL_BYTES);
            file.seek(SeekFrom::Start(start))
                .map_err(|e| (500, format!("seek spot_ticks.jsonl failed: {e}")))?;
            skip_first_partial = true;
        }

        let mut reader = BufReader::new(file.take(MAX_SPOT_TICKS_JSONL_BYTES));
        let mut raw_line = Vec::with_capacity(256);
        let mut out: VecDeque<SpotTickRecord> = VecDeque::with_capacity(eff_limit.min(128));
        const MAX_SCAN_LINES: usize = 250_000;
        let mut scanned = 0usize;

        loop {
            if scanned >= MAX_SCAN_LINES {
                break;
            }
            raw_line.clear();
            let n = match reader.read_until(b'\n', &mut raw_line) {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            if skip_first_partial {
                skip_first_partial = false;
                continue;
            }
            scanned += 1;
            if raw_line.len() > MAX_HISTORY_LINE_BYTES {
                continue;
            }
            let Ok(line_str) = std::str::from_utf8(&raw_line) else {
                continue;
            };
            let trimmed = line_str.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(raw) = serde_json::from_str::<SpotDiskLine>(trimmed) else {
                continue;
            };
            if normalize_spot_asset(&raw.asset) != Some(canonical) {
                continue;
            }
            if !raw.price.is_finite() || raw.price <= 0.0 {
                continue;
            }
            if out.len() >= eff_limit {
                out.pop_front();
            }
            out.push_back(SpotTickRecord {
                ts_ms: raw.ts_ms,
                price: raw.price,
                source: raw.source,
            });
        }
        Ok(out.into_iter().collect())
    }

    fn ingest_json_value(
        &self,
        val: &serde_json::Value,
        default_asset: Option<&str>,
        default_source: &str,
    ) -> usize {
        let mut count = 0usize;
        let now = now_ms();

        if let Some(arr) = val.as_array() {
            for item in arr {
                count += self.ingest_json_value(item, default_asset, default_source);
            }
            return count;
        }
        let Some(obj) = val.as_object() else {
            return 0;
        };

        // Combined Binance stream envelope: {"stream": "...", "data": {...}}
        if let Some(inner) = obj.get("data") {
            if inner.is_object() || inner.is_array() {
                count += self.ingest_json_value(inner, default_asset, default_source);
                if count > 0 {
                    return count;
                }
            }
        }

        // Batch arrays: {"ticks": [...]} or {"assets": [...]}
        for key in ["ticks", "assets"] {
            if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
                for item in arr {
                    count += self.ingest_json_value(item, default_asset, default_source);
                }
                return count;
            }
        }

        // Price map: {"prices": {"BTC": 95000.0, "ETH": 3200.0}}
        if let Some(prices_obj) = obj.get("prices").and_then(|v| v.as_object()) {
            let ts = extract_ts_ms(val).unwrap_or(now);
            let src = obj
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or(default_source);
            for (k, v) in prices_obj {
                if let (Some(asset), Some(px)) = (normalize_spot_asset(k), parse_pos_f64(v)) {
                    if self.ingest_tick(asset, px, ts, src) {
                        count += 1;
                    }
                }
            }
            return count;
        }

        // Single tick object: Coinbase ticker / generic mock / Binance trade.
        let asset_opt = ["asset", "product_id", "symbol", "s", "ticker"]
            .iter()
            .find_map(|&k| obj.get(k).and_then(|v| v.as_str()))
            .and_then(normalize_spot_asset)
            .or_else(|| default_asset.and_then(normalize_spot_asset));
        let price_opt = ["price", "p", "last", "last_price", "spot_price"]
            .iter()
            .find_map(|&k| obj.get(k).and_then(parse_pos_f64));
        if let (Some(asset), Some(px)) = (asset_opt, price_opt) {
            let ts = extract_ts_ms(val).unwrap_or(now);
            let src = obj
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or(default_source);
            if self.ingest_tick(asset, px, ts, src) {
                count += 1;
            }
            return count;
        }

        // Flat asset-keyed map: {"BTC": 95000.0, "ETH": 3200.0}
        let ts = extract_ts_ms(val).unwrap_or(now);
        let src = obj
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or(default_source);
        for (k, v) in obj {
            if let (Some(asset), Some(px)) = (normalize_spot_asset(k), parse_pos_f64(v)) {
                if self.ingest_tick(asset, px, ts, src) {
                    count += 1;
                }
            }
        }
        count
    }

    pub fn ingest_ws_message(
        &self,
        text: &str,
        source_override: Option<&str>,
    ) -> Result<usize, String> {
        let val: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("invalid spot WS JSON: {e}"))?;
        if let Some(err) = val
            .get("type")
            .and_then(|v| v.as_str())
            .filter(|&t| t == "error")
        {
            let _ = err;
            let msg = val
                .get("message")
                .or_else(|| val.get("reason"))
                .and_then(|v| v.as_str())
                .unwrap_or("spot WS error");
            return Err(msg.to_string());
        }
        let src = source_override.unwrap_or("ws");
        Ok(self.ingest_json_value(&val, None, src))
    }

    pub fn ingest_rest_payload(
        &self,
        text: &str,
        default_asset: Option<&str>,
        source_override: Option<&str>,
    ) -> Result<usize, String> {
        let val: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("invalid spot REST JSON: {e}"))?;
        let src = source_override.unwrap_or("rest");
        Ok(self.ingest_json_value(&val, default_asset, src))
    }
}

fn parse_pos_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        .filter(|x| x.is_finite() && *x > 0.0)
}

fn extract_ts_ms(val: &serde_json::Value) -> Option<i64> {
    if let Some(ms) = val.get("ts_ms").or_else(|| val.get("updated_ms")).and_then(|v| v.as_i64()) {
        if ms > 0 {
            return Some(ms);
        }
    }
    if let Some(ms) = val.get("E").or_else(|| val.get("T")).and_then(|v| v.as_i64()) {
        if ms > 0 {
            return Some(ms);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Public Unauthenticated WS/REST Client & Background Feed Worker
// ---------------------------------------------------------------------------

enum SpotTarget {
    Ws { ws_url: String },
    Rest { rest_url: String },
    DefaultCoinbase,
}

fn validate_spot_ws_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("spot WS URL must not contain control or whitespace characters".to_string());
    }
    if trimmed.starts_with("wss://") {
        let _ = parse_ws_url(trimmed)?;
        return Ok(trimmed.to_string());
    }
    if trimmed.starts_with("ws://") {
        let (_, host, _, _) = parse_ws_url(trimmed)?;
        let h = host.to_ascii_lowercase();
        if matches!(h.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
            return Ok(trimmed.to_string());
        }
        return Err("plain ws:// spot URL is permitted only on loopback".to_string());
    }
    Err("spot WS URL must start with wss:// or loopback ws://".to_string())
}

fn resolve_spot_target() -> Result<SpotTarget, String> {
    if let Ok(raw) = std::env::var("OPENBELL_SPOT_URL") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
                return validate_spot_ws_url(trimmed).map(|ws_url| SpotTarget::Ws { ws_url });
            }
            return kalshi_live::validate_api_base_url(trimmed)
                .map(|rest_url| SpotTarget::Rest { rest_url });
        }
    }
    Ok(SpotTarget::DefaultCoinbase)
}

fn parse_ws_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let trimmed = url.trim();
    let (tls, rest) = if let Some(r) = trimmed.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err("WS URL must start with ws:// or wss://".to_string());
    };
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = if let Some((h, p_str)) = authority.rsplit_once(':') {
        let port = p_str
            .parse::<u16>()
            .map_err(|_| "invalid WS port".to_string())?;
        (h.to_string(), port)
    } else {
        (authority.to_string(), if tls { 443 } else { 80 })
    };
    if host.is_empty() {
        return Err("WS URL missing host".to_string());
    }
    Ok((tls, host, port, path))
}

enum WsTransport {
    Plain(TcpStream),
    TlsProc {
        child: Child,
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
    },
}

impl WsTransport {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            WsTransport::Plain(s) => s.write_all(buf),
            WsTransport::TlsProc { stdin, .. } => stdin.write_all(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            WsTransport::Plain(s) => s.flush(),
            WsTransport::TlsProc { stdin, .. } => stdin.flush(),
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            WsTransport::Plain(s) => s.read(buf),
            WsTransport::TlsProc { stdout, .. } => stdout.read(buf),
        }
    }
}

impl Drop for WsTransport {
    fn drop(&mut self) {
        if let WsTransport::TlsProc { child, .. } = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct WsConnection {
    io: WsTransport,
    leftover: Vec<u8>,
    offset: usize,
}

impl WsConnection {
    fn read_exact_bytes(&mut self, dst: &mut [u8]) -> std::io::Result<()> {
        let mut filled = 0usize;
        while filled < dst.len() {
            if self.offset < self.leftover.len() {
                let avail = self.leftover.len() - self.offset;
                let take = avail.min(dst.len() - filled);
                dst[filled..filled + take]
                    .copy_from_slice(&self.leftover[self.offset..self.offset + take]);
                self.offset += take;
                filled += take;
                continue;
            }
            let n = self.io.read(&mut dst[filled..])?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "websocket EOF",
                ));
            }
            filled += n;
        }
        Ok(())
    }

    fn send_masked_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | (opcode & 0x0f));
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if len <= 65_535 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mut mask = [0u8; 4];
        let _ = getrandom::getrandom(&mut mask);
        frame.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i & 3]);
        }
        self.io
            .write_all(&frame)
            .and_then(|_| self.io.flush())
            .map_err(|e| format!("spot WS write failed: {e}"))
    }

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send_masked_frame(0x1, text.as_bytes())
    }

    fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let mut full_payload = Vec::new();
        let mut first_opcode = 0u8;
        let mut fragments = 0usize;
        loop {
            fragments += 1;
            if fragments > MAX_WS_CONTINUATION_FRAMES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws too many continuation frames",
                ));
            }
            let mut hdr = [0u8; 2];
            self.read_exact_bytes(&mut hdr)?;
            let fin = (hdr[0] & 0x80) != 0;
            let opcode = hdr[0] & 0x0f;
            if !matches!(opcode, 0x0..=0x2 | 0x8..=0xA) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws unknown opcode",
                ));
            }
            if opcode == 0 && first_opcode == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws unexpected continuation frame",
                ));
            }
            if first_opcode != 0 && opcode != 0 && opcode < 0x8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws interleaved data frame during continuation",
                ));
            }
            if first_opcode == 0 && opcode != 0 {
                first_opcode = opcode;
            }
            let masked = (hdr[1] & 0x80) != 0;
            let len7 = hdr[1] & 0x7f;
            if opcode >= 0x8 && (!fin || len7 > 125) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws invalid control frame",
                ));
            }
            let payload_len: usize = match len7 {
                0..=125 => len7 as usize,
                126 => {
                    let mut ext = [0u8; 2];
                    self.read_exact_bytes(&mut ext)?;
                    u16::from_be_bytes(ext) as usize
                }
                _ => {
                    let mut ext = [0u8; 8];
                    self.read_exact_bytes(&mut ext)?;
                    let v = u64::from_be_bytes(ext);
                    if v > MAX_WS_FRAME_BYTES as u64 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "ws frame too large",
                        ));
                    }
                    v as usize
                }
            };
            if payload_len > MAX_WS_FRAME_BYTES
                || full_payload
                    .len()
                    .checked_add(payload_len)
                    .is_none_or(|sum| sum > MAX_WS_FRAME_BYTES)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws payload exceeds limit",
                ));
            }
            let mut mask_key = [0u8; 4];
            if masked {
                self.read_exact_bytes(&mut mask_key)?;
            }
            let start = full_payload.len();
            full_payload.resize(start + payload_len, 0);
            if payload_len > 0 {
                self.read_exact_bytes(&mut full_payload[start..])?;
                if masked {
                    for (i, b) in full_payload[start..].iter_mut().enumerate() {
                        *b ^= mask_key[i & 3];
                    }
                }
            }
            if opcode == 0x9 {
                let _ = self.send_masked_frame(0xA, &full_payload[start..]);
                full_payload.truncate(start);
                continue;
            }
            if opcode == 0xA {
                full_payload.truncate(start);
                continue;
            }
            if fin {
                return Ok((first_opcode, full_payload));
            }
        }
    }
}

fn connect_public_ws(url: &str) -> Result<WsConnection, String> {
    let (tls, host, port, path) = parse_ws_url(url)?;
    let mut io = if !tls {
        let addr = format!("{host}:{port}");
        let stream =
            TcpStream::connect(&addr).map_err(|e| format!("spot WS connect failed: {e}"))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        WsTransport::Plain(stream)
    } else {
        let target = format!("{host}:{port}");
        let mut child = Command::new("openssl")
            .args([
                "s_client",
                "-quiet",
                "-servername",
                &host,
                "-connect",
                &target,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spot WS TLS transport unavailable: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "spot WS TLS stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "spot WS TLS stdout unavailable".to_string())?;
        WsTransport::TlsProc {
            child,
            stdin,
            stdout,
        }
    };

    let mut key_raw = [0u8; 16];
    let _ = getrandom::getrandom(&mut key_raw);
    let ws_key = base64::engine::general_purpose::STANDARD.encode(key_raw);
    let expected_accept = orderbook::compute_ws_accept_key(&ws_key);

    let host_header = if (tls && port == 443) || (!tls && port == 80) {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let req = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {ws_key}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    io.write_all(req.as_bytes())
        .and_then(|_| io.flush())
        .map_err(|e| format!("spot WS handshake write failed: {e}"))?;

    let mut resp_buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    let hdr_end = loop {
        if resp_buf.len() > 16_384 {
            return Err("spot WS handshake response too large".to_string());
        }
        let n = io
            .read(&mut tmp)
            .map_err(|e| format!("spot WS handshake read failed: {e}"))?;
        if n == 0 {
            return Err("spot WS handshake closed before 101".to_string());
        }
        resp_buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };

    let hdr_str = std::str::from_utf8(&resp_buf[..hdr_end])
        .map_err(|_| "spot WS handshake response not UTF-8".to_string())?;
    let mut lines = hdr_str.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    if !status_line.contains(" 101 ") {
        return Err(format!("spot WS upgrade rejected: {status_line}"));
    }
    let mut accept_ok = false;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("sec-websocket-accept")
                && v.trim() == expected_accept
            {
                accept_ok = true;
            }
        }
    }
    if !accept_ok {
        return Err("spot WS Sec-WebSocket-Accept mismatch".to_string());
    }

    Ok(WsConnection {
        io,
        leftover: resp_buf[hdr_end + 4..].to_vec(),
        offset: 0,
    })
}

fn public_http_get(full_url: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    if full_url.starts_with("https://") {
        let out = Command::new("curl.exe")
            .args(["-q", "-sS", "-m", "5", "-w", "\n%{http_code}", full_url])
            .output()
            .map_err(|e| format!("curl.exe spot fetch failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "curl.exe spot error: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let (body_part, status_part) = stdout
            .rsplit_once('\n')
            .unwrap_or((stdout.as_ref(), "200"));
        let status: u16 = status_part.trim().parse().unwrap_or(502);
        if !(200..300).contains(&status) {
            return Err(format!("spot REST HTTP {status}"));
        }
        return Ok(body_part.to_string());
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build();
    match agent
        .get(full_url)
        .set("Accept", "application/json")
        .set("User-Agent", "openbell-rs-spot-oracle")
        .call()
    {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| format!("failed reading spot REST response: {e}")),
        Err(ureq::Error::Status(code, err_resp)) => {
            let msg = err_resp.into_string().unwrap_or_default();
            let snippet: String = msg.chars().take(160).collect();
            Err(format!("spot REST HTTP {code}: {snippet}"))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("spot REST transport error: {t}")),
    }
}

fn poll_coinbase_rest_once(store: &SpotFeedStore) -> (usize, Option<String>) {
    let mut ok_count = 0usize;
    let mut last_err = None;
    for &asset in &SPOT_ASSETS {
        let url = format!("{COINBASE_PUBLIC_REST_BASE}/products/{asset}-USD/ticker");
        match public_http_get(&url) {
            Ok(txt) => match store.ingest_rest_payload(&txt, Some(asset), Some("rest")) {
                Ok(n) if n > 0 => ok_count += n,
                Ok(_) => {}
                Err(e) => last_err = Some(e),
            },
            Err(e) => last_err = Some(e),
        }
    }
    (ok_count, last_err)
}

/// Start the public unauthenticated crypto spot feed worker thread.
/// Remains completely idle with zero outbound network traffic when `spot_feed_offline()` is true.
pub fn start_background_spot_feed(app: Arc<App>) {
    std::thread::spawn(move || {
        let mut backoff_ms = WS_RECONNECT_INITIAL_MS;
        let mut next_ws_attempt_ms = 0i64;

        loop {
            if spot_feed_offline() {
                std::thread::sleep(Duration::from_millis(1_500));
                continue;
            }

            let target = match resolve_spot_target() {
                Ok(t) => t,
                Err(e) => {
                    app.spot_feed.set_last_error(Some(e));
                    std::thread::sleep(Duration::from_millis(REST_POLL_INTERVAL_MS));
                    continue;
                }
            };

            match target {
                SpotTarget::Rest { rest_url } => {
                    match public_http_get(&rest_url) {
                        Ok(txt) => match app.spot_feed.ingest_rest_payload(&txt, None, Some("rest")) {
                            Ok(_) => app.spot_feed.set_last_error(None),
                            Err(e) => app.spot_feed.set_last_error(Some(e)),
                        },
                        Err(e) => app.spot_feed.set_last_error(Some(e)),
                    }
                    std::thread::sleep(Duration::from_millis(REST_POLL_INTERVAL_MS));
                }
                SpotTarget::Ws { ws_url } => {
                    let now = now_ms();
                    if now < next_ws_attempt_ms {
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    run_ws_session(&app.spot_feed, &ws_url, &mut backoff_ms, &mut next_ws_attempt_ms);
                }
                SpotTarget::DefaultCoinbase => {
                    let now = now_ms();
                    if now >= next_ws_attempt_ms {
                        run_ws_session(
                            &app.spot_feed,
                            COINBASE_PUBLIC_WS,
                            &mut backoff_ms,
                            &mut next_ws_attempt_ms,
                        );
                    } else {
                        let (_, err) = poll_coinbase_rest_once(&app.spot_feed);
                        if err.is_some() {
                            app.spot_feed.set_last_error(err);
                        }
                        std::thread::sleep(Duration::from_millis(REST_POLL_INTERVAL_MS));
                    }
                }
            }
        }
    });
}

fn run_ws_session(
    store: &SpotFeedStore,
    ws_url: &str,
    backoff_ms: &mut u64,
    next_ws_attempt_ms: &mut i64,
) {
    match connect_public_ws(ws_url) {
        Ok(mut conn) => {
            let sub = serde_json::json!({
                "type": "subscribe",
                "product_ids": ["BTC-USD", "ETH-USD", "SOL-USD", "XRP-USD", "DOGE-USD"],
                "channels": ["ticker"],
            });
            if let Err(e) = conn.send_text(&sub.to_string()) {
                store.set_last_error(Some(e));
                *backoff_ms = (*backoff_ms).saturating_mul(2).min(WS_RECONNECT_MAX_MS);
                *next_ws_attempt_ms = now_ms() + *backoff_ms as i64;
                return;
            }
            store.set_last_error(None);
            let mut session_valid = false;
            loop {
                if spot_feed_offline() {
                    break;
                }
                match conn.read_frame() {
                    Ok((0x1, payload)) => {
                        if let Ok(txt) = std::str::from_utf8(&payload) {
                            match store.ingest_ws_message(txt, Some("ws")) {
                                Ok(n) => {
                                    if n > 0 {
                                        session_valid = true;
                                        *backoff_ms = WS_RECONNECT_INITIAL_MS;
                                    }
                                    store.set_last_error(None);
                                }
                                Err(e) => store.set_last_error(Some(e)),
                            }
                        }
                    }
                    Ok((0x8, _)) => {
                        store.set_last_error(Some("spot WS closed by remote".to_string()));
                        break;
                    }
                    Ok(_) => {}
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => {
                        store.set_last_error(Some(format!("spot WS read error: {e}")));
                        break;
                    }
                }
            }
            *backoff_ms = if session_valid {
                WS_RECONNECT_INITIAL_MS
            } else {
                (*backoff_ms)
                    .max(WS_RECONNECT_INITIAL_MS)
                    .saturating_mul(2)
                    .min(WS_RECONNECT_MAX_MS)
            };
            *next_ws_attempt_ms = now_ms() + *backoff_ms as i64;
        }
        Err(e) => {
            store.set_last_error(Some(e));
            *backoff_ms = (*backoff_ms)
                .max(WS_RECONNECT_INITIAL_MS)
                .saturating_mul(2)
                .min(WS_RECONNECT_MAX_MS);
            *next_ws_attempt_ms = now_ms() + *backoff_ms as i64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_store_ingest_rejects_nan_and_computes_metrics() {
        let dir = std::env::temp_dir().join(format!("ob-spot-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SpotFeedStore::new(dir.clone());

        assert!(!store.ingest_tick("BTC", f64::NAN, 1_000_000, "unit"));
        assert!(!store.ingest_tick("BTC", f64::INFINITY, 1_000_000, "unit"));
        assert!(!store.ingest_tick("BTC", -100.0, 1_000_000, "unit"));
        assert!(!store.ingest_tick("UNKNOWN", 100.0, 1_000_000, "unit"));

        let now = now_ms();
        assert!(store.ingest_tick("KXBTC15M", 90_000.0, now - 35_000, "ws"));
        assert!(store.ingest_tick("BTC-USD", 90_090.0, now - 10_000, "ws"));
        assert!(store.ingest_tick("BTC", 90_270.0, now, "ws"));

        let st = store.status_snapshot();
        assert_eq!(st.assets.len(), 5);
        let btc = st.assets.iter().find(|a| a.asset == "BTC").unwrap();
        assert_eq!(btc.price, Some(90_270.0));
        assert!(!btc.stale);
        assert!(btc.delta_30s_bps.unwrap() > 29.0);
        assert!(btc.vol_bps.unwrap() > 0.0);

        let hist = store.read_history("BTC", 10).unwrap();
        assert_eq!(hist.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spot_store_rotation_corrupt_tolerance_and_symlink_rejection() {
        let dir = std::env::temp_dir().join(format!("ob-spot-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("spot_ticks.jsonl");
        let pad = "{\"ts_ms\":1000,\"asset\":\"ETH\",\"price\":3000.0,\"source\":\"pad\"}\n";
        let repeats = (MAX_SPOT_TICKS_JSONL_BYTES as usize / pad.len()) + 16;
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for _ in 0..repeats {
                f.write_all(pad.as_bytes()).unwrap();
            }
        }
        let store = SpotFeedStore::new(dir.clone());
        let now = now_ms();
        assert!(store.ingest_tick("ETH", 3123.45, now, "ws"));
        assert!(dir.join("spot_ticks.jsonl.1").exists());
        assert!(path.metadata().unwrap().len() < 1024);

        // Append corrupt lines and non-finite price line
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "not json at all").unwrap();
            writeln!(f, "{{\"ts_ms\":{now},\"asset\":\"ETH\",\"price\":null,\"source\":\"bad\"}}").unwrap();
            writeln!(f, "{{\"ts_ms\":{now},\"asset\":\"ETH\",\"price\":-5.0,\"source\":\"bad\"}}").unwrap();
            writeln!(f, "{{\"ts_ms\":{},\"asset\":\"ETH\",\"price\":3130.0,\"source\":\"ws\"}}", now + 100).unwrap();
        }
        let hist = store.read_history("ETH", 50).unwrap();
        assert_eq!(hist.len(), 2);
        assert!((hist[0].price - 3123.45).abs() < 1e-9);
        assert!((hist[1].price - 3130.0).abs() < 1e-9);

        #[cfg(unix)]
        {
            let outside = dir.join("outside.txt");
            std::fs::write(&outside, "secret").unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            let err = store.read_history("ETH", 10).unwrap_err();
            assert_eq!(err.0, 403);
            // Ingesting does not overwrite outside target via symlink
            assert!(store.ingest_tick("ETH", 3150.0, now + 200, "ws"));
            assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
