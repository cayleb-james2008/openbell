// OpenBell Autonomous Edge & Auto-Allocator client + deterministic offline/fixture preview fallback.
// Aligned with GET /api/allocator and POST /api/allocator/plan (X-OpenBell-CSRF: 1).
import { type Dataset, type Market, type Slots, isNum } from "@/lib/desk";
import { kalshiFeeDollars } from "@/lib/quant";

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
