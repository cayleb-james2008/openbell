// Lightweight initial-shell market tape charts & provenance tags (kept separate from heavy Quant Charts).
import { useEffect, useRef, useState } from "react";
import { m, useReducedMotion } from "motion/react";
import { StatusPill } from "@/components/ui";
import {
  type Market,
  cents,
  impliedYesProb,
  isNum,
  marketSpreadCents,
  timeUtcFullLabel,
  timeUtcLabel,
} from "@/lib/desk";
import { drawOnce } from "@/motionProps";

const PADL = 60;
const PADR = 14;

export function useContainerWidth(min = 180, fallback = 540) {
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

  const H = Math.max(176, height);
  const priceTop = 12;
  const priceBot = Math.round(H * 0.56);
  const spreadTop = priceBot + 24;
  const spreadBot = H - 22;

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
  const areaD =
    series.length >= 2
      ? `${priceD} L${X(series[series.length - 1].t, series.length - 1).toFixed(1)},${priceBot} L${X(series[0].t, 0).toFixed(1)},${priceBot} Z`
      : "";
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
      <div
        data-market-crosshair-tooltip
        className="mb-1.5 flex flex-wrap items-center justify-between gap-x-3 gap-y-1 rounded-md border border-hairline bg-board/60 px-2.5 py-1 font-mono text-xs tabular-nums"
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
        <defs>
          <linearGradient id="ob-prob-area-grad" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--dot-blue)" stopOpacity="0.22" />
            <stop offset="100%" stopColor="var(--dot-blue)" stopOpacity="0.01" />
          </linearGradient>
        </defs>

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
              y={tick === 1 ? Yp(tick) + 7 : tick === 0 ? Yp(tick) - 2 : Yp(tick) + 4}
              textAnchor="end"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-3)" }}
            >
              {Math.round(tick * 100)}c
            </text>
          </g>
        ))}

        {areaD && <path d={areaD} fill="url(#ob-prob-area-grad)" />}

        {corridorD ? (
          <path d={corridorD} style={{ fill: "var(--dot-blue)", opacity: 0.14 }} />
        ) : (
          <line
            x1={PADL}
            x2={W - PADR}
            y1={Yp(series[0].prob)}
            y2={Yp(series[0].prob)}
            strokeDasharray="2 3"
            style={{ stroke: "var(--dot-blue)", opacity: 0.45 }}
          />
        )}

        {series.length >= 2 && (
          <m.path
            key={priceD.length}
            d={priceD}
            fill="none"
            strokeWidth={1.75}
            strokeLinejoin="round"
            strokeLinecap="round"
            style={{ stroke: "var(--dot-blue)" }}
            initial={reduce ? false : { pathLength: 0 }}
            animate={{ pathLength: 1 }}
            {...drawOnce}
          />
        )}

        <line x1={PADL} x2={W - PADR} y1={spreadTop} y2={spreadTop} style={{ stroke: "var(--hairline)" }} />
        <line x1={PADL} x2={W - PADR} y1={spreadBot} y2={spreadBot} style={{ stroke: "var(--hairline)" }} />
        <text x={PADL - 8} y={spreadTop + 8} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-3)" }}>
          {maxSpread}c
        </text>
        <text x={PADL - 8} y={spreadBot} textAnchor="end" fontSize={12} className="font-mono tabular-nums" style={{ fill: "var(--ink-3)" }}>
          0c
        </text>
        {series.map((p, i) => {
          const cx = X(p.t, i);
          const yTop = Ys(p.spread);
          const hBar = Math.max(2, spreadBot - yTop);
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

        <circle
          cx={X(active.t, activeIdx)}
          cy={Yp(active.prob)}
          r={4}
          style={{ fill: "var(--dot-blue)", stroke: "var(--surface)", strokeWidth: 2 }}
        />

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

export function ProbBar({ yesProb }: { yesProb: number | null }) {
  if (yesProb === null || !Number.isFinite(yesProb)) {
    return <span className="font-mono text-xs text-ink-3">-</span>;
  }
  const yesPct = Math.round(Math.min(100, Math.max(0, yesProb * 100)));
  const noPct = 100 - yesPct;
  return (
    <div className="flex min-w-32 flex-col gap-1">
      <div className="flex items-center justify-between gap-2 text-xs tabular-nums">
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
      <StatusPill tone="info" className="min-h-6 px-2 py-0.5 text-info-foreground">
        venue
      </StatusPill>
    );
  }
  if (kind === "paper") {
    return (
      <StatusPill tone="warning" className="min-h-6 px-2 py-0.5 text-warning-foreground">
        paper
      </StatusPill>
    );
  }
  return (
    <StatusPill tone="neutral" className="min-h-6 px-2 py-0.5 text-ink-2">
      tape
    </StatusPill>
  );
}
