// Tape health, venue account and slots: bands of the one instrument surface.
import { type ReactNode, useState } from "react";
import { toast } from "sonner";
import { BorderTrail } from "@/components/ui/border-trail";
import { Button } from "@/components/ui/button";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { SlidingNumber } from "@/components/ui/sliding-number";
import { TextMorph } from "@/components/ui/text-morph";
import { Tracker } from "@/components/Tracker/Tracker";
import { type Home, type Read, type Slots, ageWords, money } from "@/lib/desk";

export function PanelError({ msg, onRetry }: { msg: string; onRetry: () => void }) {
  return (
    <div role="alert" className="flex flex-wrap items-center gap-3 rounded-md border border-unread/40 bg-unread/10 px-3 py-2 text-sm text-ink">
      <p className="min-w-0 flex-1 break-words">This part did not load ({msg}). The rest of the page is unaffected.</p>
      <Button variant="outline" size="sm" className="min-h-11" onClick={onRetry}>Retry</Button>
    </div>
  );
}

function Stat({ label, id, children }: { label: string; id: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-sm text-ink-2">{label}</div>
      <div id={id} className="mt-1 min-w-0 break-words font-mono text-2xl font-normal tabular-nums text-ink">{children}</div>
    </div>
  );
}

const UNAVAILABLE = <span className="text-ink-3">unavailable</span>;
const count = (v: number | null | undefined) => (v === null || v === undefined ? UNAVAILABLE : <SlidingNumber value={v} />);

function PanelHead({ id, title, tag }: { id: string; title: string; tag: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <h2 id={id} tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">{title}</h2>
      {tag}
    </div>
  );
}

export function TapeHealth({ read, onRetry, onCollected }: { read: Read<Home> | null; onRetry: () => void; onCollected: () => void }) {
  const [collecting, setCollecting] = useState(false);
  const [status, setStatus] = useState("");
  const home = read && read.ok ? read.v : null;
  const c = home?.collector;

  async function collect() {
    setCollecting(true);
    setStatus("");
    let problem = "";
    try {
      const r = await fetch("/api/collect", { method: "POST" });
      const body = await r.json().catch(() => null);
      if (!r.ok) problem = (body && body.error) || "the desk answered " + r.status;
      else if (body && body.error) problem = body.error;
    } catch (e) {
      problem = e instanceof Error ? e.message : String(e);
    }
    if (problem) {
      setStatus("Could not collect right now. Nothing was written. Try again in a minute. Server said: " + problem);
      toast.error("Collect failed. Nothing was written.");
    } else {
      toast.success("Collected one pass into the tape.");
    }
    setCollecting(false);
    onCollected();
  }

  const failed = c?.series_failed_last_pass ?? 0;
  const names = c?.series_failed_names ?? [];
  const healthy = !!c && c.collecting && c.dropped_lines === 0 && c.conflicting_settlements === 0 && !failed;
  const word = !c ? "Checking" : c.snapshots_on_disk === 0 ? "Empty" : !c.collecting ? "Stale" : healthy ? "Healthy" : "Collecting with issues";
  const note = !c ? "Checking the tape."
    : c.snapshots_on_disk === 0
      ? "Empty: no tape has been written yet, so nothing is scored. The collector writes its first pass on start, or when you press Collect now. Windows cannot be backfilled."
      : !c.collecting
        ? "Stale: the newest snapshot is older than two collect intervals, so the collector is not collecting. Uncollected windows cannot be backfilled."
        : healthy
          ? "Healthy: collecting, a pass lands every " + c.collect_interval_secs + "s. The tape is the forward record, and windows are scored from what was collected before they settled."
          : "Collecting, with " + (c.dropped_lines ? c.dropped_lines + " dropped line(s)" : "") + (c.dropped_lines && c.conflicting_settlements ? " and " : "") + (c.conflicting_settlements ? c.conflicting_settlements + " window(s) with conflicting settlements, which are not scored" : "") + (failed ? ((c.dropped_lines || c.conflicting_settlements) ? " and " : "") + failed + " of " + c.series_total + " market reads failed on the last pass" : "") + ".";
  const dot = !c ? "bg-track" : c.collecting ? (healthy ? "bg-info-foreground" : "bg-dot-amber") : "bg-unread";

  return (
    <section id="tape" aria-labelledby="tape-title" className="relative min-w-0 scroll-mt-28 bg-surface p-5 lg:p-6">
      {collecting && <BorderTrail className="bg-blue" size={120} />}
      <PanelHead id="tape-title" title="Collector, the forward tape" tag={<Pill variant="outline" className="text-ink-2">tape</Pill>} />
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      <div className="mt-4 flex items-start gap-3 text-sm text-ink-2">
        <span id="c-dot" aria-hidden className={"mt-1.5 size-2 shrink-0 rounded-full " + dot} />
        <div className="min-w-0">
          <TextMorph as="span" className="font-medium text-ink">{word}</TextMorph>
          <p id="c-note" className="mt-0.5">{note}</p>
          {!!failed && names.length > 0 && <p id="c-fail" className="mt-1 break-words">Failed series: {names.join(", ")}.</p>}
        </div>
      </div>
      <div className="mt-5 grid grid-cols-2 gap-x-6 gap-y-5">
        <Stat label="Tape lines" id="c-snaps">{count(c?.snapshots_on_disk)}</Stat>
        <Stat label="Markets seen" id="c-markets">{count(c?.markets_seen)}</Stat>
        <Stat label="Settled windows" id="c-settled">{count(c?.settled_windows)}</Stat>
        <Stat label="Dropped lines" id="c-dropped">{count(c?.dropped_lines)}</Stat>
        <Stat label="Conflicting settlements" id="c-conflicts">{count(c?.conflicting_settlements)}</Stat>
        <Stat label="Newest snapshot" id="c-age">{c ? ageWords(c.newest_snapshot_age_secs) : UNAVAILABLE}</Stat>
      </div>
      <div className="mt-6 flex flex-wrap items-center gap-3">
        <Button id="collect-btn" variant="default" className="min-h-11" disabled={collecting} onClick={collect}>{collecting ? "Collecting…" : "Collect now"}</Button>
        <p id="collect-status" role="status" className="min-w-0 basis-full break-words text-sm text-unread empty:hidden">{status}</p>
      </div>
    </section>
  );
}

