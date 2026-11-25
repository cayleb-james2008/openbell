// OpenBell Autonomous Edge & Auto-Allocator client + deterministic offline/fixture preview fallback.
// Aligned with GET /api/allocator and POST /api/allocator/plan (X-OpenBell-CSRF: 1).
import { type Dataset, type Market, type Slots, isNum } from "@/lib/desk";
import { kalshiFeeDollars } from "@/lib/fees";

export type AllocatorSizingMode = "quarter_kelly" | "half_kelly" | "equal_winners";
export type AllocatorExecutionMode = "maker" | "taker";

export type AutoAllocatorRequest = {
  sizing_mode: AllocatorSizingMode;
  execution_mode: AllocatorExecutionMode;
  maker_fill_rate: number;
  min_oos_trades: number;
  max_slot_risk_pct: number;
  max_total_heat_pct: number;
  slippage_cents: number;
  starting_bankroll: number;
  require_stress_resilience: boolean;
  enable_regime_throttle: boolean;
  enable_diversification_guard: boolean;
  min_fill_rate_pct: number;
};

export type AssetClusterRisk = {
  cluster_id: string;
  assets: string[];
  raw_heat_pct: number;
  guarded_heat_pct: number;
  max_intra_cluster_corr: number | null;
  penalty_applied_pct: number;
};

export type DiversificationGuardReport = {
  enabled: boolean;
  status: "DIVERSIFIED" | "CLUSTER_CAPPED" | "NO_DATA" | string;
  effective_diversification_score: number;
  max_cross_asset_corr: number | null;
  correlated_cluster_heat_pct: number;
  cluster_heat_cap_pct: number;
  heat_trimmed_pct: number;
  clusters: AssetClusterRisk[];
  diversified_safe_preset_id: string | null;
  summary: string;
};

export type FillRateSensitivityPoint = {
  fill_rate_pct: number;
  adverse_haircut_cents: number;
  effective_ev_cents: number | null;
  projected_daily_net_dollars: number;
  clears_min_fill_gate: boolean;
};

export type DriftGauge = {
  paper_expectancy_cents: number | null;
  shadow_expectancy_cents: number | null;
  drift_cents: number | null;
  status: "HEALTHY" | "DECAY_WARNING" | "DECAY_CRITICAL" | "NO_DATA" | string;
  summary: string;
};

export type EdgeScreenerRow = {
  rank: number;
  strategy: string;
  side: "yes" | "no" | string;
  top_series: string;
  execution_mode: AllocatorExecutionMode | string;
  oos_trades: number;
  avg_entry_cents: number;
  maker_fee_cents: number;
  taker_fee_cents: number;
  fee_edge_saved_cents: number;
  oos_net_expectancy_cents: number;
  breakeven_fill_rate_pct: number;
  fill_headroom_pct: number;
  folds_positive: number;
  folds_total: number;
  worst_fold_expectancy_cents: number | null;
  daily_dollar_contribution: number;
  daily_target_share_pct: number;
  edge_quality_score: number;
  recommended_contracts: number;
  basket_eligible: boolean;
  verdict: string;
};

export type BestBasketPlan = {
  legs: PlannedAllocatorIntent[];
  total_contracts: number;
  total_allocated_dollars: number;
  projected_daily_dollars: number;
  avg_fill_headroom_pct: number;
  ready_for_dry_run_preflight: boolean;
  summary: string;
};

export type BootstrapEquityPoint = {
  day: number;
  p5_equity: number;
  p50_equity: number;
  p95_equity: number;
  stress_1c_p50_equity: number;
};

export type BootstrapStressSummary = {
  trials: number;
  horizon_days: number;
  windows_per_day: number;
  oos_sample_windows: number;
  p5_end_equity: number;
  p50_end_equity: number;
  p95_end_equity: number;
  p95_max_drawdown_pct: number;
  daily_rail_breach_prob_pct: number;
  prob_500_day_pct: number;
  stress_1c_p5_end_equity: number;
  stress_1c_p50_end_equity: number;
  stress_1c_p95_end_equity: number;
  stress_1c_p95_max_drawdown_pct: number;
  stress_1c_daily_rail_breach_prob_pct: number;
  stress_1c_prob_500_day_pct: number;
  trajectory: BootstrapEquityPoint[];
  summary: string;
};

export type StrategyPresetCandidate = {
  id: string;
  label: string;
  subtitle: string;
  config: AutoAllocatorRequest;
  active_winners_count: number;
  net_expectancy_cents: number | null;
  oos_net_pnl_dollars: number;
  oos_max_dd_pct: number;
  bootstrap_p50_equity: number;
  bootstrap_p95_max_dd_pct: number;
  stress_1c_p50_equity: number;
  stress_1c_p95_max_dd_pct: number;
  daily_rail_breach_prob_pct: number;
  stress_1c_daily_rail_breach_prob_pct: number;
  risk_adjusted_score: number;
  safe_gate_passed: boolean;
  is_top_safe: boolean;
  note: string;
};

export type ForwardEnvelopeMonitor = {
  status: "ON_TRACK" | "DRIFT_DEMOTE" | "NO_DATA" | string;
  active_preset_id: string | null;
  active_preset_label: string;
  envelope_day: number;
  envelope_p5_equity: number;
  envelope_p50_equity: number;
  observed_paper_equity: number;
  predicted_ev_cents: number | null;
  observed_forward_ev_cents: number | null;
  expectancy_drift_cents: number | null;
  realized_slippage_vs_model_cents: number;
  daily_rail_breach_prob_pct: number;
  stress_1c_daily_rail_breach_prob_pct: number;
  next_safe_preset_id: string | null;
  next_safe_preset_label: string | null;
  live_armed: boolean;
  summary: string;
};

export type RegimeEdgeBucket = {
  regime: string;
  trades: number;
  win_rate: number;
  gross_expectancy_cents: number;
  fee_drag_cents: number;
  net_expectancy_cents: number;
};

export type StressProfile = {
  extra_slippage_0c_cents: number;
  extra_slippage_1c_cents: number;
  extra_slippage_2c_cents: number;
  folds_positive: number;
  folds_total: number;
  worst_fold_expectancy_cents: number | null;
  stress_resilient: boolean;
};

export type AllocatedWinner = {
  strategy: string;
  side: "yes" | "no" | string;
  execution_mode: AllocatorExecutionMode | string;
  is_net_expectancy_cents: number | null;
  oos_trades: number;
  oos_win_rate: number;
  oos_avg_entry_price: number;
  taker_fee_cents: number;
  maker_fee_cents: number;
  gross_expectancy_cents: number;
  fee_drag_cents: number;
  oos_net_expectancy_cents: number;
  full_kelly_pct: number;
  breakeven_fill_rate_pct: number;
  regime_throttle_mult: number;
  allocated_pct: number;
  allocated_dollars: number;
  rail_clamped_contracts: number;
  max_abs_corr: number | null;
  status: string;
  stress: StressProfile;
};

export type QuarantinedStrategy = {
  strategy: string;
  side: "yes" | "no" | string;
  oos_trades: number;
  oos_net_expectancy_cents: number | null;
  reason: string;
};

export type ComparisonPoint = {
  close_epoch: number;
  legacy_equity: number;
  allocator_equity: number;
};

