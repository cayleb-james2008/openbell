// OpenBell v0.5.0 Paper Quant Analytics API client + deterministic client-side fallback engine.
// Strictly paper-only: computes reliability calibration (Wilson 95% CI, Brier, ECE), seeded 1,000-path
// Monte Carlo bootstrap fan curves, chronological 60/40 Walk-Forward IS vs OOS metrics, and in-memory
// Rune strategy sandbox validation + backtesting over loaded 15m tape windows.
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  type Trade,
  impliedYesProb,
  isNum,
  supportsV050,
} from "@/lib/desk";

export type CalibrationBucket = {
  binIndex: number;
  low: number;
  high: number;
  meanPredicted: number;
  realizedRate: number;
  wilsonLow: number;
  wilsonHigh: number;
  count: number;
};

export type CalibrationData = {
  brierScore: number | null;
  ece: number | null;
  totalWindows: number;
  buckets: CalibrationBucket[];
  source: "server" | "client";
};

export type MonteCarloData = {
  paths: number;
  seed: number;
  startEquity: number;
  steps: number;
  percentiles: {
    p5: number[];
    p25: number[];
    p50: number[];
    p75: number[];
    p95: number[];
  };
  terminal: {
    p5: number;
    p25: number;
    p50: number;
    p75: number;
    p95: number;
  };
  ddProb10: number;
  ddProb25: number;
  elapsedMs: number;
  source: "server" | "client";
};

export type SplitMetrics = {
  windows: number;
  netPnlDollars: number;
  sharpe: number | null;
  sortino: number | null;
  profitFactor: number | null;
  maxDrawdownPct: number;
  feeDragPct: number;
  maxWinStreak: number;
  maxLossStreak: number;
};

export type WalkForwardRow = {
  strategy: string;
  isPortfolio: boolean;
  inSample: SplitMetrics;
  outOfSample: SplitMetrics;
};

export type WalkForwardData = {
  inSampleRatio: number;
  rows: WalkForwardRow[];
  source: "server" | "client";
};

export type RuneDiagnostic = {
  level: "error" | "warn" | "info";
  line?: number;
  col?: number;
  message: string;
};

export type RuneValidationResult = {
  valid: boolean;
  name: string;
  side: "yes" | "no" | "unknown";
  diagnostics: RuneDiagnostic[];
  elapsedMs: number;
  source: "server" | "client";
};

export type BacktestSummary = {
  netPnlDollars: number;
  sharpe: number | null;
  sortino: number | null;
  maxDrawdownPct: number;
  brierScore: number | null;
  ece: number | null;
  winRatePct: number;
  windowsTraded: number;
  meanCentsPerContract: number;
  feeDragPct: number;
};

export type BacktestTrade = {
  closeEpoch: number;
  ticker: string;
  side: "yes" | "no";
  price: number;
  contracts: number;
  pnlDollars: number;
  result: string;
};

export type BacktestResult = {
  valid: boolean;
  strategy: string;
  side: "yes" | "no" | "unknown";
  diagnostics: RuneDiagnostic[];
  summary: BacktestSummary;
  equityCurve: [number, number][];
  trades: BacktestTrade[];
  elapsedMs: number;
  source: "server" | "client";
};

export const DEFAULT_SANDBOX_RUNE = `// Paper-only Kalshi 15m Rune research strategy
// Entry quotes are strictly whole-cent (0.01..=0.99); scored on paper only.
pub fn name() {
    "late-favourite-edge"
}

pub fn side() {
    "yes"
}

pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
    yes_ask >= 0.62 && yes_ask <= 0.84 && secs_to_close <= 360 && secs_to_close >= 61
}
`;

// ---------------------------------------------------------------------------
// Exact mathematical helpers (Wilson 95% CI, Kalshi fee model, seeded PRNG)
// ---------------------------------------------------------------------------

export function wilsonInterval(wins: number, n: number, z = 1.95996398454): [number, number] {
  if (n <= 0) return [0, 0];
  const p = Math.min(1, Math.max(0, wins / n));
  const z2 = z * z;
  const denom = 1 + z2 / n;
  const center = (p + z2 / (2 * n)) / denom;
  const margin = (z * Math.sqrt((p * (1 - p) + z2 / (4 * n)) / n)) / denom;
  return [Math.max(0, center - margin), Math.min(1, center + margin)];
}

/** Kalshi taker fee rounded up to whole cents: ceil(0.07 * C * P * (1 - P) * 100) / 100. */
export function kalshiFeeDollars(contracts: number, price: number): number {
  const c = Math.max(0, Math.floor(contracts));
  const p = Math.min(0.99, Math.max(0.01, Math.round(price * 100) / 100));
  if (c === 0) return 0;
  return Math.ceil(0.07 * c * p * (1 - p) * 100 - 1e-9) / 100;
}

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

// ---------------------------------------------------------------------------
// Deterministic client-side quant engines (strictly from real loaded data)
// ---------------------------------------------------------------------------

export function computeCalibrationFromTape(markets: Market[], slots?: Slots | null): CalibrationData {
  const bucketsRaw = Array.from({ length: 10 }, (_, idx) => ({
    binIndex: idx,
    low: idx / 10,
    high: (idx + 1) / 10,
    sumPred: 0,
    wins: 0,
    count: 0,
  }));

  let brierSum = 0;
  let total = 0;

  const pushObs = (prob: number, won: boolean) => {
    const p = Math.min(0.99, Math.max(0.01, prob));
    const y = won ? 1 : 0;
    const bin = Math.min(9, Math.max(0, Math.floor(p * 10)));
    bucketsRaw[bin].sumPred += p;
    bucketsRaw[bin].wins += y;
    bucketsRaw[bin].count += 1;
    brierSum += (p - y) * (p - y);
    total += 1;
  };

  // Prefer settled markets that still carry an interior pre-close implied probability (0.01 < p < 0.99).
  for (const m of markets) {
    if (!m || typeof m !== "object") continue;
    const res = typeof m.result === "string" ? m.result.trim().toLowerCase() : "";
    if (res !== "yes" && res !== "no") continue;
    const prob = impliedYesProb(m);
    if (prob === null || !Number.isFinite(prob) || prob <= 0.01 || prob >= 0.99) continue;
    pushObs(prob, res === "yes");
  }

  // When /api/markets only carries post-settlement 0/1 last prices, use real pre-settlement trade prices from slots.trades.
  if (total === 0 && slots?.trades && Array.isArray(slots.trades)) {
    for (const tr of slots.trades) {
      if (!tr || !isNum(tr.price) || !isNum(tr.pnl_dollars)) continue;
      if (tr.price <= 0.01 || tr.price >= 0.99) continue;
      pushObs(tr.price, tr.pnl_dollars > 0);
    }
  }

  // Final fallback if only 0/1 settled markets exist and zero trades
  if (total === 0) {
    for (const m of markets) {
      if (!m || typeof m !== "object") continue;
      const res = typeof m.result === "string" ? m.result.trim().toLowerCase() : "";
      if (res !== "yes" && res !== "no") continue;
      const prob = impliedYesProb(m);
      if (prob === null || !Number.isFinite(prob)) continue;
      pushObs(prob, res === "yes");
    }
  }

  let eceSum = 0;
  const buckets: CalibrationBucket[] = bucketsRaw.map((b) => {
    if (b.count === 0) {
      return {
        binIndex: b.binIndex,
        low: b.low,
        high: b.high,
        meanPredicted: (b.low + b.high) / 2,
        realizedRate: 0,
        wilsonLow: 0,
        wilsonHigh: 0,
        count: 0,
      };
    }
    const meanPred = b.sumPred / b.count;
    const realized = b.wins / b.count;
    const [wLo, wHi] = wilsonInterval(b.wins, b.count);
    eceSum += (b.count / Math.max(1, total)) * Math.abs(realized - meanPred);
    return {
      binIndex: b.binIndex,
      low: b.low,
      high: b.high,
      meanPredicted: meanPred,
      realizedRate: realized,
      wilsonLow: wLo,
      wilsonHigh: wHi,
      count: b.count,
    };
  });

  return {
    brierScore: total > 0 ? brierSum / total : null,
    ece: total > 0 ? eceSum : null,
    totalWindows: total,
    buckets,
    source: "client",
  };
}

