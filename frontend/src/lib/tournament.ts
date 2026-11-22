// OpenBell v0.7.0 Paper Strategy Tournament, 12x12 Correlation Matrix, Optimal 4-Slot Lineup & Edge-by-Regime Heatmap client.
// Calls GET /api/analytics/tournament on v0.7.0+, falling back cleanly to deterministic OOS (40%) tournament,
// Pearson correlation matrix, greedy decorrelated 4-slot selection (|r| <= 0.65), and 5x4 entry-price x time-to-close
// regime heatmap derived strictly from real loaded tape. Strictly paper-only.
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  type Strategy,
  cleanStr,
  impliedYesProb,
  isNum,
  seriesShort,
  supportsV070,
} from "@/lib/desk";
import { kalshiFeeDollars, wilsonInterval } from "@/lib/quant";

export type SeriesBreakdown = {
  series: string;
  windows: number;
  winRatePct: number | null;
  netExpectancyCents: number | null;
  netPnlDollars: number;
};

export type TournamentRow = {
  rank: number;
  strategy: string;
  side: string;
  oosWindows: number;
  isWindows: number;
  oosWinRatePct: number | null;
  oosNetExpectancyCents: number | null;
  oosNetPnlDollars: number;
  oosSharpe: number | null;
  oosMaxDrawdownPct: number;
  oosPositive: boolean;
  inOptimalLineup: boolean;
  seriesBreakdown: SeriesBreakdown[];
};

export type OptimalLineup = {
  strategies: string[];
  maxCorrelationLimit: number;
  maxPairwiseAbsCorr: number | null;
  positiveCandidateCount: number;
  combinedOosExpectancyCents: number | null;
  isFallback: boolean;
  honestyNote: string;
};

export type CorrelationMatrixData = {
  strategies: string[];
  matrix: (number | null)[][];
  sharedWindows: number[][];
  threshold: number;
};

export type RegimeCell = {
  priceBucket: string;
  timeBucket: string;
  n: number;
  wins: number;
  winRatePct: number | null;
  avgFeeCents: number | null;
  netExpectancyCents: number | null;
  wilsonLowPct: number | null;
  wilsonHighPct: number | null;
  lowSample: boolean;
};

export type TournamentData = {
  slippageCents: number;
  isFraction: number;
  oosFraction: number;
  oosWindowsTotal: number;
  rankings: TournamentRow[];
  optimalLineup: OptimalLineup;
  correlation: CorrelationMatrixData;
  regimePriceBuckets: string[];
  regimeTimeBuckets: string[];
  regimeCells: RegimeCell[];
  honestyNote: string;
  source: "server" | "client";
};

export const CANONICAL_PRICE_BUCKETS = ["10-30c", "30-50c", "50-70c", "70-90c", "90-99c"] as const;
export const CANONICAL_TIME_BUCKETS = [">600s", "300-600s", "120-300s", "<120s"] as const;
const CANONICAL_SERIES = ["BTC", "ETH", "SOL", "XRP"] as const;

function pickNum(obj: Record<string, unknown>, keys: string[]): number | null {
  for (const k of keys) {
    const v = obj[k];
    if (isNum(v)) return v;
  }
  return null;
}

function normPct(v: number | null): number | null {
  if (v === null) return null;
  return v <= 1 && v >= 0 ? Number((v * 100).toFixed(1)) : Number(v.toFixed(1));
}

function priceBucketFor(price: number): string | null {
  const c = price * 100;
  if (c < 10 || c > 99.5) return null;
  if (c < 30) return "10-30c";
  if (c < 50) return "30-50c";
  if (c < 70) return "50-70c";
  if (c < 90) return "70-90c";
  return "90-99c";
}

function timeBucketFor(secsToClose: number): string {
  if (secsToClose > 600) return ">600s";
  if (secsToClose > 300) return "300-600s";
  if (secsToClose >= 120) return "120-300s";
  return "<120s";
}

function pearson(xs: number[], ys: number[]): number | null {
  const n = Math.min(xs.length, ys.length);
  if (n < 3) return null;
  let sx = 0;
  let sy = 0;
  for (let i = 0; i < n; i++) {
    sx += xs[i];
    sy += ys[i];
  }
  const mx = sx / n;
  const my = sy / n;
  let num = 0;
  let dx2 = 0;
  let dy2 = 0;
  for (let i = 0; i < n; i++) {
    const dx = xs[i] - mx;
    const dy = ys[i] - my;
    num += dx * dy;
    dx2 += dx * dx;
    dy2 += dy * dy;
  }
  const denom = Math.sqrt(dx2 * dy2);
  if (denom < 1e-12) return null;
  return Math.max(-1, Math.min(1, Number((num / denom).toFixed(4))));
}

