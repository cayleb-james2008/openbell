//! Paper Strategy Tournament, 12x12 Return Correlation Matrix, Optimal
//! Uncorrelated 4-Slot Lineup & 2D Edge-by-Regime Heatmap (strictly paper-only).
//!
//! Three quantitative diagnostics over collected forward 15-minute tapes:
//! 1. **Out-of-Sample Strategy Tournament:** ranks all loaded paper strategies
//!    strictly on chronological Out-of-Sample (last 40%) net expectancy after
//!    fees and slippage, with Sharpe, Sortino, Max Drawdown %, Wilson CIs,
//!    sample-depth honesty warnings, and per-series breakdown (BTC/ETH/SOL/XRP).
//! 2. **Strategy Return Correlation Matrix & Optimal Uncorrelated 4-Slot Lineup:**
//!    computes exact pairwise Pearson correlation across shared settled windows,
//!    then finds the exact highest-expectancy subset of up to 4 positive-OOS
//!    strategies satisfying pairwise `|r| <= 0.65`, with explicit honest
//!    fallback disclosure whenever fewer than 4 strategies qualify.
//! 3. **2D Edge-by-Regime Heatmap:** Entry Price Bucket (`10-30c`, `30-50c`,
//!    `50-70c`, `70-90c`, `90-99c`) x Seconds-to-Close (`>600s`, `300-600s`,
//!    `120-300s`, `<120s`) reporting `n`, realized win rate, average fee,
//!    net paper expectancy (cents/contract), and Wilson sample-depth warnings.

#![allow(dead_code)]

use crate::analytics::{
    compute_return_ratios, is_valid_whole_cent, wilson_interval_95,
};
use crate::{
    fee, is_budget_halt, parse_iso_z, window_facts_scan, window_fingerprint, Snapshot, Strategy,
    WindowFacts, BUDGET_DEATH_LIMIT, STRATEGY_BUDGET, STRATEGY_DEADLINE_SECS,
    STRATEGY_MEMORY_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Maximum allowed absolute pairwise Pearson correlation inside the optimal 4-slot lineup.
pub const CORRELATION_CAP: f64 = 0.65;

/// Target number of uncorrelated positive-OOS slots in the recommended lineup.
pub const TARGET_LINEUP_SLOTS: usize = 4;

/// Canonical 2D regime price bucket labels.
pub const PRICE_BUCKET_LABELS: [&str; 5] = ["10-30c", "30-50c", "50-70c", "70-90c", "90-99c"];

/// Canonical 2D regime seconds-to-close bucket labels.
pub const TIME_BUCKET_LABELS: [&str; 4] = [">600s", "300-600s", "120-300s", "<120s"];

/// Cache mapping `(strategy_fingerprint, window_fingerprint)` -> `Option<(entry_price, secs_to_close)>`
/// so repeated tournament builds over append-only tapes avoid re-scanning settled windows.
type TournamentEntryCache = Mutex<HashMap<(u64, u64), Option<(f64, i64)>>>;
static TOURNAMENT_ENTRY_CACHE: LazyLock<TournamentEntryCache> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[inline]
fn finite_opt(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// Map a Kalshi 15m series ticker (`KXBTC15M`, `KXETH15M`, ...) to its short asset label.
pub fn short_asset_label(series: &str) -> String {
    let up = series.trim().to_ascii_uppercase();
    if up.starts_with("KXBTC") || up == "BTC" {
        "BTC".to_string()
    } else if up.starts_with("KXETH") || up == "ETH" {
        "ETH".to_string()
    } else if up.starts_with("KXSOL") || up == "SOL" {
        "SOL".to_string()
    } else if up.starts_with("KXXRP") || up == "XRP" {
        "XRP".to_string()
    } else if up.starts_with("KXDOGE") || up == "DOGE" {
        "DOGE".to_string()
    } else if up.is_empty() {
        "UNKNOWN".to_string()
    } else {
        up
    }
}

/// Classify a whole-cent entry price into one of the 5 heatmap price buckets (`0..5`),
/// returning `None` if below 10c (`< 0.10`).
#[inline]
pub fn price_bucket_index(price: f64) -> Option<usize> {
    if !price.is_finite() {
        return None;
    }
    let cents = (price * 100.0).round() as i64;
    if !(10..=99).contains(&cents) {
        return None;
    }
    Some(match cents {
        10..=29 => 0,
        30..=49 => 1,
        50..=69 => 2,
        70..=89 => 3,
        _ => 4,
    })
}

/// Classify `secs_to_close` into one of the 4 heatmap time buckets (`0..4`).
#[inline]
pub fn time_bucket_index(secs_to_close: i64) -> usize {
    if secs_to_close > 600 {
        0
    } else if secs_to_close >= 300 {
        1
    } else if secs_to_close >= 120 {
        2
    } else {
        3
    }
}

// ---------------------------------------------------------------------------
// Public Data Types (all serde-clean, zero NaN/Inf)
// ---------------------------------------------------------------------------

/// One settled strategy entry recorded with exact `secs_to_close` for regime analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TournamentEntry {
    pub close_epoch: i64,
    pub ticker: String,
    pub series: String,
    pub strategy_idx: usize,
    pub strategy: String,
    pub side: String,
    pub entry_price: f64,
    pub secs_to_close: i64,
    pub won: bool,
}

/// Strategy identity metadata stored inside `TournamentTapeContext`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyMeta {
    pub name: String,
    pub side: String,
}

/// Pre-scanned thread-safe tape context (`Send + Sync`) so `GET /api/analytics/tournament`
/// can singleflight-cache window scans and recompute any slippage setting in milliseconds.
#[derive(Debug, Clone)]
pub struct TournamentTapeContext {
    pub strategies: Vec<StrategyMeta>,
    pub windows_settled_total: usize,
    pub windows_settled_is: usize,
    pub windows_settled_oos: usize,
    pub windows_conflicted: usize,
    pub split_epoch: Option<i64>,
    pub observed_series: Vec<String>,
    pub entries: Vec<TournamentEntry>,
    pub window_vol_scores: HashMap<String, f64>,
}

/// Compute observable pre-settlement microstructure turbulence (mean quote spread + price range in cents).
fn compute_window_vol_score(snaps: &[Snapshot]) -> f64 {
    let mut spread_sum = 0.0f64;
    let mut spread_n = 0usize;
    let mut min_mid = f64::INFINITY;
    let mut max_mid = f64::NEG_INFINITY;
    for s in snaps {
        if let (Some(yb), Some(ya)) = (s.yes_bid.get(), s.yes_ask.get()) {
            if ya >= yb {
                spread_sum += (ya - yb) * 100.0;
                spread_n += 1;
                let mid = (yb + ya) * 50.0;
                if mid < min_mid {
                    min_mid = mid;
                }
                if mid > max_mid {
                    max_mid = mid;
                }
            }
        } else if let Some(lp) = s.last_price.get() {
            let mid = lp * 100.0;
            if mid < min_mid {
                min_mid = mid;
            }
            if mid > max_mid {
                max_mid = mid;
            }
        }
    }
    let mean_spread = if spread_n > 0 {
        spread_sum / (spread_n as f64)
    } else {
        2.0
    };
    let price_range = if min_mid.is_finite() && max_mid >= min_mid {
        max_mid - min_mid
    } else {
        0.0
    };
    mean_spread + 0.35 * price_range
}

/// Per-series (BTC/ETH/SOL/XRP) OOS performance breakdown for one strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesBreakdown {
    pub series: String,
    pub asset: String,
    pub oos_trades: usize,
    pub oos_wins: usize,
    pub oos_win_rate: Option<f64>,
    pub oos_net_expectancy_cents: Option<f64>,
    pub oos_net_pnl_dollars_1ct: f64,
}

/// One ranked strategy row in the Out-of-Sample Strategy Tournament.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyTournamentRow {
    pub rank: usize,
    pub strategy: String,
    pub side: String,
    pub total_trades: usize,
    pub is_trades: usize,
    pub oos_trades: usize,
    pub oos_wins: usize,
    pub oos_losses: usize,
    pub oos_win_rate: Option<f64>,
    pub oos_wilson_lower_95: Option<f64>,
    pub oos_wilson_upper_95: Option<f64>,
    pub oos_avg_entry_price: Option<f64>,
    pub oos_avg_fee_cents: Option<f64>,
    pub is_net_expectancy_cents: Option<f64>,
    /// Primary ranking metric: held-out OOS mean net P&L in cents/contract after fees & slippage.
    pub oos_net_expectancy_cents: Option<f64>,
    pub oos_net_pnl_dollars_1ct: f64,
    pub oos_compounded_pnl_dollars: f64,
    pub oos_sharpe: Option<f64>,
    pub oos_sortino: Option<f64>,
    pub oos_max_drawdown_pct: f64,
    pub oos_positive: bool,
    pub sample_warning: Option<String>,
    pub series_breakdown: Vec<SeriesBreakdown>,
}

