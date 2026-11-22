//! $500/Day Paper Capital Scaling & Liquidity Capacity Engine (strictly paper-only).
//!
//! Answers the practical desk-scaling question honestly from collected forward
//! tape and strictly Out-of-Sample (last 40%) fee- and slippage-adjusted
//! expectancy:
//! - Current empirical paper daily run-rate vs target (`$500/day` default).
//! - Required bankroll under Equal-Slots, Quarter-Kelly, and Half-Kelly sizing
//!   — explicitly `null` with an honest refusal reason whenever OOS expectancy
//!   after fees and slippage is `<= 0`.
//! - Per-series and multi-series 15-minute liquidity ceiling (`96` windows/day
//!   × `max_contracts_per_window`), exact `fill_ratio_at_target`, and required
//!   contracts per window.
//! - 180-trading-day deterministic 1,000-path Monte Carlo compounding trajectory
//!   (`P10`/`P50`/`P90` equity & daily run-rate, `prob_reach_target`,
//!   `median_days_to_target`, `prob_dd_gte_25_pct`) cleanly separating
//!   `"Empirical Tape"` from optional `"Sensitivity (Assumed Edge)"`.
//! - 5 mechanical readiness gates (`sample_depth >= 200`, `oos_net_expectancy > 0`,
//!   `oos_ece < 0.05`, `max_drawdown_pct < 20.0`, `liquidity_fill_ratio >= 0.75`)
//!   producing `READY_FOR_OWNER_REVIEW` vs `PAPER_RESEARCH_ONLY`.

#![allow(dead_code)]

use crate::analytics::{
    compute_performance_metrics, is_valid_whole_cent, portfolio_trades_from_sim,
};
use crate::recalibration::{run_recalibration_study, TimedCalibrationObs};
use crate::{
    facts_fingerprint, fee, window_entry_keyed, window_facts_keyed, window_fingerprint,
    SlotSimulation, Snapshot, Strategy, WindowFacts,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Number of 15-minute windows per calendar day in a 24/7 series (`24 * 4`).
pub const WINDOWS_PER_DAY_PER_SERIES: f64 = 96.0;

/// Projection horizon in trading days for the compounding Monte Carlo engine.
pub const PROJECTION_DAYS: usize = 180;

/// Number of pre-aggregated empirical day draws used to preserve exact discrete
/// trade payoff skewness while keeping 1,000-path × 180-day simulation < 15ms.
const DAY_POOL_SIZE: usize = 512;

/// Canonical 15-minute public series list for multi-series capacity illustrations.
const CANONICAL_15M_SERIES: [&str; 8] = [
    "KXBTC15M",
    "KXETH15M",
    "KXSOL15M",
    "KXXRP15M",
    "KXDOGE15M",
    "KXADA15M",
    "KXLTC15M",
    "KXAVAX15M",
];

fn default_target_daily() -> f64 {
    500.0
}
fn default_starting_bankroll() -> f64 {
    100.0
}
fn default_max_slot_risk_pct() -> f64 {
    100.0 / 12.0
}
fn default_slippage_cents() -> f64 {
    1.0
}
fn default_max_contracts() -> u64 {
    250
}
fn default_active_series() -> usize {
    4
}
fn default_sizing_mode() -> String {
    "equal_slots".to_string()
}
fn default_paths() -> usize {
    1_000
}
fn default_seed() -> u64 {
    20_261_122
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

// ---------------------------------------------------------------------------
// Public API Structs
// ---------------------------------------------------------------------------

/// User-supplied request parameters for `POST /api/analytics/scale-plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalePlanRequest {
    #[serde(default = "default_target_daily")]
    pub target_daily_dollars: f64,
    #[serde(default = "default_starting_bankroll")]
    pub starting_bankroll: f64,
    #[serde(default = "default_max_slot_risk_pct")]
    pub max_slot_risk_pct: f64,
    #[serde(default = "default_slippage_cents")]
    pub slippage_cents: f64,
    #[serde(default = "default_max_contracts")]
    pub max_contracts_per_window: u64,
    #[serde(default = "default_active_series")]
    pub active_series_count: usize,
    #[serde(default = "default_sizing_mode")]
    pub sizing_mode: String,
    #[serde(default = "default_paths")]
    pub paths: usize,
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// Optional what-if edge in cents per contract after fees/slippage.
    /// Always rendered separately and labeled `"Sensitivity (Assumed Edge)"`.
    #[serde(default)]
    pub assumed_edge_cents: Option<f64>,
    /// Optional single-strategy filter (`None` or `"portfolio"` analyzes all slots).
    #[serde(default)]
    pub strategy: Option<String>,
}

impl Default for ScalePlanRequest {
    fn default() -> Self {
        Self {
            target_daily_dollars: default_target_daily(),
            starting_bankroll: default_starting_bankroll(),
            max_slot_risk_pct: default_max_slot_risk_pct(),
            slippage_cents: default_slippage_cents(),
            max_contracts_per_window: default_max_contracts(),
            active_series_count: default_active_series(),
            sizing_mode: default_sizing_mode(),
            paths: default_paths(),
            seed: default_seed(),
            assumed_edge_cents: None,
            strategy: None,
        }
    }
}

impl ScalePlanRequest {
    /// Sanitize all numeric fields to safe bounded values with zero NaN/Inf risk.
    pub fn sanitized(&self) -> Self {
        let sizing_mode = match self.sizing_mode.trim().to_ascii_lowercase().as_str() {
            "quarter_kelly" | "quarter-kelly" | "qkelly" => "quarter_kelly".to_string(),
            "half_kelly" | "half-kelly" | "hkelly" => "half_kelly".to_string(),
            _ => "equal_slots".to_string(),
        };
        let strategy = self
            .strategy
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("portfolio") && !s.eq_ignore_ascii_case("all"))
            .map(ToString::to_string);
        Self {
            target_daily_dollars: finite_or(self.target_daily_dollars, default_target_daily())
                .clamp(1.0, 1_000_000.0),
            starting_bankroll: finite_or(self.starting_bankroll, default_starting_bankroll())
                .clamp(1.0, 100_000_000.0),
            max_slot_risk_pct: finite_or(self.max_slot_risk_pct, default_max_slot_risk_pct())
                .clamp(0.5, 50.0),
            slippage_cents: finite_or(self.slippage_cents, default_slippage_cents())
                .clamp(0.0, 5.0),
            max_contracts_per_window: self.max_contracts_per_window.clamp(1, 100_000),
            active_series_count: self.active_series_count.clamp(1, 8),
            sizing_mode,
            paths: self.paths.clamp(10, 5_000),
            seed: self.seed,
            assumed_edge_cents: self
                .assumed_edge_cents
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(-20.0, 25.0)),
            strategy,
        }
    }
}

/// Strictly Out-of-Sample empirical tape statistics backing the scale plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmpiricalEdgeSummary {
    pub windows_settled_total: usize,
    pub windows_settled_oos: usize,
    pub total_trades: usize,
    pub oos_trades: usize,
    pub oos_win_rate: Option<f64>,
    pub oos_avg_entry_price: Option<f64>,
    pub oos_avg_fee_cents: Option<f64>,
    pub slippage_cents: f64,
    /// Mean OOS paper net expectancy in cents per contract AFTER fees and slippage.
    pub oos_net_expectancy_cents: Option<f64>,
    /// Mean OOS return per dollar risked (`net_pnl / effective_cost * 100`).
    pub oos_return_on_risk_pct: Option<f64>,
    pub oos_ece: Option<f64>,
    pub max_drawdown_pct: f64,
    pub full_kelly_fraction: Option<f64>,
    pub active_risk_fraction: f64,
    /// Fraction of settled windows with at least one strategy entry (`0.0..=1.0`).
    pub traded_windows_rate: f64,
    pub avg_slots_per_traded_window: f64,
    pub expected_traded_windows_per_series_per_day: f64,
    pub expected_traded_windows_portfolio_per_day: f64,
}

