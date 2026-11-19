// The lead bands of the one instrument surface: the verdict and the paper pot, the labelled equity trace, and the report's lines as the audit trail.
import { type ReactNode, useEffect, useRef, useState } from "react";
import { motion } from "motion/react";
import { Pill } from "@/components/kibo-ui/pill";
import { ProgressCircle } from "@/components/ProgressCircle/ProgressCircle";
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";
import { SlidingNumber } from "@/components/ui/sliding-number";
import { drawOnce } from "@/motionProps";
import { type Dataset, type Equity, type Read, type Reads, type Slots, dayLabel, money, moneyWhole } from "@/lib/desk";
import { PanelError } from "@/Panels";
import { AuditTrail, splitVerdict } from "@/Trail";

const ok = <T,>(r: Read<T> | undefined): T | null => (r && r.ok ? r.v : null);
const SERIES_COLORS = ["var(--info-foreground)", "var(--success-foreground)", "var(--ink-2)", "var(--destructive-foreground)"];
const H = 184, PADL = 64, PADR = 8, PADT = 10, PADB = 26;

function useWidth() {
  const ref = useRef<HTMLDivElement>(null);
  const [w, setW] = useState(640);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setW(Math.round(e.contentRect.width)));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, w] as const;
}

function PotFigure({ pot }: { pot: number }) {
  const cents = Math.round(Math.abs(pot) * 100);
  const whole = Math.floor(cents / 100);
  const frac = String(cents % 100).padStart(2, "0");
  return (
    <div id="pot-figure" className="flex flex-nowrap items-baseline whitespace-nowrap font-mono text-[clamp(2.25rem,5.6vw,3.75rem)] leading-none font-normal tabular-nums text-ink">
      <span className="shrink-0">{pot < 0 ? "-$" : "$"}</span>
      <span className="inline-flex shrink-0 whitespace-nowrap"><SlidingNumber value={whole} /></span>
      <span className="shrink-0 text-[0.5em] text-ink-2">.{frac}</span>
    </div>
  );
}

