//! Autonomous Kalshi Edge & Auto-Allocator.
//!
//! Replaces equal-split 1/N taker sizing across all fixed paper shapes with:
//! 1. **Post-Only Maker Fee Economics & Whole-Cent Kalshi Rounding:**
//!    Theoretical per-contract schedule `maker_fee(p) = 0.0175 * p * (1 - p)`
//!    (exact 1/4 of taker `0.07 * p * (1 - p)`), plus exact Kalshi whole-cent
//!    order fee rounding `ceil(rate * C * p * (1 - p) * 100) / 100` in replay.
//! 2. **Strict Out-of-Sample (OOS) Winner Gate & Auto-Quarantine:** evaluates all
//!    loaded strategies on the chronological last-40% OOS tape split from
//!    `TournamentTapeContext`; strategies with `oos_trades < min_oos_trades` or
//!    `oos_net_expectancy_cents <= 0.0` are quarantined at 0% weight.
//! 3. **Zero-Lookahead Causal OOS Replay:** when In-Sample (`close_epoch < split_epoch`)
//!    history is present, counterfactual OOS replay weights are selected strictly
//!    from In-Sample windows and frozen onto held-out OOS windows (zero OOS selection
//!    leakage).
//! 4. **Pairwise Correlation Pruning:** greedy deduplication above `|r| > 0.65`
//!    on shared windows, preserving highest net expectancy first.
//! 5. **Fractional-Kelly Weighting & Heat Caps:** binary Kelly `f*` scaled by
//!    `quarter_kelly` (0.25x), `half_kelly` (0.50x), or `equal_winners`,
//!    clamped per slot (`max_slot_risk_pct`) and total heat (`max_total_heat_pct`).
//!    When 0 strategies qualify, 100% remains in cash reserve (`0.0%` allocated).
//! 6. **Hard Risk Rail Compliance:** clamps target contracts to active
//!    `RiskRailsConfig` (`max_order_contracts`, `max_order_dollars`) and keeps
//!    live execution gated by `live_armed` (always `false` on boot).

#![allow(dead_code)]

use crate::fee;
use crate::risk_rails::RiskRailsConfig;
use crate::tournament::{pearson_correlation, TournamentEntry, TournamentTapeContext};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Maximum pairwise Pearson correlation allowed among active winners.
pub const ALLOCATOR_CORRELATION_CAP: f64 = 0.65;

/// Target daily profit threshold for capital readiness verdicts.
pub const TARGET_DAILY_PROFIT_DOLLARS: f64 = 500.0;

/// Approximate number of 15-minute Kalshi crypto windows per UTC day.
const WINDOWS_PER_DAY_15M: f64 = 96.0;

/// Kalshi post-only maker fee schedule in dollars per contract (`1/4` of taker `0.07 * p * (1 - p)`).
#[inline]
pub(crate) fn maker_fee(price: f64) -> f64 {
    let p = price.clamp(0.01, 0.99);
    0.0175 * p * (1.0 - p)
}

/// Exact Kalshi whole-cent rounded order fee in dollars for `contracts` at `price`:
/// `ceil(rate * contracts * p * (1 - p) * 100) / 100`.
#[inline]
pub(crate) fn kalshi_order_fee_dollars(contracts: u32, price: f64, post_only: bool) -> f64 {
    if contracts == 0 {
        return 0.0;
    }
    let p = price.clamp(0.01, 0.99);
    let rate = if post_only { 0.0175 } else { 0.07 };
    ((rate * (contracts as f64) * p * (1.0 - p) * 100.0 - 1e-9)
        .ceil()
        .max(0.0))
        / 100.0
}

#[inline]
fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
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

fn default_sizing_mode() -> String {
    "quarter_kelly".to_string()
}
fn default_execution_mode() -> String {
    "maker".to_string()
}
fn default_maker_fill_rate() -> f64 {
    0.75
}
fn default_min_oos_trades() -> usize {
    10
}
fn default_max_slot_risk_pct() -> f64 {
    15.0
}
fn default_max_total_heat_pct() -> f64 {
    40.0
}
fn default_slippage_cents() -> f64 {
    0.5
}
fn default_starting_bankroll() -> f64 {
    100.0
}
fn default_require_stress_resilience() -> bool {
    true
}
fn default_enable_regime_throttle() -> bool {
    true
}
fn default_min_fill_rate_pct() -> f64 {
    40.0
}

/// Request parameters for `GET /api/allocator` and `POST /api/allocator/plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoAllocatorRequest {
    #[serde(default = "default_sizing_mode")]
    pub sizing_mode: String,
    #[serde(default = "default_execution_mode")]
    pub execution_mode: String,
    #[serde(default = "default_maker_fill_rate")]
    pub maker_fill_rate: f64,
    #[serde(default = "default_min_oos_trades")]
    pub min_oos_trades: usize,
    #[serde(default = "default_max_slot_risk_pct")]
    pub max_slot_risk_pct: f64,
    #[serde(default = "default_max_total_heat_pct")]
    pub max_total_heat_pct: f64,
    #[serde(default = "default_slippage_cents")]
    pub slippage_cents: f64,
    #[serde(default = "default_starting_bankroll")]
    pub starting_bankroll: f64,
    #[serde(default = "default_require_stress_resilience")]
    pub require_stress_resilience: bool,
    #[serde(default = "default_enable_regime_throttle")]
    pub enable_regime_throttle: bool,
    #[serde(default = "default_min_fill_rate_pct")]
    pub min_fill_rate_pct: f64,
}

impl Default for AutoAllocatorRequest {
    fn default() -> Self {
        Self {
            sizing_mode: default_sizing_mode(),
            execution_mode: default_execution_mode(),
            maker_fill_rate: default_maker_fill_rate(),
            min_oos_trades: default_min_oos_trades(),
            max_slot_risk_pct: default_max_slot_risk_pct(),
            max_total_heat_pct: default_max_total_heat_pct(),
            slippage_cents: default_slippage_cents(),
            starting_bankroll: default_starting_bankroll(),
            require_stress_resilience: default_require_stress_resilience(),
            enable_regime_throttle: default_enable_regime_throttle(),
            min_fill_rate_pct: default_min_fill_rate_pct(),
        }
    }
}

impl AutoAllocatorRequest {
    /// Normalize modes and clamp all numeric inputs to finite, safe ranges.
    pub fn sanitized(&self) -> Self {
        let sizing_mode = match self.sizing_mode.trim().to_ascii_lowercase().as_str() {
            "half_kelly" => "half_kelly",
            "equal_winners" => "equal_winners",
            _ => "quarter_kelly",
        }
        .to_string();

        let execution_mode = match self.execution_mode.trim().to_ascii_lowercase().as_str() {
            "taker" => "taker",
            _ => "maker",
        }
        .to_string();

        Self {
            sizing_mode,
            execution_mode,
            maker_fill_rate: finite_or(self.maker_fill_rate, default_maker_fill_rate())
                .clamp(0.2, 1.0),
            min_oos_trades: self.min_oos_trades.clamp(1, 200),
            max_slot_risk_pct: finite_or(self.max_slot_risk_pct, default_max_slot_risk_pct())
                .clamp(1.0, 50.0),
            max_total_heat_pct: finite_or(self.max_total_heat_pct, default_max_total_heat_pct())
                .clamp(5.0, 100.0),
            slippage_cents: finite_or(self.slippage_cents, default_slippage_cents())
                .clamp(0.0, 5.0),
            starting_bankroll: finite_or(self.starting_bankroll, default_starting_bankroll())
                .clamp(10.0, 10_000_000.0),
            require_stress_resilience: self.require_stress_resilience,
            enable_regime_throttle: self.enable_regime_throttle,
            min_fill_rate_pct: finite_or(self.min_fill_rate_pct, default_min_fill_rate_pct())
                .clamp(10.0, 100.0),
        }
    }
}

/// Chronological walk-forward fold consistency and `+0c/+1c/+2c` adverse-fill stress profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StressProfile {
    pub extra_slippage_0c_cents: f64,
    pub extra_slippage_1c_cents: f64,
    pub extra_slippage_2c_cents: f64,
    pub folds_positive: usize,
    pub folds_total: usize,
    pub worst_fold_expectancy_cents: Option<f64>,
    pub stress_resilient: bool,
}

/// Fee-vs-gross expectancy attribution for one entry-price regime across active winners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeEdgeBucket {
    pub regime: String,
    pub trades: usize,
    pub win_rate: f64,
    pub gross_expectancy_cents: f64,
    pub fee_drag_cents: f64,
    pub net_expectancy_cents: f64,
}

/// Maker fill-rate sensitivity row evaluating queue adverse selection across fill tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillRateSensitivityPoint {
    pub fill_rate_pct: f64,
    pub adverse_haircut_cents: f64,
    pub effective_ev_cents: Option<f64>,
    pub projected_daily_net_dollars: f64,
    pub clears_min_fill_gate: bool,
}

/// Paper IS / baseline vs trailing OOS live-shadow expectancy drift telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftGauge {
    pub paper_expectancy_cents: Option<f64>,
    pub shadow_expectancy_cents: Option<f64>,
    pub drift_cents: Option<f64>,
    pub status: String,
    pub summary: String,
}

/// Strictly causal multi-market spread, fee, fill-headroom, and $500/day edge screener row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeScreenerRow {
    pub rank: usize,
    pub strategy: String,
    pub side: String,
    pub top_series: String,
    pub execution_mode: String,
    pub oos_trades: usize,
    pub avg_entry_cents: u8,
    pub maker_fee_cents: f64,
    pub taker_fee_cents: f64,
    pub fee_edge_saved_cents: f64,
    pub oos_net_expectancy_cents: f64,
    pub breakeven_fill_rate_pct: f64,
    pub fill_headroom_pct: f64,
    pub folds_positive: usize,
    pub folds_total: usize,
    pub worst_fold_expectancy_cents: Option<f64>,
    pub daily_dollar_contribution: f64,
    pub daily_target_share_pct: f64,
    pub edge_quality_score: f64,
    pub recommended_contracts: u32,
    pub basket_eligible: bool,
    pub verdict: String,
}

/// Guarded dry-run preflight basket assembled from top causal screener winners (live_armed stays false).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BestBasketPlan {
    pub legs: Vec<PlannedAllocatorIntent>,
    pub total_contracts: u32,
    pub total_allocated_dollars: f64,
    pub projected_daily_dollars: f64,
    pub avg_fill_headroom_pct: f64,
    pub ready_for_dry_run_preflight: bool,
    pub summary: String,
}

/// Daily percentile checkpoint (`day 0..=30`) from 1,000 causal OOS bootstrap trials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapEquityPoint {
    pub day: u32,
    pub p5_equity: f64,
    pub p50_equity: f64,
    pub p95_equity: f64,
    pub stress_1c_p50_equity: f64,
}

/// 1,000-trial causal bootstrap equity & drawdown stress simulator (`+0c` vs `+1c` adverse fill).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapStressSummary {
    pub trials: usize,
    pub horizon_days: u32,
    pub windows_per_day: usize,
    pub oos_sample_windows: usize,
    pub p5_end_equity: f64,
    pub p50_end_equity: f64,
    pub p95_end_equity: f64,
    pub p95_max_drawdown_pct: f64,
    pub daily_rail_breach_prob_pct: f64,
    pub prob_500_day_pct: f64,
    pub stress_1c_p5_end_equity: f64,
    pub stress_1c_p50_end_equity: f64,
    pub stress_1c_p95_end_equity: f64,
    pub stress_1c_p95_max_drawdown_pct: f64,
    pub stress_1c_daily_rail_breach_prob_pct: f64,
    pub stress_1c_prob_500_day_pct: f64,
    pub trajectory: Vec<BootstrapEquityPoint>,
    pub summary: String,
}

/// One strategy passing OOS depth, positive-EV, stress, and correlation gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocatedWinner {
    pub strategy: String,
    pub side: String,
    pub execution_mode: String,
    pub is_net_expectancy_cents: Option<f64>,
    pub oos_trades: usize,
    pub oos_win_rate: f64,
    pub oos_avg_entry_price: f64,
    pub taker_fee_cents: f64,
    pub maker_fee_cents: f64,
    pub gross_expectancy_cents: f64,
    pub fee_drag_cents: f64,
    pub oos_net_expectancy_cents: f64,
    pub full_kelly_pct: f64,
    pub breakeven_fill_rate_pct: f64,
    pub regime_throttle_mult: f64,
    pub allocated_pct: f64,
    pub allocated_dollars: f64,
    pub rail_clamped_contracts: u32,
    pub max_abs_corr: Option<f64>,
    pub status: String,
    pub stress: StressProfile,
}

/// One strategy assigned `0%` weight due to insufficient OOS depth, negative EV, or correlation conflict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantinedStrategy {
    pub strategy: String,
    pub side: String,
    pub oos_trades: usize,
    pub oos_net_expectancy_cents: Option<f64>,
    pub reason: String,
}