/// Dense N x N pairwise Pearson return correlation matrix across shared settled windows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationMatrix {
    pub correlation_basis: String,
    pub strategies: Vec<String>,
    /// `matrix[i][j]` is Pearson `r` in `[-1.0, 1.0]`, or `None` when `< 2` shared
    /// settled windows or zero variance on the overlap.
    pub matrix: Vec<Vec<Option<f64>>>,
    /// Number of shared settled windows both strategies `i` and `j` entered.
    pub shared_windows: Vec<Vec<usize>>,
}

/// One strategy slot selected into the Optimal Uncorrelated 4-Slot Lineup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineupSlot {
    pub slot_number: usize,
    pub strategy: String,
    pub side: String,
    pub oos_trades: usize,
    pub oos_net_expectancy_cents: f64,
    pub oos_sharpe: Option<f64>,
    pub oos_max_drawdown_pct: f64,
    pub max_abs_corr_with_lineup: Option<f64>,
}

/// A positive-OOS candidate excluded from the lineup due to exceeding `|r| <= 0.65`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationConflict {
    pub excluded_strategy: String,
    pub conflicting_with: String,
    pub correlation: f64,
    pub shared_windows: usize,
}

/// Optimal uncorrelated 4-slot lineup (max OOS expectancy subject to pairwise `|r| <= 0.65`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimalLineup {
    pub target_slots: usize,
    pub correlation_cap: f64,
    pub positive_oos_candidates: usize,
    pub lineup_complete: bool,
    pub fallback_used: bool,
    pub fallback_disclosure: Option<String>,
    pub combined_oos_expectancy_cents: Option<f64>,
    pub max_abs_correlation: Option<f64>,
    pub mean_abs_correlation: Option<f64>,
    pub selected: Vec<LineupSlot>,
    pub rejected_for_correlation: Vec<CorrelationConflict>,
}

/// One cell of the 2D Entry Price x Seconds-to-Close regime heatmap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeCell {
    pub price_bucket_index: usize,
    pub price_label: String,
    pub time_bucket_index: usize,
    pub time_label: String,
    pub n: usize,
    pub wins: usize,
    pub realized_win_rate: Option<f64>,
    pub wilson_lower_95: Option<f64>,
    pub wilson_upper_95: Option<f64>,
    pub wilson_width: Option<f64>,
    pub avg_entry_price: Option<f64>,
    pub avg_fee_cents: Option<f64>,
    pub net_expectancy_cents: Option<f64>,
    pub total_pnl_dollars_1ct: f64,
    /// `"empty"` | `"insufficient"` (`<20`) | `"caution"` (`20..50`) | `"adequate"` (`>=50`).
    pub sample_depth_status: String,
    pub sample_warning: Option<String>,
}

/// Complete 5 x 4 (Entry Price x Seconds-to-Close) Edge-by-Regime Heatmap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeHeatmap {
    pub scope: String,
    pub slippage_cents: f64,
    pub price_buckets: Vec<String>,
    pub time_buckets: Vec<String>,
    /// Row-major `5 * 4 = 20` cells (`price_bucket_index * 4 + time_bucket_index`).
    pub cells: Vec<RegimeCell>,
    pub total_entries_in_grid: usize,
    pub excluded_below_10c: usize,
    pub total_entries_evaluated: usize,
    pub best_regime: Option<String>,
    pub worst_regime: Option<String>,
}

/// Top-level payload for `GET /api/analytics/tournament`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TournamentReport {
    pub mode_notice: String,
    pub slippage_cents: f64,
    pub correlation_cap: f64,
    pub windows_settled_total: usize,
    pub windows_settled_is: usize,
    pub windows_settled_oos: usize,
    pub windows_conflicted: usize,
    pub split_epoch: Option<i64>,
    pub observed_series: Vec<String>,
    pub rankings: Vec<StrategyTournamentRow>,
    pub correlation_matrix: CorrelationMatrix,
    pub optimal_lineup: OptimalLineup,
    pub regime_heatmap_oos: RegimeHeatmap,
    pub regime_heatmap_full: RegimeHeatmap,
}

// ---------------------------------------------------------------------------
// Fast Window Scanner (captures exact entry_price + secs_to_close with cache)
// ---------------------------------------------------------------------------

/// Scan one settled window for `(entry_price, secs_to_close)` using an already-borrowed
/// strategy VM, precomputed `allow_entry` hash, and exact whole-cent & budget/memory guards.
fn scan_window_with_vm(
    strat: &Strategy,
    vm: &mut rune::Vm,
    snaps: &[Snapshot],
    allow_entry_hash: rune::Hash,
) -> Option<(f64, i64)> {
    if !strat.budget_open() {
        return None;
    }

    let t0 = Instant::now();
    let is_yes = strat.side == "yes";
    let mut last_close_str: Option<&str> = None;
    let mut last_close_epoch: Option<i64> = None;
    let mut found: Option<(f64, i64)> = None;

    for snap in snaps {
        let (Some(ya), Some(na)) = (snap.yes_ask.get(), snap.no_ask.get()) else {
            continue;
        };
        let price = if is_yes { ya } else { na };
        if !is_valid_whole_cent(price) {
            continue;
        }
        let close_epoch = if last_close_str == Some(snap.close_time.as_ref()) {
            last_close_epoch
        } else {
            let parsed = parse_iso_z(&snap.close_time);
            last_close_str = Some(snap.close_time.as_ref());
            last_close_epoch = parsed;
            parsed
        };
        let Some(close) = close_epoch else {
            continue;
        };
        let secs_to_close = close - snap.fetched_at;

        let call_res = rune::runtime::budget::with(
            STRATEGY_BUDGET,
            rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || {
                vm.call(allow_entry_hash, (ya, na, secs_to_close))
            }),
        )
        .call();

        match call_res {
            Ok(val) => {
                strat.budget_deaths.set(0);
                match rune::from_value::<bool>(val) {
                    Ok(true) => {
                        let norm = (price * 100.0).round() / 100.0;
                        found = Some((norm, secs_to_close));
                        break;
                    }
                    Ok(false) => {}
                    Err(_) => {
                        strat.call_failures.set(strat.call_failures.get() + 1);
                    }
                }
            }
            Err(e) => {
                strat.call_failures.set(strat.call_failures.get() + 1);
                if is_budget_halt(&e) {
                    let deaths = strat.budget_deaths.get() + 1;
                    strat.budget_deaths.set(deaths);
                    if deaths >= BUDGET_DEATH_LIMIT {
                        strat.exhausted.set(true);
                        break;
                    }
                }
            }
        }
    }

    strat.spent.set(strat.spent.get() + t0.elapsed());
    if strat.spent.get().as_secs() >= STRATEGY_DEADLINE_SECS {
        strat.exhausted.set(true);
    }

    found
}

