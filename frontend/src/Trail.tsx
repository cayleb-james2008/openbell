// The daily report's own lines as a compact audit trail. Top-level lines are rows; each indented group is one disclosure row
// whose one-line summary is counted from the report's own text. Nothing is invented; the full verbatim text stays in the disclosure.
import { Pill } from "@/components/kibo-ui/pill";
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";

export type TrailRow = { label: string; tag: "venue" | "tape" | "paper" | null; value: string; note: string; time: string | null; depth: number };
type Item = { kind: "row"; row: TrailRow } | { kind: "group"; title: string; summary: string; rows: TrailRow[] };

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
      const paren = label.indexOf(" (");
      if (paren > 0 && !label.includes(DASH) && tag === null) {
        note = label.slice(paren).trim() + (note ? " " + note : "");
        label = label.slice(0, paren);
      }
      label = label.replace(TAG, "").replace(/\s{2,}/g, " ").trim();
      rows.push({ label, tag, value, note, time, depth });
    }
  }
  return rows;
}

const count = (xs: string[], re: RegExp) => xs.filter((x) => re.test(x)).length;

// Every number in a summary is counted from the lines the report wrote.
function summarise(name: string, kids: string[]): string {
  if (name === "Slots") {
    // Only the "Slot N" lines are slots; a Retired or Note line indented under the group is not counted.
    kids = kids.filter((k) => /^\s*Slot \d+:/.test(k));
    const active = count(kids, /\(active\)/), retired = count(kids, /\(retired\)/);
    const strikes = kids.map((k) => k.match(/strikes (\d)\/3/)?.[1]).filter((x): x is string => !!x).map(Number);
    const parts = [kids.length + " slots", active + " active", retired + " retired"];
    if (strikes.length) parts.push("strikes " + Math.min(...strikes) + "/3 to " + Math.max(...strikes) + "/3");
    return parts.join(", ");
  }
  if (name === "Strategy verdicts") {
    const proven = count(kids, / — PROVEN/), notProven = count(kids, / — NOT PROVEN/), insufficient = count(kids, / — INSUFFICIENT/);
    const other = kids.length - proven - notProven - insufficient;
    const parts = [kids.length + " strategies", proven + " proven", notProven + " not proven", insufficient + " insufficient evidence"];
    if (other > 0) parts.push(other + " other");
    return parts.join(", ");
  }
  return kids.length + " lines";
}

export function buildTrail(lines: string[]): Item[] {
  const items: Item[] = [];
  let i = 0;
  while (i < lines.length) {
    const raw = lines[i];
    if (!raw.trim()) { i++; continue; }
    if (!raw.startsWith(" ") && raw.trimEnd().endsWith(":")) {
      const kids: string[] = [];
      i++;
      while (i < lines.length && lines[i].startsWith("  ")) { if (lines[i].trim()) kids.push(lines[i]); i++; }
      const name = raw.trim().replace(/:$/, "").split(" (")[0];
      const slotKids = name === "Slots" ? kids.filter((k) => /^\s*Slot \d+:/.test(k)) : kids;
      items.push({ kind: "group", title: name + " (" + slotKids.length + ")", summary: summarise(name, kids), rows: parseTrail(kids) });
      continue;
    }
    for (const row of parseTrail([raw])) items.push({ kind: "row", row });
    i++;
  }
  return items;
}

const TAG_CLASS: Record<NonNullable<TrailRow["tag"]>, string> = {
  venue: "text-info-foreground",
  tape: "text-ink-2",
  paper: "text-warning-foreground",
};

function RowItem({ r }: { r: TrailRow }) {
  return (
    <li className={"flex min-w-0 items-center gap-2 py-1.5 pr-1 text-sm md:grid md:min-h-9 md:grid-cols-[minmax(0,13rem)_minmax(0,1fr)_auto] md:gap-x-4 " + (r.depth ? "pl-8" : "pl-4")}>
      <div className="flex min-w-0 max-w-[45%] shrink-0 items-center gap-2 md:max-w-none">
        <span className="min-w-0 truncate font-medium text-ink" title={r.label}>{r.label}</span>
        {r.tag && <Pill variant="outline" className={"h-auto shrink-0 py-0 " + TAG_CLASS[r.tag]}>{r.tag}</Pill>}
      </div>
      <div className="min-w-0 flex-1 truncate md:flex-none" title={[r.value, r.note].filter(Boolean).join(" — ")}>
        {r.value ? <span className="font-mono tabular-nums text-ink">{r.value}</span> : <span className="text-ink-2">-</span>}
        {r.note && <span className="ml-2 text-ink-2">{r.note}</span>}
      </div>
      {r.time && <time dateTime={r.time} className="hidden font-mono text-sm tabular-nums text-info-foreground md:block md:text-right">{r.time}</time>}
    </li>
  );
}

export function AuditTrail({ lines }: { lines: string[] }) {
  const items = buildTrail(lines);
  return (
    <ul aria-label="The daily report, line by line" className="ml-1 border-l border-hairline">
      {items.map((it, i) => it.kind === "row" ? (
        <RowItem key={i} r={it.row} />
      ) : (
        <li key={i} className="pl-4">
          <Expandable expandDirection="vertical" expandBehavior="push" className="w-full">
            {({ isExpanded }: { isExpanded: boolean }) => (
              <>
                <ExpandableTrigger aria-expanded={isExpanded}
                  className="flex min-h-11 w-full cursor-pointer items-center justify-between gap-3 rounded-md py-1 pr-1 text-left outline-none transition-[background-color] duration-(--dur-press) ease-(--ease-house) hover:bg-board focus-visible:ring-2 focus-visible:ring-blue">
                  <span className="min-w-0 font-medium text-ink">{it.title}</span>
                  <span className="min-w-0 truncate text-sm text-ink-2">{it.summary}</span>
                </ExpandableTrigger>
                <ExpandableContent preset="fade">
                  <ul className="ml-1 border-l border-hairline">
                    {it.rows.map((r, j) => <RowItem key={j} r={r} />)}
                  </ul>
                </ExpandableContent>
              </>
            )}
          </Expandable>
        </li>
      ))}
    </ul>
  );
}
