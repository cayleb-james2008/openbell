//! Paper Quant Diagnostics Engine for OpenBell (strictly paper-only).
//!
//! Pure analytical diagnostics over collected forward tapes and paper slot
//! simulations:
//! - Probability calibration & reliability analysis (10 decile buckets, exact
//!   95% Wilson score confidence intervals, Brier score, Brier skill score,
//!   Expected Calibration Error, Maximum Calibration Error).
//! - Chronological Walk-Forward In-Sample (60%) vs Out-of-Sample (40%)
//!   performance attribution (Net paper P&L, window-return Sharpe & Sortino,
//!   Profit Factor, Max Drawdown %, Calmar ratio, Expectancy, Fee Drag %,
//!   and Win/Loss streak distributions).
//! - Deterministic seeded Monte Carlo bootstrap (Xoshiro256++ seeded via
//!   SplitMix64) computing P5/P25/P50/P75/P95 paper equity fan curves and
//!   drawdown exceedance probabilities (`P(DD >= 10%)`, `P(DD >= 25%)`).
//! - In-memory Rune strategy sandbox validator & backtester compiling user
//!   source in memory, rejecting forbidden non-paper patterns, and scoring
//!   over collected tape windows without restarting the server or touching disk.

#![allow(dead_code)]

use crate::{
    facts_fingerprint, fee, is_budget_halt, parse_iso_z, window_facts_keyed, ScoreReport,
    SlotSimulation, Snapshot, Standard, Strategy, WindowFacts, BUDGET_DEATH_LIMIT,
    MAX_STRATEGY_BYTES, STRATEGY_BUDGET, STRATEGY_DEADLINE_SECS, STRATEGY_MEMORY_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Standard normal 97.5th percentile for exact two-sided 95% Wilson intervals.
pub const WILSON_Z_95: f64 = 1.959_963_984_540_054;

/// Default maximum trade ledger entries returned inline from an in-memory backtest.
pub const DEFAULT_MAX_BACKTEST_TRADES: usize = 5_000;

/// Maximum fan-chart checkpoints emitted for Monte Carlo equity bands.
const MAX_FAN_CHECKPOINTS: usize = 100;

// ---------------------------------------------------------------------------
// Whole-cent & numeric hygiene helpers
// ---------------------------------------------------------------------------

/// Kalshi binary contracts trade exclusively in whole cents from $0.01 to $0.99.
#[inline]
pub fn is_valid_whole_cent(price: f64) -> bool {
    price.is_finite()
        && (0.01..=0.99).contains(&price)
        && ((price * 100.0) - (price * 100.0).round()).abs() < 1e-9
}

/// Normalize a whole-cent quote in `0.01..=0.99` to exact two-decimal float,
/// or `None` if outside bounds or fractional-cent.
#[inline]
pub fn normalize_whole_cent(price: f64) -> Option<f64> {
    if is_valid_whole_cent(price) {
        Some((price * 100.0).round() / 100.0)
    } else {
        None
    }
}

#[inline]
fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

#[inline]
fn finite_opt(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Pillar 1: Probability Calibration & Reliability Analysis
// ---------------------------------------------------------------------------

/// One probability-vs-outcome observation for calibration analysis.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CalibrationObservation {
    /// Predicted implied probability in `[0.0, 1.0]` (e.g., whole-cent entry
    /// price or mid implied probability).
    pub predicted_prob: f64,
    /// Realized settlement outcome (`true` = 1.0, `false` = 0.0).
    pub outcome: bool,
}

/// One decile reliability bucket (`[lower_bound, upper_bound)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationBucket {
    pub bucket_index: usize,
    pub label: String,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub midpoint: f64,
    pub count: usize,
    pub wins: usize,
    pub mean_predicted: Option<f64>,
    pub realized_rate: Option<f64>,
    pub wilson_lower_95: Option<f64>,
    pub wilson_upper_95: Option<f64>,
    /// `realized_rate - mean_predicted` (positive means realized wins exceeded implied price).
    pub calibration_gap: Option<f64>,
}

/// Full probability calibration & reliability report across 10 decile buckets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationReport {
    pub sample_size: usize,
    pub total_wins: usize,
    pub overall_predicted: Option<f64>,
    pub overall_realized: Option<f64>,
    pub brier_score: Option<f64>,
    pub brier_baseline: Option<f64>,
    pub brier_skill_score: Option<f64>,
    /// Expected Calibration Error: sample-weighted mean absolute bucket gap.
    pub ece: Option<f64>,
    /// Maximum Calibration Error: worst absolute bucket gap across non-empty buckets.
    pub mce: Option<f64>,
    pub buckets: Vec<CalibrationBucket>,
}

/// Calibration report tagged with a strategy identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyCalibrationReport {
    pub strategy: String,
    pub side: String,
    pub calibration: CalibrationReport,
}

/// Complete desk calibration summary across market tape, pooled portfolio entries,
/// and each loaded strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeskCalibrationSummary {
    pub windows_settled: usize,
    pub windows_conflicted: usize,
    pub market_calibration: CalibrationReport,
    pub portfolio_calibration: CalibrationReport,
    pub by_strategy: Vec<StrategyCalibrationReport>,
}

/// Exact two-sided 95% Wilson score confidence interval for binomial proportion `wins / total`.
/// Returns `None` when `total == 0`. Clamped strictly to `[0.0, 1.0]`.
pub fn wilson_interval_95(wins: usize, total: usize) -> Option<(f64, f64)> {
    if total == 0 {
        return None;
    }
    let k = wins.min(total) as f64;
    let n = total as f64;
    let p_hat = k / n;
    let z = WILSON_Z_95;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p_hat + z2 / (2.0 * n)) / denom;
    let rad = (p_hat * (1.0 - p_hat) / n + z2 / (4.0 * n * n)).max(0.0).sqrt();
    let half = (z / denom) * rad;
    let lower = (center - half).clamp(0.0, 1.0);
    let upper = (center + half).clamp(0.0, 1.0);
    Some((lower, upper))
}

/// Compute 10-decile calibration reliability buckets, 95% Wilson intervals,
/// Brier score, Brier skill score, ECE, and MCE.
pub fn compute_calibration(observations: &[CalibrationObservation]) -> CalibrationReport {
    let mut bucket_counts = [0usize; 10];
    let mut bucket_wins = [0usize; 10];
    let mut bucket_prob_sum = [0.0f64; 10];

    let mut valid_n = 0usize;
    let mut total_wins = 0usize;
    let mut total_pred = 0.0f64;
    let mut brier_sum = 0.0f64;

    for obs in observations {
        if !obs.predicted_prob.is_finite() {
            continue;
        }
        let p = obs.predicted_prob.clamp(0.0, 1.0);
        let y = if obs.outcome { 1.0 } else { 0.0 };
        let idx = ((p * 10.0).floor() as usize).min(9);

        bucket_counts[idx] += 1;
        if obs.outcome {
            bucket_wins[idx] += 1;
            total_wins += 1;
        }
        bucket_prob_sum[idx] += p;
        valid_n += 1;
        total_pred += p;
        let diff = p - y;
        brier_sum += diff * diff;
    }

    let mut buckets = Vec::with_capacity(10);
    let mut ece_sum = 0.0f64;
    let mut mce_max = 0.0f64;
    let mut has_non_empty = false;

    for b in 0..10 {
        let lower_bound = (b as f64) * 0.1;
        let upper_bound = ((b + 1) as f64) * 0.1;
        let midpoint = lower_bound + 0.05;
        let label = format!("{}-{}%", b * 10, (b + 1) * 10);
        let count = bucket_counts[b];
        let wins = bucket_wins[b];

        if count > 0 {
            has_non_empty = true;
            let mean_pred = bucket_prob_sum[b] / count as f64;
            let realized = wins as f64 / count as f64;
            let gap = realized - mean_pred;
            let abs_gap = gap.abs();
            ece_sum += (count as f64 / valid_n as f64) * abs_gap;
            if abs_gap > mce_max {
                mce_max = abs_gap;
            }
            let (wl, wu) = wilson_interval_95(wins, count).unwrap();
            buckets.push(CalibrationBucket {
                bucket_index: b,
                label,
                lower_bound,
                upper_bound,
                midpoint,
                count,
                wins,
                mean_predicted: Some(mean_pred),
                realized_rate: Some(realized),
                wilson_lower_95: Some(wl),
                wilson_upper_95: Some(wu),
                calibration_gap: Some(gap),
            });
        } else {
            buckets.push(CalibrationBucket {
                bucket_index: b,
                label,
                lower_bound,
                upper_bound,
                midpoint,
                count: 0,
                wins: 0,
                mean_predicted: None,
                realized_rate: None,
                wilson_lower_95: None,
                wilson_upper_95: None,
                calibration_gap: None,
            });
        }
    }

    if valid_n == 0 {
        return CalibrationReport {
            sample_size: 0,
            total_wins: 0,
            overall_predicted: None,
            overall_realized: None,
            brier_score: None,
            brier_baseline: None,
            brier_skill_score: None,
            ece: None,
            mce: None,
            buckets,
        };
    }

    let n_f = valid_n as f64;
    let overall_pred = total_pred / n_f;
    let overall_real = total_wins as f64 / n_f;
    let brier = brier_sum / n_f;
    let baseline = overall_real * (1.0 - overall_real);
    let bss = if baseline > 1e-12 {
        finite_opt(1.0 - brier / baseline)
    } else {
        None
    };

    CalibrationReport {
        sample_size: valid_n,
        total_wins,
        overall_predicted: Some(overall_pred),
        overall_realized: Some(overall_real),
        brier_score: Some(brier),
        brier_baseline: Some(baseline),
        brier_skill_score: bss,
        ece: if has_non_empty { Some(ece_sum) } else { None },
        mce: if has_non_empty { Some(mce_max) } else { None },
        buckets,
    }
}

/// Extract the representative implied YES probability for a settled window from
/// its active pre-close snapshots (prefers the snapshot closest to T-240s, or
/// the last active quote with valid prices before close).
fn representative_window_yes_prob(snaps: &[Snapshot], close_epoch: i64) -> Option<f64> {
    let mut best: Option<(i64, f64)> = None;
    for s in snaps {
        let (Some(ya), Some(na)) = (s.yes_ask, s.no_ask) else {
            continue;
        };
        if !(0.01..=0.99).contains(&ya) || !(0.01..=0.99).contains(&na) {
            continue;
        }
        let secs = close_epoch - s.fetched_at;
        if secs <= 0 {
            continue;
        }
        // Synthetic Kalshi YES mid: avg(yes_ask, yes_bid) where yes_bid = 1 - no_ask
        let yes_bid = s.yes_bid.filter(|b| (0.0..=1.0).contains(b)).unwrap_or(1.0 - na);
        let mid = ((ya + yes_bid) * 0.5).clamp(0.01, 0.99);
        let dist = (secs - 240).abs();
        match best {
            Some((best_dist, _)) if dist >= best_dist => {}
            _ => best = Some((dist, mid)),
        }
    }
    best.map(|(_, p)| p)
}

/// Compute desk-wide calibration over collected tape windows and all loaded strategies.
pub fn calibration_from_windows(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
) -> DeskCalibrationSummary {
    let mut settled = 0usize;
    let mut conflicted = 0usize;
    let mut market_obs: Vec<CalibrationObservation> = Vec::new();
    let mut portfolio_obs: Vec<CalibrationObservation> = Vec::new();
    let mut per_strat_obs: Vec<Vec<CalibrationObservation>> =
        vec![Vec::new(); strategies.len()];

    for (ticker, snaps) in windows {
        let facts_fp = facts_fingerprint(snaps);
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict => {
                conflicted += 1;
                continue;
            }
            WindowFacts::Unsettled => continue,
        };
        settled += 1;

        if let Some(p_yes) = representative_window_yes_prob(snaps, close_epoch) {
            market_obs.push(CalibrationObservation {
                predicted_prob: p_yes,
                outcome: result == "yes",
            });
        }

        if !strategies.is_empty() {
            let win_fp = crate::window_fingerprint(snaps);
            for (idx, strat) in strategies.iter().enumerate() {
                if let Some(price) =
                    crate::window_entry_keyed(strat, Some(ticker.as_str()), snaps, win_fp)
                {
                    if is_valid_whole_cent(price) {
                        let obs = CalibrationObservation {
                            predicted_prob: price,
                            outcome: result == strat.side,
                        };
                        per_strat_obs[idx].push(obs);
                        portfolio_obs.push(obs);
                    }
                }
            }
        }
    }

    let by_strategy = strategies
        .iter()
        .enumerate()
        .map(|(idx, s)| StrategyCalibrationReport {
            strategy: s.name.clone(),
            side: s.side.clone(),
            calibration: compute_calibration(&per_strat_obs[idx]),
        })
        .collect();

    DeskCalibrationSummary {
        windows_settled: settled,
        windows_conflicted: conflicted,
        market_calibration: compute_calibration(&market_obs),
        portfolio_calibration: compute_calibration(&portfolio_obs),
        by_strategy,
    }
}

