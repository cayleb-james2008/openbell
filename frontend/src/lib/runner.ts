// OpenBell Shadow & Fail-Closed Auto-Live Runner client, SSE subscriber, and deterministic preview state.
// Aligned with /api/runner/{status,config,start,stop,ledger} and /api/tape/stream (`event: runner`).
import {
  type Dataset,
  type Home,
  type Market,
  type Strategy,
  cleanStr,
  isNum,
  semverAtLeast,
} from "@/lib/desk";

export type RunnerMode = "stopped" | "shadow" | "live_auto";
export type RunnerExecutionMode = "maker" | "taker";

export type RunnerSlotConfig = {
  slot_id: number;
  strategy: string;
  series: string[];
  max_contracts: number;
  execution_mode: RunnerExecutionMode;
  auto_pause_drawdown_dollars: number;
  enabled: boolean;
};

export type RunnerSlotState = RunnerSlotConfig & {
  paused: boolean;
  pause_reason: string | null;
  realized_pnl_dollars: number;
  drawdown_dollars: number;
  quotes_posted: number;
  fills: number;
  open_quotes_count: number;
  last_signal_epoch: number | null;
};

export type VirtualOrLiveOrder = {
  order_id: string;
  slot_id: number;
  strategy: string;
  ticker: string;
  series: string;
  side: "yes" | "no";
  mode: "shadow" | "live";
  execution_mode: RunnerExecutionMode;
  limit_cents: number;
  contracts: number;
  remaining_contracts: number;
  queue_ahead_initial: number;
  queue_ahead_remaining: number;
  est_fill_eta_secs: number | null;
  posted_epoch: number;
  close_epoch: number | null;
  status: string;
};

export type RunnerAggregateStats = {
  quotes_posted: number;
  fills: number;
  fill_rate_pct: number;
  realized_pnl_dollars: number;
  unrealized_exposure_dollars: number;
  avg_fill_slippage_cents: number;
  avg_queue_wait_secs: number;
};

export type RunnerStatus = {
  mode: RunnerMode;
  live_armed: boolean;
  killed: boolean;
  tick_interval_ms: number;
  cancel_before_close_secs: number;
  bankroll_dollars: number;
  active_slots: RunnerSlotState[];
  open_quotes: VirtualOrLiveOrder[];
  stats: RunnerAggregateStats;
  last_error: string | null;
  source: "server" | "preview";
};

export type RunnerConfigPayload = {
  slots: RunnerSlotConfig[];
  cancel_before_close_secs: number;
  bankroll_dollars: number;
};

export type RunnerStartPayload = {
  mode: "shadow" | "live_auto";
  confirm_auto_live?: boolean;
};

export type RunnerStopPayload = {
  cancel_resting?: boolean;
};

export type LedgerEventType =
  | "QuotePosted"
  | "QuoteCanceled"
  | "OrderFilled"
  | "WindowSettled"
  | "SlotPaused";

export type LedgerEvent = {
  id: string;
  seq?: number;
  ts_epoch: number;
  ts_iso: string;
  event: LedgerEventType | string;
  mode: "shadow" | "live";
  slot_id: number;
  strategy: string;
  ticker: string;
  series?: string | null;
  order_id?: string | null;
  live_order_id?: string | null;
  execution_mode?: RunnerExecutionMode | null;
  side: "yes" | "no" | null;
  price_cents: number | null;
  limit_cents: number | null;
  fill_cents: number | null;
  contracts: number | null;
  fee_dollars: number | null;
  pnl_dollars: number | null;
  slippage_cents: number | null;
  queue_wait_secs: number | null;
  result?: string | null;
  reason: string | null;
};

export type StrategyExecutionStats = {
  strategy: string;
  mode: "shadow" | "live" | "all";
  quotes_posted: number;
  fills: number;
  canceled: number;
  windows_settled: number;
  fill_rate_pct: number;
  wins: number;
  losses: number;
  realized_pnl_dollars: number;
  total_fees_dollars: number;
  avg_fill_slippage_cents: number;
  avg_queue_wait_secs: number;
  paused_count: number;
};

export type RunnerLedgerResponse = {
  entries: LedgerEvent[];
  summary_by_strategy: StrategyExecutionStats[];
  source: "server" | "preview";
};

export type RunnerSseEventKind =
  | "quote_posted"
  | "quote_filled"
  | "quote_canceled"
  | "window_settled"
  | "slot_auto_paused"
  | "runner_halted";

