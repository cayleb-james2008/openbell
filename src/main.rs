//! openbell-rs — Cayleb's Kalshi compounding desk, rebuilt lean.
//!
//! Venue truth on every number: every figure the app shows comes from
//! Kalshi's own record or is shown as "unavailable". This build is a
//! measuring machine — a forward collector plus a shadow scorer. It has NO
//! order-placement code path at all: nothing here can place, arm, or
//! simulate placing a live order. Strategies are Rune scripts, scored on
//! paper against collected forward data, and a strategy is only "proven"
//! when it passes the desk's promotion standard on forward evidence.

mod open;
mod update;

use serde_json::json;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

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

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    fetched_at: i64,
    ticker: String,
    event_ticker: String,
    series: String,
    close_time: String,
    status: String,
    result: String,
    yes_bid: Option<f64>,
    yes_ask: Option<f64>,
    no_bid: Option<f64>,
    no_ask: Option<f64>,
    last_price: Option<f64>,
}

fn dollars(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
}

fn fetch_markets(series: &str, status: &str) -> Result<(Vec<Snapshot>, usize), String> {
    let url = format!("{API_BASE}/markets?series_ticker={series}&status={status}&limit=100");
    let mut last_err = "venue response had no markets array".to_string();
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
        let body: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("venue returned non-JSON: {e}"))?;
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
                ticker: get("ticker"),
                event_ticker: get("event_ticker"),
                series: series.to_string(),
                close_time: get("close_time"),
                status: get("status"),
                result: get("result"),
                yes_bid: dollars(m, "yes_bid_dollars"),
                yes_ask: dollars(m, "yes_ask_dollars"),
                no_bid: dollars(m, "no_bid_dollars"),
                no_ask: dollars(m, "no_ask_dollars"),
                last_price: dollars(m, "last_price_dollars"),
            });
        }
        return Ok((snaps, skipped));
    }
    Err(last_err)
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

/// Run one pass. Ok carries (snapshots written, per-series errors);
/// a pass that wrote nothing BECAUSE everything failed is Err, so
/// the API can say so instead of returning a cheerful 200 with 0.
fn collect_once(data_dir: &Path) -> Result<(usize, Vec<String>), String> {
    let _guard = COLLECT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _pass = PassGuard::new();
    let _ = std::fs::create_dir_all(data_dir);
    let path = data_dir.join("snapshots.jsonl");
    // Build the whole pass in memory and append it in one write: a
    // reader of the tape (the report, the health count) then sees the
    // pass whole or not at all, never a torn half-pass.
    let mut buf = String::new();
    let mut written = 0;
    let mut errors: Vec<String> = Vec::new();
    let mut failed_series: Vec<String> = Vec::new();
    let pace_ms: u64 = std::env::var("OPENBELL_COLLECT_PACE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(250)
        .min(5000);
    let mut call_idx = 0usize;
    for series in SERIES {
        for status in ["open", "settled"] {
            if call_idx > 0 && pace_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(pace_ms));
            }
            call_idx += 1;
            match fetch_markets(series, status) {
                Ok((snaps, skipped)) => {
                    if skipped > 0 {
                        errors.push(format!(
                            "{series}/{status}: skipped {skipped} invalid record(s)"
                        ));
                        if !failed_series.iter().any(|f| f == series) {
                            failed_series.push(series.to_string());
                        }
                    }
                    for snap in &snaps {
                        buf.push_str(&serde_json::to_string(snap).unwrap());
                        buf.push('\n');
                        written += 1;
                    }
                }
                Err(e) => {
                    eprintln!("collect {series}/{status}: {e}");
                    errors.push(format!("{series}/{status}: {e}"));
                    if !failed_series.iter().any(|f| f == series) {
                        failed_series.push(series.to_string());
                    }
                }
            }
        }
    }
    SERIES_FAILED_LAST_PASS.store(errors.len(), std::sync::atomic::Ordering::Relaxed);
    *SERIES_FAILED_NAMES.lock().unwrap_or_else(|p| p.into_inner()) = failed_series;
    if written == 0 {
        return Err(if errors.is_empty() {
            "collection wrote nothing — every series returned empty".to_string()
        } else {
            format!(
                "collection failed on every series: {}",
                errors.join("; ")
            )
        });
    }
    append_tape(&path, &buf).map_err(|e| e.to_string())?;
    Ok((written, errors))
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
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let snaps = lines
        .iter()
        .filter_map(|l| std::str::from_utf8(l).ok())
        .filter_map(|l| serde_json::from_str::<Snapshot>(l).ok())
        .collect();
    (snaps, lines.len())
}

// ---------------------------------------------------------------------------
// Shadow scorer — Rune strategies, promotion standard as code (paper only)
// ---------------------------------------------------------------------------

/// The desk's promotion standard. A strategy is proven only on forward
/// evidence: enough settled windows, and BOTH chronological halves of the
/// record positive, at or above the per-contract bar.
#[derive(Clone, Copy, Serialize)]
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
}