/// Build a reusable, thread-safe `TournamentTapeContext` from grouped tape windows
/// and loaded strategies (single strategy-outer pass with batched cache lookups).
pub fn build_tournament_context(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
) -> TournamentTapeContext {
    let allow_entry_hash = rune::Hash::type_hash(["allow_entry"]);

    struct SettledWin<'a> {
        ticker: &'a str,
        close_epoch: i64,
        result: String,
        series: String,
        snaps: &'a [Snapshot],
        win_fp: u64,
    }
    let mut settled_windows: Vec<SettledWin<'_>> = Vec::new();
    let mut conflicted = 0usize;
    let mut series_set: BTreeSet<String> = BTreeSet::new();

    for (ticker, snaps) in windows {
        let (result, close_epoch) = match window_facts_scan(snaps) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict => {
                conflicted += 1;
                continue;
            }
            WindowFacts::Unsettled => continue,
        };
        let series = snaps
            .first()
            .map(|s| s.series.to_string())
            .unwrap_or_default();
        if !series.is_empty() {
            series_set.insert(series.clone());
        }
        let win_fp = window_fingerprint(snaps);
        settled_windows.push(SettledWin {
            ticker: ticker.as_str(),
            close_epoch,
            result,
            series,
            snaps: snaps.as_slice(),
            win_fp,
        });
    }

    settled_windows.sort_by(|a, b| {
        a.close_epoch
            .cmp(&b.close_epoch)
            .then_with(|| a.ticker.cmp(b.ticker))
    });

    let mut epochs: Vec<i64> = settled_windows.iter().map(|w| w.close_epoch).collect();
    epochs.dedup();
    let (split_epoch, windows_settled_is, windows_settled_oos) = if epochs.len() < 2 {
        (epochs.first().copied(), settled_windows.len(), 0)
    } else {
        let is_epoch_cnt = ((epochs.len() * 3) / 5).max(1).min(epochs.len() - 1);
        let boundary = epochs[is_epoch_cnt];
        let cut = settled_windows.partition_point(|w| w.close_epoch < boundary);
        (Some(boundary), cut, settled_windows.len() - cut)
    };

    // Strategy-outer loop keeps one VM hot in cache and batches TOURNAMENT_ENTRY_CACHE
    // lookups/inserts to 2 mutex acquisitions per strategy instead of 24,000.
    let mut entries: Vec<TournamentEntry> = Vec::new();
    for (si, strat) in strategies.iter().enumerate() {
        let fp = strat.fingerprint;
        let cached: Vec<Option<Option<(f64, i64)>>> = {
            if let Ok(guard) = TOURNAMENT_ENTRY_CACHE.lock() {
                settled_windows
                    .iter()
                    .map(|w| guard.get(&(fp, w.win_fp)).copied())
                    .collect()
            } else {
                vec![None; settled_windows.len()]
            }
        };

        let mut new_cache_entries: Vec<(u64, Option<(f64, i64)>)> = Vec::new();
        strat.with_vm(|vm| {
            for (wi, w) in settled_windows.iter().enumerate() {
                let entry_opt = match cached[wi] {
                    Some(hit) => hit,
                    None => {
                        let computed = scan_window_with_vm(strat, vm, w.snaps, allow_entry_hash);
                        if !strat.budget_exhausted() {
                            new_cache_entries.push((w.win_fp, computed));
                        }
                        computed
                    }
                };
                if let Some((price, secs_to_close)) = entry_opt {
                    entries.push(TournamentEntry {
                        close_epoch: w.close_epoch,
                        ticker: w.ticker.to_string(),
                        series: w.series.clone(),
                        strategy_idx: si,
                        strategy: strat.name.clone(),
                        side: strat.side.clone(),
                        entry_price: price,
                        secs_to_close,
                        won: w.result == strat.side,
                    });
                }
            }
        });

        if !new_cache_entries.is_empty() {
            if let Ok(mut guard) = TOURNAMENT_ENTRY_CACHE.lock() {
                if guard.len() + new_cache_entries.len() > 250_000 {
                    guard.clear();
                }
                for (win_fp, val) in new_cache_entries {
                    guard.insert((fp, win_fp), val);
                }
            }
        }
    }
    entries.shrink_to_fit();

    let strategy_metas = strategies
        .iter()
        .map(|s| StrategyMeta {
            name: s.name.clone(),
            side: s.side.clone(),
        })
        .collect();

    let mut window_vol_scores = HashMap::with_capacity(settled_windows.len());
    for w in &settled_windows {
        window_vol_scores.insert(w.ticker.to_string(), compute_window_vol_score(w.snaps));
    }

    TournamentTapeContext {
        strategies: strategy_metas,
        windows_settled_total: settled_windows.len(),
        windows_settled_is,
        windows_settled_oos,
        windows_conflicted: conflicted,
        split_epoch,
        observed_series: series_set.into_iter().collect(),
        entries,
        window_vol_scores,
    }
}

// ---------------------------------------------------------------------------
// Core Math: Pairwise Pearson Correlation, Exact Optimal Lineup & 2D Heatmap
// ---------------------------------------------------------------------------

/// Exact Pearson correlation over paired observations `xs` and `ys`.
/// Returns `None` when `xs.len() < 2` or either series has zero variance.
pub fn pearson_correlation(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len().min(ys.len());
    if n < 2 {
        return None;
    }
    let n_f = n as f64;
    let mean_x = xs[..n].iter().sum::<f64>() / n_f;
    let mean_y = ys[..n].iter().sum::<f64>() / n_f;
    let mut cov = 0.0f64;
    let mut var_x = 0.0f64;
    let mut var_y = 0.0f64;
    for k in 0..n {
        let dx = xs[k] - mean_x;
        let dy = ys[k] - mean_y;
        cov += dx * dy;
        var_x += dx * dx;
        var_y += dy * dy;
    }
    let denom = (var_x * var_y).sqrt();
    if denom <= 1e-12 || !denom.is_finite() {
        return None;
    }
    finite_opt((cov / denom).clamp(-1.0, 1.0))
}

/// Compute symmetric N x N Pearson return correlation matrix across shared settled windows.
fn build_correlation_matrix(
    strategies: &[StrategyMeta],
    entries: &[TournamentEntry],
    slip_dollars: f64,
) -> CorrelationMatrix {
    let n = strategies.len();
    // Map each strategy index -> map<(close_epoch, ticker), net_return_on_cost>
    let mut per_strat: Vec<HashMap<(i64, &str), f64>> = vec![HashMap::new(); n];
    for e in entries {
        if e.strategy_idx >= n {
            continue;
        }
        let p = e.entry_price.clamp(0.01, 0.99);
        let f_ct = fee(p);
        let cost = (p + f_ct + slip_dollars).min(1.0);
        let payoff = if e.won { 1.0 } else { 0.0 };
        let net_pnl = payoff - p - f_ct - slip_dollars;
        let ret = if cost > 1e-9 { net_pnl / cost } else { 0.0 };
        per_strat[e.strategy_idx].insert((e.close_epoch, e.ticker.as_str()), ret);
    }

    let mut matrix = vec![vec![None; n]; n];
    let mut shared_windows = vec![vec![0usize; n]; n];

    for i in 0..n {
        matrix[i][i] = Some(1.0);
        shared_windows[i][i] = per_strat[i].len();
        for j in (i + 1)..n {
            // Iterate the smaller map for O(min(|A|, |B|)) overlap join.
            let (small, large, swapped) = if per_strat[i].len() <= per_strat[j].len() {
                (&per_strat[i], &per_strat[j], false)
            } else {
                (&per_strat[j], &per_strat[i], true)
            };
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for (k, &v1) in small {
                if let Some(&v2) = large.get(k) {
                    if swapped {
                        xs.push(v2);
                        ys.push(v1);
                    } else {
                        xs.push(v1);
                        ys.push(v2);
                    }
                }
            }
            let overlap = xs.len();
            let corr = pearson_correlation(&xs, &ys);
            matrix[i][j] = corr;
            matrix[j][i] = corr;
            shared_windows[i][j] = overlap;
            shared_windows[j][i] = overlap;
        }
    }

    CorrelationMatrix {
        correlation_basis: "shared_settled_windows".to_string(),
        strategies: strategies.iter().map(|s| s.name.clone()).collect(),
        matrix,
        shared_windows,
    }
}