export type RunnerSseEvent = {
  kind: RunnerSseEventKind | string;
  mode?: RunnerMode;
  slot_id?: number;
  strategy?: string;
  ticker?: string;
  ts_epoch?: number;
  detail?: string;
  raw: Record<string, unknown>;
};

const MUTATION_HEADERS: HeadersInit = {
  "Content-Type": "application/json",
  "X-OpenBell-CSRF": "1",
};

export const AVAILABLE_SERIES = [
  "KXBTC15M",
  "KXETH15M",
  "KXSOL15M",
  "KXXRP15M",
  "KXDOGE15M",
];

function hasCap(caps: string[] | undefined, name: string): boolean {
  if (!Array.isArray(caps)) return false;
  return caps.includes(name) || caps.includes(name.replace(/-/g, "_"));
}

export function supportsShadowRunner(home: Home | null): boolean {
  if (!home) return false;
  if (hasCap(home.capabilities, "shadow_runner") || hasCap(home.capabilities, "runner")) {
    return true;
  }
  return semverAtLeast(home.version, [0, 25, 0]);
}

function pickNum(o: Record<string, unknown>, keys: string[]): number | null {
  for (const k of keys) {
    if (isNum(o[k])) return o[k] as number;
  }
  return null;
}

function normalizeMode(raw: unknown): RunnerMode {
  const s = typeof raw === "string" ? raw.trim().toLowerCase() : "";
  if (s === "live_auto" || s === "live") return "live_auto";
  if (s === "shadow") return "shadow";
  return "stopped";
}

function normalizeExecMode(raw: unknown): RunnerExecutionMode {
  const s = typeof raw === "string" ? raw.trim().toLowerCase() : "";
  return s === "taker" ? "taker" : "maker";
}

function normalizeSeriesList(raw: unknown): string[] {
  if (Array.isArray(raw)) {
    const out = raw
      .map((s) => (typeof s === "string" ? s.trim().toUpperCase() : ""))
      .filter((s) => s.length > 0);
    return out.length > 0 ? out : ["KXBTC15M"];
  }
  if (typeof raw === "string" && raw.trim()) {
    const out = raw
      .split(",")
      .map((s) => s.trim().toUpperCase())
      .filter((s) => s.length > 0);
    return out.length > 0 ? out : ["KXBTC15M"];
  }
  return ["KXBTC15M"];
}

function normalizeSlotState(raw: unknown, idx: number): RunnerSlotState | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  return {
    slot_id: Math.max(1, Math.round(pickNum(o, ["slot_id", "slot"]) ?? idx + 1)),
    strategy: cleanStr(o.strategy, `slot-${idx + 1}`),
    series: normalizeSeriesList(o.series),
    max_contracts: Math.max(1, Math.round(pickNum(o, ["max_contracts"]) ?? 10)),
    execution_mode: normalizeExecMode(o.execution_mode),
    auto_pause_drawdown_dollars: Math.max(
      1,
      pickNum(o, ["auto_pause_drawdown_dollars", "auto_pause_dd_dollars"]) ?? 25,
    ),
    enabled: o.enabled === undefined ? true : Boolean(o.enabled),
    paused: Boolean(o.paused),
    pause_reason: typeof o.pause_reason === "string" && o.pause_reason.trim() ? o.pause_reason.trim() : null,
    realized_pnl_dollars: pickNum(o, ["realized_pnl_dollars", "pnl_dollars"]) ?? 0,
    drawdown_dollars: Math.max(0, pickNum(o, ["drawdown_dollars"]) ?? 0),
    quotes_posted: Math.max(0, Math.round(pickNum(o, ["quotes_posted"]) ?? 0)),
    fills: Math.max(0, Math.round(pickNum(o, ["fills"]) ?? 0)),
    open_quotes_count: Math.max(0, Math.round(pickNum(o, ["open_quotes_count"]) ?? 0)),
    last_signal_epoch: pickNum(o, ["last_signal_epoch"]),
  };
}

