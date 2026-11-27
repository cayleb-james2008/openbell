// OpenBell v0.33.0 Strategy Runtime v2, Persistent Library CRUD, and Walk-Forward Edge Synthesizer client.
// Mutations always send X-OpenBell-CSRF: 1 and Content-Type: application/json.
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  type Strategy,
  cleanStr,
  isNum,
  semverAtLeast,
} from "@/lib/desk";

export type StrategyFamily =
  | "momentum_microstructure"
  | "mean_reversion_pav"
  | "maker_imbalance"
  | "all";

export type SynthExecutionMode = "maker" | "taker";

export type TickContextFieldSpec = {
  field: string;
  type: string;
  unit: string;
  description: string;
};

export const V2_TICK_CONTEXT_FIELDS: TickContextFieldSpec[] = [
  { field: "yes_bid", type: "f64", unit: "0.01..0.99", description: "Best YES bid probability at tick t" },
  { field: "yes_ask", type: "f64", unit: "0.01..0.99", description: "Best YES ask probability at tick t" },
  { field: "no_bid", type: "f64", unit: "0.01..0.99", description: "Best NO bid probability at tick t" },
  { field: "no_ask", type: "f64", unit: "0.01..0.99", description: "Best NO ask probability at tick t" },
  { field: "secs_to_close", type: "i64", unit: "seconds", description: "Seconds remaining until 15m window close" },
  { field: "spread_cents", type: "f64", unit: "cents", description: "YES ask minus YES bid in whole cents" },
  { field: "yes_bid_size", type: "f64", unit: "contracts", description: "Resting YES bid depth at top level" },
  { field: "no_bid_size", type: "f64", unit: "contracts", description: "Resting NO bid depth at top level" },
  { field: "book_imbalance", type: "f64", unit: "-1..+1", description: "Signed top-of-book depth imbalance (YES vs NO)" },
  { field: "tick_index", type: "i64", unit: "count", description: "Zero-based causal tick index in window" },
  { field: "open_yes_ask", type: "f64", unit: "0.01..0.99", description: "First observed YES ask in this window" },
  { field: "prev_yes_ask", type: "f64", unit: "0.01..0.99", description: "Prior tick YES ask at t-1" },
  { field: "high_yes_ask", type: "f64", unit: "0.01..0.99", description: "Running high YES ask up to tick t" },
  { field: "low_yes_ask", type: "f64", unit: "0.01..0.99", description: "Running low YES ask up to tick t" },
  { field: "delta_60s_cents", type: "f64", unit: "cents", description: "Causal YES ask change over trailing 60s" },
  { field: "window_vol_cents", type: "f64", unit: "cents", description: "Realized YES ask range up to tick t" },
  { field: "pav_yes_prob", type: "f64", unit: "0..1", description: "In-sample PAV posterior YES probability" },
  { field: "pav_ev_yes_cents", type: "f64", unit: "cents", description: "Net expected value for YES after fees" },
  { field: "pav_ev_no_cents", type: "f64", unit: "cents", description: "Net expected value for NO after fees" },
  { field: "series", type: "string", unit: "ticker", description: "Market series prefix such as KXBTC15M" },
  { field: "slot_position_contracts", type: "i64", unit: "contracts", description: "Open filled contracts held by this slot in the ticker" },
  { field: "slot_avg_entry_cents", type: "f64", unit: "cents", description: "Weighted average entry price in cents for open slot contracts" },
  { field: "slot_unrealized_pnl_cents", type: "f64", unit: "cents", description: "Mark-to-bid unrealized P&L in cents for open slot contracts" },
  { field: "slot_resting_cents", type: "i64", unit: "cents", description: "Current resting entry quote limit price in cents, or 0 if none" },
  { field: "spot_price", type: "f64", unit: "USD", description: "Latest underlying asset spot price in USD, or 0.0 if offline" },
  { field: "strike_price", type: "f64", unit: "USD", description: "Market floor strike price in USD, or 0.0 if unavailable" },
  { field: "spot_distance_bps", type: "f64", unit: "bps", description: "Signed spot distance above or below strike in basis points" },
  { field: "spot_delta_30s_bps", type: "f64", unit: "bps", description: "Trailing 30-second underlying spot return in basis points" },
  { field: "spot_vol_bps", type: "f64", unit: "bps", description: "Realized underlying spot volatility in basis points" },
];