function Trace({ eq, start, traded, settled }: { eq: Equity | null; start: number; traded: number; settled: number | null }) {
  const [ref, width] = useWidth();
  const allSeries: { strategy: string; points: [number, number][]; headline?: boolean }[] = eq
    ? [{ strategy: "paper pot", points: eq.pot, headline: true }, ...eq.series.filter((s) => s.points.length)]
    : [];
  const flat = allSeries.flatMap((s) => s.points);
  const drawable = !!eq && flat.length > 1 && eq.pot.length > 1;
  const W = Math.max(300, width);
  const frameTop = PADT, frameBottom = H - PADB;
  const mid = (frameTop + frameBottom) / 2;

  let grid: ReactNode, startY: number, paths: ReactNode = null, dayLabels: ReactNode = null;
  if (drawable) {
    const xs = flat.map((p) => p[0]), ys = flat.map((p) => p[1]);
    const x0 = Math.min(...xs), x1 = Math.max(...xs);
    const y0 = Math.min(...ys, start), y1 = Math.max(...ys, start);
    const X = (x: number) => PADL + ((x - x0) / Math.max(1, x1 - x0)) * (W - PADL - PADR);
    const Y = (y: number) => H - PADB - ((y - y0) / Math.max(1e-9, y1 - y0)) * (H - PADT - PADB);
    const ticks = [y1, (y0 + y1) / 2, y0];
    grid = ticks.map((t) => (
      <g key={t}>
        <line x1={PADL} x2={W - PADR} y1={Y(t)} y2={Y(t)} style={{ stroke: "var(--hairline)" }} />
        <text x={PADL - 8} y={Y(t) + 4} textAnchor="end" className="font-mono" fontSize={12} style={{ fill: "var(--ink-3)" }}>{moneyWhole(t)}</text>
      </g>
    ));
    startY = Y(start);
    paths = allSeries.map((s, i) => {
      if (s.points.length < 2) return null;
      const d = s.points.map((p, j) => (j ? "L" : "M") + X(p[0]).toFixed(1) + "," + Y(p[1]).toFixed(1)).join(" ");
      const color = s.headline ? "var(--ink)" : SERIES_COLORS[(i - 1) % SERIES_COLORS.length];
      return (
        <motion.path key={s.strategy} d={d} fill="none" strokeWidth={s.headline ? 2.2 : 1.4} strokeLinejoin="round"
          style={{ stroke: color }} initial={{ pathLength: 0 }} animate={{ pathLength: 1 }} {...drawOnce} />
      );
    });
    dayLabels = (
      <>
        <text x={PADL} y={H - 6} className="font-mono" fontSize={12} style={{ fill: "var(--ink-3)" }}>{dayLabel(x0)}</text>
        <text x={W - PADR} y={H - 6} textAnchor="end" className="font-mono" fontSize={12} style={{ fill: "var(--ink-3)" }}>{dayLabel(x1)}</text>
      </>
    );
  } else {
    // The empty frame: axis hairlines, the start line and the word paper, drawn where the curve would sit.
    grid = [frameTop, mid, frameBottom].map((y) => <line key={y} x1={PADL} x2={W - PADR} y1={y} y2={y} style={{ stroke: "var(--hairline)" }} />);
    startY = mid;
  }

  return (
    <div className="min-w-0">
      <div ref={ref} id="chart" role="img" aria-label="Paper equity curves, the pot and each strategy, with dollar axis labels" className="relative min-w-0">
        <svg viewBox={`0 0 ${W} ${H}`} width={W} height={H} className="block h-auto max-w-none" aria-hidden>
          <line x1={PADL} x2={PADL} y1={frameTop} y2={frameBottom} style={{ stroke: "var(--hairline)" }} />
          <line x1={PADL} x2={W - PADR} y1={frameBottom} y2={frameBottom} style={{ stroke: "var(--ink-3)" }} />
          {grid}
          <line x1={PADL} x2={W - PADR} y1={startY} y2={startY} strokeDasharray="3 3" style={{ stroke: "var(--ink-3)" }} />
          <text x={PADL + 8} y={frameTop + 12} fontSize={12} style={{ fill: "var(--ink-3)" }}>paper</text>
          {paths}
          {dayLabels}
        </svg>
        {!drawable && (
          <div className="absolute inset-0 flex items-center justify-center p-4 text-center">
            <p id="chart-empty" className="max-w-[60ch] rounded-md bg-surface px-3 py-2 text-sm text-ink-2">
              {traded === 0
                ? "No paper trades yet, so there is no pot line. The line starts at the first window a slot stakes on. " + (settled === null ? "The tape count is unavailable right now." : "The tape has " + settled + " settled window" + (settled === 1 ? "" : "s") + ", and no slot has traded in them yet.")
                : "Not enough collected tape to draw the curve yet. Two points are needed."}
            </p>
          </div>
        )}
      </div>
      {drawable && eq && (
        <>
          <p id="chart-note" className="mt-3 max-w-[90ch] text-sm text-ink-2">Paper simulation over the collected tape, starting pot ${eq.paper_pot_start.toFixed(2)} split evenly across the slots each window. The heavy line is the pot, the dashed line is the start. Paper only, not venue money.</p>
          <div id="legend" className="mt-3 flex flex-wrap gap-2">
            {allSeries.map((s, i) => {
              const last = s.points.length ? s.points[s.points.length - 1][1] : null;
              const color = s.headline ? "var(--ink)" : SERIES_COLORS[(i - 1) % SERIES_COLORS.length];
              return (
                <Pill key={s.strategy} variant="outline" className="text-ink-2">
                  <span aria-hidden className="inline-block h-0.5 w-3 rounded-full" style={{ background: color }} />
                  {s.strategy}: <span className="font-mono text-ink">{last === null ? "no trades" : money(last)}</span>
                </Pill>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}

export function Lead({ reads, dataset, onRetry }: { reads: Reads | null; dataset: Dataset; onRetry: () => void }) {
  const sl = reads?.slots;
  const eq = reads ? ok(reads.equity) : null;
  const home = reads ? ok(reads.home) : null;
  const rep = reads ? ok(reads.report) : null;
  const report = rep ? rep.report : null;
  const lines = report ? report.split("\n") : [];
  const verdict = report ? splitVerdict(lines[0] ?? "") : null;
  const trailLines = lines.slice(1);
  const slots: Slots | null = sl && sl.ok ? sl.v : null;
  const pot = slots ? slots.paper_equity_dollars : null;
  const start = slots ? slots.paper_pot_start : 0;
  const target = (slots?.stage_target ?? home?.stage_target)?.target_dollars ?? 1000;
  const pct = pot === null ? 0 : Math.max(0, Math.min(100, (pot / target) * 100));
  const change = pot === null ? 0 : pot - start;
  const reached = pot !== null && pot >= target;
  const traded = slots ? slots.slots.reduce((n, s) => n + s.windows_traded, 0) + slots.retired.reduce((n, s) => n + s.windows_traded, 0) : 0;
  const settled = home?.collector.settled_windows ?? null;

  return (
    <>
      <div className="grid min-w-0 gap-8 bg-surface p-5 lg:grid-cols-[minmax(0,7fr)_minmax(0,5fr)] lg:p-6">
        <section id="report-card" aria-labelledby="report-title" className="min-w-0 scroll-mt-28">
          <div className="flex flex-wrap items-center gap-2">
            <h2 id="report-title" tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">Daily report</h2>
            <Pill variant="outline" className="text-info-foreground">venue</Pill>
            <Pill variant="outline" className="text-ink-2">tape</Pill>
          </div>
          {rep === null && reads && !reads.report.ok && <div className="mt-3"><PanelError msg={reads.report.msg} onRetry={onRetry} /></div>}
          {!verdict && !(reads && !reads.report.ok) && <span className="mt-4 block h-12 w-2/3 rounded-md bg-track" aria-hidden />}
          {verdict && (
            <>
              <p id="report-head" className="mt-4 break-words text-[clamp(1.75rem,3.4vw,2.5rem)] leading-[1.08] font-medium tracking-[-0.03em] text-ink text-balance">{verdict.head}</p>
              {verdict.rest && <p id="report-rest" className="mt-3 max-w-[60ch] text-base leading-relaxed text-ink-2">{verdict.rest}</p>}
            </>
          )}
        </section>

        <section id="pot" aria-labelledby="pot-title" className="min-w-0 scroll-mt-28">
          <div className="flex flex-wrap items-center gap-2">
            <h2 id="pot-title" tabIndex={-1} className="mr-auto text-sm font-medium text-ink-2 outline-none">Paper pot</h2>
            <Pill variant="outline" className="text-warning-foreground">paper</Pill>
          </div>
          {sl && !sl.ok && <div className="mt-3"><PanelError msg={sl.msg} onRetry={onRetry} /></div>}
          {pot === null && !(sl && !sl.ok) && <span className="mt-4 block h-12 w-48 rounded-md bg-track" aria-hidden />}
          {pot !== null && (
            <>
              <div className="mt-4 flex flex-wrap items-end gap-x-6 gap-y-3">
                <PotFigure pot={pot} />
                <div className="flex min-w-0 items-center gap-3 pb-1">
                  <ProgressCircle value={pct} radius={30} strokeWidth={6}>
                    <span className="font-mono text-xs text-ink">{Math.round(pct)}%</span>
                  </ProgressCircle>
                </div>
              </div>
              <p id="pot-change" className={"mt-2 font-mono text-sm tabular-nums " + (change > 0 ? "text-success-foreground" : change < 0 ? "text-unread" : "text-ink-2")}>
                {(change >= 0 ? "+" : "-") + "$" + Math.abs(change).toFixed(2) + " against the $" + start.toFixed(2) + " start (paper)"}
              </p>
              <p id="stage-line" className="mt-3 text-sm text-ink-2">
                {reached
                  ? "Stage target (paper): " + money(pot) + " of the " + money(target) + " stage-one target, reached on paper. Paper only, not venue money."
                  : "Stage target (paper): " + money(pot) + " of the " + money(target) + " stage-one target, " + pct.toFixed(1) + "% of the way, " + money(target - pot) + " to go. Paper only, not venue money."}
              </p>
            </>
          )}
        </section>
      </div>

      <section id="chart-card" aria-labelledby="chart-title" className="min-w-0 scroll-mt-28 bg-surface p-5 lg:p-6">
        <div className="flex flex-wrap items-center gap-2">
          <h2 id="chart-title" className="mr-auto text-sm font-medium text-ink-2">Paper equity over the collected tape</h2>
          <Pill variant="outline" className="text-warning-foreground">paper</Pill>
        </div>
        {reads && !reads.equity.ok && <div className="mt-3"><PanelError msg={reads.equity.msg} onRetry={onRetry} /></div>}
        <div className="mt-4"><Trace eq={eq} start={start} traded={traded} settled={settled} /></div>
      </section>

      {report && (
        <section id="trail-card" aria-labelledby="trail-title" className="min-w-0 scroll-mt-28 bg-surface p-5 lg:p-6">
          <div className="flex flex-wrap items-center gap-2">
            <h2 id="trail-title" className="mr-auto text-sm font-medium text-ink-2">The report, line by line</h2>
            <span className="text-sm text-ink-2">only what the report wrote</span>
          </div>
          <div className="mt-4"><AuditTrail lines={trailLines} /></div>
          <Expandable expandDirection="vertical" expandBehavior="push" id="report-body" className="mt-4 w-full">
            {({ isExpanded }: { isExpanded: boolean }) => (
              <>
                <ExpandableTrigger aria-expanded={isExpanded} aria-controls="report"
                  className="flex min-h-11 w-full cursor-pointer items-center justify-between gap-3 rounded-md border border-hairline px-4 text-left outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) hover:bg-board focus-visible:ring-2 focus-visible:ring-blue">
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
      )}
    </>
  );
}