function normalizeOpenQuote(raw: unknown, idx: number): VirtualOrLiveOrder | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const sideRaw = String(o.side ?? "yes").toLowerCase();
  const modeRaw = String(o.mode ?? "shadow").toLowerCase();
  const contracts = Math.max(1, Math.round(pickNum(o, ["contracts", "count"]) ?? 1));
  const remaining = Math.max(0, Math.round(pickNum(o, ["remaining_contracts", "remaining_count"]) ?? contracts));
  const qInit = Math.max(0, Math.round(pickNum(o, ["queue_ahead_initial", "initial_queue_ahead"]) ?? 0));
  const qRem = Math.max(0, Math.round(pickNum(o, ["queue_ahead_remaining", "queue_ahead"]) ?? qInit));
  return {
    order_id: cleanStr(o.order_id ?? o.id, `quote-${idx + 1}`),
    slot_id: Math.max(1, Math.round(pickNum(o, ["slot_id", "slot"]) ?? 1)),
    strategy: cleanStr(o.strategy, "unnamed"),
    ticker: cleanStr(o.ticker, "KXBTC15M"),
    series: cleanStr(o.series, cleanStr(o.ticker, "KXBTC15M").split("-")[0] || "KXBTC15M"),
    side: sideRaw === "no" ? "no" : "yes",
    mode: modeRaw === "live" || modeRaw === "live_auto" ? "live" : "shadow",
    execution_mode: normalizeExecMode(o.execution_mode),
    limit_cents: Math.min(99, Math.max(1, Math.round(pickNum(o, ["limit_cents", "price_cents"]) ?? 50))),
    contracts,
    remaining_contracts: remaining,
    queue_ahead_initial: Math.max(qInit, qRem),
    queue_ahead_remaining: qRem,
    est_fill_eta_secs: pickNum(o, ["est_fill_eta_secs", "fill_eta_secs"]),
    posted_epoch: Math.round(pickNum(o, ["posted_epoch", "created_epoch"]) ?? 0),
    close_epoch: pickNum(o, ["close_epoch"]),
    status: cleanStr(o.status, "resting"),
  };
}

export function normalizeRunnerStatus(raw: unknown, source: "server" | "preview" = "server"): RunnerStatus {
  const o = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const statsObj = (o.stats && typeof o.stats === "object" ? o.stats : {}) as Record<string, unknown>;
  const slotsRaw = Array.isArray(o.active_slots) ? o.active_slots : [];
  const quotesRaw = Array.isArray(o.open_quotes) ? o.open_quotes : [];
  const quotesPosted = Math.max(0, Math.round(pickNum(statsObj, ["quotes_posted"]) ?? 0));
  const fills = Math.max(0, Math.round(pickNum(statsObj, ["fills"]) ?? 0));
  const fillRate =
    pickNum(statsObj, ["fill_rate_pct"]) ?? (quotesPosted > 0 ? Number(((fills / quotesPosted) * 100).toFixed(1)) : 0);

  return {
    mode: normalizeMode(o.mode),
    live_armed: Boolean(o.live_armed),
    killed: Boolean(o.killed),
    tick_interval_ms: Math.max(250, Math.round(pickNum(o, ["tick_interval_ms"]) ?? 2000)),
    cancel_before_close_secs: Math.max(5, Math.round(pickNum(o, ["cancel_before_close_secs"]) ?? 30)),
    bankroll_dollars: Math.max(10, pickNum(o, ["bankroll_dollars"]) ?? 250),
    active_slots: slotsRaw
      .map((s, i) => normalizeSlotState(s, i))
      .filter((s): s is RunnerSlotState => s !== null),
    open_quotes: quotesRaw
      .map((q, i) => normalizeOpenQuote(q, i))
      .filter((q): q is VirtualOrLiveOrder => q !== null),
    stats: {
      quotes_posted: quotesPosted,
      fills,
      fill_rate_pct: Number(fillRate.toFixed(1)),
      realized_pnl_dollars: pickNum(statsObj, ["realized_pnl_dollars"]) ?? 0,
      unrealized_exposure_dollars: Math.max(0, pickNum(statsObj, ["unrealized_exposure_dollars"]) ?? 0),
      avg_fill_slippage_cents: pickNum(statsObj, ["avg_fill_slippage_cents"]) ?? 0,
      avg_queue_wait_secs: Math.max(0, pickNum(statsObj, ["avg_queue_wait_secs"]) ?? 0),
    },
    last_error: typeof o.last_error === "string" && o.last_error.trim() ? o.last_error.trim() : null,
    source,
  };
}

