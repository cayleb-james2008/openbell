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
import {
  type SpotHistoryTick,
  formatSpotUsd,
  formatSpotUsdCompact,
} from "@/lib/spot";
import { drawOnce } from "@/motionProps";

const PADL = 52;
const PADR = 14;
const PADR_SPOT = 64;

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

/** Interactive SVG Price + Spread History Chart with Strike threshold reference, Spot price overlay toggle & hover crosshair tooltip. */
export function MarketHistoryChart({
  points,
  ticker,
  height = 224,
  spotHistory,
  liveSpotPrice,
  floorStrike,
  capStrike,
}: {
  points: Market[];
  ticker: string;
  height?: number;
  spotHistory?: SpotHistoryTick[];
  liveSpotPrice?: number | null;
  floorStrike?: number | null;
  capStrike?: number | null;
}) {
  const [ref, W] = useContainerWidth(220, 440);
  const reduce = useReducedMotion();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  const [showSpotOverlay, setShowSpotOverlay] = useState(true);

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

  const effectiveFloorStrike = (() => {
    if (isNum(floorStrike) && floorStrike > 0) return floorStrike;
    for (let i = series.length - 1; i >= 0; i--) {
      const fs = series[i].raw.floor_strike;
      if (isNum(fs) && fs > 0) return fs;
    }
    return null;
  })();

  const effectiveCapStrike = (() => {
    if (isNum(capStrike) && capStrike > 0) return capStrike;
    for (let i = series.length - 1; i >= 0; i--) {
      const cs = series[i].raw.cap_strike;
      if (isNum(cs) && cs > 0) return cs;
    }
    return null;
  })();

  const x0 = series[0].t;
  const x1 = series[series.length - 1].t;
  const spanX = Math.max(1, x1 - x0);

  const validSpotHistory = Array.isArray(spotHistory)
    ? spotHistory.filter((tk) => isNum(tk.ts_ms) && tk.ts_ms > 0 && isNum(tk.price) && tk.price > 0)
    : [];

  const snapshotSpotPoints = series
    .map((p, idx) => {
      const sp = p.raw.spot_price;
      return isNum(sp) && sp > 0 ? { t: p.t, idx, price: sp } : null;
    })
    .filter((p): p is NonNullable<typeof p> => p !== null);

  const latestSpotPrice =
    isNum(liveSpotPrice) && liveSpotPrice > 0
      ? liveSpotPrice
      : validSpotHistory.length > 0
        ? validSpotHistory[validSpotHistory.length - 1].price
        : snapshotSpotPoints.length > 0
          ? snapshotSpotPoints[snapshotSpotPoints.length - 1].price
          : null;

  const spotValues: number[] = [];
  for (const tk of validSpotHistory) spotValues.push(tk.price);
  for (const sp of snapshotSpotPoints) spotValues.push(sp.price);
  if (effectiveFloorStrike !== null) spotValues.push(effectiveFloorStrike);
  if (effectiveCapStrike !== null) spotValues.push(effectiveCapStrike);
  if (latestSpotPrice !== null) spotValues.push(latestSpotPrice);

  const hasSpotOrStrike = spotValues.length > 0;
  const rightPad = showSpotOverlay && hasSpotOrStrike ? PADR_SPOT : PADR;
  const plotW = Math.max(80, W - PADL - rightPad);

  const H = Math.max(176, height);
  const priceTop = 12;
  const priceBot = Math.round(H * 0.56);
  const spreadTop = priceBot + 24;
  const spreadBot = H - 22;

  const X = (t: number, idx: number) =>
    series.length === 1
      ? PADL + plotW / 2
      : spanX <= 0
        ? PADL + (idx / Math.max(1, series.length - 1)) * plotW
        : PADL + ((t - x0) / spanX) * plotW;
  const Yp = (v: number) => priceBot - Math.min(1, Math.max(0, v)) * (priceBot - priceTop);

  const rawSpotMin = spotValues.length > 0 ? Math.min(...spotValues) : 0;
  const rawSpotMax = spotValues.length > 0 ? Math.max(...spotValues) : 1;
  const spotMid = (rawSpotMin + rawSpotMax) / 2;
  const minSpotSpan = Math.max(0.0005, spotMid * 0.0015);
  const spotSpan = Math.max(minSpotSpan, rawSpotMax - rawSpotMin);
  const spotPad = spotSpan * 0.18;
  const spotDomainMin = rawSpotMin - spotPad;
  const spotDomainMax = rawSpotMax + spotPad;
  const Yspot = (v: number) => {
    const norm = (v - spotDomainMin) / Math.max(1e-9, spotDomainMax - spotDomainMin);
    const clamped = Math.min(1, Math.max(0, norm));
    return priceBot - clamped * (priceBot - priceTop);
  };

  const spotChartPoints: { x: number; y: number; price: number }[] = (() => {
    if (!showSpotOverlay) return [];
    if (validSpotHistory.length > 0) {
      const inRange =
        series.length > 1 && spanX > 1
          ? validSpotHistory.filter((tk) => tk.ts_ms / 1000 >= x0 - 900 && tk.ts_ms / 1000 <= x1 + 900)
          : [];
      if (inRange.length >= 2 && series.length > 1 && spanX > 1) {
        return inRange.map((tk) => {
          const tsSec = tk.ts_ms / 1000;
          const frac = Math.min(1, Math.max(0, (tsSec - x0) / spanX));
          return { x: PADL + frac * plotW, y: Yspot(tk.price), price: tk.price };
        });
      }
      if (validSpotHistory.length >= 2) {
        return validSpotHistory.map((tk, idx) => ({
          x: PADL + (idx / (validSpotHistory.length - 1)) * plotW,
          y: Yspot(tk.price),
          price: tk.price,
        }));
      }
      return [
        {
          x: PADL + plotW / 2,
          y: Yspot(validSpotHistory[0].price),
          price: validSpotHistory[0].price,
        },
      ];
    }
    if (snapshotSpotPoints.length > 0) {
      return snapshotSpotPoints.map((sp) => ({
        x: X(sp.t, sp.idx),
        y: Yspot(sp.price),
        price: sp.price,
      }));
    }
    if (latestSpotPrice !== null) {
      return [{ x: W - rightPad, y: Yspot(latestSpotPrice), price: latestSpotPrice }];
    }
    return [];
  })();

  const spotPathD =
    spotChartPoints.length >= 2
      ? spotChartPoints.map((pt, i) => `${i ? "L" : "M"}${pt.x.toFixed(1)},${pt.y.toFixed(1)}`).join(" ")
      : "";

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
  const barW = Math.max(3, Math.min(14, plotW / Math.max(8, series.length * 1.6)));

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

  const activeSpotDisplay =
    isNum(active.raw.spot_price) && active.raw.spot_price > 0 ? active.raw.spot_price : latestSpotPrice;
  const strikeY = effectiveFloorStrike !== null ? Yspot(effectiveFloorStrike) : null;
  const showStrikeAxisLabel =
    strikeY !== null &&
    Math.abs(strikeY - (priceTop + 8)) >= 20 &&
    Math.abs(strikeY - (priceBot - 2)) >= 20;

  return (
    <div ref={ref} className="min-w-0" data-market-price-spread-chart>
      <div className="mb-1.5 flex min-w-0 flex-wrap items-center justify-between gap-2 text-xs">
        <div className="flex min-w-0 flex-wrap items-center gap-2">
          <button
            type="button"
            data-spot-overlay-toggle
            aria-pressed={showSpotOverlay}
            onClick={() => setShowSpotOverlay((v) => !v)}
            className={
              "min-h-11 cursor-pointer rounded-md border px-2.5 text-xs font-medium outline-none transition-[background-color,border-color,color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
              (showSpotOverlay
                ? "border-blue/60 bg-blue-soft text-info-foreground"
                : "border-hairline bg-board text-ink-2 [@media(hover:hover)_and_(pointer:fine)]:hover:text-ink")
            }
          >
            {showSpotOverlay ? "Spot & Strike: On" : "Spot & Strike: Off"}
          </button>
          <span data-chart-strike-label className="mono text-ink-2">
            Strike <strong className="font-semibold text-ink">{formatSpotUsd(effectiveFloorStrike)}</strong>
          </span>
          <span data-chart-spot-label className="mono text-ink-2">
            Spot <strong className="font-semibold text-ink">{formatSpotUsd(latestSpotPrice)}</strong>
          </span>
        </div>
        {showSpotOverlay && !hasSpotOrStrike && (
          <span data-chart-spot-offline className="text-xs text-ink-2">
            Spot &amp; strike N/A
          </span>
        )}
      </div>

      <div
        data-market-crosshair-tooltip
        className="mb-1.5 flex flex-wrap items-center justify-between gap-x-3 gap-y-1 rounded-md border border-hairline bg-board/60 px-2.5 py-1 text-xs tabular-nums"
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
        {showSpotOverlay && (
          <span className="mono text-ink-2">
            Spot {formatSpotUsd(activeSpotDisplay)}
          </span>
        )}
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
              x2={W - rightPad}
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
            x2={W - rightPad}
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

        {showSpotOverlay && hasSpotOrStrike && (
          <g data-spot-overlay-group>
            {strikeY !== null && (
              <line
                data-strike-line
                x1={PADL}
                x2={W - rightPad}
                y1={strikeY}
                y2={strikeY}
                strokeWidth={1.5}
                strokeDasharray="4 3"
                style={{ stroke: "var(--dot-amber)" }}
              />
            )}

            {spotPathD ? (
              <path
                data-spot-overlay-path
                d={spotPathD}
                fill="none"
                strokeWidth={1.75}
                strokeLinejoin="round"
                strokeLinecap="round"
                style={{ stroke: "var(--dot-green)" }}
              />
            ) : spotChartPoints.length === 1 ? (
              <>
                <line
                  data-spot-overlay-path
                  x1={PADL}
                  x2={W - rightPad}
                  y1={spotChartPoints[0].y}
                  y2={spotChartPoints[0].y}
                  strokeWidth={1.25}
                  strokeDasharray="2 2"
                  style={{ stroke: "var(--dot-green)", opacity: 0.7 }}
                />
                <circle
                  cx={spotChartPoints[0].x}
                  cy={spotChartPoints[0].y}
                  r={3.5}
                  style={{ fill: "var(--dot-green)", stroke: "var(--surface)", strokeWidth: 1.5 }}
                />
              </>
            ) : null}

            <text
              x={W - rightPad + 6}
              y={priceTop + 8}
              textAnchor="start"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-2)" }}
            >
              {formatSpotUsdCompact(spotDomainMax)}
            </text>
            {showStrikeAxisLabel && (
              <text
                x={W - rightPad + 6}
                y={strikeY + 4}
                textAnchor="start"
                fontSize={12}
                className="font-mono"
                style={{ fill: "var(--warning-foreground)" }}
              >
                Strike
              </text>
            )}
            <text
              x={W - rightPad + 6}
              y={priceBot - 2}
              textAnchor="start"
              fontSize={12}
              className="font-mono tabular-nums"
              style={{ fill: "var(--ink-2)" }}
            >
              {formatSpotUsdCompact(spotDomainMin)}
            </text>
          </g>
        )}

        <line x1={PADL} x2={W - rightPad} y1={spreadTop} y2={spreadTop} style={{ stroke: "var(--hairline)" }} />
        <line x1={PADL} x2={W - rightPad} y1={spreadBot} y2={spreadBot} style={{ stroke: "var(--hairline)" }} />
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
        <text x={W - rightPad} y={H - 5} textAnchor="end" fontSize={12} className="font-mono" style={{ fill: "var(--ink-3)" }}>
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