const READINGS = ["Balance", "Day P&L", "Open positions", "Capital at risk"];

export function AccountPanel({ read, onRetry }: { read: Read<Home> | null; onRetry: () => void }) {
  const home = read && read.ok ? read.v : null;
  const a = home?.account;
  const allNull = !!a && a.balance === null && a.day_pnl === null && a.open_positions === null;
  const pnlClass = a && a.day_pnl !== null ? (a.day_pnl > 0 ? "text-dot-green" : a.day_pnl < 0 ? "text-unread" : "text-ink") : "text-ink";
  return (
    <section id="account" aria-labelledby="account-title" className="min-w-0 scroll-mt-28 bg-surface p-5 lg:p-6">
      <PanelHead id="account-title" title="Venue account" tag={<Pill variant="outline" className="text-info-foreground">venue</Pill>} />
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      {allNull ? (
        <>
          {/* The one statement of the no-key fact: the server's own sentence. */}
          <p id="account-note" className="mt-4 max-w-[60ch] text-sm text-ink-2">{a?.note ?? ""}</p>
          <dl className="mt-4 grid gap-0">
            {READINGS.map((k) => (
              <div key={k} className="flex min-h-11 items-center justify-between gap-3 border-t border-hairline first:border-t-0">
                <dt className="text-sm text-ink">{k}</dt>
                <dd className="flex items-center gap-2 font-mono text-sm">
                  <span className="text-ink-3">unavailable</span>
                  <Pill variant="outline" className="h-auto text-ink-2">no key</Pill>
                </dd>
              </div>
            ))}
          </dl>
        </>
      ) : (
        <>
          <div className="mt-4 grid grid-cols-2 gap-x-6 gap-y-5">
            <Stat label="Balance" id="balance">{a?.balance === null || a?.balance === undefined ? UNAVAILABLE : money(a.balance)}</Stat>
            <Stat label="Day P&L" id="pnl"><span className={pnlClass}>{a?.day_pnl === null || a?.day_pnl === undefined ? "unavailable" : money(a.day_pnl)}</span></Stat>
            <Stat label="Open positions" id="positions">{a?.open_positions === null || a?.open_positions === undefined ? UNAVAILABLE : a.open_positions.length}</Stat>
            <Stat label="Capital at risk" id="risk">{a?.open_positions === null || a?.open_positions === undefined ? UNAVAILABLE : a.open_positions.length === 0 ? "$0.00 (no open positions)" : "see positions"}</Stat>
          </div>
          <p id="account-note" className="mt-4 text-sm text-ink-2">{a?.note ?? ""}</p>
        </>
      )}
    </section>
  );
}