function normalizeLedgerEvent(raw: unknown, idx: number): LedgerEvent | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const tsEpoch = Math.round(pickNum(o, ["ts", "ts_epoch", "epoch", "timestamp"]) ?? 0);
  const tsIso =
    typeof o.ts_iso === "string" && o.ts_iso
      ? o.ts_iso
      : tsEpoch > 0
        ? new Date(tsEpoch * 1000).toISOString()
        : "";
  const sideRaw = typeof o.side === "string" ? o.side.toLowerCase() : "";
  const modeRaw = String(o.mode ?? "shadow").toLowerCase();
  const seq = pickNum(o, ["seq"]);
  const limitCents = pickNum(o, ["limit_cents"]);
  const fillCents = pickNum(o, ["fill_cents"]);
  const priceCents = pickNum(o, ["fill_cents", "price_cents", "limit_cents"]);
  return {
    id: cleanStr(o.id ?? (seq !== null ? `seq-${seq}` : undefined) ?? o.order_id, `led-${idx}`),
    seq: seq ?? undefined,
    ts_epoch: tsEpoch,
    ts_iso: tsIso,
    event: cleanStr(o.event ?? o.event_type ?? o.kind, "QuotePosted"),
    mode: modeRaw === "live" || modeRaw === "live_auto" ? "live" : "shadow",
    slot_id: Math.max(0, Math.round(pickNum(o, ["slot_id", "slot"]) ?? 0)),
    strategy: cleanStr(o.strategy, "unnamed"),
    ticker: cleanStr(o.ticker, "-"),
    series: typeof o.series === "string" && o.series.trim() ? o.series.trim() : null,
    order_id: typeof o.order_id === "string" && o.order_id.trim() ? o.order_id.trim() : null,
    live_order_id: typeof o.live_order_id === "string" && o.live_order_id.trim() ? o.live_order_id.trim() : null,
    execution_mode: typeof o.execution_mode === "string" ? normalizeExecMode(o.execution_mode) : null,
    side: sideRaw === "yes" ? "yes" : sideRaw === "no" ? "no" : null,
    price_cents: priceCents,
    limit_cents: limitCents,
    fill_cents: fillCents,
    contracts: pickNum(o, ["contracts", "count"]),
    fee_dollars: pickNum(o, ["fee_dollars"]),
    pnl_dollars: pickNum(o, ["realized_pnl_dollars", "pnl_dollars"]),
    slippage_cents: pickNum(o, ["slippage_cents"]),
    queue_wait_secs: pickNum(o, ["queue_wait_secs"]),
    result: typeof o.result === "string" && o.result.trim() ? o.result.trim() : null,
    reason: typeof o.reason === "string" && o.reason.trim() ? o.reason.trim() : null,
  };
}

function normalizeStrategyStats(raw: unknown, idx: number): StrategyExecutionStats | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const quotesPosted = Math.max(0, Math.round(pickNum(o, ["quotes_posted"]) ?? 0));
  const fills = Math.max(0, Math.round(pickNum(o, ["fills"]) ?? 0));
  const fillRate =
    pickNum(o, ["fill_rate_pct"]) ?? (quotesPosted > 0 ? Number(((fills / quotesPosted) * 100).toFixed(1)) : 0);
  const modeRaw = typeof o.mode === "string" ? o.mode.toLowerCase() : "all";
  return {
    strategy: cleanStr(o.strategy, `strategy-${idx + 1}`),
    mode: modeRaw === "shadow" ? "shadow" : modeRaw === "live" ? "live" : "all",
    quotes_posted: quotesPosted,
    fills,
    canceled: Math.max(0, Math.round(pickNum(o, ["quotes_canceled", "canceled", "cancelled"]) ?? 0)),
    windows_settled: Math.max(0, Math.round(pickNum(o, ["windows_settled"]) ?? 0)),
    fill_rate_pct: Number(fillRate.toFixed(1)),
    wins: Math.max(0, Math.round(pickNum(o, ["wins"]) ?? 0)),
    losses: Math.max(0, Math.round(pickNum(o, ["losses"]) ?? 0)),
    realized_pnl_dollars: pickNum(o, ["realized_pnl_dollars", "pnl_dollars"]) ?? 0,
    total_fees_dollars: Math.max(0, pickNum(o, ["total_fees_dollars", "fees_dollars"]) ?? 0),
    avg_fill_slippage_cents: pickNum(o, ["avg_fill_slippage_cents"]) ?? 0,
    avg_queue_wait_secs: Math.max(0, pickNum(o, ["avg_queue_wait_secs"]) ?? 0),
    paused_count: Math.max(0, Math.round(pickNum(o, ["paused_count"]) ?? 0)),
  };
}

