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
    Some(days_from_civil(y, mo, da) * 86400 + hh * 3600 + mm * 60 + ss)
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

fn fetch_markets(series: &str, status: &str) -> Result<Vec<Snapshot>, String> {
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
    let mut snaps = Vec::new();
    if let Some(markets) = body.get("markets").and_then(|m| m.as_array()) {
        for m in markets {
            let get = |k: &str| m.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
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
    Ok(snaps)
}

/// One collector pass over every series the desk trades: open markets (the
/// forward tape) plus settled markets (so windows get their results).
/// Appends JSONL snapshots to disk. Returns how many snapshots were written.
fn collect_once(data_dir: &Path) -> Result<usize, String> {
    let _ = std::fs::create_dir_all(data_dir);
    let path = data_dir.join("snapshots.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut written = 0;
    for series in SERIES {
        for status in ["open", "settled"] {
            match fetch_markets(series, status) {
                Ok(snaps) => {
                    for snap in &snaps {
                        writeln!(file, "{}", serde_json::to_string(snap).unwrap())
                            .map_err(|e| e.to_string())?;
                        written += 1;
                    }
                }
                Err(e) => eprintln!("collect {series}/{status}: {e}"),
            }
        }
    }
    Ok(written)
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
}

impl Strategy {
    fn load(path: &Path) -> Result<Strategy, String> {
        let script = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
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
        };
        strat.name = strat.call_string("name")?;
        strat.side = strat.call_string("side")?;
        Ok(strat)
    }

    fn vm(&self) -> rune::Vm {
        rune::Vm::new(self.runtime.clone(), self.unit.clone())
    }

    fn call_string(&self, name: &str) -> Result<String, String> {
        let value = self
            .vm()
            .call([name], ())
            .map_err(|e| format!("strategy call {name} failed: {e}"))?;
        rune::from_value::<String>(value).map_err(|e| e.to_string())
    }

    fn allow_entry(&self, yes_ask: f64, no_ask: f64, secs_to_close: i64) -> bool {
        match self.vm().call(["allow_entry"], (yes_ask, no_ask, secs_to_close)) {
            Ok(value) => rune::from_value::<bool>(value).unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Serialize)]
struct ScoreReport {
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
            return Some(if strat.side == "yes" { ya } else { na });
        }
    }
    None
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
    for (_ticker, snaps) in windows {
        let result = snaps
            .iter()
            .rev()
            .find(|s| !s.result.is_empty())
            .map(|s| s.result.clone());
        let Some(result) = result else { continue };
        settled += 1;
        let Some(price) = window_entry(strat, &snaps) else {
            continue;
        };
        if price <= 0.0 || price >= 1.0 {
            continue;
        }
        let payoff = if result == strat.side { 1.0 } else { 0.0 };
        let pnl = payoff - price - fee(price);
        let close_epoch = parse_iso_z(&snaps[0].close_time).unwrap_or(0);
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
    for (_ticker, snaps) in group_windows(snapshots) {
        let Some(result) = snaps
            .iter()
            .rev()
            .find(|s| !s.result.is_empty())
            .map(|s| s.result.clone())
        else {
            continue;
        };
        let close_epoch = parse_iso_z(&snaps[0].close_time).unwrap_or(0);
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
    (pot, per.into_iter().collect())
}

fn load_strategies(dir: &Path) -> Vec<Strategy> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "rn").unwrap_or(false))
            .collect();
        paths.sort();
        for path in paths {
            match Strategy::load(&path) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("strategy load failed: {e}"),
            }
        }
    }
    out
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
    match (key_id, key_path) {
        (Some(_), Some(p)) if Path::new(&p).exists() => AccountState {
            status: "unavailable".to_string(),
            balance: None,
            open_positions: None,
            day_pnl: None,
            note: "A credential is present in the environment, but signed venue reads are not enabled in this build — figures are shown as unavailable rather than guessed."
                .to_string(),
        },
        _ => AccountState {
            status: "unavailable".to_string(),
            balance: None,
            open_positions: None,
            day_pnl: None,
            note: "No venue credential in the environment — account figures are unavailable. Market data below is public venue data and needs no credential."
                .to_string(),
        },
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
        None => "Day P&L (venue): unavailable — no venue credential in the environment; nothing below invents one.".to_string(),
    };
    let snapshots = load_snapshots(&app.data_dir.join("snapshots.jsonl"));
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
}

