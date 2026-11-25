//! 1-Click Full Quant Edge Audit Scorecard (strictly paper-only analysis).
//!
//! Combines all five quant validation stages into one deterministic Go / No-Go
//! scorecard:
//! 1. All 12 strategies -> Out-of-Sample (OOS) Tournament ranking
//! 2. Uncorrelated 4-slot lineup (`|r| <= 0.65`)
//! 3. PAV Isotonic Recalibration & EV-After-Fees gate
//! 4. Kalshi fee (`0.07 * p * (1 - p)`) & slippage-adjusted net EV breakdown
//! 5. 1,000-path Monte Carlo drawdown & 5-Gate Readiness Scorecard

#![allow(dead_code)]

use crate::analytics::MonteCarloResult;
use crate::recalibration::DeskRecalibrationReport;
use crate::scaling::{ReadinessGate, ScalePlanResponse};
use crate::tournament::TournamentReport;
use crate::ScoreReport;
use serde::{Deserialize, Serialize};

#[inline]
fn finite_opt(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v)
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditStrategySummary {
    pub strategy: String,
    pub side: String,
    pub oos_trades: usize,
    pub oos_win_rate: Option<f64>,
    pub oos_avg_entry_price: Option<f64>,
    pub oos_avg_fee_cents: Option<f64>,
    pub oos_net_expectancy_cents: Option<f64>,
    pub oos_sharpe: Option<f64>,
    pub oos_max_drawdown_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditSlot {
    pub slot_number: usize,
    pub strategy: String,
    pub side: String,
    pub oos_trades: usize,
    pub oos_net_expectancy_cents: f64,
    pub oos_sharpe: Option<f64>,
    pub oos_max_drawdown_pct: f64,
    pub max_abs_corr_with_lineup: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditLineup {
    pub slots: Vec<EdgeAuditSlot>,
    pub correlation_cap: f64,
    pub max_abs_correlation: Option<f64>,
    pub mean_abs_correlation: Option<f64>,
    pub lineup_complete: bool,
    pub fallback_used: bool,
    pub combined_oos_expectancy_cents: Option<f64>,
    pub honesty_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditRecalibration {
    pub is_count: usize,
    pub oos_count: usize,
    pub oos_brier_before: Option<f64>,
    pub oos_brier_after: Option<f64>,
    pub oos_ece_before: Option<f64>,
    pub oos_ece_after: Option<f64>,
    pub oos_improved: bool,
    pub min_detectable_edge: Option<f64>,
    pub ev_gate_total: usize,
    pub ev_gate_accepted: usize,
    pub ev_gate_refused: usize,
    pub ev_gate_accepted_win_rate: Option<f64>,
    pub ev_gate_before_pnl_dollars: f64,
    pub ev_gate_after_pnl_dollars: f64,
    pub ev_gate_pnl_delta_dollars: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditFeeBreakdown {
    pub avg_entry_price: Option<f64>,
    pub gross_edge_cents: Option<f64>,
    pub kalshi_fee_drag_cents: Option<f64>,
    pub slippage_drag_cents: f64,
    pub net_ev_cents: Option<f64>,
    pub realized_oos_win_rate_pct: Option<f64>,
    pub break_even_win_rate_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditMonteCarlo {
    pub seed: u64,
    pub paths: usize,
    pub start_equity_dollars: f64,
    pub p5_final_equity_dollars: f64,
    pub p50_final_equity_dollars: f64,
    pub p95_final_equity_dollars: f64,
    pub median_max_drawdown_pct: f64,
    pub prob_dd_gte_10_pct: f64,
    pub prob_dd_gte_25_pct: f64,
    pub prob_profit: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditReadiness {
    pub verdict: String,
    pub gates_passed: usize,
    pub gates_total: usize,
    pub gates: Vec<ReadinessGate>,
    pub observed_median_depth: Option<f64>,
    pub fill_ratio_at_target: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditStage {
    pub id: String,
    pub title: String,
    pub passed: bool,
    pub metric: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAuditReport {
    pub mode_notice: String,
    pub dataset: String,
    pub verdict: String,
    pub verdict_label: String,
    pub blocking_reasons: Vec<String>,
    pub slippage_cents: f64,
    pub strategies_total: usize,
    pub strategies_oos_positive: usize,
    pub strategies_forward_proven: usize,
    pub windows_settled_total: usize,
    pub windows_settled_is: usize,
    pub windows_settled_oos: usize,
    pub windows_conflicted: usize,
    pub top_strategy: Option<EdgeAuditStrategySummary>,
    pub lineup: EdgeAuditLineup,
    pub recalibration: EdgeAuditRecalibration,
    pub fee_ev: EdgeAuditFeeBreakdown,
    pub monte_carlo: EdgeAuditMonteCarlo,
    pub readiness: EdgeAuditReadiness,
    pub funnel: Vec<EdgeAuditStage>,
}

/// Assemble a complete deterministic Go / No-Go Edge Audit Scorecard from the
/// five underlying analytical engines.
pub fn build_edge_audit_report(
    dataset: &str,
    scores: &[ScoreReport],
    tournament: &TournamentReport,
    recal: &DeskRecalibrationReport,
    scale_plan: &ScalePlanResponse,
    mc: &MonteCarloResult,
) -> EdgeAuditReport {
    let strategies_total = tournament.rankings.len().max(scores.len());
    let strategies_oos_positive = tournament.rankings.iter().filter(|r| r.oos_positive).count();
    let strategies_forward_proven = scores
        .iter()
        .filter(|s| s.verdict.starts_with("PROVEN on forward evidence"))
        .count();

    let top_strategy = tournament
        .rankings
        .first()
        .map(|r| EdgeAuditStrategySummary {
            strategy: r.strategy.clone(),
            side: r.side.clone(),
            oos_trades: r.oos_trades,
            oos_win_rate: r.oos_win_rate.and_then(finite_opt),
            oos_avg_entry_price: r.oos_avg_entry_price.and_then(finite_opt),
            oos_avg_fee_cents: r.oos_avg_fee_cents.and_then(finite_opt),
            oos_net_expectancy_cents: r.oos_net_expectancy_cents.and_then(finite_opt),
            oos_sharpe: r.oos_sharpe.and_then(finite_opt),
            oos_max_drawdown_pct: finite_or_zero(r.oos_max_drawdown_pct),
        });

    let lineup_slots: Vec<EdgeAuditSlot> = tournament
        .optimal_lineup
        .selected
        .iter()
        .map(|s| EdgeAuditSlot {
            slot_number: s.slot_number,
            strategy: s.strategy.clone(),
            side: s.side.clone(),
            oos_trades: s.oos_trades,
            oos_net_expectancy_cents: finite_or_zero(s.oos_net_expectancy_cents),
            oos_sharpe: s.oos_sharpe.and_then(finite_opt),
            oos_max_drawdown_pct: finite_or_zero(s.oos_max_drawdown_pct),
            max_abs_corr_with_lineup: s.max_abs_corr_with_lineup.and_then(finite_opt),
        })
        .collect();

    let lineup_honesty = tournament
        .optimal_lineup
        .fallback_disclosure
        .clone()
        .unwrap_or_else(|| {
            if tournament.optimal_lineup.lineup_complete {
                format!(
                    "4 uncorrelated slots selected with max |r| <= {:.2}.",
                    tournament.optimal_lineup.correlation_cap
                )
            } else {
                format!(
                    "{}/{} slots qualified under |r| <= {:.2}.",
                    lineup_slots.len(),
                    tournament.optimal_lineup.target_slots,
                    tournament.optimal_lineup.correlation_cap
                )
            }
        });

    let lineup = EdgeAuditLineup {
        slots: lineup_slots,
        correlation_cap: finite_or_zero(tournament.optimal_lineup.correlation_cap),
        max_abs_correlation: tournament.optimal_lineup.max_abs_correlation.and_then(finite_opt),
        mean_abs_correlation: tournament.optimal_lineup.mean_abs_correlation.and_then(finite_opt),
        lineup_complete: tournament.optimal_lineup.lineup_complete,
        fallback_used: tournament.optimal_lineup.fallback_used,
        combined_oos_expectancy_cents: tournament
            .optimal_lineup
            .combined_oos_expectancy_cents
            .and_then(finite_opt),
        honesty_note: lineup_honesty,
    };

    let p_recal = &recal.portfolio;
    let recalibration = EdgeAuditRecalibration {
        is_count: p_recal.is_count,
        oos_count: p_recal.oos_count,
        oos_brier_before: p_recal.oos_before.brier_score.and_then(finite_opt),
        oos_brier_after: p_recal.oos_after.brier_score.and_then(finite_opt),
        oos_ece_before: p_recal.oos_before.ece.and_then(finite_opt),
        oos_ece_after: p_recal.oos_after.ece.and_then(finite_opt),
        oos_improved: p_recal.oos_improved,
        min_detectable_edge: p_recal.min_detectable_edge.and_then(finite_opt),
        ev_gate_total: p_recal.entry_gate.total_evaluated,
        ev_gate_accepted: p_recal.entry_gate.accepted_count,
        ev_gate_refused: p_recal.entry_gate.refused_count,
        ev_gate_accepted_win_rate: p_recal.entry_gate.accepted_win_rate.and_then(finite_opt),
        ev_gate_before_pnl_dollars: finite_or_zero(p_recal.entry_gate.baseline_total_pnl_dollars_1ct),
        ev_gate_after_pnl_dollars: finite_or_zero(p_recal.entry_gate.accepted_total_pnl_dollars_1ct),
        ev_gate_pnl_delta_dollars: finite_or_zero(p_recal.entry_gate.pnl_delta_dollars_1ct),
    };

    // Fee & slippage-adjusted net EV decomposition grounded in empirical OOS tape.
    let emp = &scale_plan.empirical_summary;
    let avg_entry = emp
        .oos_avg_entry_price
        .or_else(|| top_strategy.as_ref().and_then(|t| t.oos_avg_entry_price))
        .and_then(finite_opt);
    let fee_drag = emp
        .oos_avg_fee_cents
        .or_else(|| top_strategy.as_ref().and_then(|t| t.oos_avg_fee_cents))
        .and_then(finite_opt);
    let slip_drag = finite_or_zero(tournament.slippage_cents);
    let net_ev = lineup
        .combined_oos_expectancy_cents
        .or(emp.oos_net_expectancy_cents)
        .and_then(finite_opt);
    let gross_edge = match (net_ev, fee_drag) {
        (Some(net), Some(f)) => finite_opt(net + f + slip_drag),
        _ => None,
    };
    let realized_wr_pct = emp
        .oos_win_rate
        .or_else(|| top_strategy.as_ref().and_then(|t| t.oos_win_rate))
        .map(|w| finite_or_zero(w * 100.0));
    let break_even_wr_pct = match (avg_entry, fee_drag) {
        (Some(p), Some(f)) => finite_opt((p * 100.0) + f + slip_drag),
        (Some(p), None) => finite_opt((p * 100.0) + slip_drag),
        _ => None,
    };

    let fee_ev = EdgeAuditFeeBreakdown {
        avg_entry_price: avg_entry,
        gross_edge_cents: gross_edge,
        kalshi_fee_drag_cents: fee_drag,
        slippage_drag_cents: slip_drag,
        net_ev_cents: net_ev,
        realized_oos_win_rate_pct: realized_wr_pct,
        break_even_win_rate_pct: break_even_wr_pct,
    };

    let monte_carlo = EdgeAuditMonteCarlo {
        seed: mc.seed,
        paths: mc.paths,
        start_equity_dollars: finite_or_zero(mc.start_equity_dollars),
        p5_final_equity_dollars: finite_or_zero(mc.terminal_equity.p5),
        p50_final_equity_dollars: finite_or_zero(mc.terminal_equity.p50),
        p95_final_equity_dollars: finite_or_zero(mc.terminal_equity.p95),
        median_max_drawdown_pct: finite_or_zero(mc.max_drawdown_pct.p50),
        prob_dd_gte_10_pct: finite_or_zero(mc.prob_dd_gte_10_pct),
        prob_dd_gte_25_pct: finite_or_zero(mc.prob_dd_gte_25_pct),
        prob_profit: finite_or_zero(mc.prob_profit),
    };

    let readiness = EdgeAuditReadiness {
        verdict: scale_plan.readiness_gates.verdict.clone(),
        gates_passed: scale_plan.readiness_gates.passed_count,
        gates_total: scale_plan.readiness_gates.total_gates,
        gates: scale_plan.readiness_gates.gates.clone(),
        observed_median_depth: scale_plan
            .liquidity_ceiling
            .observed_median_depth
            .and_then(finite_opt),
        fill_ratio_at_target: finite_or_zero(scale_plan.liquidity_ceiling.fill_ratio_at_target),
    };

    let mut blocking_reasons = scale_plan.readiness_gates.blocking_reasons.clone();
    if dataset == "fixture" {
        blocking_reasons.insert(
            0,
            "Example fixture tape is illustrative test data, never forward live evidence."
                .to_string(),
        );
    }
    if lineup.fallback_used || !lineup.lineup_complete {
        blocking_reasons.push(lineup.honesty_note.clone());
    }
    if !recalibration.oos_improved {
        blocking_reasons.push(
            "PAV isotonic recalibration did not improve held-out OOS Brier and ECE.".to_string(),
        );
    }

    let go = dataset != "fixture"
        && readiness.verdict == "READY_FOR_OWNER_REVIEW"
        && lineup.lineup_complete
        && !lineup.fallback_used
        && net_ev.is_some_and(|e| e > 0.0)
        && recalibration.oos_improved;

    let (verdict, verdict_label) = if go {
        (
            "GO".to_string(),
            "GO — Edge Validated (Ready for Owner Review)".to_string(),
        )
    } else {
        (
            "NO_GO".to_string(),
            "NO-GO — Paper Research Only".to_string(),
        )
    };

    let stage1_metric = top_strategy
        .as_ref()
        .and_then(|t| t.oos_net_expectancy_cents.map(|c| format!("{c:+.2}c/ct")))
        .unwrap_or_else(|| "n/a".to_string());
    let stage2_metric = format!(
        "{}/4 slots · {}",
        lineup.slots.len(),
        lineup
            .max_abs_correlation
            .map(|r| format!("max |r| {r:.2}"))
            .unwrap_or_else(|| "no pairs".to_string())
    );
    let stage3_metric = match (recalibration.oos_brier_before, recalibration.oos_brier_after) {
        (Some(b), Some(a)) => format!("Brier {b:.4} to {a:.4}"),
        _ => "insufficient OOS".to_string(),
    };
    let stage4_metric = net_ev
        .map(|c| format!("{c:+.2}c net/ct"))
        .unwrap_or_else(|| "n/a".to_string());
    let stage5_metric = format!(
        "{}/{} gates · P(DD>=25%) {:.1}%",
        readiness.gates_passed,
        readiness.gates_total,
        monte_carlo.prob_dd_gte_25_pct * 100.0
    );

    let funnel = vec![
        EdgeAuditStage {
            id: "tournament".to_string(),
            title: format!("{strategies_total} Strategies OOS Tournament"),
            passed: strategies_oos_positive > 0 && tournament.windows_settled_oos >= 30,
            metric: stage1_metric,
            detail: format!(
                "{strategies_oos_positive}/{strategies_total} positive OOS across {} OOS windows",
                tournament.windows_settled_oos
            ),
        },
        EdgeAuditStage {
            id: "lineup".to_string(),
            title: "Uncorrelated 4-Slot Lineup".to_string(),
            passed: lineup.lineup_complete && !lineup.fallback_used,
            metric: stage2_metric,
            detail: lineup.honesty_note.clone(),
        },
        EdgeAuditStage {
            id: "recalibration".to_string(),
            title: "PAV Isotonic Recalibration".to_string(),
            passed: recalibration.oos_improved,
            metric: stage3_metric,
            detail: format!(
                "EV gate accepted {}/{} OOS quotes (delta {:+.2})",
                recalibration.ev_gate_accepted,
                recalibration.ev_gate_total,
                recalibration.ev_gate_pnl_delta_dollars
            ),
        },
        EdgeAuditStage {
            id: "net_ev".to_string(),
            title: "Kalshi Fee & Slippage Net EV".to_string(),
            passed: net_ev.is_some_and(|e| e > 0.0),
            metric: stage4_metric,
            detail: format!(
                "Fee drag {} + slippage {:.1}c",
                fee_drag
                    .map(|f| format!("{f:.2}c"))
                    .unwrap_or_else(|| "n/a".to_string()),
                slip_drag
            ),
        },
        EdgeAuditStage {
            id: "readiness".to_string(),
            title: "Monte Carlo DD & 5-Gate Readiness".to_string(),
            passed: readiness.verdict == "READY_FOR_OWNER_REVIEW",
            metric: stage5_metric,
            detail: if blocking_reasons.is_empty() {
                "All 5 mechanical readiness gates passed.".to_string()
            } else {
                blocking_reasons[0].clone()
            },
        },
    ];

    EdgeAuditReport {
        mode_notice:
            "Paper edge audit — strictly read-only simulation, no venue capital at risk."
                .to_string(),
        dataset: dataset.to_string(),
        verdict,
        verdict_label,
        blocking_reasons,
        slippage_cents: slip_drag,
        strategies_total,
        strategies_oos_positive,
        strategies_forward_proven,
        windows_settled_total: tournament.windows_settled_total,
        windows_settled_is: tournament.windows_settled_is,
        windows_settled_oos: tournament.windows_settled_oos,
        windows_conflicted: tournament.windows_conflicted,
        top_strategy,
        lineup,
        recalibration,
        fee_ev,
        monte_carlo,
        readiness,
        funnel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        iso_z, load_strategies, score_strategy_windows, simulate_slots_windows, window_entry,
        Snapshot, Standard,
    };
    use std::collections::BTreeMap;
    use std::path::Path;

    fn mk_snap(
        ticker: &str,
        fetched_at: i64,
        close_iso: &str,
        result: &str,
        ya: Option<f64>,
        na: Option<f64>,
    ) -> Snapshot {
        Snapshot {
            fetched_at,
            ticker: ticker.to_string(),
            event_ticker: ticker.to_string(),
            series: "KXBTC15M".to_string(),
            close_time: close_iso.to_string(),
            status: if result.is_empty() {
                "active".to_string()
            } else {
                "settled".to_string()
            },
            result: result.to_string(),
            yes_bid: ya.map(|p| (p - 0.02).max(0.01)),
            yes_ask: ya,
            no_bid: na.map(|p| (p - 0.02).max(0.01)),
            no_ask: na,
            last_price: ya,
            yes_bid_size: 150.0,
            no_bid_size: 150.0,
        }
    }

    #[test]
    fn causal_signal_bar_truncation_and_1bar_lookahead_canary() {
        let strategies = load_strategies(Path::new("strategies"));
        let strat = strategies
            .iter()
            .find(|s| s.name == "favourite-late")
            .expect("favourite-late strategy exists");

        // Build a synthetic window where:
        // bar 0 (t=-500s): yes_ask=0.92 (no entry: >0.85 and >300s)
        // bar 1 (t=-240s): yes_ask=0.78 (SIGNAL BAR! triggers entry at 0.78)
        // bar 2 (t=-120s): yes_ask=0.52 (post-signal bar)
        // bar 3 (t=+30s): settled yes
        let close_epoch = 1_765_000_000i64;
        let close_iso = iso_z(close_epoch);
        let full_window = vec![
            mk_snap("KXBTC15M-CAUSAL-1", close_epoch - 500, &close_iso, "", Some(0.92), Some(0.10)),
            mk_snap("KXBTC15M-CAUSAL-1", close_epoch - 240, &close_iso, "", Some(0.78), Some(0.24)),
            mk_snap("KXBTC15M-CAUSAL-1", close_epoch - 120, &close_iso, "", Some(0.52), Some(0.50)),
            mk_snap("KXBTC15M-CAUSAL-1", close_epoch + 30, &close_iso, "yes", None, None),
        ];

        // Cut AT signal bar (bar 1), preserving only settlement metadata at close:
        let cut_at_signal = vec![
            full_window[0].clone(),
            full_window[1].clone(),
            mk_snap("KXBTC15M-CAUSAL-1", close_epoch + 30, &close_iso, "yes", None, None),
        ];

        let entry_full = window_entry(strat, &full_window);
        let entry_cut = window_entry(strat, &cut_at_signal);
        assert_eq!(entry_full, Some(0.78));
        assert_eq!(
            entry_full, entry_cut,
            "causal entry must be invariant to cutting future price bars after signal bar"
        );

        // 1-bar lookahead canary:
        // Build a window where bar 0..=1 do NOT trigger entry, and ONLY bar 2 (1 bar past the cut)
        // triggers entry. Cutting at bar 1 MUST produce None; peeking 1 bar ahead (bar 2) produces Some(0.74).
        let canary_full = vec![
            mk_snap("KXBTC15M-CANARY-1", close_epoch - 500, &close_iso, "", Some(0.92), Some(0.10)),
            mk_snap("KXBTC15M-CANARY-1", close_epoch - 240, &close_iso, "", Some(0.91), Some(0.11)),
            mk_snap("KXBTC15M-CANARY-1", close_epoch - 120, &close_iso, "", Some(0.74), Some(0.28)),
            mk_snap("KXBTC15M-CANARY-1", close_epoch + 30, &close_iso, "yes", None, None),
        ];
        let canary_cut_before_bar2 = vec![
            canary_full[0].clone(),
            canary_full[1].clone(),
            mk_snap("KXBTC15M-CANARY-1", close_epoch + 30, &close_iso, "yes", None, None),
        ];
        assert_eq!(
            window_entry(strat, &canary_cut_before_bar2),
            None,
            "causal scorer cut before future bar must NOT enter"
        );
        assert_eq!(
            window_entry(strat, &canary_full),
            Some(0.74),
            "1-bar lookahead canary: adding bar 2 flips entry from None to Some(0.74) — verifying the canary catches 1-bar leakage"
        );

        // Verify tournament & edge audit build cleanly with zero NaN across multi-window tape.
        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        for i in 0..24 {
            let ce = close_epoch + i * 900;
            let ci = iso_z(ce);
            let tk = format!("KXBTC15M-AUDIT-{i:02}");
            let res = if i % 4 == 0 { "no" } else { "yes" };
            windows.insert(
                tk.clone(),
                vec![
                    mk_snap(&tk, ce - 240, &ci, "", Some(0.76), Some(0.26)),
                    mk_snap(&tk, ce + 30, &ci, res, None, None),
                ],
            );
        }
        let sim = simulate_slots_windows(&strategies, &windows, 100.0);
        let scores: Vec<ScoreReport> = strategies
            .iter()
            .map(|s| score_strategy_windows(s, &windows, Standard::default()))
            .collect();
        let tour = crate::tournament::tournament_from_windows(&windows, &strategies, 1.0);
        let recal = crate::recalibration::recalibration_from_windows(&windows, &strategies, 1.0);
        let scale = crate::scaling::compute_scale_plan(
            &windows,
            &strategies,
            &sim,
            &crate::scaling::ScalePlanRequest::default(),
        );
        let mc = crate::analytics::run_monte_carlo_with_equity(
            &crate::analytics::portfolio_window_returns(&sim),
            100.0,
            200,
            42,
        );
        let audit = build_edge_audit_report("live", &scores, &tour, &recal, &scale, &mc);
        let json = serde_json::to_string(&audit).unwrap();
        assert!(!json.contains("NaN") && !json.contains("Infinity"));
        assert_eq!(audit.funnel.len(), 5);
        assert_eq!(audit.readiness.gates.len(), 5);
    }
}
