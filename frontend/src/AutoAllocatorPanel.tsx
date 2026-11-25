// Autonomous Edge & Auto-Allocator workbench panel (v0.10.0):
// 1-Click Full Edge Discovery & Allocation Pipeline (Maker Post-Only 1.75% vs Taker 7%),
// fractional-Kelly OOS winner weighting, automatic quarantine of negative/thin/correlated shapes,
// $500/day Go/No-Go verdict, custom zero-bloat SVG equity comparison, and 1-click ticket staging
// gated behind fail-closed Hard Risk Rails (live_armed: false on boot).
import { useCallback, useEffect, useMemo, useState } from "react";
import { ChevronDown, RefreshCw, ShieldAlert, ShieldCheck, ShieldOff, SlidersHorizontal, TrendingUp } from "lucide-react";
import { toast } from "sonner";
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
import { AllocatorComparisonChart, ProvenanceTag } from "@/Charts";
import {
  type Dataset,
  type Market,
  type Slots,
  money,
  moneySigned,
} from "@/lib/desk";
import {
  type AllocatorExecutionMode,
  type AllocatorSizingMode,
  type AutoAllocatorReport,
  type AutoAllocatorRequest,
  DEFAULT_ALLOCATOR_REQUEST,
  fetchAllocatorReport,
} from "@/lib/allocator";

const SIZING_LABELS: Record<AllocatorSizingMode, string> = {
  quarter_kelly: "Quarter-Kelly",
  half_kelly: "Half-Kelly",
  equal_winners: "Equal Winners",
};

const EXEC_LABELS: Record<AllocatorExecutionMode, string> = {
  maker: "Maker Post-Only",
  taker: "Taker",
};

function fmtCents(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "n/a";
  return `${v >= 0 ? "+" : ""}${v.toFixed(2)}c`;
}

function fmtPct(v: number | null | undefined, digits = 1): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "0.0%";
  return `${v.toFixed(digits)}%`;
}

export type StagedOrderDraft = {
  ticker: string;
  side: "yes" | "no";
  count: number;
  priceCents: number;
  postOnly: boolean;
};

