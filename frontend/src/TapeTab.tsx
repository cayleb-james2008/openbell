// Live 15m Tape workbench tab: dynamically derived multi-series filter chips, live SSE status pill,
// ticking countdown-to-settlement badges, interactive SVG price + spread chart with hover crosshair,
// and Vaul Market Inspector drawer.
import { useEffect, useMemo, useState } from "react";
import { Drawer } from "vaul";
import { Activity, Clock, RefreshCw, X } from "lucide-react";
import {
  Badge,
  Button,
  StatusPill,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { MarketHistoryChart, ProbBar, ProvenanceTag } from "@/Charts";
import { AutoCollectorStrip } from "@/AutoCollectorStrip";
import { useWide } from "@/lib/media";
import {
  type CollectorScheduleStatus,
  type Dataset,
  type Home,
  type Market,
  type Read,
  type TapeStreamState,
  cents,
  cleanStr,
  closeClock,
  deriveSeriesChips,
  fetchMarketHistory,
  impliedYesProb,
  isNum,
  marketIsOpen,
  marketSpreadCents,
  seriesShort,
  timeUtcFullLabel,
} from "@/lib/desk";

type StatusFilter = "all" | "open" | "settled";

function spreadCentsLabel(m: Market): string {
  const sp = marketSpreadCents(m);
  return sp !== null ? `${sp}c` : "-";
}

function CountdownBadge({ closeTime, nowMs }: { closeTime: string | null; nowMs: number }) {
  const clock = closeClock(closeTime, nowMs);
  if (!clock.isOpen) {
    return <span className="mono text-xs text-ink-2">{clock.label}</span>;
  }
  const urgent = clock.secsRemaining !== null && clock.secsRemaining <= 120;
  return (
    <span
      data-countdown-badge
      className={
        "mono inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium tabular-nums " +
        (urgent
          ? "border-dot-amber bg-pill-warn-bg text-warning-foreground"
          : "border-hairline bg-blue-soft text-info-foreground")
      }
      title="Time remaining until 15-minute settlement"
    >
      <Clock className="size-3 shrink-0" aria-hidden />
      <span>{clock.label}</span>
    </span>
  );
}

function MarketInspector({
  market,
  dataset,
  onClose,
}: {
  market: Market | null;
  dataset: Dataset;
  onClose: () => void;
}) {
  const wide = useWide();
  const [history, setHistory] = useState<Market[] | null>(null);
  const [err, setErr] = useState("");
  const [loading, setLoading] = useState(false);
  const ticker = market ? cleanStr(market.ticker, "") : "";

  useEffect(() => {
    if (!ticker) {
      setHistory(null);
      setErr("");
      return;
    }
    let alive = true;
    setLoading(true);
    setErr("");
    fetchMarketHistory(ticker, dataset)
      .then((res) => {
        if (alive) setHistory(res.points);
      })
      .catch((e) => {
        if (alive) setErr(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [ticker, dataset]);

  const prob = market ? impliedYesProb(market) : null;
  const open = market ? marketIsOpen(market) : false;
  const res = market && typeof market.result === "string" ? market.result.trim().toUpperCase() : "";

  return (
    <Drawer.Root open={!!market} onOpenChange={(v) => !v && onClose()} direction={wide ? "right" : "bottom"}>
      <Drawer.Portal>
        <Drawer.Overlay className="fixed inset-0 z-40 bg-ink/30" />
        <Drawer.Content
          data-market-drawer
          className={
            wide
              ? "fixed inset-y-0 right-0 z-50 flex w-full max-w-[540px] flex-col border-l border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
              : "fixed inset-x-0 bottom-0 z-50 flex max-h-[88dvh] flex-col rounded-t-[14px] border-t border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
          }
        >
          {market && (
            <>
              <div className="flex items-start justify-between gap-3 border-b border-hairline px-5 py-4">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge variant="outline" className="font-mono text-xs">
                      {seriesShort(market.series, market.ticker)} 15m
                    </Badge>
                    {open ? (
                      <StatusPill tone="success" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                        Open
                      </StatusPill>
                    ) : (
                      <StatusPill tone="neutral" className="min-h-6 px-2 py-0.5 text-xs text-ink-2">
                        {res ? `Settled ${res}` : "Closed"}
                      </StatusPill>
                    )}
                  </div>
                  <Drawer.Title className="mono mt-2 break-all text-base font-semibold text-ink">
                    {cleanStr(market.ticker, "unnamed market")}
                  </Drawer.Title>
                  <Drawer.Description className="mt-0.5 font-mono text-xs text-ink-2">
                    Close {typeof market.close_time === "string" && market.close_time ? market.close_time : "unknown"}
                  </Drawer.Description>
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label="Close"
                  className="size-11 shrink-0"
                  onClick={onClose}
                >
                  <X className="size-4" strokeWidth={1.75} aria-hidden />
                </Button>
              </div>

              <div className="flex-1 space-y-5 overflow-y-auto px-5 py-5">
                <dl className="grid grid-cols-2 divide-y divide-hairline border border-hairline rounded-lg sm:grid-cols-4 sm:divide-x sm:divide-y-0">
                  <div className="p-3">
                    <dt className="text-xs text-ink-3">YES Bid / Ask</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">
                      {cents(market.yes_bid)} / {cents(market.yes_ask)}
                    </dd>
                  </div>
                  <div className="p-3">
                    <dt className="text-xs text-ink-3">NO Bid / Ask</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">
                      {cents(market.no_bid)} / {cents(market.no_ask)}
                    </dd>
                  </div>
                  <div className="p-3">
                    <dt className="text-xs text-ink-3">Spread</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">{spreadCentsLabel(market)}</dd>
                  </div>
                  <div className="p-3">
                    <dt className="text-xs text-ink-3">Last Trade</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">{cents(market.last_price)}</dd>
                  </div>
                </dl>

                <div className="border-t border-hairline pt-4">
                  <div className="mb-2 flex items-center justify-between text-xs">
                    <span className="font-medium text-ink">Implied YES / NO Probability</span>
                    <ProvenanceTag kind="venue" />
                  </div>
                  <ProbBar yesProb={prob} />
                </div>

                <div className="border-t border-hairline pt-4">
                  <div className="mb-2 flex items-center justify-between text-xs">
                    <span className="font-semibold text-ink">Price &amp; Spread History</span>
                    <span className="font-mono text-ink-3">
                      {history ? `${history.length} snapshot${history.length === 1 ? "" : "s"}` : "Loading…"}
                    </span>
                  </div>
                  {loading ? (
                    <div className="h-48 animate-pulse rounded-md bg-track" />
                  ) : err ? (
                    <p role="alert" className="text-xs text-destructive-foreground">
                      Could not load market history: {err}
                    </p>
                  ) : (
                    <MarketHistoryChart points={history || [market]} ticker={ticker} />
                  )}
                </div>

                <div className="border-t border-hairline pt-4">
                  <h3 className="mb-2 text-xs font-semibold text-ink-2">
                    Snapshot Tape ({history?.length ?? 1})
                  </h3>
                  <ul className="divide-y divide-hairline border-t border-hairline">
                    {(history && history.length > 0 ? [...history].reverse().slice(0, 30) : [market]).map((pt, idx) => (
                      <li key={idx} className="flex flex-wrap items-center justify-between gap-2 py-2 font-mono text-xs">
                        <span className="text-ink-2">{timeUtcFullLabel(pt.fetched_at)}</span>
                        <span className="text-ink">
                          YES {cents(pt.yes_bid)}/{cents(pt.yes_ask)} | NO {cents(pt.no_bid)}/{cents(pt.no_ask)}
                        </span>
                        <span className="text-ink-3">
                          spread {spreadCentsLabel(pt)}
                          {pt.result ? ` (${pt.result.toUpperCase()})` : ""}
                        </span>
                      </li>
                    ))}
                  </ul>
                </div>
              </div>
            </>
          )}
        </Drawer.Content>
      </Drawer.Portal>
    </Drawer.Root>
  );
}

export function TapeTab({
  marketsRead,
  home,
  dataset,
  search,
  stream,
  collectorStatus,
  updatingSchedule,
  onSelectSchedule,
  onOpenScorecard,
  onTradeMarket,
  onRetry,
  onCollect,
  collectingNow,
  jumpTicker,
  onClearJump,
}: {
  marketsRead?: Read<Market[]>;
  home: Home | null;
  dataset: Dataset;
  search: string;
  stream: TapeStreamState;
  collectorStatus: CollectorScheduleStatus | null;
  updatingSchedule: boolean;
  onSelectSchedule: (enabled: boolean, intervalSecs: number) => void;
  onOpenScorecard: () => void;
  onTradeMarket?: (ticker: string, yesAskCents: number | null) => void;
  onRetry: () => void;
  onCollect: () => void;
  collectingNow: boolean;
  jumpTicker?: string | null;
  onClearJump?: () => void;
}) {
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [seriesFilter, setSeriesFilter] = useState<string>("All");
  const [showAll, setShowAll] = useState(false);
  const [selected, setSelected] = useState<Market | null>(null);
  const [pinnedTicker, setPinnedTicker] = useState<string>("");
  const [inlineHistory, setInlineHistory] = useState<Market[] | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());

  const validMarkets = useMemo(() => {
    if (!marketsRead || !marketsRead.ok) return [];
    return marketsRead.v.filter((m): m is Market => !!m && typeof m === "object");
  }, [marketsRead]);

  const seriesChips = useMemo(
    () => deriveSeriesChips(validMarkets, home?.series),
    [validMarkets, home?.series],
  );

  // Reset series filter if dynamic chips no longer contain current filter
  useEffect(() => {
    if (seriesFilter !== "All" && !seriesChips.includes(seriesFilter)) {
      setSeriesFilter("All");
    }
  }, [seriesChips, seriesFilter]);

  const openCount = useMemo(() => validMarkets.filter(marketIsOpen).length, [validMarkets]);
  const settledCount = validMarkets.length - openCount;

  useEffect(() => {
    if (openCount === 0) return;
    const id = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(id);
  }, [openCount]);

  // Handle jump from Cmd+K Command Palette
  useEffect(() => {
    if (!jumpTicker || validMarkets.length === 0) return;
    const found = validMarkets.find((m) => cleanStr(m.ticker, "") === jumpTicker);
    if (found) {
      setStatusFilter("all");
      setSeriesFilter("All");
      setSelected(found);
      setPinnedTicker(jumpTicker);
      onClearJump?.();
    }
  }, [jumpTicker, validMarkets, onClearJump]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    const list = validMarkets.filter((m) => {
      const isOpen = marketIsOpen(m);
      if (statusFilter === "open" && !isOpen) return false;
      if (statusFilter === "settled" && isOpen) return false;
      if (seriesFilter !== "All" && seriesShort(m.series, m.ticker) !== seriesFilter) return false;
      if (q) {
        const hay = `${cleanStr(m.ticker, "")} ${cleanStr(m.series, "")} ${cleanStr(m.status, "")} ${cleanStr(m.result, "")}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
    return [...list].sort((a, b) => {
      const oa = marketIsOpen(a);
      const ob = marketIsOpen(b);
      if (oa !== ob) return oa ? -1 : 1;
      const ca = typeof a.close_time === "string" ? a.close_time : "";
      const cb = typeof b.close_time === "string" ? b.close_time : "";
      return oa ? ca.localeCompare(cb) : cb.localeCompare(ca);
    });
  }, [validMarkets, statusFilter, seriesFilter, search]);

  const visible = showAll ? filtered : filtered.slice(0, 50);

  // Keep a leading/pinned market loaded for the inline interactive Price + Spread chart.
  const featuredMarket = useMemo(() => {
    if (filtered.length === 0) return null;
    if (pinnedTicker) {
      const match = filtered.find((m) => cleanStr(m.ticker, "") === pinnedTicker);
      if (match) return match;
    }
    return filtered[0];
  }, [filtered, pinnedTicker]);

  const featuredTicker = featuredMarket ? cleanStr(featuredMarket.ticker, "") : "";

  useEffect(() => {
    if (!featuredTicker) {
      setInlineHistory(null);
      return;
    }
    let alive = true;
    fetchMarketHistory(featuredTicker, dataset)
      .then((res) => {
        if (alive) setInlineHistory(res.points);
      })
      .catch(() => {
        if (alive && featuredMarket) setInlineHistory([featuredMarket]);
      });
    return () => {
      alive = false;
    };
  }, [featuredTicker, dataset, featuredMarket]);

  return (
    <section id="markets-card" aria-label="Live 15-minute Kalshi market tape" className="space-y-3">
      {/* Toolbar: status pills + dynamically derived series pills + SSE status */}
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter markets by status">
          {(
            [
              ["all", `All (${validMarkets.length})`],
              ["open", `Open (${openCount})`],
              ["settled", `Settled (${settledCount})`],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              aria-pressed={statusFilter === id}
              onClick={() => {
                setStatusFilter(id);
                setShowAll(false);
              }}
              className={
                "min-h-11 cursor-pointer rounded-md border px-3 text-xs font-medium outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9 " +
                (statusFilter === id
                  ? "border-ink bg-ink text-canvas"
                  : "border-hairline bg-surface text-ink-2 hover:bg-board hover:text-ink")
              }
            >
              {label}
            </button>
          ))}
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter markets by crypto series">
            {seriesChips.map((sym) => (
              <button
                key={sym}
                type="button"
                data-series-chip={sym}
                aria-pressed={seriesFilter === sym}
                onClick={() => {
                  setSeriesFilter(sym);
                  setShowAll(false);
                }}
                className={
                  "mono min-h-11 min-w-11 cursor-pointer rounded-md border px-2.5 text-xs font-medium outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9 sm:min-w-0 " +
                  (seriesFilter === sym
                    ? "border-blue bg-blue-soft text-info-foreground"
                    : "border-hairline bg-surface text-ink-2 hover:bg-board hover:text-ink")
                }
              >
                {sym}
              </button>
            ))}
          </div>

          <StatusPill
            id="tape-stream-pill"
            tone={stream.mode === "sse" ? "success" : "info"}
            className="min-h-7 px-2.5 py-0.5 text-xs text-ink-2"
            title={
              stream.mode === "sse"
                ? `Live SSE tape stream connected (${stream.eventCount} events)`
                : "Polling /api/markets every 30s (auto-upgrades when /api/tape/stream is live)"
            }
          >
            <Activity className="size-3" strokeWidth={1.5} aria-hidden />
            <span className="mono">{stream.mode === "sse" ? "SSE live" : "30s poll"}</span>
          </StatusPill>
        </div>
      </div>

      {/* Inline interactive SVG Price + Spread History card for leading/pinned 15m market */}
      {featuredMarket && (
        <div
          id="tape-inline-chart"
          className="rounded-[10px] border border-hairline bg-surface p-3.5"
        >
          <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant="outline" className="mono text-xs">
                {seriesShort(featuredMarket.series, featuredMarket.ticker)} 15m
              </Badge>
              <span className="mono break-all text-xs font-semibold text-ink sm:text-sm">
                {featuredTicker}
              </span>
              <CountdownBadge closeTime={featuredMarket.close_time} nowMs={nowMs} />
            </div>
            <div className="flex items-center gap-2">
              <span className="hidden text-xs text-ink-3 sm:inline">Hover chart for bid/ask &amp; spread</span>
              {onTradeMarket && (
                <Button
                  variant="outline"
                  className="min-h-11 px-3 text-xs sm:min-h-8"
                  onClick={() => {
                    const askCents = isNum(featuredMarket.yes_ask)
                      ? Math.min(99, Math.max(1, Math.round(featuredMarket.yes_ask * 100)))
                      : null;
                    onTradeMarket(featuredTicker, askCents);
                  }}
                >
                  Trade
                </Button>
              )}
              <Button
                variant="outline"
                className="min-h-11 px-3 text-xs sm:min-h-8"
                onClick={() => setSelected(featuredMarket)}
              >
                Full tape
              </Button>
            </div>
          </div>
          <MarketHistoryChart
            points={inlineHistory && inlineHistory.length > 0 ? inlineHistory : [featuredMarket]}
            ticker={featuredTicker}
            height={204}
          />
        </div>
      )}

      {/* Content states */}
      {!marketsRead ? (
        <div className="rounded-[10px] border border-hairline bg-surface p-6">
          <div className="h-6 w-48 animate-pulse rounded bg-track" />
          <div className="mt-3 space-y-2">
            <div className="h-10 w-full animate-pulse rounded bg-track" />
            <div className="h-10 w-full animate-pulse rounded bg-track" />
          </div>
        </div>
      ) : !marketsRead.ok ? (
        <div role="alert" className="flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-5">
          <div>
            <p className="text-sm font-medium text-destructive-foreground">Could not read the market tape.</p>
            <p className="mt-0.5 text-xs text-ink-2">{marketsRead.msg}</p>
          </div>
          <Button variant="outline" className="min-h-11" onClick={onRetry}>
            Retry
          </Button>
        </div>
      ) : validMarkets.length === 0 ? (
        <div role="status" className="flex flex-wrap items-center justify-between gap-4 rounded-[10px] border border-hairline bg-surface p-6">
          <div>
            <p className="text-sm font-semibold text-ink">No snapshots collected yet</p>
            <p className="mt-1 text-xs text-ink-2">
              OpenBell collects live Kalshi 15m crypto quotes for charting, backtesting, and live order staging. Pull the first pass to populate the tape.
            </p>
          </div>
          <Button variant="default" className="min-h-11 gap-2" disabled={collectingNow} onClick={onCollect}>
            <RefreshCw className={`size-4 ${collectingNow ? "animate-spin" : ""}`} aria-hidden />
            {collectingNow ? "Collecting…" : "Collect live tape"}
          </Button>
        </div>
      ) : filtered.length === 0 ? (
        <div role="status" className="flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-5">
          <p className="text-sm text-ink-2">No 15-minute markets match the active filters.</p>
          <Button
            variant="outline"
            className="min-h-11"
            onClick={() => {
              setStatusFilter("all");
              setSeriesFilter("All");
            }}
          >
            Reset filters
          </Button>
        </div>
      ) : (
        <div className="overflow-hidden rounded-[10px] border border-hairline bg-surface">
          <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-2 text-xs text-ink-2">
            <span id="markets-count" className="mono">
              Showing {visible.length} of {filtered.length} markets
            </span>
            <span className="hidden sm:inline">Click any row to inspect snapshot history</span>
          </div>
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Market Ticker</TableHead>
                <TableHead>Series</TableHead>
                <TableHead>YES / NO Implied</TableHead>
                <TableHead className="text-right">YES Bid / Ask</TableHead>
                <TableHead className="text-right">Spread</TableHead>
                <TableHead className="text-right">Settlement</TableHead>
                <TableHead className="text-right">Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {visible.map((m, idx) => {
                const ticker = cleanStr(m.ticker, `market-${idx}`);
                const isOpen = marketIsOpen(m);
                const prob = impliedYesProb(m);
                const res = typeof m.result === "string" ? m.result.trim().toUpperCase() : "";
                return (
                  <TableRow
                    key={`${ticker}-${idx}`}
                    className="cursor-pointer hover:bg-board"
                    onClick={() => {
                      setPinnedTicker(ticker);
                      setSelected(m);
                    }}
                  >
                    <TableCell className="m-grow max-w-[300px]">
                      <button
                        type="button"
                        data-market-row
                        aria-label={`Inspect ${ticker}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          setPinnedTicker(ticker);
                          setSelected(m);
                        }}
                        className="mono inline-flex min-h-11 w-full cursor-pointer items-center break-all text-left text-sm font-medium text-ink underline-offset-4 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                      >
                        {ticker}
                      </button>
                    </TableCell>
                    <TableCell className="m-right">
                      <Badge variant="outline" className="mono text-xs">
                        {seriesShort(m.series, m.ticker)}
                      </Badge>
                    </TableCell>
                    <TableCell className="m-full">
                      <ProbBar yesProb={prob} />
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink">
                      {cents(m.yes_bid)} / {cents(m.yes_ask)}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">{spreadCentsLabel(m)}</TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      <CountdownBadge closeTime={m.close_time} nowMs={nowMs} />
                    </TableCell>
                    <TableCell className="m-right text-right">
                      {isOpen ? (
                        <StatusPill tone="success" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                          Open
                        </StatusPill>
                      ) : res === "YES" ? (
                        <StatusPill tone="success" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                          YES
                        </StatusPill>
                      ) : res === "NO" ? (
                        <StatusPill tone="info" className="min-h-6 px-2 py-0.5 text-xs text-info-foreground">
                          NO
                        </StatusPill>
                      ) : (
                        <StatusPill tone="neutral" className="min-h-6 px-2 py-0.5 text-xs text-ink-2">
                          Closed
                        </StatusPill>
                      )}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
          {filtered.length > 50 && (
            <div className="flex justify-center border-t border-hairline bg-board/40 p-2.5">
              <Button variant="ghost" className="min-h-11 text-xs" onClick={() => setShowAll((v) => !v)}>
                {showAll ? "Show top 50 markets" : `Show all ${filtered.length} markets`}
              </Button>
            </div>
          )}
        </div>
      )}

      {/* Background Auto-Collector Control Strip + 200-Window Readiness Gate Progress Bar */}
      <AutoCollectorStrip
        status={collectorStatus}
        updating={updatingSchedule}
        onSelectSchedule={onSelectSchedule}
        onOpenScorecard={onOpenScorecard}
        variant="banner"
      />

      <MarketInspector market={selected} dataset={dataset} onClose={() => setSelected(null)} />
    </section>
  );
}