export function computeMonteCarloFromTrades(
  trades: Trade[],
  startEquity: number,
  paths = 1000,
  seed = 42,
): MonteCarloData {
  const t0 = performance.now();
  const s0 = isNum(startEquity) && startEquity > 0 ? startEquity : 100;
  const valid = trades.filter((t) => t && isNum(t.pnl_dollars));
  const nPaths = Math.min(5000, Math.max(100, Math.round(paths)));
  const steps = Math.min(120, valid.length);

  if (steps === 0) {
    return {
      paths: nPaths,
      seed,
      startEquity: s0,
      steps: 0,
      percentiles: { p5: [s0], p25: [s0], p50: [s0], p75: [s0], p95: [s0] },
      terminal: { p5: s0, p25: s0, p50: s0, p75: s0, p95: s0 },
      ddProb10: 0,
      ddProb25: 0,
      elapsedMs: Math.round(performance.now() - t0),
      source: "client",
    };
  }

  // Compute proportional step returns relative to running paper pot so compounding scales cleanly.
  let running = s0;
  const returns: number[] = [];
  for (const tr of valid) {
    const base = Math.max(10, running);
    const r = Math.max(-0.5, Math.min(1.5, tr.pnl_dollars / base));
    returns.push(r);
    running = Math.max(1, running + tr.pnl_dollars);
  }

  const rng = mulberry32(seed);
  // Store matrix [step][path] for quantile extraction
  const stepBuckets: number[][] = Array.from({ length: steps + 1 }, () => new Array<number>(nPaths));
  for (let p = 0; p < nPaths; p++) stepBuckets[0][p] = s0;

  let dd10Hits = 0;
  let dd25Hits = 0;

  for (let p = 0; p < nPaths; p++) {
    let eq = s0;
    let peak = s0;
    let maxDd = 0;
    for (let s = 1; s <= steps; s++) {
      const idx = Math.floor(rng() * returns.length);
      eq = Math.max(0.01, eq * (1 + returns[idx]));
      if (eq > peak) peak = eq;
      const dd = peak > 0 ? (peak - eq) / peak : 0;
      if (dd > maxDd) maxDd = dd;
      stepBuckets[s][p] = eq;
    }
    if (maxDd >= 0.1) dd10Hits++;
    if (maxDd >= 0.25) dd25Hits++;
  }

  const p5: number[] = [];
  const p25: number[] = [];
  const p50: number[] = [];
  const p75: number[] = [];
  const p95: number[] = [];

  for (let s = 0; s <= steps; s++) {
    const arr = stepBuckets[s].sort((a, b) => a - b);
    p5.push(Number(quantile(arr, 0.05).toFixed(2)));
    p25.push(Number(quantile(arr, 0.25).toFixed(2)));
    p50.push(Number(quantile(arr, 0.5).toFixed(2)));
    p75.push(Number(quantile(arr, 0.75).toFixed(2)));
    p95.push(Number(quantile(arr, 0.95).toFixed(2)));
  }

  return {
    paths: nPaths,
    seed,
    startEquity: s0,
    steps,
    percentiles: { p5, p25, p50, p75, p95 },
    terminal: {
      p5: p5[steps],
      p25: p25[steps],
      p50: p50[steps],
      p75: p75[steps],
      p95: p95[steps],
    },
    ddProb10: dd10Hits / nPaths,
    ddProb25: dd25Hits / nPaths,
    elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
    source: "client",
  };
}

export function computeSplitMetrics(trades: Trade[], startEquity: number): SplitMetrics {
  const s0 = isNum(startEquity) && startEquity > 0 ? startEquity : 100;
  const sorted = [...trades].sort((a, b) => a.close_epoch - b.close_epoch);
  if (sorted.length === 0) {
    return {
      windows: 0,
      netPnlDollars: 0,
      sharpe: null,
      sortino: null,
      profitFactor: null,
      maxDrawdownPct: 0,
      feeDragPct: 0,
      maxWinStreak: 0,
      maxLossStreak: 0,
    };
  }

  let eq = s0;
  let peak = s0;
  let maxDd = 0;
  let grossWin = 0;
  let grossLoss = 0;
  let totalFees = 0;
  let netPnl = 0;
  let curW = 0;
  let curL = 0;
  let maxW = 0;
  let maxL = 0;
  const rets: number[] = [];

  for (const t of sorted) {
    const pnl = isNum(t.pnl_dollars) ? t.pnl_dollars : 0;
    const base = Math.max(10, eq);
    rets.push(pnl / base);
    eq += pnl;
    netPnl += pnl;
    if (eq > peak) peak = eq;
    const dd = peak > 0 ? ((peak - eq) / peak) * 100 : 0;
    if (dd > maxDd) maxDd = dd;

    if (pnl > 0) {
      grossWin += pnl;
      curW += 1;
      curL = 0;
      if (curW > maxW) maxW = curW;
    } else if (pnl < 0) {
      grossLoss += Math.abs(pnl);
      curL += 1;
      curW = 0;
      if (curL > maxL) maxL = curL;
    }
    totalFees += kalshiFeeDollars(t.contracts || 1, t.price || 0.5);
  }

  const n = rets.length;
  const mean = rets.reduce((a, b) => a + b, 0) / n;
  const variance = n > 1 ? rets.reduce((a, r) => a + (r - mean) ** 2, 0) / (n - 1) : 0;
  const std = Math.sqrt(variance);
  const downsideVals = rets.filter((r) => r < 0);
  const downDev =
    downsideVals.length > 0
      ? Math.sqrt(downsideVals.reduce((a, r) => a + r * r, 0) / n)
      : 0;

  // Per-15m-window scaling (~96 windows/day -> sqrt(n) or annualized sqrt(96) daily Sharpe)
  const scale = Math.sqrt(Math.min(96, Math.max(1, n)));
  const sharpe = std > 1e-9 ? (mean / std) * scale : null;
  const sortino = downDev > 1e-9 ? (mean / downDev) * scale : null;
  const profitFactor = grossLoss > 1e-9 ? grossWin / grossLoss : grossWin > 0 ? 9.99 : null;
  const grossEdge = grossWin + totalFees;
  const feeDragPct = grossEdge > 1e-9 ? Math.min(100, (totalFees / grossEdge) * 100) : 0;

  return {
    windows: n,
    netPnlDollars: Number(netPnl.toFixed(2)),
    sharpe: sharpe !== null ? Number(sharpe.toFixed(2)) : null,
    sortino: sortino !== null ? Number(sortino.toFixed(2)) : null,
    profitFactor: profitFactor !== null ? Number(profitFactor.toFixed(2)) : null,
    maxDrawdownPct: Number(maxDd.toFixed(2)),
    feeDragPct: Number(feeDragPct.toFixed(1)),
    maxWinStreak: maxW,
    maxLossStreak: maxL,
  };
}