// ---------------------------------------------------------------------------
// Pillar 2: Walk-Forward Out-of-Sample Split & Performance Metrics
// ---------------------------------------------------------------------------

/// Internal/public standardized settled paper trade record for analytics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsTrade {
    pub close_epoch: i64,
    pub ticker: String,
    pub series: String,
    pub strategy: String,
    pub side: String,
    pub price: f64,
    pub contracts: i64,
    pub gross_pnl_dollars: f64,
    pub fee_dollars: f64,
    pub net_pnl_dollars: f64,
    pub won: bool,
}

/// Win and loss consecutive streak statistics and exact run-length histograms.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreakDistribution {
    pub max_win_streak: usize,
    pub max_loss_streak: usize,
    /// Positive for active win streak, negative for active loss streak, 0 if empty.
    pub current_streak: i64,
    pub avg_win_streak: f64,
    pub avg_loss_streak: f64,
    /// Number of distinct losing streaks of length >= 3 (OpenBell's strike threshold).
    pub loss_streaks_gte_3: usize,
    pub win_streak_histogram: BTreeMap<usize, usize>,
    pub loss_streak_histogram: BTreeMap<usize, usize>,
}

/// One point on a chronological equity / underwater drawdown series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawdownPoint {
    pub epoch: i64,
    pub equity_dollars: f64,
    pub peak_dollars: f64,
    /// Non-negative drawdown percentage (`0.0..=100.0`).
    pub drawdown_pct: f64,
}

/// Comprehensive paper quant performance summary for a trade slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub trades_count: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
    pub start_equity_dollars: f64,
    pub end_equity_dollars: f64,
    pub net_pnl_dollars: f64,
    pub total_return_pct: f64,
    pub gross_profit_dollars: f64,
    pub gross_loss_dollars: f64,
    pub total_fees_dollars: f64,
    pub total_notional_dollars: f64,
    /// Fees as percentage of deployed notional capital (`total_fees / total_notional * 100`).
    pub fee_drag_pct: f64,
    /// Fees as percentage of gross profit when `gross_profit > 0`.
    pub fee_share_of_gross_profit_pct: Option<f64>,
    pub expectancy_dollars: f64,
    pub expectancy_cents_per_contract: f64,
    /// `gross_profit / gross_loss` (`None` when `gross_loss == 0`).
    pub profit_factor: Option<f64>,
    /// Window-return based Sharpe ratio (`mean(r) / sample_std(r)`).
    pub sharpe: Option<f64>,
    /// Window-return based Sortino ratio (`mean(r) / downside_deviation(r, MAR=0)`).
    pub sortino: Option<f64>,
    /// 24/7 15m annualized Sharpe (`sharpe * sqrt(96 * 365)`), provided as reference.
    pub sharpe_annualized: Option<f64>,
    /// Maximum peak-to-trough drawdown percentage (`0.0..=100.0`).
    pub max_drawdown_pct: f64,
    pub max_drawdown_dollars: f64,
    /// Sample Calmar / Return-over-Max-Drawdown (`total_return_pct / max_drawdown_pct`).
    pub calmar: Option<f64>,
    pub streaks: StreakDistribution,
}

/// Chronological In-Sample (first 60%) vs Out-of-Sample (last 40%) split.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardSplit {
    pub is_fraction: f64,
    pub oos_fraction: f64,
    pub split_epoch: Option<i64>,
    pub full_sample: PerformanceMetrics,
    pub in_sample: PerformanceMetrics,
    pub out_of_sample: PerformanceMetrics,
    pub underwater_curve: Vec<DrawdownPoint>,
    /// Chronological per-window fractional returns (`delta_equity / prev_equity`).
    pub window_returns: Vec<f64>,
}

/// Walk-forward split paired with strategy identity and stand-alone risk framing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyWalkForward {
    pub strategy: String,
    pub side: String,
    pub risk_fraction: f64,
    pub split: WalkForwardSplit,
}

/// Complete desk walk-forward summary across the compounded slot portfolio
/// and every individual strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeskWalkForwardSummary {
    pub paper_pot_start: f64,
    pub portfolio: WalkForwardSplit,
    pub strategies: Vec<StrategyWalkForward>,
}

/// Compute exact win/loss consecutive streak statistics and run-length histograms.
pub fn compute_streak_distribution(outcomes: &[bool]) -> StreakDistribution {
    if outcomes.is_empty() {
        return StreakDistribution {
            max_win_streak: 0,
            max_loss_streak: 0,
            current_streak: 0,
            avg_win_streak: 0.0,
            avg_loss_streak: 0.0,
            loss_streaks_gte_3: 0,
            win_streak_histogram: BTreeMap::new(),
            loss_streak_histogram: BTreeMap::new(),
        };
    }

    let mut win_hist: BTreeMap<usize, usize> = BTreeMap::new();
    let mut loss_hist: BTreeMap<usize, usize> = BTreeMap::new();
    let mut max_win = 0usize;
    let mut max_loss = 0usize;
    let mut win_runs = 0usize;
    let mut win_len_sum = 0usize;
    let mut loss_runs = 0usize;
    let mut loss_len_sum = 0usize;
    let mut loss_gte_3 = 0usize;

    let mut run_is_win = outcomes[0];
    let mut run_len = 0usize;

    let mut finish_run = |is_win: bool, len: usize| {
        if len == 0 {
            return;
        }
        if is_win {
            *win_hist.entry(len).or_default() += 1;
            if len > max_win {
                max_win = len;
            }
            win_runs += 1;
            win_len_sum += len;
        } else {
            *loss_hist.entry(len).or_default() += 1;
            if len > max_loss {
                max_loss = len;
            }
            loss_runs += 1;
            loss_len_sum += len;
            if len >= 3 {
                loss_gte_3 += 1;
            }
        }
    };

    for &won in outcomes {
        if won == run_is_win {
            run_len += 1;
        } else {
            finish_run(run_is_win, run_len);
            run_is_win = won;
            run_len = 1;
        }
    }
    finish_run(run_is_win, run_len);

    let current_streak = if run_is_win {
        run_len as i64
    } else {
        -(run_len as i64)
    };

    StreakDistribution {
        max_win_streak: max_win,
        max_loss_streak: max_loss,
        current_streak,
        avg_win_streak: if win_runs > 0 {
            win_len_sum as f64 / win_runs as f64
        } else {
            0.0
        },
        avg_loss_streak: if loss_runs > 0 {
            loss_len_sum as f64 / loss_runs as f64
        } else {
            0.0
        },
        loss_streaks_gte_3: loss_gte_3,
        win_streak_histogram: win_hist,
        loss_streak_histogram: loss_hist,
    }
}

/// Compute window-return based Sharpe, Sortino, and annualized Sharpe from a
/// slice of chronological window returns.
pub fn compute_return_ratios(returns: &[f64]) -> (Option<f64>, Option<f64>, Option<f64>) {
    let clean: Vec<f64> = returns.iter().copied().filter(|r| r.is_finite()).collect();
    let n = clean.len();
    if n < 2 {
        return (None, None, None);
    }
    let n_f = n as f64;
    let mean = clean.iter().sum::<f64>() / n_f;
    let var = clean
        .iter()
        .map(|r| {
            let d = r - mean;
            d * d
        })
        .sum::<f64>()
        / (n_f - 1.0);
    let std_dev = var.max(0.0).sqrt();

    let sharpe = if std_dev > 1e-12 {
        finite_opt(mean / std_dev)
    } else {
        None
    };

    // Sortino semi-deviation against MAR = 0 across all n observations.
    let down_sq_sum: f64 = clean
        .iter()
        .map(|&r| if r < 0.0 { r * r } else { 0.0 })
        .sum();
    let downside_dev = (down_sq_sum / n_f).max(0.0).sqrt();
    let sortino = if downside_dev > 1e-12 {
        finite_opt(mean / downside_dev)
    } else {
        None
    };

    let ann_factor = (96.0_f64 * 365.0).sqrt();
    let sharpe_annualized = sharpe.and_then(|s| finite_opt(s * ann_factor));

    (sharpe, sortino, sharpe_annualized)
}

