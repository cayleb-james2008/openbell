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
fn default_enable_diversification_guard() -> bool {
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
    #[serde(default = "default_enable_diversification_guard")]
    pub enable_diversification_guard: bool,
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
            enable_diversification_guard: default_enable_diversification_guard(),
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
            enable_diversification_guard: self.enable_diversification_guard,
            min_fill_rate_pct: finite_or(self.min_fill_rate_pct, default_min_fill_rate_pct())
                .clamp(10.0, 100.0),
        }
    }
}

/// Chronological walk-forward fold consistency and `+0c/+1c/+2c` adverse-fill stress profile.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct RegimeEdgeBucket {
    pub regime: String,
    pub trades: usize,
    pub win_rate: f64,
    pub gross_expectancy_cents: f64,
    pub fee_drag_cents: f64,
    pub net_expectancy_cents: f64,
}

/// Maker fill-rate sensitivity row evaluating queue adverse selection across fill tiers.
#[derive(Debug, Clone, Serialize)]
pub struct FillRateSensitivityPoint {
    pub fill_rate_pct: f64,
    pub adverse_haircut_cents: f64,
    pub effective_ev_cents: Option<f64>,
    pub projected_daily_net_dollars: f64,
    pub clears_min_fill_gate: bool,
}

/// Paper IS / baseline vs trailing OOS live-shadow expectancy drift telemetry.
#[derive(Debug, Clone, Serialize)]
pub struct DriftGauge {
    pub paper_expectancy_cents: Option<f64>,
    pub shadow_expectancy_cents: Option<f64>,
    pub drift_cents: Option<f64>,
    pub status: String,
    pub summary: String,
}

/// Strictly causal multi-market spread, fee, fill-headroom, and $500/day edge screener row.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapEquityPoint {
    pub day: u32,
    pub p5_equity: f64,
    pub p50_equity: f64,
    pub p95_equity: f64,
    pub stress_1c_p50_equity: f64,
}

/// 1,000-trial causal bootstrap equity & drawdown stress simulator (`+0c` vs `+1c` adverse fill).
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
    pub execution_throttle_mult: f64,
    pub execution_status: String,
    pub allocated_pct: f64,
    pub allocated_dollars: f64,
    pub rail_clamped_contracts: u32,
    pub max_abs_corr: Option<f64>,
    pub status: String,
    pub stress: StressProfile,
}

/// One strategy assigned `0%` weight due to insufficient OOS depth, negative EV, or correlation conflict.
#[derive(Debug, Clone, Serialize)]
pub struct QuarantinedStrategy {
    pub strategy: String,
    pub side: String,
    pub oos_trades: usize,
    pub oos_net_expectancy_cents: Option<f64>,
    pub reason: String,
}

/// Chronological equity checkpoint comparing Legacy 1/N Taker vs Auto-Allocator.
#[derive(Debug, Clone, Serialize)]
pub struct ComparisonPoint {
    pub close_epoch: i64,
    pub legacy_equity: f64,
    pub allocator_equity: f64,
}

/// OOS counterfactual replay comparing legacy 1/N equal-split taker vs auto-allocator.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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

/// Execution-ready dry-run leg with exact Kalshi maker/taker fees, observable spread cost,
/// minimum fill size for positive post-fee edge, and correlation cap audit (`live_armed: false`).
#[derive(Debug, Clone, Serialize)]
pub struct DryRunOrderPlanLeg {
    pub strategy: String,
    pub side: String,
    pub series: String,
    pub post_only: bool,
    pub target_contracts: u32,
    pub max_limit_cents: u8,
    pub notional_dollars: f64,
    pub maker_fee_per_ct_cents: f64,
    pub taker_fee_per_ct_cents: f64,
    pub exact_order_fee_dollars: f64,
    pub spread_cross_cost_cents: f64,
    pub slippage_cost_cents: f64,
    pub gross_oos_ev_cents: f64,
    pub net_oos_ev_after_all_cents: f64,
    pub min_fill_contracts_for_positive_ev: u32,
    pub correlation_cap_applied: bool,
    pub sanity_passed: bool,
    pub verdict: String,
    pub blockers: Vec<String>,
}

/// Read-only execution-ready dry-run trade ticket & fee/spread sanity audit (`live_armed: false`).
#[derive(Debug, Clone, Serialize)]
pub struct DryRunOrderPlan {
    pub status: String,
    pub live_armed: bool,
    pub execution_mode: String,
    pub total_contracts: u32,
    pub total_notional_dollars: f64,
    pub total_exact_fee_dollars: f64,
    pub weighted_net_oos_ev_after_all_cents: Option<f64>,
    pub correlation_ceiling_status: String,
    pub legs: Vec<DryRunOrderPlanLeg>,
    pub summary: String,
}

/// Partial-fill & orderbook impact projection at 1x, 2x, and 5x recommended contract sizing.
#[derive(Debug, Clone, Serialize)]
pub struct LiquidityScaleScenario {
    pub scale_label: String,
    pub scale_factor: u32,
    pub target_contracts: u32,
    pub fill_ratio: f64,
    pub impact_drag_cents: f64,
    pub net_ev_after_impact_cents: f64,
    pub feasible: bool,
}

/// Per-slot causal orderbook depth & scalable bankroll capacity telemetry.
#[derive(Debug, Clone, Serialize)]
pub struct LiquidityCapacityLeg {
    pub strategy: String,
    pub side: String,
    pub series: String,
    pub base_contracts: u32,
    pub max_limit_cents: u8,
    pub causal_book_depth_contracts: f64,
    pub depth_source: String,
    pub capacity_tier: String,
    pub max_scalable_contracts: u32,
    pub max_scalable_dollars: f64,
    pub scenarios: Vec<LiquidityScaleScenario>,
}

/// Causal orderbook depth, 1x/2x/5x scaling stress & max scalable dollar capacity (`live_armed: false`).
#[derive(Debug, Clone, Serialize)]
pub struct LiquidityCapacityReport {
    pub status: String,
    pub live_armed: bool,
    pub observed_depth_windows: usize,
    pub median_book_depth_contracts: f64,
    pub base_contracts: u32,
    pub max_scalable_contracts: u32,
    pub max_scalable_dollars: f64,
    pub max_scalable_bankroll: f64,
    pub portfolio_scenarios: Vec<LiquidityScaleScenario>,
    pub legs: Vec<LiquidityCapacityLeg>,
    pub summary: String,
}

/// Honest $500/day capital-readiness verdict derived from fee-adjusted OOS expectancy.
#[derive(Debug, Clone, Serialize)]
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

/// One rung on the causal adverse-fill slippage sensitivity ladder (`+0c`..`+3c`).
#[derive(Debug, Clone, Serialize)]
pub struct SlippageLadderStep {
    pub extra_slippage_cents: f64,
    pub net_expectancy_cents: Option<f64>,
    pub projected_daily_net_dollars: f64,
    pub oos_net_pnl_dollars: f64,
    pub daily_rail_breach_prob_pct: f64,
    pub positive_ev: bool,
    pub rail_safe: bool,
    pub survival_ok: bool,
}

/// Zero-config strategy preset ranked across OOS + `+1c` adverse-fill bootstrap trials.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyPresetCandidate {
    pub id: String,
    pub label: String,
    pub subtitle: String,
    pub config: AutoAllocatorRequest,
    pub active_winners_count: usize,
    pub net_expectancy_cents: Option<f64>,
    pub oos_net_pnl_dollars: f64,
    pub oos_max_dd_pct: f64,
    pub bootstrap_p50_equity: f64,
    pub bootstrap_p95_max_dd_pct: f64,
    pub stress_1c_p50_equity: f64,
    pub stress_1c_p95_max_dd_pct: f64,
    pub daily_rail_breach_prob_pct: f64,
    pub stress_1c_daily_rail_breach_prob_pct: f64,
    pub risk_adjusted_score: f64,
    pub safe_gate_passed: bool,
    pub is_top_safe: bool,
    pub calm_expectancy_cents: Option<f64>,
    pub high_vol_expectancy_cents: Option<f64>,
    pub calm_stress_1c_survival: bool,
    pub high_vol_stress_1c_survival: bool,
    pub regime_stability: String,
    pub breakeven_slippage_cents: Option<f64>,
    pub slippage_cushion_status: String,
    pub slippage_ladder: Vec<SlippageLadderStep>,
    pub note: String,
}

/// Causal chronological Calm vs High-Vol / Wide-Spread walk-forward regime stability report.
#[derive(Debug, Clone, Serialize)]
pub struct RegimeStabilityReport {
    pub status: String,
    pub causal_percentile_pct: f64,
    pub calm_windows: usize,
    pub high_vol_windows: usize,
    pub active_calm_ev_cents: Option<f64>,
    pub active_high_vol_ev_cents: Option<f64>,
    pub active_calm_stress_1c_ok: bool,
    pub active_high_vol_stress_1c_ok: bool,
    pub all_regime_safe_presets: usize,
    pub summary: String,
}

/// Paper forward-test envelope monitor comparing realized OOS expectancy & slippage
/// against the active preset's predicted bootstrap P5-P50 trajectory and selecting
/// the next 0%-rail-breach safe rotation preset (`live_armed: false`).
#[derive(Debug, Clone, Serialize)]
pub struct ForwardEnvelopeMonitor {
    pub status: String,
    pub active_preset_id: Option<String>,
    pub active_preset_label: String,
    pub envelope_day: u32,
    pub envelope_p5_equity: f64,
    pub envelope_p50_equity: f64,
    pub observed_paper_equity: f64,
    pub predicted_ev_cents: Option<f64>,
    pub observed_forward_ev_cents: Option<f64>,
    pub expectancy_drift_cents: Option<f64>,
    pub realized_slippage_vs_model_cents: f64,
    pub daily_rail_breach_prob_pct: f64,
    pub stress_1c_daily_rail_breach_prob_pct: f64,
    pub next_safe_preset_id: Option<String>,
    pub next_safe_preset_label: Option<String>,
    pub live_armed: bool,
    pub summary: String,
}

/// Connected cross-asset correlation cluster (`|r| >= 0.55`) and cluster heat cap telemetry.
#[derive(Debug, Clone, Serialize)]
pub struct AssetClusterRisk {
    pub cluster_id: String,
    pub assets: Vec<String>,
    pub raw_heat_pct: f64,
    pub guarded_heat_pct: f64,
    pub max_intra_cluster_corr: Option<f64>,
    pub penalty_applied_pct: f64,
}

/// Cross-asset (`BTC`/`ETH`/`SOL`/`XRP`/`DOGE`) & strategy cluster exposure ceiling guard report.
#[derive(Debug, Clone, Serialize)]
pub struct DiversificationGuardReport {
    pub enabled: bool,
    pub status: String,
    pub effective_diversification_score: f64,
    pub max_cross_asset_corr: Option<f64>,
    pub correlated_cluster_heat_pct: f64,
    pub cluster_heat_cap_pct: f64,
    pub heat_trimmed_pct: f64,
    pub daily_rail_ceiling_pct: f64,
    pub regime_drawdown_ceiling_pct: f64,
    pub hv_regime_mult: f64,
    pub worst_correlated_cluster_heat_pct: f64,
    pub ceiling_trimmed_pct: f64,
    pub ceiling_status: String,
    pub clusters: Vec<AssetClusterRisk>,
    pub diversified_safe_preset_id: Option<String>,
    pub summary: String,
}

/// Concise session briefing & slippage attribution report for 1-click copy/download (`live_armed: false`).
#[derive(Debug, Clone, Serialize)]
pub struct SessionBriefingReport {
    pub status: String,
    pub live_armed: bool,
    pub expected_fill_edge_cents: Option<f64>,
    pub realized_fill_edge_cents: Option<f64>,
    pub fill_edge_delta_cents: Option<f64>,
    pub fee_drag_cents: f64,
    pub maker_fee_savings_dollars: f64,
    pub correlation_cluster_penalty_pct: f64,
    pub regime_drawdown_ceiling_pct: f64,
    pub worst_correlated_cluster_heat_pct: f64,
    pub ceiling_status: String,
    pub daily_loss_rail_dollars: f64,
    pub projected_worst_day_loss_dollars: f64,
    pub daily_loss_headroom_dollars: f64,
    pub daily_loss_headroom_pct: f64,
    pub active_winners_count: usize,
    pub projected_daily_net_dollars: f64,
    pub regime_stability: String,
    pub calm_expectancy_cents: Option<f64>,
    pub high_vol_expectancy_cents: Option<f64>,
    pub breakeven_slippage_cents: Option<f64>,
    pub slippage_cushion_status: String,
    pub slippage_ladder: Vec<SlippageLadderStep>,
    pub summary: String,
}

