// OpenBell E2 Live L2 Orderbook client, SSE stream merger, history reader, and client-side book-walk VWAP calculator.
// Aligned with Zone A endpoints:
//   GET  /api/orderbook?ticker=<TICKER>
//   GET  /api/orderbook/history?ticker=<TICKER>&limit=<LIMIT>
//   GET  /api/orderbook/stream-status
//   POST /api/orderbook/subscribe
//   SSE  /api/tape/stream (`event: orderbook`)
import { useCallback, useEffect, useRef, useState } from "react";
import { cleanStr, isNum } from "@/lib/desk";

export type OrderbookSource = "ws" | "rest" | "empty";
export type OrderbookStreamMode = "ws" | "rest" | "offline";

export type OrderbookLevel = {
  price_cents: number;
  qty: number;
  cumulative_qty: number;
};

export type OrderbookSnapshot = {
  ok?: boolean;
  ticker: string;
  yes_bids: OrderbookLevel[];
  no_bids: OrderbookLevel[];
  yes_asks: OrderbookLevel[];
  no_asks: OrderbookLevel[];
  best_yes_bid: number | null;
  best_yes_ask: number | null;
  spread_cents: number | null;
  mid_cents: number | null;
  vwap_10: number | null;
  last_seq: number;
  updated_ms: number;
  source: OrderbookSource;
  stale: boolean;
};

export type OrderbookStreamStatus = {
  ws_connected: boolean;
  mode: OrderbookStreamMode;
  subscribed_tickers: string[];
  active_books: number;
  stale_books: number;
  last_error: string | null;
  delta_count: number;
  resync_count: number;
  auto_subscribed_count: number;
  active_l2_tickers: string[];
  last_trade_ms: number | null;
  tape_sync_rate_hz: number;
};

export type OrderbookHistoryResponse = {
  ok: boolean;
  ticker: string;
  records: OrderbookSnapshot[];
};

export type OrderbookSubscribeResult = {
  ok: boolean;
  subscribed_tickers: string[];
};

export type BookWalkResult = {
  vwapCents: number | null;
  slippageCents: number | null;
  fillableQty: number;
  levelsTouched: number;
};

const MUTATION_HEADERS: HeadersInit = {
  "Content-Type": "application/json",
  "X-OpenBell-CSRF": "1",
};

function pickNum(o: Record<string, unknown>, keys: string[]): number | null {
  for (const k of keys) {
    if (isNum(o[k])) return o[k] as number;
  }
  return null;
}

function normalizeSource(raw: unknown, hasLevels: boolean): OrderbookSource {
  const s = typeof raw === "string" ? raw.trim().toLowerCase() : "";
  if (s === "ws" || s === "kalshi_ws") return "ws";
  if (s === "rest" || s === "rest_l2") return "rest";
  if (s === "empty") return "empty";
  return hasLevels ? "rest" : "empty";
}

function normalizeStreamMode(raw: unknown, wsConnected: boolean): OrderbookStreamMode {
  const s = typeof raw === "string" ? raw.trim().toLowerCase() : "";
  if (s === "ws" || s === "kalshi_ws") return "ws";
  if (s === "rest" || s === "rest_l2" || s === "rest_l2_poll") return "rest";
  if (s === "offline" || s === "idle") return "offline";
  return wsConnected ? "ws" : "offline";
}

function normalizeLevels(raw: unknown, direction: "asc" | "desc"): OrderbookLevel[] {
  if (!Array.isArray(raw)) return [];
  const parsed: OrderbookLevel[] = [];
  for (const item of raw) {
    if (Array.isArray(item) && item.length >= 2 && isNum(item[0]) && isNum(item[1])) {
      const price = Math.round(item[0]);
      const qty = Math.max(0, Math.round(item[1]));
      if (price >= 1 && price <= 99 && qty > 0) {
        parsed.push({ price_cents: price, qty, cumulative_qty: 0 });
      }
      continue;
    }
    if (!item || typeof item !== "object") continue;
    const o = item as Record<string, unknown>;
    const priceRaw = pickNum(o, ["price_cents", "price"]);
    const qtyRaw = pickNum(o, ["qty", "contracts", "size", "quantity"]);
    const cumRaw = pickNum(o, ["cumulative_qty", "cum_contracts", "cum_qty"]);
    if (priceRaw === null || qtyRaw === null) continue;
    const price = Math.round(priceRaw);
    const qty = Math.max(0, Math.round(qtyRaw));
    if (price < 1 || price > 99 || qty <= 0) continue;
    parsed.push({
      price_cents: price,
      qty,
      cumulative_qty: cumRaw !== null ? Math.max(qty, Math.round(cumRaw)) : 0,
    });
  }

  parsed.sort((a, b) =>
    direction === "desc" ? b.price_cents - a.price_cents : a.price_cents - b.price_cents,
  );

  let running = 0;
  return parsed.map((lvl) => {
    running += lvl.qty;
    return {
      price_cents: lvl.price_cents,
      qty: lvl.qty,
      cumulative_qty: lvl.cumulative_qty > 0 ? lvl.cumulative_qty : running,
    };
  });
}

