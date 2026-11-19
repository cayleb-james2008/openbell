// Strategies against the promotion standard, and the collected markets: tables with one search field each.
import { useState } from "react";
import { Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { Tracker } from "@/components/Tracker/Tracker";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { type Dataset, type Market, type Read, type Strategy } from "@/lib/desk";
import { PanelError } from "@/Panels";

type Found<T> = Read<T> | null;

function SearchField({ label, value, onChange }: { label: string; value: string; onChange: (v: string) => void }) {
  return (
    <InputGroup className="max-w-sm">
      <InputGroupAddon><Search className="size-4 text-icon" strokeWidth={1.5} aria-hidden /></InputGroupAddon>
      <InputGroupInput type="search" aria-label={label} placeholder={label} value={value} onChange={(e) => onChange(e.target.value)} />
    </InputGroup>
  );
}

function VerdictPill({ verdict, fixture }: { verdict: string; fixture: boolean }) {
  if (fixture && verdict.startsWith("PROVEN")) {
    return <Pill variant="outline" className="text-blue">meets the bar on the example tape (not forward evidence)</Pill>;
  }
  if (verdict.startsWith("PROVEN")) {
    return <Pill variant="outline" className="text-dot-green"><PillStatus><PillIndicator variant="success" />{verdict}</PillStatus></Pill>;
  }
  if (verdict.startsWith("NOT PROVEN")) {
    return <Pill variant="outline" className="h-auto whitespace-normal text-unread"><PillStatus><PillIndicator variant="error" />{verdict}</PillStatus></Pill>;
  }
  return <Pill variant="outline" className="text-ink-2">{verdict}</Pill>;
}

export function Strategies({ read, libraryErrors, dataset, onRetry }: { read: Found<Strategy[]>; libraryErrors: { file: string; error: string }[]; dataset: Dataset; onRetry: () => void }) {
  const [q, setQ] = useState("");
  const all = read && read.ok ? read.v : null;
  const needle = q.trim().toLowerCase();
  const shown = all ? all.filter((s) => s.strategy.toLowerCase().includes(needle)) : [];
  return (
    <section id="strategies" aria-labelledby="strategies-title" className="scroll-mt-28 rounded-[10px] border border-hairline bg-surface p-5">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id="strategies-title" tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">Strategies against the promotion standard</h2>
        <Pill variant="outline" className="text-blue">paper</Pill>
      </div>
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      <p className="mt-3 max-w-[70ch] text-sm text-ink-2">A strategy is proven only after 100 forward windows, both chronological halves positive, and +3.5 cents per contract after fees. Until then it is unproven, and the verdict says so.</p>
      <div className="mt-4"><SearchField label="Search strategies" value={q} onChange={setQ} /></div>
      {!all && !(read && !read.ok) && <span className="mt-4 block h-24 rounded-md bg-track" aria-hidden />}
      {all && (
        <div className="mt-4 overflow-x-auto">
          <Table className="min-w-[620px]">
            <TableHeader>
              <TableRow>
                <TableHead>Strategy</TableHead>
                <TableHead>Side</TableHead>
                <TableHead>Windows traded, of 100</TableHead>
                <TableHead className="text-right">Mean cents per contract</TableHead>
                <TableHead>Verdict</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {shown.map((s) => {
                const need = s.standard ? s.standard.min_windows : 100;
                const done = Math.min(s.windows_traded, need);
                const label = s.windows_traded >= need ? s.windows_traded + " windows (" + need + " needed)" : s.windows_traded + " of " + need;
                return (
                  <TableRow key={s.strategy}>
                    <TableCell className="max-w-56 font-mono"><span className="block truncate" title={s.strategy}>{s.strategy}</span></TableCell>
                    <TableCell className="font-mono">{s.side}</TableCell>
                    <TableCell>
                      <div className="flex min-w-40 items-center gap-3">
                        <Tracker data={Array.from({ length: 10 }, (_, i) => ({ key: i, color: (i + 1) * need / 10 <= done ? "bg-info-foreground" : "bg-track", tooltip: "Windows " + (i * need / 10 + 1) + " to " + ((i + 1) * need / 10) }))} className="h-2 w-28" />
                        <span className="whitespace-nowrap font-mono text-sm text-ink-2">{label}</span>
                      </div>
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">{s.mean_cents_per_contract === null ? "-" : s.mean_cents_per_contract.toFixed(2)}</TableCell>
                    <TableCell><VerdictPill verdict={s.verdict} fixture={dataset === "fixture"} /></TableCell>
                  </TableRow>
                );
              })}
              {!all.length && (
                <TableRow><TableCell colSpan={5} className="text-ink-2">No strategies in the library. Each strategy is a small .rn file in strategies/, and the desk scores whatever is there.</TableCell></TableRow>
              )}
              {!!all.length && !shown.length && (
                <TableRow><TableCell colSpan={5} role="status" className="text-ink-2">No strategy matches “{q.trim()}”.</TableCell></TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      )}
      <div id="library-errors" className="mt-3 grid gap-2">
        {libraryErrors.map((e) => (
          <div key={e.file} className="rounded-md border border-hairline bg-board px-3 py-2 text-sm text-ink-2">
            <strong className="font-medium text-ink">{e.file}</strong> did not load, so it is not scored. {e.error}
          </div>
        ))}
      </div>
    </section>
  );
}

export function Markets({ read, onRetry }: { read: Found<Market[]>; onRetry: () => void }) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const all = read && read.ok ? read.v : null;
  const needle = q.trim().toLowerCase();
  const shown = all ? all.filter((m) => m.ticker.toLowerCase().includes(needle)).slice(0, 40) : [];
  const total = all ? all.length : null;
  const dash = (v: number | null | undefined) => (v === null || v === undefined ? "-" : String(v));
  return (
    <section id="markets-card" aria-labelledby="markets-title" className="scroll-mt-28 rounded-[10px] border border-hairline bg-surface p-5">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id="markets-title" tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">Collected markets</h2>
        <Pill variant="outline" className="text-ink-2">tape</Pill>
      </div>
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      <Button id="markets-toggle" variant="outline" aria-expanded={open} aria-controls="markets-panel" className="mt-4 w-full justify-between" onClick={() => setOpen(!open)}>
        <span id="markets-count">{total === null ? "Collected tape, loading" : "Collected tape, " + total + " markets"}</span>
        <span aria-hidden>{open ? "Hide" : "Show"}</span>
      </Button>
      <div id="markets-panel" className="mt-4" hidden={!open}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <SearchField label="Search markets" value={q} onChange={setQ} />
          <p id="markets-note" className="text-sm text-ink-2">{total !== null && total > 40 ? "Showing the first 40 of " + total + " collected markets." : ""}</p>
        </div>
        <div className="mt-4 overflow-x-auto">
          <Table className="min-w-[600px]">
            <TableHeader>
              <TableRow>
                <TableHead>Market</TableHead>
                <TableHead>Closes</TableHead>
                <TableHead>Yes bid/ask</TableHead>
                <TableHead>No bid/ask</TableHead>
                <TableHead className="hidden md:table-cell">Last</TableHead>
                <TableHead>Result</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody id="markets">
              {shown.map((m) => (
                <TableRow key={m.ticker}>
                  <TableCell className="max-w-56 font-mono"><span className="block truncate" title={m.ticker}>{m.ticker}</span></TableCell>
                  <TableCell className="font-mono whitespace-nowrap">{m.close_time.replace("T", " ").replace("Z", " UTC")}</TableCell>
                  <TableCell className="font-mono">{dash(m.yes_bid)} / {dash(m.yes_ask)}</TableCell>
                  <TableCell className="font-mono">{dash(m.no_bid)} / {dash(m.no_ask)}</TableCell>
                  <TableCell className="hidden font-mono md:table-cell">{dash(m.last_price)}</TableCell>
                  <TableCell className="font-mono">{m.result || "-"}</TableCell>
                </TableRow>
              ))}
              {all && !all.length && (
                <TableRow><TableCell colSpan={6} className="text-ink-2">No snapshots collected yet. The collector writes one pass every collect interval, or press Collect now to write one pass.</TableCell></TableRow>
              )}
              {!!all?.length && !shown.length && (
                <TableRow><TableCell colSpan={6} role="status" className="text-ink-2">No market matches “{q.trim()}”.</TableCell></TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      </div>
    </section>
  );
}
