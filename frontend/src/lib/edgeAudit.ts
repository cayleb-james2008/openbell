// Client loader + deterministic fallback for GET /api/analytics/edge-audit.
import { type Dataset, type Home, type Market, type Slots, type Strategy, supportsV090 } from "@/lib/desk";
import { runMonteCarlo } from "@/lib/quant";
import { fetchRecalibration, runScalePlan } from "@/lib/scaling";
import { fetchTournament } from "@/lib/tournament";

export type EdgeAuditStage = {
  id: string;
  title: string;
  passed: boolean;
  metric: string;
  detail: string;
};

export type EdgeAuditSlot = {
  slot_number: number;
  strategy: string;
  side: string;
  oos_trades: number;
  oos_net_expectancy_cents: number;
  oos_sharpe: number | null;
  oos_max_drawdown_pct: number;
  max_abs_corr_with_lineup: number | null;
};

export type EdgeAuditGate = {
  id: string;
  label: string;
  passed: boolean;
  observed: number | null;
  threshold: number;
  unit: string;
  detail: string;
};

export type EdgeAuditData = {
  mode_notice: string;
  dataset: string;
  verdict: "GO" | "NO_GO" | string;
  verdict_label: string;
  blocking_reasons: string[];
  slippage_cents: number;
  strategies_total: number;
  strategies_oos_positive: number;
  strategies_forward_proven: number;
  windows_settled_total: number;
  windows_settled_is: number;
  windows_settled_oos: number;
  windows_conflicted: number;
  top_strategy: {
    strategy: string;
    side: string;
    oos_trades: number;
    oos_win_rate: number | null;
    oos_avg_entry_price: number | null;
    oos_avg_fee_cents: number | null;
    oos_net_expectancy_cents: number | null;
    oos_sharpe: number | null;
    oos_max_drawdown_pct: number;
  } | null;
  lineup: {
    slots: EdgeAuditSlot[];
    correlation_cap: number;
    max_abs_correlation: number | null;
    mean_abs_correlation: number | null;
    lineup_complete: boolean;
    fallback_used: boolean;
    combined_oos_expectancy_cents: number | null;
    honesty_note: string;
  };
  recalibration: {
    is_count: number;
    oos_count: number;
    oos_brier_before: number | null;
    oos_brier_after: number | null;
    oos_ece_before: number | null;
    oos_ece_after: number | null;
    oos_improved: boolean;
    min_detectable_edge: number | null;
    ev_gate_total: number;
    ev_gate_accepted: number;
    ev_gate_refused: number;
    ev_gate_accepted_win_rate: number | null;
    ev_gate_before_pnl_dollars: number;
    ev_gate_after_pnl_dollars: number;
    ev_gate_pnl_delta_dollars: number;
  };
  fee_ev: {
    avg_entry_price: number | null;
    gross_edge_cents: number | null;
    kalshi_fee_drag_cents: number | null;
    slippage_drag_cents: number;
    net_ev_cents: number | null;
    realized_oos_win_rate_pct: number | null;
    break_even_win_rate_pct: number | null;
  };
  monte_carlo: {
    seed: number;
    paths: number;
    start_equity_dollars: number;
    p5_final_equity_dollars: number;
    p50_final_equity_dollars: number;
    p95_final_equity_dollars: number;
    median_max_drawdown_pct: number;
    prob_dd_gte_10_pct: number;
    prob_dd_gte_25_pct: number;
    prob_profit: number;
  };
  readiness: {
    verdict: string;
    gates_passed: number;
    gates_total: number;
    gates: EdgeAuditGate[];
    observed_median_depth: number | null;
    fill_ratio_at_target: number;
  };
  funnel: EdgeAuditStage[];
  source: "server" | "client";
  elapsedMs: number;
};