function deriveComplementLevels(levels: OrderbookLevel[], direction: "asc" | "desc"): OrderbookLevel[] {
  if (levels.length === 0) return [];
  const mapped = levels
    .map((lvl) => ({
      price_cents: 100 - lvl.price_cents,
      qty: lvl.qty,
      cumulative_qty: 0,
    }))
    .filter((lvl) => lvl.price_cents >= 1 && lvl.price_cents <= 99 && lvl.qty > 0);
  mapped.sort((a, b) =>
    direction === "desc" ? b.price_cents - a.price_cents : a.price_cents - b.price_cents,
  );
  let running = 0;
  return mapped.map((lvl) => {
    running += lvl.qty;
    return { ...lvl, cumulative_qty: running };
  });
}

export function emptyOrderbookSnapshot(ticker = ""): OrderbookSnapshot {
  return {
    ok: false,
    ticker: ticker.trim().toUpperCase(),
    yes_bids: [],
    no_bids: [],
    yes_asks: [],
    no_asks: [],
    best_yes_bid: null,
    best_yes_ask: null,
    spread_cents: null,
    mid_cents: null,
    vwap_10: null,
    last_seq: 0,
    updated_ms: 0,
    source: "empty",
    stale: false,
  };
}

export function defaultOrderbookStreamStatus(): OrderbookStreamStatus {
  return {
    ws_connected: false,
    mode: "offline",
    subscribed_tickers: [],
    active_books: 0,
    stale_books: 0,
    last_error: null,
    delta_count: 0,
    resync_count: 0,
    auto_subscribed_count: 0,
    active_l2_tickers: [],
    last_trade_ms: null,
    tape_sync_rate_hz: 0,
  };
}

function safeEpochMs(raw: unknown): number {
  if (isNum(raw)) {
    const rounded = Math.round(raw);
    if (rounded > 0 && rounded <= 8_640_000_000_000_000) return rounded;
    return 0;
  }
  if (typeof raw === "string" && raw.trim().length > 0) {
    const parsed = Date.parse(raw.trim());
    if (Number.isFinite(parsed) && parsed > 0 && parsed <= 8_640_000_000_000_000) {
      return Math.round(parsed);
    }
  }
  return 0;
}