impl Strategy {
    fn load(path: &Path) -> Result<Strategy, String> {
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
        let cache_key = std::fs::metadata(path).ok().map(|m| {
            (
                script.len() as u64,
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            )
        });
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
                });
            }
        }
        let context = rune::Context::with_default_modules().map_err(|e| e.to_string())?;
        let runtime = Arc::new(context.runtime().map_err(|e| e.to_string())?);
        let mut sources = rune::Sources::new();
        sources
            .insert(rune::Source::memory(&script).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let mut diagnostics = rune::Diagnostics::new();
        let unit = rune::prepare(&mut sources)
            .with_context(&context)
            .with_diagnostics(&mut diagnostics)
            .build()
            .map_err(|e| {
                format!(
                    "strategy {} failed to build: {}",
                    path.display(),
                    e.to_string().replace(" (see diagnostics for details)", "")
                )
            })?;
        let unit = Arc::new(unit);
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
}

#[derive(Clone, Serialize)]
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
    std::sync::Mutex<std::collections::HashMap<(u64, String), (usize, u64, Option<f64>)>>;
static PREFIX_CACHE: std::sync::LazyLock<PrefixCache> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn window_fingerprint(snaps: &[Snapshot]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    snaps.len().hash(&mut h);
    for s in snaps {
        s.fetched_at.hash(&mut h);
        s.yes_ask.map(f64::to_bits).hash(&mut h);
        s.no_ask.map(f64::to_bits).hash(&mut h);
        s.close_time.hash(&mut h);
    }
    h.finish()
}

/// The entry a strategy would have taken in one window: the ask on its
/// side at the first snapshot (in fetch order) where its Rune
/// `allow_entry` says yes. None when it never enters.
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
    // A strategy whose aggregate allowance is spent degrades to
    // no-entry for the rest of the pass — from the index or not.
    if !strat.budget_open() {
        return None;
    }
    let key = (strat.fingerprint, win_fp);
    if let Some(hit) = ENTRY_CACHE.lock().unwrap().get(&key) {
        return *hit;
    }
    // Resume a previous scan of this window when its covered
    // prefix is still the window's prefix verbatim.
    let mut resume_from = 0usize;
    if let Some(t) = ticker {
        let prior = PREFIX_CACHE
            .lock()
            .unwrap()
            .get(&(strat.fingerprint, t.to_string()))
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
                    return Some(price);
                }
                resume_from = scanned;
            }
        }
    }
    let entry = window_entry_scan(strat, &snaps[resume_from..]);
    if !strat.budget_exhausted() {
        let mut guard = ENTRY_CACHE.lock().unwrap();
        // Windows stop changing once settled, so the index stays
        // small; the cap is insurance for a months-long process —
        // a cleared index only costs recompute, never correctness.
        if guard.len() > 250_000 {
            guard.clear();
        }
        guard.insert(key, entry);
        drop(guard);
        if let Some(t) = ticker {
            let mut guard = PREFIX_CACHE.lock().unwrap();
            if guard.len() > 250_000 {
                guard.clear();
            }
            guard.insert(
                (strat.fingerprint, t.to_string()),
                (snaps.len(), win_fp, entry),
            );
        }
    }
    entry
}