export async function fetchEdgeAudit(
  dataset: Dataset,
  slippageCents: number,
  home: Home | null,
  strategies: Strategy[],
  markets: Market[],
  slots: Slots | null,
): Promise<EdgeAuditData> {
  const t0 = performance.now();
  const slip = Number.isFinite(slippageCents) ? Math.min(5, Math.max(0, slippageCents)) : 1;

  if (supportsV090(home, "edge_audit") || true) {
    try {
      const qs = new URLSearchParams({ dataset, slippage_cents: String(slip) });
      const res = await fetch(`/api/analytics/edge-audit?${qs.toString()}`);
      if (res.ok) {
        const raw = (await res.json()) as Omit<EdgeAuditData, "source" | "elapsedMs">;
        if (raw && Array.isArray(raw.funnel)) {
          return {
            ...raw,
            source: "server",
            elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
          };
        }
      }
    } catch {
      /* fallback to client composition */
    }
  }

  const [tour, recal, scale, mc] = await Promise.all([
    fetchTournament(dataset, slip, home, strategies, markets, slots),
    fetchRecalibration(dataset, home, markets, slots),
    runScalePlan(
      {
        target_daily_dollars: 500,
        starting_bankroll: 1000,
        max_slot_risk_pct: 25,
        slippage_cents: slip,
        max_contracts_per_window: 250,
        active_series_count: 4,
        sizing_mode: "equal_slots",
        paths: 1000,
        seed: 42,
        assumed_edge_cents: null,
        dataset,
      },
      dataset,
      home,
      markets,
      slots,
    ),
    runMonteCarlo(dataset, home, slots, 42, 1000),
  ]);

  const positiveCount = tour.rankings.filter((r) => (r.oosNetExpectancyCents ?? -1) > 0).length;
  const top = tour.rankings[0] ?? null;
  const gatesPassed = scale.readinessGates.gates.filter((g) => g.passed).length;
  const gatesTotal = scale.readinessGates.gates.length;
  const netEv = tour.optimalLineup.combinedOosExpectancyCents ?? scale.oosExpectancyCentsPerContract;
  const avgPrice = null;
  const feeDrag = null;
  const oosTotal = tour.oosWindowsTotal;
  const isTotal = Math.round(oosTotal * (tour.isFraction / Math.max(0.01, tour.oosFraction)));
  const totalWindows = isTotal + oosTotal;
  const medianDdPct = (() => {
    const p50 = mc.percentiles.p50;
    let peak = mc.startEquity;
    let maxDd = 0;
    for (const v of p50) {
      if (v > peak) peak = v;
      const dd = peak > 0 ? ((peak - v) / peak) * 100 : 0;
      if (dd > maxDd) maxDd = dd;
    }
    return Number(maxDd.toFixed(2));
  })();

  return {
    mode_notice: "Paper edge audit: read-only simulation, no venue capital at risk.",
    dataset,
    verdict: "NO_GO",
    verdict_label: "NO-GO: Paper Research Only",
    blocking_reasons: scale.readinessGates.blockingReasons,
    slippage_cents: slip,
    strategies_total: tour.rankings.length,
    strategies_oos_positive: positiveCount,
    strategies_forward_proven: strategies.filter((s) => String(s.verdict || "").startsWith("PROVEN on forward evidence")).length,
    windows_settled_total: totalWindows,
    windows_settled_is: isTotal,
    windows_settled_oos: oosTotal,
    windows_conflicted: 0,
    top_strategy: top
      ? {
          strategy: top.strategy,
          side: top.side,
          oos_trades: top.oosWindows,
          oos_win_rate: top.oosWinRatePct !== null ? top.oosWinRatePct / 100 : null,
          oos_avg_entry_price: avgPrice,
          oos_avg_fee_cents: feeDrag,
          oos_net_expectancy_cents: top.oosNetExpectancyCents,
          oos_sharpe: top.oosSharpe,
          oos_max_drawdown_pct: top.oosMaxDrawdownPct,
        }
      : null,
    lineup: {
      slots: tour.rankings
        .filter((r) => r.inOptimalLineup)
        .slice(0, 4)
        .map((r, i) => ({
          slot_number: i + 1,
          strategy: r.strategy,
          side: r.side,
          oos_trades: r.oosWindows,
          oos_net_expectancy_cents: r.oosNetExpectancyCents ?? 0,
          oos_sharpe: r.oosSharpe,
          oos_max_drawdown_pct: r.oosMaxDrawdownPct,
          max_abs_corr_with_lineup: null,
        })),
      correlation_cap: tour.optimalLineup.maxCorrelationLimit,
      max_abs_correlation: tour.optimalLineup.maxPairwiseAbsCorr,
      mean_abs_correlation: null,
      lineup_complete: !tour.optimalLineup.isFallback,
      fallback_used: tour.optimalLineup.isFallback,
      combined_oos_expectancy_cents: tour.optimalLineup.combinedOosExpectancyCents,
      honesty_note: tour.optimalLineup.honestyNote,
    },
    recalibration: {
      is_count: recal.isCount,
      oos_count: recal.oosCount,
      oos_brier_before: recal.beforeOos.brierScore,
      oos_brier_after: recal.afterOos.brierScore,
      oos_ece_before: recal.beforeOos.ece,
      oos_ece_after: recal.afterOos.ece,
      oos_improved: recal.oosImproved,
      min_detectable_edge: recal.minDetectableEdge,
      ev_gate_total: recal.evGate.totalCount,
      ev_gate_accepted: recal.evGate.acceptedCount,
      ev_gate_refused: recal.evGate.refusedCount,
      ev_gate_accepted_win_rate: recal.evGate.acceptedWinRatePct !== null ? recal.evGate.acceptedWinRatePct / 100 : null,
      ev_gate_before_pnl_dollars: recal.evGate.beforePnlDollars,
      ev_gate_after_pnl_dollars: recal.evGate.afterPnlDollars,
      ev_gate_pnl_delta_dollars: recal.evGate.pnlDeltaDollars,
    },
    fee_ev: {
      avg_entry_price: avgPrice,
      gross_edge_cents: netEv !== null ? netEv + slip : null,
      kalshi_fee_drag_cents: feeDrag,
      slippage_drag_cents: slip,
      net_ev_cents: netEv,
      realized_oos_win_rate_pct: top?.oosWinRatePct ?? null,
      break_even_win_rate_pct: null,
    },
    monte_carlo: {
      seed: mc.seed,
      paths: mc.paths,
      start_equity_dollars: mc.startEquity,
      p5_final_equity_dollars: mc.terminal.p5,
      p50_final_equity_dollars: mc.terminal.p50,
      p95_final_equity_dollars: mc.terminal.p95,
      median_max_drawdown_pct: medianDdPct,
      prob_dd_gte_10_pct: mc.ddProb10,
      prob_dd_gte_25_pct: mc.ddProb25,
      prob_profit: mc.terminal.p50 > mc.startEquity ? 0.5 : 0.0,
    },
    readiness: {
      verdict: scale.readinessGates.verdict,
      gates_passed: gatesPassed,
      gates_total: gatesTotal,
      gates: scale.readinessGates.gates.map((g) => ({
        id: g.id,
        label: g.label,
        passed: g.passed,
        observed: null,
        threshold: 0,
        unit: "",
        detail: g.reason || `${g.actualDisplay} (gate ${g.thresholdDisplay})`,
      })),
      observed_median_depth: scale.liquidityCeiling.maxContractsPerWindow,
      fill_ratio_at_target: scale.liquidityCeiling.fillRatioAtTarget,
    },
    funnel: [
      {
        id: "tournament",
        title: `${tour.rankings.length} Strategies OOS Tournament`,
        passed: positiveCount > 0 && oosTotal >= 30,
        metric: top?.oosNetExpectancyCents !== null && top?.oosNetExpectancyCents !== undefined ? `${top.oosNetExpectancyCents.toFixed(2)}c/ct` : "n/a",
        detail: `${positiveCount}/${tour.rankings.length} positive OOS across ${oosTotal} OOS windows`,
      },
      {
        id: "lineup",
        title: "Uncorrelated 4-Slot Lineup",
        passed: !tour.optimalLineup.isFallback,
        metric: `${tour.optimalLineup.strategies.length}/4 slots`,
        detail: tour.optimalLineup.honestyNote,
      },
      {
        id: "recalibration",
        title: "PAV Isotonic Recalibration",
        passed: recal.oosImproved,
        metric: recal.oosImproved ? "OOS Improved" : "No OOS Gain",
        detail: `EV gate accepted ${recal.evGate.acceptedCount}/${recal.evGate.totalCount} OOS quotes`,
      },
      {
        id: "net_ev",
        title: "Kalshi Fee & Slippage Net EV",
        passed: netEv !== null && netEv > 0,
        metric: netEv !== null ? `${netEv.toFixed(2)}c net/ct` : "n/a",
        detail: `Slippage ${slip.toFixed(1)}c`,
      },
      {
        id: "readiness",
        title: "Monte Carlo DD & 5-Gate Readiness",
        passed: scale.readinessGates.verdict === "READY_FOR_OWNER_REVIEW",
        metric: `${gatesPassed}/${gatesTotal} gates`,
        detail: scale.readinessGates.blockingReasons[0] ?? "All 5 readiness gates passed.",
      },
    ],
    source: "client",
    elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
  };
}
