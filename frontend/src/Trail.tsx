// The daily report's own lines as an audit trail. Every row is a line (or one ' · ' part of a line) of the server's report.
// Nothing is invented: a row exists only where the report wrote one, and the full verbatim text stays in the disclosure.
import { Pill } from "@/components/kibo-ui/pill";

export type TrailRow = { label: string; tag: "venue" | "tape" | "paper" | null; value: string; note: string; time: string | null; depth: number };

const TAG = /\((venue|tape|paper)[^)]*\)/;
const TIME = /(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)/;
const DASH = " — ";

export function splitVerdict(text: string): { head: string; rest: string } {
  const i = text.indexOf(DASH);
  return i >= 0 ? { head: text.slice(0, i), rest: text.slice(i + DASH.length) } : { head: text, rest: "" };
}

export function parseTrail(lines: string[]): TrailRow[] {
  const rows: TrailRow[] = [];
  for (const raw of lines) {
    if (!raw.trim()) continue;
    const depth = raw.startsWith("  ") ? 1 : 0;
    for (const seg of raw.trim().split(" · ")) {
      let s = seg.trim();
      const tm = s.match(TIME);
      const time = tm ? tm[1] : null;
      if (time) s = s.replace(time, "").replace(/\s{2,}/g, " ").trim();
      const tag = (s.match(TAG)?.[1] as TrailRow["tag"]) ?? null;
      let label: string, value = "", note = "";
      const colon = s.indexOf(": ");
      if (colon >= 0) {
        label = s.slice(0, colon);
        const rest = s.slice(colon + 2);
        const d = rest.indexOf(DASH);
        value = d >= 0 ? rest.slice(0, d) : rest;
        note = d >= 0 ? rest.slice(d + DASH.length) : "";
      } else if (s.endsWith(":")) {
        label = s.slice(0, -1);
      } else {
        const d = s.indexOf(DASH);
        label = d >= 0 ? s.slice(0, d) : s;
        note = d >= 0 ? s.slice(d + DASH.length) : "";
      }
      // A header such as "Slots (paper — starting pot ...):" keeps its name; the parenthesis goes to the note.
      const paren = label.indexOf(" (");
      if (paren > 0 && label.includes(DASH) === false && tag === null && paren < label.length) {
        note = label.slice(paren).trim() + (note ? " " + note : "");
        label = label.slice(0, paren);
      }
      label = label.replace(TAG, "").replace(/\s{2,}/g, " ").trim();
      rows.push({ label, tag, value, note, time, depth });
    }
  }
  return rows;
}

const TAG_CLASS: Record<NonNullable<TrailRow["tag"]>, string> = {
  venue: "text-info-foreground",
  tape: "text-ink-2",
  paper: "text-blue",
};

export function AuditTrail({ lines }: { lines: string[] }) {
  const rows = parseTrail(lines);
  return (
    <ol aria-label="The daily report, line by line" className="ml-1 border-l border-hairline">
      {rows.map((r, i) => (
        <li key={i} className={"grid min-w-0 gap-x-6 gap-y-1 py-3 pr-1 md:grid-cols-[minmax(0,15rem)_minmax(0,1fr)_auto] md:items-baseline " + (r.depth ? "pl-8" : "pl-5")}>
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <span className="min-w-0 break-words font-medium text-ink">{r.label}</span>
            {r.tag && <Pill variant="outline" className={"h-auto py-0.5 " + TAG_CLASS[r.tag]}>{r.tag}</Pill>}
          </div>
          <div className="min-w-0 break-words font-mono text-base tabular-nums text-ink">
            {r.value || <span className="text-ink-2">-</span>}
            {r.note && <span className="mt-1 block font-sans text-sm leading-snug text-ink-2">{r.note}</span>}
          </div>
          {r.time && <time dateTime={r.time} className="font-mono text-sm tabular-nums text-info-foreground md:text-right">{r.time}</time>}
        </li>
      ))}
    </ol>
  );
}