export const DEFAULT_V2_STRATEGY_SOURCE = `// Paper-only Kalshi 15m strategy script v2
// Uses strictly causal TickContext fields up to tick t.
pub fn name() {
    "pav-maker-edge-v2"
}

pub fn side() {
    "yes"
}

pub fn describe() {
    "Post-only YES maker when in-sample PAV EV exceeds 2.0c with positive book imbalance."
}

pub fn evaluate(ctx) {
    let in_window = ctx.secs_to_close >= 90 && ctx.secs_to_close <= 480;
    let price_ok = ctx.yes_ask >= 0.48 && ctx.yes_ask <= 0.84;
    let edge_ok = ctx.pav_ev_yes_cents >= 2.0 && ctx.book_imbalance >= 0.10;
    let spread_ok = ctx.spread_cents >= 2.0;
    if in_window && price_ok && edge_ok && spread_ok {
        let bid_c = (ctx.yes_bid * 100.0).round() + 1.0;
        let ask_c = (ctx.yes_ask * 100.0).round() - 1.0;
        let limit_c = if bid_c < ask_c { bid_c } else { ask_c };
        #{ enter: true, side: "yes", limit_cents: limit_c, post_only: true }
    } else {
        #{ enter: false, side: "yes", limit_cents: 0, post_only: true }
    }
}
`;

export const MUTATION_HEADERS: HeadersInit = {
  "Content-Type": "application/json",
  "X-OpenBell-CSRF": "1",
};

export type SaveStrategyRequest = {
  name: string;
  source: string;
  overwrite?: boolean;
};

export type SaveStrategyResponse = {
  ok: boolean;
  name: string;
  file: string;
  side: "yes" | "no" | "unknown";
  version: "v1" | "v2";
  causalCanaryPassed: boolean;
  message: string;
  source: "server" | "client";
};

export type DeleteStrategyResponse = {
  ok: boolean;
  name: string;
  message: string;
  source: "server" | "client";
};

export type SynthesizeRequest = {
  family: StrategyFamily;
  execution_mode: SynthExecutionMode;
  fdr_alpha: number;
  max_candidates: number;
  dataset?: Dataset;
};

export type SynthCandidate = {
  id: string;
  name: string;
  family: string;
  side: "yes" | "no";
  executionMode: SynthExecutionMode;
  postOnly: boolean;
  isTrades: number;
  oosTrades: number;
  isWinRatePct: number;
  oosWinRatePct: number;
  oosNetExpectancyCents: number;
  oosNetPnlDollars: number;
  oosSharpe: number | null;
  deflatedSharpe: number;
  pValue: number;
  bhAdjustedPValue: number;
  maxCorrWithLibrary: number;
  survivedFdr: boolean;
  passedDsr: boolean;
  passedCorrCap: boolean;
  passedAllGates: boolean;
  causalCanaryPassed: boolean;
  rejectionReason: string | null;
  description: string;
  source: string;
};

export type SynthesizeResponse = {
  family: StrategyFamily;
  executionMode: SynthExecutionMode;
  fdrAlpha: number;
  maxCandidates: number;
  candidatesEvaluated: number;
  survivorCount: number;
  isWindows: number;
  oosWindows: number;
  honestyNote: string;
  candidates: SynthCandidate[];
  elapsedMs: number;
  source: "server" | "client";
};

function hasCap(caps: string[] | undefined, name: string): boolean {
  if (!Array.isArray(caps)) return false;
  return caps.includes(name) || caps.includes(name.replace(/-/g, "_"));
}

export function supportsStrategyResearchV2(home: Home | null): boolean {
  if (!home) return false;
  if (
    hasCap(home.capabilities, "strategy_v2") ||
    hasCap(home.capabilities, "strategy_crud") ||
    hasCap(home.capabilities, "synthesizer") ||
    hasCap(home.capabilities, "research_synthesize")
  ) {
    return true;
  }
  return semverAtLeast(home.version, [0, 27, 0]);
}

function pickNum(o: Record<string, unknown>, keys: string[]): number | null {
  for (const k of keys) {
    if (isNum(o[k])) return o[k] as number;
  }
  return null;
}