fn window_entry_scan(strat: &Strategy, snaps: &[Snapshot]) -> Option<f64> {
    for snap in snaps {
        let (Some(ya), Some(na)) = (snap.yes_ask, snap.no_ask) else {
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
                return Some(price);
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
        if !s.result.is_empty() && !distinct.contains(&s.result.as_str()) {
            distinct.push(s.result.as_str());
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
        .find(|s| s.result == result)
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
fn group_windows(snapshots: &[Snapshot]) -> BTreeMap<String, Vec<Snapshot>> {
    let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
    for snap in snapshots {
        windows.entry(snap.ticker.clone()).or_default().push(snap.clone());
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
        let Some(price) = window_entry_keyed(strat, Some(_ticker), snaps, win_fp) else {
            continue;
        };
        if price <= 0.0 || price >= 1.0 {
            continue;
        }
        let payoff = if result == strat.side { 1.0 } else { 0.0 };
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
    // Settled windows in chronological order, with every strategy's
    // entry price precomputed.
    struct Window {
        close_epoch: i64,
        result: String,
        entries: Vec<Option<f64>>,
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
            .map(|s| window_entry_keyed(s, Some(_ticker), snaps, win_fp))
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
            let Some(price) = window.entries[si] else {
                continue;
            };
            if price <= 0.0 || price >= 1.0 {
                continue;
            }
            let contracts = (share / price).floor() as i64;
            if contracts <= 0 {
                continue;
            }
            let payoff = if window.result == strat.side {
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
            .collect();
        paths.sort();
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
    let collecting = health["collecting"].as_bool().unwrap_or(false);
    let age = health["newest_snapshot_age_secs"].as_i64();
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
            health["snapshots_on_disk"],
            health["markets_seen"]
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
            health["snapshots_on_disk"],
            health["markets_seen"],
            health["settled_windows"],
            age_text,
            match health["series_failed_last_pass"].as_u64() {
                Some(n) if n > 0 => {
                    let names: Vec<&str> = health["series_failed_names"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    let named = if names.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", names.join(", "))
                    };
                    format!(
                        " {n} of {} market reads failed on the last pass ({} series, open and settled){named}.",
                        health["series_total"],
                        SERIES.len()
                    )
                }
                _ => String::new(),
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
    for report in &scores {
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
/// to summarise.
struct TapeEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    /// Shared: every surface reads the same parse, and a cache hit
    /// must not clone a quarter-million snapshots per request.
    snaps: Arc<Vec<Snapshot>>,
    /// The same snapshots grouped into market windows (each in
    /// fetch order) — the analysis's working shape. Grouping a
    /// quarter-million snapshots per recompute cost seconds; here
    /// it is built once per parse and folded incrementally over
    /// appends, like everything else in this entry.
    grouped: Arc<BTreeMap<String, Vec<Snapshot>>>,
    /// Every line in the file, parseable or not — the health block's
    /// dropped-line count is total minus parsed, so both numbers
    /// must come from the same read.
    total_lines: usize,
    /// Collector-health aggregates over the parsed tape, folded in
    /// as the tape is parsed (and folded incrementally over the
    /// tail on append): the distinct markets, and each market's set
    /// of non-empty results — settled windows and conflicting
    /// settlements both derive from these. Computing them per
    /// request instead made every surface pay a full-tape scan.
    markets: std::collections::HashSet<String>,
    results_by_ticker: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Byte offset at which the last line starts, and its raw text:
    /// the seam an append must continue.
    seam_offset: u64,
    seam_line: String,
    /// Whether the parsed bytes end at a line boundary. A torn last
    /// line cannot be extended: the next pass's first byte would
    /// finish it, and the line count would be off by one.
    ends_newline: bool,
}

/// The seam an append must continue, plus the health aggregates:
/// see TapeEntry.
fn fold_health(
    snaps: &[Snapshot],
    markets: &mut std::collections::HashSet<String>,
    results_by_ticker: &mut std::collections::HashMap<String, std::collections::HashSet<String>>,
) {
    for s in snaps {
        markets.insert(s.ticker.clone());
        if !s.result.is_empty() {
            results_by_ticker
                .entry(s.ticker.clone())
                .or_default()
                .insert(s.result.clone());
        }
    }
}

/// One consistent read of a tape's current state: the parsed
/// snapshots plus everything the health block states about them.
struct TapeView {
    snaps: Arc<Vec<Snapshot>>,
    grouped: Arc<BTreeMap<String, Vec<Snapshot>>>,
    total_lines: usize,
    markets: usize,
    settled_windows: usize,
    conflicting_settlements: usize,
    newest_fetched_at: Option<i64>,
}
/// (offset of the last line, its text) for a text whose first byte
/// sits at `base` in the file.
fn seam_of(text: &str, base: u64) -> (u64, String) {
    let t = text.strip_suffix('\n').unwrap_or(text);
    match t.rfind('\n') {
        Some(i) => (base + i as u64 + 1, t[i + 1..].to_string()),
        None => (base, t.to_string()),
    }
}

struct AnalysisEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
    /// Hash of everything besides the tape that shapes the result:
    /// the paper pot and each strategy's (name, source fingerprint).
    key: u64,
    sim: SlotSimulation,
    scores: Vec<ScoreReport>,
}

struct App {
    data_dir: PathBuf,
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
    tape_cache: std::sync::Mutex<Option<TapeEntry>>,
    /// The computed analysis (slot simulation + per-strategy scores)
    /// for one exact tape state. Scoring the full tape runs Rune over
    /// every window; the result is a pure function of (tape state,
    /// strategy sources, paper pot), so repeat requests against an
    /// unchanged tape serve it directly, and a grown tape recomputes
    /// with the per-window entry index doing the incremental work.
    analysis_cache: std::sync::Mutex<Option<AnalysisEntry>>,
}

fn tape_stat(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| {
        (
            m.len(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        )
    })
}

impl App {
    /// The tape's parsed snapshots and its total line count, from
    /// one consistent read (see TapeEntry).
    fn tape(&self, path: &Path) -> TapeView {
        fn view(e: &TapeEntry) -> TapeView {
            TapeView {
                snaps: e.snaps.clone(),
                grouped: e.grouped.clone(),
                total_lines: e.total_lines,
                markets: e.markets.len(),
                settled_windows: e.results_by_ticker.len(),
                conflicting_settlements: e
                    .results_by_ticker
                    .values()
                    .filter(|r| r.len() > 1)
                    .count(),
                newest_fetched_at: e.snaps.last().map(|s| s.fetched_at),
            }
        }
        let Some((len, mtime)) = tape_stat(path) else {
            return TapeView {
                snaps: Arc::new(Vec::new()),
                grouped: Arc::new(BTreeMap::new()),
                total_lines: 0,
                markets: 0,
                settled_windows: 0,
                conflicting_settlements: 0,
                newest_fetched_at: None,
            };
        };
        {
            let guard = self.tape_cache.lock().unwrap();
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
            let mut guard = self.tape_cache.lock().unwrap();
            match &*guard {
                Some(e) if e.path == path && len > e.len && e.len > 0 => guard.take(),
                _ => None,
            }
        };
        if let Some(e) = prev {
            if let Some(extended) = extend_tape(e, path, len, mtime) {
                let out = view(&extended);
                *self.tape_cache.lock().unwrap() = Some(extended);
                return out;
            }
            // Seam check failed: the file was replaced, not appended
            // to. Fall through to a full parse of the current bytes.
        }
        let bytes = std::fs::read(path).unwrap_or_default();
        let (snaps, total_lines) = parse_tape_bytes(&bytes);
        let (seam_offset, seam_line) = seam_of(&String::from_utf8_lossy(&bytes), 0);
        let ends_newline = bytes.last().is_none_or(|b| *b == b'\n');
        let mut markets = std::collections::HashSet::new();
        let mut results_by_ticker = std::collections::HashMap::new();
        fold_health(&snaps, &mut markets, &mut results_by_ticker);
        let grouped = Arc::new(group_windows(&snaps));
        let entry = TapeEntry {
            path: path.to_path_buf(),
            len,
            mtime,
            snaps: Arc::new(snaps),
            grouped,
            total_lines,
            markets,
            results_by_ticker,
            seam_offset,
            seam_line,
            ends_newline,
        };
        let out = view(&entry);
        *self.tape_cache.lock().unwrap() = Some(entry);
        out
    }

    fn snapshots_cached(&self, path: &Path) -> Arc<Vec<Snapshot>> {
        self.tape(path).snaps
    }

    /// The slot simulation and per-strategy scores over one dataset,
    /// cached by the tape's exact state (see analysis_cache).
    fn analysis(&self, dpath: &Path) -> (SlotSimulation, Vec<ScoreReport>) {
        let strategies = load_strategies(&self.strategies_dir);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.paper_pot.to_bits().hash(&mut h);
            for s in &strategies {
                s.name.hash(&mut h);
                s.fingerprint.hash(&mut h);
            }
            h.finish()
        };
        let (len, mtime) = tape_stat(dpath).unwrap_or((0, 0));
        {
            let guard = self.analysis_cache.lock().unwrap();
            if let Some(e) = &*guard {
                if e.path == dpath && e.len == len && e.mtime == mtime && e.key == key {
                    return (e.sim.clone(), e.scores.clone());
                }
            }
        }
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
        let sim = simulate_slots_windows_fps(&strategies, &view.grouped, self.paper_pot, Some(&fps));
        let scores: Vec<ScoreReport> = strategies
            .iter()
            .map(|s| score_strategy_windows_fps(s, &view.grouped, Standard::default(), Some(&fps)))
            .collect();
        let entry = AnalysisEntry {
            path: dpath.to_path_buf(),
            len,
            mtime,
            key,
            sim,
            scores,
        };
        let out = (entry.sim.clone(), entry.scores.clone());
        *self.analysis_cache.lock().unwrap() = Some(entry);
        out
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
    let (new_snaps, tail_lines) = parse_tape_bytes(&tail_buf);
    e.total_lines += tail_lines;
    // The held parse is shared with readers; extending it mutates
    // in place when this entry is the only holder (the common
    // case), and every fold touches only the new snapshots.
    fold_health(&new_snaps, &mut e.markets, &mut e.results_by_ticker);
    {
        let grouped = Arc::make_mut(&mut e.grouped);
        for s in &new_snaps {
            let w = grouped.entry(s.ticker.clone()).or_default();
            // Appends arrive in fetch order, so the window stays
            // sorted; an out-of-order tail re-sorts its window
            // instead of silently corrupting the order.
            let out_of_order = w
                .last()
                .is_some_and(|last| s.fetched_at < last.fetched_at);
            w.push(s.clone());
            if out_of_order {
                w.sort_by_key(|x| x.fetched_at);
            }
        }
    }
    Arc::make_mut(&mut e.snaps).extend(new_snaps);
    if !tail_buf.is_empty() {
        let (off, line) = seam_of(&String::from_utf8_lossy(&tail_buf), e.len);
        e.seam_offset = off;
        e.seam_line = line;
        e.ends_newline = tail_buf.last() == Some(&b'\n');
    }
    e.len = len;
    e.mtime = mtime;
    Some(e)
}

/// Collector health, computed from the tape itself — the same numbers
/// an independent count of the file gives.
fn collector_health(app: &App) -> serde_json::Value {
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
    serde_json::json!({
        "snapshots_on_disk": view.snaps.len(),
        "lines_on_disk": view.total_lines,
        "dropped_lines": view.total_lines.saturating_sub(view.snaps.len()),
        "markets_seen": view.markets,
        "settled_windows": view.settled_windows,
        "conflicting_settlements": view.conflicting_settlements,
        "newest_snapshot_age_secs": age,
        "collect_interval_secs": app.collect_secs,
        "collecting": collecting,
        "series_failed_last_pass": SERIES_FAILED_LAST_PASS.load(std::sync::atomic::Ordering::Relaxed),
        "series_failed_names": SERIES_FAILED_NAMES.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        "series_total": SERIES.len() * 2,
    })
}

/// Resolve the `dataset=` query parameter to a tape file in the data
/// dir. Names are restricted to [a-z0-9-] so the parameter can never
/// walk out of the data dir. No parameter means the live collected tape.
/// The dataset a query names: "live" for the collected tape, or
/// the sanitised dataset= value. Responses carry the name so a
/// capture of fixture figures can never pass as live ones.
fn dataset_name(query: &str) -> String {
    if let Some(pos) = query.find("dataset=") {
        let raw = &query[pos + "dataset=".len()..];
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
    if let Some(pos) = query.find("dataset=") {
        let raw = &query[pos + "dataset=".len()..];
        let name: String = raw
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if !name.is_empty() && name != "live" {
            return app.data_dir.join(format!("{name}-snapshots.jsonl"));
        }
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

fn json_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
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
    let host = header("host").to_ascii_lowercase();
    let host_name = authority_host(&host);
    let host_ok = matches!(host_name, "localhost" | "127.0.0.1" | "[::1]")
        || allowed.iter().any(|a| a == host_name);
    if !host_ok {
        return Some((403, "this desk answers only on localhost unless OPENBELL_ALLOWED_HOSTS names this host"));
    }
    if matches!(method, "GET" | "HEAD") {
        return None;
    }
    if header("sec-fetch-site") == "cross-site" {
        return Some((403, "cross-site requests are refused"));
    }
    let origin = header("origin");
    if !origin.is_empty() {
        let origin_authority = origin.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
        if !origin_authority.eq_ignore_ascii_case(&host) {
            return Some((403, "cross-origin requests are refused"));
        }
    }
    if body_len > 0 && !header("content-type").to_ascii_lowercase().starts_with("application/json") {
        return Some((415, "requests with a body must be Content-Type: application/json"));
    }
    None
}

type RawRequest = (String, String, String, Vec<(String, String)>);
fn read_request(stream: &mut std::net::TcpStream) -> Option<RawRequest> {
    // One deadline for the whole request: a per-read timeout alone
    // is defeated by a client dripping a byte just under it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    // Header lines are read byte by byte so the deadline is checked
    // for EVERY byte: a read_line would wait inside one line while
    // a drip feeds it a byte per per-read timeout.
    let read_line_by_byte =
        |reader: &mut BufReader<std::net::TcpStream>| -> Option<String> {
            let mut line = String::new();
            loop {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                let mut b = [0u8; 1];
                let n = reader.read(&mut b).ok()?;
                if n == 0 {
                    return if line.is_empty() { None } else { Some(line) };
                }
                line.push(b[0] as char);
                if b[0] == b'\n' || line.len() > 16384 {
                    return Some(line);
                }
            }
        };
    let request_line = read_line_by_byte(&mut reader)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length = 0usize;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let line = read_line_by_byte(&mut reader)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Some((
            "__too_large__".to_string(),
            String::new(),
            String::new(),
            Vec::new(),
        ));
    }
    let mut body: Vec<u8> = Vec::with_capacity(content_length);
    if content_length > 0 {
        let mut chunk = [0u8; 65536];
        while body.len() < content_length {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            let want = (content_length - body.len()).min(chunk.len());
            match reader.read(&mut chunk[..want]) {
                Ok(0) => return None,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
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

fn latest_markets_from(app: &App, path: &Path) -> Vec<Snapshot> {
    let snapshots = app.snapshots_cached(path);
    let mut latest: BTreeMap<String, Snapshot> = BTreeMap::new();
    for snap in snapshots.iter() {
        latest
            .entry(snap.ticker.clone())
            .and_modify(|e| {
                if snap.fetched_at >= e.fetched_at {
                    *e = snap.clone();
                }
            })
            .or_insert_with(|| snap.clone());
    }
    let mut out: Vec<Snapshot> = latest.into_values().collect();
    out.sort_by(|a, b| a.close_time.cmp(&b.close_time));
    out
}

fn market_history_from(app: &App, path: &Path, ticker: &str) -> Vec<Snapshot> {
    let snapshots = app.snapshots_cached(path);
    let mut pts: Vec<Snapshot> = snapshots
        .iter()
        .filter(|s| s.ticker == ticker)
        .cloned()
        .collect();
    pts.sort_by_key(|s| s.fetched_at);
    let total = pts.len();
    if total > 240 {
        pts.drain(0..total - 240);
    }
    pts
}

fn strategy_source_for(dir: &Path, name: &str) -> Option<(String, String, String)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "rn").unwrap_or(false))
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

fn handle(app: &App, method: &str, path: &str) -> String {
    let (route, query) = match path.split_once('?') {
        Some((r, q)) => (r, q),
        None => (path, ""),
    };
    match (method, route) {
        ("GET", "/") => html_response(include_str!("../assets/index.html")),
        ("GET", "/api/home") => {
            let (sim, _) = app.analysis(&app.data_dir.join("snapshots.jsonl"));
            let home = serde_json::json!({
                "account": account_state(),
                "collector": collector_health(app),
                "stage_target": stage_progress(&sim),
                "library_errors": load_strategies_with_errors(&app.strategies_dir)
                    .1
                    .iter()
                    .map(|(file, error)| serde_json::json!({ "file": file, "error": error }))
                    .collect::<Vec<_>>(),
                "version": env!("CARGO_PKG_VERSION"),
                "mode": "shadow — this build never places orders",
                "series": SERIES,
            });
            json_response(200, &home.to_string())
        }
        ("GET", "/api/markets") => {
            let dpath = dataset_path(app, query);
            if dataset_name(query) != "live" && !dpath.exists() {
                return json_response(
                    404,
                    &serde_json::json!({ "error": format!("no such dataset: {}", dataset_name(query)) })
                        .to_string(),
                );
            }
            json_response(200, &serde_json::to_string(&latest_markets_from(app, &dpath)).unwrap())
        }
        ("GET", "/api/markets/history") => {
            let dpath = dataset_path(app, query);
            if dataset_name(query) != "live" && !dpath.exists() {
                return json_response(
                    404,
                    &serde_json::json!({ "error": format!("no such dataset: {}", dataset_name(query)) })
                        .to_string(),
                );
            }
            let Some(ticker) = query_param(query, "ticker") else {
                return json_response(400, "{\"error\":\"missing ticker parameter\"}");
            };
            let points = market_history_from(app, &dpath, &ticker);
            let body = serde_json::json!({
                "ticker": ticker,
                "dataset": dataset_name(query),
                "points": points,
            });
            json_response(200, &body.to_string())
        }
        ("GET", "/api/strategy/source") => {
            let Some(name) = query_param(query, "name") else {
                return json_response(400, "{\"error\":\"missing name parameter\"}");
            };
            match strategy_source_for(&app.strategies_dir, &name) {
                Some((file, side, source)) => json_response(
                    200,
                    &serde_json::json!({
                        "strategy": name,
                        "file": file,
                        "side": side,
                        "source": source,
                    })
                    .to_string(),
                ),
                None => json_response(
                    404,
                    &serde_json::json!({ "error": format!("no such strategy: {name}") }).to_string(),
                ),
            }
        }
        ("POST", "/api/collect") => match collect_once(&app.data_dir) {
            Ok((n, errors)) => {
                if errors.is_empty() {
                    json_response(200, &format!("{{\"snapshots_written\":{n}}}"))
                } else {
                    json_response(
                        200,
                        &serde_json::json!({
                            "snapshots_written": n,
                            "errors": errors
                        })
                        .to_string(),
                    )
                }
            }
            Err(e) => json_response(
                502,
                &serde_json::json!({ "error": e }).to_string(),
            ),
        },
        ("GET", "/api/strategies") => {
            let dpath = dataset_path(app, query);
            if dataset_name(query) != "live" && !dpath.exists() {
                return json_response(
                    404,
                    &serde_json::json!({ "error": format!("no such dataset: {}", dataset_name(query)) })
                        .to_string(),
                );
            }
            let (_, mut reports) = app.analysis(&dpath);
            let dataset = dataset_name(query);
            for report in &mut reports {
                report.verdict = dataset_verdict(&report.verdict, &dataset);
            }
            json_response(200, &serde_json::to_string(&reports).unwrap())
        }
        ("GET", "/api/slots") => {
            let dpath = dataset_path(app, query);
            if dataset_name(query) != "live" && !dpath.exists() {
                return json_response(
                    404,
                    &serde_json::json!({ "error": format!("no such dataset: {}", dataset_name(query)) })
                        .to_string(),
                );
            }
            let (sim, _) = app.analysis(&dpath);
            let mut body = serde_json::to_value(&sim).unwrap();
            body["stage_target"] = serde_json::to_value(stage_progress(&sim)).unwrap();
            let total = sim.trades.len();
            let shown: Vec<&Trade> =
                sim.trades.iter().skip(total.saturating_sub(5000)).collect();
            body["trades"] = serde_json::to_value(&shown).unwrap();
            body["trades_truncated"] = serde_json::json!(total > 5000);
            body["library_errors"] = serde_json::json!(
                load_strategies_with_errors(&app.strategies_dir)
                    .1
                    .iter()
                    .map(|(file, error)| serde_json::json!({ "file": file, "error": error }))
                    .collect::<Vec<_>>()
            );
            body["dataset"] = serde_json::json!(dataset_name(query));
            json_response(200, &body.to_string())
        }
        ("GET", "/api/equity") => {
            let dpath = dataset_path(app, query);
            if dataset_name(query) != "live" && !dpath.exists() {
                return json_response(
                    404,
                    &serde_json::json!({ "error": format!("no such dataset: {}", dataset_name(query)) })
                        .to_string(),
                );
            }
            let strategies = load_strategies(&app.strategies_dir);
            let (sim, _) = app.analysis(&dpath);
            let (pot, per) = equity_curves(&sim);
            let mut series: Vec<serde_json::Value> = strategies
                .iter()
                .map(|s| {
                    let points = per
                        .iter()
                        .find(|(name, _)| *name == s.name)
                        .map(|(_, pts)| pts.clone())
                        .unwrap_or_default();
                    serde_json::json!({ "strategy": s.name, "points": points })
                })
                .collect();
            series.sort_by(|a, b| a["strategy"].as_str().cmp(&b["strategy"].as_str()));
            let body = serde_json::json!({
                "paper_pot_start": sim.paper_pot_start,
                "pot": pot,
                "series": series,
                "library_errors": load_strategies_with_errors(&app.strategies_dir)
                    .1
                    .iter()
                    .map(|(file, error)| serde_json::json!({ "file": file, "error": error }))
                    .collect::<Vec<_>>(),
                "dataset": dataset_name(query),
            });
            json_response(200, &body.to_string())
        }
        ("GET", "/api/report") => {
            let report = daily_report(app);
            json_response(200, &serde_json::json!({ "report": report }).to_string())
        }
        _ => json_response(404, "{\"error\":\"not found\"}"),
    }
}

/// The default desk port, and how many ports above it a busy default may use.
const DEFAULT_PORT: u16 = 8472;
const DEFAULT_PORT_SPAN: u16 = 20;

/// The strategy library built into the binary, so a first run has the five shapes without a checkout.
const BUILT_IN_STRATEGIES: [(&str, &str); 5] = [
    ("favourite_floor.rn", include_str!("../strategies/favourite_floor.rn")),
    ("favourite_late.rn", include_str!("../strategies/favourite_late.rn")),
    ("longshot_late.rn", include_str!("../strategies/longshot_late.rn")),
    ("mid_window_momentum.rn", include_str!("../strategies/mid_window_momentum.rn")),
    ("no_favourite.rn", include_str!("../strategies/no_favourite.rn")),
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
        "\n  OpenBell {}\n  Your desk:  {url}  ({browser})\n  Tape:       {}\n  Updates:    {updates}\n  Shadow mode: no order path exists in this build.\n",
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

fn main() {
    if std::env::args().nth(1).as_deref().is_some_and(|a| a == "--version" || a == "-V") {
        println!("openbell-rs {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // An explicit PORT is a promise: it is used exactly, or the start fails loudly.
    let explicit_port: Option<u16> = std::env::var("PORT").ok().map(|p| {
        p.parse().unwrap_or_else(|_| {
            panic!("PORT is set to '{p}', which is not a valid port — refusing to silently fall back to 8472")
        })
    });
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

    // Forward collector: one pass now, then on an interval.
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
    {
        let data_dir = data_dir.clone();
        std::thread::spawn(move || loop {
            match collect_once(&data_dir) {
                Ok((n, errors)) => {
                    if errors.is_empty() {
                        println!("collector: {n} snapshots written");
                    } else {
                        println!(
                            "collector: {n} snapshots written, {} series failed",
                            errors.len()
                        );
                    }
                }
                Err(e) => eprintln!("collector: {e}"),
            }
            std::thread::sleep(std::time::Duration::from_secs(collect_secs));
        });
    }

    let app = Arc::new(App {
        tape_cache: std::sync::Mutex::new(None),
        analysis_cache: std::sync::Mutex::new(None),
        data_dir: data_dir.clone(),
        strategies_dir,
        paper_pot,
        collect_secs,
    });
    let allowed_hosts: Vec<String> = std::env::var("OPENBELL_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    updater().start_background();
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
    let url = format!("http://127.0.0.1:{port}");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opening = open::wanted(&args, &get_env, std::env::consts::OS);
    if opening {
        open::open_in_browser(&url);
    }
    println!("{}", start_banner(&url, &data_dir, updater().cfg.mode, opening));
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let app = app.clone();
                let allowed_hosts = allowed_hosts.clone();
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                    if let Some((method, path, body, headers)) = read_request(&mut stream) {
                        if method == "__too_large__" {
                            let resp = "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/json\r\n\r\n{\"error\":\"request body too large\"}";
                            let _ = stream.write_all(resp.as_bytes());
                            return;
                        }
                        let host = headers.iter().find(|(n, _)| n == "host").map(|(_, v)| v.as_str()).unwrap_or("");
                        let response = match refuse_request(&method, &headers, body.len(), &allowed_hosts) {
                            Some((status, message)) => {
                                json_response(status, &serde_json::json!({ "error": message }).to_string())
                            }
                            None => update_response(&method, &path, host).unwrap_or_else(|| handle(&app, &method, &path)),
                        };
                        let _ = stream.write_all(response.as_bytes());
                    }
                });
            }
            Err(_) => continue,
        }
    }
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
                return Some(json_response(
                    403,
                    &json!({ "error": "updating is only allowed from this machine, not through a shared address" }).to_string(),
                ));
            }
            if path == "/api/update/apply" {
                match up.apply() {
                    Ok(v) => {
                        let mut snap = up.snapshot();
                        snap["applied"] = json!(v);
                        json_response(200, &snap.to_string())
                    }
                    Err(e) => json_response(409, &json!({ "error": e }).to_string()),
                }
            } else if let Some(e) = restart_refusal(
                up.snapshot()["staged"].is_string(),
                PASSES_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst) > 0,
            ) {
                json_response(409, &json!({ "error": e }).to_string())
            } else {
                let restarter = up.clone();
                std::thread::spawn(move || {
                    // Give the answer below time to leave before this process is replaced.
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    if let Err(e) = restarter.restart() {
                        eprintln!("openbell-rs: restart failed: {e}");
                    }
                });
                json_response(200, &json!({ "restarting": true }).to_string())
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
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
            ticker: ticker.to_string(),
            event_ticker: "EV".to_string(),
            series: "KXBTC15M".to_string(),
            close_time: close.to_string(),
            status: if result.is_empty() { "active".into() } else { "settled".into() },
            result: result.to_string(),
            yes_bid: ya.map(|v| v - 0.02),
            yes_ask: ya,
            no_bid: na.map(|v| v - 0.02),
            no_ask: na,
            last_price: None,
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
            yes_bid: None,
            yes_ask: Some(0.5),
            no_bid: None,
            no_ask: Some(0.5),
            last_price: None,
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
        std::fs::write(&tape, format!("{}\n{}\nnot json\n", line("A", 100), line("B", 110)))
            .unwrap();
        let view = app.tape(&tape);
        assert_eq!((view.snaps.len(), view.total_lines), (2, 3), "garbage line counted, not parsed");
        // Append-only growth: the held parse extends by the tail.
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).open(&tape).unwrap();
        writeln!(f, "{}", line("C", 120)).unwrap();
        drop(f);
        let view = app.tape(&tape);
        assert_eq!((view.snaps.len(), view.total_lines), (3, 4));
        assert_eq!(view.snaps[2].ticker, "C");
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
        assert_eq!((view.snaps.len(), view.total_lines), (6, 6));
        assert!(view.snaps.iter().all(|s| s.ticker == "ZZ"));
        // A shorter replacement parses whole too.
        std::fs::write(&tape, format!("{}\n", line("Q", 300))).unwrap();
        let view = app.tape(&tape);
        assert_eq!((view.snaps.len(), view.total_lines), (1, 1));
        assert_eq!(view.snaps[0].ticker, "Q");
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
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
            (view.snaps.len(), view.total_lines),
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
        assert_eq!((view.snaps.len(), view.total_lines), (3, 5));
        assert_eq!(view.snaps[2].ticker, "C");
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
        // A crash left a torn last line with no newline.
        std::fs::write(&tape, format!("{}\n{{\"ticker\":\"TO", line("A", 100))).unwrap();
        let view = app.tape(&tape);
        assert_eq!((view.snaps.len(), view.total_lines), (1, 2));
        // The next pass lands after the torn line.
        append_tape(&tape, &format!("{}\n{}\n", line("B", 110), line("C", 120))).unwrap();
        let view = app.tape(&tape);
        let tickers: Vec<&str> = view.snaps.iter().map(|s| s.ticker.as_str()).collect();
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
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
        assert_eq!((v["name"].clone(), v["version"].clone()), (json!("openbell-rs"), json!("0.4.0")));
        assert_eq!(v["release_url"], json!("https://github.com/cayleb-james2008/openbell/releases/tag/v0.4.0"));
    }

    #[test]
    fn update_snapshot_has_its_shape_before_anything_is_fetched() {
        let (status, u) = body_json(&update_response("GET", "/api/update", "localhost:8472").unwrap());
        assert_eq!(status, 200);
        for key in ["name", "current", "mode", "repo", "platform", "checked_at", "available", "latest", "notes", "release_url", "error", "staged", "can_apply", "dev_build"] {
            assert!(u.get(key).is_some(), "missing {key}: {u}");
        }
        assert_eq!((u["name"].clone(), u["current"].clone(), u["available"].clone()), (json!("openbell-rs"), json!("0.4.0"), json!(false)));
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: dir.clone(),
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
        let (_, home) = body_json(&handle(&app, "GET", "/api/home"));
        assert_eq!(home["version"], json!("0.4.0"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn market_history_and_strategy_source_endpoints_return_read_only_data() {
        let dir = std::env::temp_dir().join(format!("ob-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s1 = Snapshot {
            fetched_at: 1000,
            ticker: "KXBTC15M-TEST".to_string(),
            event_ticker: "KXBTC15M".to_string(),
            series: "KXBTC15M".to_string(),
            close_time: "2026-11-21T23:00:00Z".to_string(),
            status: "open".to_string(),
            result: String::new(),
            yes_bid: Some(0.51),
            yes_ask: Some(0.54),
            no_bid: Some(0.46),
            no_ask: Some(0.49),
            last_price: Some(0.52),
        };
        let mut s2 = s1.clone();
        s2.fetched_at = 1060;
        s2.yes_ask = Some(0.58);
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
        let app = App {
            data_dir: dir.clone(),
            strategies_dir: strat_dir,
            paper_pot: 100.0,
            collect_secs: 60,
            tape_cache: std::sync::Mutex::new(None),
            analysis_cache: std::sync::Mutex::new(None),
        };
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
        assert_eq!(load_strategies(&dir).len(), 5, "all five shapes load from the seeded folder");
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
        assert!(b.contains("auto (installs") && b.contains("opening it in your browser") && b.contains("no order path"));
    }
}
