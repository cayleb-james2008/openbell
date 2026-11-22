// OpenBell v0.6.0 PAV Isotonic Probability Recalibration & $500/Day Paper Capital Scaling Engine client.
// Calls GET /api/analytics/recalibration and POST /api/analytics/scale-plan on v0.6.0+, falling back
// cleanly to deterministic client-side IS(60%)/OOS(40%) PAV isotonic regression and 180-day 1,000-path
// paper scaling simulations derived strictly from real loaded tape. Strictly paper-only.
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  impliedYesProb,
  isNum,
  supportsV060,
} from "@/lib/desk";
import { kalshiFeeDollars, wilsonInterval } from "@/lib/quant";

export type SizingMode = "equal_slots" | "quarter_kelly" | "half_kelly";

export type RecalBucket = {
  low: number;
  high: number;
  n: number;
  wins: number;
  meanPredP: number;
  realisedRate: number;
  wilsonLow: number;
  wilsonHigh: number;
  gap: number;
  calibratedP: number;
};

export type OosCalMetrics = {
  brierScore: number | null;
  logLoss: number | null;
  ece: number | null;
};

export type EvGateImpact = {
  acceptedCount: number;
  refusedCount: number;
  totalCount: number;
  acceptedWinRatePct: number | null;
  refusedWinRatePct: number | null;
  beforePnlDollars: number;
  afterPnlDollars: number;
  pnlDeltaDollars: number;
  slippageMarginCents: number;
  ruleFormula: string;
};

export type RecalibrationData = {
  isCount: number;
  oosCount: number;
  beforeOos: OosCalMetrics;
  afterOos: OosCalMetrics;
  oosImproved: boolean;
  minDetectableEdge: number | null;
  buckets: RecalBucket[];
  evGate: EvGateImpact;
  note: string;
  source: "server" | "client";
};

export type ScalePlanRequest = {
  target_daily_dollars: number;
  starting_bankroll: number;
  max_slot_risk_pct: number;
  slippage_cents: number;
  max_contracts_per_window: number;
  active_series_count: number;
  sizing_mode: SizingMode;
  paths: number;
  seed: number;
  assumed_edge_cents?: number | null;
  dataset?: Dataset;
};

export type ReadinessGate = {
  id: string;
  label: string;
  passed: boolean;
  actualDisplay: string;
  thresholdDisplay: string;
  reason: string;
};

export type ScalePlanData = {
  modeLabel: "Empirical Tape" | "Sensitivity (Assumed Edge)";
  assumedEdgeCents: number | null;
  targetDailyDollars: number;
  startingBankroll: number;
  sizingMode: SizingMode;
  currentEmpiricalDailyDollars: number;
  oosExpectancyCentsPerContract: number | null;
  oosEdgePositive: boolean;
  requiredBankrollEqualSlots: number | null;
  requiredBankrollQuarterKelly: number | null;
  requiredBankrollHalfKelly: number | null;
  negativeEdgeReason: string | null;
  liquidityCeiling: {
    singleSeriesMaxDailyDollars: number;
    portfolioMaxDailyDollars: number;
    activeSeriesCount: number;
    maxContractsPerWindow: number;
    windowsPerDay: number;
    fillRatioAtTarget: number;
  };
  compoundingProjection: {
    days: number;
    p10Equity: number[];
    p50Equity: number[];
    p90Equity: number[];
    p10Daily: number[];
    p50Daily: number[];
    p90Daily: number[];
    probReachTarget: number;
    medianDaysToTarget: number | null;
    probDdGte25Pct: number;
  };
  readinessGates: {
    verdict: "READY_FOR_OWNER_REVIEW" | "PAPER_RESEARCH_ONLY";
    gates: ReadinessGate[];
    blockingReasons: string[];
  };
  seed: number;
  paths: number;
  elapsedMs: number;
  source: "server" | "client";
};

// ---------------------------------------------------------------------------
// Helpers: deterministic PRNG, quantile, chronological observation extraction
// ---------------------------------------------------------------------------

function mulberry32(seed: number): () => number {
  let a = (seed >>> 0) || 1;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function quantile(sorted: number[], q: number): number {
  if (sorted.length === 0) return 0;
  const pos = (sorted.length - 1) * q;
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  if (lo === hi) return sorted[lo];
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo);
}

type Obs = {
  epoch: number;
  pred: number;
  won: number;
  price: number;
  contracts: number;
  pnlDollars: number;
};

function extractObservations(markets: Market[], slots?: Slots | null): Obs[] {
  const out: Obs[] = [];

  // Prefer settled markets carrying real interior pre-close implied probabilities.
  for (const m of markets) {
    if (!m || typeof m !== "object") continue;
    const res = typeof m.result === "string" ? m.result.trim().toLowerCase() : "";
    if (res !== "yes" && res !== "no") continue;
    const p = impliedYesProb(m);
    if (p === null || !Number.isFinite(p) || p <= 0.01 || p >= 0.99) continue;
    const closeMs = typeof m.close_time === "string" ? Date.parse(m.close_time) : NaN;
    const epoch = Number.isFinite(closeMs) ? Math.floor(closeMs / 1000) : (m.fetched_at ?? 0);
    const price = Math.min(0.99, Math.max(0.01, Math.round(p * 100) / 100));
    const won = res === "yes" ? 1 : 0;
    const fee = kalshiFeeDollars(1, price);
    const pnl = won ? 1 - price - fee : -price - fee;
    out.push({ epoch, pred: price, won, price, contracts: 1, pnlDollars: pnl });
  }

  // Fallback when /api/markets only carries post-settlement 0/1 last_price: use real recorded trade quotes.
  if (out.length === 0 && slots?.trades && Array.isArray(slots.trades)) {
    for (const tr of slots.trades) {
      if (!tr || !isNum(tr.price) || !isNum(tr.pnl_dollars)) continue;
      if (tr.price <= 0.01 || tr.price >= 0.99) continue;
      out.push({
        epoch: isNum(tr.close_epoch) ? tr.close_epoch : 0,
        pred: Math.min(0.99, Math.max(0.01, Math.round(tr.price * 100) / 100)),
        won: tr.pnl_dollars > 0 ? 1 : 0,
        price: Math.min(0.99, Math.max(0.01, Math.round(tr.price * 100) / 100)),
        contracts: Math.max(1, Math.round(tr.contracts || 1)),
        pnlDollars: tr.pnl_dollars,
      });
    }
  }

  return out.sort((a, b) => a.epoch - b.epoch);
}