export function computeWalkForwardFromTrades(
  trades: Trade[],
  startEquity: number,
  inSampleRatio = 0.6,
): WalkForwardData {
  const valid = trades
    .filter((t) => t && isNum(t.close_epoch) && isNum(t.pnl_dollars))
    .sort((a, b) => a.close_epoch - b.close_epoch);

  const splitSlice = (list: Trade[]): [Trade[], Trade[]] => {
    if (list.length <= 1) return [list, []];
    const cut = Math.max(1, Math.min(list.length - 1, Math.floor(list.length * inSampleRatio)));
    return [list.slice(0, cut), list.slice(cut)];
  };

  const rows: WalkForwardRow[] = [];
  if (valid.length > 0) {
    const [pIs, pOos] = splitSlice(valid);
    rows.push({
      strategy: "Portfolio (All Slots)",
      isPortfolio: true,
      inSample: computeSplitMetrics(pIs, startEquity),
      outOfSample: computeSplitMetrics(pOos, startEquity),
    });

    const byStrat = new Map<string, Trade[]>();
    for (const t of valid) {
      const k = t.strategy || "unnamed";
      const arr = byStrat.get(k);
      if (arr) arr.push(t);
      else byStrat.set(k, [t]);
    }
    for (const [name, list] of [...byStrat.entries()].sort((a, b) => b[1].length - a[1].length)) {
      const [sIs, sOos] = splitSlice(list);
      rows.push({
        strategy: name,
        isPortfolio: false,
        inSample: computeSplitMetrics(sIs, startEquity / Math.max(1, byStrat.size)),
        outOfSample: computeSplitMetrics(sOos, startEquity / Math.max(1, byStrat.size)),
      });
    }
  }

  return {
    inSampleRatio,
    rows,
    source: "client",
  };
}

// ---------------------------------------------------------------------------
// Safe recursive-descent Rune Sandbox Validator & Paper Tape Backtester
// (Zero eval / zero Function constructor / strictly paper-only)
// ---------------------------------------------------------------------------

type Tok =
  | { k: "num"; v: number }
  | { k: "id"; v: string }
  | { k: "op"; v: string }
  | { k: "lp" | "rp" | "eof" };

function tokenizeExpr(expr: string): Tok[] {
  const toks: Tok[] = [];
  let i = 0;
  while (i < expr.length) {
    const ch = expr[i];
    if (/\s/.test(ch)) {
      i++;
      continue;
    }
    if (ch === "(") {
      toks.push({ k: "lp" });
      i++;
      continue;
    }
    if (ch === ")") {
      toks.push({ k: "rp" });
      i++;
      continue;
    }
    const two = expr.slice(i, i + 2);
    if (["&&", "||", "<=", ">=", "==", "!="].includes(two)) {
      toks.push({ k: "op", v: two });
      i += 2;
      continue;
    }
    if (["<", ">", "+", "-", "*", "/", "!"].includes(ch)) {
      toks.push({ k: "op", v: ch });
      i++;
      continue;
    }
    if (/[0-9.]/.test(ch)) {
      let j = i;
      while (j < expr.length && /[0-9._]/.test(expr[j])) j++;
      const raw = expr.slice(i, j).replace(/_/g, "");
      const num = Number(raw);
      if (!Number.isFinite(num)) throw new Error(`Invalid numeric literal '${raw}'`);
      toks.push({ k: "num", v: num });
      i = j;
      continue;
    }
    if (/[A-Za-z_]/.test(ch)) {
      let j = i;
      while (j < expr.length && /[A-Za-z0-9_]/.test(expr[j])) j++;
      toks.push({ k: "id", v: expr.slice(i, j) });
      i = j;
      continue;
    }
    throw new Error(`Unexpected token '${ch}' in allow_entry expression`);
  }
  toks.push({ k: "eof" });
  return toks;
}

function evalAllowEntryExpr(expr: string, env: Record<string, number>): boolean {
  const toks = tokenizeExpr(expr);
  let pos = 0;
  const peek = () => toks[pos] || { k: "eof" as const };
  const next = () => toks[pos++] || { k: "eof" as const };

  const parseOr = (): number => {
    let left = parseAnd();
    while (peek().k === "op" && (peek() as { k: "op"; v: string }).v === "||") {
      next();
      const right = parseAnd();
      left = left || right ? 1 : 0;
    }
    return left;
  };
  const parseAnd = (): number => {
    let left = parseCmp();
    while (peek().k === "op" && (peek() as { k: "op"; v: string }).v === "&&") {
      next();
      const right = parseCmp();
      left = left && right ? 1 : 0;
    }
    return left;
  };
  const parseCmp = (): number => {
    let left = parseAdd();
    while (
      peek().k === "op" &&
      ["<=", ">=", "<", ">", "==", "!="].includes((peek() as { k: "op"; v: string }).v)
    ) {
      const op = (next() as { k: "op"; v: string }).v;
      const right = parseAdd();
      if (op === "<=") left = left <= right ? 1 : 0;
      else if (op === ">=") left = left >= right ? 1 : 0;
      else if (op === "<") left = left < right ? 1 : 0;
      else if (op === ">") left = left > right ? 1 : 0;
      else if (op === "==") left = Math.abs(left - right) < 1e-9 ? 1 : 0;
      else left = Math.abs(left - right) >= 1e-9 ? 1 : 0;
    }
    return left;
  };
  const parseAdd = (): number => {
    let left = parseMul();
    while (peek().k === "op" && ["+", "-"].includes((peek() as { k: "op"; v: string }).v)) {
      const op = (next() as { k: "op"; v: string }).v;
      const right = parseMul();
      left = op === "+" ? left + right : left - right;
    }
    return left;
  };
  const parseMul = (): number => {
    let left = parseUnary();
    while (peek().k === "op" && ["*", "/"].includes((peek() as { k: "op"; v: string }).v)) {
      const op = (next() as { k: "op"; v: string }).v;
      const right = parseUnary();
      left = op === "*" ? left * right : right === 0 ? 0 : left / right;
    }
    return left;
  };
  const parseUnary = (): number => {
    const t = peek();
    if (t.k === "op" && t.v === "!") {
      next();
      return parseUnary() ? 0 : 1;
    }
    if (t.k === "op" && t.v === "-") {
      next();
      return -parseUnary();
    }
    if (t.k === "lp") {
      next();
      const v = parseOr();
      if (next().k !== "rp") throw new Error("Unclosed '(' in allow_entry");
      return v;
    }
    if (t.k === "num") {
      next();
      return t.v;
    }
    if (t.k === "id") {
      next();
      if (t.v === "true") return 1;
      if (t.v === "false") return 0;
      if (t.v in env) return env[t.v];
      throw new Error(`Unknown identifier '${t.v}' in allow_entry`);
    }
    throw new Error("Unexpected end of expression in allow_entry");
  };

  const val = parseOr();
  if (peek().k !== "eof") throw new Error("Trailing tokens in allow_entry expression");
  return val !== 0;
}

