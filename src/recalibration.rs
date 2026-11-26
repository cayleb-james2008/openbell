//! Monotone Pool-Adjacent-Violators (PAV) Isotonic Recalibration &
//! EV-After-Fees Entry Gate Engine (strictly paper-only analysis).
//!
//! Grounded in strict chronological out-of-sample discipline:
//! - Every PAV monotone map `p_cal = f(p_raw)` is fitted STRICTLY on the
//!   chronological In-Sample prefix (first 60% of settled window epochs) and
//!   evaluated STRICTLY on the held-out Out-of-Sample suffix (last 40%).
//! - Never fits and scores on the same rows.
//! - Emits 5-point probability buckets (`0-5%` .. `95-100%`) with exact 95%
//!   Wilson confidence intervals, Brier score, log loss, ECE, MCE, power
//!   honesty (`min_detectable_edge` at 80% power, alpha=0.05), and a
//!   machine-readable EV-After-Fees entry gate map (`p_cal > price + fee + slippage`).

#![allow(dead_code)]

use crate::analytics::{is_valid_whole_cent, wilson_interval_95, WILSON_Z_95};
use crate::{
    facts_fingerprint, fee, window_entry_keyed, window_facts_keyed, window_fingerprint, Snapshot,
    Strategy, WindowFacts,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Two-sided alpha=0.05 (z=1.960) + 80% power (z=0.84162123) multiplier for
/// minimum detectable binomial proportion shift.
const MDE_Z_SUM: f64 = WILSON_Z_95 + 0.841_621_233_572_914_3;

/// Safe probability clip strictly for binary cross-entropy (log loss) so
/// extreme 0/1 probabilities never emit NaN or Infinity.
const LOG_LOSS_EPS: f64 = 1e-6;

/// Number of 5-point probability buckets across `[0.00, 1.00]`.
pub const NUM_5PT_BUCKETS: usize = 20;

#[inline]
fn finite_opt(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

#[inline]
fn binary_log_loss(prob: f64, outcome: bool) -> f64 {
    let p = prob.clamp(LOG_LOSS_EPS, 1.0 - LOG_LOSS_EPS);
    if outcome {
        -p.ln()
    } else {
        -(1.0 - p).ln()
    }
}

#[inline]
fn bucket_5pt_index(prob: f64) -> usize {
    let p = prob.clamp(0.0, 1.0);
    ((p * (NUM_5PT_BUCKETS as f64)).floor() as usize).min(NUM_5PT_BUCKETS - 1)
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// One chronologically ordered settled observation for recalibration analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimedCalibrationObs {
    pub close_epoch: i64,
    pub ticker: String,
    pub series: String,
    pub side: String,
    /// Predicted implied probability in `[0.01, 0.99]`.
    pub raw_prob: f64,
    /// Whole-cent contract entry ask in `[0.01, 0.99]`.
    pub entry_price: f64,
    /// Realized settlement outcome (`true` iff settlement matched `side`).
    pub outcome: bool,
}

/// One pooled monotone block emitted by Pool-Adjacent-Violators (PAV).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PavBlock {
    pub min_p: f64,
    pub max_p: f64,
    pub mean_pred_p: f64,
    pub calibrated_p: f64,
    pub weight: f64,
    pub wins: f64,
}

/// One 5-point probability reliability bucket (`0-5%`, `5-10%`, ..., `95-100%`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket5Pt {
    pub bucket_index: usize,
    pub label: String,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub midpoint: f64,
    pub n: usize,
    pub wins: usize,
    pub realised_rate: Option<f64>,
    pub wilson_lower_95: Option<f64>,
    pub wilson_upper_95: Option<f64>,
    pub mean_pred_p: Option<f64>,
    /// `realised_rate - mean_pred_p` (positive = realized wins beat implied prob).
    pub gap: Option<f64>,
    /// IS-fitted PAV calibrated probability at this bucket's mean predicted prob
    /// (or bucket midpoint when the bucket is empty).
    pub calibrated_p: f64,
    pub brier_contrib: f64,
    pub log_loss_contrib: f64,
}

/// Aggregate proper-scoring metrics over a probability series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationScoreMetrics {
    pub sample_size: usize,
    pub brier_score: Option<f64>,
    pub log_loss: Option<f64>,
    pub ece: Option<f64>,
    pub mce: Option<f64>,
}

/// One cell of the machine-readable EV-After-Fees Entry Gate Map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryGateCell {
    pub bucket_index: usize,
    pub label: String,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub sample_count_is: usize,
    pub sample_count_oos: usize,
    pub raw_p: f64,
    pub calibrated_p: f64,
    pub reference_price: f64,
    pub fee_per_contract: f64,
    pub slippage_margin: f64,
    pub required_prob: f64,
    /// `(calibrated_p - required_prob) * 100.0` in cents per contract.
    pub ev_after_fees_cents: f64,
    pub allow_entry: bool,
}

/// Retrospective OOS impact of applying the IS-fitted EV-After-Fees gate
/// (`p_cal > entry_price + fee(entry_price) + slippage_margin`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryGateImpact {
    pub slippage_margin_cents: f64,
    pub total_evaluated: usize,
    pub accepted_count: usize,
    pub refused_count: usize,
    pub acceptance_rate: f64,
    pub baseline_win_rate: Option<f64>,
    pub accepted_win_rate: Option<f64>,
    pub refused_win_rate: Option<f64>,
    pub baseline_pnl_cents_per_contract: Option<f64>,
    pub accepted_pnl_cents_per_contract: Option<f64>,
    pub refused_pnl_cents_per_contract: Option<f64>,
    pub baseline_total_pnl_dollars_1ct: f64,
    pub accepted_total_pnl_dollars_1ct: f64,
    pub refused_total_pnl_dollars_1ct: f64,
    /// `accepted_total_pnl_dollars_1ct - baseline_total_pnl_dollars_1ct`.
    pub pnl_delta_dollars_1ct: f64,
    pub cells: Vec<EntryGateCell>,
}