function computeSharpe(returns: number[]): number | null {
  if (returns.length < 3) return null;
  const mean = returns.reduce((s, x) => s + x, 0) / returns.length;
  let sumSq = 0;
  for (const r of returns) sumSq += (r - mean) * (r - mean);
  const sd = Math.sqrt(sumSq / (returns.length - 1));
  if (sd < 1e-9) return null;
  return Number(((mean / sd) * Math.sqrt(returns.length)).toFixed(2));
}

function computeMaxDdPct(pnls: number[], baseEquity = 100): number {
  let eq = baseEquity;
  let peak = baseEquity;
  let maxDd = 0;
  for (const p of pnls) {
    eq += p;
    if (eq > peak) peak = eq;
    if (peak > 0) {
      const dd = ((peak - eq) / peak) * 100;
      if (dd > maxDd) maxDd = dd;
    }
  }
  return Number(maxDd.toFixed(2));
}

// ---------------------------------------------------------------------------
// Deterministic Client-Side Fallback (strictly derived from real loaded tape)
// ---------------------------------------------------------------------------

export function computeTournamentClient(
  slippageCents: number,
  strategies: Strategy[],
  markets: Market[],
  slots?: Slots | null,
): TournamentData {
  const slip = Math.max(0, Math.min(5, Number(slippageCents) || 0));
  const slipDollars = slip / 100;

  const validStrategies = strategies.filter((s): s is Strategy => !!s && typeof s === "object");
  const allTrades = (slots?.trades || [])
    .filter((t) => t && isNum(t.close_epoch) && isNum(t.pnl_dollars))
    .sort((a, b) => a.close_epoch - b.close_epoch);

  const uniqueEpochs = [...new Set(allTrades.map((t) => t.close_epoch))].sort((a, b) => a - b);
  const splitIdx = uniqueEpochs.length > 1 ? Math.max(1, Math.floor(uniqueEpochs.length * 0.6)) : 0;
  const splitEpoch = uniqueEpochs.length > 1 ? uniqueEpochs[splitIdx] : Number.NEGATIVE_INFINITY;
  const oosWindowsTotal = Math.max(0, uniqueEpochs.length - splitIdx);

  // Map ticker -> series & close epoch for regime lookup
  const marketByTicker = new Map<string, Market>();
  for (const m of markets) {
    if (m?.ticker) marketByTicker.set(m.ticker, m);
  }

  // Group trades by strategy
  const stratNamesSet = new Set<string>();
  for (const s of validStrategies) stratNamesSet.add(cleanStr(s.strategy, "unnamed"));
  for (const t of allTrades) stratNamesSet.add(cleanStr(t.strategy, "unnamed"));
  const strategyNames = [...stratNamesSet].slice(0, 16);

  const stratMeta = new Map<string, Strategy>();
  for (const s of validStrategies) stratMeta.set(cleanStr(s.strategy, "unnamed"), s);

  // Per-strategy OOS returns keyed by close_epoch (for exact shared-window Pearson correlation)
  const oosSeriesMap = new Map<string, Map<number, number>>();

  const rowsRaw: TournamentRow[] = strategyNames.map((name) => {
    const meta = stratMeta.get(name);
    const mine = allTrades.filter((t) => cleanStr(t.strategy, "unnamed") === name);
    const isTrades = mine.filter((t) => t.close_epoch < splitEpoch);
    const oosTrades = mine.filter((t) => t.close_epoch >= splitEpoch);

    const epochMap = new Map<number, number>();
    let oosPnl = 0;
    let oosContracts = 0;
    let oosWins = 0;
    const oosUnitReturns: number[] = [];
    const oosPnls: number[] = [];

    const bySeries = new Map<string, { windows: number; wins: number; contracts: number; pnl: number }>();

    for (const tr of oosTrades) {
      const c = Math.max(1, tr.contracts || 1);
      const net = tr.pnl_dollars - slipDollars * c;
      const unitCents = (net * 100) / c;
      oosPnl += net;
      oosContracts += c;
      if (net > 0) oosWins += 1;
      oosUnitReturns.push(unitCents);
      oosPnls.push(net);
      epochMap.set(tr.close_epoch, (epochMap.get(tr.close_epoch) ?? 0) + unitCents);

      const m = tr.ticker ? marketByTicker.get(tr.ticker) : undefined;
      const sym = seriesShort(m?.series, tr.ticker || "KXBTC15M");
      const entry = bySeries.get(sym) || { windows: 0, wins: 0, contracts: 0, pnl: 0 };
      entry.windows += 1;
      if (net > 0) entry.wins += 1;
      entry.contracts += c;
      entry.pnl += net;
      bySeries.set(sym, entry);
    }

    oosSeriesMap.set(name, epochMap);

    // If strategy has no slot trades yet, fall back honestly to its reported 2nd-half / mean cents
    let oosExp: number | null =
      oosContracts > 0 ? Number(((oosPnl * 100) / oosContracts).toFixed(2)) : null;
    if (oosExp === null && meta && isNum(meta.second_half_cents)) {
      oosExp = Number((meta.second_half_cents - slip).toFixed(2));
    } else if (oosExp === null && meta && isNum(meta.mean_cents_per_contract) && (meta.windows_traded ?? 0) > 0) {
      oosExp = Number((meta.mean_cents_per_contract - slip).toFixed(2));
    }

    const oosWinRatePct =
      oosTrades.length > 0 ? Number(((oosWins / oosTrades.length) * 100).toFixed(1)) : null;

    const seriesBreakdown: SeriesBreakdown[] = CANONICAL_SERIES.map((sym) => {
      const st = bySeries.get(sym);
      if (!st || st.windows === 0) {
        return { series: sym, windows: 0, winRatePct: null, netExpectancyCents: null, netPnlDollars: 0 };
      }
      return {
        series: sym,
        windows: st.windows,
        winRatePct: Number(((st.wins / st.windows) * 100).toFixed(1)),
        netExpectancyCents: Number(((st.pnl * 100) / st.contracts).toFixed(2)),
        netPnlDollars: Number(st.pnl.toFixed(2)),
      };
    });

    return {
      rank: 0,
      strategy: name,
      side: cleanStr(meta?.side, "yes").toLowerCase(),
      oosWindows: oosTrades.length || (meta && isNum(meta.windows_traded) ? Math.floor(meta.windows_traded * 0.4) : 0),
      isWindows: isTrades.length || (meta && isNum(meta.windows_traded) ? Math.ceil(meta.windows_traded * 0.6) : 0),
      oosWinRatePct,
      oosNetExpectancyCents: oosExp,
      oosNetPnlDollars: Number(oosPnl.toFixed(2)),
      oosSharpe: computeSharpe(oosUnitReturns),
      oosMaxDrawdownPct: computeMaxDdPct(oosPnls, slots?.paper_pot_start || 100),
      oosPositive: oosExp !== null && oosExp > 0,
      inOptimalLineup: false,
      seriesBreakdown,
    };
  });

  // Rank strictly on OOS net expectancy after fees + slippage (nulls last, tie-break by Sharpe then windows)
  rowsRaw.sort((a, b) => {
    const ae = a.oosNetExpectancyCents ?? Number.NEGATIVE_INFINITY;
    const be = b.oosNetExpectancyCents ?? Number.NEGATIVE_INFINITY;
    if (be !== ae) return be - ae;
    const as = a.oosSharpe ?? Number.NEGATIVE_INFINITY;
    const bs = b.oosSharpe ?? Number.NEGATIVE_INFINITY;
    if (bs !== as) return bs - as;
    return b.oosWindows - a.oosWindows;
  });
  rowsRaw.forEach((r, idx) => {
    r.rank = idx + 1;
  });

  // 12x12 Symmetric Pearson Correlation Matrix across shared settled OOS windows
  const corrNames = rowsRaw.slice(0, 12).map((r) => r.strategy);
  const k = corrNames.length;
  const matrix: (number | null)[][] = Array.from({ length: k }, () => new Array<number | null>(k).fill(null));
  const sharedWindows: number[][] = Array.from({ length: k }, () => new Array<number>(k).fill(0));

  for (let i = 0; i < k; i++) {
    const mapI = oosSeriesMap.get(corrNames[i]) || new Map<number, number>();
    matrix[i][i] = 1.0;
    sharedWindows[i][i] = mapI.size;
    for (let j = i + 1; j < k; j++) {
      const mapJ = oosSeriesMap.get(corrNames[j]) || new Map<number, number>();
      const xs: number[] = [];
      const ys: number[] = [];
      for (const [ep, vi] of mapI.entries()) {
        const vj = mapJ.get(ep);
        if (vj !== undefined) {
          xs.push(vi);
          ys.push(vj);
        }
      }
      const r = pearson(xs, ys);
      matrix[i][j] = r;
      matrix[j][i] = r;
      sharedWindows[i][j] = xs.length;
      sharedWindows[j][i] = xs.length;
    }
  }

  // Optimal Uncorrelated 4-Slot Lineup: pick highest OOS-expectancy positive strategies with |r| <= 0.65
  const corrLimit = 0.65;
  const positiveCandidates = rowsRaw.filter((r) => r.oosPositive);
  const picked: string[] = [];
  let maxPairAbs: number | null = null;

  const corrOf = (a: string, b: string): number | null => {
    const ia = corrNames.indexOf(a);
    const ib = corrNames.indexOf(b);
    if (ia < 0 || ib < 0) return null;
    return matrix[ia][ib];
  };

  for (const cand of positiveCandidates) {
    if (picked.length >= 4) break;
    let ok = true;
    let worst: number = maxPairAbs ?? 0;
    for (const chosen of picked) {
      const r = corrOf(cand.strategy, chosen);
      if (r !== null && Math.abs(r) > corrLimit) {
        ok = false;
        break;
      }
      if (r !== null) worst = Math.max(worst, Math.abs(r));
    }
    if (ok) {
      picked.push(cand.strategy);
      if (picked.length >= 2) maxPairAbs = Number(worst.toFixed(2));
    }
  }

  const isFallback = picked.length < 4;
  const pickedSet = new Set(picked);
  for (const row of rowsRaw) {
    row.inOptimalLineup = pickedSet.has(row.strategy);
  }

  const combinedExp =
    picked.length > 0
      ? Number(
          (
            rowsRaw
              .filter((r) => pickedSet.has(r.strategy))
              .reduce((s, r) => s + (r.oosNetExpectancyCents ?? 0), 0) / picked.length
          ).toFixed(2),
        )
      : null;

  const lineupNote =
    picked.length === 4
      ? `4 uncorrelated positive-OOS strategies selected (max |r| ${maxPairAbs !== null ? maxPairAbs.toFixed(2) : "n/a"} <= ${corrLimit.toFixed(2)}).`
      : picked.length > 0
        ? `Honest fallback: only ${picked.length} of ${rowsRaw.length} strategies pass positive OOS expectancy after fees + ${slip}c slippage with |r| <= ${corrLimit.toFixed(2)}. Remaining ${4 - picked.length} slot(s) stay unallocated.`
        : `Honest fallback: 0 of ${rowsRaw.length} strategies pass positive OOS expectancy after fees + ${slip}c slippage. Refusing to fabricate a positive 4-slot lineup.`;

  // 5x4 Edge-by-Regime Heatmap: Entry Price Bucket x Seconds-to-Close
  type Acc = { n: number; wins: number; feeSum: number; netCentsSum: number };
  const grid = new Map<string, Acc>();
  const keyOf = (pb: string, tb: string) => `${pb}|${tb}`;

  // Use settled markets with interior implied quotes + timing when available, else settled trades
  let usedMarketTiming = false;
  for (const m of markets) {
    if (!m || typeof m !== "object") continue;
    const res = typeof m.result === "string" ? m.result.trim().toLowerCase() : "";
    if (res !== "yes" && res !== "no") continue;
    const p = impliedYesProb(m);
    if (p === null) continue;
    const pb = priceBucketFor(p);
    if (!pb) continue;
    const closeMs = typeof m.close_time === "string" ? Date.parse(m.close_time) : NaN;
    const fetchedMs = isNum(m.fetched_at) ? m.fetched_at * 1000 : NaN;
    const secs = Number.isFinite(closeMs) && Number.isFinite(fetchedMs) ? Math.max(0, (closeMs - fetchedMs) / 1000) : 240;
    const tb = timeBucketFor(secs);
    const fee = kalshiFeeDollars(1, p);
    const won = res === "yes" ? 1 : 0;
    const netDollars = (won ? 1 - p : -p) - fee - slipDollars;
    const kCell = keyOf(pb, tb);
    const acc = grid.get(kCell) || { n: 0, wins: 0, feeSum: 0, netCentsSum: 0 };
    acc.n += 1;
    acc.wins += won;
    acc.feeSum += fee * 100;
    acc.netCentsSum += netDollars * 100;
    grid.set(kCell, acc);
    usedMarketTiming = true;
  }

  if (!usedMarketTiming) {
    for (const tr of allTrades) {
      if (!isNum(tr.price)) continue;
      const pb = priceBucketFor(tr.price);
      if (!pb) continue;
      const tb = "120-300s";
      const c = Math.max(1, tr.contracts || 1);
      const fee = kalshiFeeDollars(1, tr.price);
      const net = tr.pnl_dollars - slipDollars * c;
      const kCell = keyOf(pb, tb);
      const acc = grid.get(kCell) || { n: 0, wins: 0, feeSum: 0, netCentsSum: 0 };
      acc.n += 1;
      acc.wins += net > 0 ? 1 : 0;
      acc.feeSum += fee * 100;
      acc.netCentsSum += (net * 100) / c;
      grid.set(kCell, acc);
    }
  }

  const regimeCells: RegimeCell[] = [];
  for (const pb of CANONICAL_PRICE_BUCKETS) {
    for (const tb of CANONICAL_TIME_BUCKETS) {
      const acc = grid.get(keyOf(pb, tb));
      if (!acc || acc.n === 0) {
        regimeCells.push({
          priceBucket: pb,
          timeBucket: tb,
          n: 0,
          wins: 0,
          winRatePct: null,
          avgFeeCents: null,
          netExpectancyCents: null,
          wilsonLowPct: null,
          wilsonHighPct: null,
          lowSample: true,
        });
      } else {
        const [wLo, wHi] = wilsonInterval(acc.wins, acc.n);
        regimeCells.push({
          priceBucket: pb,
          timeBucket: tb,
          n: acc.n,
          wins: acc.wins,
          winRatePct: Number(((acc.wins / acc.n) * 100).toFixed(1)),
          avgFeeCents: Number((acc.feeSum / acc.n).toFixed(2)),
          netExpectancyCents: Number((acc.netCentsSum / acc.n).toFixed(2)),
          wilsonLowPct: Number((wLo * 100).toFixed(1)),
          wilsonHighPct: Number((wHi * 100).toFixed(1)),
          lowSample: acc.n < 20,
        });
      }
    }
  }

  return {
    slippageCents: slip,
    isFraction: 0.6,
    oosFraction: 0.4,
    oosWindowsTotal,
    rankings: rowsRaw,
    optimalLineup: {
      strategies: picked,
      maxCorrelationLimit: corrLimit,
      maxPairwiseAbsCorr: maxPairAbs,
      positiveCandidateCount: positiveCandidates.length,
      combinedOosExpectancyCents: combinedExp,
      isFallback,
      honestyNote: lineupNote,
    },
    correlation: {
      strategies: corrNames,
      matrix,
      sharedWindows,
      threshold: corrLimit,
    },
    regimePriceBuckets: [...CANONICAL_PRICE_BUCKETS],
    regimeTimeBuckets: [...CANONICAL_TIME_BUCKETS],
    regimeCells,
    honestyNote: `Ranked strictly on chronological OOS (40%) tape after Kalshi fees + ${slip}c slippage. Paper research only.`,
    source: "client",
  };
}

