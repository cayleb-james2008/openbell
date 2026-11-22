// Rune Strategies workbench tab: ranked leaderboard for all 12+ Rune strategies with null/malformed guards,
// j/k/Enter keyboard navigation, Tremor SparkAreaChart + Tracker evidence bar, Vaul Strategy Inspector
// with Cult UI Expandable Rune source, and Interactive Rune Sandbox Editor + Backtester.
import { useEffect, useMemo, useState } from "react";
import { Drawer } from "vaul";
import { ChevronDown, Code2, Play, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { Tracker, type TrackerBlockProps } from "@/components/Tracker/Tracker";
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";
import { StrategyEquityChart, TremorSpark, validPoints, type Point } from "@/Charts";
import { RuneSandbox } from "@/RuneSandbox";
import { useWide } from "@/lib/media";
import {
  type Dataset,
  type Equity,
  type Home,
  type Market,
  type Read,
  type Slots,
  type Strategy,
  type StrategySource,
  type Trade,
  classifyVerdict,
  cleanStr,
  dayLabel,
  fetchStrategySource,
  isNum,
  moneySigned,
  signedCents,
  timeUtcLabel,
} from "@/lib/desk";

type VerdictFilter = "all" | "proven" | "testing" | "rejected";

function VerdictPill({ verdict }: { verdict: unknown }) {
  const c = classifyVerdict(verdict);
  if (c.kind === "proven_forward") {
    return (
      <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
        <PillStatus className="border-r-0 pr-0">
          <PillIndicator variant="success" />
          Proven
        </PillStatus>
      </Pill>
    );
  }
  if (c.kind === "proven_example") {
    return (
      <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-warning-foreground">
        <PillStatus className="border-r-0 pr-0">
          <PillIndicator variant="warning" />
          Example pass
        </PillStatus>
      </Pill>
    );
  }
  if (c.kind === "rejected") {
    return (
      <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-destructive-foreground">
        <PillStatus className="border-r-0 pr-0">
          <PillIndicator variant="error" />
          Rejected
        </PillStatus>
      </Pill>
    );
  }
  return (
    <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-ink-2">
      <PillStatus className="border-r-0 pr-0">
        <PillIndicator variant="info" />
        Testing
      </PillStatus>
    </Pill>
  );
}

function EvidenceTracker({ traded, minWindows }: { traded: number; minWindows: number }) {
  const target = minWindows > 0 ? minWindows : 100;
  const filled = Math.min(10, Math.round((traded / target) * 10));
  const data: TrackerBlockProps[] = Array.from({ length: 10 }, (_, idx) => ({
    color: idx < filled ? (traded >= target ? "bg-dot-green" : "bg-blue") : "bg-track",
    tooltip: `${traded} / ${target} traded windows`,
  }));
  return (
    <div className="flex min-w-28 items-center gap-2">
      <div className="w-20 shrink-0">
        <Tracker data={data} />
      </div>
      <span className="mono text-xs text-ink-2">
        {traded}/{target}
      </span>
    </div>
  );
}

function StrategyInspector({
  strategy,
  pnl,
  points,
  trades,
  tradesTruncated,
  onOpenInSandbox,
  onClose,
}: {
  strategy: Strategy | null;
  pnl: number | null;
  points: Point[];
  trades: Trade[];
  tradesTruncated: boolean;
  onOpenInSandbox: (name: string) => void;
  onClose: () => void;
}) {
  const wide = useWide();
  const [source, setSource] = useState<StrategySource | null>(null);
  const [sourceErr, setSourceErr] = useState("");
  const name = strategy ? cleanStr(strategy.strategy, "") : "";

  useEffect(() => {
    if (!name) {
      setSource(null);
      setSourceErr("");
      return;
    }
    let alive = true;
    setSource(null);
    setSourceErr("");
    fetchStrategySource(name)
      .then((res) => {
        if (alive) setSource(res);
      })
      .catch((e) => {
        if (alive) setSourceErr(e instanceof Error ? e.message : String(e));
      });
    return () => {
      alive = false;
    };
  }, [name]);

  const v = strategy ? classifyVerdict(strategy.verdict) : null;
  const side = strategy ? cleanStr(strategy.side, "-").toUpperCase() : "-";
  const traded = strategy && isNum(strategy.windows_traded) ? strategy.windows_traded : 0;
  const minW =
    strategy?.standard && typeof strategy.standard === "object" && isNum(strategy.standard.min_windows)
      ? strategy.standard.min_windows
      : 100;

  return (
    <Drawer.Root open={!!strategy} onOpenChange={(o) => !o && onClose()} direction={wide ? "right" : "bottom"}>
      <Drawer.Portal>
        <Drawer.Overlay className="fixed inset-0 z-40 bg-ink/30" />
        <Drawer.Content
          data-strategy-drawer
          className={
            wide
              ? "fixed inset-y-0 right-0 z-50 flex w-full max-w-[540px] flex-col border-l border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
              : "fixed inset-x-0 bottom-0 z-50 flex max-h-[88dvh] flex-col rounded-t-[14px] border-t border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
          }
        >
          {strategy && v && (
            <>
              <div className="flex items-start justify-between gap-3 border-b border-hairline px-5 py-4">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge variant="outline" className="mono text-xs">
                      {side}
                    </Badge>
                    <VerdictPill verdict={strategy.verdict} />
                  </div>
                  <Drawer.Title className="mt-2 break-all text-lg font-semibold text-ink">{name || "unnamed"}</Drawer.Title>
                  <Drawer.Description className="mt-0.5 text-xs text-ink-2">{v.text}</Drawer.Description>
                </div>
                <Button variant="ghost" size="icon" aria-label="Close" className="size-11 shrink-0" onClick={onClose}>
                  <X className="size-4" strokeWidth={1.75} aria-hidden />
                </Button>
              </div>

              <div className="flex-1 space-y-5 overflow-y-auto px-5 py-5">
                <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-hairline bg-board/60 px-3.5 py-2.5">
                  <span className="text-xs text-ink-2">Modify thresholds and score on tape in memory</span>
                  <Button
                    variant="outline"
                    className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-8"
                    onClick={() => onOpenInSandbox(name)}
                  >
                    <Play className="size-3.5" aria-hidden />
                    <span>Edit in Sandbox</span>
                  </Button>
                </div>

                <dl className="grid grid-cols-2 gap-2.5 sm:grid-cols-4">
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">Paper Net P&amp;L</dt>
                    <dd
                      className={
                        "mono mt-1 text-sm font-semibold " +
                        (pnl === null ? "text-ink-2" : pnl >= 0 ? "text-success-foreground" : "text-destructive-foreground")
                      }
                    >
                      {pnl !== null ? moneySigned(pnl) : "$0.00"}
                    </dd>
                  </div>
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">Mean / Contract</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">
                      {signedCents(strategy.mean_cents_per_contract)}
                    </dd>
                  </div>
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">Windows Traded</dt>
                    <dd className="mono mt-1 text-sm font-semibold text-ink">
                      {traded} / {minW}
                    </dd>
                  </div>
                  <div className="rounded-lg border border-hairline bg-board p-3">
                    <dt className="text-xs text-ink-3">Half Split</dt>
                    <dd className="mono mt-1 text-xs font-medium text-ink">
                      {signedCents(strategy.first_half_cents)} / {signedCents(strategy.second_half_cents)}
                    </dd>
                  </div>
                </dl>

                {/* Cumulative paper equity curve */}
                <div className="rounded-lg border border-hairline bg-board/50 p-3.5">
                  <div className="mb-2 flex items-center justify-between text-xs">
                    <span className="font-semibold text-ink">Cumulative Paper P&amp;L Curve</span>
                    <span className="mono text-ink-3">Paper sim · Fee-adjusted</span>
                  </div>
                  <StrategyEquityChart
                    points={points}
                    start={0}
                    label={`Cumulative paper P&L for ${name}`}
                  />
                </div>

                {/* Cult UI Expandable: strategy script source */}
                <div className="rounded-lg border border-hairline bg-board/50 px-3.5 py-2.5">
                  <Expandable expandDirection="vertical" expandBehavior="push" className="w-full">
                    <ExpandableTrigger className="flex min-h-11 w-full cursor-pointer items-center justify-between gap-2 text-left text-xs font-semibold text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring">
                      <span className="inline-flex items-center gap-2">
                        <Code2 className="size-4 text-ink-2" aria-hidden />
                        <span>Strategy script source {source?.file ? `(${source.file})` : ""}</span>
                      </span>
                      <ChevronDown className="size-4 text-ink-2" aria-hidden />
                    </ExpandableTrigger>
                    <ExpandableContent preset="fade">
                      {source ? (
                        <pre className="mono mt-2 overflow-x-auto whitespace-pre-wrap break-words rounded-md border border-hairline bg-surface p-3 text-xs text-ink">
                          {source.source}
                        </pre>
                      ) : sourceErr ? (
                        <p className="mt-2 text-xs text-ink-2">Strategy source unavailable ({sourceErr}).</p>
                      ) : (
                        <p className="mt-2 text-xs text-ink-3">Loading strategy script…</p>
                      )}
                    </ExpandableContent>
                  </Expandable>
                </div>

                {/* Paper trade ledger */}
                <div>
                  <div className="mb-2 flex items-center justify-between gap-2">
                    <h3 className="text-xs font-semibold uppercase tracking-wide text-ink-3">
                      Paper Trade Ledger ({trades.length}
                      {tradesTruncated ? "+" : ""})
                    </h3>
                    <span className="text-xs text-ink-3">Most recent 40 shown</span>
                  </div>
                  {trades.length === 0 ? (
                    <p className="rounded-lg border border-hairline bg-board/40 px-4 py-3 text-xs text-ink-2">
                      No paper trades for this strategy yet.
                    </p>
                  ) : (
                    <ul className="divide-y divide-hairline rounded-lg border border-hairline bg-board/40">
                      {[...trades].reverse().slice(0, 40).map((t, idx) => (
                        <li key={idx} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2 font-mono text-xs">
                          <span className="text-ink-2">
                            {dayLabel(t.close_epoch)} {timeUtcLabel(t.close_epoch)}
                          </span>
                          <span className="text-ink">
                            Slot {t.slot} · {t.contracts}x @ {Math.round((t.price || 0) * 100)}c
                          </span>
                          <span className={t.pnl_dollars >= 0 ? "font-medium text-success-foreground" : "font-medium text-destructive-foreground"}>
                            {moneySigned(t.pnl_dollars)}
                          </span>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              </div>
            </>
          )}
        </Drawer.Content>
      </Drawer.Portal>
    </Drawer.Root>
  );
}

export function StrategiesTab({
  strategiesRead,
  slotsRead,
  equityRead,
  marketsRead,
  home,
  dataset,
  search,
  onRetry,
  isActiveTab,
  jumpStrategy,
  sandboxSignal,
  onClearJump,
}: {
  strategiesRead?: Read<Strategy[]>;
  slotsRead?: Read<Slots>;
  equityRead?: Read<Equity>;
  marketsRead?: Read<Market[]>;
  home: Home | null;
  dataset: Dataset;
  search: string;
  onRetry: () => void;
  isActiveTab: boolean;
  jumpStrategy?: string | null;
  sandboxSignal?: number;
  onClearJump?: () => void;
}) {
  const [filter, setFilter] = useState<VerdictFilter>("all");
  const [activeIdx, setActiveIdx] = useState<number>(-1);
  const [openName, setOpenName] = useState<string | null>(null);
  const [sandboxOpen, setSandboxOpen] = useState(false);
  const [sandboxInitName, setSandboxInitName] = useState<string | null>(null);

  const slots = slotsRead?.ok ? slotsRead.v : null;
  const equity = equityRead?.ok ? equityRead.v : null;
  const markets = useMemo(
    () => (marketsRead?.ok ? marketsRead.v.filter((m): m is Market => !!m && typeof m === "object") : []),
    [marketsRead],
  );

  const validRows = useMemo(() => {
    if (!strategiesRead || !strategiesRead.ok) return [];
    return strategiesRead.v.filter((s): s is Strategy => !!s && typeof s === "object");
  }, [strategiesRead]);

  useEffect(() => {
    if (jumpStrategy && validRows.some((s) => cleanStr(s.strategy, "") === jumpStrategy)) {
      setFilter("all");
      setOpenName(jumpStrategy);
      onClearJump?.();
    }
  }, [jumpStrategy, validRows, onClearJump]);

  useEffect(() => {
    if (sandboxSignal && sandboxSignal > 0) {
      setSandboxOpen(true);
    }
  }, [sandboxSignal]);

  const pnlByStrategy = useMemo(() => {
    const map = new Map<string, number>();
    if (slots?.strategy_totals) {
      for (const st of slots.strategy_totals) {
        if (st && typeof st.strategy === "string" && isNum(st.paper_pnl_dollars)) {
          map.set(st.strategy, st.paper_pnl_dollars);
        }
      }
    }
    if (equity?.series) {
      for (const s of equity.series) {
        if (s && typeof s.strategy === "string" && !map.has(s.strategy)) {
          const pts = validPoints(s.points);
          if (pts.length > 0) map.set(s.strategy, pts[pts.length - 1][1]);
        }
      }
    }
    return map;
  }, [slots, equity]);

  const pointsByStrategy = useMemo(() => {
    const map = new Map<string, Point[]>();
    if (equity?.series) {
      for (const s of equity.series) {
        if (s && typeof s.strategy === "string") {
          map.set(s.strategy, validPoints(s.points));
        }
      }
    }
    return map;
  }, [equity]);

  const counts = useMemo(() => {
    let proven = 0;
    let testing = 0;
    let rejected = 0;
    for (const s of validRows) {
      const k = classifyVerdict(s.verdict).kind;
      if (k === "proven_forward" || k === "proven_example") proven++;
      else if (k === "rejected") rejected++;
      else testing++;
    }
    return { all: validRows.length, proven, testing, rejected };
  }, [validRows]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    const rows = validRows.filter((s) => {
      const v = classifyVerdict(s.verdict);
      if (filter === "proven" && v.kind !== "proven_forward" && v.kind !== "proven_example") return false;
      if (filter === "testing" && v.kind !== "testing") return false;
      if (filter === "rejected" && v.kind !== "rejected") return false;
      if (q) {
        const hay = `${cleanStr(s.strategy, "")} ${cleanStr(s.side, "")} ${v.label} ${v.text}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
    return [...rows].sort((a, b) => {
      const pa = pnlByStrategy.get(cleanStr(a.strategy, "")) ?? -Infinity;
      const pb = pnlByStrategy.get(cleanStr(b.strategy, "")) ?? -Infinity;
      if (pa !== pb) return pb - pa;
      return cleanStr(a.strategy, "").localeCompare(cleanStr(b.strategy, ""));
    });
  }, [validRows, filter, search, pnlByStrategy]);

  // j / k / Enter keyboard navigation on Rune Strategies tab and inside open Strategy Inspector.
  useEffect(() => {
    if (!isActiveTab && !openName) return;
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target?.isContentEditable) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "j" || e.key === "k") {
        if (filtered.length === 0) return;
        e.preventDefault();
        const nextIdx =
          activeIdx < 0
            ? 0
            : e.key === "j"
              ? Math.min(filtered.length - 1, activeIdx + 1)
              : Math.max(0, activeIdx - 1);
        setActiveIdx(nextIdx);
        if (openName) setOpenName(cleanStr(filtered[nextIdx].strategy, ""));
      } else if (e.key === "Enter" && !openName && activeIdx >= 0 && activeIdx < filtered.length) {
        if (tag === "BUTTON" || tag === "A") return;
        e.preventDefault();
        setOpenName(cleanStr(filtered[activeIdx].strategy, ""));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isActiveTab, openName, filtered, activeIdx]);

  const openStrategy = openName
    ? validRows.find((s) => cleanStr(s.strategy, "") === openName) || null
    : null;
  const openTrades = useMemo(() => {
    if (!openName || !slots?.trades) return [];
    return slots.trades.filter((t) => t && t.strategy === openName);
  }, [openName, slots]);

  return (
    <section id="strategies" aria-label="Strategies leaderboard and sandbox" className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter strategies by verdict">
          {(
            [
              ["all", `All (${counts.all})`],
              ["proven", `Proven (${counts.proven})`],
              ["testing", `Testing (${counts.testing})`],
              ["rejected", `Rejected (${counts.rejected})`],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              aria-pressed={filter === id}
              onClick={() => {
                setFilter(id);
                setActiveIdx(-1);
              }}
              className={
                "min-h-11 cursor-pointer rounded-md border px-3 text-xs font-medium outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9 " +
                (filter === id
                  ? "border-ink bg-ink text-canvas"
                  : "border-hairline bg-surface text-ink-2 hover:bg-board hover:text-ink")
              }
            >
              {label}
            </button>
          ))}
        </div>

        <div className="flex flex-wrap items-center gap-2.5">
          <span className="hidden text-xs text-ink-3 lg:inline">
            Use <kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">j</kbd>/<kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">k</kbd> and <kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">Enter</kbd>
          </span>
          <Button
            id="open-sandbox-btn"
            variant={sandboxOpen ? "default" : "outline"}
            className="min-h-11 gap-1.5 px-3 text-xs font-medium sm:min-h-9"
            aria-expanded={sandboxOpen}
            onClick={() => setSandboxOpen((v) => !v)}
          >
            <Code2 className="size-3.5" aria-hidden />
            <span>{sandboxOpen ? "Close Strategy Sandbox" : "Strategy Sandbox"}</span>
          </Button>
        </div>
      </div>

      {/* Interactive Strategy Sandbox Editor & Backtest Panel */}
      {sandboxOpen && (
        <RuneSandbox
          strategies={validRows}
          markets={markets}
          slots={slots}
          home={home}
          dataset={dataset}
          initialStrategyName={sandboxInitName}
          onClose={() => setSandboxOpen(false)}
        />
      )}

      {!strategiesRead ? (
        <div className="rounded-[10px] border border-hairline bg-surface p-6">
          <div className="h-6 w-48 animate-pulse rounded bg-track" />
          <div className="mt-3 space-y-2">
            <div className="h-10 w-full animate-pulse rounded bg-track" />
            <div className="h-10 w-full animate-pulse rounded bg-track" />
          </div>
        </div>
      ) : !strategiesRead.ok ? (
        <div role="alert" className="flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-5">
          <div>
            <p className="text-sm font-medium text-destructive-foreground">Could not score the strategy library.</p>
            <p className="mt-0.5 text-xs text-ink-2">{strategiesRead.msg}</p>
          </div>
          <Button variant="outline" className="min-h-11" onClick={onRetry}>
            Retry
          </Button>
        </div>
      ) : validRows.length === 0 ? (
        <div role="status" className="rounded-[10px] border border-hairline bg-surface p-6">
          <p className="text-sm font-semibold text-ink">No strategies in the library</p>
          <p className="mt-1 text-xs text-ink-2">Add strategy scripts to the strategies folder to score them on paper.</p>
        </div>
      ) : filtered.length === 0 ? (
        <div role="status" className="flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-5">
          <p className="text-sm text-ink-2">No strategy matches the current filter or search query.</p>
          <Button variant="outline" className="min-h-11" onClick={() => setFilter("all")}>
            Show all strategies
          </Button>
        </div>
      ) : (
        <div className="overflow-hidden rounded-[10px] border border-hairline bg-surface">
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Strategy</TableHead>
                <TableHead>Side</TableHead>
                <TableHead className="text-right">Paper Net P&amp;L</TableHead>
                <TableHead className="text-right">Mean / Contract</TableHead>
                <TableHead>Evidence Bar</TableHead>
                <TableHead>Paper Curve</TableHead>
                <TableHead className="text-right">Verdict</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {filtered.map((s, idx) => {
                const name = cleanStr(s.strategy, `strategy-${idx}`);
                const side = cleanStr(s.side, "-").toUpperCase();
                const pnl = pnlByStrategy.has(name) ? (pnlByStrategy.get(name) as number) : null;
                const pts = pointsByStrategy.get(name) || [];
                const traded = isNum(s.windows_traded) ? s.windows_traded : 0;
                const minW = s.standard && typeof s.standard === "object" && isNum(s.standard.min_windows) ? s.standard.min_windows : 100;
                const selected = idx === activeIdx;
                return (
                  <TableRow
                    key={`${name}-${idx}`}
                    aria-selected={selected ? "true" : undefined}
                    className={"cursor-pointer " + (selected ? "bg-blue-soft" : "hover:bg-board")}
                    onClick={() => {
                      setActiveIdx(idx);
                      setOpenName(name);
                    }}
                  >
                    <TableCell className="m-grow max-w-[260px]">
                      <button
                        type="button"
                        data-strategy-row
                        aria-label={`Open ${name}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          setActiveIdx(idx);
                          setOpenName(name);
                        }}
                        className="inline-flex min-h-11 w-full cursor-pointer items-center break-all text-left text-sm font-medium text-ink underline-offset-4 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                      >
                        {name}
                      </button>
                    </TableCell>
                    <TableCell className="m-right">
                      <Badge variant="outline" className="mono text-xs">
                        {side}
                      </Badge>
                    </TableCell>
                    <TableCell
                      className={
                        "mono text-right text-sm font-medium " +
                        (pnl === null ? "text-ink-2" : pnl > 0 ? "text-success-foreground" : pnl < 0 ? "text-destructive-foreground" : "text-ink")
                      }
                    >
                      {pnl !== null ? moneySigned(pnl) : "$0.00"}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      {signedCents(s.mean_cents_per_contract)}
                    </TableCell>
                    <TableCell>
                      <EvidenceTracker traded={traded} minWindows={minW} />
                    </TableCell>
                    <TableCell>
                      <TremorSpark points={pts} start={0} label={`Paper curve for ${name}`} className="h-7 w-24" />
                    </TableCell>
                    <TableCell className="m-right text-right">
                      <VerdictPill verdict={s.verdict} />
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}

      <StrategyInspector
        strategy={openStrategy}
        pnl={openStrategy ? (pnlByStrategy.get(cleanStr(openStrategy.strategy, "")) ?? null) : null}
        points={openStrategy ? pointsByStrategy.get(cleanStr(openStrategy.strategy, "")) || [] : []}
        trades={openTrades}
        tradesTruncated={!!slots?.trades_truncated}
        onOpenInSandbox={(name) => {
          setOpenName(null);
          setSandboxInitName(name);
          setSandboxOpen(true);
        }}
        onClose={() => setOpenName(null)}
      />
    </section>
  );
}
