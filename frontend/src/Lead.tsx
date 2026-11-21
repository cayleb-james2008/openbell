// The daily report on the ledger: its verdict line leads the page; its own lines close it as the audit trail, with the verbatim text behind a key.
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";
import { type Dataset, type Read, type Report, type Strategy } from "@/lib/desk";
import { PanelError } from "@/Panels";
import { AuditTrail, splitVerdict } from "@/Trail";

const linesOf = (r: Read<Report> | null) => (r && r.ok ? r.v.report.split("\n") : null);

export function ReportLine({ read, onRetry, strategies, dataset }: { read: Read<Report> | null; onRetry: () => void; strategies: Read<Strategy[]> | null; dataset: Dataset }) {
  const lines = linesOf(read);
  const verdict = lines ? splitVerdict(lines[0] ?? "") : null;
  const standing = <Standing read={strategies} dataset={dataset} />;
  return (
    <section id="report-card" aria-labelledby="report-title" className="min-w-0 scroll-mt-28 bg-surface p-4 md:p-5 lg:p-6">
      <div>
        <h2 id="report-title" tabIndex={-1} className="sr-only">Daily report</h2>
      </div>
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      {!verdict && !(read && !read.ok) && <span className="mt-3 block h-8 w-2/3 rounded-md bg-track" aria-hidden />}
      {verdict ? (
        <div className="grid gap-x-8 gap-y-2 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] lg:items-baseline">
          {/* Beside the verdict at desk widths; on a phone the verdict and its explanation stay together and the standing line closes the card. */}
          <div className="grid min-w-0 grid-cols-[minmax(0,1fr)] gap-y-2 max-lg:contents">
            <p id="report-head" className="max-lg:order-1 break-words text-[clamp(1.375rem,2.4vw,1.75rem)] leading-[1.15] font-medium tracking-[-0.02em] text-ink text-balance">{verdict.head}</p>
            {standing}
          </div>
          <div className="grid justify-items-start gap-2 max-lg:order-2">
            {verdict.rest && <p id="report-rest" className="max-w-[64ch] text-sm leading-relaxed text-ink-2">{verdict.rest}</p>}
            <p className="text-sm text-ink-2">From the daily report, which reads the venue and the tape.</p>
          </div>
        </div>
      ) : <div className="mt-3">{standing}</div>}
    </section>
  );
}

// The desk's standing answer, under the day's verdict: how many strategies the library has proven, before anything is scrolled.
// It lives in the report card's free left column (closing the card on a phone) so no job gains a step: e2e/gate.mjs holds the
// pot, Collect now and all five slots on the first screen.
// Every count comes from the server's own verdict sentences and its own standard, never recomputed here, so this line cannot
// contradict the table below it. A pass on the example tape is shown as that, and never counted as forward evidence.
const PROVEN = /^PROVEN/;
const NOT_PROVEN = /^NOT PROVEN/;

export type StandingRow = { n: number; text: string };
export type StandingLine = { figure: string; label: string; rows: StandingRow[]; news: boolean };

export function standingLine(read: Read<Strategy[]> | null, dataset: Dataset): StandingLine | null {
  if (!read) return null; // first read in flight: the band holds a skeleton, not a claim
  if (!read.ok) return { figure: "", label: "Strategy state unavailable", rows: [], news: false };
  const all = read.v;
  if (!all.length) return { figure: "", label: "No strategies in the library, so the desk scores nothing", rows: [], news: false };
  const verdictOf = (s: Strategy | null) => String(s?.verdict ?? "");
  const example = dataset === "fixture";
  const proven = all.filter((s) => PROVEN.test(verdictOf(s)));
  const failed = all.filter((s) => NOT_PROVEN.test(verdictOf(s)));
  const short = all.length - proven.length - failed.length;
  const needs = all.map((s) => s?.standard?.min_windows);
  const sameNeed = typeof needs[0] === "number" && needs.every((n) => n === needs[0]);
  const rows: StandingRow[] = [];
  if (proven.length) {
    // Names only for a real forward pass: that is the news. The example tape's passes are named in the table.
    const names = example ? [] : proven.map((s) => String(s?.strategy ?? "")).filter(Boolean);
    const shown = names.slice(0, 2).join(" and ") + (names.length > 2 ? " and " + (names.length - 2) + " more" : "");
    rows.push({ n: proven.length, text: example ? "passed on the example tape only" : "proven" + (shown ? " — " + shown : "") });
  }
  if (failed.length) rows.push({ n: failed.length, text: "not proven" });
  if (short) rows.push({ n: short, text: sameNeed ? "below " + needs[0] + " traded windows" : "waiting for evidence" });
  // The figure is the tally the desk exists to answer. A fixture pass is never counted as forward-proven.
  return { figure: (example ? 0 : proven.length) + " of " + all.length, label: "Strategies proven on forward evidence", rows, news: !example && proven.length > 0 };
}