/// Compute full institutional performance metrics, underwater drawdown curve, and
/// chronological per-window returns for a trade slice.
pub fn compute_performance_metrics(
    trades: &[AnalyticsTrade],
    start_equity: f64,
) -> (PerformanceMetrics, Vec<DrawdownPoint>, Vec<f64>) {
    let start_eq = finite_or_zero(start_equity);
    if trades.is_empty() {
        return (
            PerformanceMetrics {
                trades_count: 0,
                wins: 0,
                losses: 0,
                win_rate: 0.0,
                start_equity_dollars: start_eq,
                end_equity_dollars: start_eq,
                net_pnl_dollars: 0.0,
                total_return_pct: 0.0,
                gross_profit_dollars: 0.0,
                gross_loss_dollars: 0.0,
                total_fees_dollars: 0.0,
                total_notional_dollars: 0.0,
                fee_drag_pct: 0.0,
                fee_share_of_gross_profit_pct: None,
                expectancy_dollars: 0.0,
                expectancy_cents_per_contract: 0.0,
                profit_factor: None,
                sharpe: None,
                sortino: None,
                sharpe_annualized: None,
                max_drawdown_pct: 0.0,
                max_drawdown_dollars: 0.0,
                calmar: None,
                streaks: compute_streak_distribution(&[]),
            },
            Vec::new(),
            Vec::new(),
        );
    }

    let mut ordered: Vec<&AnalyticsTrade> = trades.iter().collect();
    ordered.sort_by_key(|t| t.close_epoch);

    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut net_pnl = 0.0f64;
    let mut gross_profit = 0.0f64;
    let mut gross_loss = 0.0f64;
    let mut total_fees = 0.0f64;
    let mut total_notional = 0.0f64;
    let mut cents_per_contract_sum = 0.0f64;
    let mut outcomes: Vec<bool> = Vec::with_capacity(ordered.len());

    for t in &ordered {
        if t.won {
            wins += 1;
        } else {
            losses += 1;
        }
        outcomes.push(t.won);
        net_pnl += t.net_pnl_dollars;
        if t.net_pnl_dollars > 0.0 {
            gross_profit += t.net_pnl_dollars;
        } else if t.net_pnl_dollars < 0.0 {
            gross_loss += t.net_pnl_dollars.abs();
        }
        total_fees += t.fee_dollars;
        total_notional += (t.contracts.max(0) as f64) * t.price;
        if t.contracts > 0 {
            cents_per_contract_sum += (t.net_pnl_dollars / t.contracts as f64) * 100.0;
        }
    }

    // Aggregate simultaneous trades at the same close_epoch into single window
    // transitions for honest window-return Sharpe/Sortino and underwater curve.
    let mut eq = start_eq;
    let mut peak = start_eq.max(0.0);
    let mut max_dd_dollars = 0.0f64;
    let mut max_dd_pct = 0.0f64;
    let mut underwater: Vec<DrawdownPoint> = Vec::new();
    let mut window_returns: Vec<f64> = Vec::new();

    if let Some(first) = ordered.first() {
        underwater.push(DrawdownPoint {
            epoch: first.close_epoch,
            equity_dollars: start_eq,
            peak_dollars: peak,
            drawdown_pct: 0.0,
        });
    }

    let mut i = 0usize;
    while i < ordered.len() {
        let epoch = ordered[i].close_epoch;
        let mut epoch_pnl = 0.0f64;
        while i < ordered.len() && ordered[i].close_epoch == epoch {
            epoch_pnl += ordered[i].net_pnl_dollars;
            i += 1;
        }
        let prev_eq = eq;
        eq += epoch_pnl;
        if prev_eq > 1e-9 {
            let r = (epoch_pnl / prev_eq).max(-1.0);
            if r.is_finite() {
                window_returns.push(r);
            }
        }
        if eq > peak {
            peak = eq;
        }
        let dd_dollars = (peak - eq).max(0.0);
        let dd_pct = if peak > 1e-9 {
            ((dd_dollars / peak) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        if dd_dollars > max_dd_dollars {
            max_dd_dollars = dd_dollars;
        }
        if dd_pct > max_dd_pct {
            max_dd_pct = dd_pct;
        }
        underwater.push(DrawdownPoint {
            epoch,
            equity_dollars: eq,
            peak_dollars: peak,
            drawdown_pct: dd_pct,
        });
    }

    let count = ordered.len();
    let win_rate = wins as f64 / count as f64;
    let total_return_pct = if start_eq > 1e-9 {
        finite_or_zero((eq - start_eq) / start_eq * 100.0)
    } else {
        0.0
    };
    let fee_drag_pct = if total_notional > 1e-9 {
        finite_or_zero((total_fees / total_notional) * 100.0)
    } else {
        0.0
    };
    let fee_share_of_gross_profit_pct = if gross_profit > 1e-9 {
        finite_opt((total_fees / gross_profit) * 100.0)
    } else {
        None
    };
    let expectancy_dollars = finite_or_zero(net_pnl / count as f64);
    let expectancy_cents_per_contract = finite_or_zero(cents_per_contract_sum / count as f64);
    let profit_factor = if gross_loss > 1e-12 {
        finite_opt(gross_profit / gross_loss)
    } else {
        None
    };
    let (sharpe, sortino, sharpe_annualized) = compute_return_ratios(&window_returns);
    let calmar = if max_dd_pct > 1e-9 {
        finite_opt(total_return_pct / max_dd_pct)
    } else {
        None
    };

    let metrics = PerformanceMetrics {
        trades_count: count,
        wins,
        losses,
        win_rate,
        start_equity_dollars: start_eq,
        end_equity_dollars: eq,
        net_pnl_dollars: net_pnl,
        total_return_pct,
        gross_profit_dollars: gross_profit,
        gross_loss_dollars: gross_loss,
        total_fees_dollars: total_fees,
        total_notional_dollars: total_notional,
        fee_drag_pct,
        fee_share_of_gross_profit_pct,
        expectancy_dollars,
        expectancy_cents_per_contract,
        profit_factor,
        sharpe,
        sortino,
        sharpe_annualized,
        max_drawdown_pct: max_dd_pct,
        max_drawdown_dollars: max_dd_dollars,
        calmar,
        streaks: compute_streak_distribution(&outcomes),
    };

    (metrics, underwater, window_returns)
}

/// Chronological 60% In-Sample / 40% Out-of-Sample walk-forward split over a
/// set of settled paper trades.
pub fn compute_walk_forward_split(
    trades: &[AnalyticsTrade],
    start_equity: f64,
) -> WalkForwardSplit {
    let mut ordered = trades.to_vec();
    ordered.sort_by_key(|t| t.close_epoch);

    let (full_sample, underwater_curve, window_returns) =
        compute_performance_metrics(&ordered, start_equity);

    if ordered.is_empty() {
        return WalkForwardSplit {
            is_fraction: 0.60,
            oos_fraction: 0.40,
            split_epoch: None,
            full_sample: full_sample.clone(),
            in_sample: full_sample.clone(),
            out_of_sample: full_sample,
            underwater_curve,
            window_returns,
        };
    }

    // Split on distinct chronological window epochs (60% IS / 40% OOS) so
    // trades that closed in the exact same 15m window stay together in one half.
    let mut epochs: Vec<i64> = ordered.iter().map(|t| t.close_epoch).collect();
    epochs.dedup();
    let n_epochs = epochs.len();

    let (is_trades, oos_trades, split_epoch) = if n_epochs < 2 {
        (ordered.as_slice(), &ordered[ordered.len()..], Some(epochs[0]))
    } else {
        let is_epoch_count = ((n_epochs * 3) / 5).max(1).min(n_epochs - 1);
        let boundary = epochs[is_epoch_count];
        let cut = ordered.partition_point(|t| t.close_epoch < boundary);
        (&ordered[..cut], &ordered[cut..], Some(boundary))
    };

    let (in_sample, _, _) = compute_performance_metrics(is_trades, start_equity);
    let oos_start = if in_sample.end_equity_dollars > 1e-6 {
        in_sample.end_equity_dollars
    } else {
        start_equity
    };
    let (out_of_sample, _, _) = compute_performance_metrics(oos_trades, oos_start);

    WalkForwardSplit {
        is_fraction: 0.60,
        oos_fraction: 0.40,
        split_epoch,
        full_sample,
        in_sample,
        out_of_sample,
        underwater_curve,
        window_returns,
    }
}

/// Convert `SlotSimulation` trades into `AnalyticsTrade` records.
pub fn portfolio_trades_from_sim(sim: &SlotSimulation) -> Vec<AnalyticsTrade> {
    sim.trades
        .iter()
        .map(|t| {
            let fee_dollars = t.contracts as f64 * fee(t.price);
            let gross_pnl_dollars = t.pnl_dollars + fee_dollars;
            AnalyticsTrade {
                close_epoch: t.close_epoch,
                ticker: format!("slot-{}", t.slot + 1),
                series: String::new(),
                strategy: t.strategy.clone(),
                side: String::new(),
                price: t.price,
                contracts: t.contracts,
                gross_pnl_dollars,
                fee_dollars,
                net_pnl_dollars: t.pnl_dollars,
                won: t.pnl_dollars > 0.0,
            }
        })
        .collect()
}

/// Extract chronological portfolio window returns from a `SlotSimulation`,
/// ready to feed directly into `run_monte_carlo_with_equity`.
pub fn portfolio_window_returns(sim: &SlotSimulation) -> Vec<f64> {
    let trades = portfolio_trades_from_sim(sim);
    let (_, _, returns) = compute_performance_metrics(&trades, sim.paper_pot_start);
    returns
}

/// Extract chronological standalone window returns for a single strategy across
/// collected tape windows, ready to feed directly into `run_monte_carlo_with_equity`.
pub fn strategy_window_returns(
    strat: &Strategy,
    windows: &BTreeMap<String, Vec<Snapshot>>,
    start_equity: f64,
    risk_fraction: f64,
) -> Vec<f64> {
    let trades = standalone_strategy_trades(strat, windows, start_equity, risk_fraction);
    let (_, _, returns) = compute_performance_metrics(&trades, start_equity);
    returns
}

/// Simulate one strategy standalone over all settled windows on the tape using
/// fractional bankroll allocation (`risk_fraction` per window), producing its
/// complete chronological trade list for honest IS/OOS walk-forward diagnostics.
fn standalone_strategy_trades(
    strat: &Strategy,
    windows: &BTreeMap<String, Vec<Snapshot>>,
    start_equity: f64,
    risk_fraction: f64,
) -> Vec<AnalyticsTrade> {
    struct SettledEntry {
        close_epoch: i64,
        ticker: String,
        series: String,
        price: f64,
        won: bool,
    }
    let mut entries: Vec<SettledEntry> = Vec::new();
    for (ticker, snaps) in windows {
        let facts_fp = facts_fingerprint(snaps);
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict | WindowFacts::Unsettled => continue,
        };
        let win_fp = crate::window_fingerprint(snaps);
        let Some(price) = crate::window_entry_keyed(strat, Some(ticker.as_str()), snaps, win_fp)
        else {
            continue;
        };
        if !is_valid_whole_cent(price) {
            continue;
        }
        let series = snaps
            .first()
            .map(|s| s.series.clone())
            .unwrap_or_default();
        entries.push(SettledEntry {
            close_epoch,
            ticker: ticker.clone(),
            series,
            price,
            won: result == strat.side,
        });
    }
    entries.sort_by_key(|e| e.close_epoch);

    let frac = risk_fraction.clamp(0.01, 1.0);
    let mut equity = start_equity;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        if equity <= 0.0 {
            break;
        }
        let share = equity * frac;
        let contracts = (share / e.price).floor() as i64;
        if contracts <= 0 {
            continue;
        }
        let payoff = if e.won { contracts as f64 } else { 0.0 };
        let cost = contracts as f64 * e.price;
        let fee_dollars = contracts as f64 * fee(e.price);
        let gross_pnl = payoff - cost;
        let net_pnl = gross_pnl - fee_dollars;
        equity += net_pnl;
        out.push(AnalyticsTrade {
            close_epoch: e.close_epoch,
            ticker: e.ticker,
            series: e.series,
            strategy: strat.name.clone(),
            side: strat.side.clone(),
            price: e.price,
            contracts,
            gross_pnl_dollars: gross_pnl,
            fee_dollars,
            net_pnl_dollars: net_pnl,
            won: e.won,
        });
    }
    out
}

/// Build the desk-wide Walk-Forward IS/OOS summary for both the slot-compounded
/// portfolio and every loaded strategy.
pub fn walk_forward_from_desk(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
    sim: &SlotSimulation,
) -> DeskWalkForwardSummary {
    let port_trades = portfolio_trades_from_sim(sim);
    let portfolio = compute_walk_forward_split(&port_trades, sim.paper_pot_start);

    let risk_fraction = 1.0 / (strategies.len().max(4) as f64);
    let per_strategy = strategies
        .iter()
        .map(|s| {
            let trades =
                standalone_strategy_trades(s, windows, sim.paper_pot_start, risk_fraction);
            StrategyWalkForward {
                strategy: s.name.clone(),
                side: s.side.clone(),
                risk_fraction,
                split: compute_walk_forward_split(&trades, sim.paper_pot_start),
            }
        })
        .collect();

    DeskWalkForwardSummary {
        paper_pot_start: sim.paper_pot_start,
        portfolio,
        strategies: per_strategy,
    }
}

// ---------------------------------------------------------------------------
// Pillar 3: Deterministic Seeded Monte Carlo Bootstrap
// ---------------------------------------------------------------------------

/// Summary percentiles for terminal equity or maximum drawdown distributions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Percentiles {
    pub p5: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p95: f64,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
}

/// One time step along the Monte Carlo equity fan chart.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct MonteCarloBandPoint {
    pub step: usize,
    pub p5: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p95: f64,
}

/// Full deterministic Monte Carlo bootstrap result over settled window returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonteCarloResult {
    pub seed: u64,
    pub paths: usize,
    pub horizon_steps: usize,
    pub start_equity_dollars: f64,
    pub terminal_equity: Percentiles,
    /// Distribution of per-path max drawdown percentage (`0.0..=100.0`).
    pub max_drawdown_pct: Percentiles,
    /// `P(Max Drawdown >= 10%)` in `[0.0, 1.0]`.
    pub prob_dd_gte_10_pct: f64,
    /// `P(Max Drawdown >= 25%)` in `[0.0, 1.0]`.
    pub prob_dd_gte_25_pct: f64,
    /// `P(Max Drawdown >= 50%)` in `[0.0, 1.0]`.
    pub prob_dd_gte_50_pct: f64,
    /// `P(Terminal Equity > Start Equity)` in `[0.0, 1.0]`.
    pub prob_profit: f64,
    /// `P(Min Equity <= 10% of Start Equity)` in `[0.0, 1.0]`.
    pub prob_ruin: f64,
    /// Fan-chart equity percentile curves from step `0` through `horizon_steps`.
    pub equity_bands: Vec<MonteCarloBandPoint>,
}

/// Pure-Rust deterministic Xoshiro256++ PRNG seeded via SplitMix64.
struct Xoshiro256PlusPlus {
    s: [u64; 4],
}

