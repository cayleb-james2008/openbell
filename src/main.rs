//! openbell-rs — Cayleb's Kalshi compounding desk, rebuilt lean.
//!
//! Venue truth on every number: every figure the app shows comes from
//! Kalshi's own record or is shown as "unavailable". This build is a
//! measuring machine — a forward collector plus a shadow scorer. It has NO
//! order-placement code path at all: nothing here can place, arm, or
//! simulate placing a live order. Strategies are Rune scripts, scored on
//! paper against collected forward data, and a strategy is only "proven"
//! when it passes the desk's promotion standard on forward evidence.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
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
    if !(1..=12).contains(&mo) || !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || ss < 0 || ss > 60 {
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
    let out = Command::new("curl")
        .args(["-s", "-m", "25", &url])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exited {}", out.status));
    }
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("venue returned non-JSON: {e}"))?;
    let fetched_at = now_unix();
    let mut snaps: Vec<Snapshot> = Vec::new();
    let mut skipped = 0usize;
    // A response without a markets array is not market data — the
    // venue's error envelope is valid JSON with an "error" key, and
    // treating it as an empty list would report a dead venue as a
    // successful, empty collection.
    let Some(markets) = body.get("markets").and_then(|m| m.as_array()) else {
        return Err("venue response had no markets array".to_string());
    };
    {
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
    }
    Ok((snaps, skipped))
}

/// One collector pass over every series the desk trades: open markets (the
/// forward tape) plus settled markets (so windows get their results).
/// Appends JSONL snapshots to disk. Returns how many snapshots were written.
/// One collection pass at a time, across every caller (the
/// background loop and POST /api/collect): concurrent passes each
/// hold their own append handle, and their writes interleave into
/// spliced, unparseable tape lines.
static COLLECT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run one pass. Ok carries (snapshots written, per-series errors);
/// a pass that wrote nothing BECAUSE everything failed is Err, so
/// the API can say so instead of returning a cheerful 200 with 0.
fn collect_once(data_dir: &Path) -> Result<(usize, Vec<String>), String> {
    let _guard = COLLECT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _ = std::fs::create_dir_all(data_dir);
    let path = data_dir.join("snapshots.jsonl");
    // Build the whole pass in memory and append it in one write: a
    // reader of the tape (the report, the health count) then sees the
    // pass whole or not at all, never a torn half-pass.
    let mut buf = String::new();
    let mut written = 0;
    let mut errors: Vec<String> = Vec::new();
    for series in SERIES {
        for status in ["open", "settled"] {
            match fetch_markets(series, status) {
                Ok((snaps, skipped)) => {
                    if skipped > 0 {
                        errors.push(format!(
                            "{series}/{status}: skipped {skipped} invalid record(s)"
                        ));
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
                }
            }
        }
    }
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
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    file.write_all(buf.as_bytes()).map_err(|e| e.to_string())?;
    Ok((written, errors))
}

fn load_snapshots(path: &Path) -> Vec<Snapshot> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Snapshot>(l).ok())
        .collect()
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
    unit: Arc<rune::Unit>,
    runtime: Arc<rune::runtime::RuntimeContext>,
    /// When this strategy's scoring clock started (first call of a
    /// pass) — the aggregate bound beside the per-call budget.
    clock: std::cell::Cell<Option<std::time::Instant>>,
    /// Set once the aggregate allowance is spent: later calls in
    /// the pass degrade to no-entry without running the script.
    exhausted: std::cell::Cell<bool>,
    /// allow_entry calls that errored or returned a non-boolean:
    /// counted so a broken strategy reads as broken, not quiet.
    call_failures: std::cell::Cell<usize>,
}

