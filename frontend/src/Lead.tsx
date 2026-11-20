// The daily report on the ledger: its verdict line leads the page; its own lines close it as the audit trail, with the verbatim text behind a key.
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";
import { type Read, type Report } from "@/lib/desk";
import { PanelError } from "@/Panels";
import { AuditTrail, splitVerdict } from "@/Trail";

const linesOf = (r: Read<Report> | null) => (r && r.ok ? r.v.report.split("\n") : null);

export function ReportLine({ read, onRetry }: { read: Read<Report> | null; onRetry: () => void }) {
  const lines = linesOf(read);
  const verdict = lines ? splitVerdict(lines[0] ?? "") : null;
  return (
    <section id="report-card" aria-labelledby="report-title" className="min-w-0 scroll-mt-28 bg-surface p-4 md:p-5 lg:p-6">
      <div>
        <h2 id="report-title" tabIndex={-1} className="sr-only">Daily report</h2>
      </div>
      {read && !read.ok && <div className="mt-3"><PanelError msg={read.msg} onRetry={onRetry} /></div>}
      {!verdict && !(read && !read.ok) && <span className="mt-3 block h-8 w-2/3 rounded-md bg-track" aria-hidden />}
      {verdict && (
        <div className="grid gap-x-8 gap-y-2 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] lg:items-baseline">
          <p id="report-head" className="break-words text-[clamp(1.375rem,2.4vw,1.75rem)] leading-[1.15] font-medium tracking-[-0.02em] text-ink text-balance">{verdict.head}</p>
          <div className="grid justify-items-start gap-2">
            {verdict.rest && <p id="report-rest" className="max-w-[64ch] text-sm leading-relaxed text-ink-2">{verdict.rest}</p>}
            <p className="text-sm text-ink-2">From the daily report, which reads the venue and the tape.</p>
          </div>
        </div>
      )}
    </section>
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