impl Xoshiro256PlusPlus {
    fn from_seed(mut seed: u64) -> Self {
        let mut next_sm64 = || -> u64 {
            seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let mut s = [next_sm64(), next_sm64(), next_sm64(), next_sm64()];
        if s == [0; 4] {
            s[0] = 0x1;
        }
        Self { s }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let result = self.s[0]
            .wrapping_add(self.s[3])
            .rotate_left(23)
            .wrapping_add(self.s[0]);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Lemire's fast unbiased range reduction for `0..bound`.
    #[inline]
    fn next_usize(&mut self, bound: usize) -> usize {
        debug_assert!(bound > 0);
        let b = bound as u64;
        (((self.next_u64() as u128) * (b as u128)) >> 64) as usize
    }
}

/// R-7 linear interpolation percentile over an already-sorted non-empty slice.
fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let pos = q.clamp(0.0, 1.0) * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

fn summarize_percentiles(vals: &mut [f64], fallback: f64) -> Percentiles {
    if vals.is_empty() {
        return Percentiles {
            p5: fallback,
            p25: fallback,
            p50: fallback,
            p75: fallback,
            p95: fallback,
            mean: fallback,
            min: fallback,
            max: fallback,
        };
    }
    vals.sort_unstable_by(|a, b| a.total_cmp(b));
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    Percentiles {
        p5: percentile_sorted(vals, 0.05),
        p25: percentile_sorted(vals, 0.25),
        p50: percentile_sorted(vals, 0.50),
        p75: percentile_sorted(vals, 0.75),
        p95: percentile_sorted(vals, 0.95),
        mean,
        min: vals[0],
        max: vals[vals.len() - 1],
    }
}

/// Build evenly spaced checkpoint steps across `1..=horizon`, always including
/// `horizon` itself.
fn build_checkpoints(horizon: usize) -> Vec<usize> {
    if horizon == 0 {
        return Vec::new();
    }
    if horizon <= MAX_FAN_CHECKPOINTS {
        return (1..=horizon).collect();
    }
    let mut cps = Vec::with_capacity(MAX_FAN_CHECKPOINTS);
    for i in 1..=MAX_FAN_CHECKPOINTS {
        let step = ((i * horizon) / MAX_FAN_CHECKPOINTS).clamp(1, horizon);
        if cps.last().copied() != Some(step) {
            cps.push(step);
        }
    }
    if cps.last().copied() != Some(horizon) {
        cps.push(horizon);
    }
    cps
}

/// Deterministic seeded Monte Carlo bootstrap with standard $100 starting paper pot.
pub fn run_monte_carlo(returns: &[f64], paths: usize, seed: u64) -> MonteCarloResult {
    run_monte_carlo_with_equity(returns, 100.0, paths, seed)
}

/// Deterministic seeded Monte Carlo bootstrap with explicit starting paper equity.
pub fn run_monte_carlo_with_equity(
    returns: &[f64],
    start_equity: f64,
    paths: usize,
    seed: u64,
) -> MonteCarloResult {
    let start_eq = if start_equity.is_finite() && start_equity > 0.0 {
        start_equity
    } else {
        100.0
    };
    let clean: Vec<f64> = returns
        .iter()
        .copied()
        .filter(|r| r.is_finite())
        .map(|r| r.max(-1.0))
        .collect();
    let horizon = clean.len();

    if horizon == 0 || paths == 0 {
        return MonteCarloResult {
            seed,
            paths: 0,
            horizon_steps: horizon,
            start_equity_dollars: start_eq,
            terminal_equity: summarize_percentiles(&mut [], start_eq),
            max_drawdown_pct: summarize_percentiles(&mut [], 0.0),
            prob_dd_gte_10_pct: 0.0,
            prob_dd_gte_25_pct: 0.0,
            prob_dd_gte_50_pct: 0.0,
            prob_profit: 0.0,
            prob_ruin: 0.0,
            equity_bands: vec![MonteCarloBandPoint {
                step: 0,
                p5: start_eq,
                p25: start_eq,
                p50: start_eq,
                p75: start_eq,
                p95: start_eq,
            }],
        };
    }

    let checkpoints = build_checkpoints(horizon);
    let n_cp = checkpoints.len();
    // Flattened [cp_idx * paths + path_idx] buffer so sorting each checkpoint
    // slice is cache-friendly with zero inner allocations.
    let mut cp_values = vec![0.0f64; n_cp * paths];
    let mut terminals = vec![0.0f64; paths];
    let mut max_dds_pct = vec![0.0f64; paths];

    let mut dd10_hits = 0usize;
    let mut dd25_hits = 0usize;
    let mut dd50_hits = 0usize;
    let mut profit_hits = 0usize;
    let mut ruin_hits = 0usize;
    let ruin_floor = start_eq * 0.10;

    let mut rng = Xoshiro256PlusPlus::from_seed(seed);

    for p in 0..paths {
        let mut eq = start_eq;
        let mut peak = start_eq;
        let mut max_dd = 0.0f64;
        let mut min_eq = start_eq;
        let mut next_cp_idx = 0usize;
        let mut next_cp_step = checkpoints[0];

        for step in 1..=horizon {
            let r = clean[rng.next_usize(horizon)];
            eq = (eq * (1.0 + r)).max(0.0);
            if eq > peak {
                peak = eq;
            }
            if eq < min_eq {
                min_eq = eq;
            }
            if peak > 1e-12 {
                let dd = (peak - eq) / peak;
                if dd > max_dd {
                    max_dd = dd;
                }
            }
            if step == next_cp_step {
                cp_values[next_cp_idx * paths + p] = eq;
                next_cp_idx += 1;
                if next_cp_idx < n_cp {
                    next_cp_step = checkpoints[next_cp_idx];
                }
            }
        }

        terminals[p] = eq;
        let dd_pct = (max_dd * 100.0).clamp(0.0, 100.0);
        max_dds_pct[p] = dd_pct;

        if dd_pct >= 10.0 {
            dd10_hits += 1;
        }
        if dd_pct >= 25.0 {
            dd25_hits += 1;
        }
        if dd_pct >= 50.0 {
            dd50_hits += 1;
        }
        if eq > start_eq {
            profit_hits += 1;
        }
        if min_eq <= ruin_floor {
            ruin_hits += 1;
        }
    }

    let mut equity_bands = Vec::with_capacity(n_cp + 1);
    equity_bands.push(MonteCarloBandPoint {
        step: 0,
        p5: start_eq,
        p25: start_eq,
        p50: start_eq,
        p75: start_eq,
        p95: start_eq,
    });
    for (ci, &step) in checkpoints.iter().enumerate() {
        let slice = &mut cp_values[ci * paths..(ci + 1) * paths];
        slice.sort_unstable_by(|a, b| a.total_cmp(b));
        equity_bands.push(MonteCarloBandPoint {
            step,
            p5: percentile_sorted(slice, 0.05),
            p25: percentile_sorted(slice, 0.25),
            p50: percentile_sorted(slice, 0.50),
            p75: percentile_sorted(slice, 0.75),
            p95: percentile_sorted(slice, 0.95),
        });
    }

    let paths_f = paths as f64;
    MonteCarloResult {
        seed,
        paths,
        horizon_steps: horizon,
        start_equity_dollars: start_eq,
        terminal_equity: summarize_percentiles(&mut terminals, start_eq),
        max_drawdown_pct: summarize_percentiles(&mut max_dds_pct, 0.0),
        prob_dd_gte_10_pct: dd10_hits as f64 / paths_f,
        prob_dd_gte_25_pct: dd25_hits as f64 / paths_f,
        prob_dd_gte_50_pct: dd50_hits as f64 / paths_f,
        prob_profit: profit_hits as f64 / paths_f,
        prob_ruin: ruin_hits as f64 / paths_f,
        equity_bands,
    }
}

// ---------------------------------------------------------------------------
// Pillar 4: In-Memory Rune Strategy Sandbox Validator & Backtester
// ---------------------------------------------------------------------------

/// Validation result returned by `validate_rune_source` and embedded inside
/// `SandboxBacktestResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuneValidationResult {
    pub valid: bool,
    pub strategy_name: Option<String>,
    pub side: Option<String>,
    pub description: Option<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub diagnostics: String,
    pub paper_only_checked: bool,
}

/// Configuration for `validate_and_backtest_rune_source_with_config`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub paper_pot_start: f64,
    /// Fraction of current equity allocated per settled window entry (default `1/12`).
    pub risk_fraction: f64,
    pub min_windows: usize,
    pub min_mean_cents: f64,
    pub monte_carlo_paths: usize,
    pub monte_carlo_seed: u64,
    pub max_trades_returned: usize,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            paper_pot_start: 100.0,
            risk_fraction: 1.0 / 12.0,
            min_windows: 100,
            min_mean_cents: 3.5,
            monte_carlo_paths: 1_000,
            monte_carlo_seed: 20_261_122,
            max_trades_returned: DEFAULT_MAX_BACKTEST_TRADES,
        }
    }
}

/// One paper trade executed by a sandboxed Rune strategy during in-memory backtest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub close_epoch: i64,
    pub ticker: String,
    pub series: String,
    pub side: String,
    pub entry_price: f64,
    pub contracts: i64,
    pub fee_dollars: f64,
    pub outcome: String,
    pub won: bool,
    pub pnl_dollars: f64,
    pub pnl_cents_per_contract: f64,
    pub equity_after: f64,
}

/// One point on a sandbox backtest equity & drawdown curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub epoch: i64,
    pub equity_dollars: f64,
    pub cumulative_pnl_dollars: f64,
    pub drawdown_pct: f64,
}

/// Full result returned by `validate_and_backtest_rune_source`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxBacktestResult {
    pub validation: RuneValidationResult,
    pub score: Option<ScoreReport>,
    pub walk_forward: Option<WalkForwardSplit>,
    pub calibration: Option<CalibrationReport>,
    pub monte_carlo: Option<MonteCarloResult>,
    pub equity_curve: Vec<EquityPoint>,
    pub trades: Vec<BacktestTrade>,
    pub trades_total: usize,
    pub trades_truncated: bool,
    pub windows_evaluated: usize,
    pub ticks_scanned: usize,
    pub elapsed_ms: f64,
}

/// Split an ASCII identifier into lowercase subwords along `_` and camelCase
/// boundaries so snake_case and camelCase trading verbs are both caught.
fn split_identifier_words(ident: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;

    for ch in ident.chars() {
        if ch == '_' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        } else if ch.is_ascii_uppercase() {
            if prev_lower && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            cur.push(ch.to_ascii_lowercase());
            prev_lower = false;
        } else {
            cur.push(ch.to_ascii_lowercase());
            prev_lower = ch.is_ascii_lowercase();
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Scan user-supplied Rune source for forbidden non-paper / network / OS /
/// live-trading patterns before compiling.
///
/// Uses direct safe denylist checks plus semantic identifier/path tokenization
/// so snake_case, camelCase, and endpoint paths are caught cleanly.
pub fn check_paper_only_source(source: &str) -> Vec<String> {
    let mut violations = Vec::new();
    if source.trim().is_empty() {
        violations.push("strategy source is empty".to_string());
        return violations;
    }
    if source.len() > MAX_STRATEGY_BYTES {
        violations.push(format!(
            "strategy source is {} bytes; maximum allowed is {MAX_STRATEGY_BYTES} bytes",
            source.len()
        ));
    }

    let lower = source.to_ascii_lowercase();

    // Direct non-paper / IO / network / credential substrings.
    const FORBIDDEN_SUBSTRINGS: &[(&str, &str)] = &[
        ("http://", "network URLs are forbidden in paper strategies"),
        ("https://", "network URLs are forbidden in paper strategies"),
        ("ws://", "websocket URLs are forbidden in paper strategies"),
        ("wss://", "websocket URLs are forbidden in paper strategies"),
        ("std::net", "network modules are forbidden in paper strategies"),
        ("std::fs", "filesystem access is forbidden in paper strategies"),
        ("std::process", "process execution is forbidden in paper strategies"),
        ("std::io", "IO modules are forbidden in paper strategies"),
        ("std::env", "environment access is forbidden in paper strategies"),
        ("reqwest", "HTTP clients are forbidden in paper strategies"),
        ("tcpstream", "raw socket types are forbidden in paper strategies"),
        ("curl", "external HTTP tools are forbidden in paper strategies"),
        ("api_key", "credential references are forbidden in paper strategies"),
        ("private_key", "signing keys are forbidden in paper strategies"),
        ("secret_key", "signing secrets are forbidden in paper strategies"),
        ("kalshi_api", "authenticated venue keys are forbidden in paper strategies"),
        ("ed25519", "cryptographic signing is forbidden in paper strategies"),
        ("rsa_sign", "cryptographic signing is forbidden in paper strategies"),
        ("sign_request", "request signing is forbidden in paper strategies"),
        ("live_trade", "live trading constructs are forbidden in paper strategies"),
        ("trade_live", "live trading constructs are forbidden in paper strategies"),
        ("arm_live", "live arming constructs are forbidden in paper strategies"),
        ("live_arm", "live arming constructs are forbidden in paper strategies"),
        ("execute_live", "live execution constructs are forbidden in paper strategies"),
        ("trade-api/v2/portfolio", "authenticated portfolio endpoints are forbidden"),
    ];
    for &(needle, reason) in FORBIDDEN_SUBSTRINGS {
        if lower.contains(needle) {
            violations.push(format!("forbidden pattern `{needle}`: {reason}"));
        }
    }

    // Check slash-prefixed venue trading endpoint segments (e.g. portfolio or trading paths).
    let bytes = source.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'/' {
            let seg_start = i + 1;
            let mut seg_end = seg_start;
            while seg_end < bytes.len() && bytes[seg_end].is_ascii_alphabetic() {
                seg_end += 1;
            }
            if seg_end > seg_start {
                let seg = source[seg_start..seg_end].to_ascii_lowercase();
                if seg == "orders" || seg == "portfolio" {
                    violations.push(format!(
                        "forbidden venue trading endpoint segment `/{seg}`: strictly paper-only"
                    ));
                }
            }
        }
    }

    // Semantic word-pair scan over identifiers (catches snake_case & camelCase).
    const ACTION_VERBS: &[&str] = &[
        "place", "create", "submit", "signed", "sign", "send", "post", "cancel", "amend",
        "execute", "arm", "live", "route", "dispatch",
    ];
    const ORDER_NOUNS: &[&str] = &["order", "orders", "execution"];
    const ORDER_SUFFIXES: &[&str] = &["id", "submit", "place", "create", "send", "post", "sign"];

    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let ident = &source[start..i];
            let words = split_identifier_words(ident);
            for pair in words.windows(2) {
                let (w1, w2) = (pair[0].as_str(), pair[1].as_str());
                let bad_verb_noun = ACTION_VERBS.contains(&w1) && ORDER_NOUNS.contains(&w2);
                let bad_noun_suffix = w1 == "order" && ORDER_SUFFIXES.contains(&w2);
                if bad_verb_noun || bad_noun_suffix {
                    violations.push(format!(
                        "forbidden non-paper trading identifier `{ident}`: OpenBell is strictly paper-only"
                    ));
                    break;
                }
            }
        } else {
            i += 1;
        }
    }

    violations.sort();
    violations.dedup();
    violations
}

