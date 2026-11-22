// Slot Desk workbench tab: compound-sized slot allocation, 3-strike Tremor Tracker health, retired rotations,
// plus Daily Report & Audit Trail (Cult UI Expandable) and Read-Only Venue Account panel.
import { useMemo } from "react";
import { ChevronDown, FileText } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { Tracker, type TrackerBlockProps } from "@/components/Tracker/Tracker";
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";
import { ProvenanceTag } from "@/Charts";
import {
  type Home,
  type Read,
  type Report,
  type Slot,
  type Slots,
  cleanStr,
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
  homeRead,
  reportRead,
  search,
  onRetry,
}: {
  slotsRead?: Read<Slots>;
  homeRead?: Read<Home>;
  reportRead?: Read<Report>;
  search: string;
  onRetry: () => void;
}) {
  const slots = slotsRead?.ok ? slotsRead.v : null;
  const home = homeRead?.ok ? homeRead.v : null;
  const reportText = reportRead?.ok ? reportRead.v.report : "";

  const activeSlots = useMemo(() => {
    if (!slots) return [];
    const q = search.trim().toLowerCase();
    return slots.slots
      .filter((s): s is Slot => !!s && typeof s === "object")
      .filter((s) => !q || cleanStr(s.strategy, "").toLowerCase().includes(q) || `slot ${s.slot}`.includes(q));
  }, [slots, search]);

  const retiredSlots = useMemo(() => {
    if (!slots) return [];
    return slots.retired.filter((s): s is Slot => !!s && typeof s === "object");
  }, [slots]);

  const firstReportLine = useMemo(() => {
    const line = reportText
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.length > 0);
    return line || "Daily paper report ready.";
  }, [reportText]);

  return (
    <section id="slots-card" aria-label="Slot compounding desk and audit trail" className="space-y-4">
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
                Rotated / Retired Stints ({retiredSlots.length})
              </p>
              <ul className="space-y-1.5 text-xs">
                {retiredSlots.map((r, i) => (
                  <li key={i} className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-hairline bg-surface px-3 py-2">
                    <span className="break-all font-medium text-ink">
                      Slot #{r.slot} · {cleanStr(r.strategy, "unnamed")}
                      {r.rotated_to ? ` -> ${r.rotated_to}` : ""}
                    </span>
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

      {/* Audit Trail & Read-Only Venue Account row */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-12">
        <div id="report-card" className="rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-7">
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <FileText className="size-4 text-ink-2" aria-hidden />
              <h3 className="text-sm font-semibold text-ink">Daily Report &amp; Audit Trail</h3>
            </div>
            <ProvenanceTag kind="paper" />
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