export type AllocatorComparison = {
  selection_basis: string;
  legacy_equal_taker_end_equity: number;
  legacy_equal_taker_net_pnl: number;
  legacy_equal_taker_max_dd_pct: number;
  legacy_equal_taker_fees_dollars: number;
  auto_allocator_end_equity: number;
  auto_allocator_net_pnl: number;
  auto_allocator_max_dd_pct: number;
  auto_allocator_fees_dollars: number;
  fee_savings_dollars: number;
  equity_delta_dollars: number;
  equity_curve: ComparisonPoint[];
};

export type PlannedAllocatorIntent = {
  strategy: string;
  side: "yes" | "no" | string;
  post_only: boolean;
  target_contracts: number;
  max_limit_cents: number;
  allocated_dollars: number;
  rail_ready: boolean;
  note: string;
};

export type DailyTargetVerdict = {
  target_daily_profit_dollars: number;
  verdict: "GO_500_DAY" | "NO_GO_PAPER" | string;
  projected_daily_net_dollars: number;
  projected_daily_stress_1c_dollars: number;
  projected_daily_stress_2c_dollars: number;
  required_bankroll_dollars: number | null;
  portfolio_net_expectancy_cents: number | null;
  blockers: string[];
};

export type AutoAllocatorReport = {
  execution_mode: AllocatorExecutionMode | string;
  sizing_mode: AllocatorSizingMode | string;
  maker_fill_rate: number;
  min_oos_trades: number;
  max_slot_risk_pct: number;
  max_total_heat_pct: number;
  starting_bankroll: number;
  require_stress_resilience: boolean;
  enable_regime_throttle: boolean;
  enable_diversification_guard: boolean;
  min_fill_rate_pct: number;
  regime_throttle_mult: number;
  current_oos_drawdown_pct: number;
  throttle_reason: string;
  windows_settled_total: number;
  windows_settled_oos: number;
  live_armed: boolean;
  total_allocated_pct: number;
  cash_reserve_pct: number;
  active_winners: AllocatedWinner[];
  quarantined: QuarantinedStrategy[];
  regime_attribution: RegimeEdgeBucket[];
  fill_sensitivity: FillRateSensitivityPoint[];
  drift_gauge: DriftGauge;
  edge_screener: EdgeScreenerRow[];
  best_basket: BestBasketPlan;
  bootstrap_stress: BootstrapStressSummary;
  preset_leaderboard: StrategyPresetCandidate[];
  top_safe_preset: string | null;
  forward_envelope: ForwardEnvelopeMonitor;
  diversification: DiversificationGuardReport;
  comparison: AllocatorComparison;
  planned_intents: PlannedAllocatorIntent[];
  daily_target: DailyTargetVerdict;
  honesty_notes: string[];
  source?: "server" | "client";
};

export const DEFAULT_ALLOCATOR_REQUEST: AutoAllocatorRequest = {
  sizing_mode: "quarter_kelly",
  execution_mode: "maker",
  maker_fill_rate: 0.75,
  min_oos_trades: 10,
  max_slot_risk_pct: 15,
  max_total_heat_pct: 40,
  slippage_cents: 0.5,
  starting_bankroll: 100,
  require_stress_resilience: true,
  enable_regime_throttle: true,
  enable_diversification_guard: true,
  min_fill_rate_pct: 40,
};

const MUTATION_HEADERS: HeadersInit = {
  "Content-Type": "application/json",
  "X-OpenBell-CSRF": "1",
};

export function makerFeeDollars(contracts: number, price: number): number {
  return kalshiFeeDollars(contracts, price, true);
}

function finiteOr(v: unknown, fallback: number): number {
  return isNum(v) && Number.isFinite(v) ? v : fallback;
}

function finiteOpt(v: unknown): number | null {
  return isNum(v) && Number.isFinite(v) ? v : null;
}

function strOr(v: unknown, fallback: string): string {
  return typeof v === "string" && v.trim().length > 0 ? v : fallback;
}