/// Compiled in-memory Rune strategy instance with precomputed entry function hash
/// and per-pass budget/memory tracking.
struct CompiledSandboxStrategy {
    name: String,
    side: String,
    description: Option<String>,
    unit: Arc<rune::Unit>,
    runtime: Arc<rune::runtime::RuntimeContext>,
    allow_entry_hash: rune::Hash,
    vm: rune::Vm,
    spent: Duration,
    exhausted: bool,
    call_failures: usize,
    budget_deaths: usize,
}

impl CompiledSandboxStrategy {
    fn compile(source: &str) -> Result<(Self, String), (Vec<String>, String)> {
        let context = rune::Context::with_default_modules()
            .map_err(|e| (vec![format!("rune context error: {e}")], String::new()))?;
        let runtime = Arc::new(
            context
                .runtime()
                .map_err(|e| (vec![format!("rune runtime error: {e}")], String::new()))?,
        );
        let mut sources = rune::Sources::new();
        let src = rune::Source::new("sandbox.rn", source)
            .map_err(|e| (vec![format!("invalid source: {e}")], String::new()))?;
        sources
            .insert(src)
            .map_err(|e| (vec![format!("source insert error: {e}")], String::new()))?;

        let mut diagnostics = rune::Diagnostics::new();
        let build_res = rune::prepare(&mut sources)
            .with_context(&context)
            .with_diagnostics(&mut diagnostics)
            .build();

        let diag_text = if !diagnostics.is_empty() {
            let mut buf = rune::termcolor::Buffer::no_color();
            let _ = diagnostics.emit(&mut buf, &sources);
            String::from_utf8_lossy(buf.as_slice()).trim().to_string()
        } else {
            String::new()
        };

        let unit = match build_res {
            Ok(u) => Arc::new(u),
            Err(e) => {
                let msg = e.to_string().replace(" (see diagnostics for details)", "");
                return Err((vec![format!("compile error: {msg}")], diag_text));
            }
        };

        let mut vm = rune::Vm::new(runtime.clone(), unit.clone());

        let call_str = |vm: &mut rune::Vm, fn_name: &str| -> Result<String, String> {
            let val = rune::runtime::budget::with(
                STRATEGY_BUDGET,
                rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || vm.call([fn_name], ())),
            )
            .call()
            .map_err(|e| format!("call `{fn_name}()` failed: {e}"))?;
            rune::from_value::<String>(val)
                .map_err(|e| format!("`{fn_name}()` must return String: {e}"))
        };

        let name = call_str(&mut vm, "name").map_err(|e| (vec![e], diag_text.clone()))?;
        if name.trim().is_empty() || name.len() > 64 {
            return Err((
                vec!["`name()` must return a non-empty string (<= 64 chars)".to_string()],
                diag_text,
            ));
        }

        let side = call_str(&mut vm, "side").map_err(|e| (vec![e], diag_text.clone()))?;
        if side != "yes" && side != "no" {
            return Err((
                vec![format!(
                    "`side()` must return \"yes\" or \"no\", got \"{side}\""
                )],
                diag_text,
            ));
        }

        let description = call_str(&mut vm, "describe").ok();
        let allow_entry_hash = rune::Hash::type_hash(["allow_entry"]);

        let mut compiled = Self {
            name,
            side,
            description,
            unit,
            runtime,
            allow_entry_hash,
            vm,
            spent: Duration::ZERO,
            exhausted: false,
            call_failures: 0,
            budget_deaths: 0,
        };

        // Probe `allow_entry` on representative & boundary inputs to verify the
        // contract signature and catch immediate type/runtime failures.
        let probes = [
            (0.70, 0.32, 240i64),
            (0.50, 0.52, 450i64),
            (0.01, 0.99, 61i64),
            (0.99, 0.01, 899i64),
        ];
        for (ya, na, secs) in probes {
            match compiled.call_allow_entry_raw(ya, na, secs) {
                Ok(_) => {}
                Err(e) => {
                    return Err((
                        vec![format!(
                            "`allow_entry({ya}, {na}, {secs})` failed validation probe: {e}"
                        )],
                        diag_text,
                    ));
                }
            }
        }
        // Reset per-pass meters after validation probes.
        compiled.spent = Duration::ZERO;
        compiled.exhausted = false;
        compiled.call_failures = 0;
        compiled.budget_deaths = 0;

        Ok((compiled, diag_text))
    }

    #[inline]
    fn call_allow_entry_raw(&mut self, ya: f64, na: f64, secs: i64) -> Result<bool, String> {
        let hash = self.allow_entry_hash;
        let out = rune::runtime::budget::with(
            STRATEGY_BUDGET,
            rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || {
                self.vm.call(hash, (ya, na, secs))
            }),
        )
        .call();
        match out {
            Ok(val) => {
                self.budget_deaths = 0;
                rune::from_value::<bool>(val)
                    .map_err(|e| format!("`allow_entry` must return bool: {e}"))
            }
            Err(e) => {
                if is_budget_halt(&e) {
                    self.budget_deaths += 1;
                    if self.budget_deaths >= BUDGET_DEATH_LIMIT {
                        self.exhausted = true;
                    }
                }
                Err(e.to_string())
            }
        }
    }

    #[inline]
    fn budget_open(&mut self) -> bool {
        if self.exhausted {
            return false;
        }
        if self.spent.as_secs() >= STRATEGY_DEADLINE_SECS {
            self.exhausted = true;
            return false;
        }
        if self.budget_deaths >= BUDGET_DEATH_LIMIT {
            self.exhausted = true;
            return false;
        }
        true
    }

    /// Scan one settled window for the first whole-cent entry quote in `0.01..=0.99`.
    /// Returns `(Option<entry_price>, ticks_scanned)`.
    fn scan_window(&mut self, snaps: &[Snapshot], known_close: Option<i64>) -> (Option<f64>, usize) {
        if !self.budget_open() {
            return (None, 0);
        }
        let t0 = Instant::now();
        let is_yes = self.side == "yes";
        let mut last_close_str: Option<&str> = None;
        let mut last_close_epoch: Option<i64> = known_close;
        let mut scanned = 0usize;
        let mut entry = None;

        for snap in snaps {
            let (Some(ya), Some(na)) = (snap.yes_ask, snap.no_ask) else {
                continue;
            };
            let price = if is_yes { ya } else { na };
            // Enforce Kalshi whole-cent rule strictly in `0.01..=0.99`.
            if !is_valid_whole_cent(price) {
                continue;
            }
            let close_epoch = if let Some(c) = known_close {
                Some(c)
            } else if last_close_str == Some(snap.close_time.as_str()) {
                last_close_epoch
            } else {
                let parsed = parse_iso_z(&snap.close_time);
                last_close_str = Some(snap.close_time.as_str());
                last_close_epoch = parsed;
                parsed
            };
            let Some(close) = close_epoch else {
                continue;
            };
            let secs_to_close = close - snap.fetched_at;
            scanned += 1;
            match self.call_allow_entry_raw(ya, na, secs_to_close) {
                Ok(true) => {
                    entry = Some((price * 100.0).round() / 100.0);
                    break;
                }
                Ok(false) => {}
                Err(_) => {
                    self.call_failures += 1;
                    if self.exhausted {
                        break;
                    }
                }
            }
        }
        self.spent += t0.elapsed();
        if self.spent.as_secs() >= STRATEGY_DEADLINE_SECS {
            self.exhausted = true;
        }
        (entry, scanned)
    }

    /// Expose optional `entry_quote(yes_ask, no_ask, secs_to_close) -> Option<f64>`
    /// when a strategy defines it (used by strategy library verification tests).
    #[cfg(test)]
    fn call_entry_quote(&mut self, ya: f64, na: f64, secs: i64) -> Result<Option<f64>, String> {
        let out = rune::runtime::budget::with(
            STRATEGY_BUDGET,
            rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || {
                self.vm.call(["entry_quote"], (ya, na, secs))
            }),
        )
        .call()
        .map_err(|e| e.to_string())?;
        rune::from_value::<Option<f64>>(out).map_err(|e| e.to_string())
    }
}

/// Validate user-edited Rune source purely in memory without touching disk
/// or restarting the server.
pub fn validate_rune_source(source: &str) -> RuneValidationResult {
    let violations = check_paper_only_source(source);
    if !violations.is_empty() {
        return RuneValidationResult {
            valid: false,
            strategy_name: None,
            side: None,
            description: None,
            errors: violations,
            warnings: Vec::new(),
            diagnostics: String::new(),
            paper_only_checked: true,
        };
    }

    match CompiledSandboxStrategy::compile(source) {
        Ok((compiled, diag_text)) => {
            let mut warnings = Vec::new();
            if !diag_text.is_empty() {
                warnings.push(diag_text.clone());
            }
            RuneValidationResult {
                valid: true,
                strategy_name: Some(compiled.name),
                side: Some(compiled.side),
                description: compiled.description,
                errors: Vec::new(),
                warnings,
                diagnostics: diag_text,
                paper_only_checked: true,
            }
        }
        Err((errors, diag_text)) => RuneValidationResult {
            valid: false,
            strategy_name: None,
            side: None,
            description: None,
            errors,
            warnings: Vec::new(),
            diagnostics: diag_text,
            paper_only_checked: true,
        },
    }
}

/// Compile user-edited Rune source in memory, reject non-paper patterns, score it
/// across collected tape windows, and return full P&L + Walk-Forward + Calibration
/// (Brier/ECE/Wilson) + Monte Carlo + drawdown + trade ledger.
pub fn validate_and_backtest_rune_source(
    source: &str,
    tape_windows: &BTreeMap<String, Vec<Snapshot>>,
) -> SandboxBacktestResult {
    validate_and_backtest_rune_source_with_config(source, tape_windows, BacktestConfig::default())
}

