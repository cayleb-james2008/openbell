// Labelled paper lines for the ledger: a row sparkline (still, like the rest of the first screen) and the strategy drawer's chart (draws once when opened).
// Hairline axes, tabular mono ticks; blue when a series ends at or above its start, red below; reduced motion shows the chart finished.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { motion, useReducedMotion } from "motion/react";
import { dayLabel, isNum, moneyWhole } from "@/lib/desk";
import { drawOnce } from "@/motionProps";

export type Point = [number, number];

function validPoints(points: unknown): Point[] {
  if (!Array.isArray(points)) return [];
  return points.filter((p): p is Point => Array.isArray(p) && isNum(p[0]) && isNum(p[1]));
}

export function Spark({ points, start, w = 120, h = 28, label }: { points: Point[]; start: number; w?: number; h?: number; label: string }) {
  const pts = validPoints(points);
  const s0 = isNum(start) ? start : 0;
  if (pts.length < 2) return <span className="text-sm text-ink-3">no paper trades</span>;
  const xs = pts.map((p) => p[0]), ys = pts.map((p) => p[1]).concat(s0);
  const x0 = Math.min(...xs), x1 = Math.max(...xs), y0 = Math.min(...ys), y1 = Math.max(...ys);
  const d = pts.map((p, i) => (i ? "L" : "M") + (((p[0] - x0) / Math.max(1, x1 - x0)) * (w - 2) + 1).toFixed(1) + "," + ((h - 3) - ((p[1] - y0) / Math.max(1e-9, y1 - y0)) * (h - 6)).toFixed(1)).join(" ");
  const up = pts[pts.length - 1][1] >= s0;
  return (
    <svg role="img" aria-label={label + (up ? ", ending at or above its start" : ", ending below its start")} width={w} height={h} viewBox={`0 0 ${w} ${h}`} className="block overflow-visible">
      <path d={d} fill="none" strokeWidth={1.5} strokeLinejoin="round" style={{ stroke: up ? "var(--dot-blue)" : "var(--dot-red)" }} />
    </svg>
  );
}

function useWidth(min = 260) {
  const ref = useRef<HTMLDivElement>(null);
  const [w, setW] = useState(420);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setW(Math.max(min, Math.round(e.contentRect.width))));
    ro.observe(el);
    return () => ro.disconnect();
  }, [min]);
  return [ref, w] as const;
}

const PADL = 56, PADR = 8, PADT = 10, PADB = 24;

export function SeriesChart({ points, start, height, label, empty }: { points: Point[]; start: number; height: number; label: string; empty: ReactNode }) {
  const [ref, W] = useWidth();
  const reduce = useReducedMotion();
  const pts = validPoints(points);
  const s0 = isNum(start) ? start : 0;
  const H = height, top = PADT, bottom = H - PADB;
  if (pts.length < 2) return <div ref={ref}>{empty}</div>;
  const x0 = pts[0][0], x1 = pts[pts.length - 1][0];
  const all = pts.map((p) => p[1]).concat(s0);
  const lo = Math.min(...all), hi = Math.max(...all), pad = Math.max(1, (hi - lo) * 0.06);
  const y0 = lo - pad, y1 = hi + pad;
  const X = (t: number) => PADL + ((t - x0) / Math.max(1, x1 - x0)) * (W - PADL - PADR);
  const Y = (v: number) => bottom - ((v - y0) / Math.max(1e-9, y1 - y0)) * (bottom - top);
  const d = pts.map((p, j) => (j ? "L" : "M") + X(p[0]).toFixed(1) + "," + Y(p[1]).toFixed(1)).join(" ");
  const up = pts[pts.length - 1][1] >= s0;
  return (
    <div ref={ref} className="min-w-0">
      <svg role="img" aria-label={label} viewBox={`0 0 ${W} ${H}`} width={W} height={H} className="block h-auto max-w-full">
        {[y1 - pad, y0 + pad].map((t, i) => (
          <g key={i}>
            <line x1={PADL} x2={W - PADR} y1={Y(t)} y2={Y(t)} style={{ stroke: "var(--hairline)" }} />
            <text x={PADL - 8} y={Y(t) + 4} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-3)" }}>{moneyWhole(t)}</text>
          </g>
        ))}
        <line x1={PADL} x2={W - PADR} y1={Y(s0)} y2={Y(s0)} strokeDasharray="3 3" style={{ stroke: "var(--ink-3)" }} />
        <motion.path key={d.length} d={d} fill="none" strokeWidth={2} strokeLinejoin="round" style={{ stroke: up ? "var(--dot-blue)" : "var(--dot-red)" }}
          initial={reduce ? false : { pathLength: 0 }} animate={{ pathLength: 1 }} {...drawOnce} />
        <text x={PADL} y={H - 6} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>{dayLabel(x0)}</text>
        <text x={W - PADR} y={H - 6} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>{dayLabel(x1)}</text>
      </svg>
    </div>
  );
}