function normalizeReport(raw: unknown): AutoAllocatorReport | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const winnersRaw = Array.isArray(o.active_winners) ? o.active_winners : [];
  const quarantinedRaw = Array.isArray(o.quarantined) ? o.quarantined : [];
  const regRaw = Array.isArray(o.regime_attribution) ? o.regime_attribution : [];
  const fillRaw = Array.isArray(o.fill_sensitivity) ? o.fill_sensitivity : [];
  const driftRaw = (o.drift_gauge && typeof o.drift_gauge === "object" ? o.drift_gauge : {}) as Record<string, unknown>;
  const screenerRaw = Array.isArray(o.edge_screener) ? o.edge_screener : [];
  const basketRaw = (o.best_basket && typeof o.best_basket === "object" ? o.best_basket : {}) as Record<string, unknown>;
  const basketLegsRaw = Array.isArray(basketRaw.legs) ? basketRaw.legs : [];
  const bsRaw = (o.bootstrap_stress && typeof o.bootstrap_stress === "object" ? o.bootstrap_stress : {}) as Record<string, unknown>;
  const bsTrajRaw = Array.isArray(bsRaw.trajectory) ? bsRaw.trajectory : [];
  const presetRaw = Array.isArray(o.preset_leaderboard) ? o.preset_leaderboard : [];
  const envRaw = (o.forward_envelope && typeof o.forward_envelope === "object" ? o.forward_envelope : {}) as Record<string, unknown>;
  const divRaw = (o.diversification && typeof o.diversification === "object" ? o.diversification : {}) as Record<string, unknown>;
  const divClustersRaw = Array.isArray(divRaw.clusters) ? divRaw.clusters : [];
  const intentsRaw = Array.isArray(o.planned_intents) ? o.planned_intents : [];
  const notesRaw = Array.isArray(o.honesty_notes) ? o.honesty_notes : [];
  const compRaw = (o.comparison && typeof o.comparison === "object" ? o.comparison : {}) as Record<string, unknown>;
  const curveRaw = Array.isArray(compRaw.equity_curve) ? compRaw.equity_curve : [];
  const dtRaw = (o.daily_target && typeof o.daily_target === "object" ? o.daily_target : {}) as Record<string, unknown>;

  const startEq = finiteOr(o.starting_bankroll, 100);

  return {
    execution_mode: strOr(o.execution_mode, "maker"),
    sizing_mode: strOr(o.sizing_mode, "quarter_kelly"),
    maker_fill_rate: finiteOr(o.maker_fill_rate, 0.75),
    min_oos_trades: Math.max(1, Math.round(finiteOr(o.min_oos_trades, 10))),
    max_slot_risk_pct: finiteOr(o.max_slot_risk_pct, 15),
    max_total_heat_pct: finiteOr(o.max_total_heat_pct, 40),
    starting_bankroll: startEq,
    require_stress_resilience: typeof o.require_stress_resilience === "boolean" ? o.require_stress_resilience : true,
    enable_regime_throttle: typeof o.enable_regime_throttle === "boolean" ? o.enable_regime_throttle : true,
    enable_diversification_guard:
      typeof o.enable_diversification_guard === "boolean" ? o.enable_diversification_guard : true,
    min_fill_rate_pct: finiteOr(o.min_fill_rate_pct, 40),
    regime_throttle_mult: finiteOr(o.regime_throttle_mult, 1.0),
    current_oos_drawdown_pct: finiteOr(o.current_oos_drawdown_pct, 0),
    throttle_reason: strOr(o.throttle_reason, "1.00x Normal"),
    windows_settled_total: Math.max(0, Math.round(finiteOr(o.windows_settled_total, 0))),
    windows_settled_oos: Math.max(0, Math.round(finiteOr(o.windows_settled_oos, 0))),
    live_armed: Boolean(o.live_armed),
    total_allocated_pct: finiteOr(o.total_allocated_pct, 0),
    cash_reserve_pct: finiteOr(o.cash_reserve_pct, 100),
    active_winners: winnersRaw
      .filter((w): w is Record<string, unknown> => !!w && typeof w === "object")
      .map((w) => {
        const exp = finiteOr(w.oos_net_expectancy_cents, 0);
        const makerFee = finiteOr(w.maker_fee_cents, 0);
        const takerFee = finiteOr(w.taker_fee_cents, 0);
        const execM = strOr(w.execution_mode, "maker");
        const estDrag = execM === "maker" ? makerFee : takerFee;
        const stRaw = (w.stress && typeof w.stress === "object" ? w.stress : {}) as Record<string, unknown>;
        return {
          strategy: strOr(w.strategy, "unnamed"),
          side: strOr(w.side, "yes"),
          execution_mode: execM,
          is_net_expectancy_cents: finiteOpt(w.is_net_expectancy_cents),
          oos_trades: Math.max(0, Math.round(finiteOr(w.oos_trades, 0))),
          oos_win_rate: finiteOr(w.oos_win_rate, 0),
          oos_avg_entry_price: finiteOr(w.oos_avg_entry_price, 0.5),
          taker_fee_cents: takerFee,
          maker_fee_cents: makerFee,
          gross_expectancy_cents: finiteOr(w.gross_expectancy_cents, exp + estDrag),
          fee_drag_cents: finiteOr(w.fee_drag_cents, estDrag),
          oos_net_expectancy_cents: exp,
          full_kelly_pct: finiteOr(w.full_kelly_pct, 0),
          breakeven_fill_rate_pct: finiteOr(w.breakeven_fill_rate_pct, Math.max(0, Math.min(100, (1 - exp / 2) * 100))),
          regime_throttle_mult: finiteOr(w.regime_throttle_mult, 1.0),
          allocated_pct: finiteOr(w.allocated_pct, 0),
          allocated_dollars: finiteOr(w.allocated_dollars, 0),
          rail_clamped_contracts: Math.max(0, Math.round(finiteOr(w.rail_clamped_contracts, 0))),
          max_abs_corr: finiteOpt(w.max_abs_corr),
          status: strOr(w.status, "ACTIVE_WINNER"),
          stress: {
            extra_slippage_0c_cents: finiteOr(stRaw.extra_slippage_0c_cents, exp),
            extra_slippage_1c_cents: finiteOr(stRaw.extra_slippage_1c_cents, exp - 1),
            extra_slippage_2c_cents: finiteOr(stRaw.extra_slippage_2c_cents, exp - 2),
            folds_positive: Math.max(0, Math.round(finiteOr(stRaw.folds_positive, 1))),
            folds_total: Math.max(1, Math.round(finiteOr(stRaw.folds_total, 1))),
            worst_fold_expectancy_cents: finiteOpt(stRaw.worst_fold_expectancy_cents),
            stress_resilient: typeof stRaw.stress_resilient === "boolean" ? stRaw.stress_resilient : exp - 1 > 0,
          },
        };
      }),
    quarantined: quarantinedRaw
      .filter((q): q is Record<string, unknown> => !!q && typeof q === "object")
      .map((q) => ({
        strategy: strOr(q.strategy, "unnamed"),
        side: strOr(q.side, "yes"),
        oos_trades: Math.max(0, Math.round(finiteOr(q.oos_trades, 0))),
        oos_net_expectancy_cents: finiteOpt(q.oos_net_expectancy_cents),
        reason: strOr(q.reason, "negative OOS expectancy"),
      })),
    regime_attribution: regRaw
      .filter((r): r is Record<string, unknown> => !!r && typeof r === "object")
      .map((r) => ({
        regime: strOr(r.regime, "Balanced"),
        trades: Math.max(0, Math.round(finiteOr(r.trades, 0))),
        win_rate: finiteOr(r.win_rate, 0),
        gross_expectancy_cents: finiteOr(r.gross_expectancy_cents, 0),
        fee_drag_cents: finiteOr(r.fee_drag_cents, 0),
        net_expectancy_cents: finiteOr(r.net_expectancy_cents, 0),
      })),
    fill_sensitivity: fillRaw
      .filter((f): f is Record<string, unknown> => !!f && typeof f === "object")
      .map((f) => ({
        fill_rate_pct: finiteOr(f.fill_rate_pct, 75),
        adverse_haircut_cents: finiteOr(f.adverse_haircut_cents, 0),
        effective_ev_cents: finiteOpt(f.effective_ev_cents),
        projected_daily_net_dollars: finiteOr(f.projected_daily_net_dollars, 0),
        clears_min_fill_gate: Boolean(f.clears_min_fill_gate),
      })),
    drift_gauge: {
      paper_expectancy_cents: finiteOpt(driftRaw.paper_expectancy_cents),
      shadow_expectancy_cents: finiteOpt(driftRaw.shadow_expectancy_cents),
      drift_cents: finiteOpt(driftRaw.drift_cents),
      status: strOr(driftRaw.status, "NO_DATA"),
      summary: strOr(driftRaw.summary, "Awaiting OOS drift readout."),
    },
    edge_screener: screenerRaw
      .filter((r): r is Record<string, unknown> => !!r && typeof r === "object")
      .map((r, i) => ({
        rank: Math.max(1, Math.round(finiteOr(r.rank, i + 1))),
        strategy: strOr(r.strategy, "unnamed"),
        side: strOr(r.side, "yes"),
        top_series: strOr(r.top_series, "KXBTC15M"),
        execution_mode: strOr(r.execution_mode, "maker"),
        oos_trades: Math.max(0, Math.round(finiteOr(r.oos_trades, 0))),
        avg_entry_cents: Math.min(99, Math.max(1, Math.round(finiteOr(r.avg_entry_cents, 50)))),
        maker_fee_cents: finiteOr(r.maker_fee_cents, 0),
        taker_fee_cents: finiteOr(r.taker_fee_cents, 0),
        fee_edge_saved_cents: finiteOr(r.fee_edge_saved_cents, 0),
        oos_net_expectancy_cents: finiteOr(r.oos_net_expectancy_cents, 0),
        breakeven_fill_rate_pct: finiteOr(r.breakeven_fill_rate_pct, 0),
        fill_headroom_pct: finiteOr(r.fill_headroom_pct, 0),
        folds_positive: Math.max(0, Math.round(finiteOr(r.folds_positive, 0))),
        folds_total: Math.max(0, Math.round(finiteOr(r.folds_total, 0))),
        worst_fold_expectancy_cents: finiteOpt(r.worst_fold_expectancy_cents),
        daily_dollar_contribution: finiteOr(r.daily_dollar_contribution, 0),
        daily_target_share_pct: finiteOr(r.daily_target_share_pct, 0),
        edge_quality_score: finiteOr(r.edge_quality_score, 0),
        recommended_contracts: Math.max(0, Math.round(finiteOr(r.recommended_contracts, 0))),
        basket_eligible: Boolean(r.basket_eligible),
        verdict: strOr(r.verdict, "QUARANTINED"),
      })),
    best_basket: {
      legs: basketLegsRaw
        .filter((it): it is Record<string, unknown> => !!it && typeof it === "object")
        .map((it) => ({
          strategy: strOr(it.strategy, "unnamed"),
          side: strOr(it.side, "yes"),
          post_only: Boolean(it.post_only),
          target_contracts: Math.max(0, Math.round(finiteOr(it.target_contracts, 0))),
          max_limit_cents: Math.min(99, Math.max(1, Math.round(finiteOr(it.max_limit_cents, 50)))),
          allocated_dollars: finiteOr(it.allocated_dollars, 0),
          rail_ready: Boolean(it.rail_ready),
          note: strOr(it.note, ""),
        })),
      total_contracts: Math.max(0, Math.round(finiteOr(basketRaw.total_contracts, 0))),
      total_allocated_dollars: finiteOr(basketRaw.total_allocated_dollars, 0),
      projected_daily_dollars: finiteOr(basketRaw.projected_daily_dollars, 0),
      avg_fill_headroom_pct: finiteOr(basketRaw.avg_fill_headroom_pct, 0),
      ready_for_dry_run_preflight: Boolean(basketRaw.ready_for_dry_run_preflight),
      summary: strOr(basketRaw.summary, "No basket-eligible causal winners."),
    },
    bootstrap_stress: {
      trials: Math.max(0, Math.round(finiteOr(bsRaw.trials, 1000))),
      horizon_days: Math.max(1, Math.round(finiteOr(bsRaw.horizon_days, 30))),
      windows_per_day: Math.max(1, Math.round(finiteOr(bsRaw.windows_per_day, 96))),
      oos_sample_windows: Math.max(0, Math.round(finiteOr(bsRaw.oos_sample_windows, 0))),
      p5_end_equity: finiteOr(bsRaw.p5_end_equity, startEq),
      p50_end_equity: finiteOr(bsRaw.p50_end_equity, startEq),
      p95_end_equity: finiteOr(bsRaw.p95_end_equity, startEq),
      p95_max_drawdown_pct: finiteOr(bsRaw.p95_max_drawdown_pct, 0),
      daily_rail_breach_prob_pct: finiteOr(bsRaw.daily_rail_breach_prob_pct, 0),
      prob_500_day_pct: finiteOr(bsRaw.prob_500_day_pct, 0),
      stress_1c_p5_end_equity: finiteOr(bsRaw.stress_1c_p5_end_equity, startEq),
      stress_1c_p50_end_equity: finiteOr(bsRaw.stress_1c_p50_end_equity, startEq),
      stress_1c_p95_end_equity: finiteOr(bsRaw.stress_1c_p95_end_equity, startEq),
      stress_1c_p95_max_drawdown_pct: finiteOr(bsRaw.stress_1c_p95_max_drawdown_pct, 0),
      stress_1c_daily_rail_breach_prob_pct: finiteOr(bsRaw.stress_1c_daily_rail_breach_prob_pct, 0),
      stress_1c_prob_500_day_pct: finiteOr(bsRaw.stress_1c_prob_500_day_pct, 0),
      trajectory: bsTrajRaw
        .filter((pt): pt is Record<string, unknown> => !!pt && typeof pt === "object")
        .map((pt, i) => ({
          day: Math.max(0, Math.round(finiteOr(pt.day, i))),
          p5_equity: finiteOr(pt.p5_equity, startEq),
          p50_equity: finiteOr(pt.p50_equity, startEq),
          p95_equity: finiteOr(pt.p95_equity, startEq),
          stress_1c_p50_equity: finiteOr(pt.stress_1c_p50_equity, startEq),
        })),
      summary: strOr(bsRaw.summary, "Awaiting OOS causal bootstrap stress simulation."),
    },
    preset_leaderboard: presetRaw
      .filter((p): p is Record<string, unknown> => !!p && typeof p === "object")
      .map((p) => {
        const cfgRaw = (p.config && typeof p.config === "object" ? p.config : {}) as Record<string, unknown>;
        return {
          id: strOr(p.id, "preset"),
          label: strOr(p.label, "Preset"),
          subtitle: strOr(p.subtitle, ""),
          config: {
            sizing_mode: (strOr(cfgRaw.sizing_mode, "quarter_kelly") as AllocatorSizingMode),
            execution_mode: (strOr(cfgRaw.execution_mode, "maker") as AllocatorExecutionMode),
            maker_fill_rate: finiteOr(cfgRaw.maker_fill_rate, 0.75),
            min_oos_trades: Math.max(1, Math.round(finiteOr(cfgRaw.min_oos_trades, 30))),
            max_slot_risk_pct: finiteOr(cfgRaw.max_slot_risk_pct, 15),
            max_total_heat_pct: finiteOr(cfgRaw.max_total_heat_pct, 60),
            slippage_cents: finiteOr(cfgRaw.slippage_cents, 0.5),
            starting_bankroll: finiteOr(cfgRaw.starting_bankroll, startEq),
            require_stress_resilience: typeof cfgRaw.require_stress_resilience === "boolean" ? cfgRaw.require_stress_resilience : true,
            enable_regime_throttle: typeof cfgRaw.enable_regime_throttle === "boolean" ? cfgRaw.enable_regime_throttle : true,
            enable_diversification_guard:
              typeof cfgRaw.enable_diversification_guard === "boolean"
                ? cfgRaw.enable_diversification_guard
                : true,
            min_fill_rate_pct: finiteOr(cfgRaw.min_fill_rate_pct, 50),
          },
          active_winners_count: Math.max(0, Math.round(finiteOr(p.active_winners_count, 0))),
          net_expectancy_cents: finiteOpt(p.net_expectancy_cents),
          oos_net_pnl_dollars: finiteOr(p.oos_net_pnl_dollars, 0),
          oos_max_dd_pct: finiteOr(p.oos_max_dd_pct, 0),
          bootstrap_p50_equity: finiteOr(p.bootstrap_p50_equity, startEq),
          bootstrap_p95_max_dd_pct: finiteOr(p.bootstrap_p95_max_dd_pct, 0),
          stress_1c_p50_equity: finiteOr(p.stress_1c_p50_equity, startEq),
          stress_1c_p95_max_dd_pct: finiteOr(p.stress_1c_p95_max_dd_pct, 0),
          daily_rail_breach_prob_pct: finiteOr(p.daily_rail_breach_prob_pct, 0),
          stress_1c_daily_rail_breach_prob_pct: finiteOr(p.stress_1c_daily_rail_breach_prob_pct, 0),
          risk_adjusted_score: finiteOr(p.risk_adjusted_score, 0),
          safe_gate_passed: Boolean(p.safe_gate_passed),
          is_top_safe: Boolean(p.is_top_safe),
          note: strOr(p.note, ""),
        };
      }),
    top_safe_preset: typeof o.top_safe_preset === "string" && o.top_safe_preset.trim().length > 0 ? o.top_safe_preset : null,
    forward_envelope: {
      status: strOr(envRaw.status, "NO_DATA"),
      active_preset_id: typeof envRaw.active_preset_id === "string" && envRaw.active_preset_id.trim().length > 0 ? envRaw.active_preset_id : null,
      active_preset_label: strOr(envRaw.active_preset_label, "Custom"),
      envelope_day: Math.max(1, Math.min(30, Math.round(finiteOr(envRaw.envelope_day, 1)))),
      envelope_p5_equity: finiteOr(envRaw.envelope_p5_equity, startEq),
      envelope_p50_equity: finiteOr(envRaw.envelope_p50_equity, startEq),
      observed_paper_equity: finiteOr(envRaw.observed_paper_equity, startEq),
      predicted_ev_cents: finiteOpt(envRaw.predicted_ev_cents),
      observed_forward_ev_cents: finiteOpt(envRaw.observed_forward_ev_cents),
      expectancy_drift_cents: finiteOpt(envRaw.expectancy_drift_cents),
      realized_slippage_vs_model_cents: finiteOr(envRaw.realized_slippage_vs_model_cents, 0),
      daily_rail_breach_prob_pct: finiteOr(envRaw.daily_rail_breach_prob_pct, 0),
      stress_1c_daily_rail_breach_prob_pct: finiteOr(envRaw.stress_1c_daily_rail_breach_prob_pct, 0),
      next_safe_preset_id: typeof envRaw.next_safe_preset_id === "string" && envRaw.next_safe_preset_id.trim().length > 0 ? envRaw.next_safe_preset_id : null,
      next_safe_preset_label: typeof envRaw.next_safe_preset_label === "string" && envRaw.next_safe_preset_label.trim().length > 0 ? envRaw.next_safe_preset_label : null,
      live_armed: Boolean(envRaw.live_armed),
      summary: strOr(envRaw.summary, "Awaiting active OOS winners for forward-test envelope tracking."),
    },
    diversification: {
      enabled: typeof divRaw.enabled === "boolean" ? divRaw.enabled : true,
      status: strOr(divRaw.status, "NO_DATA"),
      effective_diversification_score: finiteOr(divRaw.effective_diversification_score, 0),
      max_cross_asset_corr: finiteOpt(divRaw.max_cross_asset_corr),
      correlated_cluster_heat_pct: finiteOr(divRaw.correlated_cluster_heat_pct, 0),
      cluster_heat_cap_pct: finiteOr(divRaw.cluster_heat_cap_pct, 26),
      heat_trimmed_pct: finiteOr(divRaw.heat_trimmed_pct, 0),
      clusters: divClustersRaw
        .filter((c): c is Record<string, unknown> => !!c && typeof c === "object")
        .map((c) => ({
          cluster_id: strOr(c.cluster_id, "BTC"),
          assets: Array.isArray(c.assets)
            ? c.assets.filter((a): a is string => typeof a === "string")
            : ["BTC"],
          raw_heat_pct: finiteOr(c.raw_heat_pct, 0),
          guarded_heat_pct: finiteOr(c.guarded_heat_pct, 0),
          max_intra_cluster_corr: finiteOpt(c.max_intra_cluster_corr),
          penalty_applied_pct: finiteOr(c.penalty_applied_pct, 0),
        })),
      diversified_safe_preset_id:
        typeof divRaw.diversified_safe_preset_id === "string" &&
        divRaw.diversified_safe_preset_id.trim().length > 0
          ? divRaw.diversified_safe_preset_id
          : null,
      summary: strOr(
        divRaw.summary,
        "Cross-asset correlation cluster guard ready (BTC/ETH/SOL/XRP/DOGE).",
      ),
    },
    comparison: {
      selection_basis: strOr(compRaw.selection_basis, "causal_is_to_oos"),
      legacy_equal_taker_end_equity: finiteOr(compRaw.legacy_equal_taker_end_equity, startEq),
      legacy_equal_taker_net_pnl: finiteOr(compRaw.legacy_equal_taker_net_pnl, 0),
      legacy_equal_taker_max_dd_pct: finiteOr(compRaw.legacy_equal_taker_max_dd_pct, 0),
      legacy_equal_taker_fees_dollars: finiteOr(compRaw.legacy_equal_taker_fees_dollars, 0),
      auto_allocator_end_equity: finiteOr(compRaw.auto_allocator_end_equity, startEq),
      auto_allocator_net_pnl: finiteOr(compRaw.auto_allocator_net_pnl, 0),
      auto_allocator_max_dd_pct: finiteOr(compRaw.auto_allocator_max_dd_pct, 0),
      auto_allocator_fees_dollars: finiteOr(compRaw.auto_allocator_fees_dollars, 0),
      fee_savings_dollars: finiteOr(compRaw.fee_savings_dollars, 0),
      equity_delta_dollars: finiteOr(compRaw.equity_delta_dollars, 0),
      equity_curve: curveRaw
        .filter((pt): pt is Record<string, unknown> => !!pt && typeof pt === "object")
        .map((pt) => ({
          close_epoch: Math.round(finiteOr(pt.close_epoch, 0)),
          legacy_equity: finiteOr(pt.legacy_equity, startEq),
          allocator_equity: finiteOr(pt.allocator_equity, startEq),
        })),
    },
    planned_intents: intentsRaw
      .filter((it): it is Record<string, unknown> => !!it && typeof it === "object")
      .map((it) => ({
        strategy: strOr(it.strategy, "unnamed"),
        side: strOr(it.side, "yes"),
        post_only: Boolean(it.post_only),
        target_contracts: Math.max(0, Math.round(finiteOr(it.target_contracts, 0))),
        max_limit_cents: Math.min(99, Math.max(1, Math.round(finiteOr(it.max_limit_cents, 50)))),
        allocated_dollars: finiteOr(it.allocated_dollars, 0),
        rail_ready: Boolean(it.rail_ready),
        note: strOr(it.note, ""),
      })),
    daily_target: {
      target_daily_profit_dollars: finiteOr(dtRaw.target_daily_profit_dollars, 500),
      verdict: strOr(dtRaw.verdict, "NO_GO_PAPER"),
      projected_daily_net_dollars: finiteOr(dtRaw.projected_daily_net_dollars, 0),
      projected_daily_stress_1c_dollars: finiteOr(dtRaw.projected_daily_stress_1c_dollars, 0),
      projected_daily_stress_2c_dollars: finiteOr(dtRaw.projected_daily_stress_2c_dollars, 0),
      required_bankroll_dollars: finiteOpt(dtRaw.required_bankroll_dollars),
      portfolio_net_expectancy_cents: finiteOpt(dtRaw.portfolio_net_expectancy_cents),
      blockers: Array.isArray(dtRaw.blockers)
        ? dtRaw.blockers.filter((b): b is string => typeof b === "string" && b.trim().length > 0)
        : [],
    },
    honesty_notes: notesRaw.filter((n): n is string => typeof n === "string" && n.trim().length > 0),
    source: "server",
  };
}