export function normalizeRunnerLedger(
  raw: unknown,
  source: "server" | "preview" = "server",
): RunnerLedgerResponse {
  const o = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const entriesRaw = Array.isArray(o.entries) ? o.entries : [];
  const summaryRaw = Array.isArray(o.summary_by_strategy) ? o.summary_by_strategy : [];
  return {
    entries: entriesRaw
      .map((e, i) => normalizeLedgerEvent(e, i))
      .filter((e): e is LedgerEvent => e !== null),
    summary_by_strategy: summaryRaw
      .map((s, i) => normalizeStrategyStats(s, i))
      .filter((s): s is StrategyExecutionStats => s !== null),
    source,
  };
}

// Deterministic preview/fallback generator when headless UI checks run without /api/runner/* routes.
export function buildPreviewRunnerState(
  strategies: Strategy[],
  markets: Market[],
  liveArmed: boolean,
  killed: boolean,
  _dataset: Dataset,
): { status: RunnerStatus; ledger: RunnerLedgerResponse } {
  const stratNames = strategies
    .map((s) => cleanStr(s?.strategy, ""))
    .filter((s) => s.length > 0);
  const s1 = stratNames[0] || "late-favourite-edge";
  const s2 = stratNames[1] || "mid-band-reversion";
  const s3 = stratNames[2] || "spread-squeeze-no";

  const openMarket = markets.find((m) => m && !m.result && m.ticker) ?? markets[0];
  const t1 = cleanStr(openMarket?.ticker, "KXBTC15M-PREVIEW-T00");
  const t2 = cleanStr(markets[1]?.ticker, "KXETH15M-PREVIEW-T00");
  const nowSec = 1764153000;

  const activeSlots: RunnerSlotState[] = [
    {
      slot_id: 1,
      strategy: s1,
      series: ["KXBTC15M", "KXETH15M"],
      max_contracts: 12,
      execution_mode: "maker",
      auto_pause_drawdown_dollars: 25,
      enabled: true,
      paused: false,
      pause_reason: null,
      realized_pnl_dollars: 4.18,
      drawdown_dollars: 0.9,
      quotes_posted: 14,
      fills: 9,
      open_quotes_count: 1,
      last_signal_epoch: nowSec - 45,
    },
    {
      slot_id: 2,
      strategy: s2,
      series: ["KXSOL15M", "KXXRP15M"],
      max_contracts: 10,
      execution_mode: "maker",
      auto_pause_drawdown_dollars: 20,
      enabled: true,
      paused: false,
      pause_reason: null,
      realized_pnl_dollars: 1.65,
      drawdown_dollars: 1.4,
      quotes_posted: 10,
      fills: 6,
      open_quotes_count: 1,
      last_signal_epoch: nowSec - 110,
    },
    {
      slot_id: 3,
      strategy: s3,
      series: ["KXDOGE15M"],
      max_contracts: 8,
      execution_mode: "taker",
      auto_pause_drawdown_dollars: 15,
      enabled: false,
      paused: false,
      pause_reason: null,
      realized_pnl_dollars: -0.82,
      drawdown_dollars: 1.82,
      quotes_posted: 4,
      fills: 2,
      open_quotes_count: 0,
      last_signal_epoch: nowSec - 620,
    },
  ];

  const openQuotes: VirtualOrLiveOrder[] = [
    {
      order_id: "shd-preview-1",
      slot_id: 1,
      strategy: s1,
      ticker: t1,
      series: "KXBTC15M",
      side: "yes",
      mode: "shadow",
      execution_mode: "maker",
      limit_cents: 68,
      contracts: 12,
      remaining_contracts: 12,
      queue_ahead_initial: 140,
      queue_ahead_remaining: 38,
      est_fill_eta_secs: 42,
      posted_epoch: nowSec - 75,
      close_epoch: nowSec + 285,
      status: "resting",
    },
    {
      order_id: "shd-preview-2",
      slot_id: 2,
      strategy: s2,
      ticker: t2,
      series: "KXETH15M",
      side: "no",
      mode: "shadow",
      execution_mode: "maker",
      limit_cents: 44,
      contracts: 10,
      remaining_contracts: 10,
      queue_ahead_initial: 90,
      queue_ahead_remaining: 54,
      est_fill_eta_secs: 95,
      posted_epoch: nowSec - 35,
      close_epoch: nowSec + 310,
      status: "resting",
    },
  ];

  const entries: LedgerEvent[] = [
    {
      id: "led-p-1",
      ts_epoch: nowSec - 35,
      ts_iso: new Date((nowSec - 35) * 1000).toISOString(),
      event: "QuotePosted",
      mode: "shadow",
      slot_id: 2,
      strategy: s2,
      ticker: t2,
      side: "no",
      price_cents: 44,
      limit_cents: 44,
      fill_cents: null,
      contracts: 10,
      fee_dollars: 0,
      pnl_dollars: null,
      slippage_cents: null,
      queue_wait_secs: null,
      reason: "Post-only limit resting at 44c",
    },
    {
      id: "led-p-2",
      ts_epoch: nowSec - 190,
      ts_iso: new Date((nowSec - 190) * 1000).toISOString(),
      event: "OrderFilled",
      mode: "shadow",
      slot_id: 1,
      strategy: s1,
      ticker: t1,
      side: "yes",
      price_cents: 71,
      limit_cents: 71,
      fill_cents: 71,
      contracts: 12,
      fee_dollars: 0.04,
      pnl_dollars: null,
      slippage_cents: 0.0,
      queue_wait_secs: 38,
      reason: "Queue depleted on resting bid",
    },
    {
      id: "led-p-3",
      ts_epoch: nowSec - 480,
      ts_iso: new Date((nowSec - 480) * 1000).toISOString(),
      event: "WindowSettled",
      mode: "shadow",
      slot_id: 1,
      strategy: s1,
      ticker: "KXBTC15M-PREVIEW-PREV",
      side: "yes",
      price_cents: 72,
      limit_cents: 72,
      fill_cents: 72,
      contracts: 12,
      fee_dollars: 0.04,
      pnl_dollars: 3.32,
      slippage_cents: 0.0,
      queue_wait_secs: 29,
      result: "yes",
      reason: "Settled YES",
    },
    {
      id: "led-p-4",
      ts_epoch: nowSec - 940,
      ts_iso: new Date((nowSec - 940) * 1000).toISOString(),
      event: "QuoteCanceled",
      mode: "shadow",
      slot_id: 2,
      strategy: s2,
      ticker: "KXSOL15M-PREVIEW-PREV",
      side: "no",
      price_cents: 42,
      limit_cents: 42,
      fill_cents: null,
      contracts: 10,
      fee_dollars: 0,
      pnl_dollars: 0,
      slippage_cents: null,
      queue_wait_secs: null,
      reason: "Pre-expiry cancel (30s guard)",
    },
  ];

  const summary: StrategyExecutionStats[] = [
    {
      strategy: s1,
      mode: "shadow",
      quotes_posted: 14,
      fills: 9,
      canceled: 4,
      windows_settled: 9,
      fill_rate_pct: 64.3,
      wins: 7,
      losses: 2,
      realized_pnl_dollars: 4.18,
      total_fees_dollars: 0.36,
      avg_fill_slippage_cents: 0.1,
      avg_queue_wait_secs: 34,
      paused_count: 0,
    },
    {
      strategy: s2,
      mode: "shadow",
      quotes_posted: 10,
      fills: 6,
      canceled: 3,
      windows_settled: 6,
      fill_rate_pct: 60.0,
      wins: 4,
      losses: 2,
      realized_pnl_dollars: 1.65,
      total_fees_dollars: 0.24,
      avg_fill_slippage_cents: 0.2,
      avg_queue_wait_secs: 51,
      paused_count: 0,
    },
    {
      strategy: s3,
      mode: "shadow",
      quotes_posted: 4,
      fills: 2,
      canceled: 2,
      windows_settled: 2,
      fill_rate_pct: 50.0,
      wins: 1,
      losses: 1,
      realized_pnl_dollars: -0.82,
      total_fees_dollars: 0.18,
      avg_fill_slippage_cents: 1.0,
      avg_queue_wait_secs: 4,
      paused_count: 0,
    },
  ];

  return {
    status: {
      mode: killed ? "stopped" : "shadow",
      live_armed: liveArmed,
      killed,
      tick_interval_ms: 2000,
      cancel_before_close_secs: 30,
      bankroll_dollars: 250,
      active_slots: activeSlots,
      open_quotes: killed ? [] : openQuotes,
      stats: {
        quotes_posted: 28,
        fills: 17,
        fill_rate_pct: 60.7,
        realized_pnl_dollars: 5.01,
        unrealized_exposure_dollars: killed ? 0 : 12.56,
        avg_fill_slippage_cents: 0.2,
        avg_queue_wait_secs: 36,
      },
      last_error: null,
      source: "preview",
    },
    ledger: {
      entries,
      summary_by_strategy: summary,
      source: "preview",
    },
  };
}