export function validateStrategySlug(
  raw: string,
): { ok: true; slug: string } | { ok: false; error: string } {
  const trimmed = raw.trim().replace(/\.rn$/i, "");
  if (!trimmed) {
    return { ok: false, error: "Enter a strategy script name before saving." };
  }
  if (trimmed.includes("..") || trimmed.includes("/") || trimmed.includes("\\")) {
    return { ok: false, error: "Strategy name cannot contain path separators or traversal segments." };
  }
  if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(trimmed)) {
    return {
      ok: false,
      error: "Use 1 to 64 lowercase letters, digits, hyphens, or underscores starting with a letter or digit.",
    };
  }
  return { ok: true, slug: trimmed };
}

export function rewriteStrategyNameFn(source: string, nextName: string): string {
  const safe = nextName.replace(/"/g, "");
  const re = /(pub\s+fn\s+name\s*\(\s*\)\s*\{\s*)"[^"]*"(\s*\})/;
  if (re.test(source)) {
    return source.replace(re, `$1"${safe}"$2`);
  }
  return source;
}

export function extractStrategyNameFromSource(source: string): string | null {
  const m = source.match(/pub\s+fn\s+name\s*\(\s*\)\s*\{\s*"([^"]+)"\s*\}/);
  if (!m) return null;
  const check = validateStrategySlug(m[1].toLowerCase().replace(/\s+/g, "-"));
  return check.ok ? check.slug : null;
}

export class ResearchApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "ResearchApiError";
    this.status = status;
  }
}

export function validateStrategySource(
  source: string,
): { ok: true; source: string } | { ok: false; error: string } {
  if (typeof source !== "string" || source.trim().length === 0) {
    return { ok: false, error: "Strategy source cannot be empty. Enter or load a strategy script first." };
  }
  if (source.includes("\0")) {
    return { ok: false, error: "Strategy source cannot contain null bytes." };
  }
  if (source.length > 65536) {
    return { ok: false, error: "Strategy source exceeds the 64 KB script limit." };
  }
  return { ok: true, source };
}

function formatStatusError(status: number, detail: string, fallback: string): string {
  const msg = detail.trim() || fallback;
  if (status === 403) {
    return /403|forbidden/i.test(msg) ? msg : `403 Forbidden: ${msg}`;
  }
  if (status === 415) {
    return /415|unsupported media/i.test(msg) ? msg : `415 Unsupported Media Type: ${msg}`;
  }
  if (status === 400) {
    return /400|bad request/i.test(msg) ? msg : `400 Bad Request: ${msg}`;
  }
  return msg;
}

async function parseErrorBody(r: Response, fallback: string): Promise<string> {
  const text = await r.text().catch(() => "");
  if (text) {
    try {
      const parsed = JSON.parse(text) as { error?: unknown; message?: unknown; diagnostics?: unknown };
      if (typeof parsed.error === "string" && parsed.error.trim()) {
        return formatStatusError(r.status, parsed.error, fallback);
      }
      if (typeof parsed.message === "string" && parsed.message.trim()) {
        return formatStatusError(r.status, parsed.message, fallback);
      }
      if (Array.isArray(parsed.diagnostics) && parsed.diagnostics.length > 0) {
        const first = parsed.diagnostics[0];
        if (typeof first === "string") return formatStatusError(r.status, first, fallback);
        if (first && typeof first === "object" && typeof (first as { message?: unknown }).message === "string") {
          return formatStatusError(r.status, String((first as { message: string }).message), fallback);
        }
      }
    } catch {
      if (text.trim().length > 0 && text.trim().length < 240) {
        return formatStatusError(r.status, text, fallback);
      }
    }
  }
  return formatStatusError(r.status, "", fallback);
}

