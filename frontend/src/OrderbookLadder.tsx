// OpenBell E2 Institutional L2 Depth-of-Market (DOM) Ladder component.
// Renders paired YES Bids & YES Asks with proportional horizontal depth bars,
// cumulative depth, spread & microprice header, WS/REST live/stale status pill,
// clickable price rows, live book-walk VWAP + slippage estimate, and L2 snapshot history scrubber.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Activity, AlertTriangle, ChevronLeft, ChevronRight, History, Layers, RefreshCw } from "lucide-react";
import { Badge, Button, StatusPill } from "@/components/ui";
import { timeUtcFullLabel } from "@/lib/desk";
import {
  type OrderbookLevel,
  type OrderbookSnapshot,
  type OrderbookStreamStatus,
  computeMicropriceCents,
  deriveSideBookLevels,
  fetchOrderbookHistory,
  isOrderbookCrossed,
  walkBookLevels,
} from "@/lib/orderbook";

export type OrderbookLadderProps = {
  ticker: string;
  snapshot: OrderbookSnapshot | null;
  streamStatus?: OrderbookStreamStatus | null;
  orderQty?: number;
  side?: "yes" | "no";
  action?: "buy" | "sell";
  selectedPriceCents?: number | null;
  loading?: boolean;
  error?: string | null;
  maxLevels?: number;
  historyLimit?: number;
  onSelectPrice?: (priceCents: number) => void;
  onRefresh?: () => void;
  className?: string;
};

function statusBadgeInfo(
  snapshot: OrderbookSnapshot | null,
  streamStatus?: OrderbookStreamStatus | null,
  inspectingHistory = false,
): { tone: "success" | "info" | "warning" | "neutral"; label: string } {
  if (inspectingHistory && snapshot) {
    return { tone: "info", label: `History #${snapshot.last_seq}` };
  }
  const mode = streamStatus?.mode ?? (snapshot?.source === "ws" ? "ws" : snapshot?.source === "rest" ? "rest" : "offline");
  if (snapshot?.stale) {
    return {
      tone: "warning",
      label: mode === "ws" ? "WS Stale" : mode === "rest" ? "REST Stale" : "Book Stale",
    };
  }
  if (mode === "ws" || snapshot?.source === "ws") {
    return { tone: "success", label: "WS Live" };
  }
  if (mode === "rest" || snapshot?.source === "rest") {
    return { tone: "info", label: "REST L2" };
  }
  return { tone: "neutral", label: "Offline" };
}