/// Full IS-fitted, OOS-validated PAV isotonic recalibration report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecalibrationReport {
    pub population: String,
    pub is_count: usize,
    pub oos_count: usize,
    pub total_count: usize,
    pub split_epoch: Option<i64>,
    pub is_fraction: f64,
    pub oos_fraction: f64,
    pub pav_blocks: Vec<PavBlock>,
    pub is_buckets: Vec<Bucket5Pt>,
    pub oos_buckets: Vec<Bucket5Pt>,
    pub all_buckets: Vec<Bucket5Pt>,
    pub oos_before: CalibrationScoreMetrics,
    pub oos_after: CalibrationScoreMetrics,
    /// `oos_after.brier_score - oos_before.brier_score` (negative = better).
    pub brier_delta: Option<f64>,
    /// `oos_after.log_loss - oos_before.log_loss` (negative = better).
    pub log_loss_delta: Option<f64>,
    /// `oos_after.ece - oos_before.ece` (negative = better).
    pub ece_delta: Option<f64>,
    /// Strictly true only when OOS sample depth is adequate and both Brier and
    /// ECE improve out of sample.
    pub oos_improved: bool,
    /// Minimum detectable probability edge (80% power, alpha=0.05) on OOS sample.
    pub min_detectable_edge: Option<f64>,
    pub honesty_note: String,
    pub entry_gate: EntryGateImpact,
}

/// Compact per-strategy OOS recalibration summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRecalibrationSummary {
    pub strategy: String,
    pub side: String,
    pub is_count: usize,
    pub oos_count: usize,
    pub oos_brier_before: Option<f64>,
    pub oos_brier_after: Option<f64>,
    pub oos_log_loss_before: Option<f64>,
    pub oos_log_loss_after: Option<f64>,
    pub oos_ece_before: Option<f64>,
    pub oos_ece_after: Option<f64>,
    pub oos_improved: bool,
    pub min_detectable_edge: Option<f64>,
    pub accepted_count: usize,
    pub refused_count: usize,
    pub pnl_delta_dollars_1ct: f64,
}

/// Complete desk recalibration payload separating portfolio strategy entries
/// from full-history market tape observations (never silently mixed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeskRecalibrationReport {
    pub windows_settled: usize,
    pub windows_conflicted: usize,
    pub slippage_cents: f64,
    pub portfolio: RecalibrationReport,
    pub market: RecalibrationReport,
    pub by_strategy: Vec<StrategyRecalibrationSummary>,
}

// ---------------------------------------------------------------------------
// Monotone Pool-Adjacent-Violators (PAV) Isotonic Regression
// ---------------------------------------------------------------------------

/// Fit exact weighted monotone non-decreasing Pool-Adjacent-Violators (PAV)
/// isotonic regression over `(x_pred, y_value, weight)` tuples.
///
/// Guarantees `blocks[0].calibrated_p <= blocks[1].calibrated_p <= ... <= blocks[k-1].calibrated_p`,
/// bounded in `[0.0, 1.0]`.
pub fn fit_pav_isotonic(points: &[(f64, f64, f64)]) -> Vec<PavBlock> {
    let mut sorted: Vec<(f64, f64, f64)> = points
        .iter()
        .copied()
        .filter(|(x, y, w)| x.is_finite() && y.is_finite() && w.is_finite() && *w > 0.0)
        .map(|(x, y, w)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0), w))
        .collect();
    if sorted.is_empty() {
        return Vec::new();
    }
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Coalesce identical x-coordinates first so tied knots share exact weight.
    struct WorkBlock {
        min_p: f64,
        max_p: f64,
        sum_xw: f64,
        sum_yw: f64,
        sum_w: f64,
    }
    impl WorkBlock {
        #[inline]
        fn rate(&self) -> f64 {
            (self.sum_yw / self.sum_w).clamp(0.0, 1.0)
        }
    }

    let mut stack: Vec<WorkBlock> = Vec::with_capacity(sorted.len());
    for (x, y, w) in sorted {
        let pushed_tie = if let Some(last) = stack.last_mut() {
            if (x - last.max_p).abs() <= 1e-12 {
                last.sum_xw += x * w;
                last.sum_yw += y * w;
                last.sum_w += w;
                true
            } else {
                false
            }
        } else {
            false
        };
        if !pushed_tie {
            stack.push(WorkBlock {
                min_p: x,
                max_p: x,
                sum_xw: x * w,
                sum_yw: y * w,
                sum_w: w,
            });
        }

        // Pool adjacent blocks whenever left block rate exceeds right block rate.
        while stack.len() >= 2 {
            let len = stack.len();
            if stack[len - 2].rate() > stack[len - 1].rate() {
                let right = stack.pop().unwrap();
                let left = stack.last_mut().unwrap();
                left.max_p = right.max_p;
                left.sum_xw += right.sum_xw;
                left.sum_yw += right.sum_yw;
                left.sum_w += right.sum_w;
            } else {
                break;
            }
        }
    }

    stack
        .into_iter()
        .map(|b| PavBlock {
            min_p: b.min_p,
            max_p: b.max_p,
            mean_pred_p: (b.sum_xw / b.sum_w).clamp(0.0, 1.0),
            calibrated_p: b.rate(),
            weight: b.sum_w,
            wins: b.sum_yw,
        })
        .collect()
}

/// Evaluate a fitted monotone PAV curve at `raw_prob` using piecewise-linear
/// interpolation between adjacent block anchors and flat clamping at the ends.
/// Always preserves non-decreasing monotonicity in `raw_prob`.
pub fn evaluate_pav(blocks: &[PavBlock], raw_prob: f64) -> f64 {
    let p = raw_prob.clamp(0.0, 1.0);
    if blocks.is_empty() {
        return p;
    }
    if blocks.len() == 1 || p <= blocks[0].mean_pred_p {
        return blocks[0].calibrated_p;
    }
    let last = &blocks[blocks.len() - 1];
    if p >= last.mean_pred_p {
        return last.calibrated_p;
    }
    for pair in blocks.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if p >= a.mean_pred_p && p <= b.mean_pred_p {
            let span = b.mean_pred_p - a.mean_pred_p;
            if span <= 1e-12 {
                return 0.5 * (a.calibrated_p + b.calibrated_p);
            }
            let t = ((p - a.mean_pred_p) / span).clamp(0.0, 1.0);
            return (a.calibrated_p + t * (b.calibrated_p - a.calibrated_p)).clamp(0.0, 1.0);
        }
    }
    last.calibrated_p
}