/// Chronological equity checkpoint comparing Legacy 1/N Taker vs Auto-Allocator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonPoint {
    pub close_epoch: i64,
    pub legacy_equity: f64,
    pub allocator_equity: f64,
}

/// OOS counterfactual replay comparing legacy 1/N equal-split taker vs auto-allocator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocatorComparison {
    pub selection_basis: String,
    pub legacy_equal_taker_end_equity: f64,
    pub legacy_equal_taker_net_pnl: f64,
    pub legacy_equal_taker_max_dd_pct: f64,
    pub legacy_equal_taker_fees_dollars: f64,
    pub auto_allocator_end_equity: f64,
    pub auto_allocator_net_pnl: f64,
    pub auto_allocator_max_dd_pct: f64,
    pub auto_allocator_fees_dollars: f64,
    pub fee_savings_dollars: f64,
    pub equity_delta_dollars: f64,
    pub equity_curve: Vec<ComparisonPoint>,
}

/// Concrete limit-order intent bounded by current `RiskRailsConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAllocatorIntent {
    pub strategy: String,
    pub side: String,
    pub post_only: bool,
    pub target_contracts: u32,
    pub max_limit_cents: u8,
    pub allocated_dollars: f64,
    pub rail_ready: bool,
    pub note: String,
}

/// Honest $500/day capital-readiness verdict derived from fee-adjusted OOS expectancy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyTargetVerdict {
    pub target_daily_profit_dollars: f64,
    pub verdict: String,
    pub projected_daily_net_dollars: f64,
    pub projected_daily_stress_1c_dollars: f64,
    pub projected_daily_stress_2c_dollars: f64,
    pub required_bankroll_dollars: Option<f64>,
    pub portfolio_net_expectancy_cents: Option<f64>,
    pub blockers: Vec<String>,
}

/// Complete response payload for `GET /api/allocator` and `POST /api/allocator/plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoAllocatorReport {
    pub execution_mode: String,
    pub sizing_mode: String,
    pub maker_fill_rate: f64,
    pub min_oos_trades: usize,
    pub max_slot_risk_pct: f64,
    pub max_total_heat_pct: f64,
    pub starting_bankroll: f64,
    pub require_stress_resilience: bool,
    pub enable_regime_throttle: bool,
    pub min_fill_rate_pct: f64,
    pub regime_throttle_mult: f64,
    pub current_oos_drawdown_pct: f64,
    pub throttle_reason: String,
    pub windows_settled_total: usize,
    pub windows_settled_oos: usize,
    pub live_armed: bool,
    pub total_allocated_pct: f64,
    pub cash_reserve_pct: f64,
    pub active_winners: Vec<AllocatedWinner>,
    pub quarantined: Vec<QuarantinedStrategy>,
    pub regime_attribution: Vec<RegimeEdgeBucket>,
    pub fill_sensitivity: Vec<FillRateSensitivityPoint>,
    pub drift_gauge: DriftGauge,
    pub edge_screener: Vec<EdgeScreenerRow>,
    pub best_basket: BestBasketPlan,
    pub bootstrap_stress: BootstrapStressSummary,
    pub comparison: AllocatorComparison,
    pub planned_intents: Vec<PlannedAllocatorIntent>,
    pub daily_target: DailyTargetVerdict,
    pub honesty_notes: Vec<String>,
}