export function SlotsPanel({ read, onRetry, notes }: { read: Read<Slots> | null; onRetry: () => void; notes?: string[] }) {
  const s = read && read.ok ? read.v : null;
  const rows = s ? [...s.slots.map((x) => ({ ...x, retired: false })), ...s.retired.map((x) => ({ ...x, retired: true }))] : [];
  return (
    <section id="slots-card" aria-labelledby="slots-title" className="min-w-0 scroll-mt-28 bg-surface p-5 lg:p-6">
      <PanelHead id="slots-title" title="Slots, paper only" tag={<Pill variant="outline" className="text-blue">paper</Pill>} />
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      {!s && !(read && !read.ok) && <span className="mt-4 block h-24 rounded-md bg-track" aria-hidden />}
      {s && rows.length === 0 && (
        <p className="mt-4 text-sm text-ink-2">No slots yet. The strategy library in strategies/ is empty, so there is nothing to score. Add a .rn file there and reload.</p>
      )}
      {s && rows.length > 0 && (
        <div className="mt-4 min-w-0 overflow-x-auto">
          <Table className="stack md:min-w-[640px]">
            <TableHeader>
              <TableRow>
                <TableHead>Slot</TableHead>
                <TableHead>Strategy</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Strikes</TableHead>
                <TableHead className="text-right">Stint P&amp;L</TableHead>
                <TableHead className="text-right">Equity</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((r) => {
                const stint = r.retired ? r.paper_pnl_dollars : r.stint_pnl_dollars ?? 0;
                const cls = stint > 0 ? "text-dot-green" : stint < 0 ? "text-unread" : "text-ink";
                return (
                  <TableRow key={(r.retired ? "r" : "s") + r.slot} className={r.retired ? "text-ink-2" : undefined}>
                    <TableCell data-label="Slot" className="stack-first font-mono">{r.slot + 1}</TableCell>
                    <TableCell data-label="Strategy" className="max-w-56 font-mono"><span className="block break-all md:truncate" title={r.strategy}>{r.strategy}</span></TableCell>
                    <TableCell data-label="Status">
                      <Pill variant="outline" className={"h-auto " + (r.retired ? "text-ink-2" : "text-dot-green")}>
                        <PillStatus><PillIndicator variant={r.retired ? "warning" : "success"} />{r.retired ? "retired" : r.status ?? "active"}</PillStatus>
                      </Pill>
                    </TableCell>
                    <TableCell data-label="Strikes">
                      {r.retired ? <span>{r.windows_traded} windows</span> : (
                        <div className="flex items-center gap-2">
                          <Tracker data={[0, 1, 2].map((i) => ({ key: i, color: i < (r.strikes ?? 0) ? "bg-unread" : "bg-track", tooltip: "Strike " + (i + 1) + " of 3" }))} className="h-2 w-20" />
                          <span className="font-mono text-sm text-ink-2">{r.strikes ?? 0}/3</span>
                        </div>
                      )}
                    </TableCell>
                    <TableCell data-label="Stint P&L" className={"text-right font-mono tabular-nums " + cls}>
                      {money(stint)}{r.retired ? "" : <span className="text-ink-2"> · {r.stint_windows ?? 0} windows</span>}
                    </TableCell>
                    <TableCell data-label="Equity" className="text-right font-mono tabular-nums text-ink">{r.retired ? "retired" : money(r.equity_dollars ?? 0)}</TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
          <p id="slots-note" className="mt-3 max-w-[90ch] text-sm text-ink-2">Paper simulation: the pot is re-split evenly across {s.slots.length} slots each window. Three consecutive losing windows retire a slot’s strategy and rotate the next one in. Paper only, not venue money.</p>
          {notes && notes.length > 0 && (
            <ul id="slot-notes" className="mt-2 list-disc pl-5 text-sm text-ink-2">{notes.map((n) => <li key={n} className="break-words">{n}</li>)}</ul>
          )}
        </div>
      )}
    </section>
  );
}