// ---------------------------------------------------------------------------
// Monotone Pool-Adjacent-Violators (PAV) Isotonic Regression (IS 60% -> OOS 40%)
// ---------------------------------------------------------------------------

type PavBlock = { maxX: number; minX: number; sumY: number; weight: number; mean: number };

function fitPavIsotonic(isObs: Obs[]): PavBlock[] {
  const sorted = [...isObs].sort((a, b) => a.pred - b.pred);
  const blocks: PavBlock[] = [];
  for (const o of sorted) {
    blocks.push({ minX: o.pred, maxX: o.pred, sumY: o.won, weight: 1, mean: o.won });
    while (blocks.length >= 2) {
      const b = blocks[blocks.length - 1];
      const a = blocks[blocks.length - 2];
      if (a.mean > b.mean) {
        blocks.pop();
        a.maxX = Math.max(a.maxX, b.maxX);
        a.sumY += b.sumY;
        a.weight += b.weight;
        a.mean = a.sumY / a.weight;
      } else {
        break;
      }
    }
  }
  return blocks;
}

function predictPav(blocks: PavBlock[], x: number): number {
  if (blocks.length === 0) return Math.min(0.99, Math.max(0.01, x));
  if (x <= blocks[0].minX) return Math.min(0.99, Math.max(0.01, blocks[0].mean));
  const last = blocks[blocks.length - 1];
  if (x >= last.maxX) return Math.min(0.99, Math.max(0.01, last.mean));
  for (let i = 0; i < blocks.length; i++) {
    const b = blocks[i];
    if (x >= b.minX && x <= b.maxX) return Math.min(0.99, Math.max(0.01, b.mean));
    if (i + 1 < blocks.length && x > b.maxX && x < blocks[i + 1].minX) {
      const nxt = blocks[i + 1];
      const span = Math.max(1e-9, nxt.minX - b.maxX);
      const t = (x - b.maxX) / span;
      return Math.min(0.99, Math.max(0.01, b.mean + t * (nxt.mean - b.mean)));
    }
  }
  return Math.min(0.99, Math.max(0.01, last.mean));
}

function evalCalibrationSlice(pairs: { p: number; y: number }[]): OosCalMetrics {
  if (pairs.length === 0) return { brierScore: null, logLoss: null, ece: null };
  let brier = 0;
  let ll = 0;
  const bins = Array.from({ length: 10 }, () => ({ sumP: 0, sumY: 0, n: 0 }));
  for (const { p, y } of pairs) {
    const pc = Math.min(0.995, Math.max(0.005, p));
    brier += (pc - y) * (pc - y);
    ll += -(y * Math.log(pc) + (1 - y) * Math.log(1 - pc));
    const idx = Math.min(9, Math.max(0, Math.floor(pc * 10)));
    bins[idx].sumP += pc;
    bins[idx].sumY += y;
    bins[idx].n += 1;
  }
  const n = pairs.length;
  let ece = 0;
  for (const b of bins) {
    if (b.n > 0) ece += (b.n / n) * Math.abs(b.sumY / b.n - b.sumP / b.n);
  }
  return {
    brierScore: Number((brier / n).toFixed(4)),
    logLoss: Number((ll / n).toFixed(4)),
    ece: Number(ece.toFixed(4)),
  };
}

export function computeRecalibrationClient(markets: Market[], slots?: Slots | null): RecalibrationData {
  const obs = extractObservations(markets, slots);
  if (obs.length < 4) {
    return {
      isCount: obs.length,
      oosCount: 0,
      beforeOos: { brierScore: null, logLoss: null, ece: null },
      afterOos: { brierScore: null, logLoss: null, ece: null },
      oosImproved: false,
      minDetectableEdge: null,
      buckets: [],
      evGate: {
        acceptedCount: 0,
        refusedCount: 0,
        totalCount: 0,
        acceptedWinRatePct: null,
        refusedWinRatePct: null,
        beforePnlDollars: 0,
        afterPnlDollars: 0,
        pnlDeltaDollars: 0,
        slippageMarginCents: 1,
        ruleFormula: "p_cal > entry_price + fee + 1c slippage",
      },
      note: "Need at least 4 settled windows with interior quotes for chronological IS (60%) / OOS (40%) PAV fitting.",
      source: "client",
    };
  }

  const cut = Math.max(2, Math.min(obs.length - 2, Math.floor(obs.length * 0.6)));
  const isSlice = obs.slice(0, cut);
  const oosSlice = obs.slice(cut);
  const blocks = fitPavIsotonic(isSlice);

  const rawPairs = oosSlice.map((o) => ({ p: o.pred, y: o.won }));
  const calPairs = oosSlice.map((o) => ({ p: predictPav(blocks, o.pred), y: o.won }));

  const beforeOos = evalCalibrationSlice(rawPairs);
  const afterOos = evalCalibrationSlice(calPairs);

  // 5-point probability buckets (0.00..0.05 .. 0.95..1.00), keeping populated buckets
  const rawBins = Array.from({ length: 20 }, (_, i) => ({
    low: i * 0.05,
    high: (i + 1) * 0.05,
    n: 0,
    wins: 0,
    sumPred: 0,
    sumCal: 0,
  }));

  const slippageDollars = 0.01;
  let accepted = 0;
  let refused = 0;
  let accWins = 0;
  let refWins = 0;
  let beforePnl = 0;
  let afterPnl = 0;

  for (let i = 0; i < oosSlice.length; i++) {
    const o = oosSlice[i];
    const pCal = calPairs[i].p;
    const binIdx = Math.min(19, Math.max(0, Math.floor(o.pred * 20)));
    rawBins[binIdx].n += 1;
    rawBins[binIdx].wins += o.won;
    rawBins[binIdx].sumPred += o.pred;
    rawBins[binIdx].sumCal += pCal;

    const feePerContract = kalshiFeeDollars(1, o.price);
    const gatePass = pCal > o.price + feePerContract + slippageDollars;
    beforePnl += o.pnlDollars;
    if (gatePass) {
      accepted += 1;
      accWins += o.won;
      afterPnl += o.pnlDollars;
    } else {
      refused += 1;
      refWins += o.won;
    }
  }

  const buckets: RecalBucket[] = rawBins
    .filter((b) => b.n > 0)
    .map((b) => {
      const meanPredP = b.sumPred / b.n;
      const realisedRate = b.wins / b.n;
      const [wLo, wHi] = wilsonInterval(b.wins, b.n);
      return {
        low: Number(b.low.toFixed(2)),
        high: Number(b.high.toFixed(2)),
        n: b.n,
        wins: b.wins,
        meanPredP: Number(meanPredP.toFixed(4)),
        realisedRate: Number(realisedRate.toFixed(4)),
        wilsonLow: Number(wLo.toFixed(4)),
        wilsonHigh: Number(wHi.toFixed(4)),
        gap: Number((realisedRate - meanPredP).toFixed(4)),
        calibratedP: Number((b.sumCal / b.n).toFixed(4)),
      };
    });

  const oosImproved =
    beforeOos.brierScore !== null &&
    afterOos.brierScore !== null &&
    afterOos.brierScore < beforeOos.brierScore;
  const mde = Number((1.96 * Math.sqrt(0.25 / Math.max(1, oosSlice.length))).toFixed(4));

  return {
    isCount: isSlice.length,
    oosCount: oosSlice.length,
    beforeOos,
    afterOos,
    oosImproved,
    minDetectableEdge: mde,
    buckets,
    evGate: {
      acceptedCount: accepted,
      refusedCount: refused,
      totalCount: oosSlice.length,
      acceptedWinRatePct: accepted > 0 ? Number(((accWins / accepted) * 100).toFixed(1)) : null,
      refusedWinRatePct: refused > 0 ? Number(((refWins / refused) * 100).toFixed(1)) : null,
      beforePnlDollars: Number(beforePnl.toFixed(2)),
      afterPnlDollars: Number(afterPnl.toFixed(2)),
      pnlDeltaDollars: Number((afterPnl - beforePnl).toFixed(2)),
      slippageMarginCents: 1,
      ruleFormula: "p_cal > entry_price + fee + 1c slippage",
    },
    note: `Fitted monotone PAV on first ${isSlice.length} windows (60% IS); evaluated strictly on last ${oosSlice.length} windows (40% OOS).`,
    source: "client",
  };
}