function makeOfflineDiversification(
  params: AutoAllocatorRequest,
  totalHeatPct: number,
  hasWinners: boolean,
): DiversificationGuardReport {
  return {
    enabled: params.enable_diversification_guard,
    status: hasWinners ? "DIVERSIFIED" : "NO_DATA",
    effective_diversification_score: hasWinners ? 35 : 0,
    max_cross_asset_corr: null,
    correlated_cluster_heat_pct: 0,
    cluster_heat_cap_pct: Number(
      Math.max(params.max_slot_risk_pct, params.max_total_heat_pct * 0.65).toFixed(1),
    ),
    heat_trimmed_pct: 0,
    clusters: hasWinners
      ? [
          {
            cluster_id: "BTC",
            assets: ["BTC"],
            raw_heat_pct: totalHeatPct,
            guarded_heat_pct: totalHeatPct,
            max_intra_cluster_corr: null,
            penalty_applied_pct: 0,
          },
        ]
      : [],
    diversified_safe_preset_id: null,
    summary: hasWinners
      ? "Offline preview: 1 asset cluster (BTC) · run server pipeline for full cross-asset correlation guard."
      : "No active slots available for cross-asset correlation clustering.",
  };
}

export function computeClientFallbackReport(
  params: AutoAllocatorRequest,
  slots: Slots | null,
  markets: Market[],
): AutoAllocatorReport {
  const startEq = params.starting_bankroll > 0 ? params.starting_bankroll : 100;
  const trades = (slots?.trades ?? [])
    .filter((t) => t && isNum(t.close_epoch) && isNum(t.price) && isNum(t.pnl_dollars))
    .slice()
    .sort((a, b) => a.close_epoch - b.close_epoch);

  const settledTotal = markets.filter((m) => m && (m.result === "yes" || m.result === "no")).length;
  const settledOos = Math.round(settledTotal * 0.4);

  if (trades.length === 0) {
    return {
      ...params,
      regime_throttle_mult: 1.0,
      current_oos_drawdown_pct: 0,
      throttle_reason: params.enable_regime_throttle ? "1.00x Normal (OOS DD 0.0%)" : "Disabled (1.00x)",
      windows_settled_total: settledTotal,
      windows_settled_oos: settledOos,
      live_armed: false,
      total_allocated_pct: 0,
      cash_reserve_pct: 100,
      active_winners: [],
      quarantined: [],
      regime_attribution: [],
      fill_sensitivity: [],
      drift_gauge: {
        paper_expectancy_cents: null,
        shadow_expectancy_cents: null,
        drift_cents: null,
        status: "NO_DATA",
        summary: "No settled paper trades loaded yet.",
      },
      edge_screener: [],
      best_basket: {
        legs: [],
        total_contracts: 0,
        total_allocated_dollars: 0,
        projected_daily_dollars: 0,
        avg_fill_headroom_pct: 0,
        ready_for_dry_run_preflight: false,
        summary: "No basket-eligible causal winners.",
      },
      bootstrap_stress: {
        trials: 1000,
        horizon_days: 30,
        windows_per_day: 96,
        oos_sample_windows: 0,
        p5_end_equity: startEq,
        p50_end_equity: startEq,
        p95_end_equity: startEq,
        p95_max_drawdown_pct: 0,
        daily_rail_breach_prob_pct: 0,
        prob_500_day_pct: 0,
        stress_1c_p5_end_equity: startEq,
        stress_1c_p50_end_equity: startEq,
        stress_1c_p95_end_equity: startEq,
        stress_1c_p95_max_drawdown_pct: 0,
        stress_1c_daily_rail_breach_prob_pct: 0,
        stress_1c_prob_500_day_pct: 0,
        trajectory: [],
        summary: "Awaiting OOS trades for 1,000-trial causal bootstrap simulation.",
      },
      preset_leaderboard: [],
      top_safe_preset: null,
      forward_envelope: {
        status: "NO_DATA",
        active_preset_id: null,
        active_preset_label: "Paper Preview",
        envelope_day: 1,
        envelope_p5_equity: startEq,
        envelope_p50_equity: startEq,
        observed_paper_equity: startEq,
        predicted_ev_cents: null,
        observed_forward_ev_cents: null,
        expectancy_drift_cents: null,
        realized_slippage_vs_model_cents: 0,
        daily_rail_breach_prob_pct: 0,
        stress_1c_daily_rail_breach_prob_pct: 0,
        next_safe_preset_id: null,
        next_safe_preset_label: null,
        live_armed: false,
        summary: "Awaiting OOS winners for paper envelope tracking.",
      },
      diversification: makeOfflineDiversification(params, 0, false),
      comparison: {
        selection_basis: "causal_is_to_oos",
        legacy_equal_taker_end_equity: startEq,
        legacy_equal_taker_net_pnl: 0,
        legacy_equal_taker_max_dd_pct: 0,
        legacy_equal_taker_fees_dollars: 0,
        auto_allocator_end_equity: startEq,
        auto_allocator_net_pnl: 0,
        auto_allocator_max_dd_pct: 0,
        auto_allocator_fees_dollars: 0,
        fee_savings_dollars: 0,
        equity_delta_dollars: 0,
        equity_curve: [],
      },
      planned_intents: [],
      daily_target: {
        target_daily_profit_dollars: 500,
        verdict: "NO_GO_PAPER",
        projected_daily_net_dollars: 0,
        projected_daily_stress_1c_dollars: 0,
        projected_daily_stress_2c_dollars: 0,
        required_bankroll_dollars: null,
        portfolio_net_expectancy_cents: null,
        blockers: ["No settled paper trades loaded yet; 100% cash reserve enforced."],
      },
      honesty_notes: [
        "Offline preview: no settled paper trades loaded yet; 100% cash reserve enforced.",
      ],
      source: "client",
    };
  }

  const splitIdx = Math.floor(trades.length * 0.6);
  const isTrades = trades.slice(0, splitIdx);
  const oosTrades = trades.slice(splitIdx);

  const summarizeSplit = (slice: typeof trades) => {
    const byStrat = new Map<
      string,
      { side: string; count: number; wins: number; sumPrice: number; takerFeeSum: number; makerFeeSum: number; netCentsSum: number }
    >();
    for (const t of slice) {
      const name = typeof t.strategy === "string" && t.strategy ? t.strategy : "unnamed";
      const p = Math.min(0.99, Math.max(0.01, t.price));
      const won = t.pnl_dollars > 0;
      const takerF = 0.07 * p * (1 - p) * 100;
      const makerF = 0.0175 * p * (1 - p) * 100;
      const activeFeeCents = params.execution_mode === "maker" ? makerF : takerF;
      const slipCents = params.execution_mode === "maker" ? 0 : params.slippage_cents;
      const grossCents = (won ? 1 - p : -p) * 100;
      const netCents = grossCents - activeFeeCents - slipCents;
      const cur = byStrat.get(name) ?? {
        side: t.side === "no" ? "no" : "yes",
        count: 0,
        wins: 0,
        sumPrice: 0,
        takerFeeSum: 0,
        makerFeeSum: 0,
        netCentsSum: 0,
      };
      cur.count += 1;
      if (won) cur.wins += 1;
      cur.sumPrice += p;
      cur.takerFeeSum += takerF;
      cur.makerFeeSum += makerF;
      cur.netCentsSum += netCents;
      byStrat.set(name, cur);
    }
    return byStrat;
  };

  const isMap = summarizeSplit(isTrades);
  const oosMap = summarizeSplit(oosTrades);

  const rawWinners: AllocatedWinner[] = [];
  const quarantined: QuarantinedStrategy[] = [];

  for (const [strategy, s] of oosMap.entries()) {
    const avgP = s.sumPrice / s.count;
    const wr = s.wins / s.count;
    const meanNetCents = s.netCentsSum / s.count;
    const meanTakerCents = s.takerFeeSum / s.count;
    const meanMakerCents = s.makerFeeSum / s.count;
    const isStats = isMap.get(strategy);
    const isExpCents = isStats && isStats.count > 0 ? Number((isStats.netCentsSum / isStats.count).toFixed(2)) : null;

    if (s.count < params.min_oos_trades) {
      quarantined.push({
        strategy,
        side: s.side,
        oos_trades: s.count,
        oos_net_expectancy_cents: Number(meanNetCents.toFixed(2)),
        reason: `insufficient OOS depth (< ${params.min_oos_trades})`,
      });
      continue;
    }
    if (meanNetCents <= 0) {
      quarantined.push({
        strategy,
        side: s.side,
        oos_trades: s.count,
        oos_net_expectancy_cents: Number(meanNetCents.toFixed(2)),
        reason: "negative OOS expectancy",
      });
      continue;
    }

    const activeFeeDollars = (params.execution_mode === "maker" ? meanMakerCents : meanTakerCents) / 100;
    const slipDollars = params.execution_mode === "maker" ? 0 : params.slippage_cents / 100;
    const cEff = Math.min(0.99, Math.max(0.01, avgP + activeFeeDollars + slipDollars));
    const b = (1 - cEff) / cEff;
    const fullKelly = Math.max(0, ((wr * b - (1 - wr)) / b) * 100);
    const scale = params.sizing_mode === "half_kelly" ? 0.5 : params.sizing_mode === "quarter_kelly" ? 0.25 : 0;
    const rawPct = scale > 0 ? Math.min(params.max_slot_risk_pct, fullKelly * scale) : params.max_slot_risk_pct;

    const feeDragCents = Number((activeFeeDollars * 100 + slipDollars * 100).toFixed(3));
    rawWinners.push({
      strategy,
      side: s.side,
      execution_mode: params.execution_mode,
      is_net_expectancy_cents: isExpCents,
      oos_trades: s.count,
      oos_win_rate: Number(wr.toFixed(4)),
      oos_avg_entry_price: Number(avgP.toFixed(4)),
      taker_fee_cents: Number(meanTakerCents.toFixed(3)),
      maker_fee_cents: Number(meanMakerCents.toFixed(3)),
      gross_expectancy_cents: Number((meanNetCents + feeDragCents).toFixed(2)),
      fee_drag_cents: feeDragCents,
      oos_net_expectancy_cents: Number(meanNetCents.toFixed(2)),
      full_kelly_pct: Number(fullKelly.toFixed(2)),
      breakeven_fill_rate_pct: Number((Math.max(0, Math.min(1, 1 - meanNetCents / 2)) * 100).toFixed(1)),
      regime_throttle_mult: 1.0,
      allocated_pct: Number(rawPct.toFixed(2)),
      allocated_dollars: 0,
      rail_clamped_contracts: 0,
      max_abs_corr: null,
      status: "ACTIVE_WINNER",
      stress: {
        extra_slippage_0c_cents: Number(meanNetCents.toFixed(2)),
        extra_slippage_1c_cents: Number((meanNetCents - 1).toFixed(2)),
        extra_slippage_2c_cents: Number((meanNetCents - 2).toFixed(2)),
        folds_positive: meanNetCents > 0 ? 1 : 0,
        folds_total: 1,
        worst_fold_expectancy_cents: Number(meanNetCents.toFixed(2)),
        stress_resilient: meanNetCents - 1 > 0,
      },
    });
  }

  rawWinners.sort((a, b) => b.oos_net_expectancy_cents - a.oos_net_expectancy_cents);
  if (params.sizing_mode === "equal_winners" && rawWinners.length > 0) {
    const share = Math.min(params.max_slot_risk_pct, params.max_total_heat_pct / rawWinners.length);
    for (const w of rawWinners) w.allocated_pct = Number(share.toFixed(2));
  }
  const sumPct = rawWinners.reduce((acc, w) => acc + w.allocated_pct, 0);
  if (sumPct > params.max_total_heat_pct && sumPct > 0) {
    const k = params.max_total_heat_pct / sumPct;
    for (const w of rawWinners) w.allocated_pct = Number((w.allocated_pct * k).toFixed(2));
  }

  let totalPct = 0;
  const planned_intents: PlannedAllocatorIntent[] = [];
  for (const w of rawWinners) {
    totalPct += w.allocated_pct;
    w.allocated_dollars = Number(((startEq * w.allocated_pct) / 100).toFixed(2));
    const limitCents = Math.min(99, Math.max(1, Math.round(w.oos_avg_entry_price * 100)));
    const cts = Math.min(25, Math.max(0, Math.floor(w.allocated_dollars / Math.max(0.01, limitCents / 100))));
    w.rail_clamped_contracts = cts;
    planned_intents.push({
      strategy: w.strategy,
      side: w.side,
      post_only: params.execution_mode === "maker",
      target_contracts: cts,
      max_limit_cents: limitCents,
      allocated_dollars: w.allocated_dollars,
      rail_ready: false,
      note: "Offline paper preview (server endpoint unavailable).",
    });
  }

  // Causal IS-selected winners for OOS replay
  const causalWinnerSet = new Set<string>();
  for (const [strat, s] of isMap.entries()) {
    if (s.count >= params.min_oos_trades && s.netCentsSum > 0) causalWinnerSet.add(strat);
  }
  const activeSet = causalWinnerSet.size > 0 || isTrades.length > 0 ? causalWinnerSet : new Set(rawWinners.map((w) => w.strategy));

  let legacyEq = startEq;
  let allocEq = startEq;
  let legacyPeak = startEq;
  let allocPeak = startEq;
  let legacyMaxDd = 0;
  let allocMaxDd = 0;
  let legacyFees = 0;
  let allocFees = 0;
  const curve: ComparisonPoint[] = [];

  for (const t of oosTrades) {
    const p = Math.min(0.99, Math.max(0.01, t.price));
    const tf = kalshiFeeDollars(1, p, false);
    const mf = kalshiFeeDollars(1, p, true);
    legacyFees += tf;
    legacyEq += t.pnl_dollars;
    if (legacyEq > legacyPeak) legacyPeak = legacyEq;
    legacyMaxDd = Math.max(legacyMaxDd, ((legacyPeak - legacyEq) / Math.max(1, legacyPeak)) * 100);

    if (activeSet.has(t.strategy)) {
      const activeF = params.execution_mode === "maker" ? mf : tf;
      allocFees += activeF;
      const fillScale = params.execution_mode === "maker" ? params.maker_fill_rate : 1;
      allocEq += (t.pnl_dollars + (tf - activeF)) * fillScale;
      if (allocEq > allocPeak) allocPeak = allocEq;
      allocMaxDd = Math.max(allocMaxDd, ((allocPeak - allocEq) / Math.max(1, allocPeak)) * 100);
    }
    curve.push({
      close_epoch: t.close_epoch,
      legacy_equity: Number(legacyEq.toFixed(2)),
      allocator_equity: Number(allocEq.toFixed(2)),
    });
  }

  return {
    ...params,
    regime_throttle_mult: 1.0,
    current_oos_drawdown_pct: Number(allocMaxDd.toFixed(2)),
    throttle_reason: params.enable_regime_throttle
      ? `1.00x Normal (OOS DD ${allocMaxDd.toFixed(1)}%)`
      : "Disabled (1.00x)",
    windows_settled_total: settledTotal,
    windows_settled_oos: settledOos,
    live_armed: false,
    total_allocated_pct: Number(totalPct.toFixed(2)),
    cash_reserve_pct: Number(Math.max(0, 100 - totalPct).toFixed(2)),
    active_winners: rawWinners,
    quarantined,
    regime_attribution: [],
    fill_sensitivity: [],
    drift_gauge: {
      paper_expectancy_cents: rawWinners.length > 0 ? rawWinners[0].oos_net_expectancy_cents : null,
      shadow_expectancy_cents: rawWinners.length > 0 ? rawWinners[0].oos_net_expectancy_cents : null,
      drift_cents: rawWinners.length > 0 ? 0 : null,
      status: rawWinners.length > 0 ? "HEALTHY" : "NO_DATA",
      summary: rawWinners.length > 0 ? "Offline preview drift nominal." : "No active winners.",
    },
    edge_screener: rawWinners.map((w, idx) => ({
      rank: idx + 1,
      strategy: w.strategy,
      side: w.side,
      top_series: "KXBTC15M",
      execution_mode: w.execution_mode,
      oos_trades: w.oos_trades,
      avg_entry_cents: Math.min(99, Math.max(1, Math.round(w.oos_avg_entry_price * 100))),
      maker_fee_cents: w.maker_fee_cents,
      taker_fee_cents: w.taker_fee_cents,
      fee_edge_saved_cents: Math.max(0, w.taker_fee_cents - w.maker_fee_cents),
      oos_net_expectancy_cents: w.oos_net_expectancy_cents,
      breakeven_fill_rate_pct: w.breakeven_fill_rate_pct,
      fill_headroom_pct: Math.max(0, params.maker_fill_rate * 100 - w.breakeven_fill_rate_pct),
      folds_positive: w.stress.folds_positive,
      folds_total: w.stress.folds_total,
      worst_fold_expectancy_cents: w.stress.worst_fold_expectancy_cents,
      daily_dollar_contribution: 0,
      daily_target_share_pct: 0,
      edge_quality_score: Math.min(100, Math.max(0, (w.oos_net_expectancy_cents / 15) * 60)),
      recommended_contracts: w.rail_clamped_contracts,
      basket_eligible: w.rail_clamped_contracts >= 1,
      verdict: w.rail_clamped_contracts >= 1 ? "BASKET_ELIGIBLE" : "BELOW_1_CONTRACT",
    })),
    best_basket: {
      legs: planned_intents.filter((it) => it.target_contracts >= 1).slice(0, 4),
      total_contracts: planned_intents.slice(0, 4).reduce((s, it) => s + it.target_contracts, 0),
      total_allocated_dollars: planned_intents.slice(0, 4).reduce((s, it) => s + it.allocated_dollars, 0),
      projected_daily_dollars: 0,
      avg_fill_headroom_pct: 0,
      ready_for_dry_run_preflight: planned_intents.some((it) => it.target_contracts >= 1),
      summary: "Offline preview basket.",
    },
    bootstrap_stress: {
      trials: 1000,
      horizon_days: 30,
      windows_per_day: 96,
      oos_sample_windows: settledOos,
      p5_end_equity: startEq,
      p50_end_equity: startEq,
      p95_end_equity: startEq,
      p95_max_drawdown_pct: allocMaxDd,
      daily_rail_breach_prob_pct: 0,
      prob_500_day_pct: 0,
      stress_1c_p5_end_equity: startEq,
      stress_1c_p50_end_equity: startEq,
      stress_1c_p95_end_equity: startEq,
      stress_1c_p95_max_drawdown_pct: allocMaxDd,
      stress_1c_daily_rail_breach_prob_pct: 0,
      stress_1c_prob_500_day_pct: 0,
      trajectory: [],
      summary: "Client fallback preview; run server pipeline for 1,000-trial bootstrap simulation.",
    },
    preset_leaderboard: [],
    top_safe_preset: null,
    forward_envelope: {
      status: rawWinners.length > 0 ? "ON_TRACK" : "NO_DATA",
      active_preset_id: null,
      active_preset_label: "Offline Preview",
      envelope_day: 1,
      envelope_p5_equity: startEq,
      envelope_p50_equity: startEq,
      observed_paper_equity: Number(allocEq.toFixed(2)),
      predicted_ev_cents: rawWinners.length > 0 ? rawWinners[0].oos_net_expectancy_cents : null,
      observed_forward_ev_cents: rawWinners.length > 0 ? rawWinners[0].oos_net_expectancy_cents : null,
      expectancy_drift_cents: rawWinners.length > 0 ? 0 : null,
      realized_slippage_vs_model_cents: 0,
      daily_rail_breach_prob_pct: 0,
      stress_1c_daily_rail_breach_prob_pct: 0,
      next_safe_preset_id: null,
      next_safe_preset_label: null,
      live_armed: false,
      summary: rawWinners.length > 0 ? "Offline paper envelope nominal." : "No active winners.",
    },
    diversification: makeOfflineDiversification(params, Number(totalPct.toFixed(2)), rawWinners.length > 0),
    comparison: {
      selection_basis: isTrades.length > 0 ? "causal_is_to_oos" : "oos_only_no_is_history",
      legacy_equal_taker_end_equity: Number(legacyEq.toFixed(2)),
      legacy_equal_taker_net_pnl: Number((legacyEq - startEq).toFixed(2)),
      legacy_equal_taker_max_dd_pct: Number(legacyMaxDd.toFixed(2)),
      legacy_equal_taker_fees_dollars: Number(legacyFees.toFixed(2)),
      auto_allocator_end_equity: Number(allocEq.toFixed(2)),
      auto_allocator_net_pnl: Number((allocEq - startEq).toFixed(2)),
      auto_allocator_max_dd_pct: Number(allocMaxDd.toFixed(2)),
      auto_allocator_fees_dollars: Number(allocFees.toFixed(2)),
      fee_savings_dollars: Number(Math.max(0, legacyFees - allocFees).toFixed(2)),
      equity_delta_dollars: Number((allocEq - legacyEq).toFixed(2)),
      equity_curve: curve,
    },
    planned_intents,
    daily_target: {
      target_daily_profit_dollars: 500,
      verdict: "NO_GO_PAPER",
      projected_daily_net_dollars: 0,
      projected_daily_stress_1c_dollars: 0,
      projected_daily_stress_2c_dollars: 0,
      required_bankroll_dollars: null,
      portfolio_net_expectancy_cents: rawWinners.length > 0 ? rawWinners[0].oos_net_expectancy_cents : null,
      blockers: ["Live routing is disarmed (live_armed=false)", "Offline preview mode"],
    },
    honesty_notes: [
      "Client-side paper fallback preview active; live server allocator endpoint did not respond.",
    ],
    source: "client",
  };
}

export async function fetchAllocatorReport(
  dataset: Dataset,
  params?: AutoAllocatorRequest,
  fallback?: { slots: Slots | null; markets: Market[] },
): Promise<AutoAllocatorReport> {
  const effective = params ?? DEFAULT_ALLOCATOR_REQUEST;
  const qs = dataset === "fixture" ? "?dataset=fixture" : "";

  try {
    const r = params
      ? await fetch(`/api/allocator/plan${qs}`, {
          method: "POST",
          headers: MUTATION_HEADERS,
          body: JSON.stringify(effective),
        })
      : await fetch(`/api/allocator${qs}`);

    if (r.ok) {
      const json = await r.json();
      const normalized = normalizeReport(json);
      if (normalized) return normalized;
    }
  } catch {
    /* Fall through to clean offline/fixture paper preview */
  }

  return computeClientFallbackReport(effective, fallback?.slots ?? null, fallback?.markets ?? []);
}