export async function fetchRunnerStatus(
  home: Home | null,
  fallback?: { strategies: Strategy[]; markets: Market[]; liveArmed: boolean; killed: boolean; dataset: Dataset },
): Promise<RunnerStatus> {
  if (supportsShadowRunner(home)) {
    try {
      const r = await fetch("/api/runner/status");
      if (r.ok) {
        return normalizeRunnerStatus(await r.json(), "server");
      }
    } catch {
      /* fallback to deterministic preview */
    }
  }
  const preview = buildPreviewRunnerState(
    fallback?.strategies ?? [],
    fallback?.markets ?? [],
    fallback?.liveArmed ?? false,
    fallback?.killed ?? false,
    fallback?.dataset ?? "live",
  );
  return preview.status;
}

export async function fetchRunnerLedger(
  home: Home | null,
  opts: { limit?: number; strategy?: string; mode?: "shadow" | "live" | "all" } = {},
  fallback?: { strategies: Strategy[]; markets: Market[]; liveArmed: boolean; killed: boolean; dataset: Dataset },
): Promise<RunnerLedgerResponse> {
  if (supportsShadowRunner(home)) {
    try {
      const qs = new URLSearchParams();
      qs.set("limit", String(opts.limit ?? 200));
      if (opts.strategy && opts.strategy !== "all") qs.set("strategy", opts.strategy);
      if (opts.mode && opts.mode !== "all") qs.set("mode", opts.mode);
      const r = await fetch(`/api/runner/ledger?${qs.toString()}`);
      if (r.ok) {
        return normalizeRunnerLedger(await r.json(), "server");
      }
    } catch {
      /* fallback to deterministic preview */
    }
  }
  const preview = buildPreviewRunnerState(
    fallback?.strategies ?? [],
    fallback?.markets ?? [],
    fallback?.liveArmed ?? false,
    fallback?.killed ?? false,
    fallback?.dataset ?? "live",
  );
  const filtered = preview.ledger.entries.filter((e) => {
    if (opts.strategy && opts.strategy !== "all" && e.strategy !== opts.strategy) return false;
    if (opts.mode && opts.mode !== "all" && e.mode !== opts.mode) return false;
    return true;
  });
  return {
    entries: filtered.slice(0, opts.limit ?? 200),
    summary_by_strategy: preview.ledger.summary_by_strategy,
    source: "preview",
  };
}