// ---------------------------------------------------------------------------
// Deterministic Client-Side $500/Day Paper Capital Scaling & Capacity Fallback
// ---------------------------------------------------------------------------

export function computeScalePlanClient(
  req: ScalePlanRequest,
  markets: Market[],
  slots?: Slots | null,
): ScalePlanData {
  const t0 = performance.now();
  const targetDaily = Math.max(10, Number(req.target_daily_dollars) || 500);
  const startBank = Math.max(50, Number(req.starting_bankroll) || 1000);
  const slippageCents = Math.min(5, Math.max(0, Number(req.slippage_cents) || 0));
  const maxContracts = Math.max(1, Math.round(Number(req.max_contracts_per_window) || 250));
  const seriesCount = Math.min(8, Math.max(1, Math.round(Number(req.active_series_count) || 4)));
  const maxSlotRisk = Math.min(50, Math.max(1, Number(req.max_slot_risk_pct) || 25)) / 100;
  const paths = Math.min(2000, Math.max(100, Math.round(Number(req.paths) || 1000)));
  const seed = Math.max(1, Math.floor(Number(req.seed) || 42));
  const assumedEdge =
    req.assumed_edge_cents !== undefined && req.assumed_edge_cents !== null && Number.isFinite(req.assumed_edge_cents)
      ? req.assumed_edge_cents
      : null;
  const isSensitivity = assumedEdge !== null && assumedEdge > 0;

  const trades = (slots?.trades || [])
    .filter((t) => t && isNum(t.close_epoch) && isNum(t.pnl_dollars))
    .sort((a, b) => a.close_epoch - b.close_epoch);

  const recal = computeRecalibrationClient(markets, slots);
  const settledTotal = Math.max(
    recal.isCount + recal.oosCount,
    markets.filter((m) => m && (m.result === "yes" || m.result === "no")).length,
  );

  // Compute OOS (last 40%) trade statistics and slippage-adjusted expectancy per contract
  const cut = trades.length > 1 ? Math.max(1, Math.floor(trades.length * 0.6)) : 0;
  const oosTrades = trades.length > 1 ? trades.slice(cut) : trades;

  let oosNetPnl = 0;
  let oosContracts = 0;
  let oosWins = 0;
  let avgEntryPrice = 0.72;
  let peak = slots?.paper_pot_start || 100;
  let runEq = peak;
  let maxObservedDdPct = 0;

  for (const tr of trades) {
    runEq += tr.pnl_dollars;
    if (runEq > peak) peak = runEq;
    if (peak > 0) {
      const dd = ((peak - runEq) / peak) * 100;
      if (dd > maxObservedDdPct) maxObservedDdPct = dd;
    }
  }

  if (oosTrades.length > 0) {
    let priceSum = 0;
    for (const tr of oosTrades) {
      const c = Math.max(1, tr.contracts || 1);
      const slipCost = (slippageCents / 100) * c;
      oosNetPnl += tr.pnl_dollars - slipCost;
      oosContracts += c;
      if (tr.pnl_dollars - slipCost > 0) oosWins += 1;
      priceSum += isNum(tr.price) && tr.price > 0.01 && tr.price < 0.99 ? tr.price : 0.72;
    }
    avgEntryPrice = priceSum / oosTrades.length;
  }

  const empiricalCentsPerContract = oosContracts > 0 ? (oosNetPnl * 100) / oosContracts : null;
  const effectiveEdgeCents = isSensitivity ? assumedEdge : empiricalCentsPerContract;
  const oosEdgePositive = empiricalCentsPerContract !== null && empiricalCentsPerContract > 0;
  const modelingEdgePositive = effectiveEdgeCents !== null && effectiveEdgeCents > 0;

  // Empirical daily dollar run-rate across span of OOS trades
  let currentEmpiricalDaily = 0;
  if (oosTrades.length >= 2) {
    const spanDays = Math.max(
      1 / 24,
      (oosTrades[oosTrades.length - 1].close_epoch - oosTrades[0].close_epoch) / 86400,
    );
    currentEmpiricalDaily = oosNetPnl / spanDays;
  } else if (oosTrades.length === 1) {
    currentEmpiricalDaily = oosNetPnl;
  }

  // Liquidity ceiling: 96 15m windows/day per series, active participation ~25% of windows
  const windowsPerDay = 96;
  const tradedWindowsPerSeriesDay = 24;
  const edgePerContractDollars = modelingEdgePositive ? (effectiveEdgeCents as number) / 100 : 0;
  const singleSeriesMaxDaily =
    modelingEdgePositive ? maxContracts * tradedWindowsPerSeriesDay * edgePerContractDollars : 0;
  const portfolioMaxDaily = singleSeriesMaxDaily * seriesCount;
  const fillRatioAtTarget =
    targetDaily > 0 && portfolioMaxDaily > 0 ? Math.min(1, portfolioMaxDaily / targetDaily) : 0;

  // Required bankroll under 3 sizing modes (STRICTLY null when OOS edge <= 0 and not in sensitivity mode!)
  let reqEqual: number | null = null;
  let reqQKelly: number | null = null;
  let reqHKelly: number | null = null;
  let negativeEdgeReason: string | null = null;

  if (!modelingEdgePositive) {
    negativeEdgeReason =
      oosTrades.length === 0
        ? "No settled out-of-sample paper trades yet — cannot estimate bankroll scaling."
        : `Out-of-sample expectancy after fees and ${slippageCents}c slippage is ${(empiricalCentsPerContract ?? 0).toFixed(2)}c/contract (<= 0). Scaling bankroll on non-positive edge increases expected drawdown.`;
  } else {
    const winProb = oosTrades.length > 0 ? Math.min(0.95, Math.max(0.05, oosWins / oosTrades.length)) : Math.min(0.92, avgEntryPrice + edgePerContractDollars);
    const oddsB = Math.max(0.05, (1 - avgEntryPrice) / Math.max(0.05, avgEntryPrice));
    const fullKelly = Math.max(0.01, Math.min(0.4, (winProb * oddsB - (1 - winProb)) / oddsB));
    const totalDailyTradedWindows = tradedWindowsPerSeriesDay * seriesCount;
    const neededContractsPerWindow = targetDaily / Math.max(1e-6, totalDailyTradedWindows * edgePerContractDollars);
    const costPerWindow = neededContractsPerWindow * avgEntryPrice;

    reqEqual = Number((costPerWindow / Math.max(0.02, maxSlotRisk / 4)).toFixed(2));
    reqQKelly = Number((costPerWindow / Math.max(0.005, fullKelly * 0.25)).toFixed(2));
    reqHKelly = Number((costPerWindow / Math.max(0.01, fullKelly * 0.5)).toFixed(2));
  }

  // 180-trading-day 1,000-path seeded Monte Carlo compounding trajectory
  const days = 180;
  const rng = mulberry32(seed);
  const riskFrac =
    req.sizing_mode === "half_kelly"
      ? 0.08
      : req.sizing_mode === "quarter_kelly"
        ? 0.04
        : Math.min(0.12, maxSlotRisk * 0.25);

  const dailyEdgePct =
    effectiveEdgeCents !== null
      ? Math.max(-0.04, Math.min(0.04, (effectiveEdgeCents / 100) * riskFrac * seriesCount * 1.4))
      : 0;
  const dailyVolPct = Math.max(0.015, Math.min(0.12, riskFrac * Math.sqrt(seriesCount) * 0.55));

  const eqMatrix: number[][] = Array.from({ length: days + 1 }, () => new Array<number>(paths));
  const dailyMatrix: number[][] = Array.from({ length: days + 1 }, () => new Array<number>(paths));
  const initialDaily = Math.min(
    portfolioMaxDaily || Number.POSITIVE_INFINITY,
    startBank * Math.max(0, dailyEdgePct),
  );
  for (let p = 0; p < paths; p++) {
    eqMatrix[0][p] = startBank;
    dailyMatrix[0][p] = isFinite(initialDaily) ? initialDaily : 0;
  }

  let reachedCount = 0;
  let dd25Count = 0;
  const hitDays: number[] = [];

  for (let p = 0; p < paths; p++) {
    let eq = startBank;
    let hi = startBank;
    let maxDd = 0;
    let hitDay = -1;
    for (let d = 1; d <= days; d++) {
      // Box-Muller normal deviate from deterministic PRNG
      const u1 = Math.max(1e-9, rng());
      const u2 = rng();
      const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
      const rawDailyReturn = dailyEdgePct + z * dailyVolPct;
      let deltaDollars = eq * rawDailyReturn;
      // Respect liquidity ceiling cap on positive daily gains
      if (portfolioMaxDaily > 0 && deltaDollars > portfolioMaxDaily * 1.5) {
        deltaDollars = portfolioMaxDaily * 1.5;
      }
      eq = Math.max(1, eq + deltaDollars);
      if (eq > hi) hi = eq;
      const dd = hi > 0 ? (hi - eq) / hi : 0;
      if (dd > maxDd) maxDd = dd;

      const runRate = modelingEdgePositive
        ? Math.min(portfolioMaxDaily, Math.max(0, eq * Math.max(0, dailyEdgePct)))
        : Math.min(0, eq * dailyEdgePct);
      eqMatrix[d][p] = eq;
      dailyMatrix[d][p] = runRate;
      if (hitDay < 0 && runRate >= targetDaily) hitDay = d;
    }
    if (hitDay >= 0) {
      reachedCount++;
      hitDays.push(hitDay);
    }
    if (maxDd >= 0.25) dd25Count++;
  }

  const p10Equity: number[] = [];
  const p50Equity: number[] = [];
  const p90Equity: number[] = [];
  const p10Daily: number[] = [];
  const p50Daily: number[] = [];
  const p90Daily: number[] = [];

  for (let d = 0; d <= days; d++) {
    const eqArr = eqMatrix[d].sort((a, b) => a - b);
    const dyArr = dailyMatrix[d].sort((a, b) => a - b);
    p10Equity.push(Number(quantile(eqArr, 0.1).toFixed(2)));
    p50Equity.push(Number(quantile(eqArr, 0.5).toFixed(2)));
    p90Equity.push(Number(quantile(eqArr, 0.9).toFixed(2)));
    p10Daily.push(Number(quantile(dyArr, 0.1).toFixed(2)));
    p50Daily.push(Number(quantile(dyArr, 0.5).toFixed(2)));
    p90Daily.push(Number(quantile(dyArr, 0.9).toFixed(2)));
  }

  hitDays.sort((a, b) => a - b);
  const medianDaysToTarget = hitDays.length > 0 ? Math.round(quantile(hitDays, 0.5)) : null;

  // 5 Mechanical Readiness Gates (strictly grounded in REAL empirical OOS tape — never bypassed by what-if sensitivity!)
  const oosEce = recal.afterOos.ece ?? recal.beforeOos.ece;
  const gates: ReadinessGate[] = [
    {
      id: "sample_depth",
      label: "Settled Sample Depth",
      passed: settledTotal >= 200,
      actualDisplay: `${settledTotal} windows`,
      thresholdDisplay: ">= 200 windows",
      reason: settledTotal >= 200 ? "Minimum statistical sample reached." : `Need ${200 - settledTotal} more settled 15m windows.`,
    },
    {
      id: "oos_net_expectancy",
      label: "OOS Net Expectancy",
      passed: oosEdgePositive,
      actualDisplay:
        empiricalCentsPerContract !== null ? `${empiricalCentsPerContract >= 0 ? "+" : ""}${empiricalCentsPerContract.toFixed(2)}c/ct` : "N/A",
      thresholdDisplay: "> 0.00c/ct",
      reason: oosEdgePositive
        ? "Positive out-of-sample expectancy after fees and slippage."
        : "Out-of-sample expectancy after fees and slippage is not positive.",
    },
    {
      id: "oos_ece",
      label: "OOS Calibration ECE",
      passed: oosEce !== null && oosEce < 0.05,
      actualDisplay: oosEce !== null ? `${(oosEce * 100).toFixed(2)}%` : "N/A",
      thresholdDisplay: "< 5.00%",
      reason: oosEce !== null && oosEce < 0.05 ? "OOS probabilities are well-calibrated." : "OOS calibration error exceeds 5% ceiling.",
    },
    {
      id: "max_drawdown_pct",
      label: "Observed Max Drawdown",
      passed: trades.length > 0 && maxObservedDdPct < 20.0,
      actualDisplay: trades.length > 0 ? `${maxObservedDdPct.toFixed(1)}%` : "N/A",
      thresholdDisplay: "< 20.0%",
      reason:
        trades.length === 0
          ? "No settled paper drawdown history yet."
          : maxObservedDdPct < 20.0
            ? "Historical paper drawdown stayed below 20%."
            : `Observed paper drawdown (${maxObservedDdPct.toFixed(1)}%) breaches 20% guardrail.`,
    },
    {
      id: "liquidity_fill_ratio",
      label: "Liquidity Fill Ratio",
      passed: oosEdgePositive && fillRatioAtTarget >= 0.75,
      actualDisplay: `${(fillRatioAtTarget * 100).toFixed(0)}%`,
      thresholdDisplay: ">= 75%",
      reason:
        oosEdgePositive && fillRatioAtTarget >= 0.75
          ? "Multi-series 15m liquidity covers target run-rate."
          : "Empirical capacity cannot fill >= 75% of target daily run-rate.",
    },
  ];

  const blockingReasons = gates.filter((g) => !g.passed).map((g) => `${g.label}: ${g.reason}`);
  const verdict = blockingReasons.length === 0 ? "READY_FOR_OWNER_REVIEW" : "PAPER_RESEARCH_ONLY";

  return {
    modeLabel: isSensitivity ? "Sensitivity (Assumed Edge)" : "Empirical Tape",
    assumedEdgeCents: isSensitivity ? assumedEdge : null,
    targetDailyDollars: targetDaily,
    startingBankroll: startBank,
    sizingMode: req.sizing_mode,
    currentEmpiricalDailyDollars: Number(currentEmpiricalDaily.toFixed(2)),
    oosExpectancyCentsPerContract:
      empiricalCentsPerContract !== null ? Number(empiricalCentsPerContract.toFixed(2)) : null,
    oosEdgePositive,
    requiredBankrollEqualSlots: reqEqual,
    requiredBankrollQuarterKelly: reqQKelly,
    requiredBankrollHalfKelly: reqHKelly,
    negativeEdgeReason,
    liquidityCeiling: {
      singleSeriesMaxDailyDollars: Number(singleSeriesMaxDaily.toFixed(2)),
      portfolioMaxDailyDollars: Number(portfolioMaxDaily.toFixed(2)),
      activeSeriesCount: seriesCount,
      maxContractsPerWindow: maxContracts,
      windowsPerDay,
      fillRatioAtTarget: Number(fillRatioAtTarget.toFixed(4)),
    },
    compoundingProjection: {
      days,
      p10Equity,
      p50Equity,
      p90Equity,
      p10Daily,
      p50Daily,
      p90Daily,
      probReachTarget: Number((reachedCount / paths).toFixed(4)),
      medianDaysToTarget,
      probDdGte25Pct: Number((dd25Count / paths).toFixed(4)),
    },
    readinessGates: {
      verdict,
      gates,
      blockingReasons,
    },
    seed,
    paths,
    elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
    source: "client",
  };
}