export function AutoAllocatorPanel({
  dataset,
  slots,
  markets,
  liveArmed,
  search,
  onStageOrder,
  onOpenFullAudit,
}: {
  dataset: Dataset;
  slots: Slots | null;
  markets: Market[];
  liveArmed: boolean;
  search: string;
  onStageOrder?: (draft: StagedOrderDraft) => void;
  onOpenFullAudit?: () => void;
}) {
  const [executionMode, setExecutionMode] = useState<AllocatorExecutionMode>(
    DEFAULT_ALLOCATOR_REQUEST.execution_mode,
  );
  const [sizingMode, setSizingMode] = useState<AllocatorSizingMode>(
    DEFAULT_ALLOCATOR_REQUEST.sizing_mode,
  );
  const [report, setReport] = useState<AutoAllocatorReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [quarantineOpen, setQuarantineOpen] = useState(false);

  const buildRequest = useCallback(
    (exec: AllocatorExecutionMode, sizing: AllocatorSizingMode): AutoAllocatorRequest => ({
      ...DEFAULT_ALLOCATOR_REQUEST,
      execution_mode: exec,
      sizing_mode: sizing,
    }),
    [],
  );

  const loadInitial = useCallback(async () => {
    setLoading(true);
    try {
      const next = await fetchAllocatorReport(dataset, undefined, { slots, markets });
      setReport(next);
    } finally {
      setLoading(false);
    }
  }, [dataset, slots, markets]);

  const recalculate = useCallback(
    async (exec: AllocatorExecutionMode, sizing: AllocatorSizingMode, notify = false) => {
      setLoading(true);
      try {
        const next = await fetchAllocatorReport(dataset, buildRequest(exec, sizing), { slots, markets });
        setReport(next);
        if (notify) {
          toast.success(
            `Edge pipeline complete (${EXEC_LABELS[exec]}, ${SIZING_LABELS[sizing]}): ${next.active_winners.length} winner${next.active_winners.length === 1 ? "" : "s"}, ${next.daily_target.verdict === "GO_500_DAY" ? "GO $500/day" : "Paper only"}.`,
          );
        }
      } finally {
        setLoading(false);
      }
    },
    [buildRequest, dataset, slots, markets],
  );

  useEffect(() => {
    void loadInitial();
  }, [loadInitial]);

  const handleExecChange = (nextExec: AllocatorExecutionMode) => {
    setExecutionMode(nextExec);
    void recalculate(nextExec, sizingMode, false);
  };

  const handleSizingChange = (nextSizing: AllocatorSizingMode) => {
    setSizingMode(nextSizing);
    void recalculate(executionMode, nextSizing, false);
  };

  const q = search.trim().toLowerCase();

  const filteredWinners = useMemo(() => {
    if (!report) return [];
    if (!q) return report.active_winners;
    return report.active_winners.filter(
      (w) =>
        w.strategy.toLowerCase().includes(q) ||
        w.side.toLowerCase().includes(q) ||
        w.status.toLowerCase().includes(q),
    );
  }, [report, q]);

  const filteredQuarantined = useMemo(() => {
    if (!report) return [];
    if (!q) return report.quarantined;
    return report.quarantined.filter(
      (item) =>
        item.strategy.toLowerCase().includes(q) ||
        item.side.toLowerCase().includes(q) ||
        item.reason.toLowerCase().includes(q),
    );
  }, [report, q]);

  const intentByStrategy = useMemo(() => {
    const map = new Map<string, AutoAllocatorReport["planned_intents"][number]>();
    for (const it of report?.planned_intents ?? []) {
      map.set(it.strategy, it);
    }
    return map;
  }, [report]);

  const activeOpenTicker = useMemo(() => {
    const open = markets.find((m) => m && (!m.result || m.result === ""));
    return open?.ticker || markets[0]?.ticker || "KXBTC15M-DEMO";
  }, [markets]);

  const activeExec = (report?.execution_mode === "taker" ? "taker" : "maker") as AllocatorExecutionMode;
  const activeSizing = (
    report?.sizing_mode === "half_kelly" || report?.sizing_mode === "equal_winners"
      ? report.sizing_mode
      : "quarter_kelly"
  ) as AllocatorSizingMode;
  const isArmed = report ? report.live_armed || liveArmed : liveArmed;
  const cashReservePct = report ? report.cash_reserve_pct : 100;
  const comp = report?.comparison;
  const daily = report?.daily_target;
  const isGo500 = daily?.verdict === "GO_500_DAY";

  return (
    <section
      id="auto-allocator-panel"
      data-auto-allocator
      aria-label="Autonomous Edge Allocator"
      className="rounded-[10px] border border-hairline bg-surface p-4 shadow-(--elev-card) sm:p-5"
    >
      {/* Header + Status Pills */}
      <div className="flex flex-wrap items-start justify-between gap-3 border-b border-hairline pb-3.5">
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <TrendingUp className="size-4 text-info-foreground" aria-hidden />
            <h2 className="text-base font-semibold text-ink">Autonomous Edge &amp; Capital Allocator</h2>
            <ProvenanceTag kind="paper" />
            {daily && (
              <StatusPill id="allocator-500-verdict" tone={isGo500 ? "success" : "warning"}>
                {isGo500
                  ? `GO $500/Day (${money(daily.projected_daily_net_dollars)}/d)`
                  : `NO-GO $500/Day (${money(daily.projected_daily_net_dollars)}/d)`}
              </StatusPill>
            )}
          </div>
          <p className="text-xs text-ink-2">
            Causal In-Sample to OOS tournament, Fractional-Kelly sizing, 4x lower maker post-only fees, and automatic loser quarantine.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <StatusPill id="allocator-mode-pill" tone={activeExec === "maker" ? "info" : "warning"}>
            {EXEC_LABELS[activeExec]} / {SIZING_LABELS[activeSizing]}
          </StatusPill>

          <Badge id="allocator-cash-pill" variant="outline" className="mono min-h-7 px-2.5 text-xs text-ink-2">
            Cash Reserve {fmtPct(cashReservePct, 0)}
          </Badge>

          <StatusPill
            id="allocator-arm-pill"
            tone={isArmed ? "warning" : "neutral"}
          >
            {isArmed ? (
              <ShieldCheck className="size-3.5 text-warning-foreground" aria-hidden />
            ) : (
              <ShieldOff className="size-3.5 text-ink-3" aria-hidden />
            )}
            <span>{isArmed ? "Live Armed (Rails Active)" : "Disarmed Until Armed"}</span>
          </StatusPill>
        </div>
      </div>

      {/* Controls + 1-Click Pipeline Trigger */}
      <div className="mt-3.5 flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-board/60 p-3">
        <div className="flex flex-wrap items-center gap-3">
          <div className="space-y-1">
            <span className="block text-xs font-medium text-ink-3">Execution Mode</span>
            <div
              id="allocator-exec-toggle"
              role="group"
              aria-label="Execution Mode"
              className="inline-flex rounded-md border border-hairline bg-surface p-0.5"
            >
              <button
                id="allocator-exec-maker"
                type="button"
                aria-pressed={executionMode === "maker"}
                onClick={() => handleExecChange("maker")}
                className={
                  "min-h-11 cursor-pointer rounded px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                  (executionMode === "maker"
                    ? "bg-ink-button text-ink-button-fg"
                    : "text-ink-2 hover:text-ink")
                }
              >
                Maker Post-Only (1.75%)
              </button>
              <button
                id="allocator-exec-taker"
                type="button"
                aria-pressed={executionMode === "taker"}
                onClick={() => handleExecChange("taker")}
                className={
                  "min-h-11 cursor-pointer rounded px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                  (executionMode === "taker"
                    ? "bg-ink-button text-ink-button-fg"
                    : "text-ink-2 hover:text-ink")
                }
              >
                Taker (7%)
              </button>
            </div>
          </div>

          <div className="space-y-1">
            <span className="block text-xs font-medium text-ink-3">Kelly Sizing</span>
            <div
              id="allocator-sizing-toggle"
              role="group"
              aria-label="Kelly Sizing"
              className="inline-flex rounded-md border border-hairline bg-surface p-0.5"
            >
              {(["quarter_kelly", "half_kelly", "equal_winners"] as const).map((mode) => (
                <button
                  key={mode}
                  id={`allocator-sizing-${mode}`}
                  type="button"
                  aria-pressed={sizingMode === mode}
                  onClick={() => handleSizingChange(mode)}
                  className={
                    "min-h-11 cursor-pointer rounded px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                    (sizingMode === mode
                      ? "bg-ink-button text-ink-button-fg"
                      : "text-ink-2 hover:text-ink")
                  }
                >
                  {SIZING_LABELS[mode]}
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2 self-end">
          {onOpenFullAudit && (
            <Button
              variant="outline"
              size="sm"
              onClick={onOpenFullAudit}
            >
              <ShieldAlert className="size-3.5" aria-hidden />
              <span>Full Quant Audit</span>
            </Button>
          )}
          <Button
            id="allocator-recalculate-btn"
            variant="default"
            size="sm"
            disabled={loading}
            onClick={() => void recalculate(executionMode, sizingMode, true)}
          >
            <RefreshCw className={`size-3.5 ${loading ? "animate-spin" : ""}`} aria-hidden />
            <span>{loading ? "Running Pipeline…" : "Run Discovery & Allocate"}</span>
          </Button>
        </div>
      </div>

      {/* KPI Strip */}
      <div className="mt-3.5 grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <div className="rounded-[10px] border border-hairline bg-board/60 p-3">
          <div className="text-xs text-ink-3">OOS Net P&amp;L vs Legacy 1/N</div>
          <div
            className={
              "mono mt-1 text-lg font-semibold " +
              ((comp?.equity_delta_dollars ?? 0) > 0
                ? "text-success-foreground"
                : (comp?.equity_delta_dollars ?? 0) < 0
                  ? "text-destructive-foreground"
                  : "text-ink")
            }
          >
            {comp ? moneySigned(comp.equity_delta_dollars) : "-"}
          </div>
          <div className="mono mt-0.5 text-xs text-ink-2">
            Alloc {comp ? moneySigned(comp.auto_allocator_net_pnl) : "-"} vs Legacy{" "}
            {comp ? moneySigned(comp.legacy_equal_taker_net_pnl) : "-"}
          </div>
        </div>

        <div className="rounded-[10px] border border-hairline bg-board/60 p-3">
          <div className="text-xs text-ink-3">Kalshi Fee Savings</div>
          <div className="mono mt-1 text-lg font-semibold text-info-foreground">
            {comp ? money(comp.fee_savings_dollars) : "-"}
          </div>
          <div className="mono mt-0.5 text-xs text-ink-2">
            Fees {comp ? money(comp.auto_allocator_fees_dollars) : "-"} vs Taker{" "}
            {comp ? money(comp.legacy_equal_taker_fees_dollars) : "-"}
          </div>
        </div>

        <div className="rounded-[10px] border border-hairline bg-board/60 p-3">
          <div className="text-xs text-ink-3">Max Drawdown %</div>
          <div className="mono mt-1 text-lg font-semibold text-ink">
            {comp ? fmtPct(comp.auto_allocator_max_dd_pct) : "-"}
          </div>
          <div className="mono mt-0.5 text-xs text-ink-2">
            Legacy 1/N Taker: {comp ? fmtPct(comp.legacy_equal_taker_max_dd_pct) : "-"}
          </div>
        </div>

        <div className="rounded-[10px] border border-hairline bg-board/60 p-3">
          <div className="text-xs text-ink-3">Allocated Heat / OOS Depth</div>
          <div className="mono mt-1 text-lg font-semibold text-ink">
            {report ? fmtPct(report.total_allocated_pct, 1) : "0.0%"}
          </div>
          <div className="mono mt-0.5 text-xs text-ink-2">
            {report ? `${report.windows_settled_oos} OOS / ${report.windows_settled_total} settled` : "Loading…"}
          </div>
        </div>
      </div>

      {/* $500/Day Blockers Banner */}
      {daily && daily.blockers.length > 0 && (
        <div
          id="allocator-daily-blockers"
          className="mt-3 rounded-[10px] border border-hairline bg-board/50 px-3.5 py-2.5 text-xs"
        >
          <div className="flex flex-wrap items-center justify-between gap-2">
            <span className="font-semibold text-ink">
              $500/Day Capital Readiness Blockers ({daily.blockers.length})
            </span>
            {daily.required_bankroll_dollars !== null && (
              <span className="mono text-ink-2">
                Est. Bankroll for $500/day: {money(daily.required_bankroll_dollars)}
              </span>
            )}
          </div>
          <ul className="mt-1 space-y-0.5 text-ink-2">
            {daily.blockers.slice(0, 3).map((b, i) => (
              <li key={i}>• {b}</li>
            ))}
          </ul>
        </div>
      )}

      {/* Zero-Bloat Custom SVG Comparison Chart */}
      <div className="mt-3.5 rounded-[10px] border border-hairline bg-board/40 p-3.5">
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2 text-xs">
            <SlidersHorizontal className="size-3.5 text-info-foreground" aria-hidden />
            <span className="font-semibold text-ink">
              Causal OOS Replay: {EXEC_LABELS[activeExec]} + Kelly vs Legacy 1/N Taker
            </span>
          </div>
          <div className="flex flex-wrap items-center gap-3 text-xs text-ink-2">
            <span className="inline-flex items-center gap-1.5">
              <span className="inline-block h-0.5 w-4 rounded bg-blue" aria-hidden="true" />
              Auto-Allocator ({EXEC_LABELS[activeExec]})
            </span>
            <span className="inline-flex items-center gap-1.5">
              <span className="inline-block h-0.5 w-4 border-t-2 border-dashed border-ink-3" aria-hidden="true" />
              Legacy 1/N Taker
            </span>
          </div>
        </div>

        <AllocatorComparisonChart
          points={comp?.equity_curve ?? []}
          startingBankroll={report?.starting_bankroll ?? 100}
          execLabel={EXEC_LABELS[activeExec]}
        />
      </div>

      {/* Active OOS Winners Table */}
      <div className="mt-3.5 overflow-hidden rounded-[10px] border border-hairline bg-surface">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-2.5">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-semibold text-ink">
              Active OOS Winners ({filteredWinners.length})
            </h3>
            <Badge variant="outline" className="mono text-xs text-success-foreground">
              Positive EV, |r| &le; 0.65
            </Badge>
          </div>
          <span className="mono text-xs text-ink-2">
            Max slot {fmtPct(report?.max_slot_risk_pct ?? 15, 0)}, Max heat {fmtPct(report?.max_total_heat_pct ?? 40, 0)}
          </span>
        </div>

        {filteredWinners.length === 0 ? (
          <p role="status" className="p-4 text-xs text-ink-2">
            0 strategies currently clear the OOS positive expectancy &amp; depth gate — holding{" "}
            <span className="mono font-semibold text-ink">{fmtPct(cashReservePct, 0)} cash reserve</span> until forward evidence qualifies.
          </p>
        ) : (
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Strategy</TableHead>
                <TableHead>Side</TableHead>
                <TableHead className="text-right">OOS Trades / WR</TableHead>
                <TableHead className="text-right">IS / OOS EV</TableHead>
                <TableHead className="text-right">Maker vs Taker</TableHead>
                <TableHead className="text-right">Full Kelly</TableHead>
                <TableHead className="text-right">Alloc % / $</TableHead>
                <TableHead className="text-right">Planned Intent</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {filteredWinners.map((w) => {
                const intent = intentByStrategy.get(w.strategy);
                const limitCents = intent?.max_limit_cents ?? Math.min(99, Math.max(1, Math.round(w.oos_avg_entry_price * 100)));
                const targetCts = Math.max(1, w.rail_clamped_contracts || 1);
                return (
                  <TableRow key={w.strategy} data-allocator-winner={w.strategy}>
                    <TableCell className="m-grow max-w-[230px] break-all font-medium text-ink">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span>{w.strategy}</span>
                        <StatusPill tone="success" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                          Winner
                        </StatusPill>
                      </div>
                    </TableCell>
                    <TableCell className="mono text-xs uppercase text-ink-2">{w.side}</TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      {w.oos_trades} ({(w.oos_win_rate * 100).toFixed(1)}%)
                    </TableCell>
                    <TableCell className="mono text-right text-xs font-semibold text-success-foreground">
                      {w.is_net_expectancy_cents !== null ? `${fmtCents(w.is_net_expectancy_cents)} / ` : ""}
                      {fmtCents(w.oos_net_expectancy_cents)}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      {w.maker_fee_cents.toFixed(2)}c vs {w.taker_fee_cents.toFixed(2)}c
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      {fmtPct(w.full_kelly_pct)}
                    </TableCell>
                    <TableCell className="mono text-right text-xs font-semibold text-ink">
                      {fmtPct(w.allocated_pct)} ({money(w.allocated_dollars)})
                    </TableCell>
                    <TableCell className="m-right text-right text-xs">
                      <div className="inline-flex items-center justify-end gap-2">
                        <div className="inline-flex flex-col items-end gap-0.5">
                          <span className="mono font-medium text-ink">
                            {w.rail_clamped_contracts} cts @ {limitCents}c
                          </span>
                          <span className="mono text-ink-3">
                            {(intent?.post_only ?? activeExec === "maker") ? "post_only" : "taker"} ({intent?.rail_ready ? "rail ready" : "disarmed"})
                          </span>
                        </div>
                        {onStageOrder && (
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() =>
                              onStageOrder({
                                ticker: activeOpenTicker,
                                side: w.side === "no" ? "no" : "yes",
                                count: targetCts,
                                priceCents: limitCents,
                                postOnly: intent?.post_only ?? activeExec === "maker",
                              })
                            }
                          >
                            Stage
                          </Button>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </div>

      {/* Collapsible Quarantined Shapes (0% Weight) */}
      <div className="mt-3 overflow-hidden rounded-[10px] border border-hairline bg-board/40">
        <button
          id="allocator-quarantine-toggle"
          type="button"
          aria-expanded={quarantineOpen}
          onClick={() => setQuarantineOpen((v) => !v)}
          className="flex min-h-11 w-full cursor-pointer items-center justify-between gap-2 px-4 py-2.5 text-left text-xs font-medium text-ink hover:bg-board sm:min-h-10"
        >
          <span className="flex flex-wrap items-center gap-2">
            <span className="font-semibold">Quarantined Shapes (0% Weight)</span>
            <Badge variant="outline" className="mono text-xs text-destructive-foreground">
              {filteredQuarantined.length} excluded
            </Badge>
          </span>
          <ChevronDown
            className={`size-4 text-ink-2 transition-transform ${quarantineOpen ? "rotate-180" : ""}`}
            aria-hidden
          />
        </button>

        {quarantineOpen && (
          <div id="allocator-quarantined-list" className="border-t border-hairline bg-surface">
            {filteredQuarantined.length === 0 ? (
              <p className="p-4 text-xs text-ink-2">No shapes quarantined under current filter.</p>
            ) : (
              <Table className="stack">
                <TableHeader>
                  <TableRow>
                    <TableHead>Strategy</TableHead>
                    <TableHead>Side</TableHead>
                    <TableHead className="text-right">OOS Trades</TableHead>
                    <TableHead className="text-right">OOS Net EV / ct</TableHead>
                    <TableHead>Refusal Reason</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {filteredQuarantined.map((item) => (
                    <TableRow key={item.strategy} data-allocator-quarantined={item.strategy}>
                      <TableCell className="m-grow max-w-[220px] break-all font-medium text-ink">
                        {item.strategy}
                      </TableCell>
                      <TableCell className="mono text-xs uppercase text-ink-2">{item.side}</TableCell>
                      <TableCell className="mono text-right text-xs text-ink-2">{item.oos_trades}</TableCell>
                      <TableCell
                        className={
                          "mono text-right text-xs font-medium " +
                          (item.oos_net_expectancy_cents !== null && item.oos_net_expectancy_cents < 0
                            ? "text-destructive-foreground"
                            : "text-ink-2")
                        }
                      >
                        {fmtCents(item.oos_net_expectancy_cents)}
                      </TableCell>
                      <TableCell className="text-xs text-ink-2">{item.reason}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </div>
        )}
      </div>

      {/* Honesty Notes */}
      {report && report.honesty_notes.length > 0 && (
        <ul className="mt-3 space-y-1 border-t border-hairline pt-2.5 text-xs text-ink-3">
          {report.honesty_notes.slice(0, 4).map((note, idx) => (
            <li key={idx}>• {note}</li>
          ))}
        </ul>
      )}
    </section>
  );
}
