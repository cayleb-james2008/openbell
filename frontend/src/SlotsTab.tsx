// Slot Desk workbench tab: compounding paper equity + underwater drawdown subplot, stint attribution
// breakdown, compound-sized slot allocation with 0/3..3/3 strike history, one-click .md/.csv report
// export (GET /api/export/report), verbatim audit trail (Cult UI Expandable), and Read-Only Venue Account.
import { useMemo, useState } from "react";
import { Download, FileText } from "lucide-react";
import { toast } from "sonner";
import {
  Button,
  Disclosure,
  StatusPill,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TrackerBar,
  type TrackerBlock,
} from "@/components/ui";
import { EquityDrawdownChart, ProvenanceTag, validPoints } from "@/Charts";
import { AutoAllocatorPanel, type StagedOrderDraft } from "@/AutoAllocatorPanel";
import {
  type Dataset,
  type Equity,
  type Home,
  type Market,
  type Read,
  type Report,
  type Slot,
  type Slots,
  cleanStr,
  exportDailyReport,
  isNum,
  money,
  moneySigned,
} from "@/lib/desk";

import type { LiveDeskBundle } from "@/lib/live";

function StrikeTracker({ strikes = 0 }: { strikes?: number }) {
  const n = isNum(strikes) ? Math.min(3, Math.max(0, Math.round(strikes))) : 0;
  const data: TrackerBlock[] = [0, 1, 2].map((i) => ({
    color: i < n ? "bg-dot-red" : "bg-dot-green",
    tooltip: i < n ? `Strike ${i + 1} of 3` : "Clean slot standing",
  }));
  return (
    <div className="flex items-center gap-2">
      <div className="w-12 shrink-0">
        <TrackerBar data={data} />
      </div>
      <span className="mono text-xs text-ink-2">{n}/3</span>
    </div>
  );
}