// ---------------------------------------------------------------------------
// Flexible v0.6.0 Server-First Normalizers & Fetchers
// ---------------------------------------------------------------------------

function pickNum(obj: Record<string, unknown>, keys: string[]): number | null {
  for (const k of keys) {
    const v = obj[k];
    if (isNum(v)) return v;
  }
  return null;
}

function normPct01(v: number | null): number {
  if (v === null) return 0;
  return v > 1 ? v / 100 : Math.max(0, v);
}

function normalizeRecalibration(raw: unknown): RecalibrationData | null {
  if (!raw || typeof raw !== "object") return null;
  const root = raw as Record<string, unknown>;
  // Support DeskRecalibrationReport { portfolio, market, by_strategy } OR direct RecalibrationReport
  const pickReport = (): Record<string, unknown> => {
    const port = root.portfolio as Record<string, unknown> | undefined;
    const mkt = root.market as Record<string, unknown> | undefined;
    if (port && typeof port === "object" && (Array.isArray(port.oos_buckets) || Array.isArray(port.buckets))) {
      const n = pickNum(port, ["oos_count", "total_count"]) ?? 0;
      if (n > 0 || !mkt) return port;
    }
    if (mkt && typeof mkt === "object" && (Array.isArray(mkt.oos_buckets) || Array.isArray(mkt.buckets))) {
      return mkt;
    }
    return root;
  };
  const o = pickReport();
  const rawBuckets = Array.isArray(o.oos_buckets)
    ? o.oos_buckets
    : Array.isArray(o.buckets)
      ? o.buckets
      : Array.isArray(o.all_buckets)
        ? o.all_buckets
        : Array.isArray(o.bucket_table)
          ? o.bucket_table
          : Array.isArray(o.bins)
            ? o.bins
            : null;
  if (!rawBuckets) return null;

  const buckets: RecalBucket[] = rawBuckets
    .filter((b): b is Record<string, unknown> => !!b && typeof b === "object")
    .map((b, idx) => {
      const low = pickNum(b, ["lower_bound", "low", "lower", "bin_low", "min_p"]) ?? idx * 0.05;
      const high = pickNum(b, ["upper_bound", "high", "upper", "bin_high", "max_p"]) ?? (idx + 1) * 0.05;
      const n = pickNum(b, ["n", "count", "total"]) ?? 0;
      const wins = pickNum(b, ["wins", "positives"]) ?? 0;
      const meanPredP = pickNum(b, ["mean_pred_p", "mean_predicted", "predicted_mean", "pred_p"]) ?? (low + high) / 2;
      const realisedRate = pickNum(b, ["realised_rate", "realized_rate", "empirical_rate", "win_rate"]) ?? 0;
      const wilsonLow =
        pickNum(b, ["wilson_low", "wilson_lower_95", "ci_low", "lower_95"]) ?? Math.max(0, realisedRate);
      const wilsonHigh =
        pickNum(b, ["wilson_high", "wilson_upper_95", "ci_high", "upper_95"]) ?? Math.min(1, realisedRate);
      const calibratedP = pickNum(b, ["calibrated_p", "isotonic_p", "pav_p", "after_p"]) ?? meanPredP;
      const gap = pickNum(b, ["gap", "calibration_gap"]) ?? realisedRate - meanPredP;
      return { low, high, n, wins, meanPredP, realisedRate, wilsonLow, wilsonHigh, gap, calibratedP };
    })
    .filter((b) => b.n > 0);

  const beforeObj = (
    o.oos_before && typeof o.oos_before === "object"
      ? o.oos_before
      : o.before_oos && typeof o.before_oos === "object"
        ? o.before_oos
        : o.raw_oos && typeof o.raw_oos === "object"
          ? o.raw_oos
          : o.before && typeof o.before === "object"
            ? o.before
            : o
  ) as Record<string, unknown>;

  const afterObj = (
    o.oos_after && typeof o.oos_after === "object"
      ? o.oos_after
      : o.after_oos && typeof o.after_oos === "object"
        ? o.after_oos
        : o.calibrated_oos && typeof o.calibrated_oos === "object"
          ? o.calibrated_oos
          : o.after && typeof o.after === "object"
            ? o.after
            : o
  ) as Record<string, unknown>;

  const gateObj = (
    o.entry_gate && typeof o.entry_gate === "object"
      ? o.entry_gate
      : o.ev_gate && typeof o.ev_gate === "object"
        ? o.ev_gate
        : o.ev_entry_gate && typeof o.ev_entry_gate === "object"
          ? o.ev_entry_gate
          : {}
  ) as Record<string, unknown>;

  const acceptedCount = pickNum(gateObj, ["accepted_count", "accepted", "passed_count"]) ?? 0;
  const refusedCount = pickNum(gateObj, ["refused_count", "refused", "rejected_count"]) ?? 0;
  const beforePnl =
    pickNum(gateObj, [
      "baseline_total_pnl_dollars_1ct",
      "before_pnl_dollars",
      "baseline_pnl_dollars",
      "baseline_pnl_dollars_1ct",
      "unfiltered_pnl_dollars",
    ]) ?? 0;
  const afterPnl =
    pickNum(gateObj, [
      "accepted_total_pnl_dollars_1ct",
      "after_pnl_dollars",
      "accepted_pnl_dollars",
      "accepted_pnl_dollars_1ct",
      "gated_pnl_dollars",
      "filtered_pnl_dollars",
    ]) ?? 0;
  const pnlDelta =
    pickNum(gateObj, ["pnl_delta_dollars_1ct", "pnl_delta_dollars", "delta_pnl_dollars"]) ??
    Number((afterPnl - beforePnl).toFixed(2));

  return {
    isCount: pickNum(o, ["is_count", "in_sample_count", "train_count"]) ?? 0,
    oosCount:
      pickNum(o, ["oos_count", "out_of_sample_count", "test_count"]) ??
      buckets.reduce((s, b) => s + b.n, 0),
    beforeOos: {
      brierScore: pickNum(beforeObj, ["brier_score", "before_brier_score", "raw_brier"]),
      logLoss: pickNum(beforeObj, ["log_loss", "before_log_loss", "raw_log_loss"]),
      ece: pickNum(beforeObj, ["ece", "before_ece", "raw_ece"]),
    },
    afterOos: {
      brierScore: pickNum(afterObj, ["brier_score", "after_brier_score", "calibrated_brier"]),
      logLoss: pickNum(afterObj, ["log_loss", "after_log_loss", "calibrated_log_loss"]),
      ece: pickNum(afterObj, ["ece", "after_ece", "calibrated_ece"]),
    },
    oosImproved: Boolean(o.oos_improved ?? o.improved),
    minDetectableEdge: pickNum(o, ["min_detectable_edge", "mde"]),
    buckets,
    evGate: {
      acceptedCount,
      refusedCount,
      totalCount: pickNum(gateObj, ["total_evaluated", "total_count", "total"]) ?? acceptedCount + refusedCount,
      acceptedWinRatePct: (() => {
        const w = pickNum(gateObj, ["accepted_win_rate_pct", "accepted_win_rate"]);
        return w === null ? null : w <= 1 ? Number((w * 100).toFixed(1)) : w;
      })(),
      refusedWinRatePct: (() => {
        const w = pickNum(gateObj, ["refused_win_rate_pct", "refused_win_rate"]);
        return w === null ? null : w <= 1 ? Number((w * 100).toFixed(1)) : w;
      })(),
      beforePnlDollars: beforePnl,
      afterPnlDollars: afterPnl,
      pnlDeltaDollars: pnlDelta,
      slippageMarginCents:
        pickNum(gateObj, ["slippage_margin_cents", "slippage_cents"]) ??
        pickNum(root, ["slippage_cents"]) ??
        1,
      ruleFormula:
        typeof gateObj.rule_formula === "string" && gateObj.rule_formula
          ? gateObj.rule_formula
          : "p_cal > entry_price + fee + slippage",
    },
    note:
      typeof o.honesty_note === "string" && o.honesty_note
        ? o.honesty_note
        : typeof o.note === "string" && o.note
          ? o.note
          : "Monotone PAV isotonic regression fitted strictly on IS (60%) and evaluated strictly on OOS (40%).",
    source: "server",
  };
}

