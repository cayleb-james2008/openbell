// Paper equity curves, Tremor SparkAreaChart mini-curves, market snapshot history traces, and YES/NO probability bars.
import { useEffect, useRef, useState } from "react";
import { motion, useReducedMotion } from "motion/react";
import { SparkAreaChart } from "@/components/SparkChart/SparkChart";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import {
  type Market,
  cents,
  dayLabel,
  isNum,
  money,
  moneySigned,
  moneyWhole,
  timeUtcLabel,
} from "@/lib/desk";
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
          <linearGradient id="ob-hero-eq-fill" x1="0" y1="0" x2="0" y2="1">
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
        <path d={areaD} fill="url(#ob-hero-eq-fill)" />
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
  const [ref, W] = useContainerWidth(240, 420);
  const reduce = useReducedMotion();
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

  return (
    <div ref={ref} className="min-w-0">
      <svg role="img" aria-label={label} viewBox={`0 0 ${W} ${H}`} width={W} height={H} className="block h-auto max-w-full">
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
          style={{ stroke: up ? "var(--dot-blue)" : "var(--dot-red)" }}
          initial={reduce ? false : { pathLength: 0 }}
          animate={{ pathLength: 1 }}
          {...drawOnce}
        />
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

export function MarketHistoryChart({
  points,
  ticker,
  height = 164,
}: {
  points: Market[];
  ticker: string;
  height?: number;
}) {
  const [ref, W] = useContainerWidth(240, 420);
  const reduce = useReducedMotion();
  const series = points
    .filter((m) => isNum(m.fetched_at))
    .map((m) => {
      const price =
        isNum(m.yes_bid) && isNum(m.yes_ask) && (m.yes_bid > 0 || m.yes_ask < 1)
          ? (m.yes_bid + m.yes_ask) / 2
          : isNum(m.last_price)
            ? m.last_price
            : isNum(m.yes_ask)
              ? m.yes_ask
              : null;
      return price !== null ? ([m.fetched_at as number, price] as Point) : null;
    })
    .filter((p): p is Point => p !== null);

  const H = height;
  const top = PADT;
  const bottom = H - PADB;

  if (series.length === 0) {
    return (
      <div ref={ref} className="rounded-lg border border-hairline bg-board px-4 py-6 text-sm text-ink-2">
        No quoted YES prices recorded for this ticker yet.
      </div>
    );
  }

  const x0 = series[0][0];
  const x1 = series[series.length - 1][0];
  const spanX = Math.max(1, x1 - x0);
  const X = (t: number) =>
    series.length === 1 ? (PADL + W - PADR) / 2 : PADL + ((t - x0) / spanX) * (W - PADL - PADR);
  const Y = (v: number) => bottom - Math.min(1, Math.max(0, v)) * (bottom - top);
  const d = series.map((p, i) => `${i ? "L" : "M"}${X(p[0]).toFixed(1)},${Y(p[1]).toFixed(1)}`).join(" ");

  return (
    <div ref={ref} className="min-w-0">
      <svg
        role="img"
        aria-label={`YES implied probability trace for ${ticker}, ${series.length} snapshots`}
        viewBox={`0 0 ${W} ${H}`}
        width={W}
        height={H}
        className="block h-auto max-w-full"
      >
        {[1, 0.5, 0].map((tick) => (
          <g key={tick}>
            <line
              x1={PADL}
              x2={W - PADR}
              y1={Y(tick)}
              y2={Y(tick)}
              strokeDasharray={tick === 0.5 ? "3 3" : undefined}
              style={{ stroke: "var(--hairline)" }}
            />
            <text
              x={PADL - 8}
              y={Y(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {Math.round(tick * 100)}c
            </text>
          </g>
        ))}
        {series.length >= 2 && (
          <motion.path
            key={d.length}
            d={d}
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
        {series.map((p, i) => (
          <circle
            key={i}
            cx={X(p[0])}
            cy={Y(p[1])}
            r={series.length === 1 ? 4.5 : 2.5}
            style={{ fill: "var(--dot-blue)" }}
          />
        ))}
        <text x={PADL} y={H - 6} fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {timeUtcLabel(x0)}
        </text>
        <text x={W - PADR} y={H - 6} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
          {series.length === 1 ? "1 snapshot" : timeUtcLabel(x1)}
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
