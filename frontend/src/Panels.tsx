// The ledger's readings strip, the venue account and the slots table: bands of the one instrument surface.
import { type ReactNode, useState } from "react";
import { toast } from "sonner";
import { BorderTrail } from "@/components/ui/border-trail";
import { Button } from "@/components/ui/button";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { TextMorph } from "@/components/ui/text-morph";
import { Tracker } from "@/components/Tracker/Tracker";
import NumberFlow from "@number-flow/react";
import { type Equity, type Home, type Read, type Slots, ageWords, money } from "@/lib/desk";
import { Spark } from "@/Spark";

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
      <div id={id} className="mt-1 min-w-0 break-words font-mono text-xl font-normal tabular-nums text-ink md:text-2xl">{children}</div>
    </div>
  );
}

const UNAVAILABLE = <span className="text-ink-3">unavailable</span>;

function PanelHead({ id, title, tag }: { id: string; title: string; tag: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <h2 id={id} tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">{title}</h2>
      {tag}
    </div>
  );
}

// The ledger's first row: six readings a trader scans in one pass. Each cell says where its figure comes from (paper, tape, venue).
export function Readings({ home, slots, equity, onRetry, onCollected }: {
  home: Read<Home> | null; slots: Read<Slots> | null; equity: Read<Equity> | null; onRetry: () => void; onCollected: () => void;
}) {
  const [collecting, setCollecting] = useState(false);
  const [status, setStatus] = useState("");
  const h = home && home.ok ? home.v : null;
  const s = slots && slots.ok ? slots.v : null;
  const eq = equity && equity.ok ? equity.v : null;
  const c = h?.collector;

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
  const word = !c ? "Checking" : c.snapshots_on_disk === 0 ? "Empty" : !c.collecting ? "Stale" : healthy ? "Healthy" : "Collecting";
  const dot = !c ? "bg-track" : c.collecting ? (healthy ? "bg-info-foreground" : "bg-dot-amber") : "bg-unread";
  const pot = s ? s.paper_equity_dollars : null;
  const start = s ? s.paper_pot_start : 0;
  const target = (s?.stage_target ?? h?.stage_target)?.target_dollars ?? 1000;
  const pct = pot === null ? 0 : Math.max(0, Math.min(100, (pot / target) * 100));
  const change = pot === null ? 0 : pot - start;
  const cell = "min-w-0 bg-surface px-4 py-3";
  const label = "flex items-center justify-between gap-2 text-sm text-ink-2";
  const big = "mt-1 font-mono text-xl tabular-nums text-ink";
  const small = "mt-0.5 text-sm tabular-nums text-ink-2";
  const skeleton = <span className="mt-2 block h-6 w-24 rounded-sm bg-track" aria-hidden />;

  return (
    <section id="readings" aria-labelledby="readings-title" className="relative min-w-0 scroll-mt-28 bg-surface">
      {collecting && <BorderTrail className="bg-blue" size={120} />}
      <h2 id="readings-title" tabIndex={-1} className="sr-only">Readings</h2>
      {((home && !home.ok) || (slots && !slots.ok)) && (
        <div className="p-4"><PanelError msg={home && !home.ok ? home.msg : slots && !slots.ok ? slots.msg : ""} onRetry={onRetry} /></div>
      )}
      <div className="grid grid-cols-2 gap-px bg-hairline md:grid-cols-3 xl:grid-cols-6">
        <div id="pot" className={cell}>
          <p className={label}>Paper pot<span className="text-xs text-warning-foreground">paper</span></p>
          {pot === null ? skeleton : (
            <>
              <p id="pot-figure" className={big}><NumberFlow value={pot} format={{ style: "currency", currency: "USD" }} locales="en-US" /></p>
              <p id="pot-change" className={small + " " + (change > 0 ? "text-success-foreground" : change < 0 ? "text-unread" : "")}>{(change >= 0 ? "+" : "-") + "$" + Math.abs(change).toFixed(2)} since the ${start.toFixed(2)} start</p>
              <div className="mt-2"><Spark points={eq?.pot ?? []} start={start} w={140} h={24} label="Paper pot over the tape" /></div>
            </>
          )}
        </div>
        <div className={cell}>
          <p className={label}>Stage target<span className="text-xs text-warning-foreground">paper</span></p>
          {pot === null ? skeleton : (
            <>
              <p id="stage-figure" className={big}><NumberFlow value={Math.round(pct)} suffix="%" /></p>
              <p id="stage-line" className={small}>{pot >= target ? "reached " : money(target - pot) + " to go of "}{money(target)}</p>
            </>
          )}
        </div>
        <div className={cell}>
          <p className={label}>Tape lines<span className="text-xs">tape</span></p>
          {!c ? skeleton : (
            <>
              <p id="c-snaps" className={big}><NumberFlow value={c.snapshots_on_disk} /></p>
              <p id="c-markets" className={small}>{c.markets_seen ?? "unavailable"} markets seen</p>
            </>
          )}
        </div>
        <div className={cell}>
          <p className={label}>Settled windows<span className="text-xs">tape</span></p>
          {!c ? skeleton : (
            <>
              <p id="c-settled" className={big}>{c.settled_windows ?? <span className="font-sans text-ink-3">unavailable</span>}</p>
              <p id="c-dropped" className={small}>{c.dropped_lines ?? 0} dropped, {c.conflicting_settlements ?? 0} conflicting</p>
            </>
          )}
        </div>
        <div className={cell + " flex flex-col gap-2"}>
          <div>
            <p className={label}>Collector<span className="text-xs">tape</span></p>
            {!c ? skeleton : (
              <p className="mt-1 flex items-center gap-2 text-base text-ink">
                <span id="c-dot" aria-hidden className={"size-2 shrink-0 rounded-full " + dot} />
                <TextMorph as="span" className="font-medium">{collecting ? "Collecting now" : word}</TextMorph>
              </p>
            )}
            <p id="c-age" className={small}>newest {c ? ageWords(c.newest_snapshot_age_secs) : "unavailable"}</p>
          </div>
          <Button id="collect-btn" className="min-h-11 self-start" disabled={collecting} onClick={collect}>{collecting ? "Collecting…" : "Collect now"}</Button>
        </div>
        <div className={cell}>
          <p className={label}>Venue balance<span className="text-xs text-info-foreground">venue</span></p>
          {!h ? skeleton : (
            <>
              <p id="strip-balance" className={big}>{h.account.balance === null ? <span className="font-sans text-ink-3">unavailable</span> : money(h.account.balance)}</p>
              <p className="mt-0.5 text-sm text-ink-2">{h.account.balance === null ? "no venue key on this desk" : "read from Kalshi"}</p>
            </>
          )}
        </div>
      </div>
      {(!!failed || !!status || (c && !healthy && c.snapshots_on_disk > 0)) && (
        <div className="grid gap-1 border-t border-hairline px-4 py-3 text-sm text-ink-2">
          {!!failed && <p id="c-fail" className="break-words">{failed} of {c?.series_total ?? "the"} market reads failed on the last pass{names.length ? ": " + names.join(", ") : ""}. Those series are missing from this pass of the tape.</p>}
          {c && !c.collecting && c.snapshots_on_disk > 0 && <p>Stale: the newest snapshot is older than two collect intervals. Windows that were not collected cannot be backfilled.</p>}
          {!!status && <p id="collect-status" role="status" className="break-words text-unread">{status}</p>}
        </div>
      )}
      {c && c.snapshots_on_disk === 0 && (
        <p className="border-t border-hairline px-4 py-3 text-sm text-ink-2">No tape has been written yet, so nothing is scored. The collector writes its first pass on start, or when you press Collect now.</p>
      )}
    </section>
  );
}