export function normalizeOrderbookSnapshot(raw: unknown, fallbackTicker = ""): OrderbookSnapshot {
  if (!raw || typeof raw !== "object") {
    return emptyOrderbookSnapshot(fallbackTicker);
  }
  const o = raw as Record<string, unknown>;
  const ticker = cleanStr(o.ticker, fallbackTicker).toUpperCase();
  const yesBids = normalizeLevels(o.yes_bids, "desc");
  const noBids = normalizeLevels(o.no_bids, "desc");
  const rawYesAsks = normalizeLevels(o.yes_asks, "asc");
  const rawNoAsks = normalizeLevels(o.no_asks, "asc");
  const yesAsks = rawYesAsks.length > 0 ? rawYesAsks : deriveComplementLevels(noBids, "asc");
  const noAsks = rawNoAsks.length > 0 ? rawNoAsks : deriveComplementLevels(yesBids, "asc");

  const bestYesBid =
    pickNum(o, ["best_yes_bid", "best_yes_bid_cents"]) ?? (yesBids[0]?.price_cents ?? null);
  const bestYesAsk =
    pickNum(o, ["best_yes_ask", "best_yes_ask_cents"]) ?? (yesAsks[0]?.price_cents ?? null);
  const spreadCents =
    pickNum(o, ["spread_cents"]) ??
    (bestYesBid !== null && bestYesAsk !== null ? bestYesAsk - bestYesBid : null);
  const midCents =
    pickNum(o, ["mid_cents"]) ??
    (bestYesBid !== null && bestYesAsk !== null
      ? Number(((bestYesBid + bestYesAsk) / 2).toFixed(2))
      : null);

  const hasLevels = yesBids.length > 0 || yesAsks.length > 0 || noBids.length > 0 || noAsks.length > 0;
  const vwap10 = pickNum(o, ["vwap_10"]) ?? walkBookLevels(yesAsks, 10).vwapCents;
  const updatedMs =
    safeEpochMs(o.updated_ms) || safeEpochMs(o.ts_ms) || safeEpochMs(o.updated_at) || safeEpochMs(o.ts);

  return {
    ok: typeof o.ok === "boolean" ? o.ok : hasLevels,
    ticker,
    yes_bids: yesBids,
    no_bids: noBids,
    yes_asks: yesAsks,
    no_asks: noAsks,
    best_yes_bid: bestYesBid !== null ? Math.round(bestYesBid) : null,
    best_yes_ask: bestYesAsk !== null ? Math.round(bestYesAsk) : null,
    spread_cents: spreadCents !== null ? Math.round(spreadCents) : null,
    mid_cents: midCents !== null ? Number(midCents.toFixed(2)) : null,
    vwap_10: vwap10 !== null ? Number(vwap10.toFixed(2)) : null,
    last_seq: Math.max(0, Math.round(pickNum(o, ["last_seq", "seq"]) ?? 0)),
    updated_ms: updatedMs,
    source: normalizeSource(o.source, hasLevels),
    stale: Boolean(o.stale),
  };
}

export function normalizeOrderbookStreamStatus(raw: unknown): OrderbookStreamStatus {
  if (!raw || typeof raw !== "object") return defaultOrderbookStreamStatus();
  const o = raw as Record<string, unknown>;
  const wsConnected = Boolean(o.ws_connected ?? o.connected);
  const subs = Array.isArray(o.subscribed_tickers)
    ? [
        ...new Set(
          o.subscribed_tickers
            .map((t) => (typeof t === "string" ? t.trim().toUpperCase() : ""))
            .filter((t) => t.length > 0),
        ),
      ]
    : [];
  const activeL2Tickers = Array.isArray(o.active_l2_tickers)
    ? [
        ...new Set(
          o.active_l2_tickers
            .map((t) => (typeof t === "string" ? t.trim().toUpperCase() : ""))
            .filter((t) => t.length > 0),
        ),
      ]
    : subs;
  const rawLastTradeMs = safeEpochMs(o.last_trade_ms);
  const lastTradeMs = rawLastTradeMs > 0 ? rawLastTradeMs : null;
  const rawSyncRate = pickNum(o, ["tape_sync_rate_hz"]);
  const tapeSyncRateHz = rawSyncRate !== null && rawSyncRate >= 0 ? Number(rawSyncRate.toFixed(2)) : 0;
  return {
    ws_connected: wsConnected,
    mode: normalizeStreamMode(o.mode ?? o.transport, wsConnected),
    subscribed_tickers: subs,
    active_books: Math.max(0, Math.round(pickNum(o, ["active_books"]) ?? subs.length)),
    stale_books: Math.max(0, Math.round(pickNum(o, ["stale_books"]) ?? 0)),
    last_error: typeof o.last_error === "string" && o.last_error.trim() ? o.last_error.trim() : null,
    delta_count: Math.max(0, Math.round(pickNum(o, ["delta_count", "deltas_received"]) ?? 0)),
    resync_count: Math.max(0, Math.round(pickNum(o, ["resync_count", "snapshots_recorded"]) ?? 0)),
    auto_subscribed_count: Math.max(0, Math.round(pickNum(o, ["auto_subscribed_count"]) ?? 0)),
    active_l2_tickers: activeL2Tickers,
    last_trade_ms: lastTradeMs,
    tape_sync_rate_hz: tapeSyncRateHz,
  };
}