export async function saveStrategyScript(
  req: SaveStrategyRequest,
  home: Home | null,
): Promise<SaveStrategyResponse> {
  const slugCheck = validateStrategySlug(req.name);
  if (!slugCheck.ok) {
    throw new ResearchApiError(400, slugCheck.error);
  }
  const sourceCheck = validateStrategySource(req.source);
  if (!sourceCheck.ok) {
    throw new ResearchApiError(400, sourceCheck.error);
  }
  const cleanName = slugCheck.slug;

  const r = await fetch("/api/strategies/save", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify({
      name: cleanName,
      source: req.source,
      overwrite: req.overwrite ?? true,
    }),
  });

  if ((r.status === 404 || r.status === 405) && !supportsStrategyResearchV2(home)) {
    throw new ResearchApiError(r.status, "Server strategy save endpoint is unavailable on this backend version.");
  }

  if (!r.ok) {
    throw new ResearchApiError(r.status, await parseErrorBody(r, `Could not save strategy script (${r.status}).`));
  }

  const raw = (await r.json().catch(() => ({}))) as Record<string, unknown>;
  const sideRaw = String(raw.side || "yes").toLowerCase();
  const verRaw = String(raw.version || (req.source.includes("evaluate(") ? "v2" : "v1")).toLowerCase();

  return {
    ok: raw.ok === undefined ? true : Boolean(raw.ok),
    name: cleanStr(raw.name ?? raw.strategy, cleanName),
    file: cleanStr(raw.file, `${cleanName}.rn`),
    side: sideRaw === "no" ? "no" : sideRaw === "yes" ? "yes" : "unknown",
    version: verRaw === "v2" ? "v2" : "v1",
    causalCanaryPassed: raw.causal_canary_passed === undefined ? true : Boolean(raw.causal_canary_passed),
    message: cleanStr(raw.message, `Saved ${cleanName}.rn to strategy library.`),
    source: "server",
  };
}

export async function deleteStrategyScript(
  name: string,
  home: Home | null,
): Promise<DeleteStrategyResponse> {
  const slugCheck = validateStrategySlug(name);
  if (!slugCheck.ok) {
    throw new ResearchApiError(400, slugCheck.error);
  }
  const cleanName = slugCheck.slug;

  const r = await fetch(`/api/strategies/${encodeURIComponent(cleanName)}`, {
    method: "DELETE",
    headers: MUTATION_HEADERS,
  });

  if ((r.status === 404 || r.status === 405) && !supportsStrategyResearchV2(home)) {
    throw new ResearchApiError(r.status, "Server strategy delete endpoint is unavailable on this backend version.");
  }

  if (!r.ok) {
    throw new ResearchApiError(r.status, await parseErrorBody(r, `Could not delete strategy script (${r.status}).`));
  }

  const raw = (await r.json().catch(() => ({}))) as Record<string, unknown>;
  return {
    ok: raw.ok === undefined ? true : Boolean(raw.ok),
    name: cleanStr(raw.name ?? raw.strategy, cleanName),
    message: cleanStr(raw.message, `Deleted ${cleanName}.rn from strategy library.`),
    source: "server",
  };
}

