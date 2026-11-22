// Institutional paper quant SVG charts: compounding equity + drawdown underwater subplot,
// interactive market price + spread history with crosshair tooltip, calibration reliability
// with 95% Wilson error bars, 1,000-path Monte Carlo percentile fan, and Tremor sparklines.
import { useEffect, useId, useRef, useState } from "react";
import { motion, useReducedMotion } from "motion/react";
import { SparkAreaChart } from "@/components/SparkChart/SparkChart";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import {
  type Market,
  cents,
  dayLabel,
  impliedYesProb,
  isNum,
  marketSpreadCents,
  money,
  moneySigned,
  moneyWhole,
  timeUtcFullLabel,
  timeUtcLabel,
} from "@/lib/desk";
import type { CalibrationBucket, MonteCarloData } from "@/lib/quant";
import { drawOnce } from "@/motionProps";

export type Point = [number, number];

export function validPoints(points: unknown): Point[] {
  if (!Array.isArray(points)) return [];
  return points.filter((p): p is Point => Array.isArray(p) && isNum(p[0]) && isNum(p[1]));
}

function useContainerWidth(min = 180, fallback = 540) {
  const ref = useRef<HTMLDivElement>(null);
  const [w, setW] = useState(fallback);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => {
      const next = Math.max(min, Math.floor(el.getBoundingClientRect().width));
      if (next > 0) setW(next);
    };
    measure();
    const ro = new ResizeObserver(([e]) => {
      const next = Math.max(min, Math.floor(e.contentRect.width));
      setW(next);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [min]);
  return [ref, w] as const;
}

export function TremorSpark({
  points,
  start,
  label,
  className = "h-8 w-24",
}: {
  points: Point[];
  start: number;
  label: string;
  className?: string;
}) {
  const pts = validPoints(points);
  const s0 = isNum(start) ? start : 0;
  if (pts.length < 2) {
    return <span className="font-mono text-xs text-ink-3">no trades</span>;
  }
  const data = pts.map(([t, v], idx) => ({ idx, t, value: Number(v.toFixed(2)) }));
  const up = pts[pts.length - 1][1] >= s0;
  return (
    <div aria-label={label} className="inline-flex items-center">
      <SparkAreaChart
        data={data}
        index="idx"
        categories={["value"]}
        colors={[up ? "blue" : "pink"]}
        autoMinValue
        fill="gradient"
        className={className}
      />
    </div>
  );
}

const PADL = 60;
const PADR = 14;
const PADT = 14;
const PADB = 26;

export function HeroEquityChart({
  points,
  start,
  target = 1000,
  height = 178,
  liveWarming = false,
  snapshots = 0,
  openMarkets = 0,
}: {
  points: Point[];
  start: number;
  target?: number;
  height?: number;
  liveWarming?: boolean;
  snapshots?: number;
  openMarkets?: number;
}) {
  const [ref, W] = useContainerWidth(180, 640);
  const reduce = useReducedMotion();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  const gradId = useId().replace(/:/g, "");
  const pts = validPoints(points);
  const s0 = isNum(start) ? start : 100;
  const H = height;
  const top = PADT;
  const bottom = H - PADB;

  if (pts.length < 2) {
    const midY = Math.round((top + bottom) / 2);
    return (
      <div ref={ref} className="min-w-0">
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2 text-xs text-ink-2">
          <span>
            {liveWarming
              ? `Live tape recording (${snapshots.toLocaleString()} snapshots across ${openMarkets} open 15m markets)`
              : "Awaiting first settled paper window"}
          </span>
          <span className="font-mono text-ink-3">Start {money(s0)} · Stage 1 {moneyWhole(target)}</span>
        </div>
        <svg
          role="img"
          aria-label="Paper equity curves awaiting first settled paper window"
          viewBox={`0 0 ${W} ${H}`}
          width={W}
          height={H}
          className="block h-auto max-w-full"
        >
          <line x1={PADL} x2={W - PADR} y1={top + 8} y2={top + 8} style={{ stroke: "var(--hairline)" }} />
          <line x1={PADL} x2={W - PADR} y1={midY} y2={midY} strokeDasharray="4 4" style={{ stroke: "var(--dot-blue)" }} />
          <line x1={PADL} x2={W - PADR} y1={bottom} y2={bottom} style={{ stroke: "var(--hairline)" }} />
          <text x={PADL - 8} y={midY + 4} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-2)" }}>
            {moneyWhole(s0)}
          </text>
          <text x={PADL} y={H - 6} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
            {W < 260 ? "Baseline" : "Paper baseline"}
          </text>
          <text x={W - PADR} y={H - 6} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
            {W < 420 ? "Pending" : "First settled window pending"}
          </text>
        </svg>
      </div>
    );
  }

  const x0 = pts[0][0];
  const x1 = pts[pts.length - 1][0];
  const allVals = pts.map((p) => p[1]).concat(s0);
  const lo = Math.min(...allVals);
  const hi = Math.max(...allVals);
  const pad = Math.max(1, (hi - lo) * 0.08);
  const y0 = lo - pad;
  const y1 = hi + pad;
  const X = (t: number) => PADL + ((t - x0) / Math.max(1, x1 - x0)) * (W - PADL - PADR);
  const Y = (v: number) => bottom - ((v - y0) / Math.max(1e-9, y1 - y0)) * (bottom - top);

  const lineD = pts.map((p, i) => `${i ? "L" : "M"}${X(p[0]).toFixed(1)},${Y(p[1]).toFixed(1)}`).join(" ");
  const areaD = `${lineD} L${X(x1).toFixed(1)},${bottom.toFixed(1)} L${X(x0).toFixed(1)},${bottom.toFixed(1)} Z`;
  const last = pts[pts.length - 1];
  const up = last[1] >= s0;
  const strokeColor = up ? "var(--dot-blue)" : "var(--dot-red)";

  const activeIdx = hoverIdx !== null && hoverIdx >= 0 && hoverIdx < pts.length ? hoverIdx : pts.length - 1;
  const activePt = pts[activeIdx];
  const activeDelta = activePt[1] - s0;

  const onMove = (clientX: number, rect: DOMRect) => {
    const rel = clientX - rect.left;
    const ratio = Math.min(1, Math.max(0, (rel - PADL) / Math.max(1, W - PADL - PADR)));
    const targetT = x0 + ratio * (x1 - x0);
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < pts.length; i++) {
      const dist = Math.abs(pts[i][0] - targetT);
      if (dist < bestDist) {
        bestDist = dist;
        best = i;
      }
    }
    setHoverIdx(best);
  };

  return (
    <div ref={ref} className="min-w-0">
      <div className="mb-2 flex flex-wrap items-center justify-between gap-2 text-xs">
        <div className="flex flex-wrap items-center gap-2 font-mono tabular-nums">
          <span className="font-medium text-ink">{money(activePt[1])}</span>
          <span className={activeDelta >= 0 ? "text-success-foreground" : "text-destructive-foreground"}>
            {moneySigned(activeDelta)}
          </span>
          <span className="text-ink-3">
            {dayLabel(activePt[0])} {timeUtcLabel(activePt[0])}
          </span>
        </div>
        <span className="font-mono text-ink-3">
          {pts.length} paper checkpoints · Start {moneyWhole(s0)}
        </span>
      </div>
      <svg
        role="img"
        aria-label={`Paper equity curves for the paper compounding pot, ${pts.length} points ending at ${money(last[1])}`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
        onMouseMove={(e) => onMove(e.clientX, e.currentTarget.getBoundingClientRect())}
        onMouseLeave={() => setHoverIdx(null)}
      >
        <defs>
          <linearGradient id={`ob-hero-eq-${gradId}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={strokeColor} stopOpacity="0.16" />
            <stop offset="100%" stopColor={strokeColor} stopOpacity="0.01" />
          </linearGradient>
        </defs>
        {[hi, lo].map((tick, idx) => (
          <g key={idx}>
            <line x1={PADL} x2={W - PADR} y1={Y(tick)} y2={Y(tick)} style={{ stroke: "var(--hairline)" }} />
            <text
              x={PADL - 8}
              y={Y(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {moneyWhole(tick)}
            </text>
          </g>
        ))}
        <line x1={PADL} x2={W - PADR} y1={Y(s0)} y2={Y(s0)} strokeDasharray="3 3" style={{ stroke: "var(--ink-3)" }} />
        <path d={areaD} fill={`url(#ob-hero-eq-${gradId})`} />
        <motion.path
          key={lineD.length}
          d={lineD}
          fill="none"
          strokeWidth={2}
          strokeLinejoin="round"
          strokeLinecap="round"
          style={{ stroke: strokeColor }}
          initial={reduce ? false : { pathLength: 0 }}
          animate={{ pathLength: 1 }}
          {...drawOnce}
        />
        {hoverIdx !== null && (
          <line
            x1={X(activePt[0])}
            x2={X(activePt[0])}
            y1={top}
            y2={bottom}
            strokeDasharray="2 2"
            style={{ stroke: "var(--ink-3)" }}
          />
        )}
        <circle cx={X(activePt[0])} cy={Y(activePt[1])} r={4} style={{ fill: strokeColor, stroke: "var(--surface)", strokeWidth: 2 }} />
        <text x={PADL} y={H - 6} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {W < 260 ? dayLabel(x0).slice(5) : dayLabel(x0)}
        </text>
        <text x={W - PADR} y={H - 6} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {W < 260 ? dayLabel(x1).slice(5) : dayLabel(x1)}
        </text>
      </svg>
    </div>
  );
}

/** Dual-pane synchronized Compounding Paper Equity Curve + Drawdown Underwater Subplot for Slot Desk. */
export function EquityDrawdownChart({
  points,
  start,
}: {
  points: Point[];
  start: number;
}) {
  const [ref, W] = useContainerWidth(180, 680);
  const reduce = useReducedMotion();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  const gradId = useId().replace(/:/g, "");
  const pts = validPoints(points);
  const s0 = isNum(start) && start > 0 ? start : 100;

  if (pts.length < 2) {
    return (
      <div ref={ref} className="rounded-lg border border-dashed border-hairline bg-board/40 px-4 py-6 text-xs text-ink-2">
        At least two settled paper checkpoints are required to chart equity and underwater drawdown.
      </div>
    );
  }

  // Compute running peak and underwater drawdown %
  let runPeak = s0;
  const enriched = pts.map(([t, eq]) => {
    if (eq > runPeak) runPeak = eq;
    const ddPct = runPeak > 0 ? ((eq - runPeak) / runPeak) * 100 : 0;
    return { t, eq, peak: runPeak, ddPct };
  });
  const maxDdAbs = Math.max(1, ...enriched.map((e) => Math.abs(e.ddPct)));
  const actualMaxDd = Math.min(...enriched.map((e) => e.ddPct));

  const eqH = 152;
  const gap = 16;
  const ddH = 84;
  const H = eqH + gap + ddH;

  const eqTop = 14;
  const eqBot = eqH - 12;
  const ddTop = eqH + gap;
  const ddBot = H - 24;

  const x0 = enriched[0].t;
  const x1 = enriched[enriched.length - 1].t;
  const eqVals = enriched.map((e) => e.eq).concat(s0);
  const lo = Math.min(...eqVals);
  const hi = Math.max(...eqVals);
  const pad = Math.max(1, (hi - lo) * 0.08);
  const y0 = lo - pad;
  const y1 = hi + pad;

  const X = (t: number) => PADL + ((t - x0) / Math.max(1, x1 - x0)) * (W - PADL - PADR);
  const Yeq = (v: number) => eqBot - ((v - y0) / Math.max(1e-9, y1 - y0)) * (eqBot - eqTop);
  const Ydd = (dd: number) => ddTop + (Math.abs(dd) / maxDdAbs) * (ddBot - ddTop);

  const eqLine = enriched.map((e, i) => `${i ? "L" : "M"}${X(e.t).toFixed(1)},${Yeq(e.eq).toFixed(1)}`).join(" ");
  const eqArea = `${eqLine} L${X(x1).toFixed(1)},${eqBot.toFixed(1)} L${X(x0).toFixed(1)},${eqBot.toFixed(1)} Z`;

  const ddLine = enriched.map((e, i) => `${i ? "L" : "M"}${X(e.t).toFixed(1)},${Ydd(e.ddPct).toFixed(1)}`).join(" ");
  const ddArea = `M${X(x0).toFixed(1)},${ddTop.toFixed(1)} ` +
    enriched.map((e) => `L${X(e.t).toFixed(1)},${Ydd(e.ddPct).toFixed(1)}`).join(" ") +
    ` L${X(x1).toFixed(1)},${ddTop.toFixed(1)} Z`;

  const last = enriched[enriched.length - 1];
  const up = last.eq >= s0;
  const strokeColor = up ? "var(--dot-blue)" : "var(--dot-red)";

  const activeIdx = hoverIdx !== null && hoverIdx >= 0 && hoverIdx < enriched.length ? hoverIdx : enriched.length - 1;
  const active = enriched[activeIdx];
  const delta = active.eq - s0;
  const deltaPct = s0 > 0 ? (delta / s0) * 100 : 0;

  const onMove = (clientX: number, rect: DOMRect) => {
    const rel = clientX - rect.left;
    const ratio = Math.min(1, Math.max(0, (rel - PADL) / Math.max(1, W - PADL - PADR)));
    const targetT = x0 + ratio * (x1 - x0);
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < enriched.length; i++) {
      const dist = Math.abs(enriched[i].t - targetT);
      if (dist < bestDist) {
        bestDist = dist;
        best = i;
      }
    }
    setHoverIdx(best);
  };

  return (
    <div ref={ref} className="min-w-0" data-slot-equity-dd>
      <div className="mb-2 flex flex-wrap items-center justify-between gap-2 text-xs">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono tabular-nums" data-slot-dd-tooltip>
          <span className="font-semibold text-ink">{money(active.eq)}</span>
          <span className={delta >= 0 ? "text-success-foreground" : "text-destructive-foreground"}>
            {moneySigned(delta)} ({deltaPct >= 0 ? "+" : ""}{deltaPct.toFixed(1)}%)
          </span>
          <span className="text-ink-2">Peak {money(active.peak)}</span>
          <span className={active.ddPct < -0.01 ? "font-medium text-destructive-foreground" : "text-ink-2"}>
            DD {active.ddPct.toFixed(2)}%
          </span>
          <span className="text-ink-3">
            {dayLabel(active.t)} {timeUtcLabel(active.t)}
          </span>
        </div>
        <span className="font-mono text-ink-3">
          Max DD {actualMaxDd.toFixed(2)}%
        </span>
      </div>

      <svg
        role="img"
        aria-label={`Paper equity curves and underwater drawdown subplot, max drawdown ${actualMaxDd.toFixed(2)} percent`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
        onMouseMove={(e) => onMove(e.clientX, e.currentTarget.getBoundingClientRect())}
        onMouseLeave={() => setHoverIdx(null)}
      >
        <defs>
          <linearGradient id={`ob-dd-eq-${gradId}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={strokeColor} stopOpacity="0.16" />
            <stop offset="100%" stopColor={strokeColor} stopOpacity="0.01" />
          </linearGradient>
          <linearGradient id={`ob-dd-uw-${gradId}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--dot-red)" stopOpacity="0.28" />
            <stop offset="100%" stopColor="var(--dot-red)" stopOpacity="0.08" />
          </linearGradient>
        </defs>

        {/* Top pane: Compounding paper equity */}
        {[hi, lo].map((tick, idx) => (
          <g key={idx}>
            <line x1={PADL} x2={W - PADR} y1={Yeq(tick)} y2={Yeq(tick)} style={{ stroke: "var(--hairline)" }} />
            <text
              x={PADL - 8}
              y={Yeq(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {moneyWhole(tick)}
            </text>
          </g>
        ))}
        <line x1={PADL} x2={W - PADR} y1={Yeq(s0)} y2={Yeq(s0)} strokeDasharray="3 3" style={{ stroke: "var(--ink-3)" }} />
        <path d={eqArea} fill={`url(#ob-dd-eq-${gradId})`} />
        <motion.path
          key={eqLine.length}
          d={eqLine}
          fill="none"
          strokeWidth={2}
          strokeLinejoin="round"
          strokeLinecap="round"
          style={{ stroke: strokeColor }}
          initial={reduce ? false : { pathLength: 0 }}
          animate={{ pathLength: 1 }}
          {...drawOnce}
        />

        {/* Bottom pane: Drawdown underwater subplot */}
        <line x1={PADL} x2={W - PADR} y1={ddTop} y2={ddTop} style={{ stroke: "var(--hairline)" }} />
        <line x1={PADL} x2={W - PADR} y1={ddBot} y2={ddBot} style={{ stroke: "var(--hairline)" }} />
        <text x={PADL - 8} y={ddTop + 4} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-3)" }}>
          0%
        </text>
        <text x={PADL - 8} y={ddBot + 4} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--destructive-foreground)" }}>
          -{maxDdAbs.toFixed(0)}%
        </text>
        <path d={ddArea} fill={`url(#ob-dd-uw-${gradId})`} />
        <path d={ddLine} fill="none" strokeWidth={1.5} strokeLinejoin="round" style={{ stroke: "var(--dot-red)" }} />

        {/* Synchronized crosshair across both subplots */}
        {hoverIdx !== null && (
          <line
            x1={X(active.t)}
            x2={X(active.t)}
            y1={eqTop}
            y2={ddBot}
            strokeDasharray="2 2"
            style={{ stroke: "var(--ink-3)" }}
          />
        )}
        <circle cx={X(active.t)} cy={Yeq(active.eq)} r={4} style={{ fill: strokeColor, stroke: "var(--surface)", strokeWidth: 2 }} />
        <circle cx={X(active.t)} cy={Ydd(active.ddPct)} r={3.5} style={{ fill: "var(--dot-red)", stroke: "var(--surface)", strokeWidth: 1.5 }} />

        {/* Bottom date axis */}
        <text x={PADL} y={H - 5} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {W < 280 ? dayLabel(x0).slice(5) : dayLabel(x0)}
        </text>
        <text x={W - PADR} y={H - 5} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {W < 280 ? dayLabel(x1).slice(5) : dayLabel(x1)}
        </text>
      </svg>
    </div>
  );
}

export function StrategyEquityChart({
  points,
  start = 0,
  height = 170,
  label,
}: {
  points: Point[];
  start?: number;
  height?: number;
  label: string;
}) {
  const [ref, W] = useContainerWidth(220, 420);
  const reduce = useReducedMotion();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  const pts = validPoints(points);
  const s0 = isNum(start) ? start : 0;
  const H = height;
  const top = PADT;
  const bottom = H - PADB;

  if (pts.length < 2) {
    return (
      <div ref={ref} className="rounded-lg border border-hairline bg-board px-4 py-6 text-sm text-ink-2">
        No paper trades for this strategy on the active tape yet.
      </div>
    );
  }

  const x0 = pts[0][0];
  const x1 = pts[pts.length - 1][0];
  const all = pts.map((p) => p[1]).concat(s0);
  const lo = Math.min(...all);
  const hi = Math.max(...all);
  const pad = Math.max(0.5, (hi - lo) * 0.08);
  const y0 = lo - pad;
  const y1 = hi + pad;
  const X = (t: number) => PADL + ((t - x0) / Math.max(1, x1 - x0)) * (W - PADL - PADR);
  const Y = (v: number) => bottom - ((v - y0) / Math.max(1e-9, y1 - y0)) * (bottom - top);
  const d = pts.map((p, i) => `${i ? "L" : "M"}${X(p[0]).toFixed(1)},${Y(p[1]).toFixed(1)}`).join(" ");
  const up = pts[pts.length - 1][1] >= s0;
  const strokeColor = up ? "var(--dot-blue)" : "var(--dot-red)";

  const activeIdx = hoverIdx !== null && hoverIdx >= 0 && hoverIdx < pts.length ? hoverIdx : pts.length - 1;
  const activePt = pts[activeIdx];

  const onMove = (clientX: number, rect: DOMRect) => {
    const rel = clientX - rect.left;
    const ratio = Math.min(1, Math.max(0, (rel - PADL) / Math.max(1, W - PADL - PADR)));
    const targetT = x0 + ratio * (x1 - x0);
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < pts.length; i++) {
      const dist = Math.abs(pts[i][0] - targetT);
      if (dist < bestDist) {
        bestDist = dist;
        best = i;
      }
    }
    setHoverIdx(best);
  };

  return (
    <div ref={ref} className="min-w-0">
      <div className="mb-1.5 flex flex-wrap items-center justify-between gap-2 font-mono text-xs tabular-nums">
        <span className={activePt[1] >= s0 ? "font-medium text-success-foreground" : "font-medium text-destructive-foreground"}>
          {moneySigned(activePt[1])}
        </span>
        <span className="text-ink-3">
          {dayLabel(activePt[0])} {timeUtcLabel(activePt[0])}
        </span>
      </div>
      <svg
        role="img"
        aria-label={label}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
        onMouseMove={(e) => onMove(e.clientX, e.currentTarget.getBoundingClientRect())}
        onMouseLeave={() => setHoverIdx(null)}
      >
        {[hi, lo].map((tick, idx) => (
          <g key={idx}>
            <line x1={PADL} x2={W - PADR} y1={Y(tick)} y2={Y(tick)} style={{ stroke: "var(--hairline)" }} />
            <text
              x={PADL - 8}
              y={Y(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {moneyWhole(tick)}
            </text>
          </g>
        ))}
        <line x1={PADL} x2={W - PADR} y1={Y(s0)} y2={Y(s0)} strokeDasharray="3 3" style={{ stroke: "var(--ink-3)" }} />
        <motion.path
          key={d.length}
          d={d}
          fill="none"
          strokeWidth={2}
          strokeLinejoin="round"
          strokeLinecap="round"
          style={{ stroke: strokeColor }}
          initial={reduce ? false : { pathLength: 0 }}
          animate={{ pathLength: 1 }}
          {...drawOnce}
        />
        {hoverIdx !== null && (
          <line
            x1={X(activePt[0])}
            x2={X(activePt[0])}
            y1={top}
            y2={bottom}
            strokeDasharray="2 2"
            style={{ stroke: "var(--ink-3)" }}
          />
        )}
        <circle cx={X(activePt[0])} cy={Y(activePt[1])} r={3.5} style={{ fill: strokeColor, stroke: "var(--surface)", strokeWidth: 1.5 }} />
        <text x={PADL} y={H - 6} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {W < 260 ? dayLabel(x0).slice(5) : dayLabel(x0)}
        </text>
        <text x={W - PADR} y={H - 6} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {W < 260 ? dayLabel(x1).slice(5) : dayLabel(x1)}
        </text>
      </svg>
    </div>
  );
}

/** Interactive SVG Price + Spread History Chart with hover crosshair tooltip (timestamp, YES/NO bid/ask, implied prob, spread). */
export function MarketHistoryChart({
  points,
  ticker,
  height = 224,
}: {
  points: Market[];
  ticker: string;
  height?: number;
}) {
  const [ref, W] = useContainerWidth(220, 440);
  const reduce = useReducedMotion();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);

  const series = points
    .filter((m) => !!m && typeof m === "object")
    .map((m, idx) => {
      const prob = impliedYesProb(m);
      if (prob === null) return null;
      const t = isNum(m.fetched_at) ? m.fetched_at : idx;
      const spread = marketSpreadCents(m) ?? 0;
      const yesBid = isNum(m.yes_bid) ? Math.min(1, Math.max(0, m.yes_bid)) : prob;
      const yesAsk = isNum(m.yes_ask) ? Math.min(1, Math.max(0, m.yes_ask)) : prob;
      return { t, prob, spread, yesBid, yesAsk, raw: m };
    })
    .filter((p): p is NonNullable<typeof p> => p !== null);

  if (series.length === 0) {
    return (
      <div ref={ref} className="rounded-lg border border-hairline bg-board px-4 py-6 text-sm text-ink-2">
        No quoted YES prices recorded for this ticker yet.
      </div>
    );
  }

  const H = height;
  const priceTop = 14;
  const priceBot = 132;
  const spreadTop = 150;
  const spreadBot = H - 24;

  const x0 = series[0].t;
  const x1 = series[series.length - 1].t;
  const spanX = Math.max(1, x1 - x0);
  const X = (t: number, idx: number) =>
    series.length === 1
      ? (PADL + W - PADR) / 2
      : spanX <= 0
        ? PADL + (idx / Math.max(1, series.length - 1)) * (W - PADL - PADR)
        : PADL + ((t - x0) / spanX) * (W - PADL - PADR);
  const Yp = (v: number) => priceBot - Math.min(1, Math.max(0, v)) * (priceBot - priceTop);

  const maxSpread = Math.max(4, ...series.map((s) => s.spread));
  const Ys = (sp: number) => spreadBot - (Math.min(maxSpread, Math.max(0, sp)) / maxSpread) * (spreadBot - spreadTop);

  const priceD = series.map((p, i) => `${i ? "L" : "M"}${X(p.t, i).toFixed(1)},${Yp(p.prob).toFixed(1)}`).join(" ");
  const corridorD =
    series.length >= 2
      ? series.map((p, i) => `${i ? "L" : "M"}${X(p.t, i).toFixed(1)},${Yp(p.yesAsk).toFixed(1)}`).join(" ") +
        " " +
        [...series]
          .reverse()
          .map((p, revIdx) => {
            const origIdx = series.length - 1 - revIdx;
            return `L${X(p.t, origIdx).toFixed(1)},${Yp(p.yesBid).toFixed(1)}`;
          })
          .join(" ") +
        " Z"
      : "";

  const defaultIdx = (() => {
    for (let i = series.length - 1; i >= 0; i--) {
      const r = series[i].raw;
      if (isNum(r.yes_bid) && isNum(r.yes_ask)) return i;
    }
    return series.length - 1;
  })();
  const activeIdx = hoverIdx !== null && hoverIdx >= 0 && hoverIdx < series.length ? hoverIdx : defaultIdx;
  const active = series[activeIdx];
  const barW = Math.max(3, Math.min(14, (W - PADL - PADR) / Math.max(8, series.length * 1.6)));

  const onMove = (clientX: number, rect: DOMRect) => {
    const rel = clientX - rect.left;
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < series.length; i++) {
      const dist = Math.abs(X(series[i].t, i) - rel);
      if (dist < bestDist) {
        bestDist = dist;
        best = i;
      }
    }
    setHoverIdx(best);
  };

  return (
    <div ref={ref} className="min-w-0" data-market-price-spread-chart>
      {/* Interactive crosshair readout tooltip bar */}
      <div
        data-market-crosshair-tooltip
        className="mb-2 flex flex-wrap items-center justify-between gap-x-3 gap-y-1 rounded-md border border-hairline bg-surface px-2.5 py-1.5 font-mono text-xs tabular-nums"
      >
        <span className="font-medium text-ink">
          {isNum(active.raw.fetched_at) ? timeUtcFullLabel(active.raw.fetched_at) : "Latest"}
        </span>
        <span className="text-success-foreground">
          YES {cents(active.raw.yes_bid)}/{cents(active.raw.yes_ask)}
        </span>
        <span className="text-info-foreground">
          NO {cents(active.raw.no_bid)}/{cents(active.raw.no_ask)}
        </span>
        <span className="font-medium text-ink">
          Prob {(active.prob * 100).toFixed(1)}%
        </span>
        <span className="text-ink-2">
          Spread {active.spread}c
        </span>
      </div>

      <svg
        role="img"
        aria-label={`YES implied probability and spread history for ${ticker}, ${series.length} snapshots`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
        onMouseMove={(e) => onMove(e.clientX, e.currentTarget.getBoundingClientRect())}
        onMouseLeave={() => setHoverIdx(null)}
      >
        {/* Top price pane grid */}
        {[1, 0.5, 0].map((tick) => (
          <g key={tick}>
            <line
              x1={PADL}
              x2={W - PADR}
              y1={Yp(tick)}
              y2={Yp(tick)}
              strokeDasharray={tick === 0.5 ? "3 3" : undefined}
              style={{ stroke: "var(--hairline)" }}
            />
            <text
              x={PADL - 8}
              y={Yp(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {Math.round(tick * 100)}c
            </text>
          </g>
        ))}

        {/* Bid-ask shaded corridor */}
        {corridorD && <path d={corridorD} style={{ fill: "var(--dot-blue)", opacity: 0.14 }} />}

        {/* Mid implied probability line */}
        {series.length >= 2 && (
          <motion.path
            key={priceD.length}
            d={priceD}
            fill="none"
            strokeWidth={2}
            strokeLinejoin="round"
            strokeLinecap="round"
            style={{ stroke: "var(--dot-blue)" }}
            initial={reduce ? false : { pathLength: 0 }}
            animate={{ pathLength: 1 }}
            {...drawOnce}
          />
        )}

        {/* Bottom spread history subplot */}
        <line x1={PADL} x2={W - PADR} y1={spreadTop} y2={spreadTop} style={{ stroke: "var(--hairline)" }} />
        <line x1={PADL} x2={W - PADR} y1={spreadBot} y2={spreadBot} style={{ stroke: "var(--hairline)" }} />
        <text x={PADL - 8} y={spreadTop + 4} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-3)" }}>
          {maxSpread}c
        </text>
        <text x={PADL - 8} y={spreadBot + 4} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-3)" }}>
          0c
        </text>
        {series.map((p, i) => {
          const cx = X(p.t, i);
          const yTop = Ys(p.spread);
          const hBar = Math.max(1, spreadBot - yTop);
          const isActive = i === activeIdx;
          return (
            <rect
              key={i}
              x={cx - barW / 2}
              y={yTop}
              width={barW}
              height={hBar}
              rx={1.5}
              style={{
                fill: isActive ? "var(--dot-amber)" : "var(--ink-3)",
                opacity: isActive ? 0.95 : 0.45,
              }}
            />
          );
        })}

        {/* Crosshair vertical line */}
        {hoverIdx !== null && (
          <line
            x1={X(active.t, activeIdx)}
            x2={X(active.t, activeIdx)}
            y1={priceTop}
            y2={spreadBot}
            strokeDasharray="2 2"
            style={{ stroke: "var(--ink-3)" }}
          />
        )}

        {/* Active point indicator */}
        <circle
          cx={X(active.t, activeIdx)}
          cy={Yp(active.prob)}
          r={4}
          style={{ fill: "var(--dot-blue)", stroke: "var(--surface)", strokeWidth: 2 }}
        />

        {/* Time labels */}
        <text x={PADL} y={H - 5} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {isNum(series[0].raw.fetched_at) ? timeUtcLabel(series[0].raw.fetched_at) : "Start"}
        </text>
        <text x={W - PADR} y={H - 5} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {series.length === 1
            ? "1 snapshot"
            : isNum(series[series.length - 1].raw.fetched_at)
              ? timeUtcLabel(series[series.length - 1].raw.fetched_at)
              : "Latest"}
        </text>
      </svg>
    </div>
  );
}

/** Calibration Reliability SVG Chart (Predicted probability vs Realized win rate + 95% Wilson CI whiskers). */
export function CalibrationReliabilityChart({
  buckets,
  height = 256,
}: {
  buckets: CalibrationBucket[];
  height?: number;
}) {
  const [ref, W] = useContainerWidth(220, 480);
  const reduce = useReducedMotion();
  const [hoverBin, setHoverBin] = useState<number | null>(null);

  const populated = buckets.filter((b) => b.count > 0);
  const H = height;
  const left = 52;
  const right = 18;
  const top = 16;
  const bottom = H - 30;

  const X = (p: number) => left + Math.min(1, Math.max(0, p)) * (W - left - right);
  const Y = (p: number) => bottom - Math.min(1, Math.max(0, p)) * (bottom - top);

  const active =
    hoverBin !== null
      ? populated.find((b) => b.binIndex === hoverBin) || populated[populated.length - 1] || null
      : populated[populated.length - 1] || null;

  const lineD = populated
    .map((b, i) => `${i ? "L" : "M"}${X(b.meanPredicted).toFixed(1)},${Y(b.realizedRate).toFixed(1)}`)
    .join(" ");

  const xTicks = W < 300 ? [0, 0.5, 1] : [0, 0.25, 0.5, 0.75, 1];

  return (
    <div ref={ref} className="min-w-0" data-calibration-chart>
      {/* Interactive bucket tooltip bar */}
      <div
        data-calibration-tooltip
        className="mb-2 flex flex-wrap items-center justify-between gap-2 rounded-md border border-hairline bg-surface px-2.5 py-1.5 font-mono text-xs tabular-nums"
      >
        {active ? (
          <>
            <span className="font-medium text-ink">
              Bin {Math.round(active.low * 100)}–{Math.round(active.high * 100)}% (n={active.count})
            </span>
            <span className="text-ink-2">Pred {(active.meanPredicted * 100).toFixed(1)}%</span>
            <span className="font-medium text-info-foreground">
              Realized {(active.realizedRate * 100).toFixed(1)}%
            </span>
            <span className="text-ink-3">
              95% CI [{(active.wilsonLow * 100).toFixed(0)}%, {(active.wilsonHigh * 100).toFixed(0)}%]
            </span>
          </>
        ) : (
          <span className="text-ink-2">No settled 15m windows recorded for reliability buckets yet.</span>
        )}
      </div>

      <svg
        role="img"
        aria-label={`Calibration reliability chart across ${populated.length} probability buckets with 95 percent Wilson error bars`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
      >
        {/* Horizontal & vertical gridlines */}
        {[0, 0.5, 1].map((tick) => (
          <g key={`y-${tick}`}>
            <line x1={left} x2={W - right} y1={Y(tick)} y2={Y(tick)} style={{ stroke: "var(--hairline)" }} />
            <text
              x={left - 8}
              y={Y(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {Math.round(tick * 100)}%
            </text>
          </g>
        ))}

        {xTicks.map((tick) => (
          <g key={`x-${tick}`}>
            <line x1={X(tick)} x2={X(tick)} y1={top} y2={bottom} style={{ stroke: "var(--hairline)", opacity: 0.6 }} />
            <text
              x={X(tick)}
              y={H - 8}
              textAnchor={tick === 0 ? "start" : tick === 1 ? "end" : "middle"}
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {Math.round(tick * 100)}%
            </text>
          </g>
        ))}

        {/* Diagonal perfect-calibration reference line y = x */}
        <line
          x1={X(0)}
          y1={Y(0)}
          x2={X(1)}
          y2={Y(1)}
          strokeDasharray="5 4"
          strokeWidth={1.5}
          style={{ stroke: "var(--ink-3)" }}
        />

        {/* Reliability curve connecting populated deciles */}
        {populated.length >= 2 && (
          <motion.path
            key={lineD.length}
            d={lineD}
            fill="none"
            strokeWidth={2}
            strokeLinejoin="round"
            style={{ stroke: "var(--dot-blue)" }}
            initial={reduce ? false : { pathLength: 0 }}
            animate={{ pathLength: 1 }}
            {...drawOnce}
          />
        )}

        {/* 95% Wilson confidence interval error bars + interactive bucket dots */}
        {populated.map((b) => {
          const cx = X(b.meanPredicted);
          const cy = Y(b.realizedRate);
          const yLo = Y(b.wilsonLow);
          const yHi = Y(b.wilsonHigh);
          const isSel = active?.binIndex === b.binIndex;
          const r = Math.min(7, Math.max(3.5, 3 + Math.log2(b.count + 1) * 0.7));
          return (
            <g
              key={b.binIndex}
              data-wilson-bar
              className="cursor-pointer"
              onMouseEnter={() => setHoverBin(b.binIndex)}
              onClick={() => setHoverBin(b.binIndex)}
            >
              {/* Invisible generous hit zone */}
              <circle cx={cx} cy={cy} r={14} fill="transparent" />
              {/* Vertical 95% Wilson CI whisker */}
              <line
                x1={cx}
                x2={cx}
                y1={yHi}
                y2={yLo}
                strokeWidth={1.75}
                style={{ stroke: isSel ? "var(--dot-blue)" : "var(--ink-2)" }}
              />
              {/* Top & bottom caps */}
              <line x1={cx - 4} x2={cx + 4} y1={yHi} y2={yHi} strokeWidth={1.5} style={{ stroke: isSel ? "var(--dot-blue)" : "var(--ink-2)" }} />
              <line x1={cx - 4} x2={cx + 4} y1={yLo} y2={yLo} strokeWidth={1.5} style={{ stroke: isSel ? "var(--dot-blue)" : "var(--ink-2)" }} />
              {/* Bucket center dot */}
              <circle
                cx={cx}
                cy={cy}
                r={isSel ? r + 1 : r}
                style={{
                  fill: isSel ? "var(--dot-blue)" : "var(--dot-green)",
                  stroke: "var(--surface)",
                  strokeWidth: 1.75,
                }}
              />
            </g>
          );
        })}
      </svg>
    </div>
  );
}

/** 1,000-Path Monte Carlo Bootstrap Fan Chart (P5/P25/P50/P75/P95 paper equity bands). */
export function MonteCarloFanChart({
  mc,
  height = 256,
}: {
  mc: MonteCarloData;
  height?: number;
}) {
  const [ref, W] = useContainerWidth(220, 480);
  const reduce = useReducedMotion();
  const [hoverStep, setHoverStep] = useState<number | null>(null);

  const { p5, p25, p50, p75, p95 } = mc.percentiles;
  const len = p50.length;
  const H = height;
  const top = 16;
  const bottom = H - 30;

  if (len < 2) {
    return (
      <div ref={ref} className="rounded-lg border border-dashed border-hairline bg-board/40 px-4 py-8 text-xs text-ink-2">
        Monte Carlo bootstrap needs settled paper trades to resample 1,000 paper equity paths.
      </div>
    );
  }

  const allVals = [...p5, ...p95, mc.startEquity];
  const lo = Math.min(...allVals);
  const hi = Math.max(...allVals);
  const pad = Math.max(2, (hi - lo) * 0.08);
  const y0 = Math.max(0, lo - pad);
  const y1 = hi + pad;

  const X = (step: number) => PADL + (step / Math.max(1, len - 1)) * (W - PADL - PADR);
  const Y = (v: number) => bottom - ((v - y0) / Math.max(1e-9, y1 - y0)) * (bottom - top);

  const bandPolygon = (upper: number[], lower: number[]) =>
    upper.map((v, i) => `${i ? "L" : "M"}${X(i).toFixed(1)},${Y(v).toFixed(1)}`).join(" ") +
    " " +
    [...lower]
      .reverse()
      .map((v, rev) => `L${X(len - 1 - rev).toFixed(1)},${Y(v).toFixed(1)}`)
      .join(" ") +
    " Z";

  const outerFan = bandPolygon(p95, p5);
  const innerFan = bandPolygon(p75, p25);
  const medianD = p50.map((v, i) => `${i ? "L" : "M"}${X(i).toFixed(1)},${Y(v).toFixed(1)}`).join(" ");

  const activeStep = hoverStep !== null && hoverStep >= 0 && hoverStep < len ? hoverStep : len - 1;

  const onMove = (clientX: number, rect: DOMRect) => {
    const rel = clientX - rect.left;
    const ratio = Math.min(1, Math.max(0, (rel - PADL) / Math.max(1, W - PADL - PADR)));
    setHoverStep(Math.round(ratio * (len - 1)));
  };

  return (
    <div ref={ref} className="min-w-0" data-montecarlo-chart>
      {/* Interactive step tooltip bar */}
      <div
        data-montecarlo-tooltip
        className="mb-2 flex flex-wrap items-center justify-between gap-x-2.5 gap-y-1 rounded-md border border-hairline bg-surface px-2.5 py-1.5 font-mono text-xs tabular-nums"
      >
        <span className="font-medium text-ink">Step {activeStep}/{len - 1}</span>
        <span className="text-success-foreground">P95 {moneyWhole(p95[activeStep])}</span>
        <span className="text-ink-2">P75 {moneyWhole(p75[activeStep])}</span>
        <span className="font-semibold text-info-foreground">P50 {money(p50[activeStep])}</span>
        <span className="text-ink-2">P25 {moneyWhole(p25[activeStep])}</span>
        <span className="text-destructive-foreground">P5 {moneyWhole(p5[activeStep])}</span>
      </div>

      <svg
        role="img"
        aria-label={`Monte Carlo bootstrap fan chart across ${mc.paths} paths, median terminal paper equity ${money(mc.terminal.p50)}`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
        onMouseMove={(e) => onMove(e.clientX, e.currentTarget.getBoundingClientRect())}
        onMouseLeave={() => setHoverStep(null)}
      >
        {[hi, (hi + lo) / 2, lo].map((tick, idx) => (
          <g key={idx}>
            <line x1={PADL} x2={W - PADR} y1={Y(tick)} y2={Y(tick)} style={{ stroke: "var(--hairline)" }} />
            <text
              x={PADL - 8}
              y={Y(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {moneyWhole(tick)}
            </text>
          </g>
        ))}

        {/* Starting pot reference */}
        <line
          x1={PADL}
          x2={W - PADR}
          y1={Y(mc.startEquity)}
          y2={Y(mc.startEquity)}
          strokeDasharray="4 4"
          style={{ stroke: "var(--ink-3)" }}
        />

        {/* P5-P95 outer band */}
        <path d={outerFan} style={{ fill: "var(--dot-blue)", opacity: 0.12 }} />
        {/* P25-P75 interquartile band */}
        <path d={innerFan} style={{ fill: "var(--dot-blue)", opacity: 0.24 }} />
        {/* P50 median curve */}
        <motion.path
          key={`${mc.seed}-${medianD.length}`}
          d={medianD}
          fill="none"
          strokeWidth={2.25}
          strokeLinejoin="round"
          style={{ stroke: "var(--dot-blue)" }}
          initial={reduce ? false : { pathLength: 0 }}
          animate={{ pathLength: 1 }}
          {...drawOnce}
        />

        {hoverStep !== null && (
          <line
            x1={X(activeStep)}
            x2={X(activeStep)}
            y1={top}
            y2={bottom}
            strokeDasharray="2 2"
            style={{ stroke: "var(--ink-3)" }}
          />
        )}
        <circle
          cx={X(activeStep)}
          cy={Y(p50[activeStep])}
          r={4}
          style={{ fill: "var(--dot-blue)", stroke: "var(--surface)", strokeWidth: 2 }}
        />

        {/* Step axis */}
        <text x={PADL} y={H - 8} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          T0
        </text>
        <text x={(PADL + W - PADR) / 2} y={H - 8} textAnchor="middle" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          T{Math.floor((len - 1) / 2)}
        </text>
        <text x={W - PADR} y={H - 8} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          T{len - 1}
        </text>
      </svg>
    </div>
  );
}

export function ProbBar({ yesProb }: { yesProb: number | null }) {
  if (yesProb === null || !Number.isFinite(yesProb)) {
    return <span className="font-mono text-xs text-ink-3">-</span>;
  }
  const yesPct = Math.round(Math.min(100, Math.max(0, yesProb * 100)));
  const noPct = 100 - yesPct;
  return (
    <div className="flex min-w-32 flex-col gap-1">
      <div className="flex items-center justify-between gap-2 font-mono text-xs tabular-nums">
        <span className="font-medium text-success-foreground">YES {yesPct}c</span>
        <span className="text-ink-2">NO {noPct}c</span>
      </div>
      <div className="flex h-1.5 w-full overflow-hidden rounded-full bg-track" aria-hidden="true">
        <div className="h-full bg-dot-green" style={{ width: `${yesPct}%` }} />
        <div className="h-full bg-dot-red/55" style={{ width: `${noPct}%` }} />
      </div>
    </div>
  );
}

export function ProvenanceTag({ kind }: { kind: "venue" | "tape" | "paper" }) {
  if (kind === "venue") {
    return (
      <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-info-foreground">
        <PillStatus className="border-r-0 pr-0">
          <PillIndicator variant="info" />
          venue
        </PillStatus>
      </Pill>
    );
  }
  if (kind === "paper") {
    return (
      <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-warning-foreground">
        <PillStatus className="border-r-0 pr-0">
          <PillIndicator variant="warning" />
          paper
        </PillStatus>
      </Pill>
    );
  }
  return (
    <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-ink-2">
      tape
    </Pill>
  );
}