export function walkBookLevels(levels: OrderbookLevel[], qty: number): BookWalkResult {
  if (!Array.isArray(levels) || levels.length === 0 || !Number.isFinite(qty) || qty <= 0) {
    return { vwapCents: null, slippageCents: null, fillableQty: 0, levelsTouched: 0 };
  }
  const target = Math.floor(qty);
  if (target <= 0) {
    return { vwapCents: null, slippageCents: null, fillableQty: 0, levelsTouched: 0 };
  }

  let remaining = target;
  let totalCost = 0;
  let fillableQty = 0;
  let levelsTouched = 0;
  let bestPrice: number | null = null;

  for (const lvl of levels) {
    if (!lvl || !Number.isFinite(lvl.price_cents) || !Number.isFinite(lvl.qty) || lvl.qty <= 0) {
      continue;
    }
    if (bestPrice === null) bestPrice = lvl.price_cents;
    const take = Math.min(remaining, Math.floor(lvl.qty));
    if (take <= 0) continue;
    totalCost += take * lvl.price_cents;
    fillableQty += take;
    remaining -= take;
    levelsTouched += 1;
    if (remaining <= 0) break;
  }

  if (fillableQty <= 0 || bestPrice === null) {
    return { vwapCents: null, slippageCents: null, fillableQty: 0, levelsTouched: 0 };
  }

  const vwapCents = Number((totalCost / fillableQty).toFixed(2));
  const slippageCents = Number(Math.abs(vwapCents - bestPrice).toFixed(2));
  return {
    vwapCents,
    slippageCents,
    fillableQty,
    levelsTouched,
  };
}

export function deriveSideBookLevels(
  snapshot: OrderbookSnapshot | null,
  side: "yes" | "no" = "yes",
  action: "buy" | "sell" = "buy",
): OrderbookLevel[] {
  if (!snapshot) return [];
  if (side === "yes") {
    return action === "buy" ? snapshot.yes_asks : snapshot.yes_bids;
  }
  if (action === "buy") {
    return snapshot.no_asks.length > 0
      ? snapshot.no_asks
      : deriveComplementLevels(snapshot.yes_bids, "asc");
  }
  return snapshot.no_bids.length > 0
    ? snapshot.no_bids
    : deriveComplementLevels(snapshot.yes_asks, "desc");
}

export function computeMicropriceCents(snapshot: OrderbookSnapshot | null): number | null {
  if (!snapshot) return null;
  const topBid = snapshot.yes_bids[0];
  const topAsk = snapshot.yes_asks[0];
  if (topBid && topAsk && topBid.qty > 0 && topAsk.qty > 0) {
    const weighted =
      (topBid.price_cents * topAsk.qty + topAsk.price_cents * topBid.qty) /
      (topBid.qty + topAsk.qty);
    return Number(weighted.toFixed(1));
  }
  if (snapshot.mid_cents !== null) return Number(snapshot.mid_cents.toFixed(1));
  if (snapshot.best_yes_bid !== null && snapshot.best_yes_ask !== null) {
    return Number(((snapshot.best_yes_bid + snapshot.best_yes_ask) / 2).toFixed(1));
  }
  return null;
}

export async function fetchOrderbook(ticker: string): Promise<OrderbookSnapshot> {
  const clean = ticker.trim().toUpperCase();
  if (!clean) return emptyOrderbookSnapshot("");
  const r = await fetch(`/api/orderbook?ticker=${encodeURIComponent(clean)}`);
  if (!r.ok) {
    throw new Error(`Orderbook request answered ${r.status}`);
  }
  return normalizeOrderbookSnapshot(await r.json(), clean);
}

export async function fetchOrderbookStreamStatus(): Promise<OrderbookStreamStatus> {
  const r = await fetch("/api/orderbook/stream-status");
  if (!r.ok) {
    throw new Error(`Orderbook stream-status answered ${r.status}`);
  }
  return normalizeOrderbookStreamStatus(await r.json());
}