export class RunnerApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "RunnerApiError";
    this.status = status;
  }
}

async function parseRunnerError(r: Response, fallback: string): Promise<string> {
  const body = (await r.json().catch(() => ({}))) as { error?: string; message?: string };
  return body.error || body.message || fallback;
}

export async function saveRunnerConfig(
  payload: RunnerConfigPayload,
  home: Home | null,
): Promise<RunnerStatus> {
  if (!supportsShadowRunner(home)) {
    return normalizeRunnerStatus(
      {
        mode: "stopped",
        live_armed: false,
        killed: false,
        tick_interval_ms: 2000,
        cancel_before_close_secs: payload.cancel_before_close_secs,
        bankroll_dollars: payload.bankroll_dollars,
        active_slots: payload.slots,
        open_quotes: [],
        stats: {
          quotes_posted: 0,
          fills: 0,
          fill_rate_pct: 0,
          realized_pnl_dollars: 0,
          unrealized_exposure_dollars: 0,
          avg_fill_slippage_cents: 0,
          avg_queue_wait_secs: 0,
        },
      },
      "preview",
    );
  }
  const r = await fetch("/api/runner/config", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify(payload),
  });
  if (!r.ok) {
    throw new RunnerApiError(r.status, await parseRunnerError(r, `Runner config answered ${r.status}`));
  }
  return normalizeRunnerStatus(unwrapRunnerStatus(await r.json()), "server");
}