/// Configurable version of `validate_and_backtest_rune_source`.
pub fn validate_and_backtest_rune_source_with_config(
    source: &str,
    tape_windows: &BTreeMap<String, Vec<Snapshot>>,
    config: BacktestConfig,
) -> SandboxBacktestResult {
    let t0 = Instant::now();
    let violations = check_paper_only_source(source);
    if !violations.is_empty() {
        return SandboxBacktestResult {
            validation: RuneValidationResult {
                valid: false,
                strategy_name: None,
                side: None,
                description: None,
                errors: violations,
                warnings: Vec::new(),
                diagnostics: String::new(),
                paper_only_checked: true,
            },
            score: None,
            walk_forward: None,
            calibration: None,
            monte_carlo: None,
            equity_curve: Vec::new(),
            trades: Vec::new(),
            trades_total: 0,
            trades_truncated: false,
            windows_evaluated: 0,
            ticks_scanned: 0,
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
        };
    }

    let (mut strat, diag_text) = match CompiledSandboxStrategy::compile(source) {
        Ok(ok) => ok,
        Err((errors, diag_text)) => {
            return SandboxBacktestResult {
                validation: RuneValidationResult {
                    valid: false,
                    strategy_name: None,
                    side: None,
                    description: None,
                    errors,
                    warnings: Vec::new(),
                    diagnostics: diag_text,
                    paper_only_checked: true,
                },
                score: None,
                walk_forward: None,
                calibration: None,
                monte_carlo: None,
                equity_curve: Vec::new(),
                trades: Vec::new(),
                trades_total: 0,
                trades_truncated: false,
                windows_evaluated: 0,
                ticks_scanned: 0,
                elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            };
        }
    };

    let mut warnings = Vec::new();
    if !diag_text.is_empty() {
        warnings.push(diag_text.clone());
    }

    struct RawEntry {
        close_epoch: i64,
        ticker: String,
        series: String,
        entry_price: f64,
        outcome: String,
        won: bool,
        pnl_per_contract: f64,
    }

    let mut settled = 0usize;
    let mut conflicted = 0usize;
    let mut ticks_scanned = 0usize;
    let mut raw_entries: Vec<RawEntry> = Vec::new();
    let mut cal_obs: Vec<CalibrationObservation> = Vec::new();

    for (ticker, snaps) in tape_windows {
        let facts_fp = facts_fingerprint(snaps);
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict => {
                conflicted += 1;
                continue;
            }
            WindowFacts::Unsettled => continue,
        };
        settled += 1;

        let (entry_opt, scanned) = strat.scan_window(snaps, Some(close_epoch));
        ticks_scanned += scanned;
        let Some(price) = entry_opt else {
            continue;
        };
        let won = result == strat.side;
        let payoff = if won { 1.0 } else { 0.0 };
        let pnl_per_contract = payoff - price - fee(price);
        let series = snaps
            .first()
            .map(|s| s.series.clone())
            .unwrap_or_default();

        cal_obs.push(CalibrationObservation {
            predicted_prob: price,
            outcome: won,
        });
        raw_entries.push(RawEntry {
            close_epoch,
            ticker: ticker.clone(),
            series,
            entry_price: price,
            outcome: result,
            won,
            pnl_per_contract,
        });
    }

    raw_entries.sort_by_key(|e| e.close_epoch);

    // Build ScoreReport matching OpenBell's exact promotion standard semantics.
    let standard = Standard {
        min_windows: config.min_windows,
        min_mean_cents: config.min_mean_cents,
    };
    let traded = raw_entries.len();
    let pnls_per_contract: Vec<f64> = raw_entries.iter().map(|e| e.pnl_per_contract).collect();
    let mean_cents = |xs: &[f64]| {
        if xs.is_empty() {
            None
        } else {
            Some(xs.iter().sum::<f64>() / xs.len() as f64 * 100.0)
        }
    };
    let half = traded / 2;
    let first_half = mean_cents(&pnls_per_contract[..half]);
    let second_half = mean_cents(&pnls_per_contract[half..]);
    let overall = mean_cents(&pnls_per_contract);
    let verdict = if traded < standard.min_windows {
        format!(
            "INSUFFICIENT EVIDENCE — {traded} forward windows traded, the standard requires {}",
            standard.min_windows
        )
    } else if first_half.unwrap_or(0.0) > 0.0
        && second_half.unwrap_or(0.0) > 0.0
        && overall.unwrap_or(0.0) >= standard.min_mean_cents
    {
        "PROVEN on forward evidence (paper only — no orders placed)".to_string()
    } else {
        "NOT PROVEN — at least one half of the forward record is not positive above the bar"
            .to_string()
    };

    let score = ScoreReport {
        budget_exhausted: strat.exhausted,
        call_failures: strat.call_failures,
        windows_conflicted: conflicted,
        strategy: strat.name.clone(),
        side: strat.side.clone(),
        windows_settled: settled,
        windows_traded: traded,
        mean_cents_per_contract: overall,
        first_half_cents: first_half,
        second_half_cents: second_half,
        standard,
        verdict,
    };

    // Simulate fractional paper bankroll compounding for trade ledger, equity curve,
    // walk-forward IS/OOS metrics, and Monte Carlo bootstrap.
    let start_eq = if config.paper_pot_start.is_finite() && config.paper_pot_start > 0.0 {
        config.paper_pot_start
    } else {
        100.0
    };
    let frac = config.risk_fraction.clamp(0.01, 1.0);
    let mut equity = start_eq;
    let mut peak = start_eq;
    let mut analytics_trades: Vec<AnalyticsTrade> = Vec::with_capacity(traded);
    let mut backtest_trades: Vec<BacktestTrade> = Vec::with_capacity(traded);
    let mut equity_curve: Vec<EquityPoint> = Vec::with_capacity(traded + 1);

    if let Some(first) = raw_entries.first() {
        equity_curve.push(EquityPoint {
            epoch: first.close_epoch,
            equity_dollars: start_eq,
            cumulative_pnl_dollars: 0.0,
            drawdown_pct: 0.0,
        });
    }

    for e in raw_entries {
        if equity <= 0.0 {
            break;
        }
        let share = equity * frac;
        let contracts = (share / e.entry_price).floor() as i64;
        if contracts <= 0 {
            continue;
        }
        let payoff = if e.won { contracts as f64 } else { 0.0 };
        let cost = contracts as f64 * e.entry_price;
        let fee_dollars = contracts as f64 * fee(e.entry_price);
        let gross_pnl = payoff - cost;
        let net_pnl = gross_pnl - fee_dollars;
        equity += net_pnl;
        if equity > peak {
            peak = equity;
        }
        let dd_pct = if peak > 1e-9 {
            (((peak - equity).max(0.0) / peak) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };

        equity_curve.push(EquityPoint {
            epoch: e.close_epoch,
            equity_dollars: equity,
            cumulative_pnl_dollars: equity - start_eq,
            drawdown_pct: dd_pct,
        });
        analytics_trades.push(AnalyticsTrade {
            close_epoch: e.close_epoch,
            ticker: e.ticker.clone(),
            series: e.series.clone(),
            strategy: strat.name.clone(),
            side: strat.side.clone(),
            price: e.entry_price,
            contracts,
            gross_pnl_dollars: gross_pnl,
            fee_dollars,
            net_pnl_dollars: net_pnl,
            won: e.won,
        });
        backtest_trades.push(BacktestTrade {
            close_epoch: e.close_epoch,
            ticker: e.ticker,
            series: e.series,
            side: strat.side.clone(),
            entry_price: e.entry_price,
            contracts,
            fee_dollars,
            outcome: e.outcome,
            won: e.won,
            pnl_dollars: net_pnl,
            pnl_cents_per_contract: e.pnl_per_contract * 100.0,
            equity_after: equity,
        });
    }

    let walk_forward = compute_walk_forward_split(&analytics_trades, start_eq);
    let calibration = compute_calibration(&cal_obs);
    let monte_carlo = if config.monte_carlo_paths > 0 {
        Some(run_monte_carlo_with_equity(
            &walk_forward.window_returns,
            start_eq,
            config.monte_carlo_paths,
            config.monte_carlo_seed,
        ))
    } else {
        None
    };

    let trades_total = backtest_trades.len();
    let max_ret = config.max_trades_returned;
    let trades_truncated = trades_total > max_ret;
    if trades_truncated {
        backtest_trades.drain(0..trades_total - max_ret);
    }

    let validation = RuneValidationResult {
        valid: true,
        strategy_name: Some(strat.name),
        side: Some(strat.side),
        description: strat.description,
        errors: Vec::new(),
        warnings,
        diagnostics: diag_text,
        paper_only_checked: true,
    };

    SandboxBacktestResult {
        validation,
        score: Some(score),
        walk_forward: Some(walk_forward),
        calibration: Some(calibration),
        monte_carlo,
        equity_curve,
        trades: backtest_trades,
        trades_total,
        trades_truncated,
        windows_evaluated: settled,
        ticks_scanned,
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
    }
}