/// Per-series and multi-series 15-minute order-book capacity limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityCeiling {
    pub windows_per_day_per_series: f64,
    pub max_contracts_per_window: u64,
    pub active_series_count: usize,
    pub series_names: Vec<String>,
    pub expected_traded_windows_per_series_per_day: f64,
    pub single_series_max_daily_contracts: f64,
    pub multi_series_max_daily_contracts: f64,
    pub single_series_max_notional_per_window: f64,
    /// Max daily paper P&L from one 15m series at `max_contracts_per_window`.
    pub single_series_max_daily_dollars: f64,
    /// Max daily paper P&L across `active_series_count` series.
    pub multi_series_max_daily_dollars: f64,
    /// Required contracts per traded window to hit `target_daily_dollars`
    /// (`None` when OOS net expectancy is non-positive).
    pub required_contracts_per_window_at_target: Option<f64>,
    /// Fraction of `target_daily_dollars` absorbable within `max_contracts_per_window` (`0.0..=1.0`).
    pub fill_ratio_at_target: f64,
    pub liquidity_constrained_at_target: bool,
    /// Median top-of-book depth observed on collected snapshots, if available (`None` when unrecorded).
    pub observed_median_depth: Option<f64>,
    pub note: String,
}

/// One day along the 180-trading-day compounding trajectory chart.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CompoundingDayPoint {
    pub day: usize,
    pub equity_p10: f64,
    pub equity_p50: f64,
    pub equity_p90: f64,
    pub daily_run_rate_p10: f64,
    pub daily_run_rate_p50: f64,
    pub daily_run_rate_p90: f64,
}

/// 180-trading-day seeded Monte Carlo compounding trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompoundingProjection {
    /// Clean label: `"Empirical Tape"` or `"Sensitivity (Assumed Edge)"`.
    pub mode_label: String,
    pub is_empirical: bool,
    pub assumed_edge_cents: Option<f64>,
    pub seed: u64,
    pub paths: usize,
    pub horizon_days: usize,
    pub starting_bankroll: f64,
    pub target_daily_dollars: f64,
    /// `P(daily run-rate >= target_daily_dollars within 180 days)` in `[0.0, 1.0]`.
    pub prob_reach_target: f64,
    /// Median trading day reaching target across ALL paths (`None` when `prob_reach_target < 0.5`).
    pub median_days_to_target: Option<usize>,
    /// Median trading day among the subset of paths that did reach target.
    pub median_days_among_reaching: Option<usize>,
    /// `P(Max Drawdown >= 25% over 180 days)` in `[0.0, 1.0]`.
    pub prob_dd_gte_25_pct: f64,
    pub terminal_equity_p10: f64,
    pub terminal_equity_p50: f64,
    pub terminal_equity_p90: f64,
    pub terminal_daily_dollars_p10: f64,
    pub terminal_daily_dollars_p50: f64,
    pub terminal_daily_dollars_p90: f64,
    pub curve: Vec<CompoundingDayPoint>,
}

/// One mechanical readiness gate inside `ReadinessScorecard`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessGate {
    pub id: String,
    pub label: String,
    pub passed: bool,
    pub observed: Option<f64>,
    pub threshold: f64,
    pub unit: String,
    pub detail: String,
}

/// 5-gate mechanical readiness scorecard (`READY_FOR_OWNER_REVIEW` vs `PAPER_RESEARCH_ONLY`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessScorecard {
    /// Strictly `"READY_FOR_OWNER_REVIEW"` iff all 5 gates pass, else `"PAPER_RESEARCH_ONLY"`.
    pub verdict: String,
    pub passed_count: usize,
    pub total_gates: usize,
    pub gates: Vec<ReadinessGate>,
    pub blocking_reasons: Vec<String>,
}

/// Complete response payload for `POST /api/analytics/scale-plan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalePlanResponse {
    pub mode_notice: String,
    pub request: ScalePlanRequest,
    pub sizing_mode_resolved: String,
    pub strategy_scope: String,
    pub target_daily_dollars: f64,
    pub current_empirical_daily_dollars: f64,
    pub required_bankroll_equal_slots: Option<f64>,
    pub required_bankroll_quarter_kelly: Option<f64>,
    pub required_bankroll_half_kelly: Option<f64>,
    pub required_bankroll_reason: Option<String>,
    pub empirical_summary: EmpiricalEdgeSummary,
    pub liquidity_ceiling: LiquidityCeiling,
    pub compounding_projection: CompoundingProjection,
    pub sensitivity_projection: Option<CompoundingProjection>,
    pub readiness_gates: ReadinessScorecard,
}

// ---------------------------------------------------------------------------
// Deterministic PRNG & Percentile Utilities (zero external crates)
// ---------------------------------------------------------------------------

struct Xoshiro256PP {
    s: [u64; 4],
}