export async function fetchOrderbookHistory(
  ticker: string,
  limit = 20,
  signal?: AbortSignal,
): Promise<OrderbookHistoryResponse> {
  const clean = ticker.trim().toUpperCase();
  if (!clean) return { ok: false, ticker: "", records: [] };
  const safeLimit = Number.isFinite(limit) ? Math.max(1, Math.min(200, Math.round(limit))) : 20;
  const qs = new URLSearchParams({
    ticker: clean,
    limit: String(safeLimit),
  });
  const r = await fetch(
    `/api/orderbook/history?${qs.toString()}`,
    signal ? { signal } : undefined,
  );
  if (!r.ok) {
    throw new Error(`Orderbook history request answered ${r.status}`);
  }
  const raw = (await r.json().catch(() => null)) as Record<string, unknown> | null;
  const rawList = Array.isArray(raw?.records)
    ? raw.records
    : Array.isArray(raw?.snapshots)
      ? raw.snapshots
      : Array.isArray(raw?.history)
        ? raw.history
        : [];
  const records = rawList
    .filter((item): item is Record<string, unknown> => !!item && typeof item === "object")
    .map((item) => normalizeOrderbookSnapshot(item, clean))
    .filter(
      (snap) =>
        snap.updated_ms > 0 ||
        snap.last_seq > 0 ||
        snap.yes_bids.length > 0 ||
        snap.yes_asks.length > 0,
    )
    .sort((a, b) => a.updated_ms - b.updated_ms || a.last_seq - b.last_seq);
  return {
    ok: typeof raw?.ok === "boolean" ? raw.ok : true,
    ticker: cleanStr(raw?.ticker, clean).toUpperCase(),
    records,
  };
}

export async function subscribeOrderbook(
  ticker: string | string[],
): Promise<OrderbookSubscribeResult> {
  const body = Array.isArray(ticker)
    ? { tickers: ticker.map((t) => t.trim().toUpperCase()).filter(Boolean) }
    : { ticker: ticker.trim().toUpperCase() };
  const r = await fetch("/api/orderbook/subscribe", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    throw new Error(`Orderbook subscribe answered ${r.status}`);
  }
  const j = (await r.json()) as Record<string, unknown>;
  const subs = Array.isArray(j?.subscribed_tickers)
    ? j.subscribed_tickers.map((t) => String(t).trim().toUpperCase()).filter(Boolean)
    : [];
  return {
    ok: Boolean(j?.ok ?? true),
    subscribed_tickers: subs,
  };
}

export function isOrderbookCrossed(snapshot: OrderbookSnapshot | null): boolean {
  if (!snapshot) return false;
  if (
    snapshot.best_yes_bid !== null &&
    snapshot.best_yes_ask !== null &&
    snapshot.best_yes_bid > snapshot.best_yes_ask
  ) {
    return true;
  }
  return snapshot.spread_cents !== null && snapshot.spread_cents < 0;
}