function stripRuneComments(src: string): string {
  return src.replace(/\/\/[^\n]*/g, "");
}

function extractFnBody(src: string, fnName: string): { params: string[]; body: string } | null {
  const re = new RegExp(`pub\\s+fn\\s+${fnName}\\s*\\(([^)]*)\\)\\s*\\{`);
  const m = re.exec(src);
  if (!m) return null;
  let depth = 1;
  let i = m.index + m[0].length;
  const start = i;
  while (i < src.length && depth > 0) {
    if (src[i] === "{") depth++;
    else if (src[i] === "}") depth--;
    i++;
  }
  if (depth !== 0) return null;
  const params = m[1]
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  return { params, body: src.slice(start, i - 1).trim() };
}

export function validateRuneSourceClient(source: string): RuneValidationResult {
  const t0 = performance.now();
  const diagnostics: RuneDiagnostic[] = [];
  const clean = stripRuneComments(source);

  // Paper-only safety checks (reject IO/network/process primitives in strategy code).
  const forbidden: [RegExp, string][] = [
    [/\bCommand::new\b|\bstd::process\b/i, "Process execution is forbidden in paper Rune strategies."],
    [/\bTcpStream\b|\bUdpSocket\b|\bstd::net\b|\bhttp::\b/i, "Network sockets are forbidden in paper Rune strategies."],
    [/\bstd::fs\b|\bFile::open\b/i, "Filesystem IO is forbidden in paper Rune strategies."],
  ];
  for (const [re, msg] of forbidden) {
    if (re.test(source)) diagnostics.push({ level: "error", message: msg });
  }

  // Brace/paren balance check
  let braces = 0;
  let parens = 0;
  const lines = source.split("\n");
  for (let ln = 0; ln < lines.length; ln++) {
    const line = lines[ln].replace(/\/\/.*$/, "");
    for (const ch of line) {
      if (ch === "{") braces++;
      else if (ch === "}") braces--;
      else if (ch === "(") parens++;
      else if (ch === ")") parens--;
      if (braces < 0 || parens < 0) {
        diagnostics.push({ level: "error", line: ln + 1, message: "Unmatched closing delimiter." });
        braces = Math.max(0, braces);
        parens = Math.max(0, parens);
      }
    }
  }
  if (braces !== 0) diagnostics.push({ level: "error", message: "Unclosed '{' brace in Rune script." });
  if (parens !== 0) diagnostics.push({ level: "error", message: "Unclosed '(' parenthesis in Rune script." });

  const nameFn = extractFnBody(clean, "name");
  const sideFn = extractFnBody(clean, "side");
  const allowFn = extractFnBody(clean, "allow_entry");

  let name = "custom-strategy";
  let side: "yes" | "no" | "unknown" = "unknown";

  if (!nameFn) {
    diagnostics.push({ level: "error", message: "Missing required `pub fn name()` returning a strategy name string." });
  } else {
    const nm = nameFn.body.match(/^"([^"]+)"$/);
    if (!nm) diagnostics.push({ level: "error", message: "`pub fn name()` must return a double-quoted strategy name." });
    else name = nm[1].trim();
  }

  if (!sideFn) {
    diagnostics.push({ level: "error", message: "Missing required `pub fn side()` returning \"yes\" or \"no\"." });
  } else {
    const sm = sideFn.body.match(/^"(yes|no)"$/i);
    if (!sm) diagnostics.push({ level: "error", message: "`pub fn side()` must return \"yes\" or \"no\"." });
    else side = sm[1].toLowerCase() as "yes" | "no";
  }

  if (!allowFn) {
    diagnostics.push({
      level: "error",
      message: "Missing required `pub fn allow_entry(yes_ask, no_ask, secs_to_close)` entry rule.",
    });
  } else {
    const finalExpr = allowFn.body
      .split(";")
      .map((s) => s.trim())
      .filter(Boolean);
    const tail = finalExpr[finalExpr.length - 1] || "";
    if (!tail) {
      diagnostics.push({ level: "error", message: "`allow_entry` body cannot be empty." });
    } else {
      try {
        const testEnv: Record<string, number> = {
          yes_ask: 0.72,
          no_ask: 0.30,
          yes_bid: 0.70,
          no_bid: 0.28,
          spread: 0.02,
          secs_to_close: 180,
        };
        for (const p of allowFn.params) {
          if (!(p in testEnv)) testEnv[p] = 0.5;
        }
        // Resolve simple `let x = expr;` bindings before tail expression
        for (let k = 0; k < finalExpr.length - 1; k++) {
          const lm = finalExpr[k].match(/^let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$/);
          if (lm) {
            testEnv[lm[1]] = evalAllowEntryExpr(lm[2], testEnv) ? 1 : 0;
          }
        }
        evalAllowEntryExpr(tail, testEnv);
      } catch (e) {
        diagnostics.push({
          level: "error",
          message: e instanceof Error ? e.message : "Could not parse `allow_entry` boolean expression.",
        });
      }
    }
  }

  const valid = diagnostics.every((d) => d.level !== "error");
  if (valid) {
    diagnostics.push({
      level: "info",
      message: `Compiled paper Rune rule '${name}' (${side.toUpperCase()} side) — whole-cent bounds enforced.`,
    });
  }

  return {
    valid,
    name,
    side,
    diagnostics,
    elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
    source: "client",
  };
}

