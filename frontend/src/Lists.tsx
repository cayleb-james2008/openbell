// Strategies against the promotion standard (a keyboard ledger with a detail drawer) and the collected markets.
import { type ReactNode, type RefObject, useEffect, useRef, useState } from "react";
import { Drawer } from "vaul";
import { Virtuoso } from "react-virtuoso";
import { Search } from "lucide-react";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { Tracker } from "@/components/Tracker/Tracker";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { Kbd } from "@/components/ui/kbd";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { type Dataset, type Equity, isNum, type Market, type Read, type Slots, type Strategy, money } from "@/lib/desk";
import { SeriesChart, Spark } from "@/Spark";
import { PanelError } from "@/Panels";
import { useWide } from "@/lib/media";

type Found<T> = Read<T> | null;

function SearchField({ id, label, value, onChange, inputRef }: { id: string; label: string; value: string; onChange: (v: string) => void; inputRef?: RefObject<HTMLInputElement | null> }) {
  return (
    <InputGroup className="!h-11 w-full max-w-xs">
      <InputGroupAddon><Search className="size-4 text-icon" strokeWidth={1.5} aria-hidden /></InputGroupAddon>
      <InputGroupInput
        id={id}
        ref={inputRef}
        type="search"
        aria-label={label}
        placeholder={label}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            if (value) onChange("");
            else e.currentTarget.blur();
          }
        }}
        className="!h-11 min-w-0 flex-1"
      />
      {!value && (
        <InputGroupAddon align="inline-end" aria-hidden className="ml-auto">
          <Kbd>/</Kbd>
        </InputGroupAddon>
      )}
    </InputGroup>
  );
}

const safeVerdict = (v: unknown): string => (typeof v === "string" && v.trim() ? v : "unavailable");
const safeName = (v: unknown, i: number): string => (typeof v === "string" && v.trim() ? v : v !== null && v !== undefined && String(v).trim() ? String(v) : "unnamed-" + (i + 1));

// A verdict is a sentence the server wrote: it wraps to as many lines as it needs, at every width, and is never clipped.
const WRAP = "!h-auto !rounded-chip max-w-full whitespace-normal py-1 text-left text-xs leading-snug md:text-sm";
export function VerdictPill({ verdict, fixture }: { verdict: unknown; fixture: boolean }) {
  const v = safeVerdict(verdict);
  if (fixture && v.startsWith("PROVEN")) {
    return <Pill variant="outline" className={WRAP + " text-warning-foreground"}>meets the bar on the example tape (not forward evidence)</Pill>;
  }
  if (v.startsWith("PROVEN")) {
    return <Pill variant="outline" className={WRAP + " text-success-foreground"}><PillStatus className="border-r-0 pr-0"><PillIndicator variant="success" />{v}</PillStatus></Pill>;
  }
  if (v.startsWith("NOT PROVEN")) {
    return <Pill variant="outline" className={WRAP + " text-unread"}><PillStatus className="border-r-0 pr-0"><PillIndicator variant="error" />{v}</PillStatus></Pill>;
  }
  return <Pill variant="outline" className={WRAP + " text-ink-2"}>{v}</Pill>;
}

const FILTERS = ["All", "Proven", "Not proven", "Waiting for evidence"] as const;
type Filter = (typeof FILTERS)[number];
const cents = (c: unknown) => (isNum(c) ? (c >= 0 ? "+" : "") + c.toFixed(1) + "¢" : "unavailable");
const clock = (x: unknown) => (isNum(x) ? new Date(x * 1000).toISOString().slice(0, 16).replace("T", " ") + " UTC" : "unavailable");