function formatGateObserved(val: unknown, unit: string): string {
  if (!isNum(val)) return "N/A";
  const u = unit.trim().toLowerCase();
  if (u === "trades" || u === "windows") return `${Math.round(val)} ${u}`;
  if (u.includes("cents")) return `${val >= 0 ? "+" : ""}${val.toFixed(2)}c/ct`;
  if (u === "ece") return `${(val * 100).toFixed(2)}%`;
  if (u === "%") return `${val.toFixed(1)}%`;
  if (u === "ratio") return `${(val * 100).toFixed(0)}%`;
  return `${val.toFixed(2)}${unit ? ` ${unit}` : ""}`;
}

function formatGateThreshold(id: string, val: unknown, unit: string): string {
  if (!isNum(val)) return "-";
  const u = unit.trim().toLowerCase();
  if (id === "sample_depth") return `>= ${Math.round(val)} ${u || "trades"}`;
  if (id === "oos_net_expectancy") return `> ${val.toFixed(2)}c/ct`;
  if (id === "oos_ece") return `< ${(val * 100).toFixed(2)}%`;
  if (id === "max_drawdown_pct") return `< ${val.toFixed(1)}%`;
  if (id === "liquidity_fill_ratio") return `>= ${(val * 100).toFixed(0)}%`;
  return `${val}${unit ? ` ${unit}` : ""}`;
}