export function Standing({ read, dataset }: { read: Read<Strategy[]> | null; dataset: Dataset }) {
  const line = standingLine(read, dataset);
  if (!line) return <span id="standing" className="block h-9 w-2/3 rounded-sm bg-track max-lg:order-3" aria-hidden />;
  return (
    // A hairline sets the line apart from the day's P&L above it, and it leads with its subject, so it reads as a measure of the library, not as part of the P&L.
    <div id="standing" role="group" aria-label="Proven strategies" className="-mt-0.5 grid min-w-0 grid-cols-[minmax(0,1fr)] gap-y-1 border-t border-hairline pt-1 max-lg:order-3 lg:mt-1 lg:pt-2">
      <p className="flex min-w-0 flex-wrap items-baseline gap-x-1.5 gap-y-1 md:gap-x-2.5">
        <span id="standing-label" className={(line.figure ? "text-sm text-ink-2" : "text-[15px] font-medium text-ink") + " min-w-0"}>{line.label}</span>
        {line.figure && <span id="standing-figure" className="font-mono text-lg leading-none tabular-nums text-ink md:text-xl">{line.figure}</span>}
      </p>
      {line.rows.length > 0 && (
        // On a phone the breakdown shows only when a forward pass is the news: otherwise the figure alone answers, and the pot stays on the first screen.
        <ul id="standing-detail" className={(line.news ? "flex" : "hidden md:flex") + " flex-wrap gap-x-5 gap-y-0.5 text-sm text-ink-2"}>
          {line.rows.map((r) => (
            <li key={r.text} className="flex min-w-0 items-baseline gap-x-2">
              <span className="font-mono tabular-nums text-ink">{r.n}</span>
              <span className="min-w-0 break-words">{r.text}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function ReportTrail({ read }: { read: Read<Report> | null }) {
  const lines = linesOf(read);
  if (!lines) return null;
  const trailLines = lines.slice(1);
  return (
    <section id="trail-card" aria-labelledby="trail-title" className="min-w-0 scroll-mt-28 bg-surface p-4 md:p-5 lg:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id="trail-title" tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">The report, line by line</h2>
        <span className="text-sm text-ink-2">only what the report wrote</span>
      </div>
      <div className="mt-4"><AuditTrail lines={trailLines} /></div>
          <Expandable expandDirection="vertical" expandBehavior="push" id="report-body" className="mt-4 w-full">
            {({ isExpanded }: { isExpanded: boolean }) => (
              <>
                <ExpandableTrigger aria-expanded={isExpanded} aria-controls="report"
                  className="flex min-h-11 w-full cursor-pointer items-center justify-between gap-3 rounded-md border border-hairline px-4 text-left outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) hover:bg-board focus-visible:ring-2 focus-visible:ring-ring">
                  <span className="text-sm text-ink-2">The full report, verbatim</span>
                  <span className="text-sm font-medium text-ink">{isExpanded ? "Collapse report" : "Show full report"}</span>
                </ExpandableTrigger>
                <ExpandableContent preset="fade">
                  <pre id="report" className="mt-3 max-h-[60vh] overflow-auto whitespace-pre-wrap break-words rounded-md border border-hairline bg-board p-4 font-mono text-sm leading-relaxed text-ink-2">{trailLines.join("\n")}</pre>
                </ExpandableContent>
              </>
            )}
          </Expandable>
    </section>
  );
}
