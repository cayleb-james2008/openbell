// Monotone PAV Isotonic Probability Recalibration panel (IS 60% fit -> OOS 40% eval)
// with Before vs After SVG curve, OOS Brier/LogLoss/ECE readouts, and EV-After-Fees Entry Gate impact table.
import { useEffect, useState } from "react";
import { SlidersHorizontal } from "lucide-react";
import {
  Badge,
  Disclosure,
  StatusPill,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { IsotonicRecalibrationChart, ProvenanceTag } from "@/Charts";
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  moneySigned,
} from "@/lib/desk";
import { type RecalibrationData, fetchRecalibration } from "@/lib/scaling";

function fmtDec(v: number | null, digits = 4): string {
  return v === null || !Number.isFinite(v) ? "-" : v.toFixed(digits);
}

function fmtPct(v: number | null, digits = 2): string {
  return v === null || !Number.isFinite(v) ? "-" : `${(v * 100).toFixed(digits)}%`;
}

export function RecalibrationPanel({
  dataset,
  home,
  markets,
  slots,
  panelRef,
}: {
  dataset: Dataset;
  home: Home | null;
  markets: Market[];
  slots: Slots | null;
  panelRef?: React.RefObject<HTMLDivElement | null>;
}) {
  const [recal, setRecal] = useState<RecalibrationData | null>(null);

  useEffect(() => {
    let alive = true;
    fetchRecalibration(dataset, home, markets, slots).then((res) => {
      if (alive) setRecal(res);
    });
    return () => {
      alive = false;
    };
  }, [dataset, home, markets, slots]);

  const brierDelta =
    recal?.beforeOos.brierScore !== null &&
    recal?.beforeOos.brierScore !== undefined &&
    recal?.afterOos.brierScore !== null &&
    recal?.afterOos.brierScore !== undefined
      ? recal.afterOos.brierScore - recal.beforeOos.brierScore
      : null;

  return (
    <div
      ref={panelRef}
      id="quant-recalibration"
      data-recalibration-panel
      className="scroll-mt-16 rounded-[10px] border border-hairline bg-surface p-4 shadow-(--elev-card)"
    >
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-2.5 border-b border-hairline pb-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <SlidersHorizontal className="size-4 text-info-foreground" aria-hidden />
            <h3 className="text-sm font-semibold text-ink">Isotonic Recalibration (PAV) &amp; EV Entry Gate</h3>
            <ProvenanceTag kind="tape" />
          </div>
          <p className="mt-0.5 text-xs text-ink-2">
            {recal
              ? recal.note
              : "Monotone PAV fitted strictly on chronological IS (60%) and evaluated on OOS (40%)."}
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {recal && (
            <StatusPill
              id="recal-oos-status"
              tone={recal.oosImproved ? "success" : "warning"}
              className={
                "min-h-6 px-2.5 py-0.5 text-xs " +
                (recal.oosImproved ? "text-success-foreground" : "text-warning-foreground")
              }
            >
              {recal.oosImproved ? "OOS Improved" : "No OOS Gain"}
            </StatusPill>
          )}
          <span className="mono text-xs text-ink-3">
            {recal?.source === "server" ? "Server PAV Engine" : "Client PAV fallback"}
          </span>
        </div>
      </div>

      {/* Before vs After OOS Diagnostics Strip */}
      <dl className="mt-4 grid grid-cols-2 divide-y divide-hairline border-y border-hairline sm:grid-cols-4 sm:divide-x sm:divide-y-0">
        <div className="p-2.5">
          <dt className="text-xs text-ink-3">OOS Brier (Before to After)</dt>
          <dd id="recal-brier" className="mono mt-0.5 text-xs font-semibold text-ink sm:text-sm">
            {recal ? `${fmtDec(recal.beforeOos.brierScore)} to ${fmtDec(recal.afterOos.brierScore)}` : "-"}
          </dd>
          {brierDelta !== null && (
            <span className={"mono text-xs " + (brierDelta < 0 ? "text-success-foreground" : "text-warning-foreground")}>
              {brierDelta >= 0 ? "+" : ""}
              {brierDelta.toFixed(4)} delta
            </span>
          )}
        </div>

        <div className="p-2.5">
          <dt className="text-xs text-ink-3">OOS Log Loss</dt>
          <dd className="mono mt-0.5 text-xs font-semibold text-ink sm:text-sm">
            {recal ? `${fmtDec(recal.beforeOos.logLoss, 3)} to ${fmtDec(recal.afterOos.logLoss, 3)}` : "-"}
          </dd>
          <span className="mono text-xs text-ink-3">Lower is sharper</span>
        </div>

        <div className="p-2.5">
          <dt className="text-xs text-ink-3">OOS ECE</dt>
          <dd className="mono mt-0.5 text-xs font-semibold text-ink sm:text-sm">
            {recal ? `${fmtPct(recal.beforeOos.ece)} to ${fmtPct(recal.afterOos.ece)}` : "-"}
          </dd>
          <span className="mono text-xs text-ink-3">Gate target &lt; 5.0%</span>
        </div>

        <div className="p-2.5">
          <dt className="text-xs text-ink-3">Min Detectable Edge</dt>
          <dd className="mono mt-0.5 text-xs font-semibold text-ink sm:text-sm">
            {recal?.minDetectableEdge !== null && recal?.minDetectableEdge !== undefined
              ? `${(recal.minDetectableEdge * 100).toFixed(1)}pp`
              : "-"}
          </dd>
          <span className="mono text-xs text-ink-3">
            IS {recal?.isCount ?? 0} / OOS {recal?.oosCount ?? 0}
          </span>
        </div>
      </dl>

      {/* Main 2-column grid: Before vs After Curve left, EV-After-Fees Gate Impact right */}
      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-12">
        <div className="pt-1 lg:col-span-6">
          <div className="mb-2 flex flex-wrap items-center justify-between gap-2 text-xs">
            <span className="font-semibold text-ink">Before vs After Recalibration Curve (OOS)</span>
            <span className="mono text-ink-3">Amber = raw, Blue = PAV</span>
          </div>
          {recal ? (
            <IsotonicRecalibrationChart buckets={recal.buckets} />
          ) : (
            <div className="h-56 animate-pulse rounded-md bg-track" />
          )}
        </div>

        <div id="ev-gate-impact" className="flex flex-col justify-between gap-3 border-t border-hairline pt-3 lg:col-span-6 lg:border-t-0 lg:border-l lg:pl-4 lg:pt-0">
          <div>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h4 className="text-sm font-semibold text-ink">EV-After-Fees Entry Gate Impact</h4>
              <Badge variant="outline" className="mono text-xs">
                {recal?.evGate.ruleFormula || "p_cal > entry + fee + slippage"}
              </Badge>
            </div>
            <p className="mt-1 text-xs text-ink-2">
              Retrospective out-of-sample filter accepting only positive net-EV paper quotes.
            </p>

            {recal && (
              <div className="mt-3 border-t border-hairline">
                <Table className="stack">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Gate Cohort</TableHead>
                      <TableHead className="text-right">Windows</TableHead>
                      <TableHead className="text-right">Win Rate</TableHead>
                      <TableHead className="text-right">Paper P&amp;L</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    <TableRow>
                      <TableCell className="m-grow font-medium text-ink">All OOS Quotes (Before Gate)</TableCell>
                      <TableCell className="mono text-right text-xs text-ink-2">{recal.evGate.totalCount}</TableCell>
                      <TableCell className="mono text-right text-xs text-ink-2">-</TableCell>
                      <TableCell className="mono text-right text-xs font-semibold text-ink">
                        {moneySigned(recal.evGate.beforePnlDollars)}
                      </TableCell>
                    </TableRow>
                    <TableRow className="bg-board/40">
                      <TableCell className="m-grow font-medium text-success-foreground">Accepted by EV Gate</TableCell>
                      <TableCell className="mono text-right text-xs text-ink">{recal.evGate.acceptedCount}</TableCell>
                      <TableCell className="mono text-right text-xs text-success-foreground">
                        {recal.evGate.acceptedWinRatePct !== null ? `${recal.evGate.acceptedWinRatePct.toFixed(1)}%` : "-"}
                      </TableCell>
                      <TableCell className="mono text-right text-xs font-semibold text-success-foreground">
                        {moneySigned(recal.evGate.afterPnlDollars)}
                      </TableCell>
                    </TableRow>
                    <TableRow>
                      <TableCell className="m-grow font-medium text-ink-2">Refused by EV Gate</TableCell>
                      <TableCell className="mono text-right text-xs text-ink-2">{recal.evGate.refusedCount}</TableCell>
                      <TableCell className="mono text-right text-xs text-ink-2">
                        {recal.evGate.refusedWinRatePct !== null ? `${recal.evGate.refusedWinRatePct.toFixed(1)}%` : "-"}
                      </TableCell>
                      <TableCell className="mono text-right text-xs font-medium text-ink-2">
                        Delta {moneySigned(recal.evGate.pnlDeltaDollars)}
                      </TableCell>
                    </TableRow>
                  </TableBody>
                </Table>
              </div>
            )}
          </div>

          {/* Expandable 5-point probability bucket table */}
          {recal && recal.buckets.length > 0 && (
            <div className="border-t border-hairline pt-2.5">
              <Disclosure label={`Show 5-point probability buckets (${recal.buckets.length})`}>
                <div className="max-h-56 overflow-auto border-t border-hairline">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Bucket</TableHead>
                        <TableHead className="text-right">n / Wins</TableHead>
                        <TableHead className="text-right">Raw Pred</TableHead>
                        <TableHead className="text-right">Realised</TableHead>
                        <TableHead className="text-right">95% CI</TableHead>
                        <TableHead className="text-right">PAV Cal</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {recal.buckets.map((b, i) => (
                        <TableRow key={i}>
                          <TableCell className="mono text-xs text-ink">
                            {Math.round(b.low * 100)}–{Math.round(b.high * 100)}%
                          </TableCell>
                          <TableCell className="mono text-right text-xs text-ink-2">
                            {b.n} / {b.wins}
                          </TableCell>
                          <TableCell className="mono text-right text-xs text-ink-2">
                            {(b.meanPredP * 100).toFixed(1)}%
                          </TableCell>
                          <TableCell className="mono text-right text-xs text-ink">
                            {(b.realisedRate * 100).toFixed(1)}%
                          </TableCell>
                          <TableCell className="mono text-right text-xs text-ink-3">
                            [{(b.wilsonLow * 100).toFixed(0)}%, {(b.wilsonHigh * 100).toFixed(0)}%]
                          </TableCell>
                          <TableCell className="mono text-right text-xs font-semibold text-info-foreground">
                            {(b.calibratedP * 100).toFixed(1)}%
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              </Disclosure>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