impl Strategy {
    fn load(path: &Path) -> Result<Strategy, String> {
        let script = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        if script.len() > MAX_STRATEGY_BYTES {
            return Err(format!(
                "strategy file is {} bytes; the cap is {MAX_STRATEGY_BYTES}",
                script.len()
            ));
        }
        // Compiled-strategy cache keyed by (path, length, mtime):
        // compiling Rune from source on every request made every
        // surface pay it per hit. The Strategy handed out is always
        // fresh (its budget clock and failure counters are per
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
            if let Some((p, l, t, unit, runtime, name, side)) =
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
                    unit,
                    runtime,
                    clock: std::cell::Cell::new(None),
                    exhausted: std::cell::Cell::new(false),
                    call_failures: std::cell::Cell::new(0),
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
            .map_err(|e| format!("strategy {} failed to build: {e}", path.display()))?;
        let unit = Arc::new(unit);
        let mut strat = Strategy {
            name: String::new(),
            side: String::new(),
            unit,
            runtime,
            clock: std::cell::Cell::new(None),
            exhausted: std::cell::Cell::new(false),
            call_failures: std::cell::Cell::new(0),
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

    fn vm(&self) -> rune::Vm {
        rune::Vm::new(self.runtime.clone(), self.unit.clone())
    }

    /// Every strategy call runs under an instruction budget. A
    /// strategy is a small predicate over three numbers; a million
    /// VM instructions is generous headroom for any honest shape,
    /// and a script that never terminates (a while loop in
    /// allow_entry) burns the budget and errors — degrading to
    /// no-entry like any other failed call — instead of wedging
    /// every scoring surface forever.
    fn call_budgeted(
        &self,
        name: &str,
        args: (f64, f64, i64),
    ) -> Result<rune::Value, rune::runtime::VmError> {
        let mut vm = self.vm();
        rune::runtime::budget::with(STRATEGY_BUDGET, move || vm.call([name], args)).call()
    }

    fn call_string(&self, name: &str) -> Result<String, String> {
        let mut vm = self.vm();
        let value = rune::runtime::budget::with(STRATEGY_BUDGET, move || vm.call([name], ()))
            .call()
            .map_err(|e| format!("strategy call {name} failed: {e}"))?;
        rune::from_value::<String>(value).map_err(|e| e.to_string())
    }

    /// The aggregate bound: a strategy gets STRATEGY_DEADLINE_SECS
    /// of wall clock per scoring pass (a pass builds fresh Strategy
    /// objects). An honest predicate needs microseconds per call;
    /// the allowance exists so a pathological script's cost is
    /// bounded by seconds per surface, not by tape size.
    fn budget_open(&self) -> bool {
        if self.exhausted.get() {
            return false;
        }
        let start = self.clock.get().unwrap_or_else(|| {
            let now = std::time::Instant::now();
            self.clock.set(Some(now));
            now
        });
        if start.elapsed().as_secs() >= STRATEGY_DEADLINE_SECS {
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

#[derive(Serialize)]
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

/// The entry a strategy would have taken in one window: the ask on its
/// side at the first snapshot (in fetch order) where its Rune
/// `allow_entry` says yes. None when it never enters.
fn window_entry(strat: &Strategy, snaps: &[Snapshot]) -> Option<f64> {
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
            if (0.01..=0.99).contains(&price) {
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
enum WindowFacts {
    Unsettled,
    Conflict,
    Settled { result: String, close_epoch: i64 },
}

fn window_facts(snaps: &[Snapshot]) -> WindowFacts {
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

fn score_strategy(strat: &Strategy, snapshots: &[Snapshot], standard: Standard) -> ScoreReport {
    // Group snapshots into market windows.
    let windows = group_windows(snapshots);
    // (close_epoch, pnl per contract) for every settled window we entered.
    let mut trades: Vec<(i64, f64)> = Vec::new();
    let mut settled = 0usize;
    let mut conflicted = 0usize;
    for (_ticker, snaps) in windows {
        let (result, close_epoch) = match window_facts(&snaps) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict => {
                conflicted += 1;
                continue;
            }
            WindowFacts::Unsettled => continue,
        };
        settled += 1;
        let Some(price) = window_entry(strat, &snaps) else {
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

#[derive(Serialize)]
struct Trade {
    close_epoch: i64,
    slot: usize,
    strategy: String,
    price: f64,
    contracts: i64,
    pnl_dollars: f64,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
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

#[derive(Serialize)]
struct StrategyTotal {
    strategy: String,
    windows_traded: usize,
    paper_pnl_dollars: f64,
}

#[derive(Serialize)]
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

fn simulate_slots(strategies: &[Strategy], snapshots: &[Snapshot], paper_pot: f64) -> SlotSimulation {
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
    for (_ticker, snaps) in group_windows(snapshots) {
        let (result, close_epoch) = match window_facts(&snaps) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict => {
                conflicted_windows += 1;
                continue;
            }
            WindowFacts::Unsettled => continue,
        };
        let entries = strategies.iter().map(|s| window_entry(s, &snaps)).collect();
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
fn equity_curves(sim: &SlotSimulation) -> (Vec<(i64, f64)>, Vec<(String, Vec<(i64, f64)>)>) {
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
/// Wall-clock allowance per strategy per scoring pass (see
/// Strategy::budget_open).
const STRATEGY_DEADLINE_SECS: u64 = 3;
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
        format!("${:.2}", v)
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
    let snapshots = app.snapshots_cached(&app.data_dir.join("snapshots.jsonl"));
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
            "Collector: {} — {} snapshots on disk across {} markets, {} distinct settled windows; newest snapshot {}.",
            if collecting { "collecting" } else { "NOT collecting" },
            health["snapshots_on_disk"],
            health["markets_seen"],
            health["settled_windows"],
            age_text
        ),
    ];
    if !collecting {
        lines.push("  Uncollected windows cannot be backfilled — a stopped collector is lost evidence.".to_string());
    }
    // The slot system over the tape, on paper.
    let strategies = load_strategies(&app.strategies_dir);
    let sim = simulate_slots(&strategies, &snapshots, app.paper_pot);
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
    lines.push(
        "Strategy verdicts (paper, forward tape; promotion needs 100+ windows, both halves positive, +3.5c/contract after fees):"
            .to_string(),
    );
    for strat in &strategies {
        let report = score_strategy(strat, &snapshots, Standard::default());
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
    /// Parsed-tape cache keyed by (path, byte length, mtime): the
    /// tape grows ~700 lines a pass and every surface used to
    /// re-parse all of it per request. The key IS the file state,
    /// so a hit is the file's current content by construction.
    tape_cache: std::sync::Mutex<Option<(PathBuf, u64, u64, Vec<Snapshot>)>>,
}

impl App {
    fn snapshots_cached(&self, path: &Path) -> Vec<Snapshot> {
        let key = std::fs::metadata(path).ok().map(|m| {
            (
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            )
        });
        if let Some((len, mtime)) = key {
            let guard = self.tape_cache.lock().unwrap();
            if let Some((p, l, t, snaps)) = &*guard {
                if p == path && *l == len && *t == mtime {
                    return snaps.clone();
                }
            }
        }
        let snaps = load_snapshots(path);
        if let Some((len, mtime)) = key {
            *self.tape_cache.lock().unwrap() =
                Some((path.to_path_buf(), len, mtime, snaps.clone()));
        }
        snaps
    }
}

/// Collector health, computed from the tape itself — the same numbers
/// an independent count of the file gives.
fn collector_health(app: &App) -> serde_json::Value {
    let tape_path = app.data_dir.join("snapshots.jsonl");
    let total_lines = std::fs::File::open(&tape_path)
        .map(|f| std::io::BufReader::new(f).lines().count())
        .unwrap_or(0);
    let snapshots = app.snapshots_cached(&tape_path);
    let mut markets: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut settled: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut results_by_ticker: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
        std::collections::HashMap::new();
    for s in &snapshots {
        markets.insert(s.ticker.as_str());
        if !s.result.is_empty() {
            settled.insert(s.ticker.as_str());
            results_by_ticker
                .entry(s.ticker.as_str())
                .or_default()
                .insert(s.result.as_str());
        }
    }
    let conflicting = results_by_ticker.values().filter(|r| r.len() > 1).count();
    // The tape is append-ordered: the last line is the newest
    // pass's stamp. (A max() over fetched_at would let one
    // future-dated line anywhere in the tape certify a dead
    // collector as collecting, indefinitely.)
    let newest = snapshots.last().map(|s| s.fetched_at);
    let now = now_unix();
    let age = newest.map(|n| (now - n).max(0));
    let future_dated = newest.map_or(false, |n| n > now + 300);
    let collecting = !future_dated
        && age.map_or(false, |a| a <= (app.collect_secs.max(60) * 2) as i64);
    serde_json::json!({
        "snapshots_on_disk": total_lines,
        "dropped_lines": total_lines.saturating_sub(snapshots.len()),
        "markets_seen": markets.len(),
        "settled_windows": settled.len(),
        "conflicting_settlements": conflicting,
        "newest_snapshot_age_secs": age,
        "collect_interval_secs": app.collect_secs,
        "collecting": collecting,
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
        if !name.is_empty() {
            return app.data_dir.join(format!("{name}-snapshots.jsonl"));
        }
    }
    app.data_dir.join("snapshots.jsonl")
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

fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String, String)> {
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
    loop {
        let line = read_line_by_byte(&mut reader)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Some(("__too_large__".to_string(), String::new(), String::new()));
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
    Some((method, path, String::from_utf8_lossy(&body).to_string()))
}

fn latest_markets(app: &App) -> Vec<Snapshot> {
    let snapshots = app.snapshots_cached(&app.data_dir.join("snapshots.jsonl"));
    let mut latest: BTreeMap<String, Snapshot> = BTreeMap::new();
    for snap in snapshots {
        latest
            .entry(snap.ticker.clone())
            .and_modify(|e| {
                if snap.fetched_at >= e.fetched_at {
                    *e = snap.clone();
                }
            })
            .or_insert(snap);
    }
    let mut out: Vec<Snapshot> = latest.into_values().collect();
    out.sort_by(|a, b| a.close_time.cmp(&b.close_time));
    out
}

fn handle(app: &App, method: &str, path: &str) -> String {
    let (route, query) = match path.split_once('?') {
        Some((r, q)) => (r, q),
        None => (path, ""),
    };
    match (method, route) {
        ("GET", "/") => html_response(include_str!("../assets/index.html")),
        ("GET", "/api/home") => {
            let home = serde_json::json!({
                "account": account_state(),
                "collector": collector_health(app),
                "library_errors": load_strategies_with_errors(&app.strategies_dir)
                    .1
                    .iter()
                    .map(|(file, error)| serde_json::json!({ "file": file, "error": error }))
                    .collect::<Vec<_>>(),
                "mode": "shadow — this build never places orders",
                "series": SERIES,
            });
            json_response(200, &home.to_string())
        }
        ("GET", "/api/markets") => {
            json_response(200, &serde_json::to_string(&latest_markets(app)).unwrap())
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
            let snapshots = app.snapshots_cached(&dpath);
            let reports: Vec<ScoreReport> = load_strategies(&app.strategies_dir)
                .iter()
                .map(|s| score_strategy(s, &snapshots, Standard::default()))
                .collect();
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
            let snapshots = app.snapshots_cached(&dpath);
            let strategies = load_strategies(&app.strategies_dir);
            let sim = simulate_slots(&strategies, &snapshots, app.paper_pot);
            let mut body = serde_json::to_value(&sim).unwrap();
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
            let snapshots = app.snapshots_cached(&dpath);
            let strategies = load_strategies(&app.strategies_dir);
            let sim = simulate_slots(&strategies, &snapshots, app.paper_pot);
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

fn main() {
    let port: u16 = match std::env::var("PORT") {
        Ok(p) => p.parse().unwrap_or_else(|_| {
            panic!("PORT is set to '{p}', which is not a valid port — refusing to silently fall back to 8472")
        }),
        Err(_) => 8472,
    };
    let data_dir = std::env::var("OPENBELL_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./openbell-data"));
    let strategies_dir = std::env::var("OPENBELL_STRATEGIES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./strategies"));
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
        data_dir,
        strategies_dir,
        paper_pot,
        collect_secs,
    });
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind failed");
    println!("openbell-rs listening on http://127.0.0.1:{port} (shadow mode — no order path exists)");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                    if let Some((method, path, _body)) = read_request(&mut stream) {
                        if method == "__too_large__" {
                            let resp = "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/json\r\n\r\n{\"error\":\"request body too large\"}";
                            let _ = stream.write_all(resp.as_bytes());
                            return;
                        }
                        let response = handle(&app, &method, &path);
                        let _ = stream.write_all(response.as_bytes());
                    }
                });
            }
            Err(_) => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        for i in 0..n {
            let close = format!("2026-11-17T{:02}:00:00Z", 10 + i);
            let close_epoch = parse_iso_z(&close).unwrap();
            snaps.push(snap(&format!("W{i}"), close_epoch - 240, &close, "", Some(0.70), Some(0.32)));
            snaps.push(snap(&format!("W{i}"), close_epoch + 60, &close, results[i], None, None));
        }
        snaps
    }

    #[test]
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
            let close = format!("2026-11-17T1{}:00:00Z", i);
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
}