/// Fit PAV on 5-point IS buckets (keeping the monotone curve inspectable and
/// immune to single-tick overfitting while staying strictly monotone).
fn fit_5pt_pav(is_obs: &[TimedCalibrationObs]) -> Vec<PavBlock> {
    let mut counts = [0usize; NUM_5PT_BUCKETS];
    let mut wins = [0usize; NUM_5PT_BUCKETS];
    let mut pred_sum = [0.0f64; NUM_5PT_BUCKETS];

    for o in is_obs {
        if !o.raw_prob.is_finite() {
            continue;
        }
        let p = o.raw_prob.clamp(0.0, 1.0);
        let idx = bucket_5pt_index(p);
        counts[idx] += 1;
        if o.outcome {
            wins[idx] += 1;
        }
        pred_sum[idx] += p;
    }

    let mut points = Vec::new();
    for b in 0..NUM_5PT_BUCKETS {
        if counts[b] > 0 {
            let n = counts[b] as f64;
            let mean_p = pred_sum[b] / n;
            let rate = wins[b] as f64 / n;
            points.push((mean_p, rate, n));
        }
    }
    fit_pav_isotonic(&points)
}

/// Minimum detectable probability edge at 80% power, two-sided alpha = 0.05.
pub fn compute_min_detectable_edge(wins: usize, n: usize) -> Option<f64> {
    if n == 0 {
        return None;
    }
    let p_hat = (wins.min(n) as f64 / n as f64).clamp(0.05, 0.95);
    let mde = MDE_Z_SUM * (p_hat * (1.0 - p_hat) / (n as f64)).sqrt();
    finite_opt(mde.clamp(0.0, 1.0))
}

fn score_prob_series(
    obs: &[TimedCalibrationObs],
    prob_fn: impl Fn(&TimedCalibrationObs) -> f64,
) -> CalibrationScoreMetrics {
    let mut counts = [0usize; NUM_5PT_BUCKETS];
    let mut wins = [0usize; NUM_5PT_BUCKETS];
    let mut pred_sum = [0.0f64; NUM_5PT_BUCKETS];

    let mut n = 0usize;
    let mut brier_sum = 0.0f64;
    let mut ll_sum = 0.0f64;

    for o in obs {
        let p = prob_fn(o);
        if !p.is_finite() {
            continue;
        }
        let p = p.clamp(0.0, 1.0);
        let y = if o.outcome { 1.0 } else { 0.0 };
        let diff = p - y;
        brier_sum += diff * diff;
        ll_sum += binary_log_loss(p, o.outcome);

        let idx = bucket_5pt_index(p);
        counts[idx] += 1;
        if o.outcome {
            wins[idx] += 1;
        }
        pred_sum[idx] += p;
        n += 1;
    }

    if n == 0 {
        return CalibrationScoreMetrics {
            sample_size: 0,
            brier_score: None,
            log_loss: None,
            ece: None,
            mce: None,
        };
    }

    let n_f = n as f64;
    let mut ece = 0.0f64;
    let mut mce = 0.0f64;
    for b in 0..NUM_5PT_BUCKETS {
        if counts[b] > 0 {
            let c_f = counts[b] as f64;
            let gap = (wins[b] as f64 / c_f - pred_sum[b] / c_f).abs();
            ece += (c_f / n_f) * gap;
            if gap > mce {
                mce = gap;
            }
        }
    }

    CalibrationScoreMetrics {
        sample_size: n,
        brier_score: finite_opt(brier_sum / n_f),
        log_loss: finite_opt(ll_sum / n_f),
        ece: finite_opt(ece),
        mce: finite_opt(mce),
    }
}

fn build_5pt_buckets(obs: &[TimedCalibrationObs], pav_blocks: &[PavBlock]) -> Vec<Bucket5Pt> {
    let mut counts = [0usize; NUM_5PT_BUCKETS];
    let mut wins = [0usize; NUM_5PT_BUCKETS];
    let mut pred_sum = [0.0f64; NUM_5PT_BUCKETS];
    let mut brier_sum = [0.0f64; NUM_5PT_BUCKETS];
    let mut ll_sum = [0.0f64; NUM_5PT_BUCKETS];
    let mut valid_n = 0usize;

    for o in obs {
        if !o.raw_prob.is_finite() {
            continue;
        }
        let p = o.raw_prob.clamp(0.0, 1.0);
        let y = if o.outcome { 1.0 } else { 0.0 };
        let idx = bucket_5pt_index(p);
        counts[idx] += 1;
        if o.outcome {
            wins[idx] += 1;
        }
        pred_sum[idx] += p;
        let diff = p - y;
        brier_sum[idx] += diff * diff;
        ll_sum[idx] += binary_log_loss(p, o.outcome);
        valid_n += 1;
    }

    let denom = valid_n.max(1) as f64;
    let mut out = Vec::with_capacity(NUM_5PT_BUCKETS);
    for b in 0..NUM_5PT_BUCKETS {
        let lower_bound = (b as f64) * 0.05;
        let upper_bound = ((b + 1) as f64) * 0.05;
        let midpoint = lower_bound + 0.025;
        let label = format!("{}-{}%", b * 5, (b + 1) * 5);
        let n = counts[b];
        let w = wins[b];
        if n > 0 {
            let mean_p = pred_sum[b] / n as f64;
            let realised = w as f64 / n as f64;
            let (wl, wu) = wilson_interval_95(w, n).unwrap();
            let cal_p = evaluate_pav(pav_blocks, mean_p);
            out.push(Bucket5Pt {
                bucket_index: b,
                label,
                lower_bound,
                upper_bound,
                midpoint,
                n,
                wins: w,
                realised_rate: Some(realised),
                wilson_lower_95: Some(wl),
                wilson_upper_95: Some(wu),
                mean_pred_p: Some(mean_p),
                gap: Some(realised - mean_p),
                calibrated_p: cal_p,
                brier_contrib: brier_sum[b] / denom,
                log_loss_contrib: ll_sum[b] / denom,
            });
        } else {
            out.push(Bucket5Pt {
                bucket_index: b,
                label,
                lower_bound,
                upper_bound,
                midpoint,
                n: 0,
                wins: 0,
                realised_rate: None,
                wilson_lower_95: None,
                wilson_upper_95: None,
                mean_pred_p: None,
                gap: None,
                calibrated_p: evaluate_pav(pav_blocks, midpoint),
                brier_contrib: 0.0,
                log_loss_contrib: 0.0,
            });
        }
    }
    out
}