/// Complete response payload for `GET /api/allocator` and `POST /api/allocator/plan`.
#[derive(Debug, Clone, Serialize)]
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
    pub enable_diversification_guard: bool,
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
    pub preset_leaderboard: Vec<StrategyPresetCandidate>,
    pub top_safe_preset: Option<String>,
    pub regime_stability: RegimeStabilityReport,
    pub breakeven_slippage_cents: Option<f64>,
    pub slippage_cushion_status: String,
    pub slippage_ladder: Vec<SlippageLadderStep>,
    pub forward_envelope: ForwardEnvelopeMonitor,
    pub diversification: DiversificationGuardReport,
    pub session_briefing: SessionBriefingReport,
    pub comparison: AllocatorComparison,
    pub planned_intents: Vec<PlannedAllocatorIntent>,
    pub dry_run_plan: DryRunOrderPlan,
    pub liquidity_capacity: LiquidityCapacityReport,
    pub daily_target: DailyTargetVerdict,
    pub execution_health: crate::execution_audit::ExecutionHealthSummary,
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
    audit: Option<&crate::execution_audit::ExecutionHealthReport>,
) -> SelectionResult {
    let mut qualifying: Vec<SplitCandidate> = Vec::new();
    let mut quarantined: Vec<QuarantinedStrategy> = Vec::new();
    let mut expectancy_by_idx: HashMap<usize, f64> = HashMap::new();

    for (si, meta) in ctx.strategies.iter().enumerate() {
        let list = &entries_by_strat[si];
        let n = list.len();
        let exec_q_entry = audit.and_then(|h| h.quarantine_state.find_entry(&meta.name));

        if n == 0 {
            let reason = if let Some(qe) =
                exec_q_entry.filter(|e| e.status == "quarantined" || e.throttle_mult <= 0.0)
            {
                format!(
                    "execution quarantine: {}",
                    if qe.reason.is_empty() {
                        "adverse live/shadow markout decay"
                    } else {
                        qe.reason.as_str()
                    }
                )
            } else {
                format!("insufficient OOS depth (< {min_trades}): 0 trades")
            };
            quarantined.push(QuarantinedStrategy {
                strategy: meta.name.clone(),
                side: meta.side.clone(),
                oos_trades: 0,
                oos_net_expectancy_cents: None,
                reason,
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

        if let Some(qe) = exec_q_entry.filter(|e| e.status == "quarantined" || e.throttle_mult <= 0.0) {
            quarantined.push(QuarantinedStrategy {
                strategy: meta.name.clone(),
                side: meta.side.clone(),
                oos_trades: n,
                oos_net_expectancy_cents: finite_opt(exp_cents),
                reason: format!(
                    "execution quarantine (live/shadow decay): {} (30s markout {:+.2}c, rolling fills {})",
                    if qe.reason.is_empty() {
                        "adverse markout / negative expectancy"
                    } else {
                        qe.reason.as_str()
                    },
                    qe.rolling_markout_30s_cents,
                    qe.rolling_fills,
                ),
            });
            continue;
        }

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
    compute_auto_allocator_report_with_orderbook(ctx, raw_req, rails, live_armed, killed, None)
}

/// Same as `compute_auto_allocator_report`, optionally incorporating live L2 depth
/// from `OrderbookStore` (`walk_book_fresh` + `queue_ahead_at_fresh`) in capacity sizing.
pub fn compute_auto_allocator_report_with_orderbook(
    ctx: &TournamentTapeContext,
    raw_req: &AutoAllocatorRequest,
    rails: &RiskRailsConfig,
    live_armed: bool,
    killed: bool,
    orderbook: Option<&crate::orderbook::OrderbookStore>,
) -> AutoAllocatorReport {
    compute_auto_allocator_report_with_audit(
        ctx,
        raw_req,
        rails,
        live_armed,
        killed,
        orderbook,
        None,
    )
}

/// Same as `compute_auto_allocator_report_with_orderbook`, also ingesting live/shadow
/// `ExecutionHealthReport` & `quarantine_state.json` decay telemetry so live-decaying or
/// quarantined strategies are throttled/demoted even when static `snapshots.jsonl` OOS is positive.
pub fn compute_auto_allocator_report_with_audit(
    ctx: &TournamentTapeContext,
    raw_req: &AutoAllocatorRequest,
    rails: &RiskRailsConfig,
    live_armed: bool,
    killed: bool,
    orderbook: Option<&crate::orderbook::OrderbookStore>,
    audit: Option<&crate::execution_audit::ExecutionHealthReport>,
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
        audit,
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
            None,
        ))
    } else {
        None
    };

    let n_winners = oos_sel.accepted.len();
    let (epoch_is_high_vol, calm_windows, high_vol_windows) = classify_causal_epoch_regimes(ctx);

    // Apply cross-asset correlation cluster guard & causal regime drawdown ceiling strictly causally on IS
    // for OOS replay (zero lookahead), and on OOS winners for forward live allocation weights.
    let (guarded_oos_weights_pct, mut diversification) = apply_diversification_guard(
        &oos_by_strat,
        &oos_sel.accepted,
        &oos_sel.weights_pct,
        &req,
        is_maker,
        slip_dollars,
        &epoch_is_high_vol,
        rails.max_daily_loss_dollars,
    );
    let (replay_weights, selection_basis) = build_causal_replay_weights(
        is_sel.as_ref(),
        &oos_sel,
        &guarded_oos_weights_pct,
        &is_by_strat,
        &req,
        is_maker,
        slip_dollars,
        &epoch_is_high_vol,
        rails.max_daily_loss_dollars,
    );

    let (comparison, regime_throttle_mult, current_oos_drawdown_pct, throttle_reason, oos_window_returns) =
        replay_oos_comparison(
            ctx,
            &req,
            &oos_by_epoch,
            &replay_weights,
            is_maker,
            selection_basis,
        );

    let mut active_winners = Vec::with_capacity(n_winners);
    let mut planned_intents = Vec::with_capacity(n_winners);
    let mut active_idx_set = std::collections::HashSet::with_capacity(n_winners);
    let mut sum_alloc_pct = 0.0f64;

    for (i, cand) in oos_sel.accepted.iter().enumerate() {
        active_idx_set.insert(cand.strategy_idx);
        let exec_entry = audit.and_then(|h| h.quarantine_state.find_entry(&cand.strategy));
        let execution_throttle_mult = exec_entry.map_or(1.0, |e| {
            if e.status == "quarantined" {
                0.0
            } else if e.status == "throttled" {
                finite_or(e.throttle_mult, 0.5).clamp(0.05, 1.0)
            } else {
                1.0
            }
        });
        let execution_status = exec_entry
            .map(|e| e.status.clone())
            .unwrap_or_else(|| "active".to_string());
        let alloc_pct = finite_or(
            guarded_oos_weights_pct[i] * regime_throttle_mult * execution_throttle_mult,
            0.0,
        );
        sum_alloc_pct += alloc_pct;
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
            "Blocked: emergency kill switch is latched".to_string()
        } else if !live_armed {
            "Paper plan only: live_armed=false (disarmed on boot)".to_string()
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
            execution_throttle_mult,
            execution_status,
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

    let total_allocated_pct = if n_winners == 0 {
        0.0
    } else {
        finite_or(sum_alloc_pct.clamp(0.0, 100.0), 0.0)
    };
    let cash_reserve_pct = finite_or((100.0 - total_allocated_pct).clamp(0.0, 100.0), 100.0);

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
    let (preset_leaderboard, top_safe_preset) = compute_preset_leaderboard(
        ctx,
        &req,
        rails,
        &is_by_strat,
        &oos_by_strat,
        &oos_by_epoch,
        &epoch_is_high_vol,
        is_total_entries,
        audit,
    );
    let (active_calm_ev, active_hv_ev, active_calm_1c_ok, active_hv_1c_ok, active_reg_status) =
        evaluate_regime_stability(
            &oos_by_strat,
            &oos_sel.accepted,
            &guarded_oos_weights_pct,
            &epoch_is_high_vol,
            is_maker,
            slip_dollars,
        );
    let all_regime_safe_presets = preset_leaderboard
        .iter()
        .filter(|p| p.safe_gate_passed && p.regime_stability == "ALL-REGIME")
        .count();
    let regime_stability = RegimeStabilityReport {
        status: active_reg_status.clone(),
        causal_percentile_pct: 60.0,
        calm_windows,
        high_vol_windows,
        active_calm_ev_cents: active_calm_ev,
        active_high_vol_ev_cents: active_hv_ev,
        active_calm_stress_1c_ok: active_calm_1c_ok,
        active_high_vol_stress_1c_ok: active_hv_1c_ok,
        all_regime_safe_presets,
        summary: format!(
            "{}: calm {} vs high-vol {} across {}/{} causal epochs ({} ALL-REGIME safe preset(s))",
            active_reg_status,
            active_calm_ev.map(|v| format!("{v:+.2}c")).unwrap_or_else(|| "n/a".to_string()),
            active_hv_ev.map(|v| format!("{v:+.2}c")).unwrap_or_else(|| "n/a".to_string()),
            calm_windows,
            high_vol_windows,
            all_regime_safe_presets,
        ),
    };
    diversification.diversified_safe_preset_id = preset_leaderboard
        .iter()
        .find(|p| p.id == "diversified_safe_basket" && p.safe_gate_passed)
        .or_else(|| preset_leaderboard.iter().find(|p| p.safe_gate_passed))
        .map(|p| p.id.clone());
    let forward_envelope = compute_forward_envelope_monitor(
        &req,
        &active_winners,
        &drift_gauge,
        &bootstrap_stress,
        &comparison,
        &preset_leaderboard,
        oos_window_returns.len(),
        live_armed,
    );

    let active_weighted_ev_cents = if active_winners.is_empty() {
        None
    } else {
        let w_sum: f64 = active_winners.iter().map(|w| w.allocated_pct.max(0.01)).sum();
        if w_sum > 1e-12 {
            finite_opt(
                active_winners
                    .iter()
                    .map(|w| w.oos_net_expectancy_cents * (w.allocated_pct.max(0.01) / w_sum))
                    .sum::<f64>(),
            )
        } else {
            None
        }
    };
    let active_legs: Vec<(f64, f64, usize, f64)> = active_winners
        .iter()
        .map(|w| {
            (
                w.allocated_pct,
                w.oos_avg_entry_price,
                w.oos_trades,
                w.oos_net_expectancy_cents,
            )
        })
        .collect();
    let (slippage_ladder, breakeven_slippage_cents, slippage_cushion_status) =
        compute_slippage_ladder_and_cushion(
            active_weighted_ev_cents,
            &active_legs,
            ctx.windows_settled_oos,
            &req,
            rails.max_daily_loss_dollars,
            &bootstrap_stress,
            &oos_window_returns,
            is_maker,
            1_000,
        );

    let dry_run_plan = compute_dry_run_order_plan(
        ctx,
        &req,
        &oos_by_strat,
        &active_winners,
        &diversification,
        is_maker,
        killed,
        live_armed,
    );
    let liquidity_capacity = compute_liquidity_capacity(
        ctx,
        &req,
        &oos_by_strat,
        &dry_run_plan,
        is_maker,
        orderbook,
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
    let session_briefing = compute_session_briefing(
        &req,
        rails,
        &active_winners,
        &drift_gauge,
        &forward_envelope,
        &diversification,
        &bootstrap_stress,
        &comparison,
        &daily_target,
        &regime_stability,
        breakeven_slippage_cents,
        &slippage_cushion_status,
        &slippage_ladder,
        &oos_window_returns,
        live_armed,
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
            "0 strategies cleared min_oos_trades >= {} with positive OOS net expectancy, so 100% is held in cash reserve to protect bankroll.",
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
    if req.enable_diversification_guard {
        honesty_notes.push(format!(
            "Cross-Asset Diversification Guard active: {}",
            diversification.summary
        ));
    }

    let execution_health = audit.map_or_else(
        crate::execution_audit::ExecutionHealthSummary::default,
        |h| h.to_summary(),
    );
    if !execution_health.quarantined_names.is_empty() || !execution_health.throttled_names.is_empty() {
        honesty_notes.push(format!(
            "Execution Markout Guard active: {} (quarantined: [{}], throttled: [{}]).",
            execution_health.summary,
            execution_health.quarantined_names.join(", "),
            execution_health.throttled_names.join(", "),
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
        enable_diversification_guard: req.enable_diversification_guard,
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
        preset_leaderboard,
        top_safe_preset,
        regime_stability,
        breakeven_slippage_cents,
        slippage_cushion_status,
        slippage_ladder,
        forward_envelope,
        diversification,
        session_briefing,
        comparison,
        planned_intents,
        dry_run_plan,
        liquidity_capacity,
        daily_target,
        execution_health,
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

#[inline(never)]
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

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn compute_dry_run_order_plan(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    oos_by_strat: &[Vec<&TournamentEntry>],
    winners: &[AllocatedWinner],
    diversification: &DiversificationGuardReport,
    is_maker: bool,
    killed: bool,
    live_armed: bool,
) -> DryRunOrderPlan {
    if winners.is_empty() {
        return DryRunOrderPlan {
            status: "NO_DATA".to_string(),
            live_armed,
            execution_mode: req.execution_mode.clone(),
            total_contracts: 0,
            total_notional_dollars: 0.0,
            total_exact_fee_dollars: 0.0,
            weighted_net_oos_ev_after_all_cents: None,
            correlation_ceiling_status: diversification.ceiling_status.clone(),
            legs: Vec::new(),
            summary: "No active OOS winners available for a dry-run order plan.".to_string(),
        };
    }

    let idx_by_name: HashMap<&str, usize> = ctx
        .strategies
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let corr_capped = diversification.heat_trimmed_pct > 1e-6
        || diversification.ceiling_trimmed_pct > 1e-6
        || diversification.ceiling_status == "CEILING_CLAMPED";
    let slip_cents = if is_maker { 0.0 } else { req.slippage_cents };

    let mut legs = Vec::with_capacity(winners.len());
    let mut total_contracts: u32 = 0;
    let mut total_notional = 0.0f64;
    let mut total_fee = 0.0f64;
    let mut ev_num = 0.0f64;
    let mut ev_den = 0.0f64;

    for w in winners {
        let strat_idx = idx_by_name.get(w.strategy.as_str()).copied();
        let strat_entries = strat_idx.and_then(|i| oos_by_strat.get(i));
        let top_series = dominant_series_for_entries(strat_entries);
        let mean_spread_cents = match strat_entries {
            Some(list) if !list.is_empty() => {
                let sum: f64 = list
                    .iter()
                    .map(|e| ctx.window_spread_cents.get(&e.ticker).copied().unwrap_or(2.0))
                    .sum();
                finite_or(sum / (list.len() as f64), 2.0)
            }
            _ => 2.0,
        };
        let spread_cross_cost_cents = if is_maker {
            0.0
        } else {
            finite_or((0.5 * mean_spread_cents).max(0.0), 0.0)
        };

        let max_limit_cents =
            ((w.oos_avg_entry_price * 100.0).round() as i64).clamp(1, 99) as u8;
        let p = (max_limit_cents as f64) / 100.0;
        let target_contracts = w.rail_clamped_contracts;
        let notional_dollars = finite_or((target_contracts as f64) * p, 0.0);
        let maker_fee_per_ct_cents = finite_or(maker_fee(p) * 100.0, 0.0);
        let taker_fee_per_ct_cents = finite_or(fee(p) * 100.0, 0.0);
        let exact_order_fee_dollars = if target_contracts > 0 {
            finite_or(kalshi_order_fee_dollars(target_contracts, p, is_maker), 0.0)
        } else {
            0.0
        };
        let exact_fee_per_ct_cents = if target_contracts > 0 {
            finite_or((exact_order_fee_dollars * 100.0) / (target_contracts as f64), 0.0)
        } else if is_maker {
            maker_fee_per_ct_cents
        } else {
            taker_fee_per_ct_cents
        };

        // `gross_expectancy_cents` is already before fees/slippage; deduct exact rounded Kalshi fee,
        // observable half-spread crossing cost (taker), and configured slippage.
        let gross_oos_ev_cents = finite_or(w.gross_expectancy_cents, 0.0);
        let net_oos_ev_after_all_cents = finite_or(
            gross_oos_ev_cents - exact_fee_per_ct_cents - spread_cross_cost_cents - slip_cents,
            0.0,
        );

        let mut min_fill_contracts_for_positive_ev: u32 = 0;
        for c in 1u32..=250u32 {
            let fee_c = (kalshi_order_fee_dollars(c, p, is_maker) * 100.0) / (c as f64);
            if gross_oos_ev_cents - fee_c - spread_cross_cost_cents - slip_cents > 0.0 {
                min_fill_contracts_for_positive_ev = c;
                break;
            }
        }

        let mut blockers = Vec::new();
        if killed {
            blockers.push("Emergency kill switch is latched".to_string());
        }
        if diversification.ceiling_status == "UNPROTECTED_BREACH" {
            blockers.push("Correlated cluster exceeds regime drawdown ceiling while guard is off".to_string());
        }
        if target_contracts == 0 {
            blockers.push("Rail-clamped budget below 1 contract".to_string());
        } else if min_fill_contracts_for_positive_ev == 0 {
            blockers.push("Negative OOS EV after exact fees, spread, and slippage".to_string());
        } else if target_contracts < min_fill_contracts_for_positive_ev {
            blockers.push(format!(
                "Size {target_contracts} cts below {min_fill_contracts_for_positive_ev} ct minimum to clear whole-cent Kalshi fee rounding"
            ));
        } else if net_oos_ev_after_all_cents <= 0.0 {
            blockers.push("Post-fee, spread, and slippage OOS expectancy <= 0.0c".to_string());
        }

        let sanity_passed = blockers.is_empty();
        let verdict = if killed {
            "KILLED"
        } else if diversification.ceiling_status == "UNPROTECTED_BREACH" {
            "UNPROTECTED_CLUSTER"
        } else if target_contracts == 0 || (min_fill_contracts_for_positive_ev > 0 && target_contracts < min_fill_contracts_for_positive_ev) {
            "SIZE_BELOW_MIN"
        } else if !sanity_passed {
            "EDGE_BLOCKED"
        } else {
            "EXEC_READY_PAPER"
        }
        .to_string();

        total_contracts = total_contracts.saturating_add(target_contracts);
        total_notional += notional_dollars;
        total_fee += exact_order_fee_dollars;
        let w_weight = if notional_dollars > 1e-9 {
            notional_dollars
        } else {
            w.allocated_pct.max(0.01)
        };
        ev_num += net_oos_ev_after_all_cents * w_weight;
        ev_den += w_weight;

        legs.push(DryRunOrderPlanLeg {
            strategy: w.strategy.clone(),
            side: w.side.clone(),
            series: top_series,
            post_only: is_maker,
            target_contracts,
            max_limit_cents,
            notional_dollars,
            maker_fee_per_ct_cents,
            taker_fee_per_ct_cents,
            exact_order_fee_dollars,
            spread_cross_cost_cents,
            slippage_cost_cents: slip_cents,
            gross_oos_ev_cents,
            net_oos_ev_after_all_cents,
            min_fill_contracts_for_positive_ev,
            correlation_cap_applied: corr_capped,
            sanity_passed,
            verdict,
            blockers,
        });
    }

    let weighted_net_oos_ev_after_all_cents = if ev_den > 1e-9 {
        finite_opt(ev_num / ev_den)
    } else {
        None
    };
    let ready_count = legs.iter().filter(|l| l.sanity_passed).count();
    let status = if !killed
        && ready_count > 0
        && diversification.ceiling_status != "UNPROTECTED_BREACH"
        && weighted_net_oos_ev_after_all_cents.is_some_and(|v| v > 0.0)
    {
        "PLAN_PASS"
    } else {
        "PLAN_BLOCKED"
    }
    .to_string();

    let ev_str = weighted_net_oos_ev_after_all_cents
        .map(|v| format!("{v:+.2}c/ct"))
        .unwrap_or_else(|| "n/a".to_string());
    let summary = format!(
        "{status}: {ready_count}/{} slot(s) ready, {total_contracts} cts (${total_notional:.2} notional, ${total_fee:.2} exact fee), net EV {ev_str}, ceiling {}, live_armed={live_armed}",
        legs.len(),
        diversification.ceiling_status
    );

    DryRunOrderPlan {
        status,
        live_armed,
        execution_mode: req.execution_mode.clone(),
        total_contracts,
        total_notional_dollars: finite_or(total_notional, 0.0),
        total_exact_fee_dollars: finite_or(total_fee, 0.0),
        weighted_net_oos_ev_after_all_cents,
        correlation_ceiling_status: diversification.ceiling_status.clone(),
        legs,
        summary,
    }
}

fn median_f64_slice(vals: &mut [f64]) -> Option<f64> {
    if vals.is_empty() {
        return None;
    }
    vals.sort_unstable_by(|a, b| a.total_cmp(b));
    let m = vals.len() / 2;
    if vals.len() % 2 == 1 {
        finite_opt(vals[m])
    } else {
        finite_opt((vals[m - 1] + vals[m]) * 0.5)
    }
}

fn liquidity_impact_cents(contracts: u32, book_depth: f64, mean_spread_cents: f64) -> (f64, f64) {
    if contracts == 0 {
        return (1.0, 0.0);
    }
    let depth = book_depth.max(1.0);
    let fill_ratio = (depth / (contracts as f64)).clamp(0.0, 1.0);
    let util = (contracts as f64) / depth;
    let half_spread = (0.5 * mean_spread_cents).max(0.5);
    let impact = half_spread * (0.15 * util + 1.15 * (util - 0.25).max(0.0).powf(1.35));
    (finite_or(fill_ratio, 0.0), finite_or(impact.max(0.0), 0.0))
}

fn tier_from_scenarios(scenarios: &[LiquidityScaleScenario]) -> String {
    let s1 = scenarios.first().is_some_and(|s| s.feasible);
    let s2 = scenarios.get(1).is_some_and(|s| s.feasible);
    let s5 = scenarios.get(2).is_some_and(|s| s.feasible);
    if !s1 {
        "CROWDED".to_string()
    } else if s2 && s5 {
        "LIQUID".to_string()
    } else {
        "TIGHT".to_string()
    }
}

/// Model causal orderbook depth and partial-fill impact drag at 1x, 2x, and 5x sizing (`live_armed: false`).
#[inline(never)]
fn compute_liquidity_capacity(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    oos_by_strat: &[Vec<&TournamentEntry>],
    plan: &DryRunOrderPlan,
    is_maker: bool,
    orderbook: Option<&crate::orderbook::OrderbookStore>,
) -> LiquidityCapacityReport {
    if plan.legs.is_empty() {
        return LiquidityCapacityReport {
            status: "NO_DATA".to_string(),
            live_armed: false,
            observed_depth_windows: ctx.window_depth_contracts.len(),
            median_book_depth_contracts: 0.0,
            base_contracts: 0,
            max_scalable_contracts: 0,
            max_scalable_dollars: 0.0,
            max_scalable_bankroll: 0.0,
            portfolio_scenarios: Vec::new(),
            legs: Vec::new(),
            summary: "NO_DATA: 0 active OOS legs for causal liquidity capacity sizing (live_armed=false)"
                .to_string(),
        };
    }

    let scale_factors = [1u32, 2u32, 5u32];
    let mut legs = Vec::with_capacity(plan.legs.len());
    let mut port_c = [0u32; 3];
    let mut port_fill_num = [0.0f64; 3];
    let mut port_imp_num = [0.0f64; 3];
    let mut port_ev_num = [0.0f64; 3];
    let mut port_den = [0.0f64; 3];
    let mut leg_depths = Vec::with_capacity(plan.legs.len());
    let mut obs_depths = Vec::new();
    let mut max_scalable_contracts_sum = 0u32;
    let mut max_scalable_dollars_sum = 0.0f64;

    for leg in &plan.legs {
        let strat_entries = ctx
            .strategies
            .iter()
            .position(|s| s.name == leg.strategy)
            .and_then(|i| oos_by_strat.get(i));

        let mut spread_sum = 0.0f64;
        let mut spread_n = 0usize;
        obs_depths.clear();
        if let Some(entries) = strat_entries {
            for e in entries {
                if let Some(&sp) = ctx.window_spread_cents.get(&e.ticker) {
                    spread_sum += sp;
                    spread_n += 1;
                }
                if let Some(&d) = ctx.window_depth_contracts.get(&e.ticker) {
                    if d.is_finite() && d > 0.0 {
                        obs_depths.push(d);
                    }
                }
            }
        }
        let mean_spread_cents = if spread_n > 0 {
            finite_or(spread_sum / (spread_n as f64), 2.0)
        } else {
            2.0
        };

        if obs_depths.is_empty() && !ctx.window_depth_contracts.is_empty() {
            for (ticker, &d) in &ctx.window_depth_contracts {
                if d.is_finite() && d > 0.0 && ticker.split('-').next() == Some(leg.series.as_str()) {
                    obs_depths.push(d);
                }
            }
        }
        let live_l2_ticker = orderbook.and_then(|ob| ob.fresh_ticker_for_series(&leg.series));
        let l2_total_depth = match (orderbook, &live_l2_ticker) {
            (Some(ob), Some(lt)) => ob
                .walk_book_fresh(lt, &leg.side, 10_000)
                .map(|w| w.fillable_qty as f64)
                .filter(|&d| d > 0.0),
            _ => None,
        };
        let (causal_depth, depth_source) = if let Some(d) = l2_total_depth {
            let maker_q = if is_maker {
                orderbook
                    .zip(live_l2_ticker.as_deref())
                    .and_then(|(ob, lt)| ob.queue_ahead_at_fresh(lt, &leg.side, leg.max_limit_cents))
                    .map(|q| q as f64)
                    .filter(|&q| q > 0.0)
            } else {
                None
            };
            (maker_q.unwrap_or(d), "live_l2_orderbook")
        } else if let Some(m) = median_f64_slice(&mut obs_depths) {
            (m, "observed_l1")
        } else {
            (
                (180.0 / mean_spread_cents.max(1.0)).clamp(15.0, 200.0),
                "spread_heuristic",
            )
        };
        leg_depths.push(causal_depth);

        let p = (leg.max_limit_cents as f64) / 100.0;
        let pre_impact_ev_no_fee =
            leg.gross_oos_ev_cents - leg.spread_cross_cost_cents - leg.slippage_cost_cents;
        let eval_impact = |c: u32| -> (f64, f64) {
            if c > 0 {
                if let (Some(ob), Some(lt)) = (orderbook, live_l2_ticker.as_deref()) {
                    if let Some(w) = ob.walk_book_fresh(lt, &leg.side, c as u64) {
                        let fill_ratio = (w.fillable_qty as f64 / (c as f64)).clamp(0.0, 1.0);
                        return (fill_ratio, finite_or(w.slippage_cents.max(0.0), 0.0));
                    }
                }
            }
            liquidity_impact_cents(c, causal_depth, mean_spread_cents)
        };
        let net_at = |c: u32| -> f64 {
            if c == 0 {
                return pre_impact_ev_no_fee - leg.maker_fee_per_ct_cents;
            }
            let fee_c = (kalshi_order_fee_dollars(c, p, is_maker) * 100.0) / (c as f64);
            let (_, impact) = eval_impact(c);
            finite_or(pre_impact_ev_no_fee - fee_c - impact, -100.0)
        };

        let mut best_c = 0u32;
        for c in 1u32..=32u32 {
            if net_at(c) > 0.0 {
                best_c = c;
            }
        }
        if best_c == 32 && net_at(33) > 0.0 {
            let mut lo = 33u32;
            let mut hi = 10_000u32;
            while lo <= hi {
                let mid = lo + (hi - lo) / 2;
                if net_at(mid) > 0.0 {
                    best_c = mid;
                    lo = mid + 1;
                } else {
                    hi = mid - 1;
                }
            }
        }
        let leg_max_dollars = finite_or((best_c as f64) * p, 0.0);
        max_scalable_contracts_sum = max_scalable_contracts_sum.saturating_add(best_c);
        max_scalable_dollars_sum += leg_max_dollars;

        let mut scenarios = Vec::with_capacity(3);
        let weight = if leg.notional_dollars > 1e-9 {
            leg.notional_dollars
        } else {
            1.0
        };
        for (si, &sf) in scale_factors.iter().enumerate() {
            let c = leg.target_contracts.saturating_mul(sf);
            let (fill_ratio, impact_drag_cents) = eval_impact(c);
            let net_ev_after_impact_cents = if c > 0 {
                net_at(c)
            } else {
                finite_or(leg.net_oos_ev_after_all_cents, 0.0)
            };
            let feasible = c > 0 && fill_ratio >= 0.75 && net_ev_after_impact_cents > 0.0;
            port_c[si] = port_c[si].saturating_add(c);
            port_fill_num[si] += fill_ratio * weight;
            port_imp_num[si] += impact_drag_cents * weight;
            port_ev_num[si] += net_ev_after_impact_cents * weight;
            port_den[si] += weight;
            scenarios.push(LiquidityScaleScenario {
                scale_label: format!("{sf}x"),
                scale_factor: sf,
                target_contracts: c,
                fill_ratio,
                impact_drag_cents,
                net_ev_after_impact_cents,
                feasible,
            });
        }

        let leg_tier = tier_from_scenarios(&scenarios);
        legs.push(LiquidityCapacityLeg {
            strategy: leg.strategy.clone(),
            side: leg.side.clone(),
            series: leg.series.clone(),
            base_contracts: leg.target_contracts,
            max_limit_cents: leg.max_limit_cents,
            causal_book_depth_contracts: causal_depth,
            depth_source: depth_source.to_string(),
            capacity_tier: leg_tier,
            max_scalable_contracts: best_c,
            max_scalable_dollars: leg_max_dollars,
            scenarios,
        });
    }

    let portfolio_scenarios: Vec<LiquidityScaleScenario> = scale_factors
        .iter()
        .enumerate()
        .map(|(si, &sf)| {
            let den = port_den[si].max(1e-9);
            let fill_ratio = finite_or(port_fill_num[si] / den, 0.0);
            let impact_drag_cents = finite_or(port_imp_num[si] / den, 0.0);
            let net_ev_after_impact_cents = finite_or(port_ev_num[si] / den, 0.0);
            let target_contracts = port_c[si];
            let leg_all_feasible = legs
                .iter()
                .all(|l| l.scenarios.get(si).is_some_and(|s| s.feasible));
            let feasible = target_contracts > 0
                && leg_all_feasible
                && fill_ratio >= 0.75
                && net_ev_after_impact_cents > 0.0;
            LiquidityScaleScenario {
                scale_label: format!("{sf}x"),
                scale_factor: sf,
                target_contracts,
                fill_ratio,
                impact_drag_cents,
                net_ev_after_impact_cents,
                feasible,
            }
        })
        .collect();

    let status = tier_from_scenarios(&portfolio_scenarios);
    let median_book_depth_contracts = median_f64_slice(&mut leg_depths).unwrap_or(0.0);
    let max_scalable_dollars = finite_or(max_scalable_dollars_sum, 0.0);
    let max_scalable_bankroll = if plan.total_notional_dollars > 1e-9 {
        finite_or(
            req.starting_bankroll * (max_scalable_dollars / plan.total_notional_dollars),
            0.0,
        )
    } else {
        0.0
    };
    let s1_fill = portfolio_scenarios.first().map_or(0.0, |s| s.fill_ratio * 100.0);
    let s5_fill = portfolio_scenarios.get(2).map_or(0.0, |s| s.fill_ratio * 100.0);
    let summary = format!(
        "{status}: {} leg(s), median depth {median_book_depth_contracts:.0} cts, 1x fill {s1_fill:.0}% / 5x fill {s5_fill:.0}%, max scalable ${max_scalable_dollars:.2}/window (~${max_scalable_bankroll:.0} bankroll), live_armed=false",
        legs.len()
    );

    LiquidityCapacityReport {
        status,
        live_armed: false,
        observed_depth_windows: ctx.window_depth_contracts.len(),
        median_book_depth_contracts,
        base_contracts: plan.total_contracts,
        max_scalable_contracts: max_scalable_contracts_sum,
        max_scalable_dollars,
        max_scalable_bankroll,
        portfolio_scenarios,
        legs,
        summary,
    }
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
) -> (AllocatorComparison, f64, f64, String, Vec<[f64; 4]>) {
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
    let mut oos_window_returns: Vec<[f64; 4]> = Vec::with_capacity(oos_by_epoch.len());

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
            let ct_f = alloc_epoch_contracts as f64;
            let r0 = finite_or((alloc_epoch_pnl / prev_alloc).clamp(-0.95, 5.0), 0.0);
            let r1 = finite_or(((alloc_epoch_pnl - ct_f * 0.01) / prev_alloc).clamp(-0.95, 5.0), 0.0);
            let r2 = finite_or(((alloc_epoch_pnl - ct_f * 0.02) / prev_alloc).clamp(-0.95, 5.0), 0.0);
            let r3 = finite_or(((alloc_epoch_pnl - ct_f * 0.03) / prev_alloc).clamp(-0.95, 5.0), 0.0);
            oos_window_returns.push([r0, r1, r2, r3]);
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
    oos_window_returns: &[[f64; 4]],
    starting_bankroll: f64,
    max_daily_loss_dollars: f64,
) -> BootstrapStressSummary {
    compute_bootstrap_stress_sim_trials(
        oos_window_returns,
        starting_bankroll,
        max_daily_loss_dollars,
        1_000,
    )
}

fn compute_bootstrap_stress_sim_trials(
    oos_window_returns: &[[f64; 4]],
    starting_bankroll: f64,
    max_daily_loss_dollars: f64,
    trials: usize,
) -> BootstrapStressSummary {
    let trials = trials.max(100);
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
            trials,
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

    let r0: Vec<f64> = oos_window_returns.iter().map(|r| r[0]).collect();
    let r1: Vec<f64> = oos_window_returns.iter().map(|r| r[1]).collect();
    let base = run_bootstrap_scenario(
        &r0,
        starting_bankroll,
        max_daily_loss_dollars,
        trials,
        DAYS as usize,
        WINDOWS_PER_DAY,
        0x4F50_454E_4245_4C4C,
    );
    let stress = run_bootstrap_scenario(
        &r1,
        starting_bankroll,
        max_daily_loss_dollars,
        trials,
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
    let rail_str = if base.daily_rail_breach_prob_pct > 0.0 && base.daily_rail_breach_prob_pct < 0.05 {
        "<0.1".to_string()
    } else {
        format!("{:.1}", base.daily_rail_breach_prob_pct)
    };
    let summary = format!(
        "1,000 causal OOS trials (30d): P50 ${:.2} (${:.2} @ +1c), P95 DD {:.1}% ({:.1}% @ +1c), rail-breach {rail_str}%, P(>=$500/d) {:.1}%",
        base.p50[last],
        stress.p50[last],
        base.p95_max_dd_pct,
        stress.p95_max_dd_pct,
        base.prob_500_day_pct,
    );

    BootstrapStressSummary {
        trials,
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

const CLUSTER_CORRELATION_THRESHOLD: f64 = 0.55;
const CLUSTER_HEAT_CAP_FRACTION: f64 = 0.65;

fn canonical_asset_from_series(series: &str) -> String {
    let up = series.trim().to_ascii_uppercase();
    for asset in ["BTC", "ETH", "SOL", "XRP", "DOGE"] {
        if up.contains(asset) {
            return asset.to_string();
        }
    }
    let trimmed = up.strip_prefix("KX").unwrap_or(&up);
    let alpha: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    if alpha.is_empty() {
        "OTHER".to_string()
    } else {
        alpha
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn apply_diversification_guard(
    entries_by_strat: &[Vec<&TournamentEntry>],
    accepted: &[SplitCandidate],
    raw_weights_pct: &[f64],
    req: &AutoAllocatorRequest,
    is_maker: bool,
    slip_dollars: f64,
    epoch_is_high_vol: &BTreeMap<i64, bool>,
    max_daily_loss_dollars: f64,
) -> (Vec<f64>, DiversificationGuardReport) {
    let cluster_heat_cap_pct = finite_or(
        (req.max_total_heat_pct * CLUSTER_HEAT_CAP_FRACTION).max(req.max_slot_risk_pct),
        req.max_slot_risk_pct,
    );
    let daily_rail_ceiling_pct = finite_or(
        ((max_daily_loss_dollars.max(0.0) / req.starting_bankroll.max(1.0)) * 100.0)
            .clamp(0.5, 100.0),
        100.0,
    );
    let default_regime_ceiling_pct = finite_or(
        daily_rail_ceiling_pct.clamp(0.5, cluster_heat_cap_pct.max(0.5)),
        cluster_heat_cap_pct,
    );
    if accepted.is_empty() || raw_weights_pct.is_empty() {
        return (
            Vec::new(),
            DiversificationGuardReport {
                enabled: req.enable_diversification_guard,
                status: "NO_DATA".to_string(),
                effective_diversification_score: 0.0,
                max_cross_asset_corr: None,
                correlated_cluster_heat_pct: 0.0,
                cluster_heat_cap_pct,
                heat_trimmed_pct: 0.0,
                daily_rail_ceiling_pct,
                regime_drawdown_ceiling_pct: default_regime_ceiling_pct,
                hv_regime_mult: 1.0,
                worst_correlated_cluster_heat_pct: 0.0,
                ceiling_trimmed_pct: 0.0,
                ceiling_status: "NO_DATA".to_string(),
                clusters: Vec::new(),
                diversified_safe_preset_id: None,
                summary: "No active slots available for cross-asset correlation clustering."
                    .to_string(),
            },
        );
    }

    let winner_assets: Vec<String> = accepted
        .iter()
        .map(|cand| {
            canonical_asset_from_series(&dominant_series_for_entries(
                entries_by_strat.get(cand.strategy_idx),
            ))
        })
        .collect();

    let mut asset_epoch_acc: BTreeMap<String, BTreeMap<i64, (f64, usize)>> = BTreeMap::new();
    for cand in accepted {
        if let Some(list) = entries_by_strat.get(cand.strategy_idx) {
            for e in list {
                let asset = canonical_asset_from_series(&e.series);
                let p = e.entry_price.clamp(0.01, 0.99);
                let af = if is_maker { maker_fee(p) } else { fee(p) };
                let payoff = if e.won { 1.0 } else { 0.0 };
                let net_pnl = payoff - p - af - slip_dollars;
                let cost = (p + af + slip_dollars).min(1.0);
                let ret = if cost > 1e-9 { net_pnl / cost } else { 0.0 };
                let entry = asset_epoch_acc
                    .entry(asset)
                    .or_default()
                    .entry(e.close_epoch)
                    .or_insert((0.0, 0));
                entry.0 += finite_or(ret, 0.0);
                entry.1 += 1;
            }
        }
    }

    let mut active_assets: Vec<String> = winner_assets.clone();
    active_assets.sort();
    active_assets.dedup();

    let asset_returns: Vec<BTreeMap<i64, f64>> = active_assets
        .iter()
        .map(|asset| {
            asset_epoch_acc
                .get(asset)
                .map(|m| {
                    m.iter()
                        .map(|(&ep, &(sum, cnt))| (ep, sum / (cnt.max(1) as f64)))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();

    let n_assets = active_assets.len();
    let mut adj = vec![vec![false; n_assets]; n_assets];
    let mut pair_abs_corr: HashMap<(usize, usize), f64> = HashMap::new();
    let mut max_cross_asset_corr: Option<f64> = None;

    for i in 0..n_assets {
        for j in (i + 1)..n_assets {
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for (&ep, &ri) in &asset_returns[i] {
                if let Some(&rj) = asset_returns[j].get(&ep) {
                    xs.push(ri);
                    ys.push(rj);
                }
            }
            if xs.len() >= 4 {
                if let Some(r) = pearson_correlation(&xs, &ys) {
                    let abs_r = r.abs();
                    max_cross_asset_corr = Some(match max_cross_asset_corr {
                        Some(prev) => prev.max(abs_r),
                        None => abs_r,
                    });
                    if abs_r >= CLUSTER_CORRELATION_THRESHOLD {
                        adj[i][j] = true;
                        adj[j][i] = true;
                        pair_abs_corr.insert((i, j), abs_r);
                    }
                }
            }
        }
    }

    let mut guarded_weights: Vec<f64> = raw_weights_pct
        .iter()
        .map(|&w| finite_or(w.max(0.0), 0.0))
        .collect();
    let mut visited = vec![false; n_assets];
    let mut clusters: Vec<AssetClusterRisk> = Vec::new();
    let mut cluster_members: Vec<Vec<usize>> = Vec::new();

    for start in 0..n_assets {
        if visited[start] {
            continue;
        }
        let mut stack = vec![start];
        visited[start] = true;
        let mut comp_idx = Vec::new();
        while let Some(u) = stack.pop() {
            comp_idx.push(u);
            for (v, &connected) in adj[u].iter().enumerate() {
                if connected && !visited[v] {
                    visited[v] = true;
                    stack.push(v);
                }
            }
        }
        comp_idx.sort_unstable();

        let comp_assets: Vec<String> = comp_idx
            .iter()
            .map(|&idx| active_assets[idx].clone())
            .collect();
        let cluster_id = comp_assets.join("+");
        let mut max_intra: Option<f64> = None;
        for a in 0..comp_idx.len() {
            for b in (a + 1)..comp_idx.len() {
                if let Some(&c) = pair_abs_corr.get(&(comp_idx[a], comp_idx[b])) {
                    max_intra = Some(match max_intra {
                        Some(prev) => prev.max(c),
                        None => c,
                    });
                }
            }
        }

        let member_winners: Vec<usize> = winner_assets
            .iter()
            .enumerate()
            .filter_map(|(wi, asset)| comp_assets.contains(asset).then_some(wi))
            .collect();
        let raw_heat_pct: f64 = member_winners.iter().map(|&wi| guarded_weights[wi]).sum();
        let raw_heat_pct = finite_or(raw_heat_pct, 0.0);

        let mut guarded_heat_pct = raw_heat_pct;
        if req.enable_diversification_guard
            && comp_assets.len() >= 2
            && raw_heat_pct > cluster_heat_cap_pct + 1e-9
        {
            let scale = (cluster_heat_cap_pct / raw_heat_pct).clamp(0.0, 1.0);
            for &wi in &member_winners {
                guarded_weights[wi] = finite_or(guarded_weights[wi] * scale, 0.0);
            }
            guarded_heat_pct = finite_or(
                member_winners.iter().map(|&wi| guarded_weights[wi]).sum(),
                cluster_heat_cap_pct,
            );
        }

        let penalty_applied_pct = finite_or((raw_heat_pct - guarded_heat_pct).max(0.0), 0.0);
        cluster_members.push(member_winners);
        clusters.push(AssetClusterRisk {
            cluster_id,
            assets: comp_assets,
            raw_heat_pct,
            guarded_heat_pct,
            max_intra_cluster_corr: max_intra,
            penalty_applied_pct,
        });
    }

    // Winner-level correlated exposure & causal High-Vol regime drawdown ceiling guard.
    // Closes same-asset multi-strategy stacking and tightens cluster exposure under High-Vol drawdown streaks
    // so correlated stacks stay bounded by the configured daily loss rail budget.
    let n_winners = accepted.len();
    let winner_epoch_rets: Vec<BTreeMap<i64, f64>> = accepted
        .iter()
        .map(|cand| {
            let mut ep_acc: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
            if let Some(list) = entries_by_strat.get(cand.strategy_idx) {
                for e in list {
                    let p = e.entry_price.clamp(0.01, 0.99);
                    let af = if is_maker { maker_fee(p) } else { fee(p) };
                    let payoff = if e.won { 1.0 } else { 0.0 };
                    let net_pnl = payoff - p - af - slip_dollars;
                    let cost = (p + af + slip_dollars).min(1.0);
                    let ret = if cost > 1e-9 { net_pnl / cost } else { 0.0 };
                    let slot = ep_acc.entry(e.close_epoch).or_insert((0.0, 0));
                    slot.0 += finite_or(ret, 0.0);
                    slot.1 += 1;
                }
            }
            ep_acc
                .into_iter()
                .map(|(ep, (sum, cnt))| (ep, sum / (cnt.max(1) as f64)))
                .collect()
        })
        .collect();

    let mut w_adj = vec![vec![false; n_winners]; n_winners];
    for i in 0..n_winners {
        for j in (i + 1)..n_winners {
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for (&ep, &ri) in &winner_epoch_rets[i] {
                if let Some(&rj) = winner_epoch_rets[j].get(&ep) {
                    xs.push(ri);
                    ys.push(rj);
                }
            }
            if xs.len() >= 4 {
                if let Some(r) = pearson_correlation(&xs, &ys) {
                    let same_asset_comove = winner_assets[i] == winner_assets[j] && r >= 0.35;
                    if r.abs() >= CLUSTER_CORRELATION_THRESHOLD || same_asset_comove {
                        w_adj[i][j] = true;
                        w_adj[j][i] = true;
                    }
                }
            }
        }
    }

    let mut w_visited = vec![false; n_winners];
    let mut hv_regime_mult = 1.0f64;
    let mut regime_drawdown_ceiling_pct = default_regime_ceiling_pct;
    let mut worst_correlated_cluster_heat_pct = 0.0f64;
    let mut ceiling_trimmed_pct = 0.0f64;
    let mut has_unprotected_ceiling_breach = false;
    let mut saw_multi_winner_cluster = false;

    for start in 0..n_winners {
        if w_visited[start] {
            continue;
        }
        let mut stack = vec![start];
        w_visited[start] = true;
        let mut w_comp = Vec::new();
        while let Some(u) = stack.pop() {
            w_comp.push(u);
            for (v, &connected) in w_adj[u].iter().enumerate() {
                if connected && !w_visited[v] {
                    w_visited[v] = true;
                    stack.push(v);
                }
            }
        }
        if w_comp.len() < 2 {
            continue;
        }
        saw_multi_winner_cluster = true;

        // Measure causal High-Vol loss streak & cumulative drawdown severity strictly from this split's epochs.
        let mut comp_ep_acc: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
        for &wi in &w_comp {
            for (&ep, &ret) in &winner_epoch_rets[wi] {
                let slot = comp_ep_acc.entry(ep).or_insert((0.0, 0));
                slot.0 += ret;
                slot.1 += 1;
            }
        }
        let mut hv_streak = 0usize;
        let mut max_hv_streak = 0usize;
        let mut cur_hv_dd = 0.0f64;
        let mut max_hv_dd = 0.0f64;
        let mut hv_neg_count = 0usize;
        let mut hv_total = 0usize;
        for (ep, (sum_r, cnt)) in comp_ep_acc {
            if !epoch_is_high_vol.get(&ep).copied().unwrap_or(false) {
                hv_streak = 0;
                cur_hv_dd = 0.0;
                continue;
            }
            hv_total += 1;
            let mean_r = sum_r / (cnt.max(1) as f64);
            if mean_r < -1e-6 {
                hv_neg_count += 1;
                hv_streak += 1;
                cur_hv_dd += (-mean_r).clamp(0.0, 1.0);
                if hv_streak > max_hv_streak {
                    max_hv_streak = hv_streak;
                }
                if cur_hv_dd > max_hv_dd {
                    max_hv_dd = cur_hv_dd;
                }
            } else {
                hv_streak = 0;
                cur_hv_dd = 0.0;
            }
        }
        let comp_hv_mult = if max_hv_streak >= 2 {
            (1.0 + 0.35 * ((max_hv_streak - 1).min(4) as f64))
                .max(max_hv_dd.min(2.5))
                .clamp(1.0, 2.5)
        } else if hv_total >= 4 && (hv_neg_count as f64 / (hv_total as f64)) > 0.45 {
            1.25
        } else {
            1.0
        };
        hv_regime_mult = hv_regime_mult.max(comp_hv_mult);

        let comp_ceiling_pct = finite_or(
            (daily_rail_ceiling_pct / comp_hv_mult).clamp(0.5, cluster_heat_cap_pct.max(0.5)),
            cluster_heat_cap_pct,
        );
        regime_drawdown_ceiling_pct = regime_drawdown_ceiling_pct.min(comp_ceiling_pct);

        let pre_heat: f64 = w_comp.iter().map(|&wi| guarded_weights[wi]).sum();
        let mut post_heat = pre_heat;
        if pre_heat > comp_ceiling_pct + 1e-9 {
            if req.enable_diversification_guard {
                let scale = (comp_ceiling_pct / pre_heat).clamp(0.0, 1.0);
                for &wi in &w_comp {
                    guarded_weights[wi] = finite_or(guarded_weights[wi] * scale, 0.0);
                }
                post_heat = finite_or(
                    w_comp.iter().map(|&wi| guarded_weights[wi]).sum(),
                    comp_ceiling_pct,
                );
                ceiling_trimmed_pct += (pre_heat - post_heat).max(0.0);
            } else {
                has_unprotected_ceiling_breach = true;
            }
        }
        if post_heat > worst_correlated_cluster_heat_pct {
            worst_correlated_cluster_heat_pct = post_heat;
        }
    }

    if !saw_multi_winner_cluster {
        regime_drawdown_ceiling_pct = default_regime_ceiling_pct;
    }

    // Re-sync asset cluster telemetry with any regime drawdown ceiling trims applied above.
    for (ci, members) in cluster_members.iter().enumerate() {
        let updated_heat = finite_or(members.iter().map(|&wi| guarded_weights[wi]).sum(), 0.0);
        clusters[ci].guarded_heat_pct = updated_heat;
        clusters[ci].penalty_applied_pct =
            finite_or((clusters[ci].raw_heat_pct - updated_heat).max(0.0), 0.0);
    }

    clusters.sort_by(|a, b| {
        b.guarded_heat_pct
            .total_cmp(&a.guarded_heat_pct)
            .then_with(|| a.cluster_id.cmp(&b.cluster_id))
    });

    let total_guarded: f64 = guarded_weights.iter().sum();
    let raw_corr_heat = finite_or(
        clusters
            .iter()
            .filter(|c| c.assets.len() >= 2)
            .map(|c| c.guarded_heat_pct)
            .sum(),
        0.0,
    );
    let correlated_cluster_heat_pct = if raw_corr_heat <= 1e-12 { 0.0 } else { raw_corr_heat };
    let raw_trimmed = finite_or(clusters.iter().map(|c| c.penalty_applied_pct).sum(), 0.0);
    let heat_trimmed_pct = if raw_trimmed <= 1e-12 { 0.0 } else { raw_trimmed };
    let ceiling_trimmed_pct = if ceiling_trimmed_pct <= 1e-12 {
        0.0
    } else {
        finite_or(ceiling_trimmed_pct, 0.0)
    };
    let worst_correlated_cluster_heat_pct = finite_or(worst_correlated_cluster_heat_pct, 0.0);

    let (status, effective_diversification_score) = if total_guarded <= 1e-9 {
        ("NO_DATA".to_string(), 0.0)
    } else {
        let hhi: f64 = clusters
            .iter()
            .map(|c| {
                let w = (c.guarded_heat_pct / total_guarded).max(0.0);
                w * w
            })
            .sum();
        let n_eff = (1.0 / hhi.max(1e-9)).max(1.0);
        let base_div = ((n_eff - 1.0) / 3.0).clamp(0.0, 1.0) * 65.0 + 35.0;
        let corr_factor = 1.0 - 0.30 * max_cross_asset_corr.unwrap_or(0.0).clamp(0.0, 1.0);
        let unprotected_excess_penalty = if !req.enable_diversification_guard {
            let asset_pen: f64 = clusters
                .iter()
                .filter(|c| c.assets.len() >= 2 && c.raw_heat_pct > cluster_heat_cap_pct)
                .map(|c| {
                    ((c.raw_heat_pct - cluster_heat_cap_pct) / cluster_heat_cap_pct.max(1.0) * 30.0)
                        .min(30.0)
                })
                .sum();
            let ceiling_pen = if has_unprotected_ceiling_breach
                && worst_correlated_cluster_heat_pct > regime_drawdown_ceiling_pct
            {
                ((worst_correlated_cluster_heat_pct - regime_drawdown_ceiling_pct)
                    / regime_drawdown_ceiling_pct.max(1.0)
                    * 25.0)
                    .min(25.0)
            } else {
                0.0
            };
            asset_pen + ceiling_pen
        } else {
            0.0
        };
        let score = finite_or(
            (base_div * corr_factor - unprotected_excess_penalty).clamp(0.0, 100.0),
            0.0,
        );
        let st = if heat_trimmed_pct > 1e-6 {
            "CLUSTER_CAPPED"
        } else {
            "DIVERSIFIED"
        };
        (st.to_string(), score)
    };

    let ceiling_status = if total_guarded <= 1e-9 {
        "NO_DATA"
    } else if !req.enable_diversification_guard && has_unprotected_ceiling_breach {
        "UNPROTECTED_BREACH"
    } else if ceiling_trimmed_pct > 1e-6 {
        "CEILING_CLAMPED"
    } else {
        "WITHIN_CEILING"
    }
    .to_string();

    let corr_str = max_cross_asset_corr
        .map(|r| format!("{r:.2}"))
        .unwrap_or_else(|| "n/a".to_string());
    let summary = if ceiling_status == "CEILING_CLAMPED" {
        format!(
            "Regime ceiling ({regime_drawdown_ceiling_pct:.1}% under {hv_regime_mult:.2}x HV mult) trimmed {ceiling_trimmed_pct:.1}% correlated heat -> worst cluster {worst_correlated_cluster_heat_pct:.1}%"
        )
    } else if ceiling_status == "UNPROTECTED_BREACH" {
        format!(
            "Guard OFF: correlated cluster {worst_correlated_cluster_heat_pct:.1}% exceeds {regime_drawdown_ceiling_pct:.1}% regime DD ceiling ({hv_regime_mult:.2}x HV)"
        )
    } else if status == "CLUSTER_CAPPED" {
        format!(
            "Cluster cap ({cluster_heat_cap_pct:.1}%) trimmed {heat_trimmed_pct:.1}% correlated heat (max |r| {corr_str}) -> div score {effective_diversification_score:.0}/100"
        )
    } else if !req.enable_diversification_guard
        && clusters
            .iter()
            .any(|c| c.assets.len() >= 2 && c.raw_heat_pct > cluster_heat_cap_pct)
    {
        format!(
            "Guard OFF: correlated cluster exceeds {cluster_heat_cap_pct:.1}% cap (max |r| {corr_str}) -> div score {effective_diversification_score:.0}/100"
        )
    } else {
        format!(
            "{} asset cluster(s) · max |r| {corr_str} · ceiling {worst_correlated_cluster_heat_pct:.1}% / {regime_drawdown_ceiling_pct:.1}% -> div score {effective_diversification_score:.0}/100",
            clusters.len()
        )
    };

    (
        guarded_weights,
        DiversificationGuardReport {
            enabled: req.enable_diversification_guard,
            status,
            effective_diversification_score,
            max_cross_asset_corr,
            correlated_cluster_heat_pct,
            cluster_heat_cap_pct,
            heat_trimmed_pct,
            daily_rail_ceiling_pct,
            regime_drawdown_ceiling_pct,
            hv_regime_mult,
            worst_correlated_cluster_heat_pct,
            ceiling_trimmed_pct,
            ceiling_status,
            clusters,
            diversified_safe_preset_id: None,
            summary,
        },
    )
}

/// Compute causal adverse-fill slippage ladder (`+0c`, `+1c`, `+2c`, `+3c`) and exact
/// break-even slippage ceiling (`c`) respecting the `0.0%` daily loss rail gate.
#[allow(clippy::too_many_arguments)]
fn compute_slippage_ladder_and_cushion(
    weighted_ev_cents: Option<f64>,
    winner_legs: &[(f64, f64, usize, f64)],
    windows_settled_oos: usize,
    req: &AutoAllocatorRequest,
    max_daily_loss_dollars: f64,
    bootstrap_stress: &BootstrapStressSummary,
    oos_window_returns: &[[f64; 4]],
    is_maker: bool,
    trials: usize,
) -> (Vec<SlippageLadderStep>, Option<f64>, String) {
    let oos_windows = windows_settled_oos.max(1) as f64;
    let fill_rate = if is_maker { req.maker_fill_rate } else { 1.0 };
    let mut base_daily_per_dollar = 0.0f64;
    let mut daily_contracts_per_dollar = 0.0f64;
    for &(alloc_pct, avg_entry_price, oos_trades, oos_ev_cents) in winner_legs {
        let alloc_frac = alloc_pct.max(0.0) / 100.0;
        if alloc_frac <= 0.0 {
            continue;
        }
        let p = avg_entry_price.clamp(0.01, 0.99);
        let fire_rate = (oos_trades as f64 / oos_windows).min(1.0);
        let ct_rate = (alloc_frac / p) * fire_rate * fill_rate;
        daily_contracts_per_dollar += ct_rate;
        base_daily_per_dollar += ct_rate * (oos_ev_cents / 100.0);
    }

    let trials = trials.max(100);
    let mut ladder = Vec::with_capacity(4);
    for (k, extra_c) in [0.0f64, 1.0, 2.0, 3.0].into_iter().enumerate() {
        let net_ev = weighted_ev_cents.and_then(|ev| finite_opt(ev - extra_c));
        let daily_dollars = if winner_legs.is_empty() {
            0.0
        } else {
            finite_or(
                req.starting_bankroll
                    * WINDOWS_PER_DAY_15M
                    * (base_daily_per_dollar - (extra_c / 100.0) * daily_contracts_per_dollar),
                0.0,
            )
        };
        let oos_net_pnl = if oos_window_returns.is_empty() || winner_legs.is_empty() {
            0.0
        } else {
            let mut eq = req.starting_bankroll;
            for row in oos_window_returns {
                eq = (eq * (1.0 + row[k])).max(0.0);
            }
            finite_or(eq - req.starting_bankroll, 0.0)
        };
        let breach_prob = match k {
            0 => bootstrap_stress.daily_rail_breach_prob_pct,
            1 => bootstrap_stress.stress_1c_daily_rail_breach_prob_pct,
            _ if oos_window_returns.is_empty() || winner_legs.is_empty() => 0.0,
            _ => {
                let rk: Vec<f64> = oos_window_returns.iter().map(|r| r[k]).collect();
                run_bootstrap_scenario(
                    &rk,
                    req.starting_bankroll,
                    max_daily_loss_dollars,
                    trials,
                    30,
                    96,
                    0x4F50_454E_4245_4C4C,
                )
                .daily_rail_breach_prob_pct
            }
        };
        let positive_ev = net_ev.is_some_and(|ev| ev > 0.0);
        let rail_safe = breach_prob <= 1e-9;
        let survival_ok = !winner_legs.is_empty() && positive_ev && rail_safe;
        ladder.push(SlippageLadderStep {
            extra_slippage_cents: extra_c,
            net_expectancy_cents: net_ev,
            projected_daily_net_dollars: daily_dollars,
            oos_net_pnl_dollars: oos_net_pnl,
            daily_rail_breach_prob_pct: breach_prob,
            positive_ev,
            rail_safe,
            survival_ok,
        });
    }

    let breakeven_cents = if winner_legs.is_empty() {
        None
    } else {
        weighted_ev_cents.and_then(|ev| finite_opt(ev.max(0.0)))
    };
    let status = match breakeven_cents {
        None => "NO_DATA",
        Some(c) if c < 1.0 || !ladder[0].rail_safe || !ladder[1].rail_safe => "THIN",
        Some(c) if c >= 2.0 && ladder[2].survival_ok => "SAFE",
        Some(_) => "MODERATE",
    }
    .to_string();

    (ladder, breakeven_cents, status)
}

#[allow(clippy::too_many_arguments)]
fn compute_session_briefing(
    req: &AutoAllocatorRequest,
    rails: &RiskRailsConfig,
    winners: &[AllocatedWinner],
    drift_gauge: &DriftGauge,
    forward_envelope: &ForwardEnvelopeMonitor,
    diversification: &DiversificationGuardReport,
    bootstrap_stress: &BootstrapStressSummary,
    comparison: &AllocatorComparison,
    daily_target: &DailyTargetVerdict,
    regime_stability: &RegimeStabilityReport,
    breakeven_slippage_cents: Option<f64>,
    slippage_cushion_status: &str,
    slippage_ladder: &[SlippageLadderStep],
    oos_window_returns: &[[f64; 4]],
    live_armed: bool,
) -> SessionBriefingReport {
    let expected_fill_edge_cents = forward_envelope
        .predicted_ev_cents
        .or(drift_gauge.paper_expectancy_cents);
    let realized_fill_edge_cents = forward_envelope
        .observed_forward_ev_cents
        .or(drift_gauge.shadow_expectancy_cents);
    let fill_edge_delta_cents = expected_fill_edge_cents
        .zip(realized_fill_edge_cents)
        .and_then(|(e, r)| finite_opt(r - e));

    let w_sum: f64 = winners.iter().map(|w| w.allocated_pct.max(0.01)).sum();
    let fee_drag_cents = if winners.is_empty() || w_sum <= 1e-12 {
        0.0
    } else {
        finite_or(
            winners
                .iter()
                .map(|w| w.fee_drag_cents * (w.allocated_pct.max(0.01) / w_sum))
                .sum::<f64>(),
            0.0,
        )
    };
    let maker_fee_savings_dollars = finite_or(comparison.fee_savings_dollars.max(0.0), 0.0);
    let correlation_cluster_penalty_pct = finite_or(diversification.heat_trimmed_pct.max(0.0), 0.0);
    let daily_loss_rail_dollars = finite_or(rails.max_daily_loss_dollars.max(0.0), 0.0);

    // Worst historical 1-day (<=96 windows) stressed (+1c) OOS drawdown in dollars.
    let mut worst_day_ret = 0.0f64;
    if !oos_window_returns.is_empty() {
        let day_len = oos_window_returns.len().min(96);
        let mut acc = 0.0f64;
        for (i, row) in oos_window_returns.iter().enumerate() {
            acc += row[1];
            if i >= day_len {
                acc -= oos_window_returns[i - day_len][1];
            }
            if acc < worst_day_ret {
                worst_day_ret = acc;
            }
        }
    }
    let projected_worst_day_loss_dollars =
        finite_or((-worst_day_ret).max(0.0) * req.starting_bankroll, 0.0);
    let daily_loss_headroom_dollars = finite_or(
        daily_loss_rail_dollars - projected_worst_day_loss_dollars,
        0.0,
    );
    let daily_loss_headroom_pct = if daily_loss_rail_dollars > 1e-9 {
        finite_or(
            ((daily_loss_headroom_dollars / daily_loss_rail_dollars) * 100.0).clamp(-100.0, 100.0),
            0.0,
        )
    } else {
        0.0
    };

    let status = if winners.is_empty() {
        "NO_DATA"
    } else if daily_loss_headroom_dollars > 0.0
        && bootstrap_stress.daily_rail_breach_prob_pct <= 1e-9
        && drift_gauge.status == "HEALTHY"
        && daily_target.projected_daily_net_dollars > 0.0
    {
        "PASS_READY"
    } else {
        "HOLD_PAPER"
    };

    let exp_str = expected_fill_edge_cents
        .map(|v| format!("{v:+.2}c"))
        .unwrap_or_else(|| "n/a".to_string());
    let real_str = realized_fill_edge_cents
        .map(|v| format!("{v:+.2}c"))
        .unwrap_or_else(|| "n/a".to_string());
    let cushion_str = breakeven_slippage_cents
        .map(|v| format!("{v:.2}c"))
        .unwrap_or_else(|| "n/a".to_string());
    let summary = format!(
        "{status}: expected {exp_str} vs realized {real_str} · fee drag {fee_drag_cents:.2}c/ct · cluster trim {correlation_cluster_penalty_pct:.1}% · ceiling {} ({:.1}%/{:.1}%) · daily rail headroom ${daily_loss_headroom_dollars:.2} ({daily_loss_headroom_pct:.0}%) · regime {} · slip cushion {cushion_str} ({slippage_cushion_status})",
        diversification.ceiling_status,
        diversification.worst_correlated_cluster_heat_pct,
        diversification.regime_drawdown_ceiling_pct,
        regime_stability.status
    );

    SessionBriefingReport {
        status: status.to_string(),
        live_armed,
        expected_fill_edge_cents,
        realized_fill_edge_cents,
        fill_edge_delta_cents,
        fee_drag_cents,
        maker_fee_savings_dollars,
        correlation_cluster_penalty_pct,
        regime_drawdown_ceiling_pct: diversification.regime_drawdown_ceiling_pct,
        worst_correlated_cluster_heat_pct: diversification.worst_correlated_cluster_heat_pct,
        ceiling_status: diversification.ceiling_status.clone(),
        daily_loss_rail_dollars,
        projected_worst_day_loss_dollars,
        daily_loss_headroom_dollars,
        daily_loss_headroom_pct,
        active_winners_count: winners.len(),
        projected_daily_net_dollars: daily_target.projected_daily_net_dollars,
        regime_stability: regime_stability.status.clone(),
        calm_expectancy_cents: regime_stability.active_calm_ev_cents,
        high_vol_expectancy_cents: regime_stability.active_high_vol_ev_cents,
        breakeven_slippage_cents,
        slippage_cushion_status: slippage_cushion_status.to_string(),
        slippage_ladder: slippage_ladder.to_vec(),
        summary,
    }
}

/// Strictly causal chronological partitioning into Calm (`false`) vs High-Vol / Wide-Spread (`true`)
/// epochs using backward-looking trailing 60th percentile (`< close_epoch`) — zero lookahead.
fn classify_causal_epoch_regimes(ctx: &TournamentTapeContext) -> (BTreeMap<i64, bool>, usize, usize) {
    let mut epoch_scores: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    for e in &ctx.entries {
        let score = ctx
            .window_vol_scores
            .get(&e.ticker)
            .copied()
            .unwrap_or_else(|| 2.0 + 18.0 * (0.5 - (e.entry_price - 0.5).abs()).max(0.0));
        let slot = epoch_scores.entry(e.close_epoch).or_insert((0.0, 0));
        slot.0 += score;
        slot.1 += 1;
    }

    let mut epoch_is_high_vol = BTreeMap::new();
    let mut prior_scores: Vec<f64> = Vec::with_capacity(64);
    let mut sorted_scratch: Vec<f64> = Vec::with_capacity(64);
    let mut calm_windows = 0usize;
    let mut high_vol_windows = 0usize;

    for (epoch, (sum_score, cnt)) in epoch_scores {
        let cur = sum_score / (cnt.max(1) as f64);
        let is_high_vol = if prior_scores.len() >= 4 {
            sorted_scratch.clear();
            let start = prior_scores.len().saturating_sub(48);
            sorted_scratch.extend_from_slice(&prior_scores[start..]);
            sorted_scratch.sort_by(|a, b| a.total_cmp(b));
            let idx = (((sorted_scratch.len() - 1) as f64) * 0.60).round() as usize;
            let tau = sorted_scratch[idx.min(sorted_scratch.len() - 1)];
            cur > tau + 1e-9
        } else {
            cur >= 8.0
        };
        if is_high_vol {
            high_vol_windows += 1;
        } else {
            calm_windows += 1;
        }
        epoch_is_high_vol.insert(epoch, is_high_vol);
        prior_scores.push(cur);
    }

    (epoch_is_high_vol, calm_windows, high_vol_windows)
}

/// Evaluate weighted OOS net expectancy (`+0c` & `+1c` adverse fill) separately across
/// causal Calm vs High-Vol / Wide-Spread epochs for an active strategy allocation.
fn evaluate_regime_stability(
    oos_by_strat: &[Vec<&TournamentEntry>],
    accepted: &[SplitCandidate],
    weights_pct: &[f64],
    epoch_is_high_vol: &BTreeMap<i64, bool>,
    is_maker: bool,
    slip_dollars: f64,
) -> (Option<f64>, Option<f64>, bool, bool, String) {
    if accepted.is_empty() {
        return (None, None, false, false, "NO_DATA".to_string());
    }
    let w_sum: f64 = weights_pct.iter().map(|w| w.max(0.0)).sum();
    let use_equal = w_sum <= 1e-12;
    let eq_w = 1.0 / (accepted.len() as f64);

    let mut calm_base_num = 0.0f64;
    let mut calm_1c_num = 0.0f64;
    let mut calm_den = 0.0f64;
    let mut calm_trades = 0usize;

    let mut hv_base_num = 0.0f64;
    let mut hv_1c_num = 0.0f64;
    let mut hv_den = 0.0f64;
    let mut hv_trades = 0usize;

    for (i, cand) in accepted.iter().enumerate() {
        let raw_w = weights_pct.get(i).copied().unwrap_or(0.0);
        let w = if use_equal {
            eq_w
        } else {
            raw_w.max(0.0) / w_sum
        };
        if w <= 1e-12 {
            continue;
        }
        let Some(entries) = oos_by_strat.get(cand.strategy_idx) else {
            continue;
        };
        for e in entries {
            let p = e.entry_price.clamp(0.01, 0.99);
            let af = if is_maker { maker_fee(p) } else { fee(p) };
            let payoff = if e.won { 1.0 } else { 0.0 };
            let net_base = payoff - p - af - slip_dollars;
            let net_1c = net_base - 0.01;
            let is_hv = epoch_is_high_vol.get(&e.close_epoch).copied().unwrap_or(false);
            if is_hv {
                hv_base_num += w * (net_base * 100.0);
                hv_1c_num += w * (net_1c * 100.0);
                hv_den += w;
                hv_trades += 1;
            } else {
                calm_base_num += w * (net_base * 100.0);
                calm_1c_num += w * (net_1c * 100.0);
                calm_den += w;
                calm_trades += 1;
            }
        }
    }

    let calm_ev = if calm_trades > 0 && calm_den > 1e-12 {
        finite_opt(calm_base_num / calm_den)
    } else {
        None
    };
    let hv_ev = if hv_trades > 0 && hv_den > 1e-12 {
        finite_opt(hv_base_num / hv_den)
    } else {
        None
    };
    let calm_1c_ok = calm_trades > 0 && calm_den > 1e-12 && (calm_1c_num / calm_den) > 0.0;
    let hv_1c_ok = hv_trades > 0 && hv_den > 1e-12 && (hv_1c_num / hv_den) > 0.0;

    let label = if calm_trades == 0 && hv_trades == 0 {
        "NO_DATA"
    } else if calm_ev.is_some_and(|v| v > 0.0)
        && calm_1c_ok
        && hv_ev.is_some_and(|v| v > 0.0)
        && hv_1c_ok
    {
        "ALL-REGIME"
    } else if calm_ev.is_some_and(|v| v > 0.0)
        && calm_1c_ok
        && (hv_trades == 0 || hv_ev.is_some_and(|v| v <= 0.0))
    {
        "CALM-ONLY"
    } else {
        "VOL-SENSITIVE"
    };

    (calm_ev, hv_ev, calm_1c_ok, hv_1c_ok, label.to_string())
}

#[allow(clippy::too_many_arguments)]
fn build_causal_replay_weights(
    is_sel: Option<&SelectionResult>,
    oos_sel: &SelectionResult,
    guarded_oos_weights_pct: &[f64],
    is_by_strat: &[Vec<&TournamentEntry>],
    req: &AutoAllocatorRequest,
    is_maker: bool,
    slip_dollars: f64,
    epoch_is_high_vol: &BTreeMap<i64, bool>,
    max_daily_loss_dollars: f64,
) -> (HashMap<usize, f64>, String) {
    match is_sel {
        Some(sel) => {
            let (guarded_is_weights, _) = apply_diversification_guard(
                is_by_strat,
                &sel.accepted,
                &sel.weights_pct,
                req,
                is_maker,
                slip_dollars,
                epoch_is_high_vol,
                max_daily_loss_dollars,
            );
            let mut m = HashMap::with_capacity(sel.accepted.len());
            for (i, c) in sel.accepted.iter().enumerate() {
                m.insert(c.strategy_idx, finite_or(guarded_is_weights[i], 0.0) / 100.0);
            }
            (m, "causal_is_to_oos".to_string())
        }
        None => {
            let mut m = HashMap::with_capacity(oos_sel.accepted.len());
            for (i, c) in oos_sel.accepted.iter().enumerate() {
                m.insert(
                    c.strategy_idx,
                    finite_or(guarded_oos_weights_pct[i], 0.0) / 100.0,
                );
            }
            (m, "oos_only_no_is_history".to_string())
        }
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn compute_preset_leaderboard(
    ctx: &TournamentTapeContext,
    base_req: &AutoAllocatorRequest,
    rails: &RiskRailsConfig,
    is_by_strat: &[Vec<&TournamentEntry>],
    oos_by_strat: &[Vec<&TournamentEntry>],
    oos_by_epoch: &BTreeMap<i64, Vec<&TournamentEntry>>,
    epoch_is_high_vol: &BTreeMap<i64, bool>,
    is_total_entries: usize,
    audit: Option<&crate::execution_audit::ExecutionHealthReport>,
) -> (Vec<StrategyPresetCandidate>, Option<String>) {
    struct PresetSpec {
        id: &'static str,
        label: &'static str,
        subtitle: &'static str,
        exec_mode: &'static str,
        sizing_mode: &'static str,
        fill_rate: f64,
        min_oos_floor: usize,
        slot_risk: f64,
        total_heat: f64,
        min_fill_pct: f64,
        enable_diversification_guard: bool,
    }

    const PRESETS: &[PresetSpec] = &[
        PresetSpec {
            id: "conservative_maker_qk",
            label: "Conservative Maker QK",
            subtitle: "2% slot / 10% heat · 70% maker fill · Quarter-Kelly",
            exec_mode: "maker",
            sizing_mode: "quarter_kelly",
            fill_rate: 0.70,
            min_oos_floor: 35,
            slot_risk: 2.0,
            total_heat: 10.0,
            min_fill_pct: 60.0,
            enable_diversification_guard: true,
        },
        PresetSpec {
            id: "diversified_safe_basket",
            label: "Diversified Safe Basket",
            subtitle: "8% slot / 30% heat · cross-asset cluster guard · 75% maker QK",
            exec_mode: "maker",
            sizing_mode: "quarter_kelly",
            fill_rate: 0.75,
            min_oos_floor: 25,
            slot_risk: 8.0,
            total_heat: 30.0,
            min_fill_pct: 50.0,
            enable_diversification_guard: true,
        },
        PresetSpec {
            id: "balanced_maker_qk",
            label: "Balanced Maker QK",
            subtitle: "10% slot / 35% heat · 75% maker fill · Quarter-Kelly",
            exec_mode: "maker",
            sizing_mode: "quarter_kelly",
            fill_rate: 0.75,
            min_oos_floor: 30,
            slot_risk: 10.0,
            total_heat: 35.0,
            min_fill_pct: 50.0,
            enable_diversification_guard: true,
        },
        PresetSpec {
            id: "half_kelly_guarded",
            label: "Half-Kelly Guarded",
            subtitle: "20% slot / 70% heat · 75% maker fill · Half-Kelly",
            exec_mode: "maker",
            sizing_mode: "half_kelly",
            fill_rate: 0.75,
            min_oos_floor: 30,
            slot_risk: 20.0,
            total_heat: 70.0,
            min_fill_pct: 50.0,
            enable_diversification_guard: true,
        },
        PresetSpec {
            id: "equal_winners_guarded",
            label: "Equal-Winners Guarded",
            subtitle: "15% slot / 50% heat · 75% maker fill · Equal Weight",
            exec_mode: "maker",
            sizing_mode: "equal_winners",
            fill_rate: 0.75,
            min_oos_floor: 25,
            slot_risk: 15.0,
            total_heat: 50.0,
            min_fill_pct: 40.0,
            enable_diversification_guard: true,
        },
        PresetSpec {
            id: "taker_stress_hardened",
            label: "Taker Stress-Hardened",
            subtitle: "12% slot / 45% heat · 100% immediate fill · Quarter-Kelly",
            exec_mode: "taker",
            sizing_mode: "quarter_kelly",
            fill_rate: 1.0,
            min_oos_floor: 30,
            slot_risk: 12.0,
            total_heat: 45.0,
            min_fill_pct: 25.0,
            enable_diversification_guard: true,
        },
    ];

    let small_fixture = ctx.windows_settled_oos < 60;
    let bankroll = base_req.starting_bankroll.max(1.0);
    let mut rows: Vec<StrategyPresetCandidate> = Vec::with_capacity(PRESETS.len());

    for spec in PRESETS {
        let min_oos = if small_fixture {
            spec.min_oos_floor.min(base_req.min_oos_trades.max(5))
        } else {
            spec.min_oos_floor
        };
        let cand_req = AutoAllocatorRequest {
            execution_mode: spec.exec_mode.to_string(),
            sizing_mode: spec.sizing_mode.to_string(),
            maker_fill_rate: spec.fill_rate,
            min_oos_trades: min_oos,
            max_slot_risk_pct: spec.slot_risk,
            max_total_heat_pct: spec.total_heat,
            starting_bankroll: base_req.starting_bankroll,
            slippage_cents: base_req.slippage_cents,
            require_stress_resilience: true,
            enable_regime_throttle: true,
            enable_diversification_guard: spec.enable_diversification_guard,
            min_fill_rate_pct: spec.min_fill_pct,
        }
        .sanitized();

        let is_maker = cand_req.execution_mode == "maker";
        let slip_dollars = if is_maker {
            0.0
        } else {
            cand_req.slippage_cents / 100.0
        };
        let oos_sel = evaluate_split_candidates(
            ctx,
            oos_by_strat,
            &cand_req,
            cand_req.min_oos_trades,
            is_maker,
            slip_dollars,
            audit,
        );
        let is_sel = if is_total_entries > 0 {
            Some(evaluate_split_candidates(
                ctx,
                is_by_strat,
                &cand_req,
                cand_req.min_oos_trades,
                is_maker,
                slip_dollars,
                None,
            ))
        } else {
            None
        };

        let (guarded_oos_weights, _) = apply_diversification_guard(
            oos_by_strat,
            &oos_sel.accepted,
            &oos_sel.weights_pct,
            &cand_req,
            is_maker,
            slip_dollars,
            epoch_is_high_vol,
            rails.max_daily_loss_dollars,
        );
        let (replay_weights, selection_basis) = build_causal_replay_weights(
            is_sel.as_ref(),
            &oos_sel,
            &guarded_oos_weights,
            is_by_strat,
            &cand_req,
            is_maker,
            slip_dollars,
            epoch_is_high_vol,
            rails.max_daily_loss_dollars,
        );

        let (comparison, _, _, _, oos_returns) = replay_oos_comparison(
            ctx,
            &cand_req,
            oos_by_epoch,
            &replay_weights,
            is_maker,
            selection_basis,
        );
        let boot = compute_bootstrap_stress_sim_trials(
            &oos_returns,
            cand_req.starting_bankroll,
            rails.max_daily_loss_dollars,
            500,
        );

        let w_sum: f64 = guarded_oos_weights.iter().sum();
        let net_expectancy_cents = if oos_sel.accepted.is_empty() {
            None
        } else if w_sum > 1e-12 {
            finite_opt(
                oos_sel
                    .accepted
                    .iter()
                    .zip(guarded_oos_weights.iter())
                    .map(|(c, w)| c.net_expectancy_cents * (*w / w_sum))
                    .sum::<f64>(),
            )
        } else {
            finite_opt(
                oos_sel
                    .accepted
                    .iter()
                    .map(|c| c.net_expectancy_cents)
                    .sum::<f64>()
                    / (oos_sel.accepted.len() as f64),
            )
        };

        let active_winners_count = oos_sel.accepted.len();
        let safe_gate_passed = active_winners_count > 0
            && net_expectancy_cents.is_some_and(|ev| ev > 0.0)
            && boot.daily_rail_breach_prob_pct <= 1e-9
            && boot.stress_1c_daily_rail_breach_prob_pct <= 1e-9;

        let oos_ret_pct = (comparison.auto_allocator_net_pnl / bankroll) * 100.0;
        let base_ret_pct = ((boot.p50_end_equity - bankroll) / bankroll) * 100.0;
        let stress_ret_pct = ((boot.stress_1c_p50_end_equity - bankroll) / bankroll) * 100.0;
        let raw_score = oos_ret_pct
            + 0.50 * stress_ret_pct
            + 0.25 * base_ret_pct
            - 1.25 * boot.stress_1c_p95_max_drawdown_pct;
        let risk_adjusted_score = if safe_gate_passed {
            finite_or(raw_score, 0.0)
        } else {
            finite_or(
                raw_score.min(0.0)
                    - (boot.daily_rail_breach_prob_pct + boot.stress_1c_daily_rail_breach_prob_pct)
                        * 10.0
                    - if active_winners_count == 0 { 100.0 } else { 0.0 },
                -100.0,
            )
        };

        let (
            calm_expectancy_cents,
            high_vol_expectancy_cents,
            calm_stress_1c_survival,
            high_vol_stress_1c_survival,
            regime_stability,
        ) = evaluate_regime_stability(
            oos_by_strat,
            &oos_sel.accepted,
            &guarded_oos_weights,
            epoch_is_high_vol,
            is_maker,
            slip_dollars,
        );
        let preset_legs: Vec<(f64, f64, usize, f64)> = oos_sel
            .accepted
            .iter()
            .zip(guarded_oos_weights.iter())
            .map(|(c, &w)| (w, c.avg_entry_price, c.trades, c.net_expectancy_cents))
            .collect();
        let (slippage_ladder, breakeven_slippage_cents, slippage_cushion_status) =
            compute_slippage_ladder_and_cushion(
                net_expectancy_cents,
                &preset_legs,
                ctx.windows_settled_oos,
                &cand_req,
                rails.max_daily_loss_dollars,
                &boot,
                &oos_returns,
                is_maker,
                500,
            );

        let note = if active_winners_count == 0 {
            format!("0 qualifying winners at min_oos >= {}", cand_req.min_oos_trades)
        } else if !safe_gate_passed {
            format!(
                "Rail breach gate failed: {:.1}% OOS / {:.1}% @ +1c daily breach prob",
                boot.daily_rail_breach_prob_pct, boot.stress_1c_daily_rail_breach_prob_pct
            )
        } else {
            let cushion_label = breakeven_slippage_cents
                .map(|c| format!("{c:.2}c ({slippage_cushion_status})"))
                .unwrap_or_else(|| "n/a".to_string());
            format!(
                "0.0% rail breach (OOS & +1c) · {regime_stability} · cushion {cushion_label} · +1c P50 ${:.2} · +1c P95 DD {:.1}%",
                boot.stress_1c_p50_end_equity, boot.stress_1c_p95_max_drawdown_pct
            )
        };

        rows.push(StrategyPresetCandidate {
            id: spec.id.to_string(),
            label: spec.label.to_string(),
            subtitle: spec.subtitle.to_string(),
            config: cand_req,
            active_winners_count,
            net_expectancy_cents,
            oos_net_pnl_dollars: comparison.auto_allocator_net_pnl,
            oos_max_dd_pct: comparison.auto_allocator_max_dd_pct,
            bootstrap_p50_equity: boot.p50_end_equity,
            bootstrap_p95_max_dd_pct: boot.p95_max_drawdown_pct,
            stress_1c_p50_equity: boot.stress_1c_p50_end_equity,
            stress_1c_p95_max_dd_pct: boot.stress_1c_p95_max_drawdown_pct,
            daily_rail_breach_prob_pct: boot.daily_rail_breach_prob_pct,
            stress_1c_daily_rail_breach_prob_pct: boot.stress_1c_daily_rail_breach_prob_pct,
            risk_adjusted_score,
            safe_gate_passed,
            is_top_safe: false,
            calm_expectancy_cents,
            high_vol_expectancy_cents,
            calm_stress_1c_survival,
            high_vol_stress_1c_survival,
            regime_stability,
            breakeven_slippage_cents,
            slippage_cushion_status,
            slippage_ladder,
            note,
        });
    }

    rows.sort_by(|a, b| {
        b.safe_gate_passed
            .cmp(&a.safe_gate_passed)
            .then_with(|| b.risk_adjusted_score.total_cmp(&a.risk_adjusted_score))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut top_safe_preset = None;
    if let Some(first) = rows.first_mut() {
        if first.safe_gate_passed {
            first.is_top_safe = true;
            top_safe_preset = Some(first.id.clone());
        }
    }

    (rows, top_safe_preset)
}

#[allow(clippy::too_many_arguments)]
fn compute_forward_envelope_monitor(
    req: &AutoAllocatorRequest,
    winners: &[AllocatedWinner],
    drift_gauge: &DriftGauge,
    bootstrap_stress: &BootstrapStressSummary,
    comparison: &AllocatorComparison,
    leaderboard: &[StrategyPresetCandidate],
    oos_windows_count: usize,
    live_armed: bool,
) -> ForwardEnvelopeMonitor {
    let matched_preset = leaderboard.iter().find(|p| {
        p.config.execution_mode == req.execution_mode
            && p.config.sizing_mode == req.sizing_mode
            && (p.config.max_slot_risk_pct - req.max_slot_risk_pct).abs() < 1e-6
            && (p.config.max_total_heat_pct - req.max_total_heat_pct).abs() < 1e-6
    });
    let active_preset_id = matched_preset.map(|p| p.id.clone());
    let active_preset_label = matched_preset
        .map(|p| p.label.clone())
        .unwrap_or_else(|| {
            format!(
                "Custom ({} · {:.0}%/{:.0}%)",
                req.execution_mode, req.max_slot_risk_pct, req.max_total_heat_pct
            )
        });

    let exact_day = (oos_windows_count as f64 / 96.0).clamp(0.0, 30.0);
    let envelope_day = (exact_day.round() as u32).clamp(1, 30);
    let (envelope_p5_equity, envelope_p50_equity) = if bootstrap_stress.trajectory.len() >= 2 {
        let max_idx = bootstrap_stress.trajectory.len() - 1;
        let lo = (exact_day.floor() as usize).min(max_idx);
        let hi = (exact_day.ceil() as usize).min(max_idx);
        let t = (exact_day - (lo as f64)).clamp(0.0, 1.0);
        let a = &bootstrap_stress.trajectory[lo];
        let b = &bootstrap_stress.trajectory[hi];
        (
            a.p5_equity + (b.p5_equity - a.p5_equity) * t,
            a.p50_equity + (b.p50_equity - a.p50_equity) * t,
        )
    } else {
        (req.starting_bankroll, req.starting_bankroll)
    };
    let observed_paper_equity = comparison.auto_allocator_end_equity;

    let total_w: f64 = winners.iter().map(|w| w.allocated_pct).sum();
    let predicted_ev_cents = if total_w > 1e-9 {
        finite_opt(
            winners
                .iter()
                .map(|w| w.oos_net_expectancy_cents * (w.allocated_pct / total_w))
                .sum::<f64>(),
        )
    } else if !winners.is_empty() {
        finite_opt(
            winners
                .iter()
                .map(|w| w.oos_net_expectancy_cents)
                .sum::<f64>()
                / (winners.len() as f64),
        )
    } else {
        None
    };

    let observed_forward_ev_cents = drift_gauge.shadow_expectancy_cents.or(predicted_ev_cents);
    let expectancy_drift_cents = match (observed_forward_ev_cents, predicted_ev_cents) {
        (Some(obs), Some(pred)) => finite_opt(obs - pred),
        _ => drift_gauge.drift_cents,
    };
    let realized_slippage_vs_model_cents = expectancy_drift_cents
        .map(|d| finite_or((-d).max(0.0), 0.0))
        .unwrap_or(0.0);

    let next_safe = leaderboard
        .iter()
        .find(|p| p.safe_gate_passed && active_preset_id.as_ref() != Some(&p.id));

    let has_data = !winners.is_empty() && oos_windows_count > 0;
    let rail_breached = bootstrap_stress.daily_rail_breach_prob_pct > 0.0
        || bootstrap_stress.stress_1c_daily_rail_breach_prob_pct > 0.0;
    let below_p5 = has_data && observed_paper_equity + 1e-6 < envelope_p5_equity;
    let decay_critical = drift_gauge.status == "DECAY_CRITICAL"
        || observed_forward_ev_cents.is_some_and(|ev| ev < 0.0)
        || realized_slippage_vs_model_cents > 2.5;

    let status = if !has_data {
        if decay_critical {
            "DRIFT_DEMOTE"
        } else {
            "NO_DATA"
        }
    } else if rail_breached || below_p5 || decay_critical {
        "DRIFT_DEMOTE"
    } else {
        "ON_TRACK"
    }
    .to_string();

    let summary = match status.as_str() {
        "ON_TRACK" => format!(
            "ON TRACK (Day {envelope_day} envelope): paper ${observed_paper_equity:.2} >= P5 ${envelope_p5_equity:.2} (P50 ${envelope_p50_equity:.2}), 0.0% daily rail breach."
        ),
        "DRIFT_DEMOTE" => {
            let fmt_b = |v: f64| -> String {
                if v > 0.0 && v < 0.05 {
                    "<0.1%".to_string()
                } else {
                    format!("{v:.1}%")
                }
            };
            let mut reasons: Vec<String> = Vec::new();
            if rail_breached {
                reasons.push(format!(
                    "rail breach {}/{} @ +1c",
                    fmt_b(bootstrap_stress.daily_rail_breach_prob_pct),
                    fmt_b(bootstrap_stress.stress_1c_daily_rail_breach_prob_pct)
                ));
            }
            if below_p5 {
                reasons.push(format!(
                    "paper ${observed_paper_equity:.2} < P5 ${envelope_p5_equity:.2}"
                ));
            }
            if decay_critical {
                reasons.push(format!("slippage/drift {realized_slippage_vs_model_cents:.2}c/ct"));
            }
            let rot = next_safe
                .map(|p| format!(" -> rotate to {}", p.label))
                .unwrap_or_default();
            format!(
                "DRIFT / DEMOTE ({}): {}{rot}",
                active_preset_label,
                reasons.join("; ")
            )
        }
        _ => "Awaiting active OOS winners & paper forward-test windows for envelope tracking."
            .to_string(),
    };

    ForwardEnvelopeMonitor {
        status,
        active_preset_id,
        active_preset_label,
        envelope_day,
        envelope_p5_equity,
        envelope_p50_equity,
        observed_paper_equity,
        predicted_ev_cents,
        observed_forward_ev_cents,
        expectancy_drift_cents,
        realized_slippage_vs_model_cents,
        daily_rail_breach_prob_pct: bootstrap_stress.daily_rail_breach_prob_pct,
        stress_1c_daily_rail_breach_prob_pct: bootstrap_stress.stress_1c_daily_rail_breach_prob_pct,
        next_safe_preset_id: next_safe.map(|p| p.id.clone()),
        next_safe_preset_label: next_safe.map(|p| p.label.clone()),
        live_armed,
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
            window_vol_scores: HashMap::new(),
            window_spread_cents: HashMap::new(),
            window_depth_contracts: HashMap::new(),
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
            enable_diversification_guard: true,
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

    #[test]
    fn preset_leaderboard_ranks_zero_rail_breach_top_safe_and_keeps_live_disarmed() {
        let strats = vec![
            StrategyMeta {
                name: "steady-maker".into(),
                side: "yes".into(),
            },
            StrategyMeta {
                name: "loser-trap".into(),
                side: "no".into(),
            },
        ];
        let mut entries = Vec::new();
        for w in 0..80i64 {
            let epoch = 30_000 + w * 900;
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXBTC15M-LB{w:02}"),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "steady-maker".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: w % 4 != 0, // 75% win rate
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXETH15M-LB{w:02}"),
                series: "KXETH15M".into(),
                strategy_idx: 1,
                strategy: "loser-trap".into(),
                side: "no".into(),
                entry_price: 0.72,
                secs_to_close: 180,
                won: w % 4 == 0,
            });
        }
        let ctx = make_ctx(strats, entries, 80, 32, 30_000 + 48 * 900);
        let rails = RiskRailsConfig {
            max_daily_loss_dollars: 500.0,
            ..RiskRailsConfig::default()
        };
        let req = AutoAllocatorRequest {
            starting_bankroll: 500.0,
            min_oos_trades: 20,
            ..AutoAllocatorRequest::default()
        };
        let rep = compute_auto_allocator_report(&ctx, &req, &rails, false, false);
        assert!(!rep.live_armed);
        assert_eq!(rep.preset_leaderboard.len(), 6);
        assert!(rep
            .preset_leaderboard
            .iter()
            .any(|p| p.id == "diversified_safe_basket"));
        let top_id = rep
            .top_safe_preset
            .as_ref()
            .expect("expected a zero-rail-breach top safe preset");
        let top = rep
            .preset_leaderboard
            .iter()
            .find(|p| &p.id == top_id)
            .expect("top preset present in leaderboard");
        assert!(top.is_top_safe);
        assert!(top.safe_gate_passed);
        assert_eq!(top.daily_rail_breach_prob_pct, 0.0);
        assert_eq!(top.stress_1c_daily_rail_breach_prob_pct, 0.0);
        assert!(top.active_winners_count >= 1);

        // Tighter daily-loss rail ($1) must disqualify breaching presets from top_safe_preset.
        let strict_rails = RiskRailsConfig {
            max_daily_loss_dollars: 1.0,
            ..RiskRailsConfig::default()
        };
        let strict_rep = compute_auto_allocator_report(&ctx, &req, &strict_rails, false, false);
        for p in &strict_rep.preset_leaderboard {
            if p.daily_rail_breach_prob_pct > 0.0 || p.stress_1c_daily_rail_breach_prob_pct > 0.0 {
                assert!(!p.safe_gate_passed);
                assert!(!p.is_top_safe);
            }
        }
    }

    #[test]
    fn forward_envelope_monitor_detects_drift_and_selects_next_safe_rotation() {
        let strats = vec![StrategyMeta {
            name: "decay-maker".into(),
            side: "yes".into(),
        }];
        let mut entries = Vec::new();
        // IS + first two OOS folds strong (80% WR), trailing OOS tail collapses -> active winner overall, DECAY_CRITICAL trailing!
        for w in 0..120i64 {
            let won = if w < 108 { w % 5 != 0 } else { false };
            entries.push(TournamentEntry {
                close_epoch: 40_000 + w * 900,
                ticker: format!("KXBTC15M-FE{w:03}"),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "decay-maker".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won,
            });
        }
        let ctx = make_ctx(strats, entries, 120, 48, 40_000 + 72 * 900);
        let rails = RiskRailsConfig {
            max_daily_loss_dollars: 500.0,
            ..RiskRailsConfig::default()
        };
        let req = AutoAllocatorRequest {
            starting_bankroll: 500.0,
            min_oos_trades: 20,
            require_stress_resilience: false,
            ..AutoAllocatorRequest::default()
        };
        let rep = compute_auto_allocator_report(&ctx, &req, &rails, false, false);
        assert!(!rep.forward_envelope.live_armed);
        assert_eq!(rep.forward_envelope.status, "DRIFT_DEMOTE");
        assert!(rep.forward_envelope.envelope_day >= 1);
        assert!(rep.forward_envelope.envelope_p5_equity <= rep.forward_envelope.envelope_p50_equity + 1e-6);

        // On healthy monotone positive tape with Conservative Maker QK, monitor reports ON_TRACK.
        let healthy_strats = vec![StrategyMeta {
            name: "steady-maker".into(),
            side: "yes".into(),
        }];
        let healthy_entries: Vec<TournamentEntry> = (0..80i64)
            .map(|w| TournamentEntry {
                close_epoch: 50_000 + w * 900,
                ticker: format!("KXBTC15M-OK{w:02}"),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "steady-maker".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: w % 4 != 0,
            })
            .collect();
        let h_ctx = make_ctx(healthy_strats, healthy_entries, 80, 32, 50_000 + 48 * 900);
        let cons_req = AutoAllocatorRequest {
            execution_mode: "maker".into(),
            sizing_mode: "quarter_kelly".into(),
            maker_fill_rate: 0.70,
            min_oos_trades: 20,
            max_slot_risk_pct: 2.0,
            max_total_heat_pct: 10.0,
            starting_bankroll: 500.0,
            require_stress_resilience: true,
            enable_regime_throttle: true,
            min_fill_rate_pct: 60.0,
            ..AutoAllocatorRequest::default()
        };
        let h_rep = compute_auto_allocator_report(&h_ctx, &cons_req, &rails, false, false);
        assert_eq!(h_rep.forward_envelope.status, "ON_TRACK");
        assert_eq!(
            h_rep.forward_envelope.active_preset_id.as_deref(),
            Some("conservative_maker_qk")
        );
        if let Some(next_id) = &h_rep.forward_envelope.next_safe_preset_id {
            let next_p = h_rep
                .preset_leaderboard
                .iter()
                .find(|p| &p.id == next_id)
                .expect("next_safe_preset exists in leaderboard");
            assert!(next_p.safe_gate_passed);
            assert_eq!(next_p.daily_rail_breach_prob_pct, 0.0);
            assert_eq!(next_p.stress_1c_daily_rail_breach_prob_pct, 0.0);
        }
    }

    #[test]
    fn diversification_guard_penalizes_correlated_asset_cluster_spikes_and_keeps_rails_safe() {
        let strats = vec![
            StrategyMeta {
                name: "btc-alpha".into(),
                side: "yes".into(),
            },
            StrategyMeta {
                name: "eth-corr".into(),
                side: "yes".into(),
            },
            StrategyMeta {
                name: "sol-indep".into(),
                side: "yes".into(),
            },
        ];
        let mut entries = Vec::new();
        for w in 0..90i64 {
            let epoch = 60_000 + w * 900;
            // BTC and ETH move identically in every window (r = 1.00 >= 0.55) -> BTC+ETH cluster;
            // SOL wins on a shifted orthogonal cadence -> low cross-asset correlation.
            let btc_won = w % 5 != 0;
            let sol_won = (w + 2) % 4 != 0;
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXBTC15M-D{w:03}"),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "btc-alpha".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: btc_won,
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXETH15M-D{w:03}"),
                series: "KXETH15M".into(),
                strategy_idx: 1,
                strategy: "eth-corr".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: btc_won,
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: format!("KXSOL15M-D{w:03}"),
                series: "KXSOL15M".into(),
                strategy_idx: 2,
                strategy: "sol-indep".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: sol_won,
            });
        }

        let ctx = make_ctx(strats, entries, 90, 36, 60_000 + 54 * 900);
        let rails = RiskRailsConfig {
            max_daily_loss_dollars: 500.0,
            ..RiskRailsConfig::default()
        };

        let unguarded_req = AutoAllocatorRequest {
            execution_mode: "maker".into(),
            sizing_mode: "quarter_kelly".into(),
            maker_fill_rate: 0.75,
            min_oos_trades: 20,
            max_slot_risk_pct: 20.0,
            max_total_heat_pct: 40.0,
            starting_bankroll: 1_000.0,
            require_stress_resilience: true,
            enable_regime_throttle: false,
            enable_diversification_guard: false,
            ..AutoAllocatorRequest::default()
        };
        let guarded_req = AutoAllocatorRequest {
            enable_diversification_guard: true,
            ..unguarded_req.clone()
        };

        let off = compute_auto_allocator_report(&ctx, &unguarded_req, &rails, false, false);
        let on = compute_auto_allocator_report(&ctx, &guarded_req, &rails, false, false);

        assert!(!on.live_armed);
        assert_eq!(on.diversification.status, "CLUSTER_CAPPED");
        assert!(on.diversification.heat_trimmed_pct > 0.0);
        assert!(on
            .diversification
            .max_cross_asset_corr
            .is_some_and(|r| r >= 0.90));
        assert!(
            on.diversification.correlated_cluster_heat_pct
                <= on.diversification.cluster_heat_cap_pct + 1e-6
        );
        assert!(
            on.diversification.effective_diversification_score
                > off.diversification.effective_diversification_score
        );
        assert!(on.total_allocated_pct < off.total_allocated_pct);

        let basket = on
            .preset_leaderboard
            .iter()
            .find(|p| p.id == "diversified_safe_basket")
            .expect("diversified_safe_basket present");
        assert!(basket.safe_gate_passed);
        assert_eq!(basket.daily_rail_breach_prob_pct, 0.0);

        // Session Briefing & Slippage Attribution assertions
        assert!(!on.session_briefing.live_armed);
        assert_eq!(on.session_briefing.active_winners_count, on.active_winners.len());
        assert_eq!(
            on.session_briefing.correlation_cluster_penalty_pct,
            on.diversification.heat_trimmed_pct
        );
        assert!(on.session_briefing.daily_loss_rail_dollars > 0.0);
        assert!(on.session_briefing.daily_loss_headroom_dollars <= on.session_briefing.daily_loss_rail_dollars + 1e-6);
        assert!(on.session_briefing.expected_fill_edge_cents.is_some());
        assert!(on.session_briefing.realized_fill_edge_cents.is_some());
        assert_ne!(on.regime_stability.status, "NO_DATA");
        assert_eq!(on.session_briefing.regime_stability, on.regime_stability.status);
    }

    #[test]
    fn walk_forward_regime_stability_partitions_causally_and_classifies_presets() {
        let strats = vec![
            StrategyMeta {
                name: "all-regime-edge".into(),
                side: "yes".into(),
            },
            StrategyMeta {
                name: "calm-only-edge".into(),
                side: "yes".into(),
            },
        ];
        let mut entries = Vec::new();
        let mut vol_scores = HashMap::new();
        for w in 0..100i64 {
            let epoch = 1_772_000_000 + w * 900;
            let ticker = format!("KXBTC15M-R{w:03}");
            let is_high_vol = w >= 4 && (w % 3 == 0);
            vol_scores.insert(ticker.clone(), if is_high_vol { 14.0 } else { 2.0 });

            // all-regime wins consistently across Calm & High-Vol even under +1c slippage (80% win rate at 72c -> ~+7.6c EV; coprime with 3).
            let all_won = (w % 5) != 0;
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "all-regime-edge".into(),
                side: "yes".into(),
                entry_price: 0.72,
                secs_to_close: 240,
                won: all_won,
            });

            // calm-only wins in Calm (75%) but loses in High-Vol (0%) -> quarantined overall, CALM-ONLY standalone.
            let calm_won = if is_high_vol { false } else { (w % 4) != 0 };
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker,
                series: "KXBTC15M".into(),
                strategy_idx: 1,
                strategy: "calm-only-edge".into(),
                side: "yes".into(),
                entry_price: 0.72,
                secs_to_close: 240,
                won: calm_won,
            });
        }

        let ctx_a = TournamentTapeContext {
            window_vol_scores: vol_scores.clone(),
            ..make_ctx(strats.clone(), entries.clone(), 100, 40, 1_772_000_000 + 60 * 900)
        };
        // Zero-lookahead canary: mutate future windows (>= 75) wildly and ensure earlier epochs (< 75) never change!
        let mut vol_scores_mut = vol_scores;
        for w in 75..100i64 {
            vol_scores_mut.insert(format!("KXBTC15M-R{w:03}"), 99.0);
        }
        let ctx_b = TournamentTapeContext {
            window_vol_scores: vol_scores_mut,
            ..make_ctx(strats, entries, 100, 40, 1_772_000_000 + 60 * 900)
        };
        let (reg_a, calm_n, hv_n) = classify_causal_epoch_regimes(&ctx_a);
        let (reg_b, _, _) = classify_causal_epoch_regimes(&ctx_b);
        assert!(calm_n > 0 && hv_n > 0);
        for w in 0..75i64 {
            let epoch = 1_772_000_000 + w * 900;
            assert_eq!(
                reg_a.get(&epoch),
                reg_b.get(&epoch),
                "future volatility spike at >=75 leaked into causal epoch {w}"
            );
        }

        let rails = RiskRailsConfig {
            max_daily_loss_dollars: 500.0,
            ..RiskRailsConfig::default()
        };
        let req = AutoAllocatorRequest {
            execution_mode: "maker".into(),
            sizing_mode: "quarter_kelly".into(),
            maker_fill_rate: 0.75,
            min_oos_trades: 20,
            max_slot_risk_pct: 5.0,
            max_total_heat_pct: 20.0,
            starting_bankroll: 100.0,
            require_stress_resilience: true,
            enable_regime_throttle: false,
            enable_diversification_guard: true,
            ..AutoAllocatorRequest::default()
        };
        let rep = compute_auto_allocator_report(&ctx_a, &req, &rails, false, false);
        assert!(!rep.live_armed);
        assert_eq!(rep.regime_stability.status, "ALL-REGIME");
        assert!(rep.regime_stability.active_calm_ev_cents.is_some_and(|v| v > 0.0));
        assert!(rep.regime_stability.active_high_vol_ev_cents.is_some_and(|v| v > 0.0));
        assert!(rep.regime_stability.active_calm_stress_1c_ok);
        assert!(rep.regime_stability.active_high_vol_stress_1c_ok);
        assert!(rep.regime_stability.all_regime_safe_presets >= 1);
        assert_eq!(rep.session_briefing.regime_stability, "ALL-REGIME");
        assert!(rep
            .preset_leaderboard
            .iter()
            .any(|p| p.safe_gate_passed && p.regime_stability == "ALL-REGIME"));

        // Verify calm-only strategy classifies end-to-end as CALM-ONLY (winning Calm, losing High-Vol).
        let calm_only_entries: Vec<TournamentEntry> = ctx_a
            .entries
            .iter()
            .filter(|e| e.strategy_idx == 1)
            .cloned()
            .map(|mut e| {
                e.strategy_idx = 0;
                e.entry_price = 0.60;
                if *reg_a.get(&e.close_epoch).unwrap_or(&false) {
                    e.won = e.close_epoch % 1800 == 0; // ~50% in HV -> negative EV at 60c
                } else {
                    e.won = true; // 100% in Calm -> overall positive EV
                }
                e
            })
            .collect();
        let calm_ctx = TournamentTapeContext {
            window_vol_scores: ctx_a.window_vol_scores.clone(),
            ..make_ctx(
                vec![StrategyMeta {
                    name: "calm-only-edge".into(),
                    side: "yes".into(),
                }],
                calm_only_entries,
                100,
                40,
                1_772_000_000 + 60 * 900,
            )
        };
        let calm_req = AutoAllocatorRequest {
            require_stress_resilience: false,
            ..req.clone()
        };
        let calm_rep = compute_auto_allocator_report(&calm_ctx, &calm_req, &rails, false, false);
        assert_eq!(calm_rep.regime_stability.status, "CALM-ONLY");
    }

    #[test]
    fn slippage_ladder_degrades_monotonically_and_computes_exact_breakeven_cushion() {
        let mut entries = Vec::new();
        for i in 0..123 {
            let epoch = 1_773_000_000 + i * 900;
            entries.push(TournamentEntry {
                ticker: format!("KXBTC15M-SLIP-{i:03}"),
                series: "KXBTC15M".into(),
                close_epoch: epoch,
                secs_to_close: 300,
                strategy_idx: 0,
                strategy: "safe-edge".into(),
                side: "yes".into(),
                entry_price: 0.60,
                // 75% WR at 60c: gross EV = +15.0c, maker fee = 0.42c -> net EV ≈ +14.58c (SAFE >= 2c).
                won: (i % 4) != 0,
            });
            entries.push(TournamentEntry {
                ticker: format!("KXBTC15M-SLIP-{i:03}"),
                series: "KXBTC15M".into(),
                close_epoch: epoch,
                secs_to_close: 300,
                strategy_idx: 1,
                strategy: "sub1c-edge".into(),
                side: "yes".into(),
                entry_price: 0.60,
                // 25/41 = 60.976% WR at 60c across OOS (i = 82..123): gross +0.976c - 0.42c fee = +0.556c -> THIN (< 1c).
                won: (i % 41) < 25,
            });
        }
        let ctx_safe = make_ctx(
            vec![StrategyMeta {
                name: "safe-edge".into(),
                side: "yes".into(),
            }],
            entries.iter().filter(|e| e.strategy_idx == 0).cloned().collect(),
            123,
            41,
            1_773_000_000 + 82 * 900,
        );
        let ctx_thin = make_ctx(
            vec![StrategyMeta {
                name: "sub1c-edge".into(),
                side: "yes".into(),
            }],
            entries
                .iter()
                .filter(|e| e.strategy_idx == 1)
                .cloned()
                .map(|mut e| {
                    e.strategy_idx = 0;
                    e
                })
                .collect(),
            123,
            41,
            1_773_000_000 + 82 * 900,
        );
        let rails = RiskRailsConfig {
            max_daily_loss_dollars: 500.0,
            ..RiskRailsConfig::default()
        };
        let req_safe = AutoAllocatorRequest {
            starting_bankroll: 500.0,
            maker_fill_rate: 0.50,
            max_slot_risk_pct: 0.5,
            max_total_heat_pct: 2.0,
            min_oos_trades: 20,
            require_stress_resilience: false,
            ..Default::default()
        };
        let req_thin = AutoAllocatorRequest {
            starting_bankroll: 500.0,
            min_oos_trades: 20,
            require_stress_resilience: false,
            ..Default::default()
        };
        let rep_safe = compute_auto_allocator_report(&ctx_safe, &req_safe, &rails, false, false);
        assert!(!rep_safe.live_armed);
        assert_eq!(rep_safe.slippage_ladder.len(), 4);
        assert_eq!(rep_safe.session_briefing.slippage_ladder.len(), 4);
        for k in 0..3 {
            let cur = &rep_safe.slippage_ladder[k];
            let nxt = &rep_safe.slippage_ladder[k + 1];
            assert!(
                cur.net_expectancy_cents.unwrap() > nxt.net_expectancy_cents.unwrap(),
                "ladder EV must strictly decrease with slippage"
            );
            assert!(
                cur.projected_daily_net_dollars >= nxt.projected_daily_net_dollars - 1e-6,
                "daily net must not increase with slippage"
            );
            assert!(
                cur.daily_rail_breach_prob_pct <= nxt.daily_rail_breach_prob_pct + 1e-9,
                "rail breach prob must be monotone across slippage rungs"
            );
        }
        assert!(rep_safe.breakeven_slippage_cents.unwrap() >= 2.0);
        assert_eq!(rep_safe.slippage_cushion_status, "SAFE");
        assert_eq!(
            rep_safe.session_briefing.slippage_cushion_status,
            rep_safe.slippage_cushion_status
        );

        let rep_thin = compute_auto_allocator_report(&ctx_thin, &req_thin, &rails, false, false);
        assert_eq!(rep_thin.slippage_ladder.len(), 4);
        let thin_be = rep_thin.breakeven_slippage_cents.unwrap();
        assert!(thin_be < 1.0, "expected <1c break-even cushion, got {thin_be}");
        assert_eq!(rep_thin.slippage_cushion_status, "THIN");
        assert!(!rep_thin.slippage_ladder[1].positive_ev);
        assert!(!rep_thin.slippage_ladder[1].survival_ok);
    }

    #[test]
    fn correlated_regime_drawdown_ceiling_prevents_rail_breach_stacking() {
        let strats = vec![
            StrategyMeta {
                name: "btc-trend".into(),
                side: "yes".into(),
            },
            StrategyMeta {
                name: "btc-momo".into(),
                side: "yes".into(),
            },
        ];
        // Both trade KXBTC15M with r in [0.40, 0.64] (below 0.65 pruning cutoff, yet co-moving)
        // and experience a 3-epoch High-Vol correlated drawdown streak in both IS & OOS.
        let build_entries = |tail_won: bool| {
            let mut entries = Vec::new();
            let mut vol_scores = HashMap::new();
            for w in 0..90i64 {
                let epoch = 1_774_000_000 + w * 900;
                let ticker = format!("KXBTC15M-RC{w:03}");
                let is_hv_streak = (22..=24).contains(&w) || (68..=70).contains(&w);
                vol_scores.insert(ticker.clone(), if is_hv_streak { 14.0 } else { 2.5 });
                let (w0, w1) = if is_hv_streak {
                    (false, false)
                } else if w >= 85 {
                    (tail_won, tail_won)
                } else {
                    // Co-moving shared wins/losses with moderate divergence so r lands around ~0.48
                    // (above 0.35 same-asset cluster threshold, below 0.65 prune cutoff).
                    match w % 8 {
                        0 => (false, false),
                        1 => (true, false),
                        2 => (false, true),
                        _ => (true, true),
                    }
                };
                entries.push(TournamentEntry {
                    close_epoch: epoch,
                    ticker: ticker.clone(),
                    series: "KXBTC15M".into(),
                    strategy_idx: 0,
                    strategy: "btc-trend".into(),
                    side: "yes".into(),
                    entry_price: 0.55,
                    secs_to_close: 240,
                    won: w0,
                });
                entries.push(TournamentEntry {
                    close_epoch: epoch,
                    ticker,
                    series: "KXBTC15M".into(),
                    strategy_idx: 1,
                    strategy: "btc-momo".into(),
                    side: "yes".into(),
                    entry_price: 0.55,
                    secs_to_close: 240,
                    won: w1,
                });
            }
            TournamentTapeContext {
                window_vol_scores: vol_scores,
                ..make_ctx(strats.clone(), entries, 90, 36, 1_774_000_000 + 54 * 900)
            }
        };

        let ctx_win = build_entries(true);
        let ctx_lose = build_entries(false);
        let rails = RiskRailsConfig {
            max_daily_loss_dollars: 150.0, // 15% of $1,000 bankroll
            ..RiskRailsConfig::default()
        };
        let req_off = AutoAllocatorRequest {
            execution_mode: "maker".into(),
            sizing_mode: "half_kelly".into(),
            maker_fill_rate: 1.0,
            min_oos_trades: 20,
            max_slot_risk_pct: 15.0,
            max_total_heat_pct: 30.0,
            starting_bankroll: 1_000.0,
            require_stress_resilience: false,
            enable_regime_throttle: false,
            enable_diversification_guard: false,
            ..AutoAllocatorRequest::default()
        };
        let req_on = AutoAllocatorRequest {
            enable_diversification_guard: true,
            ..req_off.clone()
        };

        let off = compute_auto_allocator_report(&ctx_win, &req_off, &rails, false, false);
        let on = compute_auto_allocator_report(&ctx_win, &req_on, &rails, false, false);
        let on_lose = compute_auto_allocator_report(&ctx_lose, &req_on, &rails, false, false);

        assert!(!on.live_armed);
        assert_eq!(on.active_winners.len(), 2, "both same-asset correlated winners survive 0.65 prune");
        assert_eq!(off.diversification.ceiling_status, "UNPROTECTED_BREACH");
        assert!(
            off.diversification.worst_correlated_cluster_heat_pct
                > off.diversification.regime_drawdown_ceiling_pct
        );
        assert_eq!(on.diversification.ceiling_status, "CEILING_CLAMPED");
        assert!(on.diversification.hv_regime_mult > 1.0);
        assert!(on.diversification.ceiling_trimmed_pct > 0.0);
        assert!(
            on.diversification.worst_correlated_cluster_heat_pct
                <= on.diversification.regime_drawdown_ceiling_pct + 1e-6
        );
        assert_eq!(on.session_briefing.ceiling_status, "CEILING_CLAMPED");
        assert!(on.total_allocated_pct < off.total_allocated_pct);

        // Causal replay invariant: early OOS curve checkpoint (w=54..84) cannot depend on tail_won at w>=85.
        assert!(
            (on.comparison.equity_curve[10].allocator_equity
                - on_lose.comparison.equity_curve[10].allocator_equity)
                .abs()
                < 1e-9,
            "causal regime drawdown ceiling replay leaked future outcome"
        );
    }

    #[test]
    fn dry_run_order_plan_audits_fees_spread_min_fill_and_stays_disarmed() {
        let strats = vec![
            StrategyMeta {
                name: "strong-maker".into(),
                side: "yes".into(),
            },
            StrategyMeta {
                name: "thin-rounding-edge".into(),
                side: "yes".into(),
            },
        ];
        // Build 100 windows:
        // - strong-maker: 75% WR at 55c -> +20c gross EV, clears even at 1 contract.
        // - thin-rounding-edge: 33/40 = 82.5% WR in OOS at 81.75c -> +0.75c gross EV (> 0.26c raw maker fee,
        //   but at C=1 Kalshi rounds 0.26c UP to 1.00c so net EV = -0.25c! Needs C >= 2 contracts for 0.50c/ct!).
        let mut entries = Vec::new();
        let mut spread_map = HashMap::new();
        for w in 0..100i64 {
            let epoch = 1_775_000_000 + w * 900;
            let ticker = format!("KXBTC15M-DR{w:03}");
            spread_map.insert(ticker.clone(), 4.0); // 4.0c bid-ask spread -> 2.0c taker cross, 0.0c maker
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker: ticker.clone(),
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "strong-maker".into(),
                side: "yes".into(),
                entry_price: 0.55,
                secs_to_close: 240,
                won: w % 4 != 0,
            });
            entries.push(TournamentEntry {
                close_epoch: epoch,
                ticker,
                series: "KXBTC15M".into(),
                strategy_idx: 1,
                strategy: "thin-rounding-edge".into(),
                side: "yes".into(),
                entry_price: 0.8175,
                secs_to_close: 240,
                won: (w % 40) < 33,
            });
        }
        let ctx = TournamentTapeContext {
            window_spread_cents: spread_map,
            ..make_ctx(strats, entries, 100, 40, 1_775_000_000 + 60 * 900)
        };
        let rails = RiskRailsConfig {
            max_order_contracts: 50,
            max_order_dollars: 50.0,
            max_daily_loss_dollars: 500.0,
            ..RiskRailsConfig::default()
        };

        let maker_req = AutoAllocatorRequest {
            execution_mode: "maker".into(),
            sizing_mode: "quarter_kelly".into(),
            maker_fill_rate: 1.0,
            min_oos_trades: 20,
            max_slot_risk_pct: 10.0,
            max_total_heat_pct: 30.0,
            starting_bankroll: 1_000.0,
            require_stress_resilience: false,
            enable_regime_throttle: false,
            enable_diversification_guard: true,
            ..AutoAllocatorRequest::default()
        };
        let m_rep = compute_auto_allocator_report(&ctx, &maker_req, &rails, false, false);
        assert!(!m_rep.dry_run_plan.live_armed);
        assert_eq!(m_rep.dry_run_plan.status, "PLAN_PASS");
        let strong = m_rep
            .dry_run_plan
            .legs
            .iter()
            .find(|l| l.strategy == "strong-maker")
            .expect("strong-maker leg");
        assert_eq!(strong.spread_cross_cost_cents, 0.0);
        assert_eq!(strong.min_fill_contracts_for_positive_ev, 1);
        assert!(strong.sanity_passed);
        assert!((strong.taker_fee_per_ct_cents - 4.0 * strong.maker_fee_per_ct_cents).abs() < 1e-6);

        let thin = m_rep
            .dry_run_plan
            .legs
            .iter()
            .find(|l| l.strategy == "thin-rounding-edge")
            .expect("thin-rounding-edge leg");
        assert!(
            thin.min_fill_contracts_for_positive_ev >= 2,
            "thin 0.75c edge must require >=2 contracts to clear 1c Kalshi min fee rounding, got {}",
            thin.min_fill_contracts_for_positive_ev
        );

        // Taker mode adds 0.5 * 4.0c = 2.0c spread crossing + taker fee + slippage.
        let taker_req = AutoAllocatorRequest {
            execution_mode: "taker".into(),
            slippage_cents: 1.0,
            ..maker_req
        };
        let t_rep = compute_auto_allocator_report(&ctx, &taker_req, &rails, false, false);
        assert!(!t_rep.dry_run_plan.live_armed);
        let t_strong = t_rep
            .dry_run_plan
            .legs
            .iter()
            .find(|l| l.strategy == "strong-maker")
            .expect("taker strong-maker");
        assert!((t_strong.spread_cross_cost_cents - 2.0).abs() < 1e-6);
        assert_eq!(t_strong.slippage_cost_cents, 1.0);
        assert!(t_strong.net_oos_ev_after_all_cents < strong.net_oos_ev_after_all_cents);
    }

    #[test]
    fn causal_liquidity_capacity_scales_1x_2x_5x_and_distinguishes_liquid_tight_crowded() {
        let strats = vec![StrategyMeta {
            name: "deep-maker".into(),
            side: "yes".into(),
        }];
        let mut entries = Vec::new();
        let mut spread_map = HashMap::new();
        let mut deep_map = HashMap::new();
        let mut tight_map = HashMap::new();
        let mut crowded_map = HashMap::new();

        for w in 0..100i64 {
            let ticker = format!("KXBTC15M-LIQ{w:03}-30");
            spread_map.insert(ticker.clone(), 2.0);
            deep_map.insert(ticker.clone(), 250.0);
            tight_map.insert(ticker.clone(), 20.0);
            crowded_map.insert(ticker.clone(), 5.0);
            entries.push(TournamentEntry {
                close_epoch: 1_775_000_000 + w * 900,
                ticker,
                series: "KXBTC15M".into(),
                strategy_idx: 0,
                strategy: "deep-maker".into(),
                side: "yes".into(),
                entry_price: 0.72,
                secs_to_close: 240,
                won: (w % 10) < 8,
            });
        }

        let base_ctx = make_ctx(strats, entries, 100, 40, 1_775_000_000 + 60 * 900);
        let deep_ctx = TournamentTapeContext {
            window_spread_cents: spread_map.clone(),
            window_depth_contracts: deep_map,
            ..base_ctx.clone()
        };
        let tight_ctx = TournamentTapeContext {
            window_spread_cents: spread_map.clone(),
            window_depth_contracts: tight_map,
            ..base_ctx.clone()
        };
        let crowded_ctx = TournamentTapeContext {
            window_spread_cents: spread_map,
            window_depth_contracts: crowded_map,
            ..base_ctx
        };

        let rails = RiskRailsConfig {
            max_order_contracts: 50,
            max_order_dollars: 50.0,
            max_daily_loss_dollars: 500.0,
            ..RiskRailsConfig::default()
        };
        let req = AutoAllocatorRequest {
            execution_mode: "maker".into(),
            sizing_mode: "quarter_kelly".into(),
            maker_fill_rate: 1.0,
            min_oos_trades: 20,
            max_slot_risk_pct: 10.0,
            max_total_heat_pct: 30.0,
            starting_bankroll: 100.0,
            require_stress_resilience: false,
            enable_regime_throttle: false,
            enable_diversification_guard: true,
            ..AutoAllocatorRequest::default()
        };

        let deep_rep = compute_auto_allocator_report(&deep_ctx, &req, &rails, false, false);
        let tight_rep = compute_auto_allocator_report(&tight_ctx, &req, &rails, false, false);
        let crowded_rep = compute_auto_allocator_report(&crowded_ctx, &req, &rails, false, false);

        assert!(!deep_rep.liquidity_capacity.live_armed);
        assert!(!tight_rep.liquidity_capacity.live_armed);
        assert!(!crowded_rep.liquidity_capacity.live_armed);
        assert_eq!(deep_rep.liquidity_capacity.observed_depth_windows, 100);
        assert_eq!(deep_rep.liquidity_capacity.status, "LIQUID");
        assert_eq!(tight_rep.liquidity_capacity.status, "TIGHT");
        assert_eq!(crowded_rep.liquidity_capacity.status, "CROWDED");
        assert_eq!(deep_rep.liquidity_capacity.portfolio_scenarios.len(), 3);
        assert_eq!(
            deep_rep.liquidity_capacity.portfolio_scenarios[0].scale_label,
            "1x"
        );
        assert_eq!(
            deep_rep.liquidity_capacity.portfolio_scenarios[1].scale_label,
            "2x"
        );
        assert_eq!(
            deep_rep.liquidity_capacity.portfolio_scenarios[2].scale_label,
            "5x"
        );
        assert!(
            deep_rep.liquidity_capacity.max_scalable_dollars
                > tight_rep.liquidity_capacity.max_scalable_dollars
        );
        assert!(
            tight_rep.liquidity_capacity.max_scalable_dollars
                > crowded_rep.liquidity_capacity.max_scalable_dollars
        );
        assert!(deep_rep.liquidity_capacity.max_scalable_dollars > 0.0);
    }
}