export function SlotsTab({
  slotsRead,
  equityRead,
  homeRead,
  reportRead,
  marketsRead,
  liveBundle,
  onOpenOrderTicket,
  onStageOrder,
  onOpenFullAudit,
  onOpenSettings,
  dataset,
  search,
  onRetry,
}: {
  slotsRead?: Read<Slots>;
  equityRead?: Read<Equity>;
  homeRead?: Read<Home>;
  reportRead?: Read<Report>;
  marketsRead?: Read<Market[]>;
  liveBundle: LiveDeskBundle;
  onOpenOrderTicket: () => void;
  onStageOrder?: (draft: StagedOrderDraft) => void;
  onOpenFullAudit?: () => void;
  onOpenSettings: () => void;
  dataset: Dataset;
  search: string;
  onRetry: () => void;
}) {
  const [exporting, setExporting] = useState<"" | "md" | "csv">("");
  const slots = slotsRead?.ok ? slotsRead.v : null;
  const equity = equityRead?.ok ? equityRead.v : null;
  const home = homeRead?.ok ? homeRead.v : null;
  const markets = useMemo(
    () => (marketsRead?.ok ? marketsRead.v.filter((m): m is Market => !!m && typeof m === "object") : []),
    [marketsRead],
  );
  const reportText = reportRead?.ok ? reportRead.v.report : "";

  const potPoints = useMemo(() => validPoints(equity?.pot), [equity]);
  const potStart = slots && isNum(slots.paper_pot_start) ? slots.paper_pot_start : 100;

  const activeSlots = useMemo(() => {
    if (!slots) return [];
    const q = search.trim().toLowerCase();
    return slots.slots
      .filter((s): s is Slot => !!s && typeof s === "object")
      .filter((s) => !q || cleanStr(s.strategy, "").toLowerCase().includes(q) || `slot ${s.slot}`.includes(q));
  }, [slots, search]);

  const retiredSlots = useMemo(() => {
    if (!slots) return [];
    const q = search.trim().toLowerCase();
    return slots.retired
      .filter((s): s is Slot => !!s && typeof s === "object")
      .filter(
        (s) =>
          !q ||
          cleanStr(s.strategy, "").toLowerCase().includes(q) ||
          cleanStr(s.rotated_to, "").toLowerCase().includes(q) ||
          `slot ${s.slot}`.includes(q),
      );
  }, [slots, search]);

  // Aggregate stint attribution across strategy totals + active/retired stints.
  const attribution = useMemo(() => {
    if (!slots) return [];
    const map = new Map<string, { strategy: string; pnl: number; windows: number; stints: number; retired: number }>();
    const add = (name: string, pnl: number, windows: number, isRetired: boolean) => {
      const prev = map.get(name);
      if (prev) {
        prev.pnl += pnl;
        prev.windows += windows;
        prev.stints += 1;
        if (isRetired) prev.retired += 1;
      } else {
        map.set(name, { strategy: name, pnl, windows, stints: 1, retired: isRetired ? 1 : 0 });
      }
    };
    for (const s of slots.slots) {
      if (!s || typeof s !== "object") continue;
      const p = isNum(s.stint_pnl_dollars) ? s.stint_pnl_dollars : isNum(s.paper_pnl_dollars) ? s.paper_pnl_dollars : 0;
      add(cleanStr(s.strategy, "unnamed"), p, isNum(s.windows_traded) ? s.windows_traded : 0, false);
    }
    for (const r of slots.retired) {
      if (!r || typeof r !== "object") continue;
      add(
        cleanStr(r.strategy, "unnamed"),
        isNum(r.paper_pnl_dollars) ? r.paper_pnl_dollars : 0,
        isNum(r.windows_traded) ? r.windows_traded : 0,
        true,
      );
    }
    if (slots.strategy_totals) {
      for (const st of slots.strategy_totals) {
        if (!st || typeof st.strategy !== "string") continue;
        if (!map.has(st.strategy)) {
          add(
            st.strategy,
            isNum(st.paper_pnl_dollars) ? st.paper_pnl_dollars : 0,
            isNum(st.windows_traded) ? st.windows_traded : 0,
            false,
          );
        }
      }
    }
    const list = [...map.values()].sort((a, b) => Math.abs(b.pnl) - Math.abs(a.pnl));
    const maxAbs = Math.max(0.01, ...list.map((x) => Math.abs(x.pnl)));
    return list.map((x) => ({
      ...x,
      barPct: Math.max(4, Math.min(100, Math.round((Math.abs(x.pnl) / maxAbs) * 100))),
    }));
  }, [slots]);

  const strikeDistribution = useMemo(() => {
    const counts = [0, 0, 0, 0];
    if (!slots) return counts;
    for (const s of slots.slots) {
      if (!s || typeof s !== "object") continue;
      const k = isNum(s.strikes) ? Math.min(3, Math.max(0, Math.round(s.strikes))) : 0;
      counts[k]++;
    }
    counts[3] += slots.retired.length;
    return counts;
  }, [slots]);

  const firstReportLine = useMemo(() => {
    const line = reportText
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.length > 0);
    return line || "Daily paper report ready.";
  }, [reportText]);

  const handleExport = async (fmt: "md" | "csv") => {
    if (exporting) return;
    setExporting(fmt);
    try {
      const res = await exportDailyReport(fmt, dataset, home, reportText, slots);
      toast.success(`Exported ${res.filename} (${res.bytes.toLocaleString()} bytes).`);
    } catch (e) {
      toast.error(`Export failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setExporting("");
    }
  };

  return (
    <section id="slots-card" aria-label="Autonomous edge allocator and baseline slot ledger" className="space-y-4">
      <AutoAllocatorPanel
        dataset={dataset}
        slots={slots}
        markets={markets}
        liveArmed={liveBundle.status.liveArmed}
        liveStatus={liveBundle.status}
        home={home}
        search={search}
        onStageOrder={onStageOrder}
        onOpenFullAudit={onOpenFullAudit}
      />

      <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-semibold text-ink">Legacy 1/N Equal-Split Baseline Comparison Ledger</h3>
          <ProvenanceTag kind="paper" />
        </div>
        <span className="text-xs text-ink-3">
          Equal-weight taker baseline kept for side-by-side comparison against the Autonomous Edge Allocator above.
        </span>
      </div>

      {/* Equity Curve + Drawdown Underwater Subplot & Stint Attribution */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-12">
        <div className="rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-7">
          <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold text-ink">Equity &amp; Underwater Drawdown</h3>
              <ProvenanceTag kind="paper" />
            </div>
            <span className="mono text-xs text-ink-3">Synchronized hover crosshair</span>
          </div>
          <EquityDrawdownChart points={potPoints} start={potStart} />
        </div>

        <div id="stint-attribution" className="flex flex-col justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-5">
          <div>
            <div className="flex items-center justify-between gap-2">
              <h3 className="text-sm font-semibold text-ink">Stint Attribution Breakdown</h3>
              <span className="mono text-xs text-ink-3">{attribution.length} strategies</span>
            </div>
            <p className="mt-0.5 text-xs text-ink-2">
              Net paper contribution across active slots and rotated stints.
            </p>

            {attribution.length === 0 ? (
              <p className="mt-4 rounded-md border border-dashed border-hairline bg-board/40 px-3 py-4 text-xs text-ink-2">
                No attributed slot stints recorded yet.
              </p>
            ) : (
              <ul className="mt-3 space-y-2.5">
                {attribution.slice(0, 6).map((item) => (
                  <li key={item.strategy} className="space-y-1">
                    <div className="flex items-center justify-between gap-2 text-xs">
                      <span className="min-w-0 truncate font-medium text-ink">{item.strategy}</span>
                      <span
                        className={
                          "mono shrink-0 font-medium " +
                          (item.pnl > 0
                            ? "text-success-foreground"
                            : item.pnl < 0
                              ? "text-destructive-foreground"
                              : "text-ink-2")
                        }
                      >
                        {moneySigned(item.pnl)} ({item.windows}w)
                      </span>
                    </div>
                    <div className="h-1.5 w-full overflow-hidden rounded-full bg-track" aria-hidden="true">
                      <div
                        className={item.pnl >= 0 ? "h-full bg-dot-green" : "h-full bg-dot-red"}
                        style={{ width: `${item.barPct}%` }}
                      />
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="border-t border-hairline pt-3">
            <div className="mb-1.5 flex items-center justify-between text-xs">
              <span className="font-medium text-ink">Strike Standing (0/3 to 3/3)</span>
              <span className="mono text-ink-3">3 losses rotate</span>
            </div>
            <div className="grid grid-cols-4 divide-x divide-hairline border-y border-hairline font-mono text-xs">
              {[0, 1, 2, 3].map((k) => (
                <div key={k} className="px-2 py-1.5 text-center">
                  <div className="text-ink-3">{k}/3</div>
                  <div className={"font-semibold " + (k === 3 && strikeDistribution[k] > 0 ? "text-destructive-foreground" : "text-ink")}>
                    {strikeDistribution[k]}
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>

      {/* Active slots table */}
      {!slotsRead ? (
        <div className="rounded-[10px] border border-hairline bg-surface p-6">
          <div className="h-6 w-48 animate-pulse rounded bg-track" />
          <div className="mt-3 h-24 w-full animate-pulse rounded bg-track" />
        </div>
      ) : !slotsRead.ok ? (
        <div role="alert" className="flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-5">
          <div>
            <p className="text-sm font-medium text-destructive-foreground">Could not read slot allocations.</p>
            <p className="mt-0.5 text-xs text-ink-2">{slotsRead.msg}</p>
          </div>
          <Button variant="outline" className="min-h-11" onClick={onRetry}>
            Retry
          </Button>
        </div>
      ) : (
        <div className="overflow-hidden rounded-[10px] border border-hairline bg-surface">
          <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-2.5">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold text-ink">Compound-Sized Slot Allocation ({activeSlots.length})</h3>
              <ProvenanceTag kind="paper" />
            </div>
            <span className="mono text-xs text-ink-2">
              Pot {money(slots?.paper_equity_dollars)}, 3 consecutive losses rotate a slot
            </span>
          </div>
          {activeSlots.length === 0 ? (
            <p role="status" className="p-5 text-sm text-ink-2">
              No active slots match the current filter.
            </p>
          ) : (
            <Table className="stack">
              <TableHeader>
                <TableRow>
                  <TableHead>Slot</TableHead>
                  <TableHead>Strategy</TableHead>
                  <TableHead>3-Strike Health</TableHead>
                  <TableHead className="text-right">Windows</TableHead>
                  <TableHead className="text-right">Stint P&amp;L</TableHead>
                  <TableHead className="text-right">Slot Equity</TableHead>
                  <TableHead className="text-right">Status</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {activeSlots.map((slot, idx) => {
                  const stintPnl = isNum(slot.stint_pnl_dollars) ? slot.stint_pnl_dollars : slot.paper_pnl_dollars;
                  const eq = isNum(slot.equity_dollars) ? slot.equity_dollars : null;
                  const status = cleanStr(slot.status, "active");
                  return (
                    <TableRow key={`${slot.slot}-${idx}`}>
                      <TableCell className="mono font-medium text-ink">#{slot.slot}</TableCell>
                      <TableCell className="m-grow max-w-[260px] break-all font-medium text-ink">
                        {cleanStr(slot.strategy, "unnamed")}
                      </TableCell>
                      <TableCell>
                        <StrikeTracker strikes={slot.strikes} />
                      </TableCell>
                      <TableCell className="mono text-right text-xs text-ink-2">
                        {isNum(slot.windows_traded) ? slot.windows_traded : 0}
                      </TableCell>
                      <TableCell
                        className={
                          "mono text-right text-sm font-medium " +
                          (isNum(stintPnl)
                            ? stintPnl > 0
                              ? "text-success-foreground"
                              : stintPnl < 0
                                ? "text-destructive-foreground"
                                : "text-ink"
                            : "text-ink-2")
                        }
                      >
                        {moneySigned(stintPnl)}
                      </TableCell>
                      <TableCell className="mono text-right text-sm text-ink">{money(eq)}</TableCell>
                      <TableCell className="m-right text-right">
                        <StatusPill tone="success" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                          {status}
                        </StatusPill>
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}

          {retiredSlots.length > 0 && (
            <div className="border-t border-hairline bg-board/40 px-4 py-3">
              <p className="mb-2 text-xs font-semibold text-ink-2">
                Strike Rotation History ({retiredSlots.length})
              </p>
              <ul className="divide-y divide-hairline border-t border-hairline text-xs">
                {retiredSlots.map((r, i) => (
                  <li key={i} className="flex flex-wrap items-center justify-between gap-2 py-2">
                    <div className="flex flex-wrap items-center gap-2">
                      <StatusPill tone="error" className="min-h-6 px-2 py-0.5 font-mono text-xs text-destructive-foreground">
                        3/3 strikes
                      </StatusPill>
                      <span className="break-all font-medium text-ink">
                        Slot #{r.slot}: {cleanStr(r.strategy, "unnamed")}
                        {r.rotated_to ? ` replaced by ${r.rotated_to}` : ""}
                      </span>
                    </div>
                    <span className="mono text-ink-2">
                      {r.windows_traded} windows, {moneySigned(r.paper_pnl_dollars)}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      {/* Audit Trail with Export (.md / .csv) & Read-Only Venue Account row */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-12">
        <div id="report-card" className="rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-7">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <FileText className="size-4 text-ink-2" aria-hidden />
              <h3 className="text-sm font-semibold text-ink">Daily Report &amp; Audit Trail</h3>
              <ProvenanceTag kind="paper" />
            </div>
            <div className="flex flex-wrap items-center gap-1.5">
              <Button
                id="export-report-md"
                variant="outline"
                className="min-h-11 gap-1.5 px-2.5 text-xs sm:min-h-8"
                disabled={exporting !== ""}
                onClick={() => void handleExport("md")}
              >
                <Download className="size-3.5" aria-hidden />
                <span>{exporting === "md" ? "Exporting…" : "Export .md"}</span>
              </Button>
              <Button
                id="export-report-csv"
                variant="outline"
                className="min-h-11 gap-1.5 px-2.5 text-xs sm:min-h-8"
                disabled={exporting !== ""}
                onClick={() => void handleExport("csv")}
              >
                <Download className="size-3.5" aria-hidden />
                <span>{exporting === "csv" ? "Exporting…" : "Export .csv"}</span>
              </Button>
            </div>
          </div>
          <p className="mt-2 text-xs text-ink-2">{firstReportLine}</p>
          <div id="report-body" className="mt-3 border-t border-hairline pt-2">
            <Disclosure label="Show full report">
              <pre
                id="report"
                className="mono max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-hairline bg-board p-3.5 text-xs text-ink"
              >
                {reportText || (reportRead && !reportRead.ok ? `Report unavailable: ${reportRead.msg}` : "Loading report…")}
              </pre>
            </Disclosure>
          </div>
        </div>

        <div id="account" className="rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-5">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h3 className="text-sm font-semibold text-ink">Connected Kalshi Account</h3>
            <ProvenanceTag kind="venue" />
          </div>
          {liveBundle.status.configured || home?.account ? (
            <>
              <dl className="mt-3 grid grid-cols-3 divide-x divide-hairline border-y border-hairline">
                <div className="px-2 py-2.5">
                  <dt className="text-xs text-ink-3">Cash Balance</dt>
                  <dd className="mono mt-1 text-xs font-semibold text-ink">
                    {money(liveBundle.account.balanceDollars ?? home?.account.balance ?? null)}
                  </dd>
                </div>
                <div className="px-2 py-2.5">
                  <dt className="text-xs text-ink-3">Portfolio / Day</dt>
                  <dd className="mono mt-1 text-xs font-semibold text-ink">
                    {liveBundle.account.portfolioValueDollars !== null
                      ? money(liveBundle.account.portfolioValueDollars)
                      : moneySigned(home?.account.day_pnl ?? null)}
                  </dd>
                </div>
                <div className="px-2 py-2.5">
                  <dt className="text-xs text-ink-3">Pos / Orders</dt>
                  <dd className="mono mt-1 text-xs font-semibold text-ink">
                    {liveBundle.status.configured
                      ? `${liveBundle.positions.length} / ${liveBundle.orders.length}`
                      : Array.isArray(home?.account.open_positions)
                        ? home.account.open_positions.length
                        : "unavailable"}
                  </dd>
                </div>
              </dl>
              <p className="mt-3 text-xs text-ink-2">
                {liveBundle.status.configured
                  ? `${liveBundle.status.environment.toUpperCase()} (${liveBundle.status.liveArmed ? "Live Armed" : "Disarmed, Paper Mode"}) — ${liveBundle.account.note}`
                  : home?.account.note || "Configure Kalshi v2 API credentials in Settings to connect."}
              </p>
              <div className="mt-3 flex flex-wrap items-center gap-2">
                <Button variant="outline" className="min-h-11 px-3 text-xs sm:min-h-8" onClick={onOpenOrderTicket}>
                  Open Order Ticket
                </Button>
                <Button variant="outline" className="min-h-11 px-3 text-xs sm:min-h-8" onClick={onOpenSettings}>
                  API &amp; Risk Rails
                </Button>
              </div>
            </>
          ) : (
            <p className="mt-3 text-xs text-ink-2">
              {homeRead && !homeRead.ok ? `Venue state unavailable (${homeRead.msg}).` : "Checking Kalshi account…"}
            </p>
          )}
        </div>
      </div>
    </section>
  );
}