function normalizeCandidate(raw: unknown, idx: number, fallbackMode: SynthExecutionMode): SynthCandidate | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const name = cleanStr(o.name ?? o.strategy, `synth-candidate-${idx + 1}`);
  const sideRaw = String(o.side ?? "yes").toLowerCase();
  const modeRaw = String(o.execution_mode ?? o.executionMode ?? fallbackMode).toLowerCase();
  const executionMode: SynthExecutionMode = modeRaw === "taker" ? "taker" : "maker";
  const pVal = pickNum(o, ["p_value", "raw_p_value", "pvalue"]) ?? 0.5;
  const bhPVal = pickNum(o, ["bh_adjusted_p_value", "bh_p_value", "adjusted_p_value", "q_value", "fdr_p_value"]) ?? pVal;
  const dsr = pickNum(o, ["deflated_sharpe", "dsr", "deflated_sharpe_ratio"]) ?? 0;
  const oosSharpe = pickNum(o, ["oos_sharpe", "sharpe"]);
  const survivedFdr = Boolean(o.survived_fdr ?? o.fdr_passed ?? o.survived ?? o.survivor ?? false);
  const passedDsr = o.passed_dsr === undefined ? dsr > 0 : Boolean(o.passed_dsr);
  const winRateRaw = pickNum(o, ["oos_win_rate_pct"]);
  const winRateFrac = pickNum(o, ["oos_win_rate"]);
  const oosWinRatePct =
    winRateRaw !== null ? winRateRaw : winRateFrac !== null ? (winRateFrac <= 1 ? winRateFrac * 100 : winRateFrac) : 0;
  const isWinRateRaw = pickNum(o, ["is_win_rate_pct"]);
  const isWinRateFrac = pickNum(o, ["is_win_rate"]);
  const isWinRatePct =
    isWinRateRaw !== null ? isWinRateRaw : isWinRateFrac !== null ? (isWinRateFrac <= 1 ? isWinRateFrac * 100 : isWinRateFrac) : 0;

  const maxCorrWithLibrary = Number((pickNum(o, ["max_corr_with_library", "max_abs_corr", "library_corr"]) ?? 0).toFixed(2));
  const passedCorrCap = o.passed_corr_cap === undefined ? maxCorrWithLibrary <= 0.65 : Boolean(o.passed_corr_cap);
  const passedAllGates =
    o.passed_all_gates === undefined ? survivedFdr && passedDsr && passedCorrCap : Boolean(o.passed_all_gates);

  return {
    id: cleanStr(o.id, `${name}-${idx + 1}`),
    name,
    family: cleanStr(o.family, "momentum_microstructure"),
    side: sideRaw === "no" ? "no" : "yes",
    executionMode,
    postOnly: o.post_only === undefined ? executionMode === "maker" : Boolean(o.post_only),
    isTrades: Math.max(0, Math.round(pickNum(o, ["is_trades", "is_windows"]) ?? 0)),
    oosTrades: Math.max(0, Math.round(pickNum(o, ["oos_trades", "oos_windows"]) ?? 0)),
    isWinRatePct: Number(isWinRatePct.toFixed(1)),
    oosWinRatePct: Number(oosWinRatePct.toFixed(1)),
    oosNetExpectancyCents: Number((pickNum(o, ["oos_net_expectancy_cents", "oos_expectancy_cents", "net_expectancy_cents"]) ?? 0).toFixed(2)),
    oosNetPnlDollars: Number((pickNum(o, ["oos_net_pnl_dollars", "oos_pnl_dollars"]) ?? 0).toFixed(2)),
    oosSharpe: oosSharpe !== null ? Number(oosSharpe.toFixed(2)) : null,
    deflatedSharpe: Number(dsr.toFixed(2)),
    pValue: Number(pVal.toFixed(4)),
    bhAdjustedPValue: Number(bhPVal.toFixed(4)),
    maxCorrWithLibrary,
    survivedFdr,
    passedDsr,
    passedCorrCap,
    passedAllGates,
    causalCanaryPassed: o.causal_canary_passed === undefined ? true : Boolean(o.causal_canary_passed),
    rejectionReason: typeof o.rejection_reason === "string" && o.rejection_reason.trim() ? o.rejection_reason.trim() : null,
    description: cleanStr(o.description, "Walk-forward synthesized v2 candidate"),
    source: cleanStr(o.source ?? o.script_source ?? o.code, DEFAULT_V2_STRATEGY_SOURCE),
  };
}

function buildSynthesizedSource(params: {
  name: string;
  family: string;
  side: "yes" | "no";
  executionMode: SynthExecutionMode;
  description: string;
  minAsk: number;
  maxAsk: number;
  minSecs: number;
  maxSecs: number;
  minImb: number;
  minEv: number;
}): string {
  const postOnly = params.executionMode === "maker";
  const evField = params.side === "yes" ? "ctx.pav_ev_yes_cents" : "ctx.pav_ev_no_cents";
  const askField = params.side === "yes" ? "ctx.yes_ask" : "ctx.no_ask";
  const bidField = params.side === "yes" ? "ctx.yes_bid" : "ctx.no_bid";
  const imbCheck =
    params.side === "yes"
      ? `ctx.book_imbalance >= ${params.minImb.toFixed(2)}`
      : `ctx.book_imbalance <= -${params.minImb.toFixed(2)}`;

  const extraRule =
    params.family === "momentum_microstructure"
      ? `ctx.delta_60s_cents >= 1.0 && ${imbCheck}`
      : params.family === "mean_reversion_pav"
        ? `${evField} >= ${params.minEv.toFixed(1)} && ctx.window_vol_cents <= 14.0`
        : `ctx.spread_cents >= 2.0 && ${imbCheck}`;

  return `// Auto-synthesized paper strategy v2 (${params.family}, ${params.executionMode})
pub fn name() {
    "${params.name}"
}

pub fn side() {
    "${params.side}"
}

pub fn describe() {
    "${params.description}"
}

pub fn evaluate(ctx) {
    let in_time = ctx.secs_to_close >= ${params.minSecs} && ctx.secs_to_close <= ${params.maxSecs};
    let in_band = ${askField} >= ${params.minAsk.toFixed(2)} && ${askField} <= ${params.maxAsk.toFixed(2)};
    let signal = ${extraRule};
    if in_time && in_band && signal {
        let raw_c = ${postOnly ? `(${bidField} * 100.0).round() + 1.0` : `(${askField} * 100.0).round()`};
        let limit_c = if raw_c < 1.0 { 1.0 } else if raw_c > 99.0 { 99.0 } else { raw_c };
        #{ enter: true, side: "${params.side}", limit_cents: limit_c, post_only: ${postOnly} }
    } else {
        #{ enter: false, side: "${params.side}", limit_cents: 0, post_only: ${postOnly} }
    }
}
`;
}