export function backtestRuneSourceClient(
  source: string,
  markets: Market[],
  slots?: Slots | null,
  startEquity = 100,
): BacktestResult {
  const t0 = performance.now();
  const check = validateRuneSourceClient(source);
  const emptySummary: BacktestSummary = {
    netPnlDollars: 0,
    sharpe: null,
    sortino: null,
    maxDrawdownPct: 0,
    brierScore: null,
    ece: null,
    winRatePct: 0,
    windowsTraded: 0,
    meanCentsPerContract: 0,
    feeDragPct: 0,
  };

  if (!check.valid || check.side === "unknown") {
    return {
      valid: false,
      strategy: check.name,
      side: check.side,
      diagnostics: check.diagnostics,
      summary: emptySummary,
      equityCurve: [],
      trades: [],
      elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
      source: "client",
    };
  }

  const clean = stripRuneComments(source);
  const allowFn = extractFnBody(clean, "allow_entry")!;
  const stmts = allowFn.body
    .split(";")
    .map((s) => s.trim())
    .filter(Boolean);
  const tailExpr = stmts[stmts.length - 1];

  // Build scored window candidates from settled markets with interior quotes, or from real recorded trade quotes.
  type CandidateWindow = {
    closeEpoch: number;
    ticker: string;
    yesAsk: number;
    noAsk: number;
    yesBid: number;
    noBid: number;
    secsToClose: number;
    result: "yes" | "no";
  };

  const candidates: CandidateWindow[] = [];
  for (const m of markets) {
    if (!m || typeof m !== "object") continue;
    const r = typeof m.result === "string" ? m.result.trim().toLowerCase() : "";
    if (r !== "yes" && r !== "no") continue;
    const yesAskRaw = isNum(m.yes_ask) && m.yes_ask > 0.01 && m.yes_ask < 0.99 ? m.yes_ask : null;
    if (yesAskRaw === null) continue;
    const closeMs = typeof m.close_time === "string" ? Date.parse(m.close_time) : NaN;
    const closeEpoch = Number.isFinite(closeMs) ? Math.floor(closeMs / 1000) : (m.fetched_at ?? Math.floor(Date.now() / 1000));
    const fetched = isNum(m.fetched_at) ? m.fetched_at : closeEpoch - 180;
    const secsToClose = Math.max(61, Math.min(900, closeEpoch > fetched ? closeEpoch - fetched : 180));
    const yesAsk = Math.min(0.99, Math.max(0.01, Math.round(yesAskRaw * 100) / 100));
    const noAsk = isNum(m.no_ask) && m.no_ask > 0.01 && m.no_ask < 0.99 ? Math.round(m.no_ask * 100) / 100 : Math.min(0.99, Math.max(0.01, Math.round((1 - yesAsk + 0.02) * 100) / 100));
    const yesBid = isNum(m.yes_bid) ? m.yes_bid : Math.max(0.01, yesAsk - 0.02);
    const noBid = isNum(m.no_bid) ? m.no_bid : Math.max(0.01, noAsk - 0.02);
    candidates.push({
      closeEpoch,
      ticker: m.ticker || "window",
      yesAsk,
      noAsk,
      yesBid,
      noBid,
      secsToClose,
      result: r,
    });
  }

  // Fallback when /api/markets latest rows are post-settlement (null bids/asks): reconstruct unique epochs from real recorded trades.
  if (candidates.length === 0 && slots?.trades && Array.isArray(slots.trades)) {
    const byEpoch = new Map<number, Trade>();
    for (const tr of slots.trades) {
      if (!tr || !isNum(tr.close_epoch) || !isNum(tr.price)) continue;
      if (tr.price <= 0.01 || tr.price >= 0.99) continue;
      if (!byEpoch.has(tr.close_epoch)) byEpoch.set(tr.close_epoch, tr);
    }
    for (const tr of [...byEpoch.values()].sort((a, b) => a.close_epoch - b.close_epoch)) {
      const p = Math.min(0.99, Math.max(0.01, Math.round(tr.price * 100) / 100));
      const noAsk = Math.min(0.99, Math.max(0.01, Math.round((1 - p + 0.02) * 100) / 100));
      const result: "yes" | "no" = tr.pnl_dollars > 0 ? "yes" : "no";
      candidates.push({
        closeEpoch: tr.close_epoch,
        ticker: tr.ticker || `15M-${tr.close_epoch}`,
        yesAsk: p,
        noAsk,
        yesBid: Math.max(0.01, p - 0.02),
        noBid: Math.max(0.01, noAsk - 0.02),
        secsToClose: 180,
        result,
      });
    }
  }

  candidates.sort((a, b) => a.closeEpoch - b.closeEpoch);

  let equity = isNum(startEquity) && startEquity > 0 ? startEquity : 100;
  let cumPnl = 0;
  let wins = 0;
  let totalContracts = 0;
  let brierSum = 0;
  const trades: BacktestTrade[] = [];
  const rawTrades: Trade[] = [];
  const curve: [number, number][] = [];

  for (const w of candidates) {
    const spread = Math.max(0, Math.round((w.yesAsk - w.yesBid) * 100) / 100);
    const env: Record<string, number> = {
      yes_ask: w.yesAsk,
      no_ask: w.noAsk,
      yes_bid: w.yesBid,
      no_bid: w.noBid,
      spread,
      secs_to_close: w.secsToClose,
    };

    let entered = false;
    try {
      entered = evalAllowEntryExpr(tailExpr, env);
    } catch {
      entered = false;
    }
    if (!entered) continue;

    const entryPrice = check.side === "yes" ? w.yesAsk : w.noAsk;
    const slotBudget = Math.max(1, equity * 0.2);
    const contracts = Math.max(1, Math.floor(slotBudget / (entryPrice + kalshiFeeDollars(1, entryPrice))));
    const fee = kalshiFeeDollars(contracts, entryPrice);
    const won = w.result === check.side;
    const gross = won ? contracts * (1 - entryPrice) : -contracts * entryPrice;
    const pnl = Number((gross - fee).toFixed(2));

    if (curve.length === 0) curve.push([w.closeEpoch, 0]);
    cumPnl = Number((cumPnl + pnl).toFixed(2));
    equity = Math.max(1, equity + pnl);
    if (won) wins++;
    totalContracts += contracts;
    brierSum += (entryPrice - (won ? 1 : 0)) ** 2;

    curve.push([w.closeEpoch, cumPnl]);
    trades.push({
      closeEpoch: w.closeEpoch,
      ticker: w.ticker,
      side: check.side,
      price: entryPrice,
      contracts,
      pnlDollars: pnl,
      result: w.result.toUpperCase(),
    });
    rawTrades.push({
      close_epoch: w.closeEpoch,
      contracts,
      pnl_dollars: pnl,
      price: entryPrice,
      slot: 1,
      strategy: check.name,
      ticker: w.ticker,
    });
  }

  const metrics = computeSplitMetrics(rawTrades, startEquity);
  const traded = trades.length;
  const meanCents = totalContracts > 0 ? (cumPnl * 100) / totalContracts : 0;
  const brier = traded > 0 ? brierSum / traded : null;

  return {
    valid: true,
    strategy: check.name,
    side: check.side,
    diagnostics: check.diagnostics,
    summary: {
      netPnlDollars: cumPnl,
      sharpe: metrics.sharpe,
      sortino: metrics.sortino,
      maxDrawdownPct: metrics.maxDrawdownPct,
      brierScore: brier !== null ? Number(brier.toFixed(4)) : null,
      ece: null,
      winRatePct: traded > 0 ? Number(((wins / traded) * 100).toFixed(1)) : 0,
      windowsTraded: traded,
      meanCentsPerContract: Number(meanCents.toFixed(2)),
      feeDragPct: metrics.feeDragPct,
    },
    equityCurve: curve,
    trades,
    elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
    source: "client",
  };
}