function normalizeScalePlan(raw: unknown, req: ScalePlanRequest): ScalePlanData | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const liq = (o.liquidity_ceiling && typeof o.liquidity_ceiling === "object" ? o.liquidity_ceiling : {}) as Record<string, unknown>;
  const empSummary = (o.empirical_summary && typeof o.empirical_summary === "object" ? o.empirical_summary : {}) as Record<string, unknown>;
  const sensProj = o.sensitivity_projection && typeof o.sensitivity_projection === "object"
    ? (o.sensitivity_projection as Record<string, unknown>)
    : null;
  const empProj = o.compounding_projection && typeof o.compounding_projection === "object"
    ? (o.compounding_projection as Record<string, unknown>)
    : {};
  const useSens = Boolean(req.assumed_edge_cents && req.assumed_edge_cents > 0 && sensProj);
  const proj = useSens && sensProj ? sensProj : empProj;

  let p10Eq: number[] = [];
  let p50Eq: number[] = [];
  let p90Eq: number[] = [];
  let p10Day: number[] = [];
  let p50Day: number[] = [];
  let p90Day: number[] = [];

  if (Array.isArray(proj.p50_equity)) {
    p10Eq = Array.isArray(proj.p10_equity) ? proj.p10_equity.filter(isNum) : [];
    p50Eq = proj.p50_equity.filter(isNum);
    p90Eq = Array.isArray(proj.p90_equity) ? proj.p90_equity.filter(isNum) : [];
    p10Day = Array.isArray(proj.p10_daily) ? proj.p10_daily.filter(isNum) : [];
    p50Day = Array.isArray(proj.p50_daily) ? proj.p50_daily.filter(isNum) : [];
    p90Day = Array.isArray(proj.p90_daily) ? proj.p90_daily.filter(isNum) : [];
  } else {
    const pts = Array.isArray(proj.curve)
      ? proj.curve
      : Array.isArray(proj.points)
        ? proj.points
        : Array.isArray(proj.days_curve)
          ? proj.days_curve
          : Array.isArray(proj.bands)
            ? proj.bands
            : Array.isArray(proj.trajectory)
              ? proj.trajectory
              : [];
    for (const row of pts) {
      if (!row || typeof row !== "object") continue;
      const r = row as Record<string, unknown>;
      const e10 = pickNum(r, ["equity_p10", "p10_equity", "p10"]);
      const e50 = pickNum(r, ["equity_p50", "p50_equity", "p50"]);
      const e90 = pickNum(r, ["equity_p90", "p90_equity", "p90"]);
      if (e10 !== null && e50 !== null && e90 !== null) {
        p10Eq.push(e10);
        p50Eq.push(e50);
        p90Eq.push(e90);
        p10Day.push(pickNum(r, ["daily_run_rate_p10", "p10_daily", "daily_p10", "run_rate_p10"]) ?? 0);
        p50Day.push(pickNum(r, ["daily_run_rate_p50", "p50_daily", "daily_p50", "run_rate_p50"]) ?? 0);
        p90Day.push(pickNum(r, ["daily_run_rate_p90", "p90_daily", "daily_p90", "run_rate_p90"]) ?? 0);
      }
    }
  }

  if (p50Eq.length < 2) return null;
  if (p50Day.length !== p50Eq.length) {
    p10Day = p10Eq.map(() => 0);
    p50Day = p50Eq.map(() => 0);
    p90Day = p90Eq.map(() => 0);
  }

  const rgRaw = o.readiness_gates;
  const gates: ReadinessGate[] = [];
  let verdict: "READY_FOR_OWNER_REVIEW" | "PAPER_RESEARCH_ONLY" = "PAPER_RESEARCH_ONLY";
  let blockingReasons: string[] = [];

  if (rgRaw && typeof rgRaw === "object") {
    const rgObj = rgRaw as Record<string, unknown>;
    const rawList = Array.isArray(rgRaw)
      ? rgRaw
      : Array.isArray(rgObj.gates)
        ? rgObj.gates
        : Array.isArray(rgObj.checks)
          ? rgObj.checks
          : [];
    for (const g of rawList) {
      if (!g || typeof g !== "object") continue;
      const go = g as Record<string, unknown>;
      const id = String(go.id || go.name || gates.length);
      const unit = typeof go.unit === "string" ? go.unit : "";
      const actualDisplay =
        typeof go.actual_display === "string" && go.actual_display
          ? go.actual_display
          : "observed" in go
            ? formatGateObserved(go.observed, unit)
            : String(go.actual ?? go.measured ?? "N/A");
      const thresholdDisplay =
        typeof go.threshold_display === "string" && go.threshold_display
          ? go.threshold_display
          : isNum(go.threshold)
            ? formatGateThreshold(id, go.threshold, unit)
            : String(go.threshold ?? "-");
      gates.push({
        id,
        label: String(go.label || go.name || go.id || "Gate"),
        passed: Boolean(go.passed ?? go.ok),
        actualDisplay,
        thresholdDisplay,
        reason: String(go.reason ?? go.detail ?? ""),
      });
    }
    const vRaw = String(rgObj.verdict ?? o.verdict ?? "").toUpperCase();
    verdict = vRaw === "READY_FOR_OWNER_REVIEW" ? "READY_FOR_OWNER_REVIEW" : "PAPER_RESEARCH_ONLY";
    if (Array.isArray(rgObj.blocking_reasons)) {
      blockingReasons = rgObj.blocking_reasons.map((x) => String(x));
    } else if (Array.isArray(o.blocking_reasons)) {
      blockingReasons = o.blocking_reasons.map((x) => String(x));
    } else {
      blockingReasons = gates.filter((g) => !g.passed).map((g) => `${g.label}: ${g.reason}`);
    }
  }

  const assumedEdge =
    isNum(o.assumed_edge_cents)
      ? o.assumed_edge_cents
      : req.assumed_edge_cents !== undefined && req.assumed_edge_cents !== null && req.assumed_edge_cents > 0
        ? req.assumed_edge_cents
        : null;
  const reqEq = pickNum(o, ["required_bankroll_equal_slots"]);
  const reqQk = pickNum(o, ["required_bankroll_quarter_kelly"]);
  const reqHk = pickNum(o, ["required_bankroll_half_kelly"]);
  const expCents =
    pickNum(o, ["oos_expectancy_cents_per_contract", "oos_expectancy_cents"]) ??
    pickNum(empSummary, [
      "oos_net_expectancy_cents",
      "oos_net_expectancy_cents_per_contract",
      "net_expectancy_cents_per_contract",
      "expectancy_cents_per_contract",
    ]);
  const oosPos =
    typeof o.oos_edge_positive === "boolean"
      ? o.oos_edge_positive
      : (expCents !== null && expCents > 0) || reqEq !== null;

  const projModeLabel = typeof proj.mode_label === "string" ? proj.mode_label : typeof o.mode_label === "string" ? o.mode_label : "";

  return {
    modeLabel:
      /sensitivity/i.test(projModeLabel) || useSens
        ? "Sensitivity (Assumed Edge)"
        : assumedEdge !== null && assumedEdge > 0 && proj.is_empirical === false
          ? "Sensitivity (Assumed Edge)"
          : "Empirical Tape",
    assumedEdgeCents: assumedEdge,
    targetDailyDollars: pickNum(o, ["target_daily_dollars"]) ?? req.target_daily_dollars,
    startingBankroll: pickNum(o, ["starting_bankroll"]) ?? pickNum(proj, ["starting_bankroll"]) ?? req.starting_bankroll,
    sizingMode: (typeof o.sizing_mode_resolved === "string"
      ? o.sizing_mode_resolved
      : typeof o.sizing_mode === "string"
        ? o.sizing_mode
        : req.sizing_mode) as SizingMode,
    currentEmpiricalDailyDollars: pickNum(o, ["current_empirical_daily_dollars", "empirical_daily_dollars"]) ?? 0,
    oosExpectancyCentsPerContract: expCents,
    oosEdgePositive: oosPos,
    requiredBankrollEqualSlots: reqEq,
    requiredBankrollQuarterKelly: reqQk,
    requiredBankrollHalfKelly: reqHk,
    negativeEdgeReason:
      typeof o.required_bankroll_reason === "string" && o.required_bankroll_reason
        ? o.required_bankroll_reason
        : typeof o.negative_edge_reason === "string" && o.negative_edge_reason
          ? o.negative_edge_reason
          : typeof o.honest_reason === "string" && o.honest_reason
            ? o.honest_reason
            : reqEq === null
              ? "Out-of-sample expectancy after fees and slippage is non-positive."
              : null,
    liquidityCeiling: {
      singleSeriesMaxDailyDollars: pickNum(liq, ["single_series_max_daily_dollars", "single_series_daily_dollars"]) ?? 0,
      portfolioMaxDailyDollars: pickNum(liq, ["multi_series_max_daily_dollars", "portfolio_max_daily_dollars", "portfolio_daily_dollars"]) ?? 0,
      activeSeriesCount: pickNum(liq, ["active_series_count"]) ?? req.active_series_count,
      maxContractsPerWindow: pickNum(liq, ["max_contracts_per_window"]) ?? req.max_contracts_per_window,
      windowsPerDay: pickNum(liq, ["windows_per_day_per_series", "windows_per_day"]) ?? 96,
      fillRatioAtTarget: normPct01(pickNum(liq, ["fill_ratio_at_target", "fill_ratio"])),
    },
    compoundingProjection: {
      days: pickNum(proj, ["days", "horizon_days"]) ?? Math.max(1, p50Eq.length - 1),
      p10Equity: p10Eq,
      p50Equity: p50Eq,
      p90Equity: p90Eq,
      p10Daily: p10Day,
      p50Daily: p50Day,
      p90Daily: p90Day,
      probReachTarget: normPct01(pickNum(proj, ["prob_reach_target", "prob_reach_target_pct"])),
      medianDaysToTarget: pickNum(proj, ["median_days_to_target"]),
      probDdGte25Pct: normPct01(pickNum(proj, ["prob_dd_gte_25_pct", "prob_dd_25"])),
    },
    readinessGates: {
      verdict,
      gates,
      blockingReasons,
    },
    seed: pickNum(o, ["seed"]) ?? pickNum(proj, ["seed"]) ?? req.seed,
    paths: pickNum(o, ["paths"]) ?? pickNum(proj, ["paths"]) ?? req.paths,
    elapsedMs: pickNum(o, ["elapsed_ms"]) ?? 1,
    source: "server",
  };
}

export async function fetchRecalibration(
  dataset: Dataset,
  home: Home | null,
  markets: Market[],
  slots?: Slots | null,
): Promise<RecalibrationData> {
  if (supportsV060(home, "recalibration")) {
    try {
      const qs = dataset === "fixture" ? "?dataset=fixture" : "";
      const r = await fetch(`/api/analytics/recalibration${qs}`);
      if (r.ok) {
        const parsed = normalizeRecalibration(await r.json());
        if (parsed) return parsed;
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return computeRecalibrationClient(markets, slots);
}

export async function runScalePlan(
  req: ScalePlanRequest,
  dataset: Dataset,
  home: Home | null,
  markets: Market[],
  slots?: Slots | null,
): Promise<ScalePlanData> {
  if (supportsV060(home, "scale_plan")) {
    try {
      const qs = dataset === "fixture" ? "?dataset=fixture" : "";
      const r = await fetch(`/api/analytics/scale-plan${qs}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ ...req, dataset }),
      });
      if (r.ok) {
        const parsed = normalizeScalePlan(await r.json(), req);
        if (parsed) return parsed;
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return computeScalePlanClient({ ...req, dataset }, markets, slots);
}