/// Collector health, computed from the tape itself — the same numbers
/// an independent count of the file gives.
fn collector_health(app: &App) -> serde_json::Value {
    let snapshots = load_snapshots(&app.data_dir.join("snapshots.jsonl"));
    let mut markets: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut settled: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut newest: Option<i64> = None;
    for s in &snapshots {
        markets.insert(s.ticker.as_str());
        if !s.result.is_empty() {
            settled.insert(s.ticker.as_str());
        }
        newest = Some(newest.map_or(s.fetched_at, |n| n.max(s.fetched_at)));
    }
    let age = newest.map(|n| (now_unix() - n).max(0));
    let collecting = age.map_or(false, |a| a <= (app.collect_secs.max(60) * 2) as i64);
    serde_json::json!({
        "snapshots_on_disk": snapshots.len(),
        "markets_seen": markets.len(),
        "settled_windows": settled.len(),
        "newest_snapshot_age_secs": age,
        "collect_interval_secs": app.collect_secs,
        "collecting": collecting,
    })
}

/// Resolve the `dataset=` query parameter to a tape file in the data
/// dir. Names are restricted to [a-z0-9-] so the parameter can never
/// walk out of the data dir. No parameter means the live collected tape.
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

fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String, String)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some((method, path, String::from_utf8_lossy(&body).to_string()))
}

fn latest_markets(data_dir: &Path) -> Vec<Snapshot> {
    let snapshots = load_snapshots(&data_dir.join("snapshots.jsonl"));
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
                "mode": "shadow — this build never places orders",
                "series": SERIES,
            });
            json_response(200, &home.to_string())
        }
        ("GET", "/api/markets") => {
            json_response(200, &serde_json::to_string(&latest_markets(&app.data_dir)).unwrap())
        }
        ("POST", "/api/collect") => match collect_once(&app.data_dir) {
            Ok(n) => json_response(200, &format!("{{\"snapshots_written\":{n}}}")),
            Err(e) => json_response(502, &format!("{{\"error\":\"{e}\"}}")),
        },
        ("GET", "/api/strategies") => {
            let snapshots = load_snapshots(&dataset_path(app, query));
            let reports: Vec<ScoreReport> = load_strategies(&app.strategies_dir)
                .iter()
                .map(|s| score_strategy(s, &snapshots, Standard::default()))
                .collect();
            json_response(200, &serde_json::to_string(&reports).unwrap())
        }
        ("GET", "/api/slots") => {
            let snapshots = load_snapshots(&dataset_path(app, query));
            let strategies = load_strategies(&app.strategies_dir);
            let sim = simulate_slots(&strategies, &snapshots, app.paper_pot);
            json_response(200, &serde_json::to_string(&sim).unwrap())
        }
        ("GET", "/api/equity") => {
            let snapshots = load_snapshots(&dataset_path(app, query));
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
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8472);
    let data_dir = std::env::var("OPENBELL_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./openbell-data"));
    let strategies_dir = std::env::var("OPENBELL_STRATEGIES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./strategies"));
    let paper_pot: f64 = std::env::var("OPENBELL_PAPER_POT")
        .ok()
        .and_then(|p| p.parse().ok())
        .filter(|p: &f64| *p > 0.0)
        .unwrap_or(100.0);
    let _ = std::fs::create_dir_all(&data_dir);

    // Forward collector: one pass now, then on an interval.
    let collect_secs: u64 = std::env::var("OPENBELL_COLLECT_SECS")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(60);
    {
        let data_dir = data_dir.clone();
        std::thread::spawn(move || loop {
            match collect_once(&data_dir) {
                Ok(n) => println!("collector: {n} snapshots written"),
                Err(e) => eprintln!("collector: {e}"),
            }
            std::thread::sleep(std::time::Duration::from_secs(collect_secs));
        });
    }

    let app = Arc::new(App {
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
                    if let Some((method, path, _body)) = read_request(&mut stream) {
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