fn build_entry_gate(
    is_obs: &[TimedCalibrationObs],
    oos_obs: &[TimedCalibrationObs],
    pav_blocks: &[PavBlock],
    slippage_cents: f64,
) -> EntryGateImpact {
    let slip_c = if slippage_cents.is_finite() {
        slippage_cents.clamp(0.0, 5.0)
    } else {
        1.0
    };
    let slip_margin = slip_c / 100.0;

    let mut is_counts = [0usize; NUM_5PT_BUCKETS];
    let mut oos_counts = [0usize; NUM_5PT_BUCKETS];
    let mut price_sum = [0.0f64; NUM_5PT_BUCKETS];
    let mut price_cnt = [0usize; NUM_5PT_BUCKETS];

    for o in is_obs {
        if o.raw_prob.is_finite() {
            let idx = bucket_5pt_index(o.raw_prob);
            is_counts[idx] += 1;
            if o.entry_price.is_finite() {
                price_sum[idx] += o.entry_price.clamp(0.01, 0.99);
                price_cnt[idx] += 1;
            }
        }
    }
    for o in oos_obs {
        if o.raw_prob.is_finite() {
            let idx = bucket_5pt_index(o.raw_prob);
            oos_counts[idx] += 1;
            if o.entry_price.is_finite() {
                price_sum[idx] += o.entry_price.clamp(0.01, 0.99);
                price_cnt[idx] += 1;
            }
        }
    }

    let mut cells = Vec::with_capacity(NUM_5PT_BUCKETS);
    for b in 0..NUM_5PT_BUCKETS {
        let lower_bound = (b as f64) * 0.05;
        let upper_bound = ((b + 1) as f64) * 0.05;
        let midpoint = (lower_bound + 0.025).clamp(0.01, 0.99);
        let label = format!("{}-{}%", b * 5, (b + 1) * 5);
        let ref_price = if price_cnt[b] > 0 {
            (price_sum[b] / price_cnt[b] as f64).clamp(0.01, 0.99)
        } else {
            midpoint
        };
        let cal_p = evaluate_pav(pav_blocks, midpoint);
        let f_ct = fee(ref_price);
        let required_prob = (ref_price + f_ct + slip_margin).min(1.0);
        let ev_cents = (cal_p - required_prob) * 100.0;
        let allow_entry = !pav_blocks.is_empty() && cal_p > required_prob;
        cells.push(EntryGateCell {
            bucket_index: b,
            label,
            lower_bound,
            upper_bound,
            sample_count_is: is_counts[b],
            sample_count_oos: oos_counts[b],
            raw_p: midpoint,
            calibrated_p: cal_p,
            reference_price: ref_price,
            fee_per_contract: f_ct,
            slippage_margin: slip_margin,
            required_prob,
            ev_after_fees_cents: ev_cents,
            allow_entry,
        });
    }

    // Retrospective evaluation strictly on held-out OOS rows.
    let mut accepted_n = 0usize;
    let mut accepted_wins = 0usize;
    let mut accepted_pnl = 0.0f64;
    let mut refused_n = 0usize;
    let mut refused_wins = 0usize;
    let mut refused_pnl = 0.0f64;

    for o in oos_obs {
        if !o.raw_prob.is_finite() || !o.entry_price.is_finite() {
            continue;
        }
        let price = o.entry_price.clamp(0.01, 0.99);
        let f_ct = fee(price);
        let required = price + f_ct + slip_margin;
        let p_cal = evaluate_pav(pav_blocks, o.raw_prob);
        let payoff = if o.outcome { 1.0 } else { 0.0 };
        let pnl_1ct = payoff - price - f_ct - slip_margin;

        if !pav_blocks.is_empty() && p_cal > required {
            accepted_n += 1;
            if o.outcome {
                accepted_wins += 1;
            }
            accepted_pnl += pnl_1ct;
        } else {
            refused_n += 1;
            if o.outcome {
                refused_wins += 1;
            }
            refused_pnl += pnl_1ct;
        }
    }

    let total = accepted_n + refused_n;
    let base_wins = accepted_wins + refused_wins;
    let base_pnl = accepted_pnl + refused_pnl;

    let rate = |w: usize, n: usize| if n > 0 { Some(w as f64 / n as f64) } else { None };
    let mean_c = |sum: f64, n: usize| {
        if n > 0 {
            Some(sum / n as f64 * 100.0)
        } else {
            None
        }
    };

    EntryGateImpact {
        slippage_margin_cents: slip_c,
        total_evaluated: total,
        accepted_count: accepted_n,
        refused_count: refused_n,
        acceptance_rate: if total > 0 {
            accepted_n as f64 / total as f64
        } else {
            0.0
        },
        baseline_win_rate: rate(base_wins, total),
        accepted_win_rate: rate(accepted_wins, accepted_n),
        refused_win_rate: rate(refused_wins, refused_n),
        baseline_pnl_cents_per_contract: mean_c(base_pnl, total),
        accepted_pnl_cents_per_contract: mean_c(accepted_pnl, accepted_n),
        refused_pnl_cents_per_contract: mean_c(refused_pnl, refused_n),
        baseline_total_pnl_dollars_1ct: base_pnl,
        accepted_total_pnl_dollars_1ct: accepted_pnl,
        refused_total_pnl_dollars_1ct: refused_pnl,
        pnl_delta_dollars_1ct: accepted_pnl - base_pnl,
        cells,
    }
}