// ---------------------------------------------------------------------------
// Flexible server-first API fetchers (v0.5.0 Rust engine with graceful fallback)
// ---------------------------------------------------------------------------

function pickNum(obj: Record<string, unknown>, keys: string[]): number | null {
  for (const k of keys) {
    const v = obj[k];
    if (isNum(v)) return v;
  }
  return null;
}

function normalizeCalibration(raw: unknown): CalibrationData | null {
  if (!raw || typeof raw !== "object") return null;
  const root = raw as Record<string, unknown>;
  // Support DeskCalibrationSummary { market_calibration, portfolio_calibration } OR direct CalibrationReport
  const pickReport = (): Record<string, unknown> => {
    const mc = root.market_calibration as Record<string, unknown> | undefined;
    const pc = root.portfolio_calibration as Record<string, unknown> | undefined;
    if (mc && typeof mc === "object" && Array.isArray(mc.buckets)) {
      const mCount = pickNum(mc, ["total_windows", "total_observations", "n", "count"]) ?? 0;
      if (mCount > 0 || !pc) return mc;
    }
    if (pc && typeof pc === "object" && Array.isArray(pc.buckets)) return pc;
    return root;
  };
  const o = pickReport();
  const rawBuckets = Array.isArray(o.buckets) ? o.buckets : Array.isArray(o.bins) ? o.bins : null;
  if (!rawBuckets) return null;

  const buckets: CalibrationBucket[] = rawBuckets.map((b, idx) => {
    const bo = (b && typeof b === "object" ? b : {}) as Record<string, unknown>;
    const low = pickNum(bo, ["low", "lower", "bin_low", "min_prob"]) ?? idx / 10;
    const high = pickNum(bo, ["high", "upper", "bin_high", "max_prob"]) ?? (idx + 1) / 10;
    const count = pickNum(bo, ["count", "total", "n", "samples"]) ?? 0;
    const meanPredicted =
      pickNum(bo, ["mean_predicted", "predicted_mean", "predicted", "avg_predicted"]) ?? (low + high) / 2;
    const realizedRate = pickNum(bo, ["realized_rate", "empirical_rate", "realized", "win_rate"]) ?? 0;
    const wilsonLow =
      pickNum(bo, ["wilson_lower_95", "wilson_low", "ci_low", "lower_95"]) ?? Math.max(0, realizedRate);
    const wilsonHigh =
      pickNum(bo, ["wilson_upper_95", "wilson_high", "ci_high", "upper_95"]) ?? Math.min(1, realizedRate);
    return {
      binIndex: pickNum(bo, ["bin_index", "index", "bucket"]) ?? idx,
      low,
      high,
      meanPredicted,
      realizedRate,
      wilsonLow,
      wilsonHigh,
      count,
    };
  });

  const totalWindows =
    pickNum(o, ["total_windows", "total_observations", "sample_count", "n", "count"]) ??
    buckets.reduce((a, b) => a + b.count, 0);

  return {
    brierScore: pickNum(o, ["brier_score", "brier"]),
    ece: pickNum(o, ["ece", "expected_calibration_error"]),
    totalWindows,
    buckets,
    source: "server",
  };
}

function normalizeMonteCarlo(raw: unknown, fallbackSeed: number, fallbackStart: number): MonteCarloData | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  let p5: number[] = [];
  let p25: number[] = [];
  let p50: number[] = [];
  let p75: number[] = [];
  let p95: number[] = [];

  if (o.percentiles && typeof o.percentiles === "object") {
    const pct = o.percentiles as Record<string, unknown>;
    if (Array.isArray(pct.p5)) p5 = pct.p5.filter(isNum);
    if (Array.isArray(pct.p25)) p25 = pct.p25.filter(isNum);
    if (Array.isArray(pct.p50)) p50 = pct.p50.filter(isNum);
    if (Array.isArray(pct.p75)) p75 = pct.p75.filter(isNum);
    if (Array.isArray(pct.p95)) p95 = pct.p95.filter(isNum);
  } else {
    const bandList = Array.isArray(o.equity_bands) ? o.equity_bands : Array.isArray(o.bands) ? o.bands : null;
    if (bandList) {
      for (const row of bandList) {
        if (!row || typeof row !== "object") continue;
        const r = row as Record<string, unknown>;
        if (isNum(r.p5) && isNum(r.p25) && isNum(r.p50) && isNum(r.p75) && isNum(r.p95)) {
          p5.push(r.p5);
          p25.push(r.p25);
          p50.push(r.p50);
          p75.push(r.p75);
          p95.push(r.p95);
        }
      }
    }
  }

  if (p50.length === 0) return null;
  const last = p50.length - 1;
  const term = (
    o.terminal_equity && typeof o.terminal_equity === "object"
      ? o.terminal_equity
      : o.terminal && typeof o.terminal === "object"
        ? o.terminal
        : {}
  ) as Record<string, unknown>;

  const normalizeProb = (pctVal: number | null, fracVal: number | null): number => {
    if (pctVal !== null) return pctVal > 1 ? pctVal / 100 : pctVal;
    if (fracVal !== null) return fracVal > 1 ? fracVal / 100 : fracVal;
    return 0;
  };

  return {
    paths: pickNum(o, ["paths", "num_paths"]) ?? 1000,
    seed: pickNum(o, ["seed"]) ?? fallbackSeed,
    startEquity: pickNum(o, ["start_equity", "paper_pot_start"]) ?? fallbackStart,
    steps: Math.max(0, p50.length - 1),
    percentiles: { p5, p25, p50, p75, p95 },
    terminal: {
      p5: pickNum(term, ["p5"]) ?? p5[last] ?? fallbackStart,
      p25: pickNum(term, ["p25"]) ?? p25[last] ?? fallbackStart,
      p50: pickNum(term, ["p50"]) ?? p50[last] ?? fallbackStart,
      p75: pickNum(term, ["p75"]) ?? p75[last] ?? fallbackStart,
      p95: pickNum(term, ["p95"]) ?? p95[last] ?? fallbackStart,
    },
    ddProb10: normalizeProb(
      pickNum(o, ["prob_dd_gte_10_pct"]),
      pickNum(o, ["dd_prob_10", "prob_dd_10", "p_dd_10"]),
    ),
    ddProb25: normalizeProb(
      pickNum(o, ["prob_dd_gte_25_pct"]),
      pickNum(o, ["dd_prob_25", "prob_dd_25", "p_dd_25"]),
    ),
    elapsedMs: pickNum(o, ["elapsed_ms"]) ?? 1,
    source: "server",
  };
}