/// Deterministic pseudo-random fill check over `(ticker, strategy)` for honest maker fill-rate simulation.
#[inline]
pub(crate) fn deterministic_maker_filled(ticker: &str, strategy: &str, fill_rate: f64) -> bool {
    if fill_rate >= 1.0 - 1e-12 {
        return true;
    }
    if fill_rate <= 0.0 {
        return false;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in ticker.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= 0xff;
    h = h.wrapping_mul(0x0100_0000_01b3);
    for &b in strategy.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    let u = ((h >> 11) as f64) / ((1u64 << 53) as f64);
    u < fill_rate
}

struct SplitCandidate {
    strategy_idx: usize,
    strategy: String,
    side: String,
    trades: usize,
    win_rate: f64,
    avg_entry_price: f64,
    taker_fee_cents: f64,
    maker_fee_cents: f64,
    gross_expectancy_cents: f64,
    fee_drag_cents: f64,
    net_expectancy_cents: f64,
    tail_fold_expectancy_cents: f64,
    full_kelly_pct: f64,
    stress: StressProfile,
    returns_by_win: HashMap<(i64, String), f64>,
}

fn candidate_pair_corr(a: &SplitCandidate, b: &SplitCandidate) -> Option<f64> {
    let (small, large, swapped) = if a.returns_by_win.len() <= b.returns_by_win.len() {
        (&a.returns_by_win, &b.returns_by_win, false)
    } else {
        (&b.returns_by_win, &a.returns_by_win, true)
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
    pearson_correlation(&xs, &ys)
}

struct SelectionResult {
    accepted: Vec<SplitCandidate>,
    weights_pct: Vec<f64>,
    max_corrs: Vec<Option<f64>>,
    quarantined: Vec<QuarantinedStrategy>,
    expectancy_by_idx: HashMap<usize, f64>,
}

fn evaluate_split_candidates(
    ctx: &TournamentTapeContext,
    entries_by_strat: &[Vec<&TournamentEntry>],
    req: &AutoAllocatorRequest,
    min_trades: usize,
    is_maker: bool,
    slip_dollars: f64,
) -> SelectionResult {
    let mut qualifying: Vec<SplitCandidate> = Vec::new();
    let mut quarantined: Vec<QuarantinedStrategy> = Vec::new();
    let mut expectancy_by_idx: HashMap<usize, f64> = HashMap::new();

    for (si, meta) in ctx.strategies.iter().enumerate() {
        let list = &entries_by_strat[si];
        let n = list.len();

        if n == 0 {
            quarantined.push(QuarantinedStrategy {
                strategy: meta.name.clone(),
                side: meta.side.clone(),
                oos_trades: 0,
                oos_net_expectancy_cents: None,
                reason: format!("insufficient OOS depth (< {min_trades}): 0 trades"),
            });
            continue;
        }

        let mut wins = 0usize;
        let mut price_sum = 0.0f64;
        let mut taker_fee_sum = 0.0f64;
        let mut maker_fee_sum = 0.0f64;
        let mut gross_pnl_sum = 0.0f64;
        let mut pnl_1ct_sum = 0.0f64;
        let mut returns_by_win: HashMap<(i64, String), f64> = HashMap::with_capacity(n);
        let mut ordered_pnls: Vec<(i64, &str, f64)> = Vec::with_capacity(n);

        for e in list {
            let p = e.entry_price.clamp(0.01, 0.99);
            let tf = fee(p);
            let mf = maker_fee(p);
            let af = if is_maker { mf } else { tf };
            let payoff = if e.won { 1.0 } else { 0.0 };
            let gross_pnl = payoff - p;
            let net_pnl = gross_pnl - af - slip_dollars;
            let cost = (p + af + slip_dollars).min(1.0);
            let ret = if cost > 1e-9 { net_pnl / cost } else { 0.0 };

            if e.won {
                wins += 1;
            }
            price_sum += p;
            taker_fee_sum += tf;
            maker_fee_sum += mf;
            gross_pnl_sum += gross_pnl;
            pnl_1ct_sum += net_pnl;
            returns_by_win.insert((e.close_epoch, e.ticker.clone()), finite_or(ret, 0.0));
            ordered_pnls.push((e.close_epoch, e.ticker.as_str(), net_pnl * 100.0));
        }

        let n_f = n as f64;
        let win_rate = finite_or(wins as f64 / n_f, 0.0);
        let avg_entry_price = finite_or((price_sum / n_f).clamp(0.01, 0.99), 0.50);
        let taker_fee_cents = finite_or((taker_fee_sum / n_f) * 100.0, 0.0);
        let maker_fee_cents = finite_or((maker_fee_sum / n_f) * 100.0, 0.0);
        let gross_exp_cents = finite_or((gross_pnl_sum / n_f) * 100.0, 0.0);
        let exp_cents = finite_or((pnl_1ct_sum / n_f) * 100.0, 0.0);
        let fee_drag_cents = finite_or((gross_exp_cents - exp_cents).max(0.0), 0.0);
        expectancy_by_idx.insert(si, exp_cents);

        // Compute rolling chronological walk-forward folds (up to 3 folds) and +0c/+1c/+2c adverse-fill stress.
        ordered_pnls.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        let folds_total = if n >= 6 {
            3
        } else if n >= 4 {
            2
        } else {
            1
        };
        let mut folds_positive = 0usize;
        let mut worst_fold: Option<f64> = None;
        let mut tail_fold_ev = exp_cents;
        for f_idx in 0..folds_total {
            let start = f_idx * n / folds_total;
            let end = (f_idx + 1) * n / folds_total;
            if end > start {
                let sum: f64 = ordered_pnls[start..end].iter().map(|x| x.2).sum();
                let fold_ev = finite_or(sum / ((end - start) as f64), 0.0);
                if fold_ev > 0.0 {
                    folds_positive += 1;
                }
                worst_fold = Some(worst_fold.map_or(fold_ev, |m| m.min(fold_ev)));
                tail_fold_ev = fold_ev;
            }
        }
        let extra_slippage_0c_cents = exp_cents;
        let extra_slippage_1c_cents = finite_or(exp_cents - 1.0, -1.0);
        let extra_slippage_2c_cents = finite_or(exp_cents - 2.0, -2.0);
        let majority_folds_ok = folds_total < 2 || folds_positive >= folds_total.div_ceil(2);
        let stress_resilient = extra_slippage_1c_cents > 0.0 && majority_folds_ok;
        let stress = StressProfile {
            extra_slippage_0c_cents,
            extra_slippage_1c_cents,
            extra_slippage_2c_cents,
            folds_positive,
            folds_total,
            worst_fold_expectancy_cents: worst_fold.and_then(finite_opt),
            stress_resilient,
        };

        if n < min_trades {
            quarantined.push(QuarantinedStrategy {
                strategy: meta.name.clone(),
                side: meta.side.clone(),
                oos_trades: n,
                oos_net_expectancy_cents: finite_opt(exp_cents),
                reason: format!("insufficient OOS depth (< {min_trades}): {n} trades"),
            });
            continue;
        }

        if exp_cents <= 0.0 {
            quarantined.push(QuarantinedStrategy {
                strategy: meta.name.clone(),
                side: meta.side.clone(),
                oos_trades: n,
                oos_net_expectancy_cents: finite_opt(exp_cents),
                reason: format!("negative OOS expectancy ({exp_cents:+.2}c/ct)"),
            });
            continue;
        }

        if req.require_stress_resilience && !stress.stress_resilient {
            let reason = if extra_slippage_1c_cents <= 0.0 {
                format!(
                    "fails +1c adverse-fill stress ({extra_slippage_1c_cents:+.2}c/ct at +1c; baseline {exp_cents:+.2}c/ct)"
                )
            } else {
                format!(
                    "fragile walk-forward stability ({folds_positive}/{folds_total} positive OOS folds; worst fold {:+.2}c/ct)",
                    worst_fold.unwrap_or(0.0)
                )
            };
            quarantined.push(QuarantinedStrategy {
                strategy: meta.name.clone(),
                side: meta.side.clone(),
                oos_trades: n,
                oos_net_expectancy_cents: finite_opt(exp_cents),
                reason,
            });
            continue;
        }

        // Binary Kelly fraction using effective cost at average entry price.
        let active_fee_per_ct = if is_maker {
            maker_fee(avg_entry_price)
        } else {
            fee(avg_entry_price)
        };
        let c_eff = (avg_entry_price + active_fee_per_ct + slip_dollars).clamp(0.01, 0.99);
        let b = (1.0 - c_eff) / c_eff;
        let f_full = if b > 1e-12 {
            ((win_rate * b - (1.0 - win_rate)) / b).max(0.0)
        } else {
            0.0
        };
        let full_kelly_pct = finite_or(f_full * 100.0, 0.0);

        qualifying.push(SplitCandidate {
            strategy_idx: si,
            strategy: meta.name.clone(),
            side: meta.side.clone(),
            trades: n,
            win_rate,
            avg_entry_price,
            taker_fee_cents,
            maker_fee_cents,
            gross_expectancy_cents: gross_exp_cents,
            fee_drag_cents,
            net_expectancy_cents: exp_cents,
            tail_fold_expectancy_cents: tail_fold_ev,
            full_kelly_pct,
            stress,
            returns_by_win,
        });
    }

    qualifying.sort_by(|a, b| {
        b.net_expectancy_cents
            .total_cmp(&a.net_expectancy_cents)
            .then_with(|| b.trades.cmp(&a.trades))
            .then_with(|| a.strategy.cmp(&b.strategy))
    });

    // Greedy correlation pruning: reject any candidate with |r| > 0.65 against a higher-EV winner.
    let mut accepted: Vec<SplitCandidate> = Vec::new();
    for cand in qualifying {
        let mut conflict: Option<(String, f64)> = None;
        for w in &accepted {
            if let Some(r) = candidate_pair_corr(&cand, w) {
                if r.abs() > ALLOCATOR_CORRELATION_CAP + 1e-12 {
                    conflict = Some((w.strategy.clone(), r));
                    break;
                }
            }
        }
        if let Some((winner_name, r)) = conflict {
            quarantined.push(QuarantinedStrategy {
                strategy: cand.strategy,
                side: cand.side,
                oos_trades: cand.trades,
                oos_net_expectancy_cents: finite_opt(cand.net_expectancy_cents),
                reason: format!(
                    "correlation > 0.65 with higher-EV winner {winner_name} (r={r:+.2})"
                ),
            });
        } else {
            accepted.push(cand);
        }
    }

    quarantined.sort_by(|a, b| {
        b.oos_net_expectancy_cents
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&a.oos_net_expectancy_cents.unwrap_or(f64::NEG_INFINITY))
            .then_with(|| a.strategy.cmp(&b.strategy))
    });

    let n_winners = accepted.len();
    let mut weights_pct: Vec<f64> = accepted
        .iter()
        .map(|w| {
            let unscaled = match req.sizing_mode.as_str() {
                "half_kelly" => w.full_kelly_pct * 0.50,
                "equal_winners" => {
                    if n_winners > 0 {
                        req.max_total_heat_pct / (n_winners as f64)
                    } else {
                        0.0
                    }
                }
                _ => w.full_kelly_pct * 0.25,
            };
            finite_or(unscaled.clamp(0.0, req.max_slot_risk_pct), 0.0)
        })
        .collect();

    let raw_sum: f64 = weights_pct.iter().sum();
    if raw_sum > req.max_total_heat_pct && raw_sum > 1e-12 {
        let scale = req.max_total_heat_pct / raw_sum;
        for w in &mut weights_pct {
            *w = finite_or(*w * scale, 0.0);
        }
    }

    let mut max_corrs: Vec<Option<f64>> = vec![None; n_winners];
    for i in 0..n_winners {
        let mut best_r: Option<f64> = None;
        for j in 0..n_winners {
            if i == j {
                continue;
            }
            if let Some(r) = candidate_pair_corr(&accepted[i], &accepted[j]) {
                let ar = r.abs();
                best_r = Some(best_r.map_or(ar, |m| m.max(ar)));
            }
        }
        max_corrs[i] = best_r.and_then(finite_opt);
    }

    SelectionResult {
        accepted,
        weights_pct,
        max_corrs,
        quarantined,
        expectancy_by_idx,
    }
}

/// Compute the full autonomous allocator plan, quarantine ledger, strictly causal
/// OOS replay comparison, and risk-rail-bounded order intents from a pre-built
/// `TournamentTapeContext`.
pub fn compute_auto_allocator_report(
    ctx: &TournamentTapeContext,
    raw_req: &AutoAllocatorRequest,
    rails: &RiskRailsConfig,
    live_armed: bool,
    killed: bool,
) -> AutoAllocatorReport {
    let req = raw_req.sanitized();
    let is_maker = req.execution_mode == "maker";
    let slip_dollars = if is_maker {
        0.0
    } else {
        req.slippage_cents / 100.0
    };

    let n_strats = ctx.strategies.len();
    let mut is_by_strat: Vec<Vec<&TournamentEntry>> = vec![Vec::new(); n_strats];
    let mut oos_by_strat: Vec<Vec<&TournamentEntry>> = vec![Vec::new(); n_strats];
    let mut oos_by_epoch: BTreeMap<i64, Vec<&TournamentEntry>> = BTreeMap::new();
    let mut is_total_entries = 0usize;

    for e in &ctx.entries {
        if e.strategy_idx >= n_strats {
            continue;
        }
        let is_oos = match ctx.split_epoch {
            Some(boundary) if ctx.windows_settled_oos > 0 => e.close_epoch >= boundary,
            _ => false,
        };
        if is_oos {
            oos_by_strat[e.strategy_idx].push(e);
            oos_by_epoch.entry(e.close_epoch).or_default().push(e);
        } else {
            is_by_strat[e.strategy_idx].push(e);
            is_total_entries += 1;
        }
    }

    // Ensure deterministic entry ordering within each OOS epoch for replay.
    for list in oos_by_epoch.values_mut() {
        list.sort_by(|a, b| {
            a.ticker
                .cmp(&b.ticker)
                .then_with(|| a.strategy_idx.cmp(&b.strategy_idx))
        });
    }

    // Evaluate OOS split for forward winner selection & quarantine.
    let oos_sel = evaluate_split_candidates(
        ctx,
        &oos_by_strat,
        &req,
        req.min_oos_trades,
        is_maker,
        slip_dollars,
    );

    // Evaluate IS split strictly causally (`close_epoch < split_epoch`) for OOS replay weights.
    let is_sel = if is_total_entries > 0 {
        Some(evaluate_split_candidates(
            ctx,
            &is_by_strat,
            &req,
            req.min_oos_trades,
            is_maker,
            slip_dollars,
        ))
    } else {
        None
    };

    let n_winners = oos_sel.accepted.len();

    // Use strictly causal IS-selected weights for OOS replay whenever IS history exists,
    // preventing selection lookahead over OOS outcomes.
    let (replay_weights, selection_basis) = match &is_sel {
        Some(sel) => {
            let mut m = HashMap::with_capacity(sel.accepted.len());
            for (i, c) in sel.accepted.iter().enumerate() {
                m.insert(c.strategy_idx, finite_or(sel.weights_pct[i], 0.0) / 100.0);
            }
            (m, "causal_is_to_oos".to_string())
        }
        None => {
            let mut m = HashMap::with_capacity(n_winners);
            for (i, c) in oos_sel.accepted.iter().enumerate() {
                m.insert(c.strategy_idx, finite_or(oos_sel.weights_pct[i], 0.0) / 100.0);
            }
            (m, "oos_only_no_is_history".to_string())
        }
    };

    let (comparison, regime_throttle_mult, current_oos_drawdown_pct, throttle_reason, oos_window_returns) =
        replay_oos_comparison(
            ctx,
            &req,
            &oos_by_epoch,
            &replay_weights,
            is_maker,
            selection_basis,
        );

    let total_allocated_pct = if n_winners == 0 {
        0.0
    } else {
        let sum_raw: f64 = oos_sel.weights_pct.iter().sum();
        finite_or((sum_raw * regime_throttle_mult).clamp(0.0, 100.0), 0.0)
    };
    let cash_reserve_pct = finite_or((100.0 - total_allocated_pct).clamp(0.0, 100.0), 100.0);

    let mut active_winners = Vec::with_capacity(n_winners);
    let mut planned_intents = Vec::with_capacity(n_winners);
    let mut active_idx_set = std::collections::HashSet::with_capacity(n_winners);

    for (i, cand) in oos_sel.accepted.iter().enumerate() {
        active_idx_set.insert(cand.strategy_idx);
        let alloc_pct = finite_or(oos_sel.weights_pct[i] * regime_throttle_mult, 0.0);
        let alloc_dollars = finite_or(req.starting_bankroll * (alloc_pct / 100.0), 0.0);
        let max_limit_cents =
            ((cand.avg_entry_price * 100.0).round() as i64).clamp(1, 99) as u8;
        let limit_dollars = (max_limit_cents as f64) / 100.0;

        let by_alloc = if limit_dollars > 0.0 {
            (alloc_dollars / limit_dollars).floor().max(0.0) as u32
        } else {
            0
        };
        let by_dollar_rail = if limit_dollars > 0.0 {
            (rails.max_order_dollars / limit_dollars).floor().max(0.0) as u32
        } else {
            0
        };
        let rail_clamped_contracts = by_alloc
            .min(rails.max_order_contracts)
            .min(by_dollar_rail);

        let notional = (rail_clamped_contracts as f64) * limit_dollars;
        let rail_ready = live_armed
            && !killed
            && rail_clamped_contracts >= 1
            && notional <= rails.max_order_dollars + 1e-9;

        let note = if killed {
            "Blocked — emergency kill switch is latched".to_string()
        } else if !live_armed {
            "Paper plan only — live_armed=false (disarmed on boot)".to_string()
        } else if rail_clamped_contracts == 0 {
            "Allocated budget below 1 whole contract at limit price".to_string()
        } else if is_maker {
            format!(
                "Ready within Hard Risk Rails (post_only=true maker limit <= {max_limit_cents}c)"
            )
        } else {
            format!("Ready within Hard Risk Rails (taker limit <= {max_limit_cents}c)")
        };

        let is_exp = is_sel
            .as_ref()
            .and_then(|s| s.expectancy_by_idx.get(&cand.strategy_idx).copied())
            .and_then(finite_opt);

        let breakeven_fill_rate_pct = finite_or(
            (1.0 - (cand.net_expectancy_cents / 2.0)).clamp(0.0, 1.0) * 100.0,
            100.0,
        );

        active_winners.push(AllocatedWinner {
            strategy: cand.strategy.clone(),
            side: cand.side.clone(),
            execution_mode: req.execution_mode.clone(),
            is_net_expectancy_cents: is_exp,
            oos_trades: cand.trades,
            oos_win_rate: cand.win_rate,
            oos_avg_entry_price: cand.avg_entry_price,
            taker_fee_cents: cand.taker_fee_cents,
            maker_fee_cents: cand.maker_fee_cents,
            gross_expectancy_cents: cand.gross_expectancy_cents,
            fee_drag_cents: cand.fee_drag_cents,
            oos_net_expectancy_cents: cand.net_expectancy_cents,
            full_kelly_pct: cand.full_kelly_pct,
            breakeven_fill_rate_pct,
            regime_throttle_mult,
            allocated_pct: alloc_pct,
            allocated_dollars: alloc_dollars,
            rail_clamped_contracts,
            max_abs_corr: oos_sel.max_corrs[i],
            status: "ACTIVE_WINNER".to_string(),
            stress: cand.stress.clone(),
        });

        planned_intents.push(PlannedAllocatorIntent {
            strategy: cand.strategy.clone(),
            side: cand.side.clone(),
            post_only: is_maker,
            target_contracts: rail_clamped_contracts,
            max_limit_cents,
            allocated_dollars: alloc_dollars,
            rail_ready,
            note,
        });
    }

    let regime_attribution = compute_regime_attribution(
        &oos_by_strat,
        &active_idx_set,
        is_maker,
        slip_dollars,
    );

    let fill_sensitivity = compute_fill_sensitivity(ctx, &req, &active_winners);
    let drift_gauge = compute_drift_gauge(&active_winners, &oos_sel.accepted);
    let (edge_screener, best_basket) = compute_edge_screener_and_basket(
        ctx,
        &req,
        &oos_by_strat,
        &active_winners,
        &oos_sel.quarantined,
        &planned_intents,
        is_maker,
        killed,
    );
    let bootstrap_stress = compute_bootstrap_stress_sim(
        &oos_window_returns,
        req.starting_bankroll,
        rails.max_daily_loss_dollars,
    );

    // Compute $500/day net expectancy readiness verdict with concrete blockers.
    let daily_target = compute_daily_target_verdict(
        ctx,
        &req,
        &active_winners,
        &comparison,
        &drift_gauge,
        live_armed,
        killed,
        is_maker,
    );

    let mut honesty_notes = Vec::new();
    if is_maker {
        honesty_notes.push(format!(
            "Maker mode models 1/4 Kalshi fees (0.0175*p*(1-p), whole-cent rounded per fill) with 0.00c crossed slippage on OOS expectancy, and replays fills using deterministic hash((ticker, strategy)) < {:.0}% maker_fill_rate.",
            req.maker_fill_rate * 100.0
        ));
    } else {
        honesty_notes.push(format!(
            "Taker mode models full Kalshi taker fees (0.07*p*(1-p), whole-cent rounded per fill) plus {:.2}c/ct crossed slippage.",
            req.slippage_cents
        ));
    }
    if comparison.selection_basis == "causal_is_to_oos" {
        honesty_notes.push(
            "Zero-lookahead replay: OOS equity curve applies weights selected strictly from In-Sample (first 60% of settled windows) onto held-out OOS (last 40%)."
                .to_string(),
        );
    } else {
        honesty_notes.push(
            "No In-Sample entries preceded split_epoch on this dataset; replay uses OOS-only weights."
                .to_string(),
        );
    }
    if n_winners == 0 {
        honesty_notes.push(format!(
            "0 strategies cleared min_oos_trades >= {} with positive OOS net expectancy — 100% held in cash reserve to protect bankroll.",
            req.min_oos_trades
        ));
    }
    if !live_armed {
        honesty_notes.push(
            "Hard Risk Rails active: live_armed is false, so planned intents remain paper-only until explicitly armed."
                .to_string(),
        );
    }

    if req.require_stress_resilience {
        honesty_notes.push(
            "Walk-Forward Stress Guard active: every winner must hold positive expectancy across >=2/3 chronological OOS folds AND survive +1c adverse-fill slippage."
                .to_string(),
        );
    } else {
        honesty_notes.push(
            "Walk-Forward Stress Guard disabled: brittle or sub-1c edge winners may appear; verify +1c/+2c stress sensitivity before scaling."
                .to_string(),
        );
    }

    if req.enable_regime_throttle {
        honesty_notes.push(format!(
            "Causal Regime Throttle active ({throttle_reason}): scales Kelly allocation down during live OOS drawdown or adverse streaks using strictly prior settled epochs."
        ));
    }

    AutoAllocatorReport {
        execution_mode: req.execution_mode,
        sizing_mode: req.sizing_mode,
        maker_fill_rate: req.maker_fill_rate,
        min_oos_trades: req.min_oos_trades,
        max_slot_risk_pct: req.max_slot_risk_pct,
        max_total_heat_pct: req.max_total_heat_pct,
        starting_bankroll: req.starting_bankroll,
        require_stress_resilience: req.require_stress_resilience,
        enable_regime_throttle: req.enable_regime_throttle,
        min_fill_rate_pct: req.min_fill_rate_pct,
        regime_throttle_mult,
        current_oos_drawdown_pct,
        throttle_reason,
        windows_settled_total: ctx.windows_settled_total,
        windows_settled_oos: ctx.windows_settled_oos,
        live_armed,
        total_allocated_pct,
        cash_reserve_pct,
        active_winners,
        quarantined: oos_sel.quarantined,
        regime_attribution,
        fill_sensitivity,
        drift_gauge,
        edge_screener,
        best_basket,
        bootstrap_stress,
        comparison,
        planned_intents,
        daily_target,
        honesty_notes,
    }
}

fn compute_fill_sensitivity(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    winners: &[AllocatedWinner],
) -> Vec<FillRateSensitivityPoint> {
    let oos_windows = ctx.windows_settled_oos.max(1) as f64;
    let total_weight: f64 = winners.iter().map(|w| w.allocated_pct).sum();
    let base_ev = if total_weight > 1e-9 {
        Some(
            winners
                .iter()
                .map(|w| w.oos_net_expectancy_cents * (w.allocated_pct / total_weight))
                .sum::<f64>(),
        )
    } else {
        None
    };

    [25.0, 50.0, 75.0, 100.0]
        .into_iter()
        .map(|pct| {
            let r = pct / 100.0;
            let adverse_haircut_cents = finite_or((1.0 - r) * 2.0, 0.0);
            let effective_ev_cents = base_ev.and_then(|ev| finite_opt(ev - adverse_haircut_cents));
            let daily_per_dollar: f64 = winners
                .iter()
                .map(|w| {
                    let fire_rate = (w.oos_trades as f64 / oos_windows).min(1.0);
                    let alloc_frac = w.allocated_pct / 100.0;
                    let p = w.oos_avg_entry_price.clamp(0.01, 0.99);
                    let eff_ev_dollars = (w.oos_net_expectancy_cents - adverse_haircut_cents) / 100.0;
                    (alloc_frac / p) * eff_ev_dollars * fire_rate * r
                })
                .sum();
            FillRateSensitivityPoint {
                fill_rate_pct: pct,
                adverse_haircut_cents,
                effective_ev_cents,
                projected_daily_net_dollars: finite_or(
                    req.starting_bankroll * daily_per_dollar * WINDOWS_PER_DAY_15M,
                    0.0,
                ),
                clears_min_fill_gate: pct + 1e-9 >= req.min_fill_rate_pct,
            }
        })
        .collect()
}

fn compute_drift_gauge(winners: &[AllocatedWinner], accepted: &[SplitCandidate]) -> DriftGauge {
    if winners.is_empty() || accepted.is_empty() {
        return DriftGauge {
            paper_expectancy_cents: None,
            shadow_expectancy_cents: None,
            drift_cents: None,
            status: "NO_DATA".to_string(),
            summary: "No active winners to measure IS-to-shadow expectancy drift.".to_string(),
        };
    }

    let mut w_sum = 0.0f64;
    let mut paper_weighted = 0.0f64;
    let mut shadow_weighted = 0.0f64;
    for (w, cand) in winners.iter().zip(accepted.iter()) {
        let weight = w.allocated_pct.max(0.01);
        let paper_ev = w.is_net_expectancy_cents.unwrap_or(w.oos_net_expectancy_cents);
        let shadow_ev = cand.tail_fold_expectancy_cents;
        w_sum += weight;
        paper_weighted += paper_ev * weight;
        shadow_weighted += shadow_ev * weight;
    }

    let paper_ev = finite_or(paper_weighted / w_sum.max(1e-9), 0.0);
    let shadow_ev = finite_or(shadow_weighted / w_sum.max(1e-9), 0.0);
    let drift = finite_or(shadow_ev - paper_ev, 0.0);

    let (status, label) = if shadow_ev < 0.0 || drift < -5.0 {
        ("DECAY_CRITICAL", "Critical decay")
    } else if drift < -2.0 {
        ("DECAY_WARNING", "Drift warning")
    } else {
        ("HEALTHY", "Healthy")
    };

    DriftGauge {
        paper_expectancy_cents: finite_opt(paper_ev),
        shadow_expectancy_cents: finite_opt(shadow_ev),
        drift_cents: finite_opt(drift),
        status: status.to_string(),
        summary: format!(
            "{label}: paper {paper_ev:+.2}c vs trailing shadow {shadow_ev:+.2}c ({drift:+.2}c drift)"
        ),
    }
}

fn dominant_series_for_entries(entries: Option<&Vec<&TournamentEntry>>) -> String {
    let Some(list) = entries else {
        return "KXBTC15M".to_string();
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in list {
        *counts.entry(e.series.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| "KXBTC15M".to_string())
}

#[allow(clippy::too_many_arguments)]
fn compute_edge_screener_and_basket(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    oos_by_strat: &[Vec<&TournamentEntry>],
    winners: &[AllocatedWinner],
    quarantined: &[QuarantinedStrategy],
    planned_intents: &[PlannedAllocatorIntent],
    is_maker: bool,
    killed: bool,
) -> (Vec<EdgeScreenerRow>, BestBasketPlan) {
    let oos_windows = ctx.windows_settled_oos.max(1) as f64;
    let fill_mult = if is_maker { req.maker_fill_rate } else { 1.0 };
    let idx_by_name: HashMap<&str, usize> = ctx
        .strategies
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    let mut rows: Vec<EdgeScreenerRow> = Vec::with_capacity(winners.len() + quarantined.len().min(6));
    for w in winners {
        let strat_idx = idx_by_name.get(w.strategy.as_str()).copied();
        let top_series = dominant_series_for_entries(strat_idx.and_then(|i| oos_by_strat.get(i)));
        let avg_entry_cents = ((w.oos_avg_entry_price * 100.0).round() as i64).clamp(1, 99) as u8;
        let fee_edge_saved_cents = if is_maker {
            finite_or((w.taker_fee_cents - w.maker_fee_cents).max(0.0), 0.0)
        } else {
            0.0
        };
        let fill_headroom_pct = finite_or(req.maker_fill_rate * 100.0 - w.breakeven_fill_rate_pct, 0.0);
        let fire_rate = (w.oos_trades as f64 / oos_windows).min(1.0);
        let p = w.oos_avg_entry_price.clamp(0.01, 0.99);
        let alloc_frac = w.allocated_pct / 100.0;
        let daily_dollar_contribution = finite_or(
            req.starting_bankroll
                * (alloc_frac / p)
                * (w.oos_net_expectancy_cents / 100.0)
                * fire_rate
                * fill_mult
                * WINDOWS_PER_DAY_15M,
            0.0,
        );
        let daily_target_share_pct = finite_or(
            (daily_dollar_contribution / TARGET_DAILY_PROFIT_DOLLARS * 100.0).max(0.0),
            0.0,
        );
        let fold_ratio = if w.stress.folds_total > 0 {
            w.stress.folds_positive as f64 / w.stress.folds_total as f64
        } else {
            0.0
        };
        let ev_score = (w.oos_net_expectancy_cents / 15.0).clamp(0.0, 1.0) * 40.0;
        let fill_score = (fill_headroom_pct / 60.0).clamp(0.0, 1.0) * 25.0;
        let fold_score = fold_ratio.clamp(0.0, 1.0) * 25.0;
        let stress_bonus = if w.stress.stress_resilient { 10.0 } else { 0.0 };
        let edge_quality_score = finite_or((ev_score + fill_score + fold_score + stress_bonus).clamp(0.0, 100.0), 0.0);

        let basket_eligible = !killed && w.rail_clamped_contracts >= 1 && (!is_maker || fill_headroom_pct >= 0.0);
        let verdict = if basket_eligible {
            "BASKET_ELIGIBLE"
        } else if killed {
            "KILLED"
        } else if w.rail_clamped_contracts == 0 {
            "BELOW_1_CONTRACT"
        } else {
            "LOW_FILL_HEADROOM"
        }
        .to_string();

        rows.push(EdgeScreenerRow {
            rank: 0,
            strategy: w.strategy.clone(),
            side: w.side.clone(),
            top_series,
            execution_mode: w.execution_mode.clone(),
            oos_trades: w.oos_trades,
            avg_entry_cents,
            maker_fee_cents: w.maker_fee_cents,
            taker_fee_cents: w.taker_fee_cents,
            fee_edge_saved_cents,
            oos_net_expectancy_cents: w.oos_net_expectancy_cents,
            breakeven_fill_rate_pct: w.breakeven_fill_rate_pct,
            fill_headroom_pct,
            folds_positive: w.stress.folds_positive,
            folds_total: w.stress.folds_total,
            worst_fold_expectancy_cents: w.stress.worst_fold_expectancy_cents,
            daily_dollar_contribution,
            daily_target_share_pct,
            edge_quality_score,
            recommended_contracts: w.rail_clamped_contracts,
            basket_eligible,
            verdict,
        });
    }

    let mut q_sorted: Vec<&QuarantinedStrategy> = quarantined.iter().collect();
    q_sorted.sort_by(|a, b| {
        let ea = a.oos_net_expectancy_cents.unwrap_or(f64::NEG_INFINITY);
        let eb = b.oos_net_expectancy_cents.unwrap_or(f64::NEG_INFINITY);
        eb.total_cmp(&ea).then_with(|| a.strategy.cmp(&b.strategy))
    });
    for q in q_sorted.into_iter().take(4) {
        let strat_idx = idx_by_name.get(q.strategy.as_str()).copied();
        let entries = strat_idx.and_then(|i| oos_by_strat.get(i));
        let top_series = dominant_series_for_entries(entries);
        let avg_p = entries
            .filter(|e| !e.is_empty())
            .map(|e| e.iter().map(|t| t.entry_price.clamp(0.01, 0.99)).sum::<f64>() / e.len() as f64)
            .unwrap_or(0.5);
        let maker_c = maker_fee(avg_p) * 100.0;
        let taker_c = fee(avg_p) * 100.0;
        let ev = q.oos_net_expectancy_cents.unwrap_or(0.0);
        let be_fill = finite_or((1.0 - (ev / 2.0)).clamp(0.0, 1.0) * 100.0, 100.0);
        rows.push(EdgeScreenerRow {
            rank: 0,
            strategy: q.strategy.clone(),
            side: q.side.clone(),
            top_series,
            execution_mode: req.execution_mode.clone(),
            oos_trades: q.oos_trades,
            avg_entry_cents: ((avg_p * 100.0).round() as i64).clamp(1, 99) as u8,
            maker_fee_cents: maker_c,
            taker_fee_cents: taker_c,
            fee_edge_saved_cents: if is_maker { (taker_c - maker_c).max(0.0) } else { 0.0 },
            oos_net_expectancy_cents: ev,
            breakeven_fill_rate_pct: be_fill,
            fill_headroom_pct: finite_or(req.maker_fill_rate * 100.0 - be_fill, -100.0),
            folds_positive: 0,
            folds_total: 0,
            worst_fold_expectancy_cents: None,
            daily_dollar_contribution: 0.0,
            daily_target_share_pct: 0.0,
            edge_quality_score: 0.0,
            recommended_contracts: 0,
            basket_eligible: false,
            verdict: "QUARANTINED".to_string(),
        });
    }

    rows.sort_by(|a, b| {
        b.basket_eligible
            .cmp(&a.basket_eligible)
            .then_with(|| b.edge_quality_score.total_cmp(&a.edge_quality_score))
            .then_with(|| b.daily_dollar_contribution.total_cmp(&a.daily_dollar_contribution))
            .then_with(|| a.strategy.cmp(&b.strategy))
    });
    for (idx, r) in rows.iter_mut().enumerate() {
        r.rank = idx + 1;
    }

    let intent_map: HashMap<&str, &PlannedAllocatorIntent> = planned_intents
        .iter()
        .map(|it| (it.strategy.as_str(), it))
        .collect();
    let mut legs = Vec::new();
    let mut projected_daily = 0.0f64;
    let mut headroom_sum = 0.0f64;
    for r in rows.iter().filter(|r| r.basket_eligible).take(4) {
        if let Some(intent) = intent_map.get(r.strategy.as_str()) {
            if intent.target_contracts >= 1 {
                legs.push((*intent).clone());
                projected_daily += r.daily_dollar_contribution;
                headroom_sum += r.fill_headroom_pct;
            }
        }
    }
    let total_contracts: u32 = legs.iter().map(|l| l.target_contracts).sum();
    let total_allocated_dollars: f64 = finite_or(legs.iter().map(|l| l.allocated_dollars).sum(), 0.0);
    let avg_fill_headroom_pct = if !legs.is_empty() {
        finite_or(headroom_sum / legs.len() as f64, 0.0)
    } else {
        0.0
    };
    let ready_for_dry_run_preflight = !legs.is_empty() && !killed;
    let summary = if legs.is_empty() {
        "No basket-eligible causal winners at current OOS depth and fill gates.".to_string()
    } else {
        format!(
            "{} causal leg(s), {} contract(s) (${total_allocated_dollars:.2} cap), avg +{avg_fill_headroom_pct:.1}pp fill headroom, est. ${projected_daily:.2}/day (dry_run preflight ready)",
            legs.len(),
            total_contracts
        )
    };

    let basket = BestBasketPlan {
        legs,
        total_contracts,
        total_allocated_dollars,
        projected_daily_dollars: finite_or(projected_daily, 0.0),
        avg_fill_headroom_pct,
        ready_for_dry_run_preflight,
        summary,
    };

    (rows, basket)
}

#[allow(clippy::too_many_arguments)]
fn compute_daily_target_verdict(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    winners: &[AllocatedWinner],
    comparison: &AllocatorComparison,
    drift_gauge: &DriftGauge,
    live_armed: bool,
    killed: bool,
    is_maker: bool,
) -> DailyTargetVerdict {
    let mut blockers = Vec::new();
    if ctx.windows_settled_oos < 30 {
        blockers.push(format!(
            "Need >= 30 OOS settled windows for statistical confidence (currently {})",
            ctx.windows_settled_oos
        ));
    }
    if winners.is_empty() {
        blockers.push(format!(
            "0 strategies pass min_oos_trades >= {} with positive fee-adjusted OOS expectancy",
            req.min_oos_trades
        ));
    }
    if is_maker && (req.maker_fill_rate * 100.0) + 1e-9 < req.min_fill_rate_pct {
        blockers.push(format!(
            "Maker fill rate {:.0}% below minimum queue gate ({:.0}%)",
            req.maker_fill_rate * 100.0,
            req.min_fill_rate_pct
        ));
    }
    if drift_gauge.status == "DECAY_CRITICAL" {
        blockers.push(format!(
            "Paper-to-shadow expectancy decay critical ({})",
            drift_gauge.summary
        ));
    }
    if comparison.auto_allocator_max_dd_pct >= 25.0 {
        blockers.push(format!(
            "OOS replay max drawdown {:.1}% exceeds 25.0% safety ceiling",
            comparison.auto_allocator_max_dd_pct
        ));
    }
    if killed {
        blockers.push("Emergency kill switch is latched".to_string());
    } else if !live_armed {
        blockers.push("Live routing is disarmed (live_armed=false)".to_string());
    }

    let fill_mult = if is_maker { req.maker_fill_rate } else { 1.0 };
    let oos_windows = ctx.windows_settled_oos.max(1) as f64;
    let total_weight: f64 = winners.iter().map(|w| w.allocated_pct).sum();
    let weighted_ev_cents = if total_weight > 1e-9 {
        Some(
            winners
                .iter()
                .map(|w| w.oos_net_expectancy_cents * (w.allocated_pct / total_weight))
                .sum::<f64>(),
        )
    } else {
        None
    };

    // Projected daily net PnL at current starting_bankroll (per-window expected dollars * 96 windows/day).
    let expected_dollars_per_window_per_dollar_bankroll: f64 = winners
        .iter()
        .map(|w| {
            let fire_rate = (w.oos_trades as f64 / oos_windows).min(1.0);
            let alloc_frac = w.allocated_pct / 100.0;
            let p = w.oos_avg_entry_price.clamp(0.01, 0.99);
            let ev_dollars_per_ct = w.oos_net_expectancy_cents / 100.0;
            (alloc_frac / p) * ev_dollars_per_ct * fire_rate * fill_mult
        })
        .sum();

    let projected_daily_net_dollars = finite_or(
        req.starting_bankroll
            * expected_dollars_per_window_per_dollar_bankroll
            * WINDOWS_PER_DAY_15M,
        0.0,
    );

    let expected_stress_1c_per_dollar: f64 = winners
        .iter()
        .map(|w| {
            let fire_rate = (w.oos_trades as f64 / oos_windows).min(1.0);
            let alloc_frac = w.allocated_pct / 100.0;
            let p = w.oos_avg_entry_price.clamp(0.01, 0.99);
            let ev_dollars_per_ct = w.stress.extra_slippage_1c_cents / 100.0;
            (alloc_frac / p) * ev_dollars_per_ct * fire_rate * fill_mult
        })
        .sum();
    let projected_daily_stress_1c_dollars = finite_or(
        req.starting_bankroll * expected_stress_1c_per_dollar * WINDOWS_PER_DAY_15M,
        0.0,
    );

    let expected_stress_2c_per_dollar: f64 = winners
        .iter()
        .map(|w| {
            let fire_rate = (w.oos_trades as f64 / oos_windows).min(1.0);
            let alloc_frac = w.allocated_pct / 100.0;
            let p = w.oos_avg_entry_price.clamp(0.01, 0.99);
            let ev_dollars_per_ct = w.stress.extra_slippage_2c_cents / 100.0;
            (alloc_frac / p) * ev_dollars_per_ct * fire_rate * fill_mult
        })
        .sum();
    let projected_daily_stress_2c_dollars = finite_or(
        req.starting_bankroll * expected_stress_2c_per_dollar * WINDOWS_PER_DAY_15M,
        0.0,
    );

    let required_bankroll_dollars = if expected_dollars_per_window_per_dollar_bankroll > 1e-9 {
        let req_bk = TARGET_DAILY_PROFIT_DOLLARS
            / (expected_dollars_per_window_per_dollar_bankroll * WINDOWS_PER_DAY_15M);
        finite_opt(req_bk)
    } else {
        None
    };

    if projected_daily_net_dollars < TARGET_DAILY_PROFIT_DOLLARS {
        match required_bankroll_dollars {
            Some(need) => blockers.push(format!(
                "Projected ${projected_daily_net_dollars:.2}/day at ${:.0} bankroll (< $500/day; requires ~${need:.0} bankroll at current edge)",
                req.starting_bankroll
            )),
            None => blockers.push(
                "No positive daily expectancy at current allocation (cannot reach $500/day)".to_string(),
            ),
        }
    }

    let verdict = if blockers.is_empty() && projected_daily_net_dollars >= TARGET_DAILY_PROFIT_DOLLARS
    {
        "GO_500_DAY".to_string()
    } else {
        "NO_GO_PAPER".to_string()
    };

    DailyTargetVerdict {
        target_daily_profit_dollars: TARGET_DAILY_PROFIT_DOLLARS,
        verdict,
        projected_daily_net_dollars,
        projected_daily_stress_1c_dollars,
        projected_daily_stress_2c_dollars,
        required_bankroll_dollars,
        portfolio_net_expectancy_cents: weighted_ev_cents,
        blockers,
    }
}

/// Strictly causal rolling drawdown and adverse-streak Kelly throttle multiplier (`0.35..=1.0`).
fn compute_causal_throttle(
    enabled: bool,
    current_dd_pct: f64,
    recent_returns: &[f64],
) -> (f64, String) {
    if !enabled {
        return (1.0, "Disabled (1.00x)".to_string());
    }
    let dd = current_dd_pct.max(0.0);
    let dd_mult: f64 = if dd >= 15.0 {
        0.50
    } else if dd >= 8.0 {
        0.70
    } else if dd >= 4.0 {
        0.85
    } else {
        1.00
    };
    let tail_neg = if recent_returns.len() >= 5 {
        recent_returns[recent_returns.len() - 5..]
            .iter()
            .filter(|&&r| r < 0.0)
            .count()
    } else {
        0
    };
    let streak_mult = if tail_neg >= 4 { 0.75 } else { 1.0 };
    let mult = finite_or((dd_mult * streak_mult).clamp(0.35, 1.0), 1.0);
    let reason = if (mult - 1.0).abs() < 1e-9 {
        format!("1.00x Normal (OOS DD {dd:.1}%)")
    } else if streak_mult < 1.0 && dd_mult < 1.0 {
        format!("{mult:.2}x Throttled (OOS DD {dd:.1}%, {tail_neg}/5 neg)")
    } else if streak_mult < 1.0 {
        format!("{mult:.2}x Streak Dampener ({tail_neg}/5 neg epochs)")
    } else {
        format!("{mult:.2}x Throttled (OOS DD {dd:.1}%)")
    };
    (mult, reason)
}

fn compute_regime_attribution(
    oos_by_strat: &[Vec<&TournamentEntry>],
    active_idxs: &std::collections::HashSet<usize>,
    is_maker: bool,
    slip_dollars: f64,
) -> Vec<RegimeEdgeBucket> {
    struct Acc {
        trades: usize,
        wins: usize,
        gross_sum: f64,
        net_sum: f64,
    }
    let labels = ["Underdog (<45c)", "Balanced (45-69c)", "Favorite (>=70c)"];
    let mut accs = [
        Acc { trades: 0, wins: 0, gross_sum: 0.0, net_sum: 0.0 },
        Acc { trades: 0, wins: 0, gross_sum: 0.0, net_sum: 0.0 },
        Acc { trades: 0, wins: 0, gross_sum: 0.0, net_sum: 0.0 },
    ];
    for &si in active_idxs {
        if let Some(entries) = oos_by_strat.get(si) {
            for e in entries {
                let p = e.entry_price.clamp(0.01, 0.99);
                let bucket = if p < 0.45 {
                    0
                } else if p < 0.70 {
                    1
                } else {
                    2
                };
                let af = if is_maker { maker_fee(p) } else { fee(p) };
                let payoff = if e.won { 1.0 } else { 0.0 };
                let gross = (payoff - p) * 100.0;
                let net = (payoff - p - af - slip_dollars) * 100.0;
                accs[bucket].trades += 1;
                if e.won {
                    accs[bucket].wins += 1;
                }
                accs[bucket].gross_sum += gross;
                accs[bucket].net_sum += net;
            }
        }
    }
    labels
        .iter()
        .enumerate()
        .filter_map(|(i, label)| {
            let a = &accs[i];
            if a.trades == 0 {
                return None;
            }
            let n_f = a.trades as f64;
            let gross_ev = finite_or(a.gross_sum / n_f, 0.0);
            let net_ev = finite_or(a.net_sum / n_f, 0.0);
            Some(RegimeEdgeBucket {
                regime: (*label).to_string(),
                trades: a.trades,
                win_rate: finite_or((a.wins as f64) / n_f, 0.0),
                gross_expectancy_cents: gross_ev,
                fee_drag_cents: finite_or((gross_ev - net_ev).max(0.0), 0.0),
                net_expectancy_cents: net_ev,
            })
        })
        .collect()
}

fn replay_oos_comparison(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    oos_by_epoch: &BTreeMap<i64, Vec<&TournamentEntry>>,
    winner_weights_by_idx: &HashMap<usize, f64>,
    is_maker: bool,
    selection_basis: String,
) -> (AllocatorComparison, f64, f64, String, Vec<(f64, f64)>) {
    let start_eq = req.starting_bankroll;
    let legacy_slot_frac = 1.0 / (ctx.strategies.len().max(1) as f64);
    let taker_slip = req.slippage_cents / 100.0;

    let mut legacy_eq = start_eq;
    let mut legacy_peak = start_eq;
    let mut legacy_max_dd_pct = 0.0f64;
    let mut legacy_fees = 0.0f64;

    let mut alloc_eq = start_eq;
    let mut alloc_peak = start_eq;
    let mut alloc_max_dd_pct = 0.0f64;
    let mut alloc_current_dd_pct = 0.0f64;
    let mut alloc_fees = 0.0f64;
    let mut recent_returns: Vec<f64> = Vec::with_capacity(16);
    let mut oos_window_returns: Vec<(f64, f64)> = Vec::with_capacity(oos_by_epoch.len());

    let mut curve: Vec<ComparisonPoint> = Vec::with_capacity(oos_by_epoch.len());

    for (&close_epoch, entries) in oos_by_epoch {
        let prev_legacy = legacy_eq;
        let prev_alloc = alloc_eq;
        // Causal throttle computed strictly from prior epochs (< close_epoch).
        let (epoch_throttle, _) =
            compute_causal_throttle(req.enable_regime_throttle, alloc_current_dd_pct, &recent_returns);
        let mut legacy_epoch_pnl = 0.0f64;
        let mut alloc_epoch_pnl = 0.0f64;
        let mut alloc_epoch_contracts = 0u32;
        let mut alloc_traded = false;

        for e in entries {
            let p = e.entry_price.clamp(0.01, 0.99);
            let payoff = if e.won { 1.0 } else { 0.0 };

            // 1. Legacy 1/N equal-split taker across all loaded strategies (Kalshi cent-rounded taker fee).
            if prev_legacy > 1e-6 {
                let contracts = ((prev_legacy * legacy_slot_frac) / p).floor().max(0.0) as u32;
                if contracts > 0 {
                    let order_fee = kalshi_order_fee_dollars(contracts, p, false);
                    let pnl = (contracts as f64) * (payoff - p - taker_slip) - order_fee;
                    legacy_fees += order_fee;
                    legacy_epoch_pnl += pnl;
                }
            }

            // 2. Auto-Allocator (causally selected uncorrelated winners only, exact cent-rounded fee).
            if prev_alloc > 1e-6 {
                if let Some(&weight) = winner_weights_by_idx.get(&e.strategy_idx) {
                    if weight > 0.0 {
                        let filled = if is_maker {
                            deterministic_maker_filled(&e.ticker, &e.strategy, req.maker_fill_rate)
                        } else {
                            true
                        };
                        if filled {
                            let contracts =
                                ((prev_alloc * weight * epoch_throttle) / p).floor().max(0.0) as u32;
                            if contracts > 0 {
                                alloc_traded = true;
                                alloc_epoch_contracts = alloc_epoch_contracts.saturating_add(contracts);
                                let order_fee = kalshi_order_fee_dollars(contracts, p, is_maker);
                                let slip = if is_maker { 0.0 } else { taker_slip };
                                let pnl = (contracts as f64) * (payoff - p - slip) - order_fee;
                                alloc_fees += order_fee;
                                alloc_epoch_pnl += pnl;
                            }
                        }
                    }
                }
            }
        }

        legacy_eq = (legacy_eq + legacy_epoch_pnl).max(0.0);
        if legacy_eq > legacy_peak {
            legacy_peak = legacy_eq;
        }
        if legacy_peak > 1e-9 {
            let dd = (((legacy_peak - legacy_eq).max(0.0) / legacy_peak) * 100.0).clamp(0.0, 100.0);
            if dd > legacy_max_dd_pct {
                legacy_max_dd_pct = dd;
            }
        }

        alloc_eq = (alloc_eq + alloc_epoch_pnl).max(0.0);
        if alloc_eq > alloc_peak {
            alloc_peak = alloc_eq;
        }
        if alloc_peak > 1e-9 {
            alloc_current_dd_pct =
                (((alloc_peak - alloc_eq).max(0.0) / alloc_peak) * 100.0).clamp(0.0, 100.0);
            if alloc_current_dd_pct > alloc_max_dd_pct {
                alloc_max_dd_pct = alloc_current_dd_pct;
            }
        }
        if prev_alloc > 1e-9 && (!winner_weights_by_idx.is_empty()) {
            let r0 = finite_or((alloc_epoch_pnl / prev_alloc).clamp(-0.95, 5.0), 0.0);
            let pnl_1c = alloc_epoch_pnl - (alloc_epoch_contracts as f64) * 0.01;
            let r1 = finite_or((pnl_1c / prev_alloc).clamp(-0.95, 5.0), 0.0);
            oos_window_returns.push((r0, r1));
        }
        if alloc_traded && prev_alloc > 1e-9 {
            recent_returns.push(finite_or(alloc_epoch_pnl / prev_alloc, 0.0));
            if recent_returns.len() > 8 {
                recent_returns.remove(0);
            }
        }

        curve.push(ComparisonPoint {
            close_epoch,
            legacy_equity: finite_or(legacy_eq, start_eq),
            allocator_equity: finite_or(alloc_eq, start_eq),
        });
    }

    if curve.len() > 400 {
        let n = curve.len();
        let mut sampled = Vec::with_capacity(400);
        for k in 0..400usize {
            let idx = if k == 399 {
                n - 1
            } else {
                (k * (n - 1)) / 399
            };
            sampled.push(curve[idx].clone());
        }
        curve = sampled;
    }

    let legacy_end = finite_or(legacy_eq, start_eq);
    let alloc_end = finite_or(alloc_eq, start_eq);
    let legacy_fees = finite_or(legacy_fees, 0.0);
    let alloc_fees = finite_or(alloc_fees, 0.0);
    let (end_throttle, end_reason) =
        compute_causal_throttle(req.enable_regime_throttle, alloc_current_dd_pct, &recent_returns);

    (
        AllocatorComparison {
            selection_basis,
            legacy_equal_taker_end_equity: legacy_end,
            legacy_equal_taker_net_pnl: finite_or(legacy_end - start_eq, 0.0),
            legacy_equal_taker_max_dd_pct: finite_or(legacy_max_dd_pct, 0.0),
            legacy_equal_taker_fees_dollars: legacy_fees,
            auto_allocator_end_equity: alloc_end,
            auto_allocator_net_pnl: finite_or(alloc_end - start_eq, 0.0),
            auto_allocator_max_dd_pct: finite_or(alloc_max_dd_pct, 0.0),
            auto_allocator_fees_dollars: alloc_fees,
            fee_savings_dollars: finite_or(legacy_fees - alloc_fees, 0.0),
            equity_delta_dollars: finite_or(alloc_end - legacy_end, 0.0),
            equity_curve: curve,
        },
        end_throttle,
        finite_or(alloc_current_dd_pct, 0.0),
        end_reason,
        oos_window_returns,
    )
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn percentile_sorted(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((pct / 100.0) * ((sorted.len() - 1) as f64)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

struct BootstrapScenarioOut {
    p5: Vec<f64>,
    p50: Vec<f64>,
    p95: Vec<f64>,
    p95_max_dd_pct: f64,
    daily_rail_breach_prob_pct: f64,
    prob_500_day_pct: f64,
}

fn run_bootstrap_scenario(
    returns: &[f64],
    start_eq: f64,
    max_daily_loss_dollars: f64,
    trials: usize,
    days: usize,
    windows_per_day: usize,
    seed: u64,
) -> BootstrapScenarioOut {
    let n = returns.len();
    let mut daily_equities = vec![vec![start_eq; trials]; days + 1];
    let mut max_dds = Vec::with_capacity(trials);
    let mut rail_breach_days = 0usize;
    let mut hit_500_days = 0usize;
    let mut rng = seed;
    // Cap deployed dollar stake per window at 4x initial bankroll (or 10x daily loss rail) to model finite 15m book capacity.
    let stake_cap = (start_eq * 4.0).max(max_daily_loss_dollars * 10.0).max(start_eq);

    for t in 0..trials {
        let mut eq = start_eq;
        let mut peak = start_eq;
        let mut max_dd = 0.0f64;
        for day_row in daily_equities.iter_mut().skip(1) {
            let day_start = eq;
            for _ in 0..windows_per_day {
                let idx = (splitmix64(&mut rng) as usize) % n;
                let r = returns[idx];
                let deployed = eq.min(stake_cap);
                eq = (eq + deployed * r).max(0.0);
                if eq > peak {
                    peak = eq;
                }
                if peak > 1e-9 {
                    let dd = (((peak - eq).max(0.0) / peak) * 100.0).clamp(0.0, 100.0);
                    if dd > max_dd {
                        max_dd = dd;
                    }
                }
            }
            let day_pnl = eq - day_start;
            if (max_daily_loss_dollars > 0.0 && day_pnl <= -max_daily_loss_dollars)
                || (day_start > 1e-9 && (-day_pnl / day_start) * 100.0 >= 25.0)
            {
                rail_breach_days += 1;
            }
            if day_pnl >= TARGET_DAILY_PROFIT_DOLLARS {
                hit_500_days += 1;
            }
            day_row[t] = finite_or(eq, start_eq);
        }
        max_dds.push(finite_or(max_dd, 0.0));
    }

    let mut p5 = Vec::with_capacity(days + 1);
    let mut p50 = Vec::with_capacity(days + 1);
    let mut p95 = Vec::with_capacity(days + 1);
    for row in &mut daily_equities {
        row.sort_by(|a, b| a.total_cmp(b));
        p5.push(percentile_sorted(row, 5.0));
        p50.push(percentile_sorted(row, 50.0));
        p95.push(percentile_sorted(row, 95.0));
    }
    max_dds.sort_by(|a, b| a.total_cmp(b));
    let total_sim_days = (trials * days).max(1) as f64;

    BootstrapScenarioOut {
        p5,
        p50,
        p95,
        p95_max_dd_pct: percentile_sorted(&max_dds, 95.0),
        daily_rail_breach_prob_pct: finite_or((rail_breach_days as f64 / total_sim_days) * 100.0, 0.0),
        prob_500_day_pct: finite_or((hit_500_days as f64 / total_sim_days) * 100.0, 0.0),
    }
}

fn compute_bootstrap_stress_sim(
    oos_window_returns: &[(f64, f64)],
    starting_bankroll: f64,
    max_daily_loss_dollars: f64,
) -> BootstrapStressSummary {
    const TRIALS: usize = 1_000;
    const DAYS: u32 = 30;
    const WINDOWS_PER_DAY: usize = 96;

    if oos_window_returns.is_empty() {
        let trajectory = (0..=DAYS)
            .map(|day| BootstrapEquityPoint {
                day,
                p5_equity: starting_bankroll,
                p50_equity: starting_bankroll,
                p95_equity: starting_bankroll,
                stress_1c_p50_equity: starting_bankroll,
            })
            .collect();
        return BootstrapStressSummary {
            trials: TRIALS,
            horizon_days: DAYS,
            windows_per_day: WINDOWS_PER_DAY,
            oos_sample_windows: 0,
            p5_end_equity: starting_bankroll,
            p50_end_equity: starting_bankroll,
            p95_end_equity: starting_bankroll,
            p95_max_drawdown_pct: 0.0,
            daily_rail_breach_prob_pct: 0.0,
            prob_500_day_pct: 0.0,
            stress_1c_p5_end_equity: starting_bankroll,
            stress_1c_p50_end_equity: starting_bankroll,
            stress_1c_p95_end_equity: starting_bankroll,
            stress_1c_p95_max_drawdown_pct: 0.0,
            stress_1c_daily_rail_breach_prob_pct: 0.0,
            stress_1c_prob_500_day_pct: 0.0,
            trajectory,
            summary: "Awaiting OOS trades for 1,000-trial causal bootstrap simulation.".to_string(),
        };
    }

    let r0: Vec<f64> = oos_window_returns.iter().map(|&(a, _)| a).collect();
    let r1: Vec<f64> = oos_window_returns.iter().map(|&(_, b)| b).collect();
    let base = run_bootstrap_scenario(
        &r0,
        starting_bankroll,
        max_daily_loss_dollars,
        TRIALS,
        DAYS as usize,
        WINDOWS_PER_DAY,
        0x4F50_454E_4245_4C4C,
    );
    let stress = run_bootstrap_scenario(
        &r1,
        starting_bankroll,
        max_daily_loss_dollars,
        TRIALS,
        DAYS as usize,
        WINDOWS_PER_DAY,
        0x4F50_454E_4245_4C4C,
    );

    let trajectory: Vec<BootstrapEquityPoint> = (0..=DAYS)
        .map(|d| {
            let idx = d as usize;
            BootstrapEquityPoint {
                day: d,
                p5_equity: base.p5[idx],
                p50_equity: base.p50[idx],
                p95_equity: base.p95[idx],
                stress_1c_p50_equity: stress.p50[idx],
            }
        })
        .collect();

    let last = DAYS as usize;
    let summary = format!(
        "1,000 causal OOS trials (30d): P50 ${:.2} (${:.2} @ +1c), P95 DD {:.1}% ({:.1}% @ +1c), rail-breach {:.1}%, P(>=$500/d) {:.1}%",
        base.p50[last],
        stress.p50[last],
        base.p95_max_dd_pct,
        stress.p95_max_dd_pct,
        base.daily_rail_breach_prob_pct,
        base.prob_500_day_pct,
    );

    BootstrapStressSummary {
        trials: TRIALS,
        horizon_days: DAYS,
        windows_per_day: WINDOWS_PER_DAY,
        oos_sample_windows: oos_window_returns.len(),
        p5_end_equity: base.p5[last],
        p50_end_equity: base.p50[last],
        p95_end_equity: base.p95[last],
        p95_max_drawdown_pct: base.p95_max_dd_pct,
        daily_rail_breach_prob_pct: base.daily_rail_breach_prob_pct,
        prob_500_day_pct: base.prob_500_day_pct,
        stress_1c_p5_end_equity: stress.p5[last],
        stress_1c_p50_end_equity: stress.p50[last],
        stress_1c_p95_end_equity: stress.p95[last],
        stress_1c_p95_max_drawdown_pct: stress.p95_max_dd_pct,
        stress_1c_daily_rail_breach_prob_pct: stress.daily_rail_breach_prob_pct,
        stress_1c_prob_500_day_pct: stress.prob_500_day_pct,
        trajectory,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tournament::StrategyMeta;

    fn make_ctx(
        strategies: Vec<StrategyMeta>,
        entries: Vec<TournamentEntry>,
        total_win: usize,
        oos_win: usize,
        split_epoch: i64,
    ) -> TournamentTapeContext {
        TournamentTapeContext {
            strategies,
            windows_settled_total: total_win,
            windows_settled_is: total_win.saturating_sub(oos_win),
            windows_settled_oos: oos_win,
            windows_conflicted: 0,
            split_epoch: Some(split_epoch),
            observed_series: vec!["KXBTC15M".to_string()],
            entries,
        }
    }

    #[test]
    fn maker_fee_is_exact_quarter_of_taker_fee_and_order_fee_rounds_whole_cents() {
        for c in 1..=99 {
            let p = c as f64 / 100.0;
            let t = fee(p);
            let m = maker_fee(p);
            assert!(
                (m - 0.25 * t).abs() < 1e-12,
                "at p={p}: maker={m} vs 0.25*taker={}",
                0.25 * t
            );
            let ord_m = kalshi_order_fee_dollars(1, p, true);
            let ord_t = kalshi_order_fee_dollars(1, p, false);
            assert!(ord_m >= 0.01 - 1e-9 && ord_m <= ord_t + 1e-9);
            assert!(((ord_m * 100.0) - (ord_m * 100.0).round()).abs() < 1e-9);
            assert!(((ord_t * 100.0) - (ord_t * 100.0).round()).abs() < 1e-9);
        }
        // At 50c x 10 contracts: taker = ceil(17.5c) = 18c; maker = ceil(4.375c) = 5c.
        assert!((kalshi_order_fee_dollars(10, 0.50, false) - 0.18).abs() < 1e-9);
        assert!((kalshi_order_fee_dollars(10, 0.50, true) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn zero_winners_holds_100_percent_cash_reserve() {
        let strats = vec![
            StrategyMeta {
                name: "loser-a".into(),
                side: "yes".into(),
            },
            StrategyMeta {
                name: "shallow-b".into(),
                side: "no".into(),
            },
        ];
        let mut entries = Vec::new();
        for w in 0..15 {
            entries.push(TournamentEntry {
                close_epoch: 1000 + w * 900,
                ticker: format!("KXBTC15M-{w}"),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "loser-a".into(),
                side: "yes".into(),
                entry_price: 0.70,
                secs_to_close: 180,
                won: w % 3 == 0,
            });
        }
        for w in 0..3 {
            entries.push(TournamentEntry {
                close_epoch: 1000 + w * 900,
                ticker: format!("KXBTC15M-{w}"),
                series: "KXBTC15M".into(),
                strategy_idx: 1,
                strategy: "shallow-b".into(),
                side: "no".into(),
                entry_price: 0.50,
                secs_to_close: 180,
                won: true,
            });
        }
        let ctx = make_ctx(strats, entries, 25, 15, 1000);
        let rep = compute_auto_allocator_report(
            &ctx,
            &AutoAllocatorRequest::default(),
            &RiskRailsConfig::default(),
            false,
            false,
        );
        assert!(rep.active_winners.is_empty());
        assert_eq!(rep.quarantined.len(), 2);
        assert_eq!(rep.total_allocated_pct, 0.0);
        assert_eq!(rep.cash_reserve_pct, 100.0);
        assert_eq!(rep.comparison.auto_allocator_end_equity, 100.0);
        assert_eq!(rep.comparison.auto_allocator_net_pnl, 0.0);
        assert!(rep.comparison.equity_delta_dollars > 0.0);
        assert_eq!(rep.daily_target.verdict, "NO_GO_PAPER");
    }

    #[test]
    fn causal_replay_uses_only_in_sample_selection_zero_lookahead() {
        // Strategy 0 ("is-loser-oos-miracle") loses badly in IS (0-19) and wins 100% in OOS (20-39).
        // Strategy 1 ("steady-winner") wins 80% in IS and 80% in OOS.
        // A non-causal allocator would pick Strategy 0 for the OOS replay; causal IS->OOS replay
        // MUST exclude Strategy 0 from replay weights so OOS replay equity is invariant to Strategy 0's OOS outcomes!
        let strats = vec![
            StrategyMeta { name: "is-loser-oos-miracle".into(), side: "yes".into() },
            StrategyMeta { name: "steady-winner".into(), side: "yes".into() },
        ];
        let build_entries = |strat0_oos_won: bool| {
            let mut entries = Vec::new();
            for w in 0..40i64 {
                let is_oos = w >= 20;
                entries.push(TournamentEntry {
                    close_epoch: 1_000 + w * 900,
                    ticker: format!("KXBTC15M-C{w:02}"),
                    series: "KXBTC15M".into(),
                    strategy_idx: 0,
                    strategy: "is-loser-oos-miracle".into(),
                    side: "yes".into(),
                    entry_price: 0.60,
                    secs_to_close: 240,
                    won: if is_oos { strat0_oos_won } else { w % 5 == 0 }, // 20% IS win rate -> negative EV
                });
                entries.push(TournamentEntry {
                    close_epoch: 1_000 + w * 900,
                    ticker: format!("KXBTC15M-C{w:02}"),
                    series: "KXBTC15M".into(),
                    strategy_idx: 1,
                    strategy: "steady-winner".into(),
                    side: "yes".into(),
                    entry_price: 0.55,
                    secs_to_close: 240,
                    won: w % 5 != 0, // 80% win rate in both IS & OOS
                });
            }
            entries
        };

        let ctx_win = make_ctx(strats.clone(), build_entries(true), 40, 20, 1_000 + 20 * 900);
        let ctx_lose = make_ctx(strats, build_entries(false), 40, 20, 1_000 + 20 * 900);

        let rep_win = compute_auto_allocator_report(
            &ctx_win,
            &AutoAllocatorRequest::default(),
            &RiskRailsConfig::default(),
            false,
            false,
        );
        let rep_lose = compute_auto_allocator_report(
            &ctx_lose,
            &AutoAllocatorRequest::default(),
            &RiskRailsConfig::default(),
            false,
            false,
        );

        assert_eq!(rep_win.comparison.selection_basis, "causal_is_to_oos");
        assert!(
            (rep_win.comparison.auto_allocator_end_equity - rep_lose.comparison.auto_allocator_end_equity).abs() < 1e-9,
            "causal OOS replay equity must be 100% invariant to future OOS outcome flip of IS-rejected strategy"
        );
    }

    #[test]
    fn correlation_pruning_kelly_scaling_heat_caps_and_rail_clamping() {
        let strats = vec![
            StrategyMeta { name: "win-hi".into(), side: "yes".into() },
            StrategyMeta { name: "win-corr".into(), side: "yes".into() },
            StrategyMeta { name: "win-indep".into(), side: "no".into() },
            StrategyMeta { name: "loser".into(), side: "yes".into() },
        ];
        let mut entries = Vec::new();
        for w in 0..40i64 {
            let epoch = 1_000 + w * 900;
            let ticker = format!("KXBTC15M-W{w:02}");
            let win_hi = w % 5 != 0;
            let win_indep = (w % 4) != 1;
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "win-hi".into(),
                side: "yes".into(),
                entry_price: 0.60,
                secs_to_close: 240,
                won: win_hi,
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".into(),
                strategy_idx: 1,
                strategy: "win-corr".into(),
                side: "yes".into(),
                entry_price: 0.65,
                secs_to_close: 240,
                won: win_hi,
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".into(),
                strategy_idx: 2,
                strategy: "win-indep".into(),
                side: "no".into(),
                entry_price: 0.55,
                secs_to_close: 180,
                won: win_indep,
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker,
                series: "KXBTC15M".into(),
                strategy_idx: 3,
                strategy: "loser".into(),
                side: "yes".into(),
                entry_price: 0.80,
                secs_to_close: 120,
                won: w % 2 == 0,
            });
        }
        let ctx = make_ctx(strats, entries, 40, 20, 1_000 + 20 * 900);
        let rails = RiskRailsConfig {
            max_order_contracts: 12,
            max_order_dollars: 50.0,
            ..RiskRailsConfig::default()
        };

        let qk_req = AutoAllocatorRequest {
            sizing_mode: "quarter_kelly".into(),
            execution_mode: "maker".into(),
            maker_fill_rate: 0.75,
            min_oos_trades: 10,
            max_slot_risk_pct: 15.0,
            max_total_heat_pct: 20.0,
            slippage_cents: 0.5,
            starting_bankroll: 1_000.0,
            require_stress_resilience: true,
            enable_regime_throttle: true,
            min_fill_rate_pct: 40.0,
        };
        let qk = compute_auto_allocator_report(&ctx, &qk_req, &rails, false, false);
        assert_eq!(qk.active_winners.len(), 2, "win-corr must be pruned for |r|>0.65");
        let active_names: Vec<&str> = qk.active_winners.iter().map(|w| w.strategy.as_str()).collect();
        assert!(active_names.contains(&"win-hi"));
        assert!(active_names.contains(&"win-indep"));
        assert!(!active_names.contains(&"win-corr"));
        assert!(!active_names.contains(&"loser"));
        assert!(qk.quarantined.iter().any(|q| q.strategy == "win-corr" && q.reason.contains("correlation > 0.65")));
        assert!(qk.quarantined.iter().any(|q| q.strategy == "loser" && q.reason.contains("negative OOS expectancy")));

        assert!(qk.total_allocated_pct <= 20.0 + 1e-9);
        assert!((qk.total_allocated_pct + qk.cash_reserve_pct - 100.0).abs() < 1e-9);
        for w in &qk.active_winners {
            assert_eq!(w.status, "ACTIVE_WINNER");
            assert!(w.allocated_pct <= 15.0 + 1e-9);
            assert!(w.rail_clamped_contracts <= rails.max_order_contracts);
            assert!((w.taker_fee_cents - 4.0 * w.maker_fee_cents).abs() < 1e-9);
        }
        assert!(!qk.live_armed);
        assert!(qk.planned_intents.iter().all(|i| i.post_only && !i.rail_ready));

        let hk_req = AutoAllocatorRequest {
            sizing_mode: "half_kelly".into(),
            max_total_heat_pct: 40.0,
            max_slot_risk_pct: 30.0,
            ..qk_req.clone()
        };
        let hk = compute_auto_allocator_report(&ctx, &hk_req, &rails, true, false);
        assert!(hk.total_allocated_pct >= qk.total_allocated_pct - 1e-9);
        assert!(hk.planned_intents.iter().all(|i| i.rail_ready));

        let qk2 = compute_auto_allocator_report(&ctx, &qk_req, &rails, false, false);
        assert_eq!(
            serde_json::to_string(&qk.comparison).unwrap(),
            serde_json::to_string(&qk2.comparison).unwrap()
        );
        assert_eq!(qk.comparison.equity_curve.len(), 20);
        assert!(qk.comparison.fee_savings_dollars > 0.0);
    }

    #[test]
    fn walk_forward_stress_guard_quarantines_brittle_and_razor_thin_edges() {
        let strats = vec![
            StrategyMeta { name: "steady-edge".into(), side: "yes".into() },
            StrategyMeta { name: "brittle-one-fold".into(), side: "yes".into() },
            StrategyMeta { name: "razor-thin".into(), side: "yes".into() },
        ];
        // 30 OOS windows (epochs 1000..).
        // 0) steady-edge: wins 80% uniformly at 55c -> EV ~ +24.6c, 3/3 folds positive.
        // 1) brittle-one-fold: loses all first 20 trades at 60c, wins all last 10 at 10c ->
        //    mean EV = (20*(-0.60) + 10*(0.90))/30 - fee ≈ -10c? Let's make it win overall:
        //    entry 0.30, first 20 windows lose 13 / win 7 (-6c in fold 1&2? Let's be explicit!):
        //    fold 0 (0..10): 2 wins / 10 @ 0.40 -> -20c/ct
        //    fold 1 (10..20): 2 wins / 10 @ 0.40 -> -20c/ct
        //    fold 2 (20..30): 10 wins / 10 @ 0.20 -> +80c/ct
        //    overall avg EV = (-2 - 2 + 8)/30 - fee ≈ +13.1c/ct (> +1c stress!), yet ONLY 1/3 folds positive!
        // 2) razor-thin: wins 24/30 (80.0%) at 0.793c + maker_fee(~0.29c) => EV ≈ +0.41c/ct (>0 at 0c, <0 at +1c)!
        let mut entries = Vec::new();
        for w in 0..30i64 {
            let epoch = 10_000 + w * 900;
            let ticker = format!("KXBTC15M-WF{w:02}");

            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "steady-edge".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: w % 5 != 0,
            });

            let (brittle_price, brittle_won) = if w < 20 {
                (0.40, w % 5 == 0) // 2/10 wins in fold 0 and fold 1 -> negative fold EV
            } else {
                (0.20, true) // 10/10 wins in fold 2 -> spikes overall mean EV > +10c!
            };
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".into(),
                strategy_idx: 1,
                strategy: "brittle-one-fold".into(),
                side: "yes".into(),
                entry_price: brittle_price,
                secs_to_close: 240,
                won: brittle_won,
            });

            // 24/30 wins (0.80) at price 0.793 -> raw EV = 0.80 - 0.793 - 0.0029 = +0.41c/ct.
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker,
                series: "KXBTC15M".into(),
                strategy_idx: 2,
                strategy: "razor-thin".into(),
                side: "yes".into(),
                entry_price: 0.793,
                secs_to_close: 240,
                won: w % 5 != 0,
            });
        }
        let ctx = make_ctx(strats, entries, 30, 30, 10_000);
        let rails = RiskRailsConfig::default();

        let strict = compute_auto_allocator_report(
            &ctx,
            &AutoAllocatorRequest {
                require_stress_resilience: true,
                ..AutoAllocatorRequest::default()
            },
            &rails,
            false,
            false,
        );
        assert_eq!(strict.active_winners.len(), 1);
        assert_eq!(strict.active_winners[0].strategy, "steady-edge");
        assert_eq!(strict.active_winners[0].stress.folds_total, 3);
        assert_eq!(strict.active_winners[0].stress.folds_positive, 3);
        assert!(strict.active_winners[0].stress.stress_resilient);
        assert!(
            strict.active_winners[0].stress.extra_slippage_1c_cents
                < strict.active_winners[0].stress.extra_slippage_0c_cents
        );
        assert!(
            strict.active_winners[0].stress.extra_slippage_2c_cents
                < strict.active_winners[0].stress.extra_slippage_1c_cents
        );
        assert!(strict.quarantined.iter().any(|q| q.strategy == "brittle-one-fold" && q.reason.contains("1/3 positive OOS folds")));
        assert!(strict.quarantined.iter().any(|q| q.strategy == "razor-thin" && q.reason.contains("+1c adverse-fill stress")));

        let relaxed = compute_auto_allocator_report(
            &ctx,
            &AutoAllocatorRequest {
                require_stress_resilience: false,
                ..AutoAllocatorRequest::default()
            },
            &rails,
            false,
            false,
        );
        assert!(relaxed.active_winners.len() >= 2, "disabling stress guard surfaces brittle/thin strategies");
    }

    #[test]
    fn regime_throttle_dials_down_on_drawdown_without_lookahead() {
        let strats = vec![StrategyMeta {
            name: "regime-edge".into(),
            side: "yes".into(),
        }];
        // 40 windows total: IS = 0..20 (all strong wins so IS selects strategy),
        // OOS = 20..40 where epochs 20..29 win (building peak) and 30..34 lose consecutively (drawdown),
        // then 35..39 recover (or flip in future-canary).
        let build_ctx = |tail_wins: bool| {
            let mut entries = Vec::new();
            for w in 0..40i64 {
                let epoch = 1_000 + w * 900;
                let won = if w < 20 {
                    w % 6 != 0
                } else if w < 30 {
                    true
                } else if w < 35 {
                    false // 5 consecutive losses -> drawdown > 8% & 5/5 neg epochs
                } else {
                    tail_wins
                };
                entries.push(TournamentEntry {
                    close_epoch: epoch,
                    ticker: format!("KXBTC15M-RT{w:02}"),
                    series: "KXBTC15M".into(),
                    strategy_idx: 0,
                    strategy: "regime-edge".into(),
                    side: "yes".into(),
                    entry_price: 0.35,
                    secs_to_close: 240,
                    won,
                });
            }
            make_ctx(strats.clone(), entries, 40, 20, 1_000 + 20 * 900)
        };

        let ctx_recover = build_ctx(true);
        let ctx_bleed = build_ctx(false);
        let rails = RiskRailsConfig::default();

        let req_throttled = AutoAllocatorRequest {
            sizing_mode: "half_kelly".into(),
            max_slot_risk_pct: 25.0,
            max_total_heat_pct: 25.0,
            maker_fill_rate: 1.0,
            require_stress_resilience: false,
            enable_regime_throttle: true,
            ..AutoAllocatorRequest::default()
        };
        let req_flat = AutoAllocatorRequest {
            enable_regime_throttle: false,
            ..req_throttled.clone()
        };

        let r_throt_rec = compute_auto_allocator_report(&ctx_recover, &req_throttled, &rails, false, false);
        let r_throt_bleed = compute_auto_allocator_report(&ctx_bleed, &req_throttled, &rails, false, false);
        let r_flat_bleed = compute_auto_allocator_report(&ctx_bleed, &req_flat, &rails, false, false);

        // 1. Causal check: checkpoint 14 (before w=35) must match identically regardless of w>=35 flip!
        assert!(
            (r_throt_rec.comparison.equity_curve[14].allocator_equity
                - r_throt_bleed.comparison.equity_curve[14].allocator_equity)
                .abs()
                < 1e-9,
            "causal throttle must never look ahead into future epochs"
        );

        // 2. Throttle dials down allocation and lowers max drawdown vs unthrottled when losing streak hits.
        assert!(r_throt_bleed.regime_throttle_mult < 1.0);
        assert!(r_throt_bleed.current_oos_drawdown_pct > 4.0);
        assert!(r_throt_bleed.total_allocated_pct < r_flat_bleed.total_allocated_pct);
        assert!(
            r_throt_bleed.comparison.auto_allocator_max_dd_pct
                <= r_flat_bleed.comparison.auto_allocator_max_dd_pct + 1e-9
        );

        // 3. Gross vs fee drag & regime attribution populated consistently.
        let w0 = &r_throt_rec.active_winners[0];
        assert!(
            ((w0.gross_expectancy_cents - w0.fee_drag_cents) - w0.oos_net_expectancy_cents).abs()
                < 1e-6
        );
        assert!(!r_throt_rec.regime_attribution.is_empty());
        assert_eq!(r_throt_rec.regime_attribution[0].regime, "Underdog (<45c)");
    }

    #[test]
    fn fill_rate_gate_and_shadow_drift_gauge_flag_decay_and_low_fills() {
        let strats = vec![StrategyMeta {
            name: "decaying-maker".into(),
            side: "yes".into(),
        }];
        let mut entries = Vec::new();
        // IS (0..30): 90% WR at 0.45 -> strong ~+44c paper EV
        // OOS (30..60): first 20 wins (Fold 0 & Fold 1 positive), last 10 lose (Fold 2 negative -> DECAY_CRITICAL)
        for w in 0..60i64 {
            let epoch = 5_000 + w * 900;
            let won = if w < 30 {
                w % 10 != 0
            } else {
                w < 50
            };
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXBTC15M-FG{w:02}"),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "decaying-maker".into(),
                side: "yes".into(),
                entry_price: 0.45,
                secs_to_close: 240,
                won,
            });
        }
        let ctx = make_ctx(strats, entries, 60, 30, 5_000 + 30 * 900);
        let rails = RiskRailsConfig::default();
        let req = AutoAllocatorRequest {
            maker_fill_rate: 0.25,
            min_fill_rate_pct: 50.0,
            ..AutoAllocatorRequest::default()
        };
        let rep = compute_auto_allocator_report(&ctx, &req, &rails, false, false);
        assert_eq!(rep.fill_sensitivity.len(), 4);
        assert!(!rep.fill_sensitivity[0].clears_min_fill_gate); // 25% < 50%
        assert!(rep.fill_sensitivity[1].clears_min_fill_gate);  // 50% >= 50%
        assert!(
            rep.daily_target
                .blockers
                .iter()
                .any(|b| b.contains("Maker fill rate 25% below minimum queue gate (50%)")),
            "blockers={:?}",
            rep.daily_target.blockers
        );
        assert_eq!(rep.drift_gauge.status, "DECAY_CRITICAL");
        assert!(
            rep.daily_target
                .blockers
                .iter()
                .any(|b| b.contains("Paper-to-shadow expectancy decay critical")),
            "blockers={:?}",
            rep.daily_target.blockers
        );
    }

    #[test]
    fn edge_screener_and_best_basket_rank_causally_and_respect_rails_and_kill_switch() {
        let strats = vec![
            StrategyMeta { name: "eth-maker-ace".into(), side: "yes".into() },
            StrategyMeta { name: "btc-maker-two".into(), side: "no".into() },
            StrategyMeta { name: "sol-loser".into(), side: "yes".into() },
        ];
        let mut entries = Vec::new();
        for w in 0..40i64 {
            let epoch = 10_000 + w * 900;
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXETH15M-E{w:02}"),
                series: "KXETH15M".into(),
                strategy_idx: 0,
                strategy: "eth-maker-ace".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: w % 5 != 0, // 80% win
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXBTC15M-B{w:02}"),
                series: "KXBTC15M".into(),
                strategy_idx: 1,
                strategy: "btc-maker-two".into(),
                side: "no".into(),
                entry_price: 0.52,
                secs_to_close: 240,
                won: w % 3 != 0, // 67% win, independent from mod 5
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXSOL15M-S{w:02}"),
                series: "KXSOL15M".into(),
                strategy_idx: 2,
                strategy: "sol-loser".into(),
                side: "yes".into(),
                entry_price: 0.78,
                secs_to_close: 180,
                won: w % 4 == 0, // 25% win -> quarantined
            });
        }
        let ctx = make_ctx(strats, entries, 40, 20, 10_000 + 20 * 900);
        let rails = RiskRailsConfig {
            max_order_contracts: 10,
            max_order_dollars: 40.0,
            ..RiskRailsConfig::default()
        };
        let req = AutoAllocatorRequest {
            starting_bankroll: 500.0,
            min_oos_trades: 10,
            ..AutoAllocatorRequest::default()
        };

        let rep = compute_auto_allocator_report(&ctx, &req, &rails, false, false);
        assert!(!rep.live_armed);
        assert_eq!(rep.edge_screener.len(), 3);
        assert_eq!(rep.edge_screener[0].rank, 1);
        assert_eq!(rep.edge_screener[0].strategy, "eth-maker-ace");
        assert_eq!(rep.edge_screener[0].top_series, "KXETH15M");
        assert!(rep.edge_screener[0].basket_eligible);
        assert!(rep.edge_screener[0].fill_headroom_pct > 0.0);
        assert_eq!(rep.edge_screener[2].strategy, "sol-loser");
        assert_eq!(rep.edge_screener[2].verdict, "QUARANTINED");
        assert!(!rep.edge_screener[2].basket_eligible);
        assert!(rep.best_basket.ready_for_dry_run_preflight);
        assert_eq!(rep.best_basket.legs.len(), 2);
        assert!(rep.best_basket.legs.iter().all(|l| l.target_contracts <= rails.max_order_contracts));

        // Kill switch latched must fail-close best basket preflight readiness.
        let rep_killed = compute_auto_allocator_report(&ctx, &req, &rails, false, true);
        assert!(!rep_killed.best_basket.ready_for_dry_run_preflight);
        assert!(rep_killed.best_basket.legs.is_empty());
    }

    #[test]
    fn bootstrap_stress_sim_runs_1000_trials_and_flags_tail_risk_and_1c_degradation() {
        let strats = vec![StrategyMeta {
            name: "causal-maker".into(),
            side: "yes".into(),
        }];
        let mut entries = Vec::new();
        for w in 0..40i64 {
            entries.push(TournamentEntry {
                close_epoch: 20_000 + w * 900,
                ticker: format!("KXBTC15M-BS{w:02}"),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "causal-maker".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: w % 4 != 0, // 75% win rate -> positive edge
            });
        }
        let ctx = make_ctx(strats, entries, 40, 20, 20_000 + 20 * 900);
        let rails = RiskRailsConfig::default();
        let req = AutoAllocatorRequest {
            starting_bankroll: 500.0,
            min_oos_trades: 10,
            ..AutoAllocatorRequest::default()
        };

        let rep = compute_auto_allocator_report(&ctx, &req, &rails, false, false);
        let bs = &rep.bootstrap_stress;
        assert_eq!(bs.trials, 1_000);
        assert_eq!(bs.horizon_days, 30);
        assert_eq!(bs.trajectory.len(), 31);
        assert!((bs.trajectory[0].p50_equity - 500.0).abs() < 1e-9);
        assert!(bs.p5_end_equity <= bs.p50_end_equity + 1e-6);
        assert!(bs.p50_end_equity <= bs.p95_end_equity + 1e-6);
        assert!(
            bs.stress_1c_p50_end_equity <= bs.p50_end_equity + 1e-6,
            "+1c adverse stress ({}) must not beat base +0c ({})",
            bs.stress_1c_p50_end_equity,
            bs.p50_end_equity
        );
        assert!(bs.p95_max_drawdown_pct >= 0.0 && bs.p95_max_drawdown_pct <= 100.0);
        assert!(bs.daily_rail_breach_prob_pct >= 0.0 && bs.daily_rail_breach_prob_pct <= 100.0);
        assert!(bs.prob_500_day_pct >= 0.0 && bs.prob_500_day_pct <= 100.0);
    }
}
