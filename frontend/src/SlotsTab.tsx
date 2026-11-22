// Slot Desk workbench tab: compounding paper equity + underwater drawdown subplot, stint attribution
// breakdown, compound-sized slot allocation with 0/3..3/3 strike history, one-click .md/.csv report
// export (GET /api/export/report), verbatim audit trail (Cult UI Expandable), and Read-Only Venue Account.
import { useMemo, useState } from "react";
import { ChevronDown, Download, FileText } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { Tracker, type TrackerBlockProps } from "@/components/Tracker/Tracker";
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";
import { EquityDrawdownChart, ProvenanceTag, validPoints } from "@/Charts";
import {
  type Dataset,
  type Equity,
  type Home,
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

function StrikeTracker({ strikes = 0 }: { strikes?: number }) {
  const n = isNum(strikes) ? Math.min(3, Math.max(0, Math.round(strikes))) : 0;
  const data: TrackerBlockProps[] = [0, 1, 2].map((i) => ({
    color: i < n ? "bg-dot-red" : "bg-dot-green",
    tooltip: i < n ? `Strike ${i + 1} of 3` : "Clean slot standing",
  }));
  return (
    <div className="flex items-center gap-2">
      <div className="w-12 shrink-0">
        <Tracker data={data} />
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
  dataset,
  search,
  onRetry,
}: {
  slotsRead?: Read<Slots>;
  equityRead?: Read<Equity>;
  homeRead?: Read<Home>;
  reportRead?: Read<Report>;
  dataset: Dataset;
  search: string;
  onRetry: () => void;
}) {
  const [exporting, setExporting] = useState<"" | "md" | "csv">("");
  const slots = slotsRead?.ok ? slotsRead.v : null;
  const equity = equityRead?.ok ? equityRead.v : null;
  const home = homeRead?.ok ? homeRead.v : null;
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
    <section id="slots-card" aria-label="Slot compounding desk and audit trail" className="space-y-4">
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
                        {moneySigned(item.pnl)} · {item.windows}w
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
            <div className="grid grid-cols-4 gap-1.5 font-mono text-xs">
              {[0, 1, 2, 3].map((k) => (
                <div key={k} className="rounded-md border border-hairline bg-board px-2 py-1.5 text-center">
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
              Pot {money(slots?.paper_equity_dollars)} · 3 consecutive losses rotate a slot
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
                  <TableHead>Rune Strategy</TableHead>
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
                        <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                          <PillStatus className="border-r-0 pr-0">
                            <PillIndicator variant="success" />
                            {status}
                          </PillStatus>
                        </Pill>
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}

          {retiredSlots.length > 0 && (
            <div className="border-t border-hairline bg-board/40 px-4 py-3">
              <p className="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-3">
                Strike Rotation History ({retiredSlots.length})
              </p>
              <ul className="space-y-1.5 text-xs">
                {retiredSlots.map((r, i) => (
                  <li key={i} className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-hairline bg-surface px-3 py-2">
                    <div className="flex flex-wrap items-center gap-2">
                      <Pill variant="outline" className="min-h-6 px-2 py-0.5 font-mono text-xs text-destructive-foreground">
                        3/3 strikes
                      </Pill>
                      <span className="break-all font-medium text-ink">
                        Slot #{r.slot} · {cleanStr(r.strategy, "unnamed")}
                        {r.rotated_to ? ` -> ${r.rotated_to}` : ""}
                      </span>
                    </div>
                    <span className="mono text-ink-2">
                      {r.windows_traded} windows · {moneySigned(r.paper_pnl_dollars)}
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
          <div id="report-body" className="mt-3">
            <Expandable expandDirection="vertical" expandBehavior="push" className="w-full">
              <ExpandableTrigger className="inline-flex min-h-11 cursor-pointer items-center gap-1.5 rounded-md border border-hairline bg-board px-3 text-xs font-medium text-ink outline-none hover:bg-track focus-visible:ring-2 focus-visible:ring-ring">
                <span>Show full report</span>
                <ChevronDown className="size-3.5 text-ink-2" aria-hidden />
              </ExpandableTrigger>
              <ExpandableContent preset="fade">
                <pre
                  id="report"
                  className="mono mt-3 max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-hairline bg-board p-3.5 text-xs text-ink"
                >
                  {reportText || (reportRead && !reportRead.ok ? `Report unavailable: ${reportRead.msg}` : "Loading report…")}
                </pre>
              </ExpandableContent>
            </Expandable>
          </div>
        </div>

        <div id="account" className="rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-5">
          <div className="flex items-center justify-between gap-2">
            <h3 className="text-sm font-semibold text-ink">Venue Account (Read-only)</h3>
            <ProvenanceTag kind="venue" />
          </div>
          {home?.account ? (
            <>
              <dl className="mt-3 grid grid-cols-3 gap-2">
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Balance</dt>
                  <dd className="mono mt-1 text-xs font-semibold text-ink">{money(home.account.balance)}</dd>
                </div>
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Day P&amp;L</dt>
                  <dd className="mono mt-1 text-xs font-semibold text-ink">{moneySigned(home.account.day_pnl)}</dd>
                </div>
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Positions</dt>
                  <dd className="mono mt-1 text-xs font-semibold text-ink">
                    {Array.isArray(home.account.open_positions) ? home.account.open_positions.length : "unavailable"}
                  </dd>
                </div>
              </dl>
              <p className="mt-3 text-xs text-ink-2">{home.account.note}</p>
            </>
          ) : (
            <p className="mt-3 text-xs text-ink-2">
              {homeRead && !homeRead.ok ? `Venue state unavailable (${homeRead.msg}).` : "Checking venue account…"}
            </p>
          )}
        </div>
      </div>
    </section>
  );
}