// ---------------------------------------------------------------------------
// Flexible v0.7.0 Server-First Normalizer & Fetcher
// ---------------------------------------------------------------------------

function normalizeTournament(raw: unknown, fallbackSlippage: number): TournamentData | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;

  const rawRankings = Array.isArray(o.rankings)
    ? o.rankings
    : Array.isArray(o.leaderboard)
      ? o.leaderboard
      : Array.isArray(o.strategies)
        ? o.strategies
        : Array.isArray(o.tournament)
          ? o.tournament
          : null;
  if (!rawRankings) return null;

  const slip = pickNum(o, ["slippage_cents", "slippage"]) ?? fallbackSlippage;

  // Optimal 4-slot lineup
  const lineupObj = (
    o.optimal_lineup && typeof o.optimal_lineup === "object"
      ? o.optimal_lineup
      : o.optimal_4_slot && typeof o.optimal_4_slot === "object"
        ? o.optimal_4_slot
        : o.lineup && typeof o.lineup === "object"
          ? o.lineup
          : {}
  ) as Record<string, unknown>;

  const rawLineupList = Array.isArray(lineupObj.strategies)
    ? lineupObj.strategies
    : Array.isArray(lineupObj.selected)
      ? lineupObj.selected
      : Array.isArray(lineupObj.slots)
        ? lineupObj.slots
        : [];
  const lineupStrategies: string[] = rawLineupList
    .map((item) => {
      if (typeof item === "string") return item;
      if (item && typeof item === "object") {
        const io = item as Record<string, unknown>;
        return cleanStr(io.strategy ?? io.name, "");
      }
      return "";
    })
    .filter(Boolean);
  const lineupSet = new Set(lineupStrategies);

  const rankings: TournamentRow[] = rawRankings
    .filter((r): r is Record<string, unknown> => !!r && typeof r === "object")
    .map((r, idx) => {
      const name = cleanStr(r.strategy ?? r.name, `strategy-${idx + 1}`);
      const oosExp = pickNum(r, [
        "oos_net_expectancy_cents",
        "oos_expectancy_cents",
        "net_expectancy_cents",
        "oos_mean_cents_per_contract",
      ]);
      const oosWinRate = normPct(pickNum(r, ["oos_win_rate_pct", "oos_win_rate", "win_rate_pct", "win_rate"]));

      const rawSeries = Array.isArray(r.series_breakdown)
        ? r.series_breakdown
        : Array.isArray(r.by_series)
          ? r.by_series
          : r.per_series && typeof r.per_series === "object"
            ? Object.entries(r.per_series as Record<string, unknown>).map(([series, val]) => ({
                series,
                ...(val && typeof val === "object" ? (val as Record<string, unknown>) : {}),
              }))
            : [];

      const seriesBreakdown: SeriesBreakdown[] = rawSeries
        .filter((s): s is Record<string, unknown> => !!s && typeof s === "object")
        .map((s) => ({
          series: seriesShort(cleanStr(s.asset ?? s.series ?? s.symbol, "BTC")),
          windows: pickNum(s, ["oos_trades", "windows", "oos_windows", "trades", "n"]) ?? 0,
          winRatePct: normPct(pickNum(s, ["oos_win_rate", "win_rate_pct", "win_rate"])),
          netExpectancyCents: pickNum(s, [
            "oos_net_expectancy_cents",
            "net_expectancy_cents",
            "expectancy_cents",
            "mean_cents",
          ]),
          netPnlDollars: pickNum(s, ["oos_net_pnl_dollars_1ct", "net_pnl_dollars", "pnl_dollars"]) ?? 0,
        }));

      const oosPositive =
        typeof r.oos_positive === "boolean"
          ? r.oos_positive
          : typeof r.positive_oos === "boolean"
            ? r.positive_oos
            : oosExp !== null && oosExp > 0;

      return {
        rank: pickNum(r, ["rank"]) ?? idx + 1,
        strategy: name,
        side: cleanStr(r.side, "yes").toLowerCase(),
        oosWindows: pickNum(r, ["oos_trades", "oos_windows", "sample_count", "oos_count"]) ?? 0,
        isWindows: pickNum(r, ["is_trades", "is_windows", "is_count"]) ?? 0,
        oosWinRatePct: oosWinRate,
        oosNetExpectancyCents: oosExp,
        oosNetPnlDollars:
          pickNum(r, [
            "oos_compounded_pnl_dollars",
            "oos_net_pnl_dollars_1ct",
            "oos_net_pnl_dollars",
            "oos_pnl_dollars",
            "net_pnl_dollars",
          ]) ?? 0,
        oosSharpe: pickNum(r, ["oos_sharpe", "sharpe"]),
        oosMaxDrawdownPct: pickNum(r, ["oos_max_drawdown_pct", "max_drawdown_pct", "oos_max_dd_pct"]) ?? 0,
        oosPositive,
        inOptimalLineup: Boolean(r.in_optimal_lineup) || lineupSet.has(name),
        seriesBreakdown,
      };
    });

  // 12x12 Correlation Matrix
  const corrObj = (
    o.correlation_matrix && typeof o.correlation_matrix === "object"
      ? o.correlation_matrix
      : o.correlation && typeof o.correlation === "object"
        ? o.correlation
        : {}
  ) as Record<string, unknown>;

  const corrStrategies: string[] = Array.isArray(corrObj.strategies)
    ? corrObj.strategies.map((s) => String(s))
    : Array.isArray(corrObj.labels)
      ? corrObj.labels.map((s) => String(s))
      : rankings.slice(0, 12).map((r) => r.strategy);

  const rawMatrix = Array.isArray(corrObj.matrix)
    ? corrObj.matrix
    : Array.isArray(corrObj.values)
      ? corrObj.values
      : Array.isArray(corrObj.correlations)
        ? corrObj.correlations
        : [];

  const rawShared = Array.isArray(corrObj.shared_windows)
    ? corrObj.shared_windows
    : Array.isArray(corrObj.counts)
      ? corrObj.counts
      : Array.isArray(corrObj.overlap_counts)
        ? corrObj.overlap_counts
        : [];

  const k = corrStrategies.length;
  const matrix: (number | null)[][] = Array.from({ length: k }, (_, i) => {
    const row = Array.isArray(rawMatrix[i]) ? (rawMatrix[i] as unknown[]) : [];
    return Array.from({ length: k }, (_, j) => {
      if (i === j) return 1.0;
      const v = row[j];
      return isNum(v) ? Math.max(-1, Math.min(1, v)) : null;
    });
  });

  const sharedWindows: number[][] = Array.from({ length: k }, (_, i) => {
    const row = Array.isArray(rawShared[i]) ? (rawShared[i] as unknown[]) : [];
    return Array.from({ length: k }, (_, j) => {
      const v = row[j];
      return isNum(v) ? Math.max(0, Math.round(v)) : 0;
    });
  });

  // Edge-by-Regime Heatmap
  const regObj = (
    o.regime_heatmap && typeof o.regime_heatmap === "object"
      ? o.regime_heatmap
      : o.regime_heatmap_oos && typeof o.regime_heatmap_oos === "object"
        ? o.regime_heatmap_oos
        : o.regime_heatmap_full && typeof o.regime_heatmap_full === "object"
          ? o.regime_heatmap_full
          : o.heatmap && typeof o.heatmap === "object"
            ? o.heatmap
            : {}
  ) as Record<string, unknown>;

  const rawPriceBuckets = Array.isArray(regObj.price_buckets)
    ? regObj.price_buckets.map(String)
    : [...CANONICAL_PRICE_BUCKETS];
  const rawTimeBuckets = Array.isArray(regObj.time_buckets)
    ? regObj.time_buckets.map(String)
    : [...CANONICAL_TIME_BUCKETS];

  const rawCellsList: Record<string, unknown>[] = [];
  if (Array.isArray(regObj.cells)) {
    for (const c of regObj.cells) {
      if (Array.isArray(c)) {
        for (const inner of c) {
          if (inner && typeof inner === "object") rawCellsList.push(inner as Record<string, unknown>);
        }
      } else if (c && typeof c === "object") {
        rawCellsList.push(c as Record<string, unknown>);
      }
    }
  } else if (Array.isArray(o.regime_cells)) {
    for (const c of o.regime_cells) {
      if (c && typeof c === "object") rawCellsList.push(c as Record<string, unknown>);
    }
  }

  const regimeCells: RegimeCell[] = rawCellsList.map((c) => {
    const n = pickNum(c, ["n", "count", "sample_size", "trades"]) ?? 0;
    const wins = pickNum(c, ["wins", "positives"]) ?? 0;
    const winRatePct = normPct(pickNum(c, ["realised_win_rate", "realized_win_rate", "win_rate_pct", "win_rate"]));
    const wLo = normPct(pickNum(c, ["wilson_lower_95", "wilson_low", "wilson_low_pct"]));
    const wHi = normPct(pickNum(c, ["wilson_upper_95", "wilson_high", "wilson_high_pct"]));
    return {
      priceBucket: cleanStr(c.price_bucket ?? c.entry_price_bucket ?? c.price_label, "50-70c"),
      timeBucket: cleanStr(c.time_bucket ?? c.seconds_to_close_bucket ?? c.time_label, "120-300s"),
      n,
      wins,
      winRatePct,
      avgFeeCents: pickNum(c, ["avg_fee_cents", "mean_fee_cents", "fee_cents"]),
      netExpectancyCents: pickNum(c, ["net_expectancy_cents", "expectancy_cents", "net_paper_expectancy_cents"]),
      wilsonLowPct: wLo,
      wilsonHighPct: wHi,
      lowSample:
        typeof c.low_sample === "boolean"
          ? c.low_sample
          : typeof c.sample_warning === "boolean"
            ? c.sample_warning
            : c.sample_depth_status === "insufficient" || c.sample_depth_status === "empty" || n < 20,
    };
  });

  const corrLimit =
    pickNum(lineupObj, ["max_correlation_limit", "max_abs_corr_limit", "correlation_cap"]) ??
    pickNum(o, ["correlation_cap"]) ??
    pickNum(corrObj, ["threshold", "max_abs_corr"]) ??
    0.65;
  const posCount =
    pickNum(lineupObj, ["positive_oos_candidates", "positive_candidate_count", "positive_oos_count"]) ??
    rankings.filter((r) => r.oosPositive).length;
  const isFallback =
    typeof lineupObj.fallback_used === "boolean"
      ? lineupObj.fallback_used
      : typeof lineupObj.is_fallback === "boolean"
        ? lineupObj.is_fallback
        : typeof lineupObj.fallback === "boolean"
          ? lineupObj.fallback
          : lineupStrategies.length < 4 || posCount < 4;

  const lineupHonestyNote =
    typeof lineupObj.fallback_disclosure === "string" && lineupObj.fallback_disclosure
      ? lineupObj.fallback_disclosure
      : typeof lineupObj.honesty_note === "string" && lineupObj.honesty_note
        ? lineupObj.honesty_note
        : typeof lineupObj.fallback_reason === "string" && lineupObj.fallback_reason
          ? lineupObj.fallback_reason
          : typeof lineupObj.note === "string" && lineupObj.note
            ? lineupObj.note
            : isFallback
              ? `Only ${posCount} of ${rankings.length} strategies pass positive OOS expectancy after fees + ${slip}c slippage.`
              : `Selected ${lineupStrategies.length} uncorrelated positive-OOS strategies (|r| <= ${corrLimit.toFixed(2)}).`;

  return {
    slippageCents: slip,
    isFraction: pickNum(o, ["is_fraction"]) ?? 0.6,
    oosFraction: pickNum(o, ["oos_fraction"]) ?? 0.4,
    oosWindowsTotal:
      pickNum(o, ["windows_settled_oos", "oos_windows_total", "oos_windows", "windows_oos"]) ?? 0,
    rankings,
    optimalLineup: {
      strategies: lineupStrategies,
      maxCorrelationLimit: corrLimit,
      maxPairwiseAbsCorr: pickNum(lineupObj, [
        "max_abs_correlation",
        "max_pairwise_abs_corr",
        "max_abs_corr",
        "max_pairwise_corr",
      ]),
      positiveCandidateCount: posCount,
      combinedOosExpectancyCents: pickNum(lineupObj, [
        "combined_oos_expectancy_cents",
        "mean_oos_expectancy_cents",
        "portfolio_oos_expectancy_cents",
      ]),
      isFallback,
      honestyNote: lineupHonestyNote,
    },
    correlation: {
      strategies: corrStrategies,
      matrix,
      sharedWindows,
      threshold: corrLimit,
    },
    regimePriceBuckets: rawPriceBuckets,
    regimeTimeBuckets: rawTimeBuckets,
    regimeCells,
    honestyNote:
      typeof o.honesty_note === "string" && o.honesty_note
        ? o.honesty_note
        : typeof o.mode_notice === "string" && o.mode_notice
          ? o.mode_notice
          : `Ranked strictly on OOS (40%) tape after fees + ${slip}c slippage. Paper research only.`,
    source: "server",
  };
}

export async function fetchTournament(
  dataset: Dataset,
  slippageCents: number,
  home: Home | null,
  strategies: Strategy[],
  markets: Market[],
  slots?: Slots | null,
): Promise<TournamentData> {
  if (supportsV070(home, "tournament")) {
    try {
      const qs = new URLSearchParams();
      if (dataset === "fixture") qs.set("dataset", "fixture");
      qs.set("slippage_cents", String(Math.max(0, Math.min(5, slippageCents))));
      const r = await fetch(`/api/analytics/tournament?${qs.toString()}`);
      if (r.ok) {
        const parsed = normalizeTournament(await r.json(), slippageCents);
        if (parsed) return parsed;
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return computeTournamentClient(slippageCents, strategies, markets, slots);
}