function LadderSideColumn({
  title,
  sideKind,
  levels,
  maxCumQty,
  ticketSide,
  selectedYesPriceCents,
  onSelectYesLevel,
}: {
  title: string;
  sideKind: "bid" | "ask";
  levels: OrderbookLevel[];
  maxCumQty: number;
  ticketSide: "yes" | "no";
  selectedYesPriceCents?: number | null;
  onSelectYesLevel?: (yesPriceCents: number) => void;
}) {
  const isBid = sideKind === "bid";
  return (
    <div className="min-w-0 rounded-lg border border-hairline bg-board/40 p-2">
      <div className="mb-1.5 grid grid-cols-3 gap-2 border-b border-hairline px-2 pb-1.5 text-xs font-medium text-ink-3">
        <span>{title}</span>
        <span className="text-right">Size</span>
        <span className="text-right">Cum</span>
      </div>
      {levels.length === 0 ? (
        <div className="px-2 py-4 text-center text-xs text-ink-3">No resting {isBid ? "bids" : "asks"}</div>
      ) : (
        <div role="list" aria-label={title} className="space-y-1">
          {levels.map((lvl) => {
            const pct = maxCumQty > 0 ? Math.min(100, Math.max(4, Math.round((lvl.cumulative_qty / maxCumQty) * 100))) : 0;
            const selected = selectedYesPriceCents === lvl.price_cents;
            const targetPrice = ticketSide === "no" ? 100 - lvl.price_cents : lvl.price_cents;
            const rowCls =
              "relative grid w-full grid-cols-3 items-center gap-2 overflow-hidden rounded-md border px-2 text-xs tabular-nums " +
              (selected
                ? "border-blue bg-blue-soft font-semibold"
                : "border-transparent bg-surface/80 [@media(hover:hover)_and_(pointer:fine)]:hover:border-hairline [@media(hover:hover)_and_(pointer:fine)]:hover:bg-surface");
            const inner = (
              <>
                <span
                  aria-hidden="true"
                  data-depth-bar
                  data-depth-side={sideKind}
                  style={{ width: `${pct}%` }}
                  className={
                    "pointer-events-none absolute inset-y-0 " +
                    (isBid ? "right-0 bg-dot-green/15" : "left-0 bg-dot-red/15")
                  }
                />
                <span
                  className={
                    "mono relative z-10 text-left font-semibold " +
                    (isBid ? "text-success-foreground" : "text-destructive-foreground")
                  }
                >
                  {lvl.price_cents}c
                </span>
                <span className="mono relative z-10 text-right text-ink">{lvl.qty}</span>
                <span className="mono relative z-10 text-right text-ink-2">{lvl.cumulative_qty}</span>
              </>
            );

            if (onSelectYesLevel) {
              return (
                <button
                  key={`${sideKind}-${lvl.price_cents}`}
                  type="button"
                  role="listitem"
                  data-orderbook-level
                  data-side={sideKind}
                  data-price-cents={lvl.price_cents}
                  aria-label={
                    ticketSide === "no"
                      ? `Set NO limit price to ${targetPrice} cents from YES ${lvl.price_cents} cents (${lvl.qty} contracts)`
                      : `Set limit price to ${lvl.price_cents} cents (${lvl.qty} contracts)`
                  }
                  onClick={() => onSelectYesLevel(lvl.price_cents)}
                  className={
                    rowCls +
                    " min-h-11 cursor-pointer outline-none transition-[border-color,background-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                  }
                >
                  {inner}
                </button>
              );
            }

            return (
              <div
                key={`${sideKind}-${lvl.price_cents}`}
                role="listitem"
                data-orderbook-level
                data-side={sideKind}
                data-price-cents={lvl.price_cents}
                className={rowCls + " min-h-8 py-1.5"}
              >
                {inner}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function OrderbookLadder({
  ticker,
  snapshot,
  streamStatus,
  orderQty = 10,
  side = "yes",
  action = "buy",
  selectedPriceCents = null,
  loading = false,
  error = null,
  maxLevels = 8,
  historyLimit = 20,
  onSelectPrice,
  onRefresh,
  className = "",
}: OrderbookLadderProps) {
  const cleanTicker = ticker.trim().toUpperCase();
  const [historyRecords, setHistoryRecords] = useState<OrderbookSnapshot[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [selectedHistoryIdx, setSelectedHistoryIdx] = useState<number | null>(null);
  const historySeqRef = useRef(0);
  const historyAbortRef = useRef<AbortController | null>(null);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      historyAbortRef.current?.abort();
      historyAbortRef.current = null;
    };
  }, []);

  const loadHistory = useCallback(async () => {
    historyAbortRef.current?.abort();
    historyAbortRef.current = null;
    const reqId = ++historySeqRef.current;
    if (!cleanTicker) {
      if (mountedRef.current) {
        setHistoryRecords([]);
        setHistoryError(null);
        setHistoryLoading(false);
        setSelectedHistoryIdx(null);
      }
      return;
    }
    const controller = new AbortController();
    historyAbortRef.current = controller;
    setHistoryLoading(true);
    setHistoryError(null);
    try {
      const res = await fetchOrderbookHistory(cleanTicker, historyLimit, controller.signal);
      if (!mountedRef.current || controller.signal.aborted || reqId !== historySeqRef.current) return;
      const safeRecords = Array.isArray(res.records) ? res.records : [];
      setHistoryRecords(safeRecords);
      setSelectedHistoryIdx((prev) =>
        prev !== null && (prev < 0 || prev >= safeRecords.length) ? null : prev,
      );
      setHistoryError(null);
    } catch (e) {
      if (!mountedRef.current || controller.signal.aborted || reqId !== historySeqRef.current) return;
      if (e instanceof Error && e.name === "AbortError") return;
      setHistoryError(e instanceof Error ? e.message : String(e));
    } finally {
      if (historyAbortRef.current === controller) historyAbortRef.current = null;
      if (mountedRef.current && reqId === historySeqRef.current) setHistoryLoading(false);
    }
  }, [cleanTicker, historyLimit]);

  useEffect(() => {
    setSelectedHistoryIdx(null);
    setHistoryRecords([]);
    setHistoryError(null);
    void loadHistory();
    return () => {
      historyAbortRef.current?.abort();
      historyAbortRef.current = null;
    };
  }, [loadHistory]);

  const handleRefreshAll = useCallback(() => {
    setSelectedHistoryIdx(null);
    onRefresh?.();
    void loadHistory();
  }, [onRefresh, loadHistory]);

  const selectedHistorySnap =
    selectedHistoryIdx !== null &&
    selectedHistoryIdx >= 0 &&
    selectedHistoryIdx < historyRecords.length
      ? historyRecords[selectedHistoryIdx]
      : null;
  const activeSnapshot = selectedHistorySnap ?? snapshot;
  const inspectingHistory = selectedHistorySnap !== null;

  const qty = Number.isFinite(orderQty) && orderQty > 0 ? Math.floor(orderQty) : 1;
  const bids = useMemo(() => (activeSnapshot?.yes_bids ?? []).slice(0, maxLevels), [activeSnapshot, maxLevels]);
  const asks = useMemo(() => (activeSnapshot?.yes_asks ?? []).slice(0, maxLevels), [activeSnapshot, maxLevels]);

  const maxCumQty = useMemo(() => {
    let m = 0;
    for (const lvl of bids) m = Math.max(m, lvl.cumulative_qty || lvl.qty);
    for (const lvl of asks) m = Math.max(m, lvl.cumulative_qty || lvl.qty);
    return m;
  }, [bids, asks]);

  const walkLevels = useMemo(
    () => deriveSideBookLevels(activeSnapshot, side, action),
    [activeSnapshot, side, action],
  );
  const walk = useMemo(() => walkBookLevels(walkLevels, qty), [walkLevels, qty]);
  const microprice = useMemo(() => computeMicropriceCents(activeSnapshot), [activeSnapshot]);
  const statusInfo = statusBadgeInfo(activeSnapshot, streamStatus, inspectingHistory);
  const isCrossed = isOrderbookCrossed(activeSnapshot);
  const isStale = !inspectingHistory && Boolean(activeSnapshot?.stale);
  const hasDepth = bids.length > 0 || asks.length > 0;

  const selectedYesPriceCents = useMemo(() => {
    if (selectedPriceCents === null || selectedPriceCents === undefined || !Number.isFinite(selectedPriceCents)) {
      return null;
    }
    const p = Math.round(selectedPriceCents);
    if (p < 1 || p > 99) return null;
    return side === "no" ? 100 - p : p;
  }, [selectedPriceCents, side]);

  const handleStagePrice = useCallback(
    (yesPriceCents: number) => {
      if (!onSelectPrice || !Number.isFinite(yesPriceCents)) return;
      const rawTarget = side === "no" ? 100 - yesPriceCents : yesPriceCents;
      const clamped = Math.min(99, Math.max(1, Math.round(rawTarget)));
      onSelectPrice(clamped);
    },
    [onSelectPrice, side],
  );

  return (
    <section
      data-orderbook-ladder
      aria-label={`L2 orderbook depth ladder for ${cleanTicker || "selected market"}`}
      className={`min-w-0 rounded-[10px] border border-hairline bg-surface p-3.5 ${className}`}
    >
      {/* Header: Title + WS/REST/Stale status pill + Refresh */}
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline pb-2.5">
        <div className="flex min-w-0 max-w-full flex-1 items-center gap-2">
          <Layers className="size-4 shrink-0 text-info-foreground" strokeWidth={1.5} aria-hidden />
          <div className="min-w-0 flex-1">
            <h4 className="text-xs font-semibold text-ink sm:text-sm">L2 Depth Ladder</h4>
            <p className="mono truncate text-xs text-ink-3">{cleanTicker || "Select a market"}</p>
          </div>
        </div>

        <div className="flex items-center gap-1.5">
          <span data-orderbook-status className="inline-flex">
            <StatusPill
              tone={statusInfo.tone}
              title={streamStatus?.last_error ?? undefined}
              className="min-h-7 px-2.5 py-0.5 text-xs text-ink-2"
            >
              <Activity className="size-3 shrink-0" strokeWidth={1.5} aria-hidden />
              <span>{statusInfo.label}</span>
            </StatusPill>
          </span>
          {onRefresh && (
            <Button
              variant="ghost"
              size="icon"
              aria-label="Refresh L2 orderbook"
              className="size-11 sm:size-8"
              onClick={handleRefreshAll}
            >
              <RefreshCw className={`size-3.5 ${loading ? "animate-spin" : ""}`} strokeWidth={1.5} aria-hidden />
            </Button>
          )}
        </div>
      </div>

      {/* Spread & Microprice Strip */}
      <dl className="grid grid-cols-2 gap-2 border-b border-hairline py-2.5 text-xs sm:grid-cols-4">
        <div className="min-w-0">
          <dt className="text-ink-3">Best YES Bid</dt>
          <dd className="mono mt-0.5 font-semibold tabular-nums text-success-foreground">
            {activeSnapshot?.best_yes_bid !== null && activeSnapshot?.best_yes_bid !== undefined
              ? `${activeSnapshot.best_yes_bid}c`
              : "-"}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-ink-3">Best YES Ask</dt>
          <dd className="mono mt-0.5 font-semibold tabular-nums text-destructive-foreground">
            {activeSnapshot?.best_yes_ask !== null && activeSnapshot?.best_yes_ask !== undefined
              ? `${activeSnapshot.best_yes_ask}c`
              : "-"}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-ink-3">Spread</dt>
          <dd
            data-orderbook-spread
            className={
              "mono mt-0.5 font-semibold tabular-nums " +
              (isCrossed ? "text-destructive-foreground" : "text-ink")
            }
          >
            {activeSnapshot?.spread_cents !== null && activeSnapshot?.spread_cents !== undefined
              ? isCrossed
                ? `${activeSnapshot.spread_cents}c (crossed)`
                : `${activeSnapshot.spread_cents}c`
              : "-"}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-ink-3">Microprice</dt>
          <dd data-orderbook-microprice className="mono mt-0.5 font-semibold tabular-nums text-ink">
            {microprice !== null ? `${microprice.toFixed(1)}c` : "-"}
          </dd>
        </div>
      </dl>

      {/* Live Book-Walk VWAP + Slippage Estimate */}
      <div
        data-orderbook-vwap
        className="my-2.5 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-hairline bg-board/60 px-3 py-2 text-xs"
      >
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="outline" className="mono tabular-nums text-xs">
            {action.toUpperCase()} {side.toUpperCase()} {qty}ct
          </Badge>
          {walk.vwapCents !== null ? (
            <span className="mono tabular-nums font-semibold text-ink">
              VWAP {walk.vwapCents.toFixed(2)}c
            </span>
          ) : (
            <span className="text-ink-3">No resting depth to walk</span>
          )}
        </div>
        {walk.vwapCents !== null && (
          <div className="flex flex-wrap items-center gap-2">
            <span
              className={
                "mono tabular-nums font-medium " +
                ((walk.slippageCents ?? 0) > 1
                  ? "text-warning-foreground"
                  : "text-ink-2")
              }
            >
              Slippage +{walk.slippageCents?.toFixed(2) ?? "0.00"}c
            </span>
            <Badge
              variant={walk.fillableQty < qty ? "warning" : "secondary"}
              className="mono tabular-nums text-xs"
            >
              {walk.fillableQty}/{qty}ct ({walk.levelsTouched} lvl{walk.levelsTouched === 1 ? "" : "s"})
            </Badge>
          </div>
        )}
      </div>

      {/* Historical Snapshot Inspection Banner */}
      {selectedHistorySnap && (
        <div
          role="status"
          data-orderbook-history-banner
          className="mb-2.5 flex flex-wrap items-center justify-between gap-2 border-b border-hairline pb-2 text-xs text-info-foreground"
        >
          <span className="mono min-w-0 break-words font-medium">
            Inspecting L2 history seq #{selectedHistorySnap.last_seq}
            {Number.isFinite(selectedHistorySnap.updated_ms) && selectedHistorySnap.updated_ms > 0
              ? ` (${timeUtcFullLabel(Math.floor(selectedHistorySnap.updated_ms / 1000))})`
              : ""}
          </span>
          <button
            type="button"
            data-orderbook-history-live
            onClick={() => setSelectedHistoryIdx(null)}
            className="min-h-11 shrink-0 cursor-pointer rounded border border-hairline bg-board px-2.5 text-xs font-medium text-ink hover:bg-surface sm:min-h-7"
          >
            Return to live
          </button>
        </div>
      )}

      {/* Stale / Crossed / Feed Warning Banners */}
      {isCrossed && (
        <div
          role="alert"
          data-orderbook-crossed-banner
          className="mb-2.5 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-dot-red/50 bg-board/60 px-3 py-2 text-xs text-destructive-foreground"
        >
          <span className="flex min-w-0 items-center gap-1.5 font-medium">
            <AlertTriangle className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
            <span className="min-w-0 break-words">
              Crossed L2 book (YES bid {activeSnapshot?.best_yes_bid}c &gt; YES ask {activeSnapshot?.best_yes_ask}c). Resync depth before staging orders.
            </span>
          </span>
          {onRefresh && (
            <button
              type="button"
              onClick={handleRefreshAll}
              className="min-h-11 shrink-0 cursor-pointer rounded border border-hairline bg-surface px-2.5 text-xs font-medium text-ink hover:bg-canvas sm:min-h-7"
            >
              Resync
            </button>
          )}
        </div>
      )}

      {(isStale || (!inspectingHistory && Boolean(error) && hasDepth)) && (
        <div
          role="status"
          data-orderbook-stale-banner
          className="mb-2.5 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-dot-amber/50 bg-board/60 px-3 py-2 text-xs text-warning-foreground"
        >
          <span className="flex min-w-0 items-center gap-1.5 font-medium">
            <AlertTriangle className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
            <span className="min-w-0 break-words">
              {error
                ? `Orderbook refresh delayed (${error}). Showing cached depth.`
                : "Orderbook snapshot is stale. Quotes and VWAP may lag venue L2."}
            </span>
          </span>
          {onRefresh && (
            <button
              type="button"
              onClick={handleRefreshAll}
              className="min-h-11 cursor-pointer rounded border border-hairline bg-surface px-2.5 text-xs font-medium text-ink hover:bg-canvas sm:min-h-7"
            >
              Resubscribe
            </button>
          )}
        </div>
      )}

      {/* Main Content: Loading / Degraded / Paired DOM Columns */}
      {loading && !activeSnapshot ? (
        <div className="space-y-2 py-2">
          <div className="h-8 w-full animate-pulse rounded bg-track" />
          <div className="h-8 w-full animate-pulse rounded bg-track" />
          <div className="h-8 w-full animate-pulse rounded bg-track" />
        </div>
      ) : error && !hasDepth && !inspectingHistory ? (
        <div
          role="alert"
          data-orderbook-empty
          className="rounded-lg border border-hairline bg-board/50 p-4 text-center text-xs text-ink-2"
        >
          <p className="font-medium text-ink">Orderbook feed unavailable</p>
          <p className="mt-1 text-ink-3">{error}</p>
          {onRefresh && (
            <Button variant="outline" size="sm" onClick={handleRefreshAll} className="mt-2.5">
              Retry
            </Button>
          )}
        </div>
      ) : !hasDepth ? (
        <div
          role="status"
          data-orderbook-empty
          className="rounded-lg border border-hairline bg-board/50 p-4 text-center text-xs text-ink-2"
        >
          <p className="font-medium text-ink">No resting L2 depth for {cleanTicker || "this market"}</p>
          <p className="mt-1 text-ink-3">
            Depth populates automatically when the market is open and the orderbook stream is active.
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
          <LadderSideColumn
            title="YES Bids"
            sideKind="bid"
            levels={bids}
            maxCumQty={maxCumQty}
            ticketSide={side}
            selectedYesPriceCents={selectedYesPriceCents}
            onSelectYesLevel={onSelectPrice ? handleStagePrice : undefined}
          />
          <LadderSideColumn
            title="YES Asks"
            sideKind="ask"
            levels={asks}
            maxCumQty={maxCumQty}
            ticketSide={side}
            selectedYesPriceCents={selectedYesPriceCents}
            onSelectYesLevel={onSelectPrice ? handleStagePrice : undefined}
          />
        </div>
      )}

      {onSelectPrice && hasDepth && (
        <p className="mt-2 text-xs text-ink-3">
          {side === "no"
            ? "Click any YES row to stage the complement NO limit price (100 - YES)."
            : "Click any bid or ask row to populate the ticket limit price."}
        </p>
      )}

      {/* Compact L2 Snapshot History Scrubber & Transition List */}
      {cleanTicker && (
        <div data-orderbook-history className="mt-3 border-t border-hairline pt-2.5">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-1.5 text-xs">
              <History className="size-3.5 shrink-0 text-info-foreground" strokeWidth={1.5} aria-hidden />
              <span className="font-semibold text-ink">L2 Snapshot History</span>
              <span className="mono text-ink-2">
                {historyLoading && historyRecords.length === 0
                  ? "Loading…"
                  : `(${historyRecords.length})`}
              </span>
            </div>

            <div className="flex items-center gap-1.5">
              {inspectingHistory && (
                <button
                  type="button"
                  onClick={() => setSelectedHistoryIdx(null)}
                  className="min-h-11 cursor-pointer rounded border border-hairline bg-board px-2.5 text-xs font-medium text-ink hover:bg-surface sm:min-h-7"
                >
                  Live
                </button>
              )}
              <button
                type="button"
                aria-label="Refresh L2 snapshot history"
                onClick={() => void loadHistory()}
                disabled={historyLoading}
                className="inline-flex min-h-11 min-w-11 cursor-pointer items-center justify-center rounded border border-hairline bg-board px-2 text-xs font-medium text-ink-2 hover:bg-surface hover:text-ink disabled:pointer-events-none disabled:bg-track disabled:text-ink-3 sm:min-h-7 sm:min-w-7"
              >
                <RefreshCw className={`size-3 ${historyLoading ? "animate-spin" : ""}`} strokeWidth={1.5} aria-hidden />
              </button>
            </div>
          </div>

          {historyError ? (
            <p className="mt-1.5 break-words text-xs text-ink-2">History unavailable ({historyError})</p>
          ) : historyLoading && historyRecords.length === 0 ? (
            <p className="mt-1.5 text-xs text-ink-2">Loading recorded L2 snapshots…</p>
          ) : historyRecords.length === 0 ? (
            <p className="mt-1.5 text-xs text-ink-2">No recorded L2 snapshots yet.</p>
          ) : (
            <div className="mt-2 space-y-2">
              <div className="flex items-center gap-1.5">
                <button
                  type="button"
                  aria-label="Previous L2 snapshot"
                  disabled={historyRecords.length === 0 || selectedHistoryIdx === 0}
                  onClick={() => {
                    setSelectedHistoryIdx((prev) => {
                      if (historyRecords.length === 0) return null;
                      if (prev === null) return Math.max(0, historyRecords.length - 1);
                      return Math.max(0, prev - 1);
                    });
                  }}
                  className="inline-flex min-h-11 min-w-11 shrink-0 cursor-pointer items-center justify-center rounded border border-hairline bg-board text-ink-2 hover:bg-surface hover:text-ink disabled:pointer-events-none disabled:bg-track disabled:text-ink-3 sm:min-h-7 sm:min-w-7"
                >
                  <ChevronLeft className="size-3.5" strokeWidth={1.5} aria-hidden />
                </button>
                <input
                  type="range"
                  min={0}
                  max={Math.max(0, historyRecords.length - 1)}
                  value={Math.min(Math.max(0, historyRecords.length - 1), selectedHistoryIdx ?? Math.max(0, historyRecords.length - 1))}
                  disabled={historyRecords.length <= 1}
                  onChange={(e) => {
                    const next = Number(e.target.value);
                    if (Number.isFinite(next) && next >= 0 && next < historyRecords.length) {
                      setSelectedHistoryIdx(next);
                    }
                  }}
                  aria-label="Scrub L2 snapshot history"
                  data-orderbook-history-scrubber
                  className="min-h-11 min-w-0 flex-1 cursor-pointer accent-[var(--blue)] disabled:cursor-default sm:min-h-7"
                />
                <button
                  type="button"
                  aria-label="Next L2 snapshot"
                  disabled={selectedHistoryIdx === null}
                  onClick={() => {
                    setSelectedHistoryIdx((prev) => {
                      if (prev === null) return null;
                      return prev + 1 >= historyRecords.length ? null : prev + 1;
                    });
                  }}
                  className="inline-flex min-h-11 min-w-11 shrink-0 cursor-pointer items-center justify-center rounded border border-hairline bg-board text-ink-2 hover:bg-surface hover:text-ink disabled:pointer-events-none disabled:bg-track disabled:text-ink-3 sm:min-h-7 sm:min-w-7"
                >
                  <ChevronRight className="size-3.5" strokeWidth={1.5} aria-hidden />
                </button>
              </div>

              <div
                role="list"
                aria-label="Recorded L2 depth transitions"
                data-orderbook-history-list
                className="max-h-36 divide-y divide-hairline overflow-y-auto border-t border-hairline"
              >
                {[...historyRecords]
                  .map((rec, origIdx) => ({ rec, origIdx }))
                  .reverse()
                  .map(({ rec, origIdx }) => {
                    const active = selectedHistoryIdx === origIdx;
                    const tsLabel =
                      Number.isFinite(rec.updated_ms) && rec.updated_ms > 0
                        ? timeUtcFullLabel(Math.floor(rec.updated_ms / 1000))
                        : "-";
                    return (
                      <button
                        key={`${rec.last_seq}-${rec.updated_ms}-${origIdx}`}
                        type="button"
                        role="listitem"
                        data-orderbook-history-item
                        data-history-seq={rec.last_seq}
                        aria-pressed={active}
                        onClick={() => setSelectedHistoryIdx(active ? null : origIdx)}
                        className={
                          "mono flex min-h-11 w-full min-w-0 cursor-pointer flex-wrap items-center justify-between gap-2 px-2 py-1.5 text-left text-xs outline-none transition-[background-color,color] duration-(--dur-press) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                          (active
                            ? "bg-blue-soft font-semibold text-ink"
                            : "text-ink-2 hover:bg-board/70 hover:text-ink")
                        }
                      >
                        <span className="flex min-w-0 flex-wrap items-center gap-2">
                          <span className="text-ink">#{rec.last_seq}</span>
                          <span>{tsLabel}</span>
                        </span>
                        <span className="flex min-w-0 flex-wrap items-center gap-2">
                          <span className="text-ink">
                            YES {rec.best_yes_bid !== null ? `${rec.best_yes_bid}c` : "-"}/
                            {rec.best_yes_ask !== null ? `${rec.best_yes_ask}c` : "-"}
                          </span>
                          <span>sp {rec.spread_cents !== null ? `${rec.spread_cents}c` : "-"}</span>
                          <span>
                            {rec.yes_bids.length}x{rec.yes_asks.length}
                          </span>
                        </span>
                      </button>
                    );
                  })}
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
