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
        }
    }
}

/// One strategy passing OOS depth, positive-EV, and correlation gates.
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
    pub oos_net_expectancy_cents: f64,
    pub full_kelly_pct: f64,
    pub allocated_pct: f64,
    pub allocated_dollars: f64,
    pub rail_clamped_contracts: u32,
    pub max_abs_corr: Option<f64>,
    pub status: String,
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
    pub windows_settled_total: usize,
    pub windows_settled_oos: usize,
    pub live_armed: bool,
    pub total_allocated_pct: f64,
    pub cash_reserve_pct: f64,
    pub active_winners: Vec<AllocatedWinner>,
    pub quarantined: Vec<QuarantinedStrategy>,
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
    net_expectancy_cents: f64,
    full_kelly_pct: f64,
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
        let mut pnl_1ct_sum = 0.0f64;
        let mut returns_by_win: HashMap<(i64, String), f64> = HashMap::with_capacity(n);

        for e in list {
            let p = e.entry_price.clamp(0.01, 0.99);
            let tf = fee(p);
            let mf = maker_fee(p);
            let af = if is_maker { mf } else { tf };
            let payoff = if e.won { 1.0 } else { 0.0 };
            let net_pnl = payoff - p - af - slip_dollars;
            let cost = (p + af + slip_dollars).min(1.0);
            let ret = if cost > 1e-9 { net_pnl / cost } else { 0.0 };

            if e.won {
                wins += 1;
            }
            price_sum += p;
            taker_fee_sum += tf;
            maker_fee_sum += mf;
            pnl_1ct_sum += net_pnl;
            returns_by_win.insert((e.close_epoch, e.ticker.clone()), finite_or(ret, 0.0));
        }

        let n_f = n as f64;
        let win_rate = finite_or(wins as f64 / n_f, 0.0);
        let avg_entry_price = finite_or((price_sum / n_f).clamp(0.01, 0.99), 0.50);
        let taker_fee_cents = finite_or((taker_fee_sum / n_f) * 100.0, 0.0);
        let maker_fee_cents = finite_or((maker_fee_sum / n_f) * 100.0, 0.0);
        let exp_cents = finite_or((pnl_1ct_sum / n_f) * 100.0, 0.0);
        expectancy_by_idx.insert(si, exp_cents);

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
            net_expectancy_cents: exp_cents,
            full_kelly_pct,
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
    let total_allocated_pct = if n_winners == 0 {
        0.0
    } else {
        finite_or(oos_sel.weights_pct.iter().sum::<f64>().clamp(0.0, 100.0), 0.0)
    };
    let cash_reserve_pct = finite_or((100.0 - total_allocated_pct).clamp(0.0, 100.0), 100.0);

    let mut active_winners = Vec::with_capacity(n_winners);
    let mut planned_intents = Vec::with_capacity(n_winners);
    let mut oos_weights_by_idx: HashMap<usize, f64> = HashMap::with_capacity(n_winners);

    for (i, cand) in oos_sel.accepted.iter().enumerate() {
        let alloc_pct = finite_or(oos_sel.weights_pct[i], 0.0);
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

        oos_weights_by_idx.insert(cand.strategy_idx, alloc_pct / 100.0);
        let is_exp = is_sel
            .as_ref()
            .and_then(|s| s.expectancy_by_idx.get(&cand.strategy_idx).copied())
            .and_then(finite_opt);

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
            oos_net_expectancy_cents: cand.net_expectancy_cents,
            full_kelly_pct: cand.full_kelly_pct,
            allocated_pct: alloc_pct,
            allocated_dollars: alloc_dollars,
            rail_clamped_contracts,
            max_abs_corr: oos_sel.max_corrs[i],
            status: "ACTIVE_WINNER".to_string(),
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
        None => (oos_weights_by_idx, "oos_only_no_is_history".to_string()),
    };

    let comparison = replay_oos_comparison(
        ctx,
        &req,
        &oos_by_epoch,
        &replay_weights,
        is_maker,
        selection_basis,
    );

    // Compute $500/day net expectancy readiness verdict with concrete blockers.
    let daily_target = compute_daily_target_verdict(
        ctx,
        &req,
        &active_winners,
        &comparison,
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

    AutoAllocatorReport {
        execution_mode: req.execution_mode,
        sizing_mode: req.sizing_mode,
        maker_fill_rate: req.maker_fill_rate,
        min_oos_trades: req.min_oos_trades,
        max_slot_risk_pct: req.max_slot_risk_pct,
        max_total_heat_pct: req.max_total_heat_pct,
        starting_bankroll: req.starting_bankroll,
        windows_settled_total: ctx.windows_settled_total,
        windows_settled_oos: ctx.windows_settled_oos,
        live_armed,
        total_allocated_pct,
        cash_reserve_pct,
        active_winners,
        quarantined: oos_sel.quarantined,
        comparison,
        planned_intents,
        daily_target,
        honesty_notes,
    }
}

fn compute_daily_target_verdict(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    winners: &[AllocatedWinner],
    comparison: &AllocatorComparison,
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
        required_bankroll_dollars,
        portfolio_net_expectancy_cents: weighted_ev_cents,
        blockers,
    }
}

fn replay_oos_comparison(
    ctx: &TournamentTapeContext,
    req: &AutoAllocatorRequest,
    oos_by_epoch: &BTreeMap<i64, Vec<&TournamentEntry>>,
    winner_weights_by_idx: &HashMap<usize, f64>,
    is_maker: bool,
    selection_basis: String,
) -> AllocatorComparison {
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
    let mut alloc_fees = 0.0f64;

    let mut curve: Vec<ComparisonPoint> = Vec::with_capacity(oos_by_epoch.len());

    for (&close_epoch, entries) in oos_by_epoch {
        let prev_legacy = legacy_eq;
        let prev_alloc = alloc_eq;
        let mut legacy_epoch_pnl = 0.0f64;
        let mut alloc_epoch_pnl = 0.0f64;

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
                            let contracts = ((prev_alloc * weight) / p).floor().max(0.0) as u32;
                            if contracts > 0 {
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
            let dd = (((alloc_peak - alloc_eq).max(0.0) / alloc_peak) * 100.0).clamp(0.0, 100.0);
            if dd > alloc_max_dd_pct {
                alloc_max_dd_pct = dd;
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
}