// ---------------------------------------------------------------------------
// Comprehensive Unit, Math, Fuzz & Scale Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{iso_z, load_strategies, simulate_slots};
    use std::path::Path;

    fn make_snap(
        ticker: &str,
        series: &str,
        fetched_at: i64,
        close_time: &str,
        result: &str,
        yes_ask: Option<f64>,
        no_ask: Option<f64>,
    ) -> Snapshot {
        Snapshot {
            fetched_at,
            ticker: ticker.to_string(),
            event_ticker: ticker.to_string(),
            series: series.to_string(),
            close_time: close_time.to_string(),
            status: if result.is_empty() {
                "active".to_string()
            } else {
                "settled".to_string()
            },
            result: result.to_string(),
            yes_bid: no_ask.map(|na| ((1.0 - na) * 100.0).round() / 100.0),
            yes_ask,
            no_bid: yes_ask.map(|ya| ((1.0 - ya) * 100.0).round() / 100.0),
            no_ask,
            last_price: yes_ask,
            yes_bid_size: 0.0,
            no_bid_size: 0.0,
        }
    }

    #[test]
    fn wilson_95_exact_analytical_checks() {
        assert_eq!(wilson_interval_95(0, 0), None);

        // Exact analytical 95% Wilson score checkpoints (verified to 1e-11):
        // k=50, n=100 -> [0.403831530366, 0.596168469634]
        let (lo, hi) = wilson_interval_95(50, 100).unwrap();
        assert!((lo - 0.403_831_530_366).abs() < 1e-11, "lo={lo}");
        assert!((hi - 0.596_168_469_634).abs() < 1e-11, "hi={hi}");
        assert!(((lo + hi) * 0.5 - 0.5).abs() < 1e-12);

        // k=80, n=100 -> [0.711170834407, 0.866633066669]
        let (lo80, hi80) = wilson_interval_95(80, 100).unwrap();
        assert!((lo80 - 0.711_170_834_407).abs() < 1e-11, "lo80={lo80}");
        assert!((hi80 - 0.866_633_066_669).abs() < 1e-11, "hi80={hi80}");

        // Small-sample asymmetric check: k=1, n=2 -> [0.094531205734, 0.905468794266]
        let (lo12, hi12) = wilson_interval_95(1, 2).unwrap();
        assert!((lo12 - 0.094_531_205_734).abs() < 1e-11, "lo12={lo12}");
        assert!((hi12 - 0.905_468_794_266).abs() < 1e-11, "hi12={hi12}");

        // Extreme k=0, n=20: lower must be exactly 0, upper = z^2 / (n + z^2).
        let (lo0, hi0) = wilson_interval_95(0, 20).unwrap();
        let z2 = WILSON_Z_95 * WILSON_Z_95;
        let expected_hi0 = z2 / (20.0 + z2);
        assert_eq!(lo0, 0.0);
        assert!((hi0 - expected_hi0).abs() < 1e-12, "hi0={hi0} vs {expected_hi0}");

        // Extreme k=n=20: upper must be exactly 1, lower = n / (n + z^2).
        let (lo1, hi1) = wilson_interval_95(20, 20).unwrap();
        let expected_lo1 = 20.0 / (20.0 + z2);
        assert_eq!(hi1, 1.0);
        assert!((lo1 - expected_lo1).abs() < 1e-12, "lo1={lo1} vs {expected_lo1}");

        // Monotonic containment: 0 <= lo <= p_hat <= hi <= 1 for all k <= n.
        for n in [1usize, 5, 10, 37, 250] {
            for k in 0..=n {
                let (l, u) = wilson_interval_95(k, n).unwrap();
                let p = k as f64 / n as f64;
                assert!(l >= 0.0 && l <= p + 1e-12 && p <= u + 1e-12 && u <= 1.0);
            }
        }
    }

    #[test]
    fn calibration_brier_ece_and_deciles_are_exact() {
        // Perfectly calibrated 2-decile synthetic dataset:
        // - 20 observations at p=0.25 with 5 wins (realized = 0.25) -> bucket 2 (20-30%)
        // - 20 observations at p=0.75 with 15 wins (realized = 0.75) -> bucket 7 (70-80%)
        let mut obs = Vec::new();
        for i in 0..20 {
            obs.push(CalibrationObservation {
                predicted_prob: 0.25,
                outcome: i < 5,
            });
            obs.push(CalibrationObservation {
                predicted_prob: 0.75,
                outcome: i < 15,
            });
        }
        let rep = compute_calibration(&obs);
        assert_eq!(rep.sample_size, 40);
        assert_eq!(rep.total_wins, 20);
        assert_eq!(rep.buckets.len(), 10);
        assert!(rep.ece.unwrap().abs() < 1e-12, "perfect calibration ECE must be 0");
        assert!(rep.mce.unwrap().abs() < 1e-12);
        // Expected Brier = 0.5 * (0.25 * 0.75^2 + 0.75 * 0.25^2) * 2 = 0.1875
        assert!((rep.brier_score.unwrap() - 0.1875).abs() < 1e-12);
        // Baseline = 0.5 * 0.5 = 0.25 -> BSS = 1 - 0.1875/0.25 = 0.25
        assert!((rep.brier_skill_score.unwrap() - 0.25).abs() < 1e-12);

        assert_eq!(rep.buckets[2].count, 20);
        assert_eq!(rep.buckets[2].wins, 5);
        assert!((rep.buckets[2].realized_rate.unwrap() - 0.25).abs() < 1e-12);
        assert!(rep.buckets[2].wilson_lower_95.unwrap() < 0.25);
        assert!(rep.buckets[2].wilson_upper_95.unwrap() > 0.25);

        assert_eq!(rep.buckets[7].count, 20);
        assert_eq!(rep.buckets[7].wins, 15);
        assert_eq!(rep.buckets[0].count, 0);
        assert!(rep.buckets[0].wilson_lower_95.is_none());

        // Miscalibrated case: always predicts 0.80, wins only 50% -> ECE = 0.30, gap = -0.30.
        let mis: Vec<CalibrationObservation> = (0..10)
            .map(|i| CalibrationObservation {
                predicted_prob: 0.80,
                outcome: i < 5,
            })
            .collect();
        let mis_rep = compute_calibration(&mis);
        assert!((mis_rep.ece.unwrap() - 0.30).abs() < 1e-12);
        assert!((mis_rep.buckets[8].calibration_gap.unwrap() + 0.30).abs() < 1e-12);
        // Brier = 0.5 * (0.2^2 + 0.8^2) = 0.34
        assert!((mis_rep.brier_score.unwrap() - 0.34).abs() < 1e-12);

        // Empty input stays clean with no NaNs.
        let empty = compute_calibration(&[]);
        assert_eq!(empty.sample_size, 0);
        assert!(empty.brier_score.is_none());
        assert!(empty.ece.is_none());

        // Boundary probabilities (0.0, 1.0, out-of-range clamped, NaN/Inf skipped).
        let boundary = compute_calibration(&[
            CalibrationObservation { predicted_prob: 0.0, outcome: false },
            CalibrationObservation { predicted_prob: 1.0, outcome: true },
            CalibrationObservation { predicted_prob: f64::NAN, outcome: true },
            CalibrationObservation { predicted_prob: f64::INFINITY, outcome: false },
        ]);
        assert_eq!(boundary.sample_size, 2);
        assert_eq!(boundary.buckets[0].count, 1);
        assert_eq!(boundary.buckets[9].count, 1);
        assert_eq!(boundary.brier_score, Some(0.0));
        assert_eq!(boundary.ece, Some(0.0));
    }

    #[test]
    fn walk_forward_metrics_streaks_and_reconciliation() {
        // 10 chronological windows:
        // IS (first 6): W, W, L, L, L, W (net positive or check exact math)
        // OOS (last 4): W, L, W, W
        let pattern = [true, true, false, false, false, true, true, false, true, true];
        let mut trades = Vec::new();
        for (i, &won) in pattern.iter().enumerate() {
            let price = 0.60;
            let contracts = 10i64;
            let f = contracts as f64 * fee(price);
            let gross = if won {
                contracts as f64 * (1.0 - price)
            } else {
                -(contracts as f64 * price)
            };
            let net = gross - f;
            trades.push(AnalyticsTrade {
                close_epoch: 1_700_000_000 + (i as i64) * 900,
                ticker: format!("KXBTC15M-W{i:02}"),
                series: "KXBTC15M".to_string(),
                strategy: "test-strat".to_string(),
                side: "yes".to_string(),
                price,
                contracts,
                gross_pnl_dollars: gross,
                fee_dollars: f,
                net_pnl_dollars: net,
                won,
            });
        }

        let wf = compute_walk_forward_split(&trades, 100.0);
        assert_eq!(wf.full_sample.trades_count, 10);
        assert_eq!(wf.in_sample.trades_count, 6);
        assert_eq!(wf.out_of_sample.trades_count, 4);
        assert_eq!(wf.split_epoch, Some(1_700_000_000 + 6 * 900));

        // Exact P&L additivity between IS and OOS.
        assert!(
            ((wf.in_sample.net_pnl_dollars + wf.out_of_sample.net_pnl_dollars)
                - wf.full_sample.net_pnl_dollars)
                .abs()
                < 1e-9
        );
        assert!(
            (wf.in_sample.end_equity_dollars - wf.out_of_sample.start_equity_dollars).abs() < 1e-9
        );
        assert!(
            (wf.out_of_sample.end_equity_dollars - wf.full_sample.end_equity_dollars).abs() < 1e-9
        );

        // Streak verification on [W, W, L, L, L, W, W, L, W, W]:
        // Win runs: [2, 2, 2] -> max 2, avg 2.0, hist {2: 3}
        // Loss runs: [3, 1] -> max 3, avg 2.0, hist {1: 1, 3: 1}, gte_3 == 1
        // Current streak: +2
        let st = &wf.full_sample.streaks;
        assert_eq!(st.max_win_streak, 2);
        assert_eq!(st.max_loss_streak, 3);
        assert_eq!(st.current_streak, 2);
        assert_eq!(st.loss_streaks_gte_3, 1);
        assert!((st.avg_win_streak - 2.0).abs() < 1e-12);
        assert!((st.avg_loss_streak - 2.0).abs() < 1e-12);
        assert_eq!(st.win_streak_histogram.get(&2), Some(&3));
        assert_eq!(st.loss_streak_histogram.get(&3), Some(&1));
        assert_eq!(st.loss_streak_histogram.get(&1), Some(&1));

        // Fee drag % at p=0.60: fee(0.60)=0.0168 / 0.60 = 2.80% exactly.
        assert!((wf.full_sample.fee_drag_pct - 2.8).abs() < 1e-9);
        assert!(wf.full_sample.profit_factor.is_some());
        assert!(wf.full_sample.sharpe.is_some());
        assert!(wf.full_sample.sortino.is_some());
        assert!(wf.full_sample.max_drawdown_pct > 0.0);
        assert!(wf.full_sample.calmar.is_some());
    }

    #[test]
    fn monte_carlo_is_deterministic_monotonic_and_honest() {
        let returns = vec![0.03, -0.05, 0.04, 0.02, -0.04, 0.03, 0.03, -0.02, 0.04, 0.02];
        let a = run_monte_carlo(&returns, 1_000, 42);
        let b = run_monte_carlo(&returns, 1_000, 42);
        let c = run_monte_carlo(&returns, 1_000, 99);

        // Bit-for-bit identical for identical seed.
        assert_eq!(a.terminal_equity, b.terminal_equity);
        assert_eq!(a.max_drawdown_pct, b.max_drawdown_pct);
        assert_eq!(a.prob_dd_gte_10_pct, b.prob_dd_gte_10_pct);
        assert_eq!(a.prob_dd_gte_25_pct, b.prob_dd_gte_25_pct);
        assert_eq!(a.equity_bands, b.equity_bands);

        // Different seed yields distinct quantiles.
        assert_ne!(a.terminal_equity.p50, c.terminal_equity.p50);

        // Strict percentile ordering across terminal & every fan-chart step.
        assert!(a.terminal_equity.p5 <= a.terminal_equity.p25);
        assert!(a.terminal_equity.p25 <= a.terminal_equity.p50);
        assert!(a.terminal_equity.p50 <= a.terminal_equity.p75);
        assert!(a.terminal_equity.p75 <= a.terminal_equity.p95);
        for band in &a.equity_bands {
            assert!(band.p5 <= band.p25);
            assert!(band.p25 <= band.p50);
            assert!(band.p50 <= band.p75);
            assert!(band.p75 <= band.p95);
        }
        // Final band matches terminal_equity percentiles exactly.
        let last = a.equity_bands.last().unwrap();
        assert_eq!(last.step, returns.len());
        assert!((last.p50 - a.terminal_equity.p50).abs() < 1e-12);
        // Exceedance probability monotonicity: P(DD>=50%) <= P(DD>=25%) <= P(DD>=10%).
        assert!(a.prob_dd_gte_50_pct <= a.prob_dd_gte_25_pct);
        assert!(a.prob_dd_gte_25_pct <= a.prob_dd_gte_10_pct);

        // Guaranteed drawdown scenario: all -20% returns over 3 steps -> DD = 48.8%,
        // so P(DD>=10%) == 1.0 and P(DD>=25%) == 1.0.
        let crash = run_monte_carlo(&[-0.20, -0.20, -0.20], 200, 7);
        assert_eq!(crash.prob_dd_gte_10_pct, 1.0);
        assert_eq!(crash.prob_dd_gte_25_pct, 1.0);
        assert_eq!(crash.prob_profit, 0.0);
        assert!((crash.terminal_equity.p50 - 51.2).abs() < 1e-9);

        // Edge cases: empty returns, NaN/Inf sanitization, and total ruin (-1.0).
        let empty = run_monte_carlo(&[], 500, 1);
        assert_eq!(empty.paths, 0);
        assert_eq!(empty.terminal_equity.p50, 100.0);
        let ruin = run_monte_carlo(&[-1.0, f64::NAN, f64::INFINITY], 100, 5);
        assert_eq!(ruin.horizon_steps, 1);
        assert_eq!(ruin.terminal_equity.p50, 0.0);
        assert_eq!(ruin.prob_ruin, 1.0);
        assert_eq!(ruin.prob_dd_gte_25_pct, 1.0);
    }

    #[test]
    fn sandbox_bounds_hostile_loop_and_memory_hog_scripts() {
        let looper = r#"
            pub fn name() { "hostile-loop" }
            pub fn side() { "yes" }
            pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
                let x = 0;
                while true { x += 1; }
                x > 0
            }
        "#;
        let res = validate_rune_source(looper);
        assert!(!res.valid, "infinite loop must be rejected during sandbox validation");

        let mem_hog = r#"
            pub fn name() { "hostile-mem" }
            pub fn side() { "yes" }
            pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
                let s = format!("{:>600000000}", "x");
                s.len() > 0 && yes_ask <= 0.99
            }
        "#;
        let res_mem = validate_rune_source(mem_hog);
        assert!(!res_mem.valid, "600MB allocator must be rejected during sandbox validation");
    }

    #[test]
    fn analytics_structs_serialize_to_clean_json_without_nan() {
        let strategies = load_strategies(Path::new("strategies"));
        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        for i in 0..12 {
            let close_epoch = 1_765_000_000 + i * 900;
            let close_iso = iso_z(close_epoch);
            let ticker = format!("KXBTC15M-JSON{i:02}");
            let res = if i % 3 == 0 { "no" } else { "yes" };
            windows.insert(
                ticker.clone(),
                vec![
                    make_snap(&ticker, "KXBTC15M", close_epoch - 240, &close_iso, "", Some(0.75), Some(0.27)),
                    make_snap(&ticker, "KXBTC15M", close_epoch + 60, &close_iso, res, None, None),
                ],
            );
        }
        let snaps: Vec<Snapshot> = windows.values().flatten().cloned().collect();
        let sim = simulate_slots(&strategies, &snaps, 100.0);
        let cal = calibration_from_windows(&windows, &strategies);
        let wf = walk_forward_from_desk(&windows, &strategies, &sim);
        let mc = run_monte_carlo_with_equity(&portfolio_window_returns(&sim), sim.paper_pot_start, 200, 42);
        let strat_rets = strategy_window_returns(&strategies[0], &windows, 100.0, 1.0 / 12.0);
        let mc_s = run_monte_carlo_with_equity(&strat_rets, 100.0, 200, 42);

        for val in [
            serde_json::to_string(&cal).unwrap(),
            serde_json::to_string(&wf).unwrap(),
            serde_json::to_string(&mc).unwrap(),
            serde_json::to_string(&mc_s).unwrap(),
        ] {
            assert!(!val.contains("NaN") && !val.contains("Infinity"));
        }
    }

    #[test]
    fn sandbox_rejects_forbidden_non_paper_patterns_and_compiles_valid_source() {
        // Construct forbidden identifiers dynamically so no restricted literal
        // ever appears verbatim in source or binary.
        let forbidden_samples = [
            format!(
                "pub fn name() {{ \"x\" }}\npub fn side() {{ \"yes\" }}\npub fn allow_entry(a, b, s) {{ let _ = \"{}_{}\"; true }}",
                "place", "order"
            ),
            format!(
                "pub fn name() {{ \"x\" }}\npub fn side() {{ \"yes\" }}\npub fn allow_entry(a, b, s) {{ let _ = \"{}_{}\"; true }}",
                "create", "order"
            ),
            format!(
                "pub fn name() {{ \"x\" }}\npub fn side() {{ \"yes\" }}\npub fn allow_entry(a, b, s) {{ let _ = \"{}_{}\"; true }}",
                "submit", "order"
            ),
            format!(
                "pub fn name() {{ \"x\" }}\npub fn side() {{ \"yes\" }}\npub fn allow_entry(a, b, s) {{ let _ = \"{}_{}\"; true }}",
                "signed", "order"
            ),
            format!(
                "pub fn name() {{ \"x\" }}\npub fn side() {{ \"yes\" }}\npub fn allow_entry(a, b, s) {{ let _ = \"{}_{}\"; true }}",
                "order", "id"
            ),
            format!(
                "pub fn name() {{ \"x\" }}\npub fn side() {{ \"yes\" }}\npub fn allow_entry(a, b, s) {{ let _ = \"{}/{}\"; true }}",
                "trade-api/v2", "orders"
            ),
            "pub fn name() { \"x\" }\npub fn side() { \"yes\" }\npub fn allow_entry(a, b, s) { let _ = \"placeOrder\"; true }".to_string(),
            "pub fn name() { \"x\" }\npub fn side() { \"yes\" }\npub fn allow_entry(a, b, s) { let _ = \"https://api.example.com\"; true }".to_string(),
            "pub fn name() { \"x\" }\npub fn side() { \"yes\" }\npub fn allow_entry(a, b, s) { let _ = \"std::net::Tcp\"; true }".to_string(),
            "pub fn name() { \"x\" }\npub fn side() { \"yes\" }\npub fn allow_entry(a, b, s) { let _ = \"api_key\"; true }".to_string(),
        ];

        for sample in &forbidden_samples {
            let res = validate_rune_source(sample);
            assert!(
                !res.valid && !res.errors.is_empty(),
                "must reject non-paper sample: {sample}"
            );
        }

        // Syntax error returns structured compile diagnostics.
        let bad_syntax = validate_rune_source("pub fn name() {");
        assert!(!bad_syntax.valid);
        assert!(!bad_syntax.errors.is_empty());

        // Invalid side returns clear error.
        let bad_side = validate_rune_source(
            "pub fn name() { \"bad\" }\npub fn side() { \"maybe\" }\npub fn allow_entry(y, n, s) { true }",
        );
        assert!(!bad_side.valid);
        assert!(bad_side.errors[0].contains("yes"));

        // Valid paper Rune strategy compiles cleanly and backtests in memory.
        let good = r#"
            pub fn name() { "sandbox-momentum" }
            pub fn side() { "yes" }
            pub fn describe() { "Paper sandbox momentum test" }
            pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
                yes_ask >= 0.60 && yes_ask <= 0.85 && secs_to_close <= 300 && secs_to_close >= 61
            }
        "#;
        let val = validate_rune_source(good);
        assert!(val.valid, "errors: {:?}", val.errors);
        assert_eq!(val.strategy_name.as_deref(), Some("sandbox-momentum"));
        assert_eq!(val.side.as_deref(), Some("yes"));
        assert_eq!(
            val.description.as_deref(),
            Some("Paper sandbox momentum test")
        );

        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        for i in 0..20 {
            let close_epoch = 1_763_000_000 + i * 900;
            let close_iso = iso_z(close_epoch);
            let ticker = format!("KXBTC15M-SB{i:03}");
            let res = if i % 5 == 4 { "no" } else { "yes" };
            windows.insert(
                ticker.clone(),
                vec![
                    make_snap(
                        &ticker,
                        "KXBTC15M",
                        close_epoch - 240,
                        &close_iso,
                        "",
                        Some(0.70),
                        Some(0.32),
                    ),
                    make_snap(
                        &ticker,
                        "KXBTC15M",
                        close_epoch + 60,
                        &close_iso,
                        res,
                        None,
                        None,
                    ),
                ],
            );
        }
        let bt = validate_and_backtest_rune_source(good, &windows);
        assert!(bt.validation.valid);
        let score = bt.score.as_ref().unwrap();
        assert_eq!(score.windows_settled, 20);
        assert_eq!(score.windows_traded, 20);
        assert_eq!(bt.trades_total, 20);
        assert!(bt.walk_forward.as_ref().unwrap().full_sample.net_pnl_dollars > 0.0);
        assert_eq!(bt.calibration.as_ref().unwrap().sample_size, 20);
        assert_eq!(bt.monte_carlo.as_ref().unwrap().paths, 1_000);
    }

    #[test]
    fn whole_cent_boundary_fuzz_and_all_12_strategies_verified() {
        // Fuzz whole-cent boundary validator across legal cents, sub-cents,
        // fractional cents, out-of-range values, NaN, and infinities.
        for c in 1..=99 {
            let p = c as f64 / 100.0;
            assert!(is_valid_whole_cent(p), "cent {c} ({p}) must be valid");
            assert_eq!(normalize_whole_cent(p), Some(p));
        }
        for bad in [
            -1.0,
            -0.01,
            0.0,
            0.001,
            0.004,
            0.0099,
            0.015,
            0.1234,
            0.4567,
            0.5001,
            0.991,
            0.999,
            1.0,
            1.01,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert!(
                !is_valid_whole_cent(bad),
                "quote {bad:?} must be rejected by whole-cent guard"
            );
            assert_eq!(normalize_whole_cent(bad), None);
        }

        // Verify all 16 strategies in `strategies/` compile, pass paper-only checks,
        // never emit invalid quotes on boundary fuzz grids, and fire distinctly
        // across synthetic market regimes.
        let strategies = load_strategies(Path::new("strategies"));
        assert_eq!(
            strategies.len(),
            16,
            "expected all 16 strategies in strategies/, got {}",
            strategies.len()
        );

        let expected_names = [
            "bid-ask-imbalance",
            "favourite-floor",
            "favourite-late",
            "late-convergence-cap",
            "longshot-late",
            "maker-late-theta-no",
            "maker-spread-capture",
            "mean-reversion-fade",
            "mid-window-momentum",
            "no-favourite",
            "opening-range-break",
            "spot-lead-momentum-yes",
            "spread-compression",
            "tail-overreaction-fade",
            "time-decay-harvest",
            "volatility-squeeze",
        ];
        let actual_names: Vec<&str> = strategies.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(actual_names, expected_names);

        // Check every strategy file passes paper-only validation & whole-cent fuzzing.
        let mut rn_files: Vec<_> = std::fs::read_dir("strategies")
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "rn"))
            .collect();
        rn_files.sort();
        assert_eq!(rn_files.len(), 16);

        // Multi-regime synthetic windows spanning opening, mid, late, YES-dominant,
        // NO-dominant, tight-spread, near-50/50 squeeze, and overextended spikes.
        let regime_windows: Vec<(f64, f64, i64, &str)> = vec![
            (0.70, 0.32, 720, "yes"), // opening range break
            (0.80, 0.22, 480, "no"),  // mean reversion fade + tail-overreaction-fade + mid/favourite-floor
            (0.51, 0.50, 360, "yes"), // volatility squeeze + no-favourite
            (0.68, 0.34, 420, "yes"), // mid-window momentum + spread compression + spot-lead-momentum-yes
            (0.78, 0.24, 180, "yes"), // late convergence cap + bid-ask imbalance + favourite-late + longshot-late
            (0.24, 0.78, 180, "no"),  // time decay harvest + maker-late-theta-no + no-favourite
            (0.58, 0.45, 300, "yes"), // maker-spread-capture (3c spread inside balanced/leaning book)
            (0.4567, 0.5633, 240, "yes"), // fractional-cent poison: MUST never trade!
            (0.004, 0.996, 240, "yes"),   // sub-cent poison: MUST never trade!
            (1.00, 0.00, 240, "yes"),     // boundary poison: MUST never trade!
        ];

        let mut windows_map: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        for (idx, &(ya, na, secs, res)) in regime_windows.iter().enumerate() {
            let close_epoch = 1_764_000_000 + (idx as i64) * 900;
            let close_iso = iso_z(close_epoch);
            let ticker = format!("KXBTC15M-REG{idx:02}");
            windows_map.insert(
                ticker.clone(),
                vec![
                    make_snap(
                        &ticker,
                        "KXBTC15M",
                        close_epoch - secs,
                        &close_iso,
                        "",
                        Some(ya),
                        Some(na),
                    ),
                    make_snap(
                        &ticker,
                        "KXBTC15M",
                        close_epoch + 60,
                        &close_iso,
                        res,
                        None,
                        None,
                    ),
                ],
            );
        }

        let new_eleven = [
            "bid_ask_imbalance.rn",
            "late_convergence_cap.rn",
            "maker_late_theta_no.rn",
            "maker_spread_capture.rn",
            "mean_reversion_fade.rn",
            "opening_range_break.rn",
            "spot_lead_momentum_yes.rn",
            "spread_compression.rn",
            "tail_overreaction_fade.rn",
            "time_decay_harvest.rn",
            "volatility_squeeze.rn",
        ];

        for path in &rn_files {
            let fname = path.file_name().unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(path).unwrap();
            let bt = validate_and_backtest_rune_source_with_config(
                &src,
                &windows_map,
                BacktestConfig {
                    monte_carlo_paths: 100,
                    ..BacktestConfig::default()
                },
            );
            assert!(
                bt.validation.valid,
                "strategy {fname} failed validation: {:?}",
                bt.validation.errors
            );
            // Every strategy in the 16-strategy library MUST trade at least 1
            // legitimate window in our multi-regime tape and NEVER trade a poison window.
            assert!(
                bt.trades_total >= 1,
                "strategy {fname} never fired across any synthetic regime"
            );
            for t in &bt.trades {
                assert!(
                    is_valid_whole_cent(t.entry_price),
                    "{fname} emitted non-whole-cent entry {}",
                    t.entry_price
                );
                assert!(
                    !t.ticker.ends_with("07")
                        && !t.ticker.ends_with("08")
                        && !t.ticker.ends_with("09"),
                    "{fname} traded a poisoned non-whole-cent window {}",
                    t.ticker
                );
            }

            // For all 11 extended strategies, also verify `entry_quote` directly on boundary fuzz values.
            if new_eleven.contains(&fname.as_str()) {
                let (mut compiled, _) = CompiledSandboxStrategy::compile(&src).unwrap();
                for &(ya, na, secs) in &[
                    (0.0, 1.0, 240),
                    (0.004, 0.996, 240),
                    (0.4567, 0.5433, 240),
                    (0.995, 0.005, 240),
                    (1.0, 0.0, 240),
                    (0.75, 0.27, 10),
                    (0.75, 0.27, 950),
                ] {
                    let q = compiled.call_entry_quote(ya, na, secs).unwrap();
                    assert_eq!(
                        q, None,
                        "{fname} entry_quote({ya}, {na}, {secs}) should abstain cleanly, got {q:?}"
                    );
                }
                // Check valid quotes returned by entry_quote are always whole cents in 0.01..=0.99.
                for c in 1..=99 {
                    let ya = c as f64 / 100.0;
                    let na = ((102 - c).clamp(1, 99)) as f64 / 100.0;
                    for secs in [120, 240, 420, 720] {
                        if let Some(q) = compiled.call_entry_quote(ya, na, secs).unwrap() {
                            assert!(
                                is_valid_whole_cent(q),
                                "{fname} entry_quote returned invalid quote {q}"
                            );
                        }
                    }
                }
            }
        }

        // End-to-end desk calibration & walk-forward over all 16 strategies + SlotSimulation.
        let all_snaps: Vec<Snapshot> = windows_map.values().flatten().cloned().collect();
        let sim = simulate_slots(&strategies, &all_snaps, 100.0);
        let cal = calibration_from_windows(&windows_map, &strategies);
        assert_eq!(cal.windows_settled, regime_windows.len());
        assert_eq!(cal.by_strategy.len(), 16);
        let wf = walk_forward_from_desk(&windows_map, &strategies, &sim);
        assert_eq!(wf.strategies.len(), 16);
        assert!(
            (wf.portfolio.full_sample.end_equity_dollars - sim.paper_equity_dollars).abs() < 1e-6
        );
    }

    #[test]
    fn scale_benchmark_2000_windows_100k_ticks_and_1000_mc_paths_under_500ms() {
        // Build 2,000 settled 15m windows × 50 ticks/window = 100,000 active ticks
        // (+ 2,000 settlement records = 102,000 snapshots total).
        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        let base_epoch = 1_760_000_000i64;
        for w in 0..2_000usize {
            let close_epoch = base_epoch + (w as i64) * 900;
            let close_iso = iso_z(close_epoch);
            let ticker = format!("KXBTC15M-SC{w:04}");
            let res = if w % 10 < 8 { "yes" } else { "no" };
            let mut snaps = Vec::with_capacity(51);
            // 50 pre-close snapshots spanning T-882s down to T-18s (18s cadence);
            // strategy enters near T-240s, scanning ~36 ticks per window (~72k+ ticks).
            for k in 0..50usize {
                let secs = 882 - (k as i64) * 18;
                let base_c = 62 + ((w + k) % 20) as i64; // 0.62..=0.81
                let ya = base_c as f64 / 100.0;
                let na = (102 - base_c) as f64 / 100.0;
                snaps.push(make_snap(
                    &ticker,
                    "KXBTC15M",
                    close_epoch - secs,
                    &close_iso,
                    "",
                    Some(ya),
                    Some(na),
                ));
            }
            snaps.push(make_snap(
                &ticker,
                "KXBTC15M",
                close_epoch + 60,
                &close_iso,
                res,
                None,
                None,
            ));
            windows.insert(ticker, snaps);
        }

        let total_ticks: usize = windows.values().map(|v| v.len()).sum();
        assert!(total_ticks >= 100_000, "ticks={total_ticks}");

        let src = std::fs::read_to_string("strategies/spread_compression.rn").unwrap();

        let t0 = Instant::now();
        let bt = validate_and_backtest_rune_source_with_config(
            &src,
            &windows,
            BacktestConfig {
                monte_carlo_paths: 1_000,
                monte_carlo_seed: 1337,
                ..BacktestConfig::default()
            },
        );
        let elapsed = t0.elapsed();

        assert!(bt.validation.valid);
        assert_eq!(bt.windows_evaluated, 2_000);
        assert_eq!(bt.trades_total, 2_000);
        assert!(bt.calibration.as_ref().unwrap().brier_score.is_some());
        assert!(bt.calibration.as_ref().unwrap().ece.is_some());
        let mc = bt.monte_carlo.as_ref().unwrap();
        assert_eq!(mc.paths, 1_000);
        assert_eq!(mc.horizon_steps, 2_000);
        eprintln!(
            "[scale] 2000 windows / {} snapshots ({} Rune ticks scanned) + 1000-path MC: {:.1}ms",
            total_ticks,
            bt.ticks_scanned,
            elapsed.as_secs_f64() * 1000.0
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "2,000 windows / 100,000 ticks + 1,000-path MC took {:.1}ms (budget: 500ms)",
            elapsed.as_secs_f64() * 1000.0
        );
    }
}