/// Exact combinatorial search for the highest OOS-expectancy portfolio of up to
/// `TARGET_LINEUP_SLOTS` (4) positive-OOS strategies subject to pairwise `|r| <= cap`.
fn select_optimal_lineup(
    rankings: &[StrategyTournamentRow],
    corr: &CorrelationMatrix,
    cap: f64,
) -> OptimalLineup {
    let idx_by_name: HashMap<&str, usize> = corr
        .strategies
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();

    // Positive-OOS candidates already sorted by tournament rank (best expectancy first).
    let candidates: Vec<(&StrategyTournamentRow, usize)> = rankings
        .iter()
        .filter(|r| r.oos_positive)
        .filter_map(|r| idx_by_name.get(r.strategy.as_str()).map(|&ci| (r, ci)))
        .collect();

    let positive_count = candidates.len();
    if positive_count == 0 {
        return OptimalLineup {
            target_slots: TARGET_LINEUP_SLOTS,
            correlation_cap: cap,
            positive_oos_candidates: 0,
            lineup_complete: false,
            fallback_used: true,
            fallback_disclosure: Some(
                "0 strategies achieved positive out-of-sample net expectancy after fees and slippage. No 4-slot lineup is recommended — remain in paper research."
                    .to_string(),
            ),
            combined_oos_expectancy_cents: None,
            max_abs_correlation: None,
            mean_abs_correlation: None,
            selected: Vec::new(),
            rejected_for_correlation: Vec::new(),
        };
    }

    let pair_ok = |a_ci: usize, b_ci: usize| -> bool {
        match corr.matrix[a_ci][b_ci] {
            Some(r) => r.abs() <= cap + 1e-12,
            None => true, // Zero/insufficient window overlap -> disjoint firing regimes.
        }
    };

    // Exact DFS over subsets of size <= TARGET_LINEUP_SLOTS (at most C(12,4)=495 subsets).
    #[derive(Clone)]
    struct BestCombo {
        indices: Vec<usize>, // indices into `candidates`
        sum_exp: f64,
        max_abs_r: f64,
        sum_sharpe: f64,
    }

    fn dfs(
        start: usize,
        target_k: usize,
        cur: &mut Vec<usize>,
        candidates: &[(&StrategyTournamentRow, usize)],
        corr: &CorrelationMatrix,
        cap: f64,
        best: &mut Option<BestCombo>,
    ) {
        if cur.len() == target_k {
            let mut sum_exp = 0.0f64;
            let mut sum_sharpe = 0.0f64;
            let mut max_abs_r = 0.0f64;
            for &idx in cur.iter() {
                sum_exp += candidates[idx].0.oos_net_expectancy_cents.unwrap_or(0.0);
                sum_sharpe += candidates[idx].0.oos_sharpe.unwrap_or(0.0);
            }
            for a in 0..cur.len() {
                for b in (a + 1)..cur.len() {
                    let ca = candidates[cur[a]].1;
                    let cb = candidates[cur[b]].1;
                    if let Some(r) = corr.matrix[ca][cb] {
                        let ar = r.abs();
                        if ar > max_abs_r {
                            max_abs_r = ar;
                        }
                    }
                }
            }
            let cand_combo = BestCombo {
                indices: cur.clone(),
                sum_exp,
                max_abs_r,
                sum_sharpe,
            };
            let better = match best {
                None => true,
                Some(prev) => {
                    if (cand_combo.sum_exp - prev.sum_exp).abs() > 1e-9 {
                        cand_combo.sum_exp > prev.sum_exp
                    } else if (cand_combo.max_abs_r - prev.max_abs_r).abs() > 1e-9 {
                        cand_combo.max_abs_r < prev.max_abs_r
                    } else {
                        cand_combo.sum_sharpe > prev.sum_sharpe
                    }
                }
            };
            if better {
                *best = Some(cand_combo);
            }
            return;
        }
        let remaining_need = target_k - cur.len();
        if candidates.len() - start < remaining_need {
            return;
        }
        for i in start..candidates.len() {
            let ci = candidates[i].1;
            let compatible = cur.iter().all(|&prev_idx| {
                let prev_ci = candidates[prev_idx].1;
                match corr.matrix[prev_ci][ci] {
                    Some(r) => r.abs() <= cap + 1e-12,
                    None => true,
                }
            });
            if compatible {
                cur.push(i);
                dfs(i + 1, target_k, cur, candidates, corr, cap, best);
                cur.pop();
            }
        }
    }

    // Prefer the largest valid uncorrelated lineup up to TARGET_LINEUP_SLOTS (4 -> 3 -> 2 -> 1).
    let max_k = positive_count.min(TARGET_LINEUP_SLOTS);
    let mut chosen: Option<BestCombo> = None;
    for k in (1..=max_k).rev() {
        let mut cur = Vec::with_capacity(k);
        let mut best_k = None;
        dfs(0, k, &mut cur, &candidates, corr, cap, &mut best_k);
        if best_k.is_some() {
            chosen = best_k;
            break;
        }
    }

    let chosen_indices = chosen.map(|b| b.indices).unwrap_or_default();
    let selected_set: BTreeSet<usize> = chosen_indices.iter().copied().collect();

    // Build selected slot details & pairwise lineup correlation summary.
    let mut selected = Vec::with_capacity(chosen_indices.len());
    let mut pair_abs_rs: Vec<f64> = Vec::new();
    for (slot_pos, &cand_i) in chosen_indices.iter().enumerate() {
        let (row, ci) = candidates[cand_i];
        let mut max_r: Option<f64> = None;
        for &other_i in &chosen_indices {
            if other_i == cand_i {
                continue;
            }
            let other_ci = candidates[other_i].1;
            if let Some(r) = corr.matrix[ci][other_ci] {
                let ar = r.abs();
                max_r = Some(max_r.map_or(ar, |m| m.max(ar)));
                if cand_i < other_i {
                    pair_abs_rs.push(ar);
                }
            }
        }
        selected.push(LineupSlot {
            slot_number: slot_pos + 1,
            strategy: row.strategy.clone(),
            side: row.side.clone(),
            oos_trades: row.oos_trades,
            oos_net_expectancy_cents: row.oos_net_expectancy_cents.unwrap_or(0.0),
            oos_sharpe: row.oos_sharpe,
            oos_max_drawdown_pct: row.oos_max_drawdown_pct,
            max_abs_corr_with_lineup: max_r,
        });
    }

    // Record any positive-OOS candidate that conflicted with a selected slot (`|r| > cap`).
    let mut rejected_for_correlation = Vec::new();
    for (cand_i, &(row, ci)) in candidates.iter().enumerate() {
        if selected_set.contains(&cand_i) {
            continue;
        }
        let mut worst: Option<(&str, f64, usize)> = None;
        for &sel_i in &chosen_indices {
            let (sel_row, sel_ci) = candidates[sel_i];
            if !pair_ok(ci, sel_ci) {
                let r = corr.matrix[ci][sel_ci].unwrap_or(0.0);
                let sw = corr.shared_windows[ci][sel_ci];
                if worst.is_none_or(|(_, wr, _)| r.abs() > wr.abs()) {
                    worst = Some((sel_row.strategy.as_str(), r, sw));
                }
            }
        }
        if let Some((with_name, r, sw)) = worst {
            rejected_for_correlation.push(CorrelationConflict {
                excluded_strategy: row.strategy.clone(),
                conflicting_with: with_name.to_string(),
                correlation: r,
                shared_windows: sw,
            });
        }
    }

    let combined_exp = if !selected.is_empty() {
        Some(
            selected
                .iter()
                .map(|s| s.oos_net_expectancy_cents)
                .sum::<f64>()
                / selected.len() as f64,
        )
    } else {
        None
    };
    let max_abs_r = pair_abs_rs.iter().copied().reduce(f64::max);
    let mean_abs_r = if !pair_abs_rs.is_empty() {
        Some(pair_abs_rs.iter().sum::<f64>() / pair_abs_rs.len() as f64)
    } else {
        None
    };

    let lineup_complete = selected.len() == TARGET_LINEUP_SLOTS;
    let fallback_used = !lineup_complete;
    let fallback_disclosure = if lineup_complete {
        None
    } else {
        let shortfall = TARGET_LINEUP_SLOTS - selected.len();
        if positive_count < TARGET_LINEUP_SLOTS {
            Some(format!(
                "Only {positive_count} of {} strategies achieved positive OOS net expectancy after fees and slippage ({} selected under |r| <= {:.2}). Keeping {shortfall} slot(s) unassigned rather than padding with negative-expectancy strategies.",
                rankings.len(),
                selected.len(),
                cap
            ))
        } else {
            Some(format!(
                "{positive_count} strategies achieved positive OOS net expectancy, but correlation clustering (|r| > {:.2}) capped the uncorrelated lineup at {} slot(s) ({} excluded for high correlation). Keeping {shortfall} slot(s) unassigned.",
                cap,
                selected.len(),
                rejected_for_correlation.len()
            ))
        }
    };

    OptimalLineup {
        target_slots: TARGET_LINEUP_SLOTS,
        correlation_cap: cap,
        positive_oos_candidates: positive_count,
        lineup_complete,
        fallback_used,
        fallback_disclosure,
        combined_oos_expectancy_cents: combined_exp,
        max_abs_correlation: max_abs_r,
        mean_abs_correlation: mean_abs_r,
        selected,
        rejected_for_correlation,
    }
}