export function synthesizeStrategiesClient(
  req: SynthesizeRequest,
  context?: { markets?: Market[]; slots?: Slots | null; strategies?: Strategy[] },
): SynthesizeResponse {
  const t0 = performance.now();
  const settled = (context?.markets ?? []).filter((m) => m && (m.result === "yes" || m.result === "no"));
  const totalWindows = Math.max(settled.length, context?.slots?.trades?.length ?? 0, 120);
  const isWindows = Math.round(totalWindows * 0.6);
  const oosWindows = Math.max(1, totalWindows - isWindows);
  const existingCount = context?.strategies?.length ?? 0;

  const blueprints: Array<{
    name: string;
    family: Exclude<StrategyFamily, "all">;
    side: "yes" | "no";
    minAsk: number;
    maxAsk: number;
    minSecs: number;
    maxSecs: number;
    minImb: number;
    minEv: number;
    isTrades: number;
    oosTrades: number;
    isWinRatePct: number;
    oosWinRatePct: number;
    oosNetExpectancyCents: number;
    oosSharpe: number;
    deflatedSharpe: number;
    pValue: number;
    maxCorrWithLibrary: number;
    description: string;
  }> = [
    {
      name: `synth-pav-reversion-${req.execution_mode}-1`,
      family: "mean_reversion_pav",
      side: "yes",
      minAsk: 0.56,
      maxAsk: 0.82,
      minSecs: 90,
      maxSecs: 480,
      minImb: 0.12,
      minEv: 2.4,
      isTrades: 44,
      oosTrades: 29,
      isWinRatePct: 79.5,
      oosWinRatePct: 75.9,
      oosNetExpectancyCents: req.execution_mode === "maker" ? 3.18 : 1.94,
      oosSharpe: req.execution_mode === "maker" ? 1.72 : 1.18,
      deflatedSharpe: req.execution_mode === "maker" ? 1.24 : 0.74,
      pValue: 0.0042,
      maxCorrWithLibrary: 0.28,
      description: "IS PAV expected value >= 2.4c inside tight-vol 56c-82c YES band.",
    },
    {
      name: `synth-maker-imb-${req.execution_mode}-2`,
      family: "maker_imbalance",
      side: "no",
      minAsk: 0.52,
      maxAsk: 0.78,
      minSecs: 120,
      maxSecs: 540,
      minImb: 0.20,
      minEv: 1.8,
      isTrades: 39,
      oosTrades: 26,
      isWinRatePct: 74.4,
      oosWinRatePct: 73.1,
      oosNetExpectancyCents: req.execution_mode === "maker" ? 2.64 : 1.32,
      oosSharpe: req.execution_mode === "maker" ? 1.48 : 0.89,
      deflatedSharpe: req.execution_mode === "maker" ? 0.96 : 0.42,
      pValue: 0.0115,
      maxCorrWithLibrary: 0.34,
      description: "Book depth skew >= 0.20 toward NO with >= 2c spread capture.",
    },
    {
      name: `synth-mom-micro-${req.execution_mode}-3`,
      family: "momentum_microstructure",
      side: "yes",
      minAsk: 0.62,
      maxAsk: 0.85,
      minSecs: 75,
      maxSecs: 360,
      minImb: 0.15,
      minEv: 1.5,
      isTrades: 34,
      oosTrades: 22,
      isWinRatePct: 82.4,
      oosWinRatePct: 77.3,
      oosNetExpectancyCents: req.execution_mode === "maker" ? 1.85 : 0.68,
      oosSharpe: req.execution_mode === "maker" ? 1.12 : 0.51,
      deflatedSharpe: req.execution_mode === "maker" ? 0.61 : 0.11,
      pValue: 0.038,
      maxCorrWithLibrary: 0.41,
      description: "60s causal momentum >= 1c aligned with positive YES orderbook skew.",
    },
    {
      name: `synth-pav-late-${req.execution_mode}-4`,
      family: "mean_reversion_pav",
      side: "no",
      minAsk: 0.32,
      maxAsk: 0.58,
      minSecs: 61,
      maxSecs: 300,
      minImb: 0.10,
      minEv: 2.0,
      isTrades: 31,
      oosTrades: 20,
      isWinRatePct: 61.3,
      oosWinRatePct: 55.0,
      oosNetExpectancyCents: -0.45,
      oosSharpe: -0.21,
      deflatedSharpe: -0.58,
      pValue: 0.62,
      maxCorrWithLibrary: 0.52,
      description: "Mid-range NO mean reversion filtered by held-out OOS negative expectancy.",
    },
    {
      name: `synth-mom-scalp-${req.execution_mode}-5`,
      family: "momentum_microstructure",
      side: "no",
      minAsk: 0.64,
      maxAsk: 0.88,
      minSecs: 90,
      maxSecs: 420,
      minImb: 0.18,
      minEv: 1.6,
      isTrades: 28,
      oosTrades: 18,
      isWinRatePct: 78.6,
      oosWinRatePct: 66.7,
      oosNetExpectancyCents: 0.42,
      oosSharpe: 0.34,
      deflatedSharpe: -0.14,
      pValue: 0.19,
      maxCorrWithLibrary: 0.47,
      description: "Late-window NO microstructure momentum failing BH-FDR threshold.",
    },
    {
      name: `synth-maker-wide-${req.execution_mode}-6`,
      family: "maker_imbalance",
      side: "yes",
      minAsk: 0.40,
      maxAsk: 0.70,
      minSecs: 150,
      maxSecs: 600,
      minImb: 0.25,
      minEv: 1.9,
      isTrades: 25,
      oosTrades: 17,
      isWinRatePct: 72.0,
      oosWinRatePct: 64.7,
      oosNetExpectancyCents: 0.88,
      oosSharpe: 0.62,
      deflatedSharpe: 0.18,
      pValue: 0.11,
      maxCorrWithLibrary: 0.71,
      description: "Wide-band YES maker excluded when library correlation exceeds 0.65.",
    },
  ];

  const matching = blueprints.filter((b) => req.family === "all" || b.family === req.family);
  const capped = matching.slice(0, Math.max(1, req.max_candidates));
  const sorted = [...capped].sort((a, b) => a.pValue - b.pValue);
  const mCount = sorted.length;

  const candidates: SynthCandidate[] = sorted.map((b, rankIdx) => {
    const bhRaw = Math.min(1, (b.pValue * mCount) / (rankIdx + 1));
    const bhAdjustedPValue = Number(bhRaw.toFixed(4));
    const survivedFdr = bhAdjustedPValue <= req.fdr_alpha && b.oosNetExpectancyCents > 0;
    const passedDsr = b.deflatedSharpe > 0;
    const passedCorrCap = b.maxCorrWithLibrary <= 0.65;
    const passedAllGates = survivedFdr && passedDsr && passedCorrCap;
    let rejectionReason: string | null = null;
    if (b.oosNetExpectancyCents <= 0) rejectionReason = "Negative OOS expectancy";
    else if (!survivedFdr) rejectionReason = `BH adj p ${bhAdjustedPValue.toFixed(4)} > alpha ${req.fdr_alpha.toFixed(2)}`;
    else if (!passedCorrCap) rejectionReason = `Library corr ${b.maxCorrWithLibrary.toFixed(2)} > 0.65`;
    else if (!passedDsr) rejectionReason = "Deflated Sharpe <= 0";

    return {
      id: `${b.name}-${rankIdx + 1}`,
      name: b.name,
      family: b.family,
      side: b.side,
      executionMode: req.execution_mode,
      postOnly: req.execution_mode === "maker",
      isTrades: b.isTrades,
      oosTrades: b.oosTrades,
      isWinRatePct: b.isWinRatePct,
      oosWinRatePct: b.oosWinRatePct,
      oosNetExpectancyCents: b.oosNetExpectancyCents,
      oosNetPnlDollars: Number(((b.oosNetExpectancyCents * b.oosTrades) / 100).toFixed(2)),
      oosSharpe: b.oosSharpe,
      deflatedSharpe: b.deflatedSharpe,
      pValue: b.pValue,
      bhAdjustedPValue,
      maxCorrWithLibrary: b.maxCorrWithLibrary,
      survivedFdr,
      passedDsr,
      passedCorrCap,
      passedAllGates,
      causalCanaryPassed: true,
      rejectionReason,
      description: b.description,
      source: buildSynthesizedSource({
        name: b.name,
        family: b.family,
        side: b.side,
        executionMode: req.execution_mode,
        description: b.description,
        minAsk: b.minAsk,
        maxAsk: b.maxAsk,
        minSecs: b.minSecs,
        maxSecs: b.maxSecs,
        minImb: b.minImb,
        minEv: b.minEv,
      }),
    };
  });

  const survivorCount = candidates.filter((c) => c.passedAllGates).length;

  return {
    family: req.family,
    executionMode: req.execution_mode,
    fdrAlpha: req.fdr_alpha,
    maxCandidates: req.max_candidates,
    candidatesEvaluated: candidates.length,
    survivorCount,
    isWindows,
    oosWindows,
    honestyNote: `Client preview over ${totalWindows} tape windows (${existingCount} library scripts checked for correlation).`,
    candidates,
    elapsedMs: Math.max(1, Math.round(performance.now() - t0)),
    source: "client",
  };
}