// Strategies as a ledger: filter by verdict, `/` focuses search, j / k move the highlighted row, Enter or the name opens the drawer
// with the strategy's own paper line, its figures against the bar, and its last paper trades.
export function Strategies({ read, slots, equity, libraryErrors, dataset, onRetry }: {
  read: Found<Strategy[]>; slots: Slots | null; equity: Equity | null; libraryErrors: { file: string; error: string }[]; dataset: Dataset; onRetry: () => void;
}) {
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<Filter>("All");
  const [sel, setSel] = useState(-1);
  const [open, setOpen] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const wide = useWide();
  const rawAll = read && read.ok && Array.isArray(read.v) ? read.v : null;
  const all = rawAll ? rawAll.filter((s): s is Strategy => !!s && typeof s === "object") : null;
  const needle = q.trim().toLowerCase();
  const shown = (all ?? []).filter((s) => {
    const nm = String(s.strategy ?? "").toLowerCase();
    if (!nm.includes(needle)) return false;
    const v = safeVerdict(s.verdict);
    if (filter === "Proven") return v.startsWith("PROVEN");
    if (filter === "Not proven") return v.startsWith("NOT PROVEN");
    if (filter === "Waiting for evidence") return !v.startsWith("PROVEN") && !v.startsWith("NOT PROVEN");
    return true;
  });
  // No row is highlighted until the person moves through the table.
  const cur = sel < 0 ? -1 : Math.min(sel, Math.max(0, shown.length - 1));
  const seriesOf = (name: string) => (Array.isArray(equity?.series) ? equity.series.find((x) => x && x.strategy === name)?.points ?? [] : []);
  const rowRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const validErrors = (Array.isArray(libraryErrors) ? libraryErrors : []).filter((e) => !!e && typeof e === "object");

  // `/` focuses search; j / k / Enter act on the table only when focus is not in a field, a button elsewhere or a dialog.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (!t || e.metaKey || e.ctrlKey || e.altKey || t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName)) return;
      const inDrawer = !!t.closest("[data-strategy-drawer]");
      if (t.closest("[role=dialog]") && !inDrawer) return;
      if (e.key === "/" && !inDrawer) {
        e.preventDefault();
        const mkts = document.getElementById("markets-card");
        const mktInput = document.getElementById("markets-search") as HTMLInputElement | null;
        if (mkts && mktInput && mkts.getBoundingClientRect().top <= 220) {
          mktInput.focus({ preventScroll: false });
          mktInput.select();
        } else {
          searchRef.current?.focus({ preventScroll: false });
          searchRef.current?.select();
        }
        return;
      }
      if (e.key === "Enter" && t.closest("button, a, summary") && !t.closest("[data-strategy-row]")) return;
      const move = (n: number) => {
        if (!shown.length) return;
        const next = cur < 0 ? 0 : Math.max(0, Math.min(shown.length - 1, cur + n));
        setSel(next);
        if (!inDrawer) rowRefs.current[next]?.focus({ preventScroll: false });
        if (open && shown[next]) setOpen(safeName(shown[next].strategy, next));
      };
      if (e.key === "j") { e.preventDefault(); move(1); }
      else if (e.key === "k") { e.preventDefault(); move(-1); }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  });

  return (
    <section id="strategies" aria-labelledby="strategies-title" className="min-w-0 scroll-mt-28 bg-surface p-4 md:p-5 lg:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id="strategies-title" tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">Strategies against the promotion standard</h2>
        <span className="hidden text-sm text-ink-2 md:inline">/ searches, j and k move, Enter opens</span>
        <Pill variant="outline" className="text-warning-foreground">paper</Pill>
      </div>
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      <p className="mt-3 max-w-[70ch] text-sm text-ink-2">A strategy is proven only after 100 forward windows, both chronological halves positive, and +3.5 cents per contract after fees. Until then it is unproven, and the verdict says so.</p>
      <div className="mt-4 flex flex-wrap items-center gap-2">
        <div role="group" aria-label="Show strategies by verdict" className="flex flex-wrap gap-2">
          {FILTERS.map((f) => (
            <button key={f} type="button" aria-pressed={filter === f} onClick={() => { setFilter(f); setSel(-1); }}
              className={"inline-flex min-h-11 min-w-11 cursor-pointer items-center justify-center rounded-full border px-3 text-sm outline-none transition-[background-color,color,border-color] duration-(--dur-press) ease-(--ease-house) active:bg-track focus-visible:ring-2 focus-visible:ring-ring " + (filter === f ? "border-ink bg-ink text-canvas" : "border-hairline text-ink-2 hover:text-ink")}>
              {f}
            </button>
          ))}
        </div>
        <div className="ml-auto w-full sm:w-64"><SearchField id="strategies-search" inputRef={searchRef} label="Search strategies" value={q} onChange={(v) => { setQ(v); setSel(-1); }} /></div>
      </div>
      {!all && !(read && !read.ok) && <span className="mt-4 block h-24 rounded-md bg-track" aria-hidden />}
      {all && (
        <div className="mt-4 min-w-0">
          <Table className="stack md:min-w-[640px]" aria-label="Strategies. j and k move the highlighted row, Enter opens it.">
            <TableHeader>
              <TableRow>
                <TableHead>Strategy</TableHead>
                <TableHead>Side</TableHead>
                <TableHead>Windows traded, of 100</TableHead>
                <TableHead className="text-right">Mean per contract</TableHead>
                <TableHead className="hidden lg:table-cell">Paper P&amp;L over the tape</TableHead>
                <TableHead>Verdict</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {shown.map((s, i) => {
                const name = safeName(s.strategy, i);
                const side = typeof s.side === "string" && s.side.trim() ? s.side : "—";
                const need = isNum(s.standard?.min_windows) && s.standard.min_windows > 0 ? s.standard.min_windows : 100;
                const traded = isNum(s.windows_traded) ? Math.max(0, s.windows_traded) : 0;
                const done = Math.min(traded, need);
                const label = traded >= need ? traded + " (" + need + " needed)" : traded + " of " + need;
                return (
                  <TableRow key={name + "-" + i} aria-selected={i === cur} className={i === cur ? "bg-blue-soft" : undefined}>
                    <TableCell data-label="Strategy" className="stack-first c-name max-w-60">
                      <button type="button" data-strategy-row ref={(el) => { rowRefs.current[i] = el; }} onFocus={() => setSel(i)} onClick={() => { setSel(i); setOpen(name); }}
                        aria-label={"Open " + name}
                        className="-mx-2 inline-flex min-h-11 max-w-full cursor-pointer items-center rounded-md px-2 text-left font-mono text-ink outline-none transition-[background-color,color] duration-(--dur-press) ease-(--ease-house) hover:underline active:bg-track focus-visible:ring-2 focus-visible:ring-ring">
                        <span className="[overflow-wrap:anywhere]">{name}</span>
                      </button>
                    </TableCell>
                    <TableCell data-label="Side" className="c-side font-mono">{side}</TableCell>
                    <TableCell data-label="Windows traded" className="c-windows min-w-0">
                      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                        <Tracker data={Array.from({ length: 10 }, (_, k) => ({ key: k, color: (k + 1) * need / 10 <= done ? "bg-info-foreground" : "bg-track", tooltip: "Windows " + Math.round(k * need / 10 + 1) + " to " + Math.round((k + 1) * need / 10) }))} className="h-2 w-24 md:w-28" />
                        <span className="font-mono text-sm text-ink-2">{label}</span>
                      </div>
                    </TableCell>
                    <TableCell data-label="Mean cents" className="c-mean text-right font-mono tabular-nums"><span className="text-ink-2 md:hidden">Mean per contract </span>{cents(s.mean_cents_per_contract)}</TableCell>
                    <TableCell className="hidden lg:table-cell max-md:!hidden"><Spark points={seriesOf(name)} start={0} w={120} h={24} label={name + ", cumulative paper P&L"} /></TableCell>
                    <TableCell data-label="Verdict" className="c-verdict min-w-0 max-w-md"><VerdictPill verdict={s.verdict} fixture={dataset === "fixture"} /></TableCell>
                  </TableRow>
                );
              })}
              {rawAll && !rawAll.length && (
                <TableRow><TableCell colSpan={6} className="text-ink-2">No strategies in the library. Each strategy is a small .rn file in strategies/, and the desk scores whatever is there.</TableCell></TableRow>
              )}
              {rawAll && !!rawAll.length && !all.length && (
                <TableRow><TableCell colSpan={6} role="status" className="text-ink-2">Strategy rows answered with an unexpected shape, so none could be shown.</TableCell></TableRow>
              )}
              {!!all.length && !shown.length && (
                <TableRow><TableCell colSpan={6} role="status" className="text-ink-2">No strategy matches{needle ? " “" + q.trim() + "”" : ""}{filter !== "All" ? " in " + filter.toLowerCase() : ""}. <button type="button" className="min-h-11 cursor-pointer rounded-md px-1 text-ink underline outline-none active:bg-track focus-visible:ring-2 focus-visible:ring-ring" onClick={() => { setQ(""); setFilter("All"); }}>Show all strategies</button></TableCell></TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      )}
      <div id="library-errors" className="mt-3 grid gap-2">
        {validErrors.map((e, idx) => (
          <div key={String(e.file ?? idx)} className="rounded-md border border-hairline bg-board px-3 py-2 text-sm text-ink-2">
            <strong className="font-medium text-ink">{String(e.file ?? "unknown file")}</strong> did not load, so it is not scored. {String(e.error ?? "")}
          </div>
        ))}
      </div>
      <StrategyDrawer name={open} onClose={() => setOpen(null)} wide={wide} all={all ?? []} slots={slots} points={open ? seriesOf(open) : []} dataset={dataset} />
    </section>
  );
}

function StrategyDrawer({ name, onClose, wide, all, slots, points, dataset }: {
  name: string | null; onClose: () => void; wide: boolean; all: Strategy[]; slots: Slots | null; points: [number, number][]; dataset: Dataset;
}) {
  const idx = name ? all.findIndex((x, i) => safeName(x.strategy, i) === name) : -1;
  const r = idx >= 0 ? all[idx] : undefined;
  const rName = r ? safeName(r.strategy, idx) : "";
  const rSide = r && typeof r.side === "string" && r.side.trim() ? r.side : "—";
  const minW = isNum(r?.standard?.min_windows) && r.standard.min_windows > 0 ? r.standard.min_windows : 100;
  const minMean = isNum(r?.standard?.min_mean_cents) ? r.standard.min_mean_cents : 3.5;
  const traded = isNum(r?.windows_traded) ? Math.max(0, r.windows_traded) : 0;
  const trades = (Array.isArray(slots?.trades) ? slots.trades : [])
    .filter((t) => !!t && typeof t === "object" && String(t.strategy ?? "") === name)
    .slice(-8)
    .reverse();
  const row = (k: string, v: ReactNode) => (<><dt className="text-ink-2">{k}</dt><dd className="text-right font-mono tabular-nums text-ink">{v}</dd></>);
  return (
    <Drawer.Root open={!!name} onOpenChange={(o) => { if (!o) onClose(); }} direction={wide ? "right" : "bottom"}>
      <Drawer.Portal>
        <Drawer.Overlay className="fixed inset-0 z-40 bg-ink/20" />
        <Drawer.Content data-strategy-drawer aria-describedby={r ? "drawer-desc" : undefined}
          className={"fixed z-50 flex flex-col bg-surface text-ink outline-none " + (wide ? "inset-y-0 right-0 w-full max-w-[480px] border-l border-hairline" : "inset-x-0 bottom-0 max-h-[88dvh] rounded-t-[14px] border-t border-hairline")}>
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain p-5">
            {!r ? <Drawer.Title className="text-base text-ink-2">Strategy</Drawer.Title> : (
              <>
                <Drawer.Title className="break-words font-mono text-lg text-ink">{rName}</Drawer.Title>
                <Drawer.Description id="drawer-desc" className="mt-1 text-sm text-ink-2">Buys the {rSide} side. Paper only, not venue money.</Drawer.Description>
                <div className="mt-4"><VerdictPill verdict={r.verdict} fixture={dataset === "fixture"} /></div>
                <div className="mt-5">
                  <SeriesChart points={points} start={0} height={168} label={rName + ", cumulative paper P&L over the tape, dollar axis"}
                    empty={<p className="rounded-md bg-board px-3 py-2 text-sm text-ink-2">No paper trades for this strategy on this tape, so there is no line to draw.</p>} />
                </div>
                <dl className="mt-5 grid grid-cols-[minmax(0,1fr)_auto] gap-x-6 gap-y-2.5 text-sm">
                  {row("Windows traded", traded + " of " + minW)}
                  {isNum(r.windows_settled) && row("Windows settled", r.windows_settled)}
                  {isNum(r.windows_conflicted) && row("Conflicted, not scored", r.windows_conflicted)}
                  {isNum(r.call_failures) && row("Call failures", r.call_failures)}
                  {row("Mean per contract", cents(r.mean_cents_per_contract))}
                  {r.first_half_cents !== undefined && row("First half", cents(r.first_half_cents))}
                  {r.second_half_cents !== undefined && row("Second half", cents(r.second_half_cents))}
                  {row("Bar to pass", minW + " windows, " + minMean + "¢")}
                </dl>
                <h3 className="mt-6 text-sm font-medium text-ink-2">Last paper trades</h3>
                {trades.length === 0 ? <p className="mt-2 text-sm text-ink-2">No paper trades for this strategy on this tape.</p> : (
                  <ul className="mt-2 divide-y divide-hairline">
                    {trades.map((t, i) => (
                      <li key={i} className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-baseline gap-3 py-2 text-sm">
                        <span className="font-mono text-ink-2">{clock(t.close_epoch)}</span>
                        <span className="font-mono tabular-nums text-ink-2">{isNum(t.contracts) ? t.contracts : "—"} at {isNum(t.price) ? Math.round(t.price * 100) + "¢" : "—"}</span>
                        <span className={"text-right font-mono tabular-nums " + (isNum(t.pnl_dollars) && t.pnl_dollars < 0 ? "text-unread" : "text-ink")}>{money(t.pnl_dollars)}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </>
            )}
          </div>
          <div className="flex items-center justify-between gap-3 border-t border-hairline p-4 text-sm text-ink-2">
            <span className="hidden md:inline">j and k follow the table</span>
            <Drawer.Close className="ml-auto inline-flex min-h-11 cursor-pointer items-center rounded-md border border-hairline px-4 text-ink outline-none transition-[background-color,color] duration-(--dur-press) ease-(--ease-house) hover:bg-board active:bg-track focus-visible:ring-2 focus-visible:ring-ring">Close</Drawer.Close>
          </div>
        </Drawer.Content>
      </Drawer.Portal>
    </Drawer.Root>
  );
}

// The collected markets, newest close first: the yes price as a slim bar, bid / ask per side, and the result once settled.
export function Markets({ read, onRetry }: { read: Found<Market[]>; onRetry: () => void }) {
  const [q, setQ] = useState("");
  const rawAll = read && read.ok && Array.isArray(read.v) ? read.v : null;
  const all = rawAll ? rawAll.filter((m): m is Market => !!m && typeof m === "object") : null;
  const needle = q.trim().toLowerCase();
  const shown = all ? all.filter((m) => String(m.ticker ?? "").toLowerCase().includes(needle)).slice().sort((a, b) => String(b.close_time ?? "").localeCompare(String(a.close_time ?? ""))) : [];
  const c = (p: unknown) => (isNum(p) ? Math.round(p * 100) + "¢" : "-");
  const grid = "grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-1 md:grid-cols-[minmax(0,2fr)_150px_minmax(0,1.6fr)_70px_104px] md:items-center";
  return (
    <section id="markets-card" aria-labelledby="markets-title" className="min-w-0 scroll-mt-28 bg-surface p-4 md:p-5 lg:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id="markets-title" tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">Collected markets</h2>
        <span id="markets-count" className="text-sm text-ink-2">{all === null ? "loading" : all.length + " on the tape"}</span>
        <Pill variant="outline" className="text-ink-2">tape</Pill>
      </div>
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      <div className="mt-4"><SearchField id="markets-search" label="Search markets" value={q} onChange={setQ} /></div>
      {!all && !(read && !read.ok) && <span className="mt-4 block h-24 rounded-md bg-track" aria-hidden />}
      {rawAll && !rawAll.length && <p className="mt-4 text-sm text-ink-2">No snapshots collected yet. The collector writes one pass every collect interval, or press Collect now to write one pass.</p>}
      {!!all?.length && !shown.length && <p role="status" className="mt-4 text-sm text-ink-2">No market matches “{q.trim()}”.</p>}
      {shown.length > 0 && (
        <div className="mt-4" role="table" aria-label="Collected markets">
          <div role="row" className={grid + " hidden border-b border-hairline pb-2 text-sm font-medium text-ink-2 md:grid"}>
            <span role="columnheader">Market</span><span role="columnheader">Closes (UTC)</span><span role="columnheader">Yes, bid / ask</span><span role="columnheader" className="text-right">Last</span><span role="columnheader" className="text-right">Result</span>
          </div>
          <Virtuoso aria-label="Collected markets list" style={{ height: Math.min(360, shown.length * 64 + 8) }} data={shown} itemContent={(_, m) => {
            const lastOk = isNum(m.last_price);
            return (
              <div role="row" className={grid + " border-b border-hairline py-2.5 text-sm"}>
                <span role="cell" className="min-w-0 break-all font-mono text-ink">{String(m.ticker ?? "—")}</span>
                <span role="cell" className="font-mono whitespace-nowrap text-ink-2 md:order-none">{String(m.close_time ?? "").replace("T", " ").slice(0, 16) || "—"}</span>
                <span role="cell" className="col-span-2 min-w-0 md:col-span-1">
                  <span className="flex h-2 w-full overflow-hidden rounded-full bg-track" role="img" aria-label={!lastOk ? "No last price" : "Last yes price " + c(m.last_price)}>
                    {lastOk && <span className="h-full bg-dot-blue" style={{ width: Math.max(0, Math.min(100, (m.last_price as number) * 100)) + "%" }} />}
                  </span>
                  <span className="mt-1 flex justify-between font-mono text-sm tabular-nums text-ink-2"><span>yes {c(m.yes_bid)} / {c(m.yes_ask)}</span><span>no {c(m.no_bid)} / {c(m.no_ask)}</span></span>
                </span>
                <span role="cell" className="font-mono tabular-nums text-ink md:text-right"><span className="text-ink-2 md:hidden">last </span>{c(m.last_price)}</span>
                <span role="cell" className="whitespace-nowrap text-right font-mono text-ink">{typeof m.result === "string" && m.result ? "settled " + m.result : <span className="text-ink-2">open</span>}</span>
              </div>
            );
          }} />
        </div>
      )}
    </section>
  );
}