/// Run a complete chronological IS (60%) -> OOS (40%) PAV isotonic recalibration
/// study on a population of settled observations.
pub fn run_recalibration_study(
    population: &str,
    obs: &[TimedCalibrationObs],
    slippage_cents: f64,
) -> RecalibrationReport {
    let mut ordered: Vec<TimedCalibrationObs> = obs
        .iter()
        .filter(|o| o.raw_prob.is_finite() && o.entry_price.is_finite())
        .cloned()
        .collect();
    ordered.sort_by(|a, b| {
        a.close_epoch
            .cmp(&b.close_epoch)
            .then_with(|| a.ticker.cmp(&b.ticker))
            .then_with(|| a.side.cmp(&b.side))
    });

    if ordered.is_empty() {
        let empty_metrics = CalibrationScoreMetrics {
            sample_size: 0,
            brier_score: None,
            log_loss: None,
            ece: None,
            mce: None,
        };
        return RecalibrationReport {
            population: population.to_string(),
            is_count: 0,
            oos_count: 0,
            total_count: 0,
            split_epoch: None,
            is_fraction: 0.60,
            oos_fraction: 0.40,
            pav_blocks: Vec::new(),
            is_buckets: build_5pt_buckets(&[], &[]),
            oos_buckets: build_5pt_buckets(&[], &[]),
            all_buckets: build_5pt_buckets(&[], &[]),
            oos_before: empty_metrics.clone(),
            oos_after: empty_metrics,
            brier_delta: None,
            log_loss_delta: None,
            ece_delta: None,
            oos_improved: false,
            min_detectable_edge: None,
            honesty_note: "No settled observations on tape yet; recalibration requires chronological in-sample and out-of-sample history.".to_string(),
            entry_gate: build_entry_gate(&[], &[], &[], slippage_cents),
        };
    }

    // Strict chronological split on distinct window close epochs (60% IS / 40% OOS)
    // so observations from the exact same 15-minute window never straddle IS and OOS.
    let mut epochs: Vec<i64> = ordered.iter().map(|o| o.close_epoch).collect();
    epochs.dedup();
    let n_epochs = epochs.len();

    let (is_obs, oos_obs, split_epoch): (&[TimedCalibrationObs], &[TimedCalibrationObs], Option<i64>) =
        if n_epochs < 2 {
            (ordered.as_slice(), &ordered[ordered.len()..], Some(epochs[0]))
        } else {
            let is_epoch_count = ((n_epochs * 3) / 5).max(1).min(n_epochs - 1);
            let boundary = epochs[is_epoch_count];
            let cut = ordered.partition_point(|o| o.close_epoch < boundary);
            (&ordered[..cut], &ordered[cut..], Some(boundary))
        };

    // Fit PAV STRICTLY on IS observations — OOS rows are NEVER seen during fitting.
    let pav_blocks = fit_5pt_pav(is_obs);

    let is_buckets = build_5pt_buckets(is_obs, &pav_blocks);
    let oos_buckets = build_5pt_buckets(oos_obs, &pav_blocks);
    let all_buckets = build_5pt_buckets(&ordered, &pav_blocks);

    // Evaluate strictly on held-out OOS observations.
    let oos_before = score_prob_series(oos_obs, |o| o.raw_prob);
    let oos_after = score_prob_series(oos_obs, |o| evaluate_pav(&pav_blocks, o.raw_prob));

    let brier_delta = match (oos_before.brier_score, oos_after.brier_score) {
        (Some(b), Some(a)) => finite_opt(a - b),
        _ => None,
    };
    let log_loss_delta = match (oos_before.log_loss, oos_after.log_loss) {
        (Some(b), Some(a)) => finite_opt(a - b),
        _ => None,
    };
    let ece_delta = match (oos_before.ece, oos_after.ece) {
        (Some(b), Some(a)) => finite_opt(a - b),
        _ => None,
    };

    let oos_wins = oos_obs.iter().filter(|o| o.outcome).count();
    let min_detectable_edge = compute_min_detectable_edge(oos_wins, oos_obs.len());

    let oos_improved = oos_obs.len() >= 20
        && brier_delta.is_some_and(|d| d < -1e-6)
        && ece_delta.is_some_and(|d| d <= 1e-6);

    let honesty_note = if oos_obs.len() < 20 {
        format!(
            "Underpowered out-of-sample split ({} OOS rows; minimum 20 required). Treat the fitted curve as exploratory only.",
            oos_obs.len()
        )
    } else if oos_improved {
        let mde_pct = min_detectable_edge.unwrap_or(0.0) * 100.0;
        format!(
            "Out-of-sample Brier ({:+.4}) and ECE ({:+.4}) improved over {} held-out rows (80%-power MDE = {:.1} pp). Verify stability as tape grows.",
            brier_delta.unwrap_or(0.0),
            ece_delta.unwrap_or(0.0),
            oos_obs.len(),
            mde_pct
        )
    } else {
        let mde_pct = min_detectable_edge.unwrap_or(0.0) * 100.0;
        format!(
            "Isotonic recalibration did NOT improve out-of-sample proper scores on {} held-out rows (Brier {:+.4}, ECE {:+.4}; MDE = {:.1} pp). Prefer a conservative margin buffer over a fitted curve.",
            oos_obs.len(),
            brier_delta.unwrap_or(0.0),
            ece_delta.unwrap_or(0.0),
            mde_pct
        )
    };

    let entry_gate = build_entry_gate(is_obs, oos_obs, &pav_blocks, slippage_cents);

    RecalibrationReport {
        population: population.to_string(),
        is_count: is_obs.len(),
        oos_count: oos_obs.len(),
        total_count: ordered.len(),
        split_epoch,
        is_fraction: 0.60,
        oos_fraction: 0.40,
        pav_blocks,
        is_buckets,
        oos_buckets,
        all_buckets,
        oos_before,
        oos_after,
        brier_delta,
        log_loss_delta,
        ece_delta,
        oos_improved,
        min_detectable_edge,
        honesty_note,
        entry_gate,
    }
}