function normalizeSplit(raw: unknown): SplitMetrics {
  const o = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const st = (o.streaks && typeof o.streaks === "object" ? o.streaks : {}) as Record<string, unknown>;
  return {
    windows: pickNum(o, ["windows", "windows_traded", "trades", "trade_count", "n"]) ?? 0,
    netPnlDollars: pickNum(o, ["net_pnl_dollars", "paper_pnl_dollars", "net_pnl"]) ?? 0,
    sharpe: pickNum(o, ["sharpe", "sharpe_ratio", "sharpe_annualized"]),
    sortino: pickNum(o, ["sortino", "sortino_ratio"]),
    profitFactor: pickNum(o, ["profit_factor"]),
    maxDrawdownPct: pickNum(o, ["max_drawdown_pct", "max_dd_pct"]) ?? 0,
    feeDragPct: pickNum(o, ["fee_drag_pct", "fee_share_of_gross_profit_pct"]) ?? 0,
    maxWinStreak: pickNum(st, ["max_win_streak"]) ?? pickNum(o, ["max_win_streak", "win_streak"]) ?? 0,
    maxLossStreak: pickNum(st, ["max_loss_streak"]) ?? pickNum(o, ["max_loss_streak", "loss_streak"]) ?? 0,
  };
}

function normalizeWalkForward(raw: unknown): WalkForwardData | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;

  // Support DeskWalkForwardSummary { portfolio, by_strategy } OR { rows / strategies }
  const rows: WalkForwardRow[] = [];
  if (o.portfolio && typeof o.portfolio === "object") {
    const p = o.portfolio as Record<string, unknown>;
    rows.push({
      strategy: "Portfolio (All Slots)",
      isPortfolio: true,
      inSample: normalizeSplit(p.in_sample ?? p.is),
      outOfSample: normalizeSplit(p.out_of_sample ?? p.oos),
    });
  }
  const list = Array.isArray(o.rows)
    ? o.rows
    : Array.isArray(o.by_strategy)
      ? o.by_strategy
      : Array.isArray(o.strategies)
        ? o.strategies
        : null;
  if (list) {
    for (const item of list) {
      if (!item || typeof item !== "object") continue;
      const r = item as Record<string, unknown>;
      const splitObj = (r.split && typeof r.split === "object" ? r.split : r) as Record<string, unknown>;
      const name = typeof r.strategy === "string" ? r.strategy : typeof r.name === "string" ? r.name : "unnamed";
      rows.push({
        strategy: name,
        isPortfolio: Boolean(r.is_portfolio || /portfolio/i.test(name)),
        inSample: normalizeSplit(splitObj.in_sample ?? splitObj.is),
        outOfSample: normalizeSplit(splitObj.out_of_sample ?? splitObj.oos),
      });
    }
  }
  if (rows.length === 0) return null;
  return {
    inSampleRatio: pickNum(o, ["in_sample_ratio", "split_ratio"]) ?? 0.6,
    rows,
    source: "server",
  };
}