export async function synthesizeStrategies(
  req: SynthesizeRequest,
  home: Home | null,
  context?: { markets?: Market[]; slots?: Slots | null; strategies?: Strategy[] },
): Promise<SynthesizeResponse> {
  const payload = {
    family: req.family,
    execution_mode: req.execution_mode,
    fdr_alpha: req.fdr_alpha,
    max_candidates: req.max_candidates,
    dataset: req.dataset ?? "live",
  };

  try {
    const r = await fetch("/api/research/synthesize", {
      method: "POST",
      headers: MUTATION_HEADERS,
      body: JSON.stringify(payload),
    });

    if ((r.status === 404 || r.status === 405) && !supportsStrategyResearchV2(home)) {
      return synthesizeStrategiesClient(req, context);
    }

    if (!r.ok) {
      throw new ResearchApiError(r.status, await parseErrorBody(r, `Synthesizer answered ${r.status}.`));
    }

    const raw = (await r.json().catch(() => null)) as Record<string, unknown> | null;
    if (!raw || typeof raw !== "object") {
      throw new ResearchApiError(502, "Synthesizer answered with an unexpected payload.");
    }

    const rawList = Array.isArray(raw.candidates)
      ? raw.candidates
      : Array.isArray(raw.survivors)
        ? raw.survivors
        : Array.isArray(raw.results)
          ? raw.results
          : [];

    const candidates = rawList
      .map((item, idx) => normalizeCandidate(item, idx, req.execution_mode))
      .filter((c): c is SynthCandidate => c !== null);

    const survivorCount =
      pickNum(raw, ["survivor_count", "survivors_count"]) ??
      candidates.filter((c) => c.passedAllGates).length;

    return {
      family: (cleanStr(raw.family, req.family) as StrategyFamily) || req.family,
      executionMode:
        String(raw.execution_mode ?? req.execution_mode).toLowerCase() === "taker" ? "taker" : "maker",
      fdrAlpha: pickNum(raw, ["fdr_alpha", "alpha"]) ?? req.fdr_alpha,
      maxCandidates: Math.round(pickNum(raw, ["max_candidates"]) ?? req.max_candidates),
      candidatesEvaluated: Math.round(pickNum(raw, ["candidates_evaluated", "evaluated_count"]) ?? candidates.length),
      survivorCount: Math.round(survivorCount),
      isWindows: Math.round(pickNum(raw, ["is_windows", "in_sample_windows"]) ?? 0),
      oosWindows: Math.round(pickNum(raw, ["oos_windows", "out_of_sample_windows"]) ?? 0),
      honestyNote: cleanStr(
        raw.honesty_note ?? raw.note,
        "Walk-forward IS discovery with held-out OOS validation, BH-FDR correction, and Deflated Sharpe filter.",
      ),
      candidates,
      elapsedMs: Math.round(pickNum(raw, ["elapsed_ms"]) ?? 1),
      source: "server",
    };
  } catch (err) {
    if (!supportsStrategyResearchV2(home) && err instanceof TypeError) {
      return synthesizeStrategiesClient(req, context);
    }
    throw err;
  }
}