/// Build the 5 x 4 (Entry Price Bucket x Seconds-to-Close) 2D Regime Heatmap.
fn build_regime_heatmap(
    scope: &str,
    entries: &[&TournamentEntry],
    slippage_cents: f64,
) -> RegimeHeatmap {
    let slip_dollars = slippage_cents / 100.0;
    const NUM_P: usize = 5;
    const NUM_T: usize = 4;

    #[derive(Default, Clone)]
    struct Acc {
        n: usize,
        wins: usize,
        price_sum: f64,
        fee_sum: f64,
        pnl_sum: f64,
    }
    let mut grid = vec![Acc::default(); NUM_P * NUM_T];
    let mut excluded_below_10c = 0usize;

    for e in entries {
        let p = e.entry_price.clamp(0.01, 0.99);
        let Some(pi) = price_bucket_index(p) else {
            excluded_below_10c += 1;
            continue;
        };
        let ti = time_bucket_index(e.secs_to_close);
        let f_ct = fee(p);
        let payoff = if e.won { 1.0 } else { 0.0 };
        let net_pnl = payoff - p - f_ct - slip_dollars;

        let cell = &mut grid[pi * NUM_T + ti];
        cell.n += 1;
        if e.won {
            cell.wins += 1;
        }
        cell.price_sum += p;
        cell.fee_sum += f_ct;
        cell.pnl_sum += net_pnl;
    }

    let mut cells = Vec::with_capacity(NUM_P * NUM_T);
    let mut total_in_grid = 0usize;
    let mut best_cell: Option<(String, f64, usize)> = None;
    let mut worst_cell: Option<(String, f64, usize)> = None;

    for pi in 0..NUM_P {
        for ti in 0..NUM_T {
            let acc = &grid[pi * NUM_T + ti];
            total_in_grid += acc.n;
            let p_label = PRICE_BUCKET_LABELS[pi];
            let t_label = TIME_BUCKET_LABELS[ti];

            if acc.n == 0 {
                cells.push(RegimeCell {
                    price_bucket_index: pi,
                    price_label: p_label.to_string(),
                    time_bucket_index: ti,
                    time_label: t_label.to_string(),
                    n: 0,
                    wins: 0,
                    realized_win_rate: None,
                    wilson_lower_95: None,
                    wilson_upper_95: None,
                    wilson_width: None,
                    avg_entry_price: None,
                    avg_fee_cents: None,
                    net_expectancy_cents: None,
                    total_pnl_dollars_1ct: 0.0,
                    sample_depth_status: "empty".to_string(),
                    sample_warning: None,
                });
                continue;
            }

            let n_f = acc.n as f64;
            let win_rate = acc.wins as f64 / n_f;
            let (wl, wu) = wilson_interval_95(acc.wins, acc.n).unwrap();
            let width = (wu - wl).clamp(0.0, 1.0);
            let avg_price = acc.price_sum / n_f;
            let avg_fee_c = (acc.fee_sum / n_f) * 100.0;
            let exp_c = (acc.pnl_sum / n_f) * 100.0;

            let (status, warning) = if acc.n < 20 {
                (
                    "insufficient",
                    Some(format!(
                        "Low sample (n={}): 95% Wilson CI spans {:.1} pp — treat regime edge as noisy.",
                        acc.n,
                        width * 100.0
                    )),
                )
            } else if acc.n < 50 {
                (
                    "caution",
                    Some(format!(
                        "Moderate sample (n={}): 95% Wilson CI spans {:.1} pp.",
                        acc.n,
                        width * 100.0
                    )),
                )
            } else {
                ("adequate", None)
            };

            let tag = format!("{p_label} @ {t_label}");
            let rank_better = |cur: Option<&(String, f64, usize)>, is_best: bool| -> bool {
                match cur {
                    None => true,
                    Some((_, cur_exp, cur_n)) => {
                        let cur_sig = *cur_n >= 10;
                        let new_sig = acc.n >= 10;
                        if new_sig != cur_sig {
                            new_sig
                        } else if is_best {
                            exp_c > *cur_exp
                        } else {
                            exp_c < *cur_exp
                        }
                    }
                }
            };
            if rank_better(best_cell.as_ref(), true) {
                best_cell = Some((tag.clone(), exp_c, acc.n));
            }
            if rank_better(worst_cell.as_ref(), false) {
                worst_cell = Some((tag, exp_c, acc.n));
            }

            cells.push(RegimeCell {
                price_bucket_index: pi,
                price_label: p_label.to_string(),
                time_bucket_index: ti,
                time_label: t_label.to_string(),
                n: acc.n,
                wins: acc.wins,
                realized_win_rate: Some(win_rate),
                wilson_lower_95: Some(wl),
                wilson_upper_95: Some(wu),
                wilson_width: Some(width),
                avg_entry_price: Some(avg_price),
                avg_fee_cents: Some(avg_fee_c),
                net_expectancy_cents: Some(exp_c),
                total_pnl_dollars_1ct: acc.pnl_sum,
                sample_depth_status: status.to_string(),
                sample_warning: warning,
            });
        }
    }

    RegimeHeatmap {
        scope: scope.to_string(),
        slippage_cents,
        price_buckets: PRICE_BUCKET_LABELS.iter().map(|s| (*s).to_string()).collect(),
        time_buckets: TIME_BUCKET_LABELS.iter().map(|s| (*s).to_string()).collect(),
        cells,
        total_entries_in_grid: total_in_grid,
        excluded_below_10c,
        total_entries_evaluated: entries.len(),
        best_regime: best_cell.map(|(lbl, exp, n)| format!("{lbl} ({exp:+.2}c/ct, n={n})")),
        worst_regime: worst_cell.map(|(lbl, exp, n)| format!("{lbl} ({exp:+.2}c/ct, n={n})")),
    }
}