export async function fetchCalibration(
  dataset: Dataset,
  home: Home | null,
  markets: Market[],
  slots?: Slots | null,
): Promise<CalibrationData> {
  if (supportsV050(home, "calibration")) {
    try {
      const qs = dataset === "fixture" ? "?dataset=fixture" : "";
      const r = await fetch(`/api/analytics/calibration${qs}`);
      if (r.ok) {
        const parsed = normalizeCalibration(await r.json());
        if (parsed) return parsed;
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return computeCalibrationFromTape(markets, slots);
}

export async function fetchWalkForward(
  dataset: Dataset,
  home: Home | null,
  slots: Slots | null,
): Promise<WalkForwardData> {
  if (supportsV050(home, "walkforward")) {
    try {
      const qs = dataset === "fixture" ? "?dataset=fixture" : "";
      const r = await fetch(`/api/analytics/walkforward${qs}`);
      if (r.ok) {
        const parsed = normalizeWalkForward(await r.json());
        if (parsed) return parsed;
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return computeWalkForwardFromTrades(slots?.trades || [], slots?.paper_pot_start ?? 100);
}

export async function runMonteCarlo(
  dataset: Dataset,
  home: Home | null,
  slots: Slots | null,
  seed: number,
  paths = 1000,
): Promise<MonteCarloData> {
  const startEq = slots?.paper_pot_start ?? 100;
  if (supportsV050(home, "montecarlo")) {
    try {
      const r = await fetch("/api/analytics/montecarlo", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ paths, seed, dataset }),
      });
      if (r.ok) {
        const parsed = normalizeMonteCarlo(await r.json(), seed, startEq);
        if (parsed) return parsed;
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return computeMonteCarloFromTrades(slots?.trades || [], startEq, paths, seed);
}

export async function validateRuneStrategy(
  source: string,
  home: Home | null,
): Promise<RuneValidationResult> {
  if (supportsV050(home, "sandbox")) {
    try {
      const r = await fetch("/api/strategy/validate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ source }),
      });
      if (r.ok || r.status === 400 || r.status === 422) {
        const raw = (await r.json().catch(() => null)) as Record<string, unknown> | null;
        if (raw && typeof raw === "object") {
          const diags: RuneDiagnostic[] = [];
          if (Array.isArray(raw.diagnostics)) {
            for (const d of raw.diagnostics) {
              if (typeof d === "string") diags.push({ level: "error", message: d });
              else if (d && typeof d === "object") {
                const dobj = d as Record<string, unknown>;
                diags.push({
                  level: dobj.level === "warn" || dobj.level === "info" ? dobj.level : "error",
                  line: isNum(dobj.line) ? dobj.line : undefined,
                  col: isNum(dobj.col) ? dobj.col : undefined,
                  message: String(dobj.message || "Diagnostic"),
                });
              }
            }
          }
          if (Array.isArray(raw.errors)) {
            for (const e of raw.errors) diags.push({ level: "error", message: String(e) });
          }
          if (typeof raw.error === "string" && raw.error) {
            diags.push({ level: "error", message: raw.error });
          }
          const valid = Boolean(raw.valid ?? raw.ok ?? (r.ok && diags.every((d) => d.level !== "error")));
          const sideStr = String(raw.side || "").toLowerCase();
          return {
            valid,
            name: typeof raw.name === "string" && raw.name ? raw.name : typeof raw.strategy === "string" ? raw.strategy : "custom-strategy",
            side: sideStr === "yes" || sideStr === "no" ? sideStr : "unknown",
            diagnostics:
              diags.length > 0
                ? diags
                : [{ level: "info", message: "Compiled cleanly in Rune sandbox VM." }],
            elapsedMs: pickNum(raw, ["elapsed_ms"]) ?? 1,
            source: "server",
          };
        }
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return validateRuneSourceClient(source);
}

export async function runRuneBacktest(
  source: string,
  dataset: Dataset,
  home: Home | null,
  markets: Market[],
  slots?: Slots | null,
  startEquity = 100,
): Promise<BacktestResult> {
  if (supportsV050(home, "sandbox")) {
    try {
      const r = await fetch("/api/backtest", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ source, dataset }),
      });
      if (r.ok || r.status === 400 || r.status === 422) {
        const raw = (await r.json().catch(() => null)) as Record<string, unknown> | null;
        if (raw && typeof raw === "object") {
          const valObj = (raw.validation && typeof raw.validation === "object" ? raw.validation : raw) as Record<string, unknown>;
          const wfFull = (
            raw.walk_forward &&
            typeof raw.walk_forward === "object" &&
            (raw.walk_forward as Record<string, unknown>).full_sample &&
            typeof (raw.walk_forward as Record<string, unknown>).full_sample === "object"
              ? (raw.walk_forward as Record<string, unknown>).full_sample
              : null
          ) as Record<string, unknown> | null;
          const scoreObj = (raw.score && typeof raw.score === "object" ? raw.score : null) as Record<string, unknown> | null;
          const calObj = (raw.calibration && typeof raw.calibration === "object" ? raw.calibration : null) as Record<string, unknown> | null;
          const sum = (raw.summary && typeof raw.summary === "object" ? raw.summary : wfFull ?? raw) as Record<string, unknown>;
          const diags: RuneDiagnostic[] = [];
          const rawDiags = Array.isArray(valObj.diagnostics) ? valObj.diagnostics : Array.isArray(raw.diagnostics) ? raw.diagnostics : [];
          for (const d of rawDiags) {
            if (typeof d === "string") diags.push({ level: "error", message: d });
            else if (d && typeof d === "object") {
              const dobj = d as Record<string, unknown>;
              diags.push({
                level: dobj.level === "warn" || dobj.level === "info" ? dobj.level : "error",
                line: isNum(dobj.line) ? dobj.line : undefined,
                message: String(dobj.message || "Diagnostic"),
              });
            }
          }
          if (typeof raw.error === "string" && raw.error) diags.push({ level: "error", message: raw.error });
          const rawCurve = Array.isArray(raw.equity_curve)
            ? raw.equity_curve
            : Array.isArray(raw.points)
              ? raw.points
              : [];
          const equityCurve: [number, number][] = [];
          for (const pt of rawCurve) {
            if (Array.isArray(pt) && isNum(pt[0]) && isNum(pt[1])) equityCurve.push([pt[0], pt[1]]);
            else if (pt && typeof pt === "object") {
              const po = pt as Record<string, unknown>;
              const t = pickNum(po, ["epoch", "close_epoch", "t"]);
              const v = pickNum(po, ["equity", "pnl", "value"]);
              if (t !== null && v !== null) equityCurve.push([t, v]);
            }
          }
          const rawTrades = Array.isArray(raw.trades) ? raw.trades : [];
          const trades: BacktestTrade[] = rawTrades
            .filter((t): t is Record<string, unknown> => !!t && typeof t === "object")
            .map((t) => ({
              closeEpoch: pickNum(t, ["close_epoch", "epoch", "ts"]) ?? 0,
              ticker: String(t.ticker || "window"),
              side: String(t.side || valObj.side || raw.side || "yes").toLowerCase() === "no" ? "no" : "yes",
              price: pickNum(t, ["price", "entry_price"]) ?? 0.5,
              contracts: pickNum(t, ["contracts", "qty"]) ?? 1,
              pnlDollars: pickNum(t, ["pnl_dollars", "pnl"]) ?? 0,
              result: String(t.result || "").toUpperCase(),
            }));
          const valid = Boolean(valObj.valid ?? valObj.ok ?? raw.valid ?? raw.ok ?? (r.ok && diags.every((d) => d.level !== "error")));
          const sideStr = String(valObj.side || scoreObj?.side || raw.side || "").toLowerCase();
          const winsCount = trades.filter((t) => t.pnlDollars > 0).length;
          const computedWinPct = trades.length > 0 ? Number(((winsCount / trades.length) * 100).toFixed(1)) : 0;
          return {
            valid,
            strategy: String(scoreObj?.strategy || valObj.name || raw.strategy || raw.name || "custom-strategy"),
            side: sideStr === "yes" || sideStr === "no" ? sideStr : "unknown",
            diagnostics:
              diags.length > 0
                ? diags
                : [{ level: "info", message: `Backtested ${trades.length} settled windows in server Rune VM.` }],
            summary: {
              netPnlDollars: pickNum(sum, ["net_pnl_dollars", "paper_pnl_dollars", "net_pnl"]) ?? 0,
              sharpe: pickNum(sum, ["sharpe", "sharpe_ratio"]),
              sortino: pickNum(sum, ["sortino", "sortino_ratio"]),
              maxDrawdownPct: pickNum(sum, ["max_drawdown_pct", "max_dd_pct"]) ?? 0,
              brierScore: (calObj ? pickNum(calObj, ["brier_score", "brier"]) : null) ?? pickNum(sum, ["brier_score", "brier"]),
              ece: (calObj ? pickNum(calObj, ["ece"]) : null) ?? pickNum(sum, ["ece"]),
              winRatePct: pickNum(sum, ["win_rate_pct", "win_rate"]) ?? computedWinPct,
              windowsTraded:
                (scoreObj ? pickNum(scoreObj, ["windows_traded"]) : null) ??
                pickNum(sum, ["windows_traded", "trades_count", "windows"]) ??
                trades.length,
              meanCentsPerContract:
                (scoreObj ? pickNum(scoreObj, ["mean_cents_per_contract"]) : null) ??
                pickNum(sum, ["mean_cents_per_contract", "expectancy_cents_per_contract", "mean_cents"]) ??
                0,
              feeDragPct: pickNum(sum, ["fee_drag_pct"]) ?? 0,
            },
            equityCurve,
            trades,
            elapsedMs: pickNum(raw, ["elapsed_ms"]) ?? 1,
            source: "server",
          };
        }
      }
    } catch {
      /* fall back cleanly */
    }
  }
  return backtestRuneSourceClient(source, markets, slots, startEquity);
}