export function subscribeOrderbookStream(
  enabled: boolean,
  onSnapshot: (snap: OrderbookSnapshot) => void,
  opts?: { onReconnect?: () => void; onError?: (err: string) => void },
): () => void {
  if (!enabled || typeof EventSource === "undefined") {
    return () => {};
  }
  let closed = false;
  let attempt = 0;
  let reconnectTimer: number | null = null;
  let es: EventSource | null = null;

  const handleOrderbookEvent = (msg: MessageEvent<string>) => {
    if (closed) return;
    try {
      const parsed = JSON.parse(msg.data) as Record<string, unknown>;
      const payload =
        parsed.snapshot && typeof parsed.snapshot === "object" ? parsed.snapshot : parsed;
      const snap = normalizeOrderbookSnapshot(payload);
      if (snap.ticker) {
        onSnapshot(snap);
      }
    } catch {
      /* ignore malformed frame */
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
      if (wasReconnect) {
        opts?.onReconnect?.();
      }
    };

    source.addEventListener("orderbook", handleOrderbookEvent as EventListener);
    source.onerror = () => {
      source.close();
      if (es === source) es = null;
      if (closed) return;
      const delayMs = Math.min(15000, 1000 * 2 ** Math.min(attempt, 4));
      attempt += 1;
      opts?.onError?.(`Orderbook SSE interrupted; retrying in ${Math.round(delayMs / 1000)}s`);
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

export function useOrderbook(
  ticker: string,
  options: { enabled?: boolean; pollMs?: number; sseEnabled?: boolean } = {},
) {
  const { enabled = true, pollMs = 4000, sseEnabled = true } = options;
  const cleanTicker = ticker.trim().toUpperCase();
  const [snapshot, setSnapshot] = useState<OrderbookSnapshot | null>(null);
  const [streamStatus, setStreamStatus] = useState<OrderbookStreamStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const seqRef = useRef(0);

  const refresh = useCallback(
    async (withResubscribe = false) => {
      if (!enabled || !cleanTicker) return;
      const reqId = ++seqRef.current;
      try {
        if (withResubscribe) {
          await subscribeOrderbook(cleanTicker).catch(() => null);
        }
        const [book, st] = await Promise.all([
          fetchOrderbook(cleanTicker),
          fetchOrderbookStreamStatus().catch(() => null),
        ]);
        if (reqId !== seqRef.current) return;
        setSnapshot(book);
        if (st) setStreamStatus(st);
        setError(null);
      } catch (e) {
        if (reqId !== seqRef.current) return;
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (reqId === seqRef.current) setLoading(false);
      }
    },
    [enabled, cleanTicker],
  );

  useEffect(() => {
    if (!enabled || !cleanTicker) {
      setSnapshot(null);
      setStreamStatus(null);
      setError(null);
      setLoading(false);
      return;
    }
    let alive = true;
    setLoading(true);
    setError(null);
    void subscribeOrderbook(cleanTicker).catch(() => null);
    void refresh(false);

    const pollId = window.setInterval(() => {
      if (alive) void refresh(false);
    }, Math.max(1000, pollMs));

    return () => {
      alive = false;
      window.clearInterval(pollId);
    };
  }, [enabled, cleanTicker, pollMs, refresh]);

  useEffect(() => {
    if (!enabled || !cleanTicker || !sseEnabled) return;
    return subscribeOrderbookStream(
      true,
      (snap) => {
        if (snap.ticker === cleanTicker) {
          setSnapshot((prev) => {
            if (
              prev &&
              prev.ticker === snap.ticker &&
              prev.last_seq > 0 &&
              snap.last_seq > 0 &&
              snap.last_seq < prev.last_seq &&
              snap.updated_ms <= prev.updated_ms
            ) {
              return prev;
            }
            return snap;
          });
          setError(null);
          setLoading(false);
        }
      },
      {
        onReconnect: () => {
          void refresh(true);
        },
      },
    );
  }, [enabled, cleanTicker, sseEnabled, refresh]);

  const manualRefresh = useCallback(() => {
    void refresh(true);
  }, [refresh]);

  return {
    snapshot,
    streamStatus,
    loading,
    error,
    refresh: manualRefresh,
  };
}

export function useOrderbookStreamStatus(
  options: { enabled?: boolean; pollMs?: number; sseEnabled?: boolean } = {},
) {
  const { enabled = true, pollMs = 4000, sseEnabled = true } = options;
  const [streamStatus, setStreamStatus] = useState<OrderbookStreamStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const seqRef = useRef(0);
  const lastSseRefreshRef = useRef(0);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    const reqId = ++seqRef.current;
    try {
      const st = await fetchOrderbookStreamStatus();
      if (reqId !== seqRef.current) return;
      setStreamStatus(st);
    } catch {
      /* keep previous status on transient error */
    } finally {
      if (reqId === seqRef.current) setLoading(false);
    }
  }, [enabled]);

  useEffect(() => {
    if (!enabled) {
      setStreamStatus(null);
      setLoading(false);
      return;
    }
    let alive = true;
    setLoading(true);
    void refresh();

    const pollId = window.setInterval(() => {
      if (alive) void refresh();
    }, Math.max(1000, pollMs));

    return () => {
      alive = false;
      window.clearInterval(pollId);
    };
  }, [enabled, pollMs, refresh]);

  useEffect(() => {
    if (!enabled || !sseEnabled) return;
    return subscribeOrderbookStream(
      true,
      () => {
        const now = Date.now();
        if (now - lastSseRefreshRef.current >= 1200) {
          lastSseRefreshRef.current = now;
          void refresh();
        }
      },
      {
        onReconnect: () => {
          void refresh();
        },
      },
    );
  }, [enabled, sseEnabled, refresh]);

  return {
    streamStatus,
    loading,
    refresh,
  };
}