const READINGS = ["Balance", "Day P&L", "Open positions", "Capital at risk"];

export function AccountPanel({ read, onRetry }: { read: Read<Home> | null; onRetry: () => void }) {
  const home = read && read.ok ? read.v : null;
  const a = home?.account;
  const allNull = !!a && a.balance === null && a.day_pnl === null && a.open_positions === null;
  const pnlClass = a && a.day_pnl !== null ? (a.day_pnl > 0 ? "text-success-foreground" : a.day_pnl < 0 ? "text-unread" : "text-ink") : "text-ink";
  return (
    <section id="account" aria-labelledby="account-title" className="min-w-0 scroll-mt-28 bg-surface p-4 md:p-5 lg:p-6">
      <PanelHead id="account-title" title="Venue account" tag={<Pill variant="outline" className="text-info-foreground">venue</Pill>} />
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      {allNull ? (
        <>
          {/* The one statement of the no-key fact: the server's own sentence. */}
          <p id="account-note" className="mt-4 max-w-[60ch] text-sm text-ink-2">{a?.note ?? ""}</p>
          <dl className="mt-4 grid gap-0">
            {READINGS.map((k) => (
              <div key={k} className="flex min-h-9 items-center justify-between gap-3 border-t border-hairline first:border-t-0 md:min-h-11">
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

export function SlotsPanel({ read, equity, onRetry, notes }: { read: Read<Slots> | null; equity: Equity | null; onRetry: () => void; notes?: string[] }) {
  const s = read && read.ok ? read.v : null;
  const seriesOf = (name: string) => equity?.series.find((x) => x.strategy === name)?.points ?? [];
  const rows = s ? [...s.slots.map((x) => ({ ...x, retired: false })), ...s.retired.map((x) => ({ ...x, retired: true }))] : [];
  return (
    <section id="slots-card" aria-labelledby="slots-title" className="min-w-0 scroll-mt-28 bg-surface p-4 md:p-5 lg:p-6">
      <PanelHead id="slots-title" title="Slots, paper only" tag={<Pill variant="outline" className="text-warning-foreground">paper</Pill>} />
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
                <TableHead className="hidden md:table-cell">Paper P&amp;L over the tape</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((r) => {
                const stint = r.retired ? r.paper_pnl_dollars : r.stint_pnl_dollars ?? 0;
                const cls = stint > 0 ? "text-success-foreground" : stint < 0 ? "text-unread" : "text-ink";
                return (
                  <TableRow key={(r.retired ? "r" : "s") + r.slot} className={r.retired ? "text-ink-2" : undefined}>
                    <TableCell data-label="Slot" className="stack-first c-slot font-mono">{r.slot + 1}</TableCell>
                    <TableCell data-label="Strategy" className="c-name max-w-56 font-mono"><span className="block break-words md:truncate" title={r.strategy}>{r.strategy}</span></TableCell>
                    <TableCell data-label="Status" className="c-status">
                      <Pill variant="outline" className={"h-auto " + (r.retired ? "text-ink-2" : "text-success-foreground")}>
                        <PillStatus className="border-r-0 pr-0"><PillIndicator variant={r.retired ? "warning" : "success"} />{r.retired ? "retired" : r.status ?? "active"}</PillStatus>
                      </Pill>
                    </TableCell>
                    <TableCell data-label="Strikes" className="c-strikes">
                      {r.retired ? <span>{r.windows_traded} windows</span> : (
                        <div className="flex items-center gap-2">
                          <Tracker data={[0, 1, 2].map((i) => ({ key: i, color: i < (r.strikes ?? 0) ? "bg-unread" : "bg-track", tooltip: "Strike " + (i + 1) + " of 3" }))} className="h-2 w-20" />
                          <span className="font-mono text-sm text-ink-2">{r.strikes ?? 0}/3</span>
                        </div>
                      )}
                    </TableCell>
                    <TableCell data-label="Stint P&L" className={"c-pnl text-right font-mono tabular-nums " + cls}>
                      {money(stint)}{r.retired ? "" : <span className="text-ink-2"> · {r.stint_windows ?? 0} windows</span>}
                    </TableCell>
                    <TableCell data-label="Equity" className="c-equity text-right font-mono tabular-nums text-ink">{r.retired ? "retired" : money(r.equity_dollars ?? 0)}</TableCell>
                    <TableCell className="hidden md:table-cell max-md:!hidden">{r.retired ? null : <Spark points={seriesOf(r.strategy)} start={0} w={120} h={24} label={r.strategy + ", cumulative paper P&L"} />}</TableCell>
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
