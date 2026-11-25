// Institutional paper quant SVG charts: compounding equity + drawdown underwater subplot,
// interactive market price + spread history with crosshair tooltip, calibration reliability
// with 95% Wilson error bars, 1,000-path Monte Carlo percentile fan, and Tremor sparklines.
import { useId, useMemo, useState } from "react";
import { m, useReducedMotion } from "motion/react";
import { StatusPill } from "@/components/ui";
import {
  dayLabel,
  isNum,
  money,
  moneySigned,
  moneyWhole,
  timeUtcFullLabel,
  timeUtcLabel,
} from "@/lib/desk";
import { useContainerWidth } from "@/TapeCharts";
export { MarketHistoryChart, ProbBar, ProvenanceTag } from "@/TapeCharts";
import type { CalibrationBucket, MonteCarloData } from "@/lib/quant";
import type { RecalBucket, ScalePlanData } from "@/lib/scaling";
import type { CorrelationMatrixData, RegimeCell } from "@/lib/tournament";
import { drawOnce } from "@/motionProps";

export type Point = [number, number];

export function validPoints(points: unknown): Point[] {
  if (!Array.isArray(points)) return [];
  return points.filter((p): p is Point => Array.isArray(p) && isNum(p[0]) && isNum(p[1]));
}

export function TremorSpark({
  points,
  start,
  label,
  className = "h-7 w-24",
}: {
  points: Point[];
  start: number;
  label: string;
  className?: string;
}) {
  const gradId = useId().replace(/:/g, "");
  const pts = validPoints(points);
  const s0 = isNum(start) ? start : 0;
  if (pts.length < 2) {
    return <span className="font-mono text-xs text-ink-3">no trades</span>;
  }
  const vals = pts.map((p) => p[1]);
  const lo = Math.min(...vals);
  const hi = Math.max(...vals);
  const span = Math.max(1e-9, hi - lo);
  const W = 96;
  const H = 28;
  const pad = 2;
  const X = (i: number) => pad + (i / Math.max(1, pts.length - 1)) * (W - pad * 2);
  const Y = (v: number) => H - pad - ((v - lo) / span) * (H - pad * 2);
  const lineD = pts.map((p, i) => `${i ? "L" : "M"}${X(i).toFixed(1)},${Y(p[1]).toFixed(1)}`).join(" ");
  const areaD = `${lineD} L${X(pts.length - 1).toFixed(1)},${H - pad} L${X(0).toFixed(1)},${H - pad} Z`;
  const up = pts[pts.length - 1][1] >= s0;
  const stroke = up ? "var(--dot-blue)" : "var(--dot-red)";
  return (
    <div aria-label={label} className="inline-flex items-center">
      <svg viewBox={`0 0 ${W} ${H}`} className={className} aria-hidden="true">
        <defs>
          <linearGradient id={`sp-${gradId}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={stroke} stopOpacity="0.22" />
            <stop offset="100%" stopColor={stroke} stopOpacity="0.02" />
          </linearGradient>
        </defs>
        <path d={areaD} fill={`url(#sp-${gradId})`} />
        <path d={lineD} fill="none" stroke={stroke} strokeWidth={1.6} strokeLinejoin="round" strokeLinecap="round" />
      </svg>
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
          <span className="font-mono text-ink-3">Start {money(s0)} / Stage 1 {moneyWhole(target)}</span>
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
          {pts.length} paper checkpoints (Start {moneyWhole(s0)})
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
        <m.path
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
        <m.path
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
        <m.path
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
          <m.path
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
        <m.path
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

/** 180-Day Compounding Trajectory SVG Chart (P10/P50/P90 daily run-rate or equity + horizontal target line). */
export function CompoundingTrajectoryChart({
  plan,
  metric,
  height = 248,
}: {
  plan: ScalePlanData;
  metric: "daily" | "equity";
  height?: number;
}) {
  const [ref, W] = useContainerWidth(220, 560);
  const reduce = useReducedMotion();
  const [hoverDay, setHoverDay] = useState<number | null>(null);

  const proj = plan.compoundingProjection;
  const p10 = metric === "daily" ? proj.p10Daily : proj.p10Equity;
  const p50 = metric === "daily" ? proj.p50Daily : proj.p50Equity;
  const p90 = metric === "daily" ? proj.p90Daily : proj.p90Equity;
  const len = p50.length;
  const H = height;
  const top = 16;
  const bottom = H - 30;
  const padL = 68;

  if (len < 2) {
    return (
      <div ref={ref} className="rounded-lg border border-dashed border-hairline bg-board/40 px-4 py-8 text-xs text-ink-2">
        Run the scale planner with settled tape windows to project 180-day P10/P50/P90 trajectories.
      </div>
    );
  }

  const refLine = metric === "daily" ? plan.targetDailyDollars : plan.startingBankroll;
  const allVals = [...p10, ...p90, refLine];
  const lo = Math.min(...allVals);
  const hi = Math.max(...allVals);
  const span = Math.max(10, hi - lo);
  const y0 = lo - span * 0.08;
  const y1 = hi + span * 0.08;

  const X = (d: number) => padL + (d / Math.max(1, len - 1)) * (W - padL - PADR);
  const Y = (v: number) => bottom - ((v - y0) / Math.max(1e-9, y1 - y0)) * (bottom - top);

  const bandD =
    p90.map((v, i) => `${i ? "L" : "M"}${X(i).toFixed(1)},${Y(v).toFixed(1)}`).join(" ") +
    " " +
    [...p10]
      .reverse()
      .map((v, rev) => `L${X(len - 1 - rev).toFixed(1)},${Y(v).toFixed(1)}`)
      .join(" ") +
    " Z";
  const medianD = p50.map((v, i) => `${i ? "L" : "M"}${X(i).toFixed(1)},${Y(v).toFixed(1)}`).join(" ");

  const activeDay = hoverDay !== null && hoverDay >= 0 && hoverDay < len ? hoverDay : len - 1;
  const fmtVal = (v: number) => (metric === "daily" ? `${moneyWhole(v)}/d` : moneyWhole(v));

  const onMove = (clientX: number, rect: DOMRect) => {
    const rel = clientX - rect.left;
    const ratio = Math.min(1, Math.max(0, (rel - padL) / Math.max(1, W - padL - PADR)));
    setHoverDay(Math.round(ratio * (len - 1)));
  };

  return (
    <div ref={ref} className="min-w-0" data-scale-trajectory-chart>
      <div
        data-scale-trajectory-tooltip
        className="mb-2 flex flex-wrap items-center justify-between gap-x-3 gap-y-1 rounded-md border border-hairline bg-surface px-2.5 py-1.5 font-mono text-xs tabular-nums"
      >
        <span className="font-medium text-ink">Day {activeDay}/{len - 1}</span>
        <span className="text-success-foreground">P90 {fmtVal(p90[activeDay])}</span>
        <span className="font-semibold text-info-foreground">P50 {fmtVal(p50[activeDay])}</span>
        <span className="text-destructive-foreground">P10 {fmtVal(p10[activeDay])}</span>
        <span className="text-ink-3">
          {metric === "daily" ? `Target ${moneyWhole(plan.targetDailyDollars)}/d` : `Start ${moneyWhole(plan.startingBankroll)}`}
        </span>
      </div>

      <svg
        role="img"
        aria-label={`180-day paper compounding trajectory showing P10, P50, and P90 ${metric === "daily" ? "daily run-rate against target" : "paper equity"}`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
        onMouseMove={(e) => onMove(e.clientX, e.currentTarget.getBoundingClientRect())}
        onMouseLeave={() => setHoverDay(null)}
      >
        {[hi, (hi + lo) / 2, lo].map((tick, idx) => (
          <g key={idx}>
            <line x1={padL} x2={W - PADR} y1={Y(tick)} y2={Y(tick)} style={{ stroke: "var(--hairline)" }} />
            <text
              x={padL - 8}
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

        {/* Horizontal target ($500/day) or starting bankroll reference line */}
        <line
          data-target-ref-line
          x1={padL}
          x2={W - PADR}
          y1={Y(refLine)}
          y2={Y(refLine)}
          strokeDasharray="5 4"
          strokeWidth={1.75}
          style={{ stroke: metric === "daily" ? "var(--dot-green)" : "var(--ink-3)" }}
        />

        {/* P10-P90 confidence band */}
        <path d={bandD} style={{ fill: "var(--dot-blue)", opacity: 0.16 }} />

        {/* P50 median curve */}
        <m.path
          key={`${plan.seed}-${metric}-${medianD.length}`}
          d={medianD}
          fill="none"
          strokeWidth={2.25}
          strokeLinejoin="round"
          style={{ stroke: "var(--dot-blue)" }}
          initial={reduce ? false : { pathLength: 0 }}
          animate={{ pathLength: 1 }}
          {...drawOnce}
        />

        {hoverDay !== null && (
          <line
            x1={X(activeDay)}
            x2={X(activeDay)}
            y1={top}
            y2={bottom}
            strokeDasharray="2 2"
            style={{ stroke: "var(--ink-3)" }}
          />
        )}
        <circle
          cx={X(activeDay)}
          cy={Y(p50[activeDay])}
          r={4}
          style={{ fill: "var(--dot-blue)", stroke: "var(--surface)", strokeWidth: 2 }}
        />

        <text x={padL} y={H - 8} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          D0
        </text>
        <text x={(padL + W - PADR) / 2} y={H - 8} textAnchor="middle" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          D{Math.floor((len - 1) / 2)}
        </text>
        <text x={W - PADR} y={H - 8} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          D{len - 1}
        </text>
      </svg>
    </div>
  );
}

/** Before vs After PAV Isotonic Recalibration SVG Chart (Raw implied + 95% Wilson CI vs Calibrated OOS curve). */
export function IsotonicRecalibrationChart({
  buckets,
  height = 248,
}: {
  buckets: RecalBucket[];
  height?: number;
}) {
  const [ref, W] = useContainerWidth(220, 480);
  const reduce = useReducedMotion();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);

  const populated = buckets.filter((b) => b.n > 0);
  const H = height;
  const left = 52;
  const right = 18;
  const top = 16;
  const bottom = H - 30;

  const X = (p: number) => left + Math.min(1, Math.max(0, p)) * (W - left - right);
  const Y = (p: number) => bottom - Math.min(1, Math.max(0, p)) * (bottom - top);

  const active =
    hoverIdx !== null && hoverIdx >= 0 && hoverIdx < populated.length
      ? populated[hoverIdx]
      : populated[populated.length - 1] || null;

  const rawD = populated
    .map((b, i) => `${i ? "L" : "M"}${X(b.meanPredP).toFixed(1)},${Y(b.realisedRate).toFixed(1)}`)
    .join(" ");
  const calD = populated
    .map((b, i) => `${i ? "L" : "M"}${X(b.meanPredP).toFixed(1)},${Y(b.calibratedP).toFixed(1)}`)
    .join(" ");

  const xTicks = W < 300 ? [0, 0.5, 1] : [0, 0.25, 0.5, 0.75, 1];

  return (
    <div ref={ref} className="min-w-0" data-recalibration-chart>
      <div
        data-recalibration-tooltip
        className="mb-2 flex flex-wrap items-center justify-between gap-2 rounded-md border border-hairline bg-surface px-2.5 py-1.5 font-mono text-xs tabular-nums"
      >
        {active ? (
          <>
            <span className="font-medium text-ink">
              {Math.round(active.low * 100)}–{Math.round(active.high * 100)}% (n={active.n})
            </span>
            <span className="text-ink-2">Raw {(active.meanPredP * 100).toFixed(1)}%</span>
            <span className="text-success-foreground">Real {(active.realisedRate * 100).toFixed(1)}%</span>
            <span className="font-semibold text-info-foreground">PAV {(active.calibratedP * 100).toFixed(1)}%</span>
            <span className="text-ink-3">
              95% CI [{(active.wilsonLow * 100).toFixed(0)}%, {(active.wilsonHigh * 100).toFixed(0)}%]
            </span>
          </>
        ) : (
          <span className="text-ink-2">Need settled out-of-sample windows to plot Before vs After PAV recalibration.</span>
        )}
      </div>

      <svg
        role="img"
        aria-label={`Before vs after PAV isotonic probability recalibration across ${populated.length} out-of-sample buckets`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
      >
        {[0, 0.5, 1].map((tick) => (
          <g key={`ry-${tick}`}>
            <line x1={left} x2={W - right} y1={Y(tick)} y2={Y(tick)} style={{ stroke: "var(--hairline)" }} />
            <text x={left - 8} y={Y(tick) + 4} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-3)" }}>
              {Math.round(tick * 100)}%
            </text>
          </g>
        ))}

        {xTicks.map((tick) => (
          <g key={`rx-${tick}`}>
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

        {/* Perfect calibration diagonal */}
        <line x1={X(0)} y1={Y(0)} x2={X(1)} y2={Y(1)} strokeDasharray="5 4" strokeWidth={1.25} style={{ stroke: "var(--ink-3)" }} />

        {/* Before: Raw OOS realized curve (dashed amber/muted) */}
        {populated.length >= 2 && (
          <path d={rawD} fill="none" strokeDasharray="3 3" strokeWidth={1.75} style={{ stroke: "var(--dot-amber)" }} />
        )}

        {/* After: Monotone PAV recalibrated curve (solid cobalt) */}
        {populated.length >= 2 && (
          <m.path
            key={calD.length}
            d={calD}
            fill="none"
            strokeWidth={2.25}
            strokeLinejoin="round"
            style={{ stroke: "var(--dot-blue)" }}
            initial={reduce ? false : { pathLength: 0 }}
            animate={{ pathLength: 1 }}
            {...drawOnce}
          />
        )}

        {/* Interactive Wilson CI whiskers & Before/After markers */}
        {populated.map((b, idx) => {
          const cx = X(b.meanPredP);
          const cyReal = Y(b.realisedRate);
          const cyCal = Y(b.calibratedP);
          const yLo = Y(b.wilsonLow);
          const yHi = Y(b.wilsonHigh);
          const isSel = active === b;
          return (
            <g
              key={idx}
              data-recal-point
              className="cursor-pointer"
              onMouseEnter={() => setHoverIdx(idx)}
              onClick={() => setHoverIdx(idx)}
            >
              <circle cx={cx} cy={cyReal} r={14} fill="transparent" />
              <line x1={cx} x2={cx} y1={yHi} y2={yLo} strokeWidth={1.5} style={{ stroke: "var(--ink-3)" }} />
              <line x1={cx - 3.5} x2={cx + 3.5} y1={yHi} y2={yHi} strokeWidth={1.5} style={{ stroke: "var(--ink-3)" }} />
              <line x1={cx - 3.5} x2={cx + 3.5} y1={yLo} y2={yLo} strokeWidth={1.5} style={{ stroke: "var(--ink-3)" }} />
              <circle cx={cx} cy={cyReal} r={isSel ? 4.5 : 3.5} style={{ fill: "var(--dot-amber)", stroke: "var(--surface)", strokeWidth: 1.5 }} />
              <circle cx={cx} cy={cyCal} r={isSel ? 5 : 4} style={{ fill: "var(--dot-blue)", stroke: "var(--surface)", strokeWidth: 1.75 }} />
            </g>
          );
        })}
      </svg>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 12x12 Strategy Return Correlation Heatmap SVG (Pearson r across OOS windows)
// ---------------------------------------------------------------------------

function shortStrategyTag(name: string, maxLen = 13): string {
  if (name.length <= maxLen) return name;
  return name.slice(0, maxLen - 1) + "…";
}

function shortColumnTag(name: string, idx: number): string {
  const parts = name.split(/[-_]/).filter(Boolean);
  if (parts.length >= 2) {
    return `${parts[0].slice(0, 2)}${parts[1].slice(0, 1)}`.toUpperCase();
  }
  return `S${idx + 1}`;
}

export function CorrelationMatrixChart({
  data,
  lineupSet,
}: {
  data: CorrelationMatrixData;
  lineupSet?: Set<string>;
}) {
  const [hover, setHover] = useState<[number, number] | null>(null);
  const strategies = data.strategies.slice(0, 12);
  const n = strategies.length;

  if (n === 0) {
    return (
      <div className="rounded-lg border border-hairline bg-board p-4 text-xs text-ink-2">
        Need strategies with settled out-of-sample windows to compute pairwise return correlations.
      </div>
    );
  }

  const left = 120;
  const top = 34;
  const cellW = 38;
  const cellH = 28;
  const right = 10;
  const bottom = 10;
  const W = left + n * cellW + right;
  const H = top + n * cellH + bottom;

  const active = hover ?? (n > 1 ? [0, 1] : [0, 0]);
  const [ai, aj] = active;
  const activeR = data.matrix[ai]?.[aj] ?? null;
  const activeN = data.sharedWindows[ai]?.[aj] ?? 0;
  const exceedsCap = ai !== aj && activeR !== null && Math.abs(activeR) > data.threshold;

  return (
    <div className="w-full space-y-2">
      {/* Live cell readout strip */}
      <div
        id="corr-hover-readout"
        className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-hairline bg-board px-3 py-1.5 font-mono text-xs tabular-nums"
      >
        <span className="truncate font-semibold text-ink">
          {strategies[ai]} × {strategies[aj]}
        </span>
        <div className="flex flex-wrap items-center gap-3">
          <span
            className={
              activeR === null
                ? "text-ink-3"
                : exceedsCap
                  ? "font-semibold text-warning-foreground"
                  : activeR >= 0
                    ? "text-info-foreground"
                    : "text-destructive-foreground"
            }
          >
            r = {activeR !== null ? `${activeR >= 0 ? "+" : ""}${activeR.toFixed(2)}` : "n/a"}
          </span>
          <span className="text-ink-2">n={activeN}</span>
          {ai !== aj && activeR !== null && (
            <span className={exceedsCap ? "text-warning-foreground" : "text-success-foreground"}>
              {exceedsCap ? `|r| > ${data.threshold.toFixed(2)}` : `|r| <= ${data.threshold.toFixed(2)}`}
            </span>
          )}
        </div>
      </div>

      <div className="w-full overflow-x-auto pb-1">
        <svg
          id="correlation-matrix-svg"
          role="img"
          aria-label={`${n} by ${n} strategy return Pearson correlation matrix with ${data.threshold} decorrelation threshold`}
          viewBox={`0 0 ${W} ${H}`}
          width={W}
          height={H}
          style={{ width: W, height: H }}
          className="block select-none"
        >
          {/* Column headers */}
          {strategies.map((s, j) => {
            const cx = left + j * cellW + cellW / 2;
            const hi = aj === j;
            return (
              <text
                key={`ch-${s}`}
                x={cx}
                y={top - 10}
                textAnchor="middle"
                fontSize={12}
                className="font-mono"
                style={{ fill: hi ? "var(--ink)" : "var(--ink-3)", fontWeight: hi ? 700 : 500 }}
              >
                {shortColumnTag(s, j)}
              </text>
            );
          })}

          {/* Rows */}
          {strategies.map((rowName, i) => {
            const ry = top + i * cellH;
            const rowHi = ai === i;
            const inLineup = lineupSet?.has(rowName) ?? false;
            return (
              <g key={`row-${rowName}`}>
                <text
                  x={left - 8}
                  y={ry + cellH / 2 + 4}
                  textAnchor="end"
                  fontSize={12}
                  className="font-mono"
                  style={{
                    fill: rowHi ? "var(--ink)" : inLineup ? "var(--blue)" : "var(--ink-2)",
                    fontWeight: rowHi || inLineup ? 600 : 400,
                  }}
                >
                  {shortStrategyTag(rowName, 13)}
                </text>

                {strategies.map((colName, j) => {
                  const rx = left + j * cellW;
                  const r = data.matrix[i]?.[j] ?? null;
                  const isDiag = i === j;
                  const isActive = ai === i && aj === j;
                  const highCorr = !isDiag && r !== null && Math.abs(r) > data.threshold;
                  const mag = r !== null ? Math.min(1, Math.abs(r)) : 0;
                  const overlayFill =
                    r === null
                      ? "transparent"
                      : isDiag
                        ? "var(--track)"
                        : r >= 0
                          ? "var(--dot-blue)"
                          : "var(--dot-red)";
                  const overlayOpacity = r === null ? 0 : isDiag ? 0.55 : 0.08 + mag * 0.34;
                  const cellText = r === null ? "·" : isDiag ? "1.0" : r.toFixed(1).replace("-0.0", "0.0");

                  return (
                    <g
                      key={`cell-${rowName}-${colName}`}
                      data-corr-cell={`${i}-${j}`}
                      className="cursor-pointer"
                      onMouseEnter={() => setHover([i, j])}
                      onClick={() => setHover([i, j])}
                    >
                      <rect
                        x={rx + 1}
                        y={ry + 1}
                        width={cellW - 2}
                        height={cellH - 2}
                        rx={3}
                        style={{ fill: "var(--board)" }}
                      />
                      {overlayOpacity > 0 && (
                        <rect
                          x={rx + 1}
                          y={ry + 1}
                          width={cellW - 2}
                          height={cellH - 2}
                          rx={3}
                          style={{ fill: overlayFill, opacity: overlayOpacity }}
                        />
                      )}
                      <rect
                        x={rx + 1}
                        y={ry + 1}
                        width={cellW - 2}
                        height={cellH - 2}
                        rx={3}
                        fill="none"
                        style={{
                          stroke: isActive
                            ? "var(--ink)"
                            : highCorr
                              ? "var(--dot-amber)"
                              : "var(--hairline)",
                          strokeWidth: isActive ? 1.75 : highCorr ? 1.25 : 0.75,
                        }}
                      />
                      <text
                        x={rx + cellW / 2}
                        y={ry + cellH / 2 + 4}
                        textAnchor="middle"
                        fontSize={12}
                        className="font-mono tabular-nums"
                        style={{
                          fill: r === null ? "var(--ink-3)" : "var(--ink)",
                          fontWeight: isActive || highCorr ? 700 : 500,
                        }}
                      >
                        {cellText}
                      </text>
                    </g>
                  );
                })}
              </g>
            );
          })}
        </svg>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 5x4 Edge-by-Regime Heatmap SVG (Entry Price Bucket x Seconds-to-Close)
// ---------------------------------------------------------------------------

export function RegimeHeatmapChart({
  priceBuckets,
  timeBuckets,
  cells,
}: {
  priceBuckets: string[];
  timeBuckets: string[];
  cells: RegimeCell[];
}) {
  const cellMap = new Map<string, RegimeCell>();
  for (const c of cells) cellMap.set(`${c.priceBucket}|${c.timeBucket}`, c);

  const firstPopulated = cells.find((c) => c.n > 0) ?? cells[0] ?? null;
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const activeCell =
    (selectedKey ? cellMap.get(selectedKey) : null) ?? firstPopulated;

  const left = 74;
  const top = 32;
  const cellW = 114;
  const cellH = 54;
  const right = 10;
  const bottom = 10;
  const W = left + timeBuckets.length * cellW + right;
  const H = top + priceBuckets.length * cellH + bottom;

  const maxAbsExp = Math.max(
    4,
    ...cells.map((c) => (c.netExpectancyCents !== null ? Math.abs(c.netExpectancyCents) : 0)),
  );

  return (
    <div className="w-full space-y-2">
      {/* Hover / active regime cell readout */}
      <div
        id="regime-hover-readout"
        className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-hairline bg-board px-3 py-1.5 font-mono text-xs tabular-nums"
      >
        {activeCell && activeCell.n > 0 ? (
          <>
            <span className="font-semibold text-ink">
              {activeCell.priceBucket} {activeCell.timeBucket} (n={activeCell.n})
            </span>
            <div className="flex flex-wrap items-center gap-3">
              <span className="text-ink-2">
                Win {activeCell.winRatePct !== null ? `${activeCell.winRatePct.toFixed(1)}%` : "-"}
                {activeCell.wilsonLowPct !== null && activeCell.wilsonHighPct !== null
                  ? ` [${Math.round(activeCell.wilsonLowPct)}–${Math.round(activeCell.wilsonHighPct)}%]`
                  : ""}
              </span>
              <span className="text-ink-3">
                Fee {activeCell.avgFeeCents !== null ? `${activeCell.avgFeeCents.toFixed(1)}c` : "-"}
              </span>
              <span
                className={
                  activeCell.netExpectancyCents !== null && activeCell.netExpectancyCents > 0
                    ? "font-semibold text-success-foreground"
                    : "font-semibold text-destructive-foreground"
                }
              >
                {activeCell.netExpectancyCents !== null
                  ? `${activeCell.netExpectancyCents >= 0 ? "+" : ""}${activeCell.netExpectancyCents.toFixed(2)}c/ct`
                  : "-"}
              </span>
              {activeCell.lowSample && (
                <span className="text-warning-foreground">Low n (&lt;20)</span>
              )}
            </div>
          </>
        ) : (
          <span className="text-ink-2">Hover any populated entry-price × time-to-close cell to inspect edge &amp; Wilson CI.</span>
        )}
      </div>

      <div className="w-full overflow-x-auto pb-1">
        <svg
          id="regime-heatmap-svg"
          role="img"
          aria-label="Edge by regime heatmap of entry price bucket versus seconds to close"
          viewBox={`0 0 ${W} ${H}`}
          width={W}
          height={H}
          style={{ width: W, height: H }}
          className="block select-none"
        >
          {/* Time-to-close column headers */}
          {timeBuckets.map((tb, j) => {
            const cx = left + j * cellW + cellW / 2;
            return (
              <text
                key={`tb-${tb}`}
                x={cx}
                y={top - 10}
                textAnchor="middle"
                fontSize={12}
                className="font-mono"
                style={{ fill: "var(--ink-2)", fontWeight: 600 }}
              >
                {tb}
              </text>
            );
          })}

          {/* Price bucket rows */}
          {priceBuckets.map((pb, i) => {
            const ry = top + i * cellH;
            return (
              <g key={`pb-${pb}`}>
                <text
                  x={left - 8}
                  y={ry + cellH / 2 + 4}
                  textAnchor="end"
                  fontSize={12}
                  className="font-mono"
                  style={{ fill: "var(--ink-2)", fontWeight: 600 }}
                >
                  {pb}
                </text>

                {timeBuckets.map((tb, j) => {
                  const key = `${pb}|${tb}`;
                  const cell = cellMap.get(key) ?? {
                    priceBucket: pb,
                    timeBucket: tb,
                    n: 0,
                    wins: 0,
                    winRatePct: null,
                    avgFeeCents: null,
                    netExpectancyCents: null,
                    wilsonLowPct: null,
                    wilsonHighPct: null,
                    lowSample: true,
                  };
                  const rx = left + j * cellW;
                  const isActive =
                    activeCell?.priceBucket === pb && activeCell?.timeBucket === tb;
                  const exp = cell.netExpectancyCents;
                  const intensity = exp !== null && cell.n > 0 ? Math.min(1, Math.abs(exp) / maxAbsExp) : 0;
                  const fillTone =
                    cell.n === 0 || exp === null
                      ? "transparent"
                      : exp > 0
                        ? "var(--dot-green)"
                        : "var(--dot-red)";
                  const fillOpacity = cell.n === 0 || exp === null ? 0 : 0.1 + intensity * 0.28;

                  return (
                    <g
                      key={key}
                      data-regime-cell={key}
                      className="cursor-pointer"
                      onMouseEnter={() => setSelectedKey(key)}
                      onClick={() => setSelectedKey(key)}
                    >
                      <rect
                        x={rx + 2}
                        y={ry + 2}
                        width={cellW - 4}
                        height={cellH - 4}
                        rx={6}
                        style={{ fill: "var(--board)" }}
                      />
                      {fillOpacity > 0 && (
                        <rect
                          x={rx + 2}
                          y={ry + 2}
                          width={cellW - 4}
                          height={cellH - 4}
                          rx={6}
                          style={{ fill: fillTone, opacity: fillOpacity }}
                        />
                      )}
                      <rect
                        x={rx + 2}
                        y={ry + 2}
                        width={cellW - 4}
                        height={cellH - 4}
                        rx={6}
                        fill="none"
                        style={{
                          stroke: isActive ? "var(--ink)" : "var(--hairline)",
                          strokeWidth: isActive ? 1.75 : 1,
                        }}
                      />

                      {cell.n > 0 && cell.lowSample && (
                        <circle
                          cx={rx + cellW - 11}
                          cy={ry + 11}
                          r={3}
                          style={{ fill: "var(--dot-amber)" }}
                        />
                      )}

                      <text
                        x={rx + cellW / 2}
                        y={ry + 22}
                        textAnchor="middle"
                        fontSize={12}
                        className="font-mono tabular-nums"
                        style={{
                          fill: cell.n > 0 ? "var(--ink)" : "var(--ink-3)",
                          fontWeight: cell.n > 0 ? 700 : 400,
                        }}
                      >
                        {cell.n > 0 && exp !== null
                          ? `${exp >= 0 ? "+" : ""}${exp.toFixed(1)}c`
                          : "—"}
                      </text>
                      <text
                        x={rx + cellW / 2}
                        y={ry + 41}
                        textAnchor="middle"
                        fontSize={12}
                        className="font-mono tabular-nums"
                        style={{ fill: "var(--ink-2)" }}
                      >
                        {cell.n > 0
                          ? `n=${cell.n}${cell.winRatePct !== null ? ` (${Math.round(cell.winRatePct)}%)` : ""}`
                          : "no trades"}
                      </text>
                    </g>
                  );
                })}
              </g>
            );
          })}
        </svg>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Autonomous Edge Allocator vs Legacy 1/N Taker Comparison SVG Chart
// ---------------------------------------------------------------------------

export function AllocatorComparisonChart({
  points,
  startingBankroll,
  execLabel,
}: {
  points: Array<{ close_epoch: number; legacy_equity: number; allocator_equity: number }>;
  startingBankroll: number;
  execLabel: string;
}) {
  const [boxRef, cw] = useContainerWidth(180, 640);
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);

  const W = Math.max(180, Math.round(cw));
  const H = 220;
  const pad = { l: 56, r: 16, t: 16, b: 28 };
  const iw = W - pad.l - pad.r;
  const ih = H - pad.t - pad.b;

  const geom = useMemo(() => {
    if (points.length < 2) return null;
    let rawMin = startingBankroll;
    let rawMax = startingBankroll;
    for (const pt of points) {
      if (pt.legacy_equity < rawMin) rawMin = pt.legacy_equity;
      if (pt.allocator_equity < rawMin) rawMin = pt.allocator_equity;
      if (pt.legacy_equity > rawMax) rawMax = pt.legacy_equity;
      if (pt.allocator_equity > rawMax) rawMax = pt.allocator_equity;
    }
    const span = Math.max(2, rawMax - rawMin);
    const minY = rawMin - span * 0.08;
    const maxY = rawMax + span * 0.08;
    const ySpan = Math.max(1e-6, maxY - minY);
    const xAt = (idx: number) => pad.l + (idx / Math.max(1, points.length - 1)) * iw;
    const yAt = (v: number) => pad.t + ih - ((v - minY) / ySpan) * ih;
    const allocPath = points
      .map((pt, i) => `${i === 0 ? "M" : "L"}${xAt(i).toFixed(1)},${yAt(pt.allocator_equity).toFixed(1)}`)
      .join(" ");
    const legacyPath = points
      .map((pt, i) => `${i === 0 ? "M" : "L"}${xAt(i).toFixed(1)},${yAt(pt.legacy_equity).toFixed(1)}`)
      .join(" ");
    const yTicks = [minY, (minY + maxY) / 2, maxY];
    const baseY = yAt(startingBankroll);
    return { minY, ySpan, allocPath, legacyPath, yTicks, baseY };
  }, [points, startingBankroll, iw, ih]);

  if (!geom || points.length < 2) {
    return (
      <div ref={boxRef} className="w-full min-w-0">
        <div className="flex h-44 items-center justify-center rounded-md border border-dashed border-hairline bg-surface/60 px-4 text-center text-xs text-ink-2">
          Awaiting at least 2 OOS replay checkpoints to plot Auto-Allocator vs Legacy 1/N Taker equity curves.
        </div>
      </div>
    );
  }

  const xAt = (idx: number) => pad.l + (idx / Math.max(1, points.length - 1)) * iw;
  const yAt = (v: number) => pad.t + ih - ((v - geom.minY) / geom.ySpan) * ih;
  const { allocPath, legacyPath, yTicks, baseY } = geom;
  const ai = hoverIdx !== null && hoverIdx >= 0 && hoverIdx < points.length ? hoverIdx : points.length - 1;
  const activePt = points[ai];

  return (
    <div ref={boxRef} className="w-full space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2 font-mono text-xs text-ink-2">
        <span>
          {activePt.close_epoch > 0
            ? `${dayLabel(activePt.close_epoch)} ${timeUtcLabel(activePt.close_epoch)} UTC`
            : `Checkpoint ${ai + 1}/${points.length}`}
        </span>
        <div className="flex flex-wrap items-center gap-3">
          <span className="font-semibold text-info-foreground">
            {execLabel}: {money(activePt.allocator_equity)}
          </span>
          <span className="text-ink-2">Legacy 1/N: {money(activePt.legacy_equity)}</span>
        </div>
      </div>

      <svg
        role="img"
        aria-label={`OOS replay equity comparison between ${execLabel} Auto-Allocator and Legacy 1/N Taker`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full select-none"
        onMouseLeave={() => setHoverIdx(null)}
        onMouseMove={(e) => {
          const rect = e.currentTarget.getBoundingClientRect();
          const rx = ((e.clientX - rect.left) / Math.max(1, rect.width)) * W;
          const t = Math.min(1, Math.max(0, (rx - pad.l) / Math.max(1, iw)));
          setHoverIdx(Math.round(t * (points.length - 1)));
        }}
      >
        {yTicks.map((v, idx) => {
          const yy = yAt(v);
          return (
            <g key={idx}>
              <line
                x1={pad.l}
                y1={yy}
                x2={W - pad.r}
                y2={yy}
                style={{ stroke: "var(--hairline)", strokeDasharray: "3 3" }}
              />
              <text
                x={pad.l - 8}
                y={yy + 4}
                textAnchor="end"
                fontSize={12}
                className="font-mono tabular-nums"
                style={{ fill: "var(--ink-3)" }}
              >
                ${Math.round(v)}
              </text>
            </g>
          );
        })}

        {baseY >= pad.t && baseY <= H - pad.b && (
          <line
            x1={pad.l}
            y1={baseY}
            x2={W - pad.r}
            y2={baseY}
            style={{ stroke: "var(--ink-3)", strokeDasharray: "2 4", opacity: 0.6 }}
          />
        )}

        <path
          d={legacyPath}
          fill="none"
          strokeWidth={1.75}
          strokeDasharray="4 4"
          style={{ stroke: "var(--ink-3)" }}
        />
        <path
          d={allocPath}
          fill="none"
          strokeWidth={2.25}
          style={{ stroke: "var(--blue)" }}
        />

        <line
          x1={xAt(ai)}
          y1={pad.t}
          x2={xAt(ai)}
          y2={H - pad.b}
          style={{ stroke: "var(--hairline)" }}
        />
        <circle
          cx={xAt(ai)}
          cy={yAt(activePt.allocator_equity)}
          r={4}
          style={{ fill: "var(--blue)" }}
        />
        <circle
          cx={xAt(ai)}
          cy={yAt(activePt.legacy_equity)}
          r={3.5}
          style={{ fill: "var(--ink-3)" }}
        />

        <text
          x={pad.l}
          y={H - 8}
          textAnchor="start"
          fontSize={12}
          className="font-mono"
          style={{ fill: "var(--ink-3)" }}
        >
          {points[0].close_epoch > 0 ? dayLabel(points[0].close_epoch) : "OOS Start"}
        </text>
        <text
          x={W - pad.r}
          y={H - 8}
          textAnchor="end"
          fontSize={12}
          className="font-mono"
          style={{ fill: "var(--ink-3)" }}
        >
          {points[points.length - 1].close_epoch > 0
            ? dayLabel(points[points.length - 1].close_epoch)
            : "OOS End"}
        </text>
      </svg>
    </div>
  );
}
