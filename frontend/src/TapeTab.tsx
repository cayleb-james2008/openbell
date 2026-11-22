// Live 15m Tape workbench tab: real Kalshi 15m markets sorted Open-first, then Settled newest-first,
// plus clickable Vaul Market Inspector fetching real /api/markets/history snapshots.
import { useEffect, useMemo, useState } from "react";
import { Drawer } from "vaul";
import { RefreshCw, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { MarketHistoryChart, ProbBar, ProvenanceTag } from "@/Charts";
import { useWide } from "@/lib/media";
import {
  type Dataset,
  type Market,
  type Read,
  cents,
  cleanStr,
  closeClock,
  fetchMarketHistory,
  impliedYesProb,
  isNum,
  marketIsOpen,
  seriesShort,
  timeUtcLabel,
} from "@/lib/desk";

const SERIES_FILTERS = ["All", "BTC", "ETH", "SOL", "XRP", "DOGE"] as const;
type StatusFilter = "all" | "open" | "settled";

function spreadCents(m: Market): string {
  if (isNum(m.yes_bid) && isNum(m.yes_ask) && m.yes_ask >= m.yes_bid && (m.yes_bid > 0 || m.yes_ask < 1)) {
    return `${Math.round((m.yes_ask - m.yes_bid) * 100)}c`;
  }
  return "-";
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
              ? "fixed inset-y-0 right-0 z-50 flex w-full max-w-[520px] flex-col border-l border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
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
                      <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                        <PillStatus className="border-r-0 pr-0">
                          <PillIndicator variant="success" />
                          Open
                        </PillStatus>
                      </Pill>
                    ) : (
                      <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-ink-2">
                        {res ? `Settled ${res}` : "Closed"}
                      </Pill>
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
                {/* Quote summary */}
                <dl className="grid grid-cols-2 gap-2.5 sm:grid-cols-4">
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">YES Bid / Ask</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">
                      {cents(market.yes_bid)} / {cents(market.yes_ask)}
                    </dd>
                  </div>
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">NO Bid / Ask</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">
                      {cents(market.no_bid)} / {cents(market.no_ask)}
                    </dd>
                  </div>
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">Spread</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">{spreadCents(market)}</dd>
                  </div>
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">Last Trade</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">{cents(market.last_price)}</dd>
                  </div>
                </dl>

                <div className="rounded-lg border border-hairline bg-board/50 p-3.5">
                  <div className="mb-2 flex items-center justify-between text-xs">
                    <span className="font-medium text-ink">Implied YES / NO Probability</span>
                    <ProvenanceTag kind="venue" />
                  </div>
                  <ProbBar yesProb={prob} />
                </div>

                {/* Snapshot price history trace */}
                <div className="rounded-lg border border-hairline bg-board/50 p-3.5">
                  <div className="mb-2 flex items-center justify-between text-xs">
                    <span className="font-semibold text-ink">Recorded Tape History</span>
                    <span className="font-mono text-ink-3">
                      {history ? `${history.length} snapshot${history.length === 1 ? "" : "s"}` : "Loading…"}
                    </span>
                  </div>
                  {loading ? (
                    <div className="h-36 animate-pulse rounded-md bg-track" />
                  ) : err ? (
                    <p role="alert" className="text-xs text-destructive-foreground">
                      Could not load market history: {err}
                    </p>
                  ) : (
                    <MarketHistoryChart points={history || [market]} ticker={ticker} />
                  )}
                </div>

                {/* Chronological snapshots list */}
                <div>
                  <h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-3">
                    Snapshot Tape ({history?.length ?? 1})
                  </h3>
                  <ul className="divide-y divide-hairline rounded-lg border border-hairline bg-board/40">
                    {(history && history.length > 0 ? [...history].reverse().slice(0, 30) : [market]).map((pt, idx) => (
                      <li key={idx} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2 font-mono text-xs">
                        <span className="text-ink-2">{timeUtcLabel(pt.fetched_at)}</span>
                        <span className="text-ink">
                          YES {cents(pt.yes_bid)}/{cents(pt.yes_ask)} · NO {cents(pt.no_bid)}/{cents(pt.no_ask)}
                        </span>
                        <span className="text-ink-3">
                          last {cents(pt.last_price)}
                          {pt.result ? ` · ${pt.result.toUpperCase()}` : ""}
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
  dataset,
  search,
  onRetry,
  onCollect,
  collectingNow,
}: {
  marketsRead?: Read<Market[]>;
  dataset: Dataset;
  search: string;
  onRetry: () => void;
  onCollect: () => void;
  collectingNow: boolean;
}) {
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [seriesFilter, setSeriesFilter] = useState<(typeof SERIES_FILTERS)[number]>("All");
  const [showAll, setShowAll] = useState(false);
  const [selected, setSelected] = useState<Market | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());

  const validMarkets = useMemo(() => {
    if (!marketsRead || !marketsRead.ok) return [];
    return marketsRead.v.filter((m): m is Market => !!m && typeof m === "object");
  }, [marketsRead]);

  const openCount = useMemo(() => validMarkets.filter(marketIsOpen).length, [validMarkets]);
  const settledCount = validMarkets.length - openCount;

  useEffect(() => {
    if (openCount === 0) return;
    const id = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(id);
  }, [openCount]);

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
    // Sort Open markets FIRST (soonest close first), then Settled markets newest-first.
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

  return (
    <section id="markets-card" aria-label="Live 15-minute Kalshi market tape" className="space-y-3">
      {/* Toolbar: status pills + series pills + count */}
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

        <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter markets by crypto series">
          {SERIES_FILTERS.map((sym) => (
            <button
              key={sym}
              type="button"
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
      </div>

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
              OpenBell reads public Kalshi 15-minute BTC, ETH, SOL, XRP, and DOGE quotes with zero order permissions. Pull the first live pass to populate the tape.
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
          <div className="flex items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-2 text-xs text-ink-2">
            <span id="markets-count" className="mono">
              Showing {visible.length} of {filtered.length} markets
            </span>
            <span>Click any market row to inspect snapshot history</span>
          </div>
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Market Ticker</TableHead>
                <TableHead>Series</TableHead>
                <TableHead>YES / NO Implied</TableHead>
                <TableHead className="text-right">YES Bid / Ask</TableHead>
                <TableHead className="text-right">Spread</TableHead>
                <TableHead className="text-right">Close</TableHead>
                <TableHead className="text-right">Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {visible.map((m, idx) => {
                const ticker = cleanStr(m.ticker, `market-${idx}`);
                const isOpen = marketIsOpen(m);
                const clock = closeClock(m.close_time, nowMs);
                const prob = impliedYesProb(m);
                const res = typeof m.result === "string" ? m.result.trim().toUpperCase() : "";
                return (
                  <TableRow
                    key={`${ticker}-${idx}`}
                    className="cursor-pointer hover:bg-board"
                    onClick={() => setSelected(m)}
                  >
                    <TableCell className="m-grow max-w-[300px]">
                      <button
                        type="button"
                        data-market-row
                        aria-label={`Inspect ${ticker}`}
                        onClick={(e) => {
                          e.stopPropagation();
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
                    <TableCell className="mono text-right text-xs text-ink-2">{spreadCents(m)}</TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      <span className={clock.isOpen ? "font-medium text-info-foreground" : ""}>{clock.label}</span>
                    </TableCell>
                    <TableCell className="m-right text-right">
                      {isOpen ? (
                        <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                          <PillStatus className="border-r-0 pr-0">
                            <PillIndicator variant="success" />
                            Open
                          </PillStatus>
                        </Pill>
                      ) : res === "YES" ? (
                        <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                          YES
                        </Pill>
                      ) : res === "NO" ? (
                        <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-info-foreground">
                          NO
                        </Pill>
                      ) : (
                        <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-ink-2">
                          Closed
                        </Pill>
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

      <MarketInspector market={selected} dataset={dataset} onClose={() => setSelected(null)} />
    </section>
  );
}