/// Compute the full `TournamentReport` from a pre-built `TournamentTapeContext`
/// at any `slippage_cents` in `0.0..=5.0` (takes < 10ms even on 2,000+ windows).
pub fn compute_tournament_from_context(
    ctx: &TournamentTapeContext,
    slippage_cents: f64,
) -> TournamentReport {
    let slip_c = if slippage_cents.is_finite() {
        slippage_cents.clamp(0.0, 5.0)
    } else {
        1.0
    };
    let slip_dollars = slip_c / 100.0;

    let n_strats = ctx.strategies.len();
    let risk_fraction = 1.0 / (n_strats.max(4) as f64);

    // Partition entries into IS vs OOS using the single global chronological split_epoch.
    let mut is_by_strat: Vec<Vec<&TournamentEntry>> = vec![Vec::new(); n_strats];
    let mut oos_by_strat: Vec<Vec<&TournamentEntry>> = vec![Vec::new(); n_strats];
    let mut oos_entries_all: Vec<&TournamentEntry> = Vec::new();
    let mut full_entries_all: Vec<&TournamentEntry> = Vec::with_capacity(ctx.entries.len());

    for e in &ctx.entries {
        if e.strategy_idx >= n_strats {
            continue;
        }
        full_entries_all.push(e);
        let is_oos = match ctx.split_epoch {
            Some(boundary) if ctx.windows_settled_oos > 0 => e.close_epoch >= boundary,
            _ => false,
        };
        if is_oos {
            oos_by_strat[e.strategy_idx].push(e);
            oos_entries_all.push(e);
        } else {
            is_by_strat[e.strategy_idx].push(e);
        }
    }

    let mut rankings: Vec<StrategyTournamentRow> = Vec::with_capacity(n_strats);

    for (si, meta) in ctx.strategies.iter().enumerate() {
        let is_list = &is_by_strat[si];
        let mut oos_list = oos_by_strat[si].clone();
        oos_list.sort_by_key(|e| e.close_epoch);

        let is_exp_c = if !is_list.is_empty() {
            let sum: f64 = is_list
                .iter()
                .map(|e| {
                    let p = e.entry_price.clamp(0.01, 0.99);
                    let payoff = if e.won { 1.0 } else { 0.0 };
                    payoff - p - fee(p) - slip_dollars
                })
                .sum();
            finite_opt((sum / is_list.len() as f64) * 100.0)
        } else {
            None
        };

        let oos_n = oos_list.len();
        let total_n = is_list.len() + oos_n;
        let mut oos_wins = 0usize;
        let mut price_sum = 0.0f64;
        let mut fee_sum = 0.0f64;
        let mut pnl_1ct_sum = 0.0f64;

        // Per-series OOS rollup keyed by canonical series string.
        let mut by_series: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new();

        // Standalone OOS compounding for window-return Sharpe, Sortino, and Max DD %.
        let start_eq = 100.0f64;
        let mut eq = start_eq;
        let mut peak = start_eq;
        let mut max_dd_pct = 0.0f64;
        let mut win_returns: Vec<f64> = Vec::with_capacity(oos_n);

        // Group simultaneous OOS entries by close_epoch (multi-series windows can share epoch).
        let mut i = 0usize;
        while i < oos_list.len() {
            let epoch = oos_list[i].close_epoch;
            let prev_eq = eq;
            let mut epoch_pnl = 0.0f64;
            while i < oos_list.len() && oos_list[i].close_epoch == epoch {
                let e = oos_list[i];
                let p = e.entry_price.clamp(0.01, 0.99);
                let f_ct = fee(p);
                let payoff = if e.won { 1.0 } else { 0.0 };
                let pnl_1ct = payoff - p - f_ct - slip_dollars;

                if e.won {
                    oos_wins += 1;
                }
                price_sum += p;
                fee_sum += f_ct;
                pnl_1ct_sum += pnl_1ct;

                let s_key = if e.series.is_empty() {
                    "UNKNOWN".to_string()
                } else {
                    e.series.clone()
                };
                let s_acc = by_series.entry(s_key).or_insert((0, 0, 0.0));
                s_acc.0 += 1;
                if e.won {
                    s_acc.1 += 1;
                }
                s_acc.2 += pnl_1ct;

                if prev_eq > 1e-6 {
                    let contracts = (prev_eq * risk_fraction / p).floor() as i64;
                    if contracts > 0 {
                        epoch_pnl += contracts as f64 * pnl_1ct;
                    }
                }
                i += 1;
            }
            if prev_eq > 1e-6 {
                eq = (eq + epoch_pnl).max(0.0);
                let r = ((eq - prev_eq) / prev_eq).max(-1.0);
                if r.is_finite() {
                    win_returns.push(r);
                }
                if eq > peak {
                    peak = eq;
                }
                if peak > 1e-9 {
                    let dd = (((peak - eq).max(0.0) / peak) * 100.0).clamp(0.0, 100.0);
                    if dd > max_dd_pct {
                        max_dd_pct = dd;
                    }
                }
            }
        }

        let (oos_sharpe, oos_sortino, _) = compute_return_ratios(&win_returns);
        let oos_win_rate = if oos_n > 0 {
            Some(oos_wins as f64 / oos_n as f64)
        } else {
            None
        };
        let (wl, wu) = match wilson_interval_95(oos_wins, oos_n) {
            Some((l, u)) => (Some(l), Some(u)),
            None => (None, None),
        };
        let oos_avg_price = if oos_n > 0 {
            Some(price_sum / oos_n as f64)
        } else {
            None
        };
        let oos_avg_fee_c = if oos_n > 0 {
            Some((fee_sum / oos_n as f64) * 100.0)
        } else {
            None
        };
        let oos_exp_c = if oos_n > 0 {
            finite_opt((pnl_1ct_sum / oos_n as f64) * 100.0)
        } else {
            None
        };
        let oos_positive = oos_n > 0 && oos_exp_c.is_some_and(|c| c > 0.0);
        let sample_warning = if oos_n == 0 {
            Some("No out-of-sample trades recorded on this tape.".to_string())
        } else if oos_n < 30 {
            let ci_pp = match (wl, wu) {
                (Some(l), Some(u)) => (u - l) * 100.0,
                _ => 0.0,
            };
            Some(format!(
                "Small OOS sample (n={oos_n} < 30; 95% Wilson CI width {ci_pp:.1} pp)."
            ))
        } else {
            None
        };

        let series_breakdown = by_series
            .into_iter()
            .map(|(series, (cnt, wins, pnl_sum))| SeriesBreakdown {
                asset: short_asset_label(&series),
                series,
                oos_trades: cnt,
                oos_wins: wins,
                oos_win_rate: if cnt > 0 {
                    Some(wins as f64 / cnt as f64)
                } else {
                    None
                },
                oos_net_expectancy_cents: if cnt > 0 {
                    finite_opt((pnl_sum / cnt as f64) * 100.0)
                } else {
                    None
                },
                oos_net_pnl_dollars_1ct: pnl_sum,
            })
            .collect();

        rankings.push(StrategyTournamentRow {
            rank: 0,
            strategy: meta.name.clone(),
            side: meta.side.clone(),
            total_trades: total_n,
            is_trades: is_list.len(),
            oos_trades: oos_n,
            oos_wins,
            oos_losses: oos_n.saturating_sub(oos_wins),
            oos_win_rate,
            oos_wilson_lower_95: wl,
            oos_wilson_upper_95: wu,
            oos_avg_entry_price: oos_avg_price,
            oos_avg_fee_cents: oos_avg_fee_c,
            is_net_expectancy_cents: is_exp_c,
            oos_net_expectancy_cents: oos_exp_c,
            oos_net_pnl_dollars_1ct: pnl_1ct_sum,
            oos_compounded_pnl_dollars: if oos_n > 0 { eq - start_eq } else { 0.0 },
            oos_sharpe,
            oos_sortino,
            oos_max_drawdown_pct: max_dd_pct,
            oos_positive,
            sample_warning,
            series_breakdown,
        });
    }

    // Strict OOS ranking: active OOS strategies first, then highest OOS net expectancy,
    // highest OOS Sharpe, lowest Max DD, largest OOS sample, deterministic name tie-break.
    rankings.sort_by(|a, b| {
        let a_has = a.oos_trades > 0;
        let b_has = b.oos_trades > 0;
        b_has
            .cmp(&a_has)
            .then_with(|| {
                b.oos_net_expectancy_cents
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&a.oos_net_expectancy_cents.unwrap_or(f64::NEG_INFINITY))
            })
            .then_with(|| {
                b.oos_sharpe
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&a.oos_sharpe.unwrap_or(f64::NEG_INFINITY))
            })
            .then_with(|| a.oos_max_drawdown_pct.total_cmp(&b.oos_max_drawdown_pct))
            .then_with(|| b.oos_trades.cmp(&a.oos_trades))
            .then_with(|| a.strategy.cmp(&b.strategy))
    });
    for (idx, row) in rankings.iter_mut().enumerate() {
        row.rank = idx + 1;
    }

    let correlation_matrix = build_correlation_matrix(&ctx.strategies, &ctx.entries, slip_dollars);
    let optimal_lineup = select_optimal_lineup(&rankings, &correlation_matrix, CORRELATION_CAP);
    let regime_heatmap_oos = build_regime_heatmap("out_of_sample", &oos_entries_all, slip_c);
    let regime_heatmap_full = build_regime_heatmap("full_tape", &full_entries_all, slip_c);

    TournamentReport {
        mode_notice: "Paper research tournament — strictly read-only simulation, no venue capital at risk."
            .to_string(),
        slippage_cents: slip_c,
        correlation_cap: CORRELATION_CAP,
        windows_settled_total: ctx.windows_settled_total,
        windows_settled_is: ctx.windows_settled_is,
        windows_settled_oos: ctx.windows_settled_oos,
        windows_conflicted: ctx.windows_conflicted,
        split_epoch: ctx.split_epoch,
        observed_series: ctx.observed_series.clone(),
        rankings,
        correlation_matrix,
        optimal_lineup,
        regime_heatmap_oos,
        regime_heatmap_full,
    }
}

/// Convenience one-shot entrypoint for `GET /api/analytics/tournament`.
pub fn tournament_from_windows(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
    slippage_cents: f64,
) -> TournamentReport {
    let ctx = build_tournament_context(windows, strategies);
    compute_tournament_from_context(&ctx, slippage_cents)
}