impl Xoshiro256PP {
    fn from_seed(mut seed: u64) -> Self {
        let mut sm64 = || -> u64 {
            seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let mut s = [sm64(), sm64(), sm64(), sm64()];
        if s == [0; 4] {
            s[0] = 1;
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

    #[inline]
    fn next_usize(&mut self, bound: usize) -> usize {
        debug_assert!(bound > 0);
        (((self.next_u64() as u128) * (bound as u128)) >> 64) as usize
    }

    #[inline]
    fn next_f64(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
    }
}

fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
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

// ---------------------------------------------------------------------------
// Compounding Simulation Helpers
// ---------------------------------------------------------------------------

/// One realized day's aggregate per-contract P&L across `traded_windows_per_day`
/// windows (keeping exact binary skew while allowing live equity & contract-cap
/// sizing at every day of the 180-day simulation).
#[derive(Clone, Copy)]
struct DayPayoffSample {
    /// Sum of `(payoff - price - fee - slippage)` across one day's traded windows
    /// for 1 contract per traded window.
    sum_pnl_1ct: f64,
    /// Mean effective cost `(price + fee + slippage)` across that day's windows.
    avg_cost: f64,
    /// Mean contract entry price across that day's windows.
    avg_price: f64,
}

#[allow(clippy::too_many_arguments)]
fn run_180_day_compounding(
    mode_label: &str,
    is_empirical: bool,
    assumed_edge_cents: Option<f64>,
    day_pool: &[DayPayoffSample],
    expected_edge_per_ct: f64,
    expected_windows_per_day: f64,
    avg_entry_price: f64,
    active_window_risk_fraction: f64,
    max_contracts_per_window: u64,
    starting_bankroll: f64,
    target_daily_dollars: f64,
    paths: usize,
    seed: u64,
) -> CompoundingProjection {
    let safe_price = avg_entry_price.clamp(0.01, 0.99);
    let max_ct = max_contracts_per_window as f64;
    let expected_daily_run_rate = |equity: f64| -> f64 {
        if equity <= 0.0 || expected_windows_per_day <= 0.0 {
            return 0.0;
        }
        let desired_ct = (equity * active_window_risk_fraction / safe_price).floor();
        let actual_ct = desired_ct.min(max_ct).max(0.0);
        expected_windows_per_day * actual_ct * expected_edge_per_ct
    };

    if day_pool.is_empty() || paths == 0 || expected_windows_per_day <= 0.0 {
        let init_rr = expected_daily_run_rate(starting_bankroll);
        let curve = (0..=PROJECTION_DAYS)
            .map(|day| CompoundingDayPoint {
                day,
                equity_p10: starting_bankroll,
                equity_p50: starting_bankroll,
                equity_p90: starting_bankroll,
                daily_run_rate_p10: init_rr,
                daily_run_rate_p50: init_rr,
                daily_run_rate_p90: init_rr,
            })
            .collect();
        return CompoundingProjection {
            mode_label: mode_label.to_string(),
            is_empirical,
            assumed_edge_cents,
            seed,
            paths,
            horizon_days: PROJECTION_DAYS,
            starting_bankroll,
            target_daily_dollars,
            prob_reach_target: if init_rr >= target_daily_dollars { 1.0 } else { 0.0 },
            median_days_to_target: if init_rr >= target_daily_dollars { Some(0) } else { None },
            median_days_among_reaching: if init_rr >= target_daily_dollars { Some(0) } else { None },
            prob_dd_gte_25_pct: 0.0,
            terminal_equity_p10: starting_bankroll,
            terminal_equity_p50: starting_bankroll,
            terminal_equity_p90: starting_bankroll,
            terminal_daily_dollars_p10: init_rr,
            terminal_daily_dollars_p50: init_rr,
            terminal_daily_dollars_p90: init_rr,
            curve,
        };
    }

    // Store equity & daily run-rate for every (day, path) in flat buffers.
    let stride = paths;
    let total_days = PROJECTION_DAYS + 1;
    let mut eq_grid = vec![0.0f64; total_days * stride];
    let mut rr_grid = vec![0.0f64; total_days * stride];

    let mut reach_days: Vec<usize> = Vec::with_capacity(paths);
    let mut dd25_hits = 0usize;
    let mut rng = Xoshiro256PP::from_seed(seed);
    let pool_len = day_pool.len();

    let init_rr = expected_daily_run_rate(starting_bankroll);

    for p in 0..paths {
        let mut eq = starting_bankroll;
        let mut peak = starting_bankroll;
        let mut max_dd = 0.0f64;
        let mut first_reach: Option<usize> = if init_rr >= target_daily_dollars {
            Some(0)
        } else {
            None
        };

        eq_grid[p] = eq;
        rr_grid[p] = init_rr;

        for day in 1..=PROJECTION_DAYS {
            if eq > 1e-6 {
                let sample = day_pool[rng.next_usize(pool_len)];
                let p_ref = sample.avg_price.clamp(0.01, 0.99);
                let desired_ct = (eq * active_window_risk_fraction / p_ref).floor();
                let contracts = desired_ct.min(max_ct).max(0.0);
                // Prevent a single day from losing more than current equity on paper.
                let day_pnl = contracts * sample.sum_pnl_1ct;
                let max_affordable_loss = eq;
                let clamped_pnl = if day_pnl < -max_affordable_loss {
                    -max_affordable_loss
                } else {
                    day_pnl
                };
                eq = (eq + clamped_pnl).max(0.0);
            }
            if eq > peak {
                peak = eq;
            }
            if peak > 1e-9 {
                let dd = (peak - eq) / peak;
                if dd > max_dd {
                    max_dd = dd;
                }
            }
            let rr = expected_daily_run_rate(eq);
            if first_reach.is_none() && rr >= target_daily_dollars {
                first_reach = Some(day);
            }
            eq_grid[day * stride + p] = eq;
            rr_grid[day * stride + p] = rr;
        }

        if let Some(d) = first_reach {
            reach_days.push(d);
        }
        if max_dd >= 0.25 {
            dd25_hits += 1;
        }
    }

    let mut curve = Vec::with_capacity(total_days);
    for day in 0..=PROJECTION_DAYS {
        let eq_slice = &mut eq_grid[day * stride..(day + 1) * stride];
        let rr_slice = &mut rr_grid[day * stride..(day + 1) * stride];
        eq_slice.sort_unstable_by(|a, b| a.total_cmp(b));
        rr_slice.sort_unstable_by(|a, b| a.total_cmp(b));
        curve.push(CompoundingDayPoint {
            day,
            equity_p10: percentile_sorted(eq_slice, 0.10),
            equity_p50: percentile_sorted(eq_slice, 0.50),
            equity_p90: percentile_sorted(eq_slice, 0.90),
            daily_run_rate_p10: percentile_sorted(rr_slice, 0.10),
            daily_run_rate_p50: percentile_sorted(rr_slice, 0.50),
            daily_run_rate_p90: percentile_sorted(rr_slice, 0.90),
        });
    }

    let prob_reach = reach_days.len() as f64 / paths as f64;
    reach_days.sort_unstable();
    let median_among = if reach_days.is_empty() {
        None
    } else {
        Some(reach_days[reach_days.len() / 2])
    };
    // Overall median days to target is defined ONLY when >= 50% of paths reach it.
    let median_all = if reach_days.len() * 2 >= paths && !reach_days.is_empty() {
        Some(reach_days[paths / 2])
    } else {
        None
    };

    let last = curve.last().copied().unwrap();

    CompoundingProjection {
        mode_label: mode_label.to_string(),
        is_empirical,
        assumed_edge_cents,
        seed,
        paths,
        horizon_days: PROJECTION_DAYS,
        starting_bankroll,
        target_daily_dollars,
        prob_reach_target: prob_reach,
        median_days_to_target: median_all,
        median_days_among_reaching: median_among,
        prob_dd_gte_25_pct: dd25_hits as f64 / paths as f64,
        terminal_equity_p10: last.equity_p10,
        terminal_equity_p50: last.equity_p50,
        terminal_equity_p90: last.equity_p90,
        terminal_daily_dollars_p10: last.daily_run_rate_p10,
        terminal_daily_dollars_p50: last.daily_run_rate_p50,
        terminal_daily_dollars_p90: last.daily_run_rate_p90,
        curve,
    }
}

/// Build a deterministic pool of `DAY_POOL_SIZE` empirical trading days by
/// bootstrapping `trades_per_day` actual OOS trade payoffs per day.
fn build_empirical_day_pool(
    oos_trades: &[(f64, f64, f64)], // (entry_price, cost, net_pnl_1ct)
    windows_per_day: usize,
    seed: u64,
) -> Vec<DayPayoffSample> {
    if oos_trades.is_empty() || windows_per_day == 0 {
        return Vec::new();
    }
    let mut rng = Xoshiro256PP::from_seed(seed ^ 0xA5A5_5A5A_1234_5678);
    let n = oos_trades.len();
    let mut pool = Vec::with_capacity(DAY_POOL_SIZE);
    let k_f = windows_per_day as f64;
    for _ in 0..DAY_POOL_SIZE {
        let mut sum_pnl = 0.0f64;
        let mut sum_cost = 0.0f64;
        let mut sum_price = 0.0f64;
        for _ in 0..windows_per_day {
            let (price, cost, pnl) = oos_trades[rng.next_usize(n)];
            sum_pnl += pnl;
            sum_cost += cost;
            sum_price += price;
        }
        pool.push(DayPayoffSample {
            sum_pnl_1ct: sum_pnl,
            avg_cost: sum_cost / k_f,
            avg_price: sum_price / k_f,
        });
    }
    pool
}

/// Build a deterministic sensitivity pool for an explicitly assumed edge in
/// cents per contract around `avg_price` (cleanly labeled as non-empirical).
fn build_sensitivity_day_pool(
    avg_price: f64,
    slippage_dollars: f64,
    assumed_edge_cents: f64,
    windows_per_day: usize,
    seed: u64,
) -> (Vec<DayPayoffSample>, f64) {
    let price = avg_price.clamp(0.05, 0.95);
    let f_ct = fee(price);
    let cost = (price + f_ct + slippage_dollars).min(0.995);
    let win_prob = (cost + assumed_edge_cents / 100.0).clamp(0.01, 0.99);
    let expected_edge = win_prob - cost;
    if windows_per_day == 0 {
        return (Vec::new(), expected_edge);
    }

    let mut rng = Xoshiro256PP::from_seed(seed ^ 0x5EED_CAFE_BEEF_0600);
    let mut pool = Vec::with_capacity(DAY_POOL_SIZE);
    for _ in 0..DAY_POOL_SIZE {
        let mut wins = 0usize;
        for _ in 0..windows_per_day {
            if rng.next_f64() < win_prob {
                wins += 1;
            }
        }
        let sum_pnl = (wins as f64) - (windows_per_day as f64) * cost;
        pool.push(DayPayoffSample {
            sum_pnl_1ct: sum_pnl,
            avg_cost: cost,
            avg_price: price,
        });
    }
    (pool, expected_edge)
}

// ---------------------------------------------------------------------------
// Core Scale Plan Computation
// ---------------------------------------------------------------------------

/// Compute a full `$500/day` paper capital scaling plan, liquidity ceiling,
/// 180-day Monte Carlo compounding trajectory, and 5-gate readiness scorecard
/// from chronologically ordered trade observations.
pub fn compute_scale_plan_from_observations(
    obs: &[TimedCalibrationObs],
    max_drawdown_pct: f64,
    windows_settled_total: usize,
    observed_series: &[String],
    strategy_scope: &str,
    raw_req: &ScalePlanRequest,
) -> ScalePlanResponse {
    compute_scale_plan_from_observations_with_depth(
        obs,
        max_drawdown_pct,
        windows_settled_total,
        observed_series,
        strategy_scope,
        raw_req,
        None,
    )
}

/// Core scaling engine with optional observed median top-of-book depth from the collector.
pub fn compute_scale_plan_from_observations_with_depth(
    obs: &[TimedCalibrationObs],
    max_drawdown_pct: f64,
    windows_settled_total: usize,
    observed_series: &[String],
    strategy_scope: &str,
    raw_req: &ScalePlanRequest,
    observed_median_depth: Option<f64>,
) -> ScalePlanResponse {
    let req = raw_req.sanitized();
    let slip_dollars = req.slippage_cents / 100.0;
    let max_slot_risk = req.max_slot_risk_pct / 100.0;

    // Chronological split (first 60% IS, last 40% OOS) matching recalibration.rs.
    let recal = run_recalibration_study(strategy_scope, obs, req.slippage_cents);
    let oos_ece = recal.oos_before.ece;

    let mut ordered: Vec<&TimedCalibrationObs> = obs
        .iter()
        .filter(|o| o.raw_prob.is_finite() && o.entry_price.is_finite())
        .collect();
    ordered.sort_by(|a, b| {
        a.close_epoch
            .cmp(&b.close_epoch)
            .then_with(|| a.ticker.cmp(&b.ticker))
    });

    let total_trades = ordered.len();
    let oos_slice: &[&TimedCalibrationObs] = match recal.split_epoch {
        Some(boundary) if recal.oos_count > 0 => {
            let cut = ordered.partition_point(|o| o.close_epoch < boundary);
            &ordered[cut..]
        }
        _ => &[],
    };
    let oos_trades_count = oos_slice.len();

    // Distinct settled epochs in OOS to compute participation and concurrent slot count.
    let mut oos_traded_windows: BTreeSet<(&str, i64)> = BTreeSet::new();
    let mut oos_payoffs: Vec<(f64, f64, f64)> = Vec::with_capacity(oos_trades_count);
    let mut oos_wins = 0usize;
    let mut oos_price_sum = 0.0f64;
    let mut oos_fee_sum = 0.0f64;
    let mut oos_net_pnl_sum = 0.0f64;
    let mut oos_roi_sum = 0.0f64;

    for o in oos_slice {
        let price = o.entry_price.clamp(0.01, 0.99);
        let f_ct = fee(price);
        let cost = (price + f_ct + slip_dollars).min(1.0);
        let payoff = if o.outcome { 1.0 } else { 0.0 };
        let net_pnl = payoff - price - f_ct - slip_dollars;
        let roi = if cost > 1e-9 { net_pnl / cost } else { 0.0 };

        oos_traded_windows.insert((o.ticker.as_str(), o.close_epoch));
        oos_payoffs.push((price, cost, net_pnl));
        if o.outcome {
            oos_wins += 1;
        }
        oos_price_sum += price;
        oos_fee_sum += f_ct;
        oos_net_pnl_sum += net_pnl;
        oos_roi_sum += roi;
    }

    let windows_settled_oos = if windows_settled_total >= 2 {
        (windows_settled_total - ((windows_settled_total * 3) / 5).max(1)).max(oos_traded_windows.len())
    } else {
        oos_traded_windows.len()
    };

    let traded_windows_rate = if windows_settled_oos > 0 {
        (oos_traded_windows.len() as f64 / windows_settled_oos as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let avg_slots_per_traded_window = if !oos_traded_windows.is_empty() {
        (oos_trades_count as f64 / oos_traded_windows.len() as f64).clamp(1.0, 12.0)
    } else {
        1.0
    };

    let oos_win_rate = if oos_trades_count > 0 {
        Some(oos_wins as f64 / oos_trades_count as f64)
    } else {
        None
    };
    let oos_avg_entry_price = if oos_trades_count > 0 {
        Some(oos_price_sum / oos_trades_count as f64)
    } else {
        None
    };
    let oos_avg_fee_cents = if oos_trades_count > 0 {
        Some((oos_fee_sum / oos_trades_count as f64) * 100.0)
    } else {
        None
    };
    let oos_net_expectancy_dollars = if oos_trades_count > 0 {
        Some(oos_net_pnl_sum / oos_trades_count as f64)
    } else {
        None
    };
    let oos_net_expectancy_cents = oos_net_expectancy_dollars.map(|v| v * 100.0);
    let oos_return_on_risk_pct = if oos_trades_count > 0 {
        Some((oos_roi_sum / oos_trades_count as f64) * 100.0)
    } else {
        None
    };

    let ref_price = oos_avg_entry_price.unwrap_or(0.70).clamp(0.05, 0.95);
    let ref_cost = (ref_price + fee(ref_price) + slip_dollars).min(0.995);

    // Exact binary-contract Kelly fraction: f* = (w - c) / (1 - c) when w > c.
    let full_kelly_fraction = match (oos_win_rate, oos_net_expectancy_dollars) {
        (Some(w), Some(ev)) if ev > 0.0 && ref_cost < 0.999 => {
            let f_star = ((w - ref_cost) / (1.0 - ref_cost)).clamp(0.0, 1.0);
            finite_opt(f_star)
        }
        (Some(_), Some(_)) => Some(0.0),
        _ => None,
    };

    let f_equal = max_slot_risk;
    let f_qkelly = full_kelly_fraction
        .map(|fk| (0.25 * fk).min(max_slot_risk))
        .unwrap_or(0.0);
    let f_hkelly = full_kelly_fraction
        .map(|fk| (0.50 * fk).min(max_slot_risk))
        .unwrap_or(0.0);

    let active_risk_fraction = match req.sizing_mode.as_str() {
        "quarter_kelly" => f_qkelly,
        "half_kelly" => f_hkelly,
        _ => f_equal,
    };
    let active_window_risk_fraction =
        (active_risk_fraction * avg_slots_per_traded_window).min(max_slot_risk * 4.0).min(0.50);

    let expected_traded_windows_per_series_per_day =
        traded_windows_rate * WINDOWS_PER_DAY_PER_SERIES;
    let expected_traded_windows_portfolio_per_day =
        expected_traded_windows_per_series_per_day * (req.active_series_count as f64);

    // Current empirical daily dollars at starting_bankroll (honestly negative if edge < 0!).
    let current_empirical_daily_dollars = match oos_net_expectancy_dollars {
        Some(ev_ct) if expected_traded_windows_portfolio_per_day > 0.0 => {
            let sizing_f = if active_window_risk_fraction > 0.0 {
                active_window_risk_fraction
            } else {
                // Even if Kelly recommends 0 on negative edge, report what equal-slots
                // would realize if forced under equal_slots, or 0.0 under Kelly refusal!
                if req.sizing_mode == "equal_slots" {
                    f_equal
                } else {
                    0.0
                }
            };
            let contracts = (req.starting_bankroll * sizing_f / ref_price)
                .floor()
                .min(req.max_contracts_per_window as f64);
            expected_traded_windows_portfolio_per_day * contracts * ev_ct
        }
        _ => 0.0,
    };

    // Required bankroll across all 3 sizing modes — STRICTLY `None` when OOS edge <= 0!
    let (
        required_bankroll_equal_slots,
        required_bankroll_quarter_kelly,
        required_bankroll_half_kelly,
        required_bankroll_reason,
    ) = match oos_net_expectancy_dollars {
        None => (
            None,
            None,
            None,
            Some("No out-of-sample trades on tape yet; cannot estimate required bankroll without held-out settlement evidence.".to_string()),
        ),
        Some(ev_ct) if ev_ct <= 1e-9 => (
            None,
            None,
            None,
            Some(format!(
                "Out-of-sample net expectancy is {:+.2}c/contract after fees and {:.1}c slippage (<= 0). Scaling capital cannot turn non-positive edge into ${:.0}/day.",
                ev_ct * 100.0,
                req.slippage_cents,
                req.target_daily_dollars
            )),
        ),
        Some(_) if expected_traded_windows_portfolio_per_day <= 1e-9 => (
            None,
            None,
            None,
            Some("Measured participation rate is zero out of sample; cannot project daily throughput.".to_string()),
        ),
        Some(ev_ct) => {
            let req_contracts_per_win =
                req.target_daily_dollars / (expected_traded_windows_portfolio_per_day * ev_ct);
            let req_notional_per_win = req_contracts_per_win * ref_price;
            let solve = |slot_f: f64| -> Option<f64> {
                let win_f = (slot_f * avg_slots_per_traded_window)
                    .min(max_slot_risk * 4.0)
                    .min(0.50);
                if win_f > 1e-9 {
                    finite_opt(req_notional_per_win / win_f)
                } else {
                    None
                }
            };
            (
                solve(f_equal),
                solve(f_qkelly),
                solve(f_hkelly),
                None,
            )
        }
    };

    // Liquidity ceiling & multi-series capacity.
    let mut series_names: Vec<String> = Vec::with_capacity(req.active_series_count);
    for s in observed_series {
        if !s.is_empty() && !series_names.contains(s) && series_names.len() < req.active_series_count
        {
            series_names.push(s.clone());
        }
    }
    for &canon in &CANONICAL_15M_SERIES {
        if series_names.len() >= req.active_series_count {
            break;
        }
        if !series_names.iter().any(|x| x == canon) {
            series_names.push(canon.to_string());
        }
    }

    let single_series_max_daily_contracts =
        expected_traded_windows_per_series_per_day * (req.max_contracts_per_window as f64);
    let multi_series_max_daily_contracts =
        single_series_max_daily_contracts * (req.active_series_count as f64);
    let positive_ev_ct = oos_net_expectancy_dollars.unwrap_or(0.0).max(0.0);
    let single_series_max_daily_dollars = single_series_max_daily_contracts * positive_ev_ct;
    let multi_series_max_daily_dollars = multi_series_max_daily_contracts * positive_ev_ct;
    let single_series_max_notional_per_window = (req.max_contracts_per_window as f64) * ref_price;

    let required_contracts_per_window_at_target = match oos_net_expectancy_dollars {
        Some(ev_ct) if ev_ct > 1e-9 && expected_traded_windows_portfolio_per_day > 1e-9 => {
            finite_opt(req.target_daily_dollars / (expected_traded_windows_portfolio_per_day * ev_ct))
        }
        _ => None,
    };

    let fill_ratio_at_target = if positive_ev_ct <= 1e-9 || expected_traded_windows_portfolio_per_day <= 1e-9
    {
        0.0
    } else {
        (multi_series_max_daily_dollars / req.target_daily_dollars).clamp(0.0, 1.0)
    };
    let liquidity_constrained_at_target = fill_ratio_at_target < 0.999_999;

    let clean_obs_depth = observed_median_depth.filter(|d| d.is_finite() && *d > 0.0);
    let mut liquidity_note = if positive_ev_ct <= 1e-9 {
        "Liquidity capacity is $0/day because OOS net expectancy after fees and slippage is non-positive.".to_string()
    } else if liquidity_constrained_at_target {
        format!(
            "At {} contracts/window across {} series ({:.0} traded windows/day), book capacity caps daily paper EV at ${:.2}/day ({:.1}% of ${:.0}/day target). Increase active series or depth limit.",
            req.max_contracts_per_window,
            req.active_series_count,
            expected_traded_windows_portfolio_per_day,
            multi_series_max_daily_dollars,
            fill_ratio_at_target * 100.0,
            req.target_daily_dollars
        )
    } else {
        format!(
            "Target ${:.0}/day needs {:.1} contracts/window across {} series ({:.1}% of the {} contract/window cap).",
            req.target_daily_dollars,
            required_contracts_per_window_at_target.unwrap_or(0.0),
            req.active_series_count,
            (required_contracts_per_window_at_target.unwrap_or(0.0) / req.max_contracts_per_window as f64) * 100.0,
            req.max_contracts_per_window
        )
    };
    if let Some(depth) = clean_obs_depth {
        if (req.max_contracts_per_window as f64) > depth {
            liquidity_note.push_str(&format!(
                " Caution: configured {} contracts/window exceeds observed median top-of-book depth ({:.0}).",
                req.max_contracts_per_window, depth
            ));
        }
    }

    let liquidity_ceiling = LiquidityCeiling {
        windows_per_day_per_series: WINDOWS_PER_DAY_PER_SERIES,
        max_contracts_per_window: req.max_contracts_per_window,
        active_series_count: req.active_series_count,
        series_names,
        expected_traded_windows_per_series_per_day,
        single_series_max_daily_contracts,
        multi_series_max_daily_contracts,
        single_series_max_notional_per_window,
        single_series_max_daily_dollars,
        multi_series_max_daily_dollars,
        required_contracts_per_window_at_target,
        fill_ratio_at_target,
        liquidity_constrained_at_target,
        observed_median_depth: clean_obs_depth,
        note: liquidity_note,
    };

    // 180-trading-day seeded 1,000-path Monte Carlo compounding projection (Empirical Tape).
    let sim_windows_per_day = expected_traded_windows_portfolio_per_day
        .round()
        .clamp(0.0, WINDOWS_PER_DAY_PER_SERIES * 8.0) as usize;
    let emp_pool = build_empirical_day_pool(&oos_payoffs, sim_windows_per_day, req.seed);
    let sim_risk_f = if active_window_risk_fraction > 0.0 {
        active_window_risk_fraction
    } else if req.sizing_mode == "equal_slots" {
        f_equal
    } else {
        0.0
    };
    let compounding_projection = run_180_day_compounding(
        "Empirical Tape",
        true,
        None,
        &emp_pool,
        oos_net_expectancy_dollars.unwrap_or(0.0),
        expected_traded_windows_portfolio_per_day,
        ref_price,
        sim_risk_f,
        req.max_contracts_per_window,
        req.starting_bankroll,
        req.target_daily_dollars,
        req.paths,
        req.seed,
    );

    // Optional what-if sensitivity projection — ALWAYS cleanly labeled `"Sensitivity (Assumed Edge)"`.
    let sensitivity_projection = req.assumed_edge_cents.map(|assumed_c| {
        let sens_windows = if sim_windows_per_day > 0 {
            sim_windows_per_day
        } else {
            (req.active_series_count * 48).min(768)
        };
        let (sens_pool, sens_ev) = build_sensitivity_day_pool(
            ref_price,
            slip_dollars,
            assumed_c,
            sens_windows,
            req.seed,
        );
        let sens_kelly = if sens_ev > 0.0 && ref_cost < 0.999 {
            (sens_ev / (1.0 - ref_cost)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let sens_slot_f = match req.sizing_mode.as_str() {
            "quarter_kelly" => (0.25 * sens_kelly).min(max_slot_risk),
            "half_kelly" => (0.50 * sens_kelly).min(max_slot_risk),
            _ => max_slot_risk,
        };
        let sens_win_f = (sens_slot_f * avg_slots_per_traded_window)
            .min(max_slot_risk * 4.0)
            .min(0.50);
        run_180_day_compounding(
            "Sensitivity (Assumed Edge)",
            false,
            Some(assumed_c),
            &sens_pool,
            sens_ev,
            sens_windows as f64,
            ref_price,
            sens_win_f,
            req.max_contracts_per_window,
            req.starting_bankroll,
            req.target_daily_dollars,
            req.paths,
            req.seed,
        )
    });

    // 5 mechanical readiness gates.
    let g_sample = total_trades >= 200;
    let g_expectancy = oos_net_expectancy_cents.is_some_and(|c| c > 0.0);
    let g_ece = oos_ece.is_some_and(|e| e < 0.05);
    let g_dd = total_trades > 0 && max_drawdown_pct.is_finite() && max_drawdown_pct < 20.0;
    let g_liq = fill_ratio_at_target >= 0.75;

    let gates = vec![
        ReadinessGate {
            id: "sample_depth".to_string(),
            label: "Sample depth >= 200 settled trades".to_string(),
            passed: g_sample,
            observed: Some(total_trades as f64),
            threshold: 200.0,
            unit: "trades".to_string(),
            detail: format!("{total_trades} settled paper trades recorded (need >= 200)."),
        },
        ReadinessGate {
            id: "oos_net_expectancy".to_string(),
            label: "OOS net expectancy > 0 after fees & slippage".to_string(),
            passed: g_expectancy,
            observed: oos_net_expectancy_cents,
            threshold: 0.0,
            unit: "cents/contract".to_string(),
            detail: match oos_net_expectancy_cents {
                Some(c) => format!(
                    "OOS net expectancy is {:+.2}c/contract across {} OOS trades (slippage {:.1}c).",
                    c, oos_trades_count, req.slippage_cents
                ),
                None => "No OOS trades available yet.".to_string(),
            },
        },
        ReadinessGate {
            id: "oos_ece".to_string(),
            label: "OOS calibration ECE < 0.05".to_string(),
            passed: g_ece,
            observed: oos_ece,
            threshold: 0.05,
            unit: "ECE".to_string(),
            detail: match oos_ece {
                Some(e) => format!("Held-out OOS Expected Calibration Error is {e:.4} (limit < 0.0500)."),
                None => "OOS calibration ECE is unavailable (no OOS sample).".to_string(),
            },
        },
        ReadinessGate {
            id: "max_drawdown_pct".to_string(),
            label: "Max paper drawdown < 20%".to_string(),
            passed: g_dd,
            observed: if total_trades > 0 { Some(max_drawdown_pct) } else { None },
            threshold: 20.0,
            unit: "%".to_string(),
            detail: if total_trades > 0 {
                format!("Measured peak-to-trough paper drawdown is {max_drawdown_pct:.2}% (limit < 20.0%).")
            } else {
                "No settled paper trades to measure drawdown.".to_string()
            },
        },
        ReadinessGate {
            id: "liquidity_fill_ratio".to_string(),
            label: "Liquidity fill ratio >= 75% at target".to_string(),
            passed: g_liq,
            observed: Some(fill_ratio_at_target),
            threshold: 0.75,
            unit: "ratio".to_string(),
            detail: format!(
                "Book capacity covers {:.1}% of ${:.0}/day target across {} series at {} contracts/window (need >= 75%).",
                fill_ratio_at_target * 100.0,
                req.target_daily_dollars,
                req.active_series_count,
                req.max_contracts_per_window
            ),
        },
    ];

    let passed_count = gates.iter().filter(|g| g.passed).count();
    let blocking_reasons: Vec<String> = gates
        .iter()
        .filter(|g| !g.passed)
        .map(|g| format!("{}: {}", g.label, g.detail))
        .collect();
    let verdict = if passed_count == gates.len() {
        "READY_FOR_OWNER_REVIEW".to_string()
    } else {
        "PAPER_RESEARCH_ONLY".to_string()
    };

    let readiness_gates = ReadinessScorecard {
        verdict,
        passed_count,
        total_gates: gates.len(),
        gates,
        blocking_reasons,
    };

    let empirical_summary = EmpiricalEdgeSummary {
        windows_settled_total,
        windows_settled_oos,
        total_trades,
        oos_trades: oos_trades_count,
        oos_win_rate,
        oos_avg_entry_price,
        oos_avg_fee_cents,
        slippage_cents: req.slippage_cents,
        oos_net_expectancy_cents,
        oos_return_on_risk_pct,
        oos_ece,
        max_drawdown_pct,
        full_kelly_fraction,
        active_risk_fraction,
        traded_windows_rate,
        avg_slots_per_traded_window,
        expected_traded_windows_per_series_per_day,
        expected_traded_windows_portfolio_per_day,
    };

    ScalePlanResponse {
        mode_notice: "Paper simulation only — strictly read-only market research, no venue capital at risk.".to_string(),
        sizing_mode_resolved: req.sizing_mode.clone(),
        strategy_scope: strategy_scope.to_string(),
        target_daily_dollars: req.target_daily_dollars,
        current_empirical_daily_dollars,
        required_bankroll_equal_slots,
        required_bankroll_quarter_kelly,
        required_bankroll_half_kelly,
        required_bankroll_reason,
        empirical_summary,
        liquidity_ceiling,
        compounding_projection,
        sensitivity_projection,
        readiness_gates,
        request: req,
    }
}

/// Pre-extracted tape context shared via `RwLock` across repeated
/// `POST /api/analytics/scale-plan` calls so slider/seed changes never
/// re-compile strategies, re-scan 100k snapshots, or re-hash 2,000 windows.
#[derive(Clone)]
pub struct ScaleTapeContext {
    pub windows_settled_total: usize,
    pub observed_series: Vec<String>,
    pub observed_median_depth: Option<f64>,
    pub portfolio_max_dd_pct: f64,
    pub portfolio_obs: Vec<TimedCalibrationObs>,
    pub by_strategy_obs: std::collections::HashMap<String, Vec<TimedCalibrationObs>>,
}

/// Build `ScaleTapeContext` once for a fixed `(tape, strategies)` state.
pub fn build_scale_tape_context(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
    sim: &SlotSimulation,
    observed_median_depth: Option<f64>,
) -> ScaleTapeContext {
    let mut windows_settled_total = 0usize;
    let mut observed_series: Vec<String> = Vec::new();
    let mut portfolio_obs: Vec<TimedCalibrationObs> = Vec::new();
    let mut by_strategy_obs: std::collections::HashMap<String, Vec<TimedCalibrationObs>> =
        std::collections::HashMap::with_capacity(strategies.len());

    for (ticker, snaps) in windows {
        let facts_fp = facts_fingerprint(snaps);
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict | WindowFacts::Unsettled => continue,
        };
        windows_settled_total += 1;
        let series = snaps.first().map(|s| s.series.clone()).unwrap_or_default();
        if !series.is_empty() && !observed_series.contains(&series) {
            observed_series.push(series.clone());
        }

        if !strategies.is_empty() {
            let win_fp = window_fingerprint(snaps);
            for strat in strategies {
                if let Some(price) = window_entry_keyed(strat, Some(ticker.as_str()), snaps, win_fp)
                {
                    if is_valid_whole_cent(price) {
                        let ob = TimedCalibrationObs {
                            close_epoch,
                            ticker: ticker.clone(),
                            series: series.clone(),
                            side: strat.side.clone(),
                            raw_prob: price,
                            entry_price: price,
                            outcome: result == strat.side,
                        };
                        portfolio_obs.push(ob.clone());
                        by_strategy_obs
                            .entry(strat.name.clone())
                            .or_default()
                            .push(ob);
                    }
                }
            }
        }
    }

    let port_trades = portfolio_trades_from_sim(sim);
    let (m, _, _) = compute_performance_metrics(&port_trades, sim.paper_pot_start);

    ScaleTapeContext {
        windows_settled_total,
        observed_series,
        observed_median_depth,
        portfolio_max_dd_pct: m.max_drawdown_pct,
        portfolio_obs,
        by_strategy_obs,
    }
}

/// Fast scale-plan evaluation over a cached `ScaleTapeContext` (zero strategy
/// compilation or snapshot scanning on parameter/seed changes).
pub fn compute_scale_plan_from_context(
    ctx: &ScaleTapeContext,
    raw_req: &ScalePlanRequest,
) -> ScalePlanResponse {
    let req = raw_req.sanitized();
    let scope = req.strategy.as_deref().unwrap_or("portfolio");
    let empty: Vec<TimedCalibrationObs> = Vec::new();
    let (obs, max_dd_pct): (&[TimedCalibrationObs], f64) = match &req.strategy {
        None => (&ctx.portfolio_obs, ctx.portfolio_max_dd_pct),
        Some(name) => {
            let slice = ctx
                .by_strategy_obs
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or(&empty);
            let risk_f = (req.max_slot_risk_pct / 100.0).clamp(0.01, 0.50);
            let mut eq = req.starting_bankroll;
            let mut peak = eq;
            let mut max_dd = 0.0f64;
            let mut ordered = slice.to_vec();
            ordered.sort_by_key(|o| o.close_epoch);
            for o in &ordered {
                if eq <= 0.0 {
                    break;
                }
                let contracts = (eq * risk_f / o.entry_price).floor() as i64;
                if contracts <= 0 {
                    continue;
                }
                let payoff = if o.outcome { contracts as f64 } else { 0.0 };
                let pnl = payoff
                    - contracts as f64 * o.entry_price
                    - contracts as f64 * fee(o.entry_price);
                eq += pnl;
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
            (slice, max_dd)
        }
    };

    compute_scale_plan_from_observations_with_depth(
        obs,
        max_dd_pct,
        ctx.windows_settled_total,
        &ctx.observed_series,
        scope,
        &req,
        ctx.observed_median_depth,
    )
}

/// Top-level desk entrypoint for `POST /api/analytics/scale-plan`.
/// Collects chronological settled strategy entries directly from the collected
/// tape windows, computes empirical drawdown from the slot simulation (or
/// selected strategy), and runs `compute_scale_plan_from_observations`.
pub fn compute_scale_plan(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
    sim: &SlotSimulation,
    raw_req: &ScalePlanRequest,
) -> ScalePlanResponse {
    compute_scale_plan_with_depth(windows, strategies, sim, raw_req, None)
}

/// Top-level desk entrypoint accepting observed median top-of-book depth from the collector.
pub fn compute_scale_plan_with_depth(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
    sim: &SlotSimulation,
    raw_req: &ScalePlanRequest,
    observed_median_depth: Option<f64>,
) -> ScalePlanResponse {
    let ctx = build_scale_tape_context(windows, strategies, sim, observed_median_depth);
    compute_scale_plan_from_context(&ctx, raw_req)
}

// ---------------------------------------------------------------------------
// Comprehensive Unit, Honesty, Sensitivity & Capacity Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{iso_z, load_strategies, simulate_slots};
    use std::path::Path;
    use std::time::Instant;

    fn make_obs(count: usize, price: f64, win_num: usize, win_den: usize) -> Vec<TimedCalibrationObs> {
        (0..count)
            .map(|i| TimedCalibrationObs {
                close_epoch: 1_767_000_000 + (i as i64) * 900,
                ticker: format!("KXBTC15M-SC{i:04}"),
                series: "KXBTC15M".to_string(),
                side: "yes".to_string(),
                raw_prob: price,
                entry_price: price,
                outcome: ((i + 1) * win_num) / win_den > (i * win_num) / win_den,
            })
            .collect()
    }

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

    #[test]
    fn negative_edge_honesty_refuses_required_bankroll_and_fails_gates() {
        // Entry at 0.70 with only 60% win rate -> strongly negative expectancy!
        let obs = make_obs(250, 0.70, 6, 10);
        let req = ScalePlanRequest {
            target_daily_dollars: 500.0,
            starting_bankroll: 1_000.0,
            slippage_cents: 1.0,
            ..ScalePlanRequest::default()
        };
        let plan = compute_scale_plan_from_observations(
            &obs,
            35.0,
            250,
            &["KXBTC15M".to_string()],
            "portfolio",
            &req,
        );

        assert!(plan.empirical_summary.oos_net_expectancy_cents.unwrap() < 0.0);
        assert!(plan.required_bankroll_equal_slots.is_none());
        assert!(plan.required_bankroll_quarter_kelly.is_none());
        assert!(plan.required_bankroll_half_kelly.is_none());
        assert!(plan.required_bankroll_reason.is_some());
        assert!(plan.current_empirical_daily_dollars < 0.0);
        assert_eq!(plan.liquidity_ceiling.fill_ratio_at_target, 0.0);
        assert_eq!(plan.readiness_gates.verdict, "PAPER_RESEARCH_ONLY");
        assert!(
            plan.readiness_gates
                .gates
                .iter()
                .any(|g| g.id == "oos_net_expectancy" && !g.passed)
        );
        assert!(plan.sensitivity_projection.is_none());
    }

    #[test]
    fn positive_calibrated_edge_passes_all_five_gates_and_scales_consistently() {
        // 250 trades at 0.70 with 74% win rate:
        // fee(0.70) = 1.47c, slippage = 1.00c -> cost = 72.47c, net EV = +1.53c/contract > 0!
        // ECE = |0.74 - 0.70| = 0.04 < 0.05!
        // Max DD = 11.2% < 20%!
        // 4 series × 96 windows/day × 250 contracts × $0.0153 = $1,468.80/day >= $500 -> fill_ratio = 1.0!
        let obs = make_obs(250, 0.70, 74, 100);
        let req = ScalePlanRequest {
            target_daily_dollars: 500.0,
            starting_bankroll: 2_500.0,
            max_slot_risk_pct: 8.0,
            slippage_cents: 1.0,
            max_contracts_per_window: 250,
            active_series_count: 4,
            sizing_mode: "quarter_kelly".to_string(),
            paths: 1_000,
            seed: 42,
            assumed_edge_cents: Some(3.0),
            strategy: None,
        };

        let t0 = Instant::now();
        let plan = compute_scale_plan_from_observations(
            &obs,
            11.2,
            250,
            &["KXBTC15M".to_string(), "KXETH15M".to_string()],
            "portfolio",
            &req,
        );
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert!(
            elapsed_ms < 500.0,
            "1,000-path 180-day empirical + sensitivity projection took {elapsed_ms:.1}ms"
        );

        assert_eq!(plan.readiness_gates.verdict, "READY_FOR_OWNER_REVIEW");
        assert_eq!(plan.readiness_gates.passed_count, 5);
        assert!(plan.readiness_gates.blocking_reasons.is_empty());

        let b_eq = plan.required_bankroll_equal_slots.unwrap();
        let b_qk = plan.required_bankroll_quarter_kelly.unwrap();
        let b_hk = plan.required_bankroll_half_kelly.unwrap();
        assert!(b_eq > 0.0 && b_qk > 0.0 && b_hk > 0.0);
        // Quarter-Kelly takes smaller risk than Half-Kelly, so requires >= bankroll.
        assert!(b_qk >= b_hk - 1e-6);

        assert_eq!(plan.liquidity_ceiling.fill_ratio_at_target, 1.0);
        assert!(plan.liquidity_ceiling.multi_series_max_daily_dollars > 500.0);

        // Empirical vs Sensitivity labels are strictly distinct.
        assert_eq!(plan.compounding_projection.mode_label, "Empirical Tape");
        assert!(plan.compounding_projection.is_empirical);
        assert_eq!(plan.compounding_projection.curve.len(), PROJECTION_DAYS + 1);
        for pt in &plan.compounding_projection.curve {
            assert!(pt.equity_p10 <= pt.equity_p50 + 1e-9);
            assert!(pt.equity_p50 <= pt.equity_p90 + 1e-9);
        }

        let sens = plan.sensitivity_projection.as_ref().unwrap();
        assert_eq!(sens.mode_label, "Sensitivity (Assumed Edge)");
        assert!(!sens.is_empirical);
        assert_eq!(sens.assumed_edge_cents, Some(3.0));

        // Deterministic repeatability on identical seed.
        let plan2 = compute_scale_plan_from_observations(
            &obs,
            11.2,
            250,
            &["KXBTC15M".to_string(), "KXETH15M".to_string()],
            "portfolio",
            &req,
        );
        assert_eq!(plan.compounding_projection.curve, plan2.compounding_projection.curve);
        assert_eq!(sens.curve, plan2.sensitivity_projection.as_ref().unwrap().curve);
    }

    #[test]
    fn thin_liquidity_fails_fill_ratio_gate_even_with_positive_edge() {
        // Same positive edge (+1.53c/ct), but only 1 series and 50 contracts/window:
        // max daily capacity = 96 * 50 * $0.0153 = $73.44 -> fill_ratio = 14.7% < 75%!
        let obs = make_obs(250, 0.70, 74, 100);
        let req = ScalePlanRequest {
            target_daily_dollars: 500.0,
            max_contracts_per_window: 50,
            active_series_count: 1,
            paths: 200,
            ..ScalePlanRequest::default()
        };
        let plan = compute_scale_plan_from_observations(
            &obs,
            10.0,
            250,
            &["KXBTC15M".to_string()],
            "portfolio",
            &req,
        );

        assert!(plan.liquidity_ceiling.fill_ratio_at_target < 0.25);
        assert!(plan.liquidity_ceiling.liquidity_constrained_at_target);
        assert_eq!(plan.readiness_gates.verdict, "PAPER_RESEARCH_ONLY");
        assert!(
            plan.readiness_gates
                .gates
                .iter()
                .any(|g| g.id == "liquidity_fill_ratio" && !g.passed)
        );
    }

    #[test]
    fn desk_scale_plan_integrates_with_12_strategies_and_emits_clean_json() {
        let strategies = load_strategies(Path::new("strategies"));
        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        for i in 0..60usize {
            let close_epoch = 1_768_000_000 + (i as i64) * 900;
            let close_iso = iso_z(close_epoch);
            let ticker = format!("KXBTC15M-DK{i:03}");
            let res = if i % 5 == 4 { "no" } else { "yes" };
            windows.insert(
                ticker.clone(),
                vec![
                    snap_json(&ticker, "KXBTC15M", close_epoch - 240, &close_iso, "", Some(0.70), Some(0.32)),
                    snap_json(&ticker, "KXBTC15M", close_epoch + 60, &close_iso, res, None, None),
                ],
            );
        }
        let snaps: Vec<Snapshot> = windows.values().flatten().cloned().collect();
        let sim = simulate_slots(&strategies, &snaps, 100.0);

        let req = ScalePlanRequest {
            paths: 250,
            assumed_edge_cents: Some(2.5),
            ..ScalePlanRequest::default()
        };
        let plan = compute_scale_plan(&windows, &strategies, &sim, &req);
        assert_eq!(plan.empirical_summary.windows_settled_total, 60);
        // Underpowered (< 200 settled trades if single strategy, or check JSON hygiene).
        let json = serde_json::to_string(&plan).unwrap();
        assert!(!json.contains("NaN") && !json.contains("Infinity"));
        assert!(!json.to_ascii_lowercase().contains("rune"));

        // Single-strategy filter works cleanly.
        let req_single = ScalePlanRequest {
            strategy: Some("favourite-late".to_string()),
            paths: 100,
            ..ScalePlanRequest::default()
        };
        let plan_single = compute_scale_plan(&windows, &strategies, &sim, &req_single);
        assert_eq!(plan_single.strategy_scope, "favourite-late");
        assert_eq!(plan_single.empirical_summary.total_trades, 60);
        assert_eq!(plan_single.readiness_gates.verdict, "PAPER_RESEARCH_ONLY");
    }

    #[test]
    fn every_readiness_gate_individually_blocks_and_hostile_inputs_clamp_safely() {
        let base_obs = make_obs(250, 0.70, 74, 100);
        let base_req = ScalePlanRequest {
            target_daily_dollars: 500.0,
            starting_bankroll: 2_500.0,
            max_slot_risk_pct: 8.0,
            slippage_cents: 1.0,
            max_contracts_per_window: 250,
            active_series_count: 4,
            sizing_mode: "equal_slots".to_string(),
            paths: 200,
            seed: 9,
            assumed_edge_cents: None,
            strategy: None,
        };
        let series = vec!["KXBTC15M".to_string()];

        // 1. Failing sample_depth (< 200) alone blocks verdict.
        let p_depth = compute_scale_plan_from_observations(
            &make_obs(150, 0.70, 74, 100),
            10.0,
            150,
            &series,
            "portfolio",
            &base_req,
        );
        assert_eq!(p_depth.readiness_gates.verdict, "PAPER_RESEARCH_ONLY");
        assert_eq!(p_depth.readiness_gates.passed_count, 4);
        assert!(p_depth.readiness_gates.gates.iter().any(|g| g.id == "sample_depth" && !g.passed));

        // 2. Failing oos_ece (>= 0.05) alone blocks verdict (70c price winning 80% -> ECE = 0.10).
        let p_ece = compute_scale_plan_from_observations(
            &make_obs(250, 0.70, 80, 100),
            10.0,
            250,
            &series,
            "portfolio",
            &base_req,
        );
        assert_eq!(p_ece.readiness_gates.verdict, "PAPER_RESEARCH_ONLY");
        assert_eq!(p_ece.readiness_gates.passed_count, 4);
        assert!(p_ece.readiness_gates.gates.iter().any(|g| g.id == "oos_ece" && !g.passed));

        // 3. Failing max_drawdown_pct (>= 20%) alone blocks verdict.
        let p_dd = compute_scale_plan_from_observations(
            &base_obs,
            24.5,
            250,
            &series,
            "portfolio",
            &base_req,
        );
        assert_eq!(p_dd.readiness_gates.verdict, "PAPER_RESEARCH_ONLY");
        assert_eq!(p_dd.readiness_gates.passed_count, 4);
        assert!(p_dd.readiness_gates.gates.iter().any(|g| g.id == "max_drawdown_pct" && !g.passed));

        // 4. Observed median depth caution surfaced cleanly.
        let p_cap = compute_scale_plan_from_observations_with_depth(
            &base_obs,
            10.0,
            250,
            &series,
            "portfolio",
            &base_req,
            Some(85.0),
        );
        assert_eq!(p_cap.liquidity_ceiling.observed_median_depth, Some(85.0));
        assert!(p_cap.liquidity_ceiling.note.contains("observed median top-of-book depth"));

        // 5. Hostile / NaN / Inf / zero request inputs sanitize cleanly with zero panics or NaNs.
        let hostile_req = ScalePlanRequest {
            target_daily_dollars: f64::NAN,
            starting_bankroll: -500.0,
            max_slot_risk_pct: f64::INFINITY,
            slippage_cents: 99.0,
            max_contracts_per_window: 0,
            active_series_count: 0,
            sizing_mode: "invalid_mode".to_string(),
            paths: 999_999,
            seed: 0,
            assumed_edge_cents: Some(f64::NAN),
            strategy: Some("   ".to_string()),
        };
        let p_hostile = compute_scale_plan_from_observations(
            &[],
            f64::NAN,
            0,
            &[],
            "portfolio",
            &hostile_req,
        );
        assert_eq!(p_hostile.sizing_mode_resolved, "equal_slots");
        assert_eq!(p_hostile.request.max_contracts_per_window, 1);
        assert_eq!(p_hostile.request.active_series_count, 1);
        assert_eq!(p_hostile.request.slippage_cents, 5.0);
        assert!(p_hostile.sensitivity_projection.is_none());
        let json = serde_json::to_string(&p_hostile).unwrap();
        assert!(!json.contains("NaN") && !json.contains("Infinity"));
    }
}