function unwrapRunnerStatus(raw: unknown): unknown {
  if (raw && typeof raw === "object" && "status" in raw) {
    const nested = (raw as { status?: unknown }).status;
    if (nested && typeof nested === "object") return nested;
  }
  return raw;
}

export async function startRunner(
  payload: RunnerStartPayload,
  home: Home | null,
  currentStatus?: RunnerStatus,
): Promise<RunnerStatus> {
  if (payload.mode === "live_auto" && !payload.confirm_auto_live) {
    throw new RunnerApiError(400, "Live Auto requires explicit confirmation.");
  }
  if (!supportsShadowRunner(home)) {
    if (payload.mode === "live_auto" && (!currentStatus?.live_armed || currentStatus?.killed)) {
      throw new RunnerApiError(403, "Live Auto is blocked until live trading is armed and kill switch is clear.");
    }
    const base = currentStatus ?? normalizeRunnerStatus({}, "preview");
    return {
      ...base,
      mode: payload.mode,
      source: "preview",
    };
  }
  const r = await fetch("/api/runner/start", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify(payload),
  });
  if (!r.ok) {
    throw new RunnerApiError(r.status, await parseRunnerError(r, `Runner start answered ${r.status}`));
  }
  return normalizeRunnerStatus(unwrapRunnerStatus(await r.json()), "server");
}

export async function stopRunner(
  payload: RunnerStopPayload,
  home: Home | null,
  currentStatus?: RunnerStatus,
): Promise<RunnerStatus> {
  if (!supportsShadowRunner(home)) {
    const base = currentStatus ?? normalizeRunnerStatus({}, "preview");
    return {
      ...base,
      mode: "stopped",
      open_quotes: payload.cancel_resting === false ? base.open_quotes : [],
      source: "preview",
    };
  }
  const r = await fetch("/api/runner/stop", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify(payload),
  });
  if (!r.ok) {
    throw new RunnerApiError(r.status, await parseRunnerError(r, `Runner stop answered ${r.status}`));
  }
  return normalizeRunnerStatus(unwrapRunnerStatus(await r.json()), "server");
}

export function subscribeRunnerStream(
  enabled: boolean,
  onEvent: (ev: RunnerSseEvent) => void,
  onReconnect?: () => void,
): () => void {
  if (!enabled || typeof EventSource === "undefined") {
    return () => {};
  }
  let closed = false;
  let attempt = 0;
  let reconnectTimer: number | null = null;
  let es: EventSource | null = null;

  const handleRunnerEvent = (msg: MessageEvent<string>) => {
    if (closed) return;
    try {
      const parsed = JSON.parse(msg.data) as Record<string, unknown>;
      const kind = cleanStr(parsed.kind ?? parsed.event ?? parsed.type, "quote_posted");
      onEvent({
        kind,
        mode: typeof parsed.mode === "string" ? normalizeMode(parsed.mode) : undefined,
        slot_id: isNum(parsed.slot_id) ? parsed.slot_id : undefined,
        strategy: typeof parsed.strategy === "string" ? parsed.strategy : undefined,
        ticker: typeof parsed.ticker === "string" ? parsed.ticker : undefined,
        ts_epoch: isNum(parsed.ts_epoch) ? parsed.ts_epoch : undefined,
        detail: typeof parsed.detail === "string" ? parsed.detail : undefined,
        raw: parsed,
      });
    } catch {
      onEvent({ kind: "runner_update", raw: {} });
    }
  };

  const connect = () => {
    if (closed) return;
    const source = new EventSource("/api/tape/stream");
    es = source;
    source.onopen = () => {
      if (closed) return;
      const wasReconnect = attempt > 0;
      attempt = 0;
      if (wasReconnect) onReconnect?.();
    };
    source.addEventListener("runner", handleRunnerEvent as EventListener);
    source.onerror = () => {
      source.close();
      if (es === source) es = null;
      if (closed) return;
      const delayMs = Math.min(15000, 1000 * 2 ** Math.min(attempt, 4));
      attempt += 1;
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delayMs);
    };
  };

  connect();

  return () => {
    closed = true;
    if (reconnectTimer !== null) {
      window.clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    if (es) {
      es.close();
      es = null;
    }
  };
}