// ---------------------------------------------------------------------------
// Comprehensive Unit, Invariant, Honesty & Scale Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{iso_z, load_strategies, OptPrice};
    use std::path::Path;

    fn snap_json(
        ticker: &str,
        series: &str,
        fetched_at: i64,
        close_time: &str,
        result: &str,
        yes_ask: Option<f64>,
        no_ask: Option<f64>,
    ) -> Snapshot {
        let yes_bid = no_ask.map(|na| ((1.0 - na) * 100.0).round() / 100.0);
        let no_bid = yes_ask.map(|ya| ((1.0 - ya) * 100.0).round() / 100.0);
        serde_json::from_value(serde_json::json!({
            "fetched_at": fetched_at,
            "ticker": ticker,
            "event_ticker": ticker,
            "series": series,
            "close_time": close_time,
            "status": if result.is_empty() { "active" } else { "settled" },
            "result": result,
            "yes_bid": yes_bid,
            "yes_ask": yes_ask,
            "no_bid": no_bid,
            "no_ask": no_ask,
            "last_price": yes_ask,
        }))
        .expect("deserialize Snapshot fixture")
    }

    fn make_synthetic_context(
        names: &[(&str, &str)],
        windows_total: usize,
        entries: Vec<TournamentEntry>,
    ) -> TournamentTapeContext {
        let split_idx = ((windows_total * 3) / 5).max(1).min(windows_total - 1);
        let split_epoch = 1_770_000_000 + (split_idx as i64) * 900;
        let mut series_set: BTreeSet<String> = BTreeSet::new();
        for e in &entries {
            if !e.series.is_empty() {
                series_set.insert(e.series.clone());
            }
        }
        TournamentTapeContext {
            strategies: names
                .iter()
                .map(|(n, s)| StrategyMeta {
                    name: (*n).to_string(),
                    side: (*s).to_string(),
                })
                .collect(),
            windows_settled_total: windows_total,
            windows_settled_is: split_idx,
            windows_settled_oos: windows_total - split_idx,
            windows_conflicted: 0,
            split_epoch: Some(split_epoch),
            observed_series: series_set.into_iter().collect(),
            entries,
            window_vol_scores: HashMap::new(),
        }
    }

    #[test]
    fn correlation_symmetry_bounds_and_optimal_4_slot_uncorrelated_lineup() {
        // 6 strategies across 100 chronological settled windows (60 IS, 40 OOS):
        // - s0 (highest EV, wins 85% at 60c)
        // - s1 (2nd highest EV, IDENTICAL outcomes to s0 -> r = +1.0 > 0.65! MUST be rejected!)
        // - s2, s3, s4 (independent positive-EV strategies with |r| <= 0.65)
        // - s5 (negative OOS expectancy -> MUST never enter lineup!)
        let metas = [
            ("alpha-core", "yes"),
            ("alpha-clone", "yes"),
            ("beta-orth", "yes"),
            ("gamma-orth", "no"),
            ("delta-orth", "yes"),
            ("loser-control", "no"),
        ];
        let series_cycle = ["KXBTC15M", "KXETH15M", "KXSOL15M", "KXXRP15M"];
        let mut entries = Vec::new();
        for w in 0..100usize {
            let epoch = 1_770_000_000 + (w as i64) * 900;
            let series = series_cycle[w % 4];
            let ticker = format!("{series}-T{w:03}");

            // Independent deterministic binary outcome patterns with low mutual correlation.
            let win0 = (w % 7) != 0; // ~85.7%
            let win1 = win0; // identical -> r = +1.0 with s0
            let win2 = ((w * 3 + 1) % 5) != 0; // 80%
            let win3 = ((w * 7 + 2) % 6) != 0; // ~83.3%
            let win4 = ((w * 5 + 3) % 4) != 0; // 75%
            let win5 = (w % 3) == 0; // 33.3% at 60c -> strongly negative EV!

            let specs = [
                (0, 0.60, 240i64, win0),
                (1, 0.61, 240i64, win1),
                (2, 0.58, 420i64, win2),
                (3, 0.62, 180i64, win3),
                (4, 0.55, 720i64, win4),
                (5, 0.60, 90i64, win5),
            ];
            for (si, price, secs, won) in specs {
                entries.push(TournamentEntry {
                    close_epoch: epoch,
                    ticker: ticker.clone(),
                    series: series.to_string(),
                    strategy_idx: si,
                    strategy: metas[si].0.to_string(),
                    side: metas[si].1.to_string(),
                    entry_price: price,
                    secs_to_close: secs,
                    won,
                });
            }
        }

        let ctx = make_synthetic_context(&metas, 100, entries);
        let rep = compute_tournament_from_context(&ctx, 1.0);

        // 1. Correlation matrix symmetry, diagonal == 1.0, and [-1, 1] bounds.
        let cm = &rep.correlation_matrix;
        assert_eq!(cm.strategies.len(), 6);
        for i in 0..6 {
            assert_eq!(cm.matrix[i][i], Some(1.0));
            for j in 0..6 {
                assert_eq!(cm.shared_windows[i][j], cm.shared_windows[j][i]);
                match (cm.matrix[i][j], cm.matrix[j][i]) {
                    (Some(a), Some(b)) => {
                        assert!((a - b).abs() < 1e-12, "asymmetric ({i},{j}): {a} vs {b}");
                        assert!((-1.0..=1.0).contains(&a) && a.is_finite());
                    }
                    (None, None) => {}
                    _ => panic!("asymmetric Option at ({i},{j})"),
                }
            }
        }
        // alpha-core and alpha-clone MUST have r == +1.0.
        assert!((cm.matrix[0][1].unwrap() - 1.0).abs() < 1e-12);

        // 2. Optimal 4-slot lineup MUST select 4 positive uncorrelated strategies,
        // excluding alpha-clone (r=1.0 conflict) AND loser-control (negative OOS EV).
        let lineup = &rep.optimal_lineup;
        assert!(lineup.lineup_complete);
        assert!(!lineup.fallback_used);
        assert_eq!(lineup.selected.len(), 4);
        let sel_names: Vec<&str> = lineup.selected.iter().map(|s| s.strategy.as_str()).collect();
        assert!(sel_names.contains(&"alpha-core"));
        assert!(!sel_names.contains(&"alpha-clone"), "clone should be rejected for |r|>0.65");
        assert!(!sel_names.contains(&"loser-control"), "negative EV must never be selected");
        assert!(lineup.max_abs_correlation.unwrap() <= CORRELATION_CAP + 1e-12);
        assert!(
            lineup
                .rejected_for_correlation
                .iter()
                .any(|c| c.excluded_strategy == "alpha-clone" && c.conflicting_with == "alpha-core")
        );

        // 3. Per-series breakdown covers all 4 assets (BTC, ETH, SOL, XRP).
        let top = &rep.rankings[0];
        let assets: BTreeSet<&str> = top.series_breakdown.iter().map(|s| s.asset.as_str()).collect();
        assert_eq!(assets, BTreeSet::from(["BTC", "ETH", "SOL", "XRP"]));
    }

    #[test]
    fn negative_edge_honesty_discloses_shortfall_and_never_pads_with_losers() {
        // 5 strategies: ONLY 2 are OOS positive, 3 are OOS negative.
        let metas = [
            ("pos-a", "yes"),
            ("pos-b", "no"),
            ("neg-c", "yes"),
            ("neg-d", "no"),
            ("neg-e", "yes"),
        ];
        let mut entries = Vec::new();
        for w in 0..50usize {
            let epoch = 1_770_000_000 + (w as i64) * 900;
            let ticker = format!("KXBTC15M-NEG{w:03}");
            let specs = [
                (0, 0.50, 240i64, (w % 5) != 0), // 80% at 50c -> positive
                (1, 0.50, 420i64, (w % 4) != 0), // 75% at 50c -> positive
                (2, 0.70, 240i64, (w % 2) == 0), // 50% at 70c -> negative
                (3, 0.75, 180i64, (w % 3) == 0), // 33% at 75c -> negative
                (4, 0.80, 90i64, (w % 4) == 0),  // 25% at 80c -> negative
            ];
            for (si, price, secs, won) in specs {
                entries.push(TournamentEntry {
                    close_epoch: epoch,
                    ticker: ticker.clone(),
                    series: "KXBTC15M".to_string(),
                    strategy_idx: si,
                    strategy: metas[si].0.to_string(),
                    side: metas[si].1.to_string(),
                    entry_price: price,
                    secs_to_close: secs,
                    won,
                });
            }
        }
        let ctx = make_synthetic_context(&metas, 50, entries);
        let rep = compute_tournament_from_context(&ctx, 1.0);
        assert_eq!(rep.optimal_lineup.positive_oos_candidates, 2);
        assert_eq!(rep.optimal_lineup.selected.len(), 2);
        assert!(!rep.optimal_lineup.lineup_complete);
        assert!(rep.optimal_lineup.fallback_used);
        let note = rep.optimal_lineup.fallback_disclosure.as_deref().unwrap();
        assert!(note.contains("Only 2 of 5"), "disclosure: {note}");

        // All-negative case -> 0 selected, honest disclosure.
        let neg_only: Vec<TournamentEntry> = ctx
            .entries
            .iter()
            .filter(|e| e.strategy_idx >= 2)
            .cloned()
            .collect();
        let ctx_neg = make_synthetic_context(&metas, 50, neg_only);
        let rep_neg = compute_tournament_from_context(&ctx_neg, 1.0);
        assert_eq!(rep_neg.optimal_lineup.selected.len(), 0);
        assert!(rep_neg.optimal_lineup.fallback_used);
        assert!(rep_neg.optimal_lineup.fallback_disclosure.unwrap().contains("0 strategies"));
    }

    #[test]
    fn heatmap_conservation_boundaries_and_wilson_warnings() {
        // One entry per (price_bucket, time_bucket) cell (20 cells) + 2 entries below 10c.
        let metas = [("grid-test", "yes")];
        let prices = [0.15, 0.40, 0.60, 0.80, 0.95];
        let times = [720i64, 450, 200, 90];
        let mut entries = Vec::new();
        let mut idx = 0i64;
        for &p in &prices {
            for &t in &times {
                entries.push(TournamentEntry {
                    close_epoch: 1_771_000_000 + idx * 900,
                    ticker: format!("KXBTC15M-HM{idx:02}"),
                    series: "KXBTC15M".to_string(),
                    strategy_idx: 0,
                    strategy: "grid-test".to_string(),
                    side: "yes".to_string(),
                    entry_price: p,
                    secs_to_close: t,
                    won: idx % 2 == 0,
                });
                idx += 1;
            }
        }
        // Add 2 sub-10c entries to verify exact conservation accounting.
        for extra in 0..2 {
            entries.push(TournamentEntry {
                close_epoch: 1_771_000_000 + (idx + extra) * 900,
                ticker: format!("KXBTC15M-SUB{extra}"),
                series: "KXBTC15M".to_string(),
                strategy_idx: 0,
                strategy: "grid-test".to_string(),
                side: "yes".to_string(),
                entry_price: 0.05,
                secs_to_close: 240,
                won: false,
            });
        }

        let ctx = make_synthetic_context(&metas, 22, entries);
        let rep = compute_tournament_from_context(&ctx, 1.0);
        let hm = &rep.regime_heatmap_full;

        assert_eq!(hm.cells.len(), 20);
        assert_eq!(hm.total_entries_in_grid, 20);
        assert_eq!(hm.excluded_below_10c, 2);
        assert_eq!(
            hm.total_entries_in_grid + hm.excluded_below_10c,
            hm.total_entries_evaluated
        );
        let cell_sum: usize = hm.cells.iter().map(|c| c.n).sum();
        assert_eq!(cell_sum, hm.total_entries_in_grid);
        // Every single one of the 20 cells has n=1 -> must carry an insufficient Wilson warning!
        for c in &hm.cells {
            assert_eq!(c.n, 1);
            assert_eq!(c.sample_depth_status, "insufficient");
            assert!(c.sample_warning.as_deref().unwrap().contains("Wilson CI"));
        }
    }

    #[test]
    fn exact_combinatorial_beats_greedy_and_handles_empty_or_single_epoch_tapes() {
        // Classic greedy trap:
        // "hub" has highest individual EV (+15c), but correlates |r| > 0.65 with all 4
        // mutually uncorrelated "+10c" strategies (s1..s4). Greedy picks hub (1 slot, 15c);
        // exact combinatorial DFS picks {s1, s2, s3, s4} (4 complete slots, 40c total).
        let metas = [
            ("hub", "yes"),
            ("s1", "yes"),
            ("s2", "yes"),
            ("s3", "yes"),
            ("s4", "yes"),
        ];
        // Construct 4 orthogonal zero-mean +/-1 vectors over 16 windows (Hadamard-like)
        // repeated across 5 blocks (80 windows: 48 IS, 32 OOS), and hub = majority vote
        // or high-correlation with each — wait: 4 orthogonal vectors can't all have r>0.65
        // with one vector (sum of r_i^2 <= 1)! Instead test 2 orthogonal vectors + 1 hub:
        // hub correlated r~0.707 with s1 and s2 (0.707^2 + 0.707^2 = 1.0 > 0.65^2*2!),
        // plus s3 & s4 disjoint in time (0 overlap with s1, s2, hub -> compatible with all).
        // Actually even cleaner: hub has r=1.0 with s1 and r=1.0 with s2? No, make s1 & s2
        // disjoint on different window halves (0 overlap -> compatible!), while hub enters
        // BOTH halves and matches s1 on half 1 (r=1.0) and s2 on half 2 (r=1.0)!
        let mut entries = Vec::new();
        for w in 0..80usize {
            let epoch = 1_773_000_000 + (w as i64) * 900;
            let ticker = format!("KXBTC15M-GRD{w:03}");
            let won_h = (w % 5) != 0; // 80% win rate
            // hub trades EVERY window at 55c (~+22c EV).
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".to_string(),
                strategy_idx: 0,
                strategy: "hub".to_string(),
                side: "yes".to_string(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: won_h,
            });
            // s1..s4 each trade 1/4 of windows (disjoint mod 4 -> 0 pairwise overlap -> mutually uncorrelated!),
            // each copying hub's exact outcome on its windows (r = +1.0 with hub > 0.65!).
            let bucket = (w % 4) + 1;
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker,
                series: "KXBTC15M".to_string(),
                strategy_idx: bucket,
                strategy: metas[bucket].0.to_string(),
                side: "yes".to_string(),
                entry_price: 0.60, // ~+17c EV (lower individually than hub's +22c!)
                secs_to_close: 240,
                won: won_h,
            });
        }
        let ctx = make_synthetic_context(&metas, 80, entries);
        let rep = compute_tournament_from_context(&ctx, 1.0);
        // hub ranks #1 individually, yet exact DFS selects all 4 disjoint strategies {s1, s2, s3, s4}!
        assert_eq!(rep.rankings[0].strategy, "hub");
        assert!(rep.optimal_lineup.lineup_complete);
        assert_eq!(rep.optimal_lineup.selected.len(), 4);
        let picked: BTreeSet<&str> = rep
            .optimal_lineup
            .selected
            .iter()
            .map(|s| s.strategy.as_str())
            .collect();
        assert_eq!(picked, BTreeSet::from(["s1", "s2", "s3", "s4"]));

        // Empty tape & single-epoch tape stay honest with zero NaNs.
        let empty_ctx = TournamentTapeContext {
            strategies: vec![StrategyMeta {
                name: "solo".to_string(),
                side: "yes".to_string(),
            }],
            windows_settled_total: 0,
            windows_settled_is: 0,
            windows_settled_oos: 0,
            windows_conflicted: 0,
            split_epoch: None,
            observed_series: Vec::new(),
            entries: Vec::new(),
            window_vol_scores: HashMap::new(),
        };
        let empty_rep = compute_tournament_from_context(&empty_ctx, 1.0);
        assert_eq!(empty_rep.rankings.len(), 1);
        assert_eq!(empty_rep.rankings[0].oos_trades, 0);
        assert!(empty_rep.optimal_lineup.selected.is_empty());
        assert!(empty_rep.optimal_lineup.fallback_used);
        assert_eq!(empty_rep.regime_heatmap_oos.total_entries_evaluated, 0);
        let json = serde_json::to_string(&empty_rep).unwrap();
        assert!(!json.contains("NaN") && !json.contains("Infinity"));
    }

    #[test]
    fn scale_100k_ticks_all_16_strategies_and_clean_json_no_rune_jargon() {
        let strategies = load_strategies(Path::new("strategies"));
        assert_eq!(strategies.len(), 16);

        // Reuse one deserialized Snapshot template (forward-compatible with any optional
        // fields added to Snapshot) so test fixture construction doesn't burn CPU on 100k JSON parses.
        let active_tpl = snap_json("T", "KXBTC15M", 0, "2026-11-22T00:00:00Z", "", Some(0.60), Some(0.42));
        let settled_tpl = snap_json("T", "KXBTC15M", 0, "2026-11-22T00:00:00Z", "yes", None, None);

        // 2,000 settled 15m windows x 50 snapshots/window = 100,000 active ticks (+ 2,000 settled).
        let series_list = ["KXBTC15M", "KXETH15M", "KXSOL15M", "KXXRP15M"];
        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        let base_epoch = 1_772_000_000i64;
        for w in 0..2_000usize {
            let close_epoch = base_epoch + (w as i64) * 900;
            let close_iso = iso_z(close_epoch);
            let series = series_list[w % 4];
            let ticker = format!("{series}-TRN{w:04}");
            let res = if w % 5 == 4 { "no" } else { "yes" };
            let mut snaps = Vec::with_capacity(51);
            for k in 0..50usize {
                let secs = 882 - (k as i64) * 18;
                let base_c = 50 + ((w * 3 + k) % 34) as i64; // 0.50..=0.83
                let ya = base_c as f64 / 100.0;
                let na = (102 - base_c) as f64 / 100.0;
                let mut s = active_tpl.clone();
                s.fetched_at = close_epoch - secs;
                s.ticker = ticker.as_str().into();
                s.event_ticker = ticker.as_str().into();
                s.series = series.into();
                s.close_time = close_iso.as_str().into();
                s.yes_bid = OptPrice::from_opt(Some((100 - (102 - base_c)) as f64 / 100.0));
                s.yes_ask = OptPrice::from_opt(Some(ya));
                s.no_bid = OptPrice::from_opt(Some((100 - base_c) as f64 / 100.0));
                s.no_ask = OptPrice::from_opt(Some(na));
                s.last_price = OptPrice::from_opt(Some(ya));
                snaps.push(s);
            }
            let mut st = settled_tpl.clone();
            st.fetched_at = close_epoch + 60;
            st.ticker = ticker.as_str().into();
            st.event_ticker = ticker.as_str().into();
            st.series = series.into();
            st.close_time = close_iso.into();
            st.result = res.into();
            snaps.push(st);
            windows.insert(ticker, snaps);
        }
        let total_snaps: usize = windows.values().map(|v| v.len()).sum();
        assert!(total_snaps >= 100_000);

        let t_ctx = Instant::now();
        let ctx = build_tournament_context(&windows, &strategies);
        let ctx_ms = t_ctx.elapsed().as_secs_f64() * 1000.0;

        let t_rep = Instant::now();
        let rep = compute_tournament_from_context(&ctx, 1.0);
        let rep_ms = t_rep.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "[tournament-scale] 2000 windows / {total_snaps} snaps x 12 strategies: ctx={ctx_ms:.1}ms, report={rep_ms:.1}ms, entries={}",
            ctx.entries.len()
        );

        assert_eq!(rep.windows_settled_total, 2_000);
        assert_eq!(rep.rankings.len(), 16);
        assert_eq!(rep.correlation_matrix.strategies.len(), 16);
        assert_eq!(rep.regime_heatmap_oos.cells.len(), 20);
        assert_eq!(
            rep.regime_heatmap_oos.total_entries_in_grid + rep.regime_heatmap_oos.excluded_below_10c,
            rep.regime_heatmap_oos.total_entries_evaluated
        );
        // Computing tournament + 12x12 correlation + exact 4-slot lineup + 2D heatmaps
        // from cached context must stay ultra-fast (< 150ms even in unoptimized debug mode).
        assert!(
            rep_ms < 150.0,
            "compute_tournament_from_context took {rep_ms:.1}ms (budget: 150ms debug)"
        );

        let json = serde_json::to_string(&rep).unwrap();
        assert!(!json.contains("NaN") && !json.contains("Infinity"));
        assert!(!json.to_ascii_lowercase().contains("rune"));
    }
}