/// Extract representative pre-close YES mid probability and YES ask for a
/// settled market window (closest active quote to T-240s).
fn representative_window_market_quote(snaps: &[Snapshot], close_epoch: i64) -> Option<(f64, f64)> {
    let mut best: Option<(i64, f64, f64)> = None;
    for s in snaps {
        let (Some(ya), Some(na)) = (s.yes_ask.get(), s.no_ask.get()) else {
            continue;
        };
        if !is_valid_whole_cent(ya) || !is_valid_whole_cent(na) {
            continue;
        }
        let secs = close_epoch - s.fetched_at;
        if secs <= 0 {
            continue;
        }
        let yes_bid = s.yes_bid.get().filter(|b| (0.0..=1.0).contains(b)).unwrap_or(1.0 - na);
        let mid = ((ya + yes_bid) * 0.5).clamp(0.01, 0.99);
        let dist = (secs - 240).abs();
        match best {
            Some((best_dist, _, _)) if dist >= best_dist => {}
            _ => best = Some((dist, mid, ya)),
        }
    }
    best.map(|(_, mid, ask)| (mid, ask))
}

/// Deterministic fingerprint of fitted PAV blocks for strategy entry cache keying.
pub fn pav_blocks_fingerprint(blocks: &[PavBlock]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    blocks.len().hash(&mut h);
    for b in blocks {
        b.mean_pred_p.to_bits().hash(&mut h);
        b.calibrated_p.to_bits().hash(&mut h);
        b.weight.to_bits().hash(&mut h);
    }
    h.finish()
}

/// Fit strictly chronological in-sample (first 60% of settled epochs) 5-point
/// PAV isotonic blocks from market tape windows. OOS outcomes are never read.
pub fn fit_is_market_pav_blocks(
    windows: &BTreeMap<String, Vec<Snapshot>>,
) -> (Vec<PavBlock>, Option<i64>) {
    let mut market_obs: Vec<TimedCalibrationObs> = Vec::new();
    for (ticker, snaps) in windows {
        let facts_fp = facts_fingerprint(snaps);
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict | WindowFacts::Unsettled => continue,
        };
        let series = snaps.first().map(|s| s.series.to_string()).unwrap_or_default();
        if let Some((mid, ask)) = representative_window_market_quote(snaps, close_epoch) {
            market_obs.push(TimedCalibrationObs {
                close_epoch,
                ticker: ticker.clone(),
                series,
                side: "yes".to_string(),
                raw_prob: mid,
                entry_price: ask,
                outcome: result == "yes",
            });
        }
    }
    if market_obs.is_empty() {
        return (Vec::new(), None);
    }
    market_obs.sort_by(|a, b| {
        a.close_epoch
            .cmp(&b.close_epoch)
            .then_with(|| a.ticker.cmp(&b.ticker))
    });
    let mut epochs: Vec<i64> = market_obs.iter().map(|o| o.close_epoch).collect();
    epochs.dedup();
    let (is_obs, split_epoch) = if epochs.len() < 2 {
        (market_obs.as_slice(), Some(epochs[0]))
    } else {
        let is_epoch_count = ((epochs.len() * 3) / 5).max(1).min(epochs.len() - 1);
        let boundary = epochs[is_epoch_count];
        let cut = market_obs.partition_point(|o| o.close_epoch < boundary);
        (&market_obs[..cut], Some(boundary))
    };
    (fit_5pt_pav(is_obs), split_epoch)
}

/// Fit 5-point PAV isotonic blocks directly over an explicitly provided slice of
/// settled `(ticker, close_epoch, result, snaps)` windows (strictly IS-only).
pub fn fit_pav_blocks_for_settled_windows<'a>(
    settled: &[(&'a str, i64, &'a str, &'a [Snapshot])],
) -> Vec<PavBlock> {
    let mut obs: Vec<TimedCalibrationObs> = Vec::with_capacity(settled.len());
    for &(ticker, close_epoch, result, snaps) in settled {
        let series = snaps.first().map(|s| s.series.to_string()).unwrap_or_default();
        if let Some((mid, ask)) = representative_window_market_quote(snaps, close_epoch) {
            obs.push(TimedCalibrationObs {
                close_epoch,
                ticker: ticker.to_string(),
                series,
                side: "yes".to_string(),
                raw_prob: mid,
                entry_price: ask,
                outcome: result == "yes",
            });
        }
    }
    if obs.is_empty() {
        return Vec::new();
    }
    fit_5pt_pav(&obs)
}

/// Run the full desk PAV isotonic recalibration study over collected tape
/// windows and loaded strategies, keeping the portfolio entry population and
/// full-history market population cleanly separated.
pub fn recalibration_from_windows(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    strategies: &[Strategy],
    slippage_cents: f64,
) -> DeskRecalibrationReport {
    let slip_c = if slippage_cents.is_finite() {
        slippage_cents.clamp(0.0, 5.0)
    } else {
        1.0
    };

    let mut settled = 0usize;
    let mut conflicted = 0usize;
    let mut market_obs: Vec<TimedCalibrationObs> = Vec::new();
    let mut portfolio_obs: Vec<TimedCalibrationObs> = Vec::new();
    let mut per_strat_obs: Vec<Vec<TimedCalibrationObs>> = vec![Vec::new(); strategies.len()];

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
        let series = snaps.first().map(|s| s.series.to_string()).unwrap_or_default();

        if let Some((mid, ask)) = representative_window_market_quote(snaps, close_epoch) {
            market_obs.push(TimedCalibrationObs {
                close_epoch,
                ticker: ticker.clone(),
                series: series.clone(),
                side: "yes".to_string(),
                raw_prob: mid,
                entry_price: ask,
                outcome: result == "yes",
            });
        }

        if !strategies.is_empty() {
            let win_fp = window_fingerprint(snaps);
            for (idx, strat) in strategies.iter().enumerate() {
                if let Some(price) = window_entry_keyed(strat, Some(ticker.as_str()), snaps, win_fp)
                {
                    if is_valid_whole_cent(price) {
                        let obs = TimedCalibrationObs {
                            close_epoch,
                            ticker: ticker.clone(),
                            series: series.clone(),
                            side: strat.side.clone(),
                            raw_prob: price,
                            entry_price: price,
                            outcome: result == strat.side,
                        };
                        per_strat_obs[idx].push(obs.clone());
                        portfolio_obs.push(obs);
                    }
                }
            }
        }
    }

    let portfolio = run_recalibration_study("portfolio_entries", &portfolio_obs, slip_c);
    let market = run_recalibration_study("market_tape", &market_obs, slip_c);

    let by_strategy = strategies
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let rep = run_recalibration_study(&s.name, &per_strat_obs[idx], slip_c);
            StrategyRecalibrationSummary {
                strategy: s.name.clone(),
                side: s.side.clone(),
                is_count: rep.is_count,
                oos_count: rep.oos_count,
                oos_brier_before: rep.oos_before.brier_score,
                oos_brier_after: rep.oos_after.brier_score,
                oos_log_loss_before: rep.oos_before.log_loss,
                oos_log_loss_after: rep.oos_after.log_loss,
                oos_ece_before: rep.oos_before.ece,
                oos_ece_after: rep.oos_after.ece,
                oos_improved: rep.oos_improved,
                min_detectable_edge: rep.min_detectable_edge,
                accepted_count: rep.entry_gate.accepted_count,
                refused_count: rep.entry_gate.refused_count,
                pnl_delta_dollars_1ct: rep.entry_gate.pnl_delta_dollars_1ct,
            }
        })
        .collect();

    DeskRecalibrationReport {
        windows_settled: settled,
        windows_conflicted: conflicted,
        slippage_cents: slip_c,
        portfolio,
        market,
        by_strategy,
    }
}

// ---------------------------------------------------------------------------
// Comprehensive Unit, Monotonicity & Negative-Control Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{iso_z, load_strategies};
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

    #[test]
    fn pav_isotonic_guarantees_strict_monotonicity_and_exact_pooling() {
        // Non-monotone sequence:
        // x=0.2 (y=0.4, w=10), x=0.4 (y=0.2, w=10) -> pool to 0.30!
        // x=0.6 (y=0.7, w=10), x=0.8 (y=0.5, w=30) -> pool to (7+15)/40 = 0.55!
        let pts = vec![(0.2, 0.4, 10.0), (0.4, 0.2, 10.0), (0.6, 0.7, 10.0), (0.8, 0.5, 30.0)];
        let blocks = fit_pav_isotonic(&pts);
        assert_eq!(blocks.len(), 2);
        assert!((blocks[0].calibrated_p - 0.30).abs() < 1e-12);
        assert!((blocks[1].calibrated_p - 0.55).abs() < 1e-12);

        // Adversarial zigzag stress across 100 points: output MUST be non-decreasing everywhere.
        let zigzag: Vec<(f64, f64, f64)> = (1..=99)
            .map(|i| {
                let x = i as f64 / 100.0;
                let y = if i % 3 == 0 {
                    (x - 0.25).clamp(0.0, 1.0)
                } else {
                    (x + 0.15).clamp(0.0, 1.0)
                };
                (x, y, ((i % 7) + 1) as f64)
            })
            .collect();
        let zblocks = fit_pav_isotonic(&zigzag);
        assert!(!zblocks.is_empty());
        for pair in zblocks.windows(2) {
            assert!(
                pair[0].calibrated_p <= pair[1].calibrated_p + 1e-12,
                "PAV violated monotonicity: {} > {}",
                pair[0].calibrated_p,
                pair[1].calibrated_p
            );
        }
        // Dense grid check of evaluate_pav across 0.00..=1.00.
        let mut prev = evaluate_pav(&zblocks, 0.0);
        for c in 1..=100 {
            let cur = evaluate_pav(&zblocks, c as f64 / 100.0);
            assert!(
                cur + 1e-12 >= prev && (0.0..=1.0).contains(&cur),
                "evaluate_pav non-monotone at {c}c: {prev} -> {cur}"
            );
            prev = cur;
        }
    }

    #[test]
    fn oos_strict_isolation_and_single_settlement_flip_sensitivity() {
        // Construct 100 chronological observations with systematic miscalibration:
        // Raw price 0.72 actually wins 85% of the time; raw price 0.82 actually wins 60%
        // (Wait: monotone miscalibration where low=0.35 wins 25%, mid=0.62 wins 78%, high=0.82 wins 88%).
        let mut obs = Vec::new();
        for i in 0..100usize {
            let close_epoch = 1_765_000_000 + (i as i64) * 900;
            let (raw_p, won) = match i % 3 {
                0 => (0.37, (i / 3) % 4 == 0), // ~25% win rate when priced at 37c -> negative EV!
                1 => (0.62, (i / 3) % 5 != 0), // ~80% win rate when priced at 62c -> strong positive EV!
                _ => (0.82, (i / 3) % 8 != 0), // ~87.5% win rate when priced at 82c -> slight positive EV
            };
            obs.push(TimedCalibrationObs {
                close_epoch,
                ticker: format!("KXBTC15M-CAL{i:03}"),
                series: "KXBTC15M".to_string(),
                side: "yes".to_string(),
                raw_prob: raw_p,
                entry_price: raw_p,
                outcome: won,
            });
        }

        let base = run_recalibration_study("synthetic", &obs, 1.0);
        assert_eq!(base.is_count, 60);
        assert_eq!(base.oos_count, 40);
        assert_eq!(base.split_epoch, Some(1_765_000_000 + 60 * 900));
        assert_eq!(base.oos_buckets.len(), 20);
        assert!(base.oos_improved, "expected OOS Brier & ECE improvement: {}", base.honesty_note);
        assert!(base.brier_delta.unwrap() < 0.0);
        assert!(base.ece_delta.unwrap() <= 0.0);
        assert!(base.entry_gate.refused_count > 0, "gate must refuse negative-EV 37c bucket");
        assert!(base.entry_gate.accepted_count > 0, "gate must accept positive-EV 62c bucket");
        assert!(
            base.entry_gate.pnl_delta_dollars_1ct > 0.0,
            "refusing negative-EV bucket must improve OOS P&L"
        );

        // NEGATIVE CONTROL 1 (Strict IS/OOS isolation):
        // Flipping an OOS settlement MUST NOT change a single PAV block fitted on IS,
        // AND MUST measurably change OOS Brier score & bucket wins!
        let mut flipped_oos = obs.clone();
        flipped_oos[80].outcome = !flipped_oos[80].outcome;
        let rep_oos_flip = run_recalibration_study("synthetic", &flipped_oos, 1.0);
        assert_eq!(
            base.pav_blocks, rep_oos_flip.pav_blocks,
            "OOS mutation leaked into IS-fitted PAV blocks!"
        );
        assert!(
            (base.oos_before.brier_score.unwrap() - rep_oos_flip.oos_before.brier_score.unwrap()).abs()
                > 1e-6,
            "1-flip OOS settlement negative control did not move OOS Brier score"
        );

        // NEGATIVE CONTROL 2: Flipping an IS settlement MUST change the IS-fitted PAV block.
        let mut flipped_is = obs.clone();
        flipped_is[10].outcome = !flipped_is[10].outcome;
        let rep_is_flip = run_recalibration_study("synthetic", &flipped_is, 1.0);
        assert_ne!(
            base.pav_blocks, rep_is_flip.pav_blocks,
            "IS settlement flip did not affect PAV fit"
        );
    }

    #[test]
    fn recalibration_from_windows_separates_populations_and_serializes_cleanly() {
        let strategies = load_strategies(Path::new("strategies"));
        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        for i in 0..50usize {
            let close_epoch = 1_766_000_000 + (i as i64) * 900;
            let close_iso = iso_z(close_epoch);
            let ticker = format!("KXBTC15M-RC{i:03}");
            let ya = (63 + (i % 10) * 2) as f64 / 100.0;
            let na = ((102 - (63 + (i % 10) * 2)) as f64) / 100.0;
            let res = if i % 5 == 4 { "no" } else { "yes" };
            windows.insert(
                ticker.clone(),
                vec![
                    snap_json(&ticker, "KXBTC15M", close_epoch - 240, &close_iso, "", Some(ya), Some(na)),
                    snap_json(&ticker, "KXBTC15M", close_epoch + 60, &close_iso, res, None, None),
                ],
            );
        }

        let desk = recalibration_from_windows(&windows, &strategies, 1.0);
        assert_eq!(desk.windows_settled, 50);
        assert_eq!(desk.windows_conflicted, 0);
        assert_eq!(desk.market.total_count, 50);
        assert_eq!(desk.market.is_count, 30);
        assert_eq!(desk.market.oos_count, 20);
        assert_eq!(desk.by_strategy.len(), strategies.len());

        let json = serde_json::to_string(&desk).unwrap();
        assert!(!json.contains("NaN") && !json.contains("Infinity"));
        assert!(!json.to_ascii_lowercase().contains("rune"));
    }

    #[test]
    fn independent_spot_check_two_buckets_and_slippage_monotonicity() {
        // Independent recomputation of two named 5-point buckets (60-65% and 80-85%)
        // directly from raw observations, plus slippage monotonicity check.
        let mut obs = Vec::new();
        for i in 0..100usize {
            let close_epoch = 1_769_000_000 + (i as i64) * 900;
            let (raw_p, outcome) = if i % 2 == 0 {
                (0.62, (i / 2) % 4 != 0) // 75% realized on 62c
            } else {
                (0.82, (i / 2) % 5 != 0) // 80% realized on 82c
            };
            obs.push(TimedCalibrationObs {
                close_epoch,
                ticker: format!("KXETH15M-SP{i:03}"),
                series: "KXETH15M".to_string(),
                side: "yes".to_string(),
                raw_prob: raw_p,
                entry_price: raw_p,
                outcome,
            });
        }

        let rep0 = run_recalibration_study("spot", &obs, 0.0);
        let rep1 = run_recalibration_study("spot", &obs, 1.0);
        let rep5 = run_recalibration_study("spot", &obs, 5.0);

        // Slippage monotonicity: tighter slippage never increases accepted OOS entries.
        assert!(rep5.entry_gate.accepted_count <= rep1.entry_gate.accepted_count);
        assert!(rep1.entry_gate.accepted_count <= rep0.entry_gate.accepted_count);

        // Independent OOS check (last 40 rows: indices 60..100).
        let oos_raw = &obs[60..100];
        for (bucket_idx, expected_p) in [(12usize, 0.62f64), (16usize, 0.82f64)] {
            let matching: Vec<&TimedCalibrationObs> = oos_raw
                .iter()
                .filter(|o| (o.raw_prob - expected_p).abs() < 1e-9)
                .collect();
            let raw_n = matching.len();
            let raw_w = matching.iter().filter(|o| o.outcome).count();
            let raw_rate = raw_w as f64 / raw_n as f64;
            let (raw_lo, raw_hi) = wilson_interval_95(raw_w, raw_n).unwrap();

            let b = &rep1.oos_buckets[bucket_idx];
            assert_eq!(b.n, raw_n);
            assert_eq!(b.wins, raw_w);
            assert!((b.realised_rate.unwrap() - raw_rate).abs() < 1e-12);
            assert!((b.mean_pred_p.unwrap() - expected_p).abs() < 1e-12);
            assert!((b.gap.unwrap() - (raw_rate - expected_p)).abs() < 1e-12);
            assert!((b.wilson_lower_95.unwrap() - raw_lo).abs() < 1e-12);
            assert!((b.wilson_upper_95.unwrap() - raw_hi).abs() < 1e-12);
        }

        // Edge cases: empty, single-epoch, extreme 0%/100% wins, and NaN/Inf skipped.
        let empty = run_recalibration_study("empty", &[], 1.0);
        assert_eq!(empty.total_count, 0);
        assert!(!empty.oos_improved);
        assert!(empty.oos_before.brier_score.is_none());

        let single = run_recalibration_study("single", &obs[..1], 1.0);
        assert_eq!(single.is_count, 1);
        assert_eq!(single.oos_count, 0);
        assert!(!single.oos_improved);

        let extreme: Vec<TimedCalibrationObs> = (0..40)
            .map(|i| TimedCalibrationObs {
                close_epoch: 1_770_000_000 + (i as i64) * 900,
                ticker: format!("KXSOL15M-EX{i:02}"),
                series: "KXSOL15M".to_string(),
                side: "yes".to_string(),
                raw_prob: if i % 2 == 0 { 0.01 } else { 0.99 },
                entry_price: if i % 2 == 0 { 0.01 } else { 0.99 },
                outcome: i % 2 != 0,
            })
            .collect();
        let ex_rep = run_recalibration_study("extreme", &extreme, 1.0);
        assert!(ex_rep.oos_before.log_loss.unwrap().is_finite());
        assert!(ex_rep.oos_after.log_loss.unwrap().is_finite());
    }
}
