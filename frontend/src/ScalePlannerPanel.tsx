// $500/Day Paper Capital Scaling Planner, Liquidity Capacity Ceiling, 180-Day Compounding Trajectory,
// and 5-Gate Mechanical Readiness Scorecard. Strictly paper-only.
import { useCallback, useEffect, useState } from "react";
import { CheckCircle2, Gauge, RefreshCw, ShieldAlert, TrendingUp, XCircle } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { CompoundingTrajectoryChart, ProvenanceTag } from "@/Charts";
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  money,
  moneySigned,
  moneyWhole,
} from "@/lib/desk";
import {
  type ScalePlanData,
  type ScalePlanRequest,
  type SizingMode,
  runScalePlan,
} from "@/lib/scaling";

const SIZING_LABELS: Record<SizingMode, string> = {
  equal_slots: "Equal Slots",
  quarter_kelly: "Quarter-Kelly",
  half_kelly: "Half-Kelly",
};

export function ScalePlannerPanel({
  dataset,
  home,
  markets,
  slots,
  panelRef,
  scorecardRef,
}: {
  dataset: Dataset;
  home: Home | null;
  markets: Market[];
  slots: Slots | null;
  panelRef?: React.RefObject<HTMLDivElement | null>;
  scorecardRef?: React.RefObject<HTMLDivElement | null>;
}) {
  const [targetDaily, setTargetDaily] = useState("500");
  const [bankroll, setBankroll] = useState("1000");
  const [slippageCents, setSlippageCents] = useState("1");
  const [maxContracts, setMaxContracts] = useState("250");
  const [seriesCount, setSeriesCount] = useState("4");
  const [sizingMode, setSizingMode] = useState<SizingMode>("equal_slots");
  const [assumedEdge, setAssumedEdge] = useState("");
  const [seed, setSeed] = useState(42);
  const [chartMetric, setChartMetric] = useState<"daily" | "equity">("daily");
  const [running, setRunning] = useState(false);
  const [plan, setPlan] = useState<ScalePlanData | null>(null);

  const buildRequest = useCallback(
    (nextSeed: number): ScalePlanRequest => {
      const parsedEdge = assumedEdge.trim() === "" ? null : Number(assumedEdge);
      return {
        target_daily_dollars: Math.max(10, Number(targetDaily) || 500),
        starting_bankroll: Math.max(50, Number(bankroll) || 1000),
        max_slot_risk_pct: 25,
        slippage_cents: Math.min(5, Math.max(0, Math.round(Number(slippageCents) || 0))),
        max_contracts_per_window: Math.max(1, Math.round(Number(maxContracts) || 250)),
        active_series_count: Math.min(8, Math.max(1, Math.round(Number(seriesCount) || 4))),
        sizing_mode: sizingMode,
        paths: 1000,
        seed: nextSeed,
        assumed_edge_cents:
          parsedEdge !== null && Number.isFinite(parsedEdge) && parsedEdge > 0 ? parsedEdge : null,
        dataset,
      };
    },
    [targetDaily, bankroll, slippageCents, maxContracts, seriesCount, sizingMode, assumedEdge, dataset],
  );

  const runPlan = useCallback(
    async (nextSeed: number, notify = false) => {
      setRunning(true);
      try {
        const req = buildRequest(nextSeed);
        const res = await runScalePlan(req, dataset, home, markets, slots);
        setPlan(res);
        setSeed(res.seed);
        if (notify) {
          toast.success(
            `Scale plan updated (${res.modeLabel} · ${SIZING_LABELS[res.sizingMode]} · ${res.elapsedMs}ms).`,
          );
        }
      } finally {
        setRunning(false);
      }
    },
    [buildRequest, dataset, home, markets, slots],
  );

  useEffect(() => {
    void runPlan(seed, false);
  }, [dataset, home, markets, slots]);

  const fillPct = plan ? Math.round(plan.liquidityCeiling.fillRatioAtTarget * 100) : 0;
  const gatesPassed = plan ? plan.readinessGates.gates.filter((g) => g.passed).length : 0;
  const gatesTotal = plan ? plan.readinessGates.gates.length : 5;
  const isReadyForReview = plan?.readinessGates.verdict === "READY_FOR_OWNER_REVIEW";

  return (
    <div
      ref={panelRef}
      id="quant-scale-planner"
      data-scale-planner
      className="rounded-[10px] border border-hairline bg-surface p-4 shadow-(--elev-card)"
    >
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-hairline pb-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <TrendingUp className="size-4 text-info-foreground" aria-hidden />
            <h3 className="text-sm font-semibold text-ink">$500/Day Paper Capital Scale Planner</h3>
            <ProvenanceTag kind="paper" />
            {plan && (
              <Pill
                id="scale-mode-pill"
                variant="outline"
                className={
                  "min-h-6 px-2 py-0.5 text-xs " +
                  (plan.modeLabel === "Empirical Tape" ? "text-info-foreground" : "text-warning-foreground")
                }
              >
                <PillStatus className="border-r-0 pr-0">
                  <PillIndicator variant={plan.modeLabel === "Empirical Tape" ? "info" : "warning"} />
                  {plan.modeLabel}
                </PillStatus>
              </Pill>
            )}
          </div>
          <p className="mt-0.5 text-xs text-ink-2">
            Paper capacity, bankroll sizing, and 180-day compounding grounded in out-of-sample tape.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Button
            id="scale-plan-run-btn"
            variant="default"
            className="min-h-11 gap-1.5 px-3.5 text-xs font-medium sm:min-h-9"
            disabled={running}
            onClick={() => void runPlan(seed, true)}
          >
            <RefreshCw className={`size-3.5 ${running ? "animate-spin" : ""}`} aria-hidden />
            <span>{running ? "Calculating…" : "Run Scale Plan"}</span>
          </Button>
        </div>
      </div>

      {/* Interactive Inputs */}
      <div className="mt-4 grid grid-cols-2 gap-2.5 sm:grid-cols-3 lg:grid-cols-7">
        <div className="flex flex-col gap-1">
          <label htmlFor="scale-target-daily" className="text-xs text-ink-3">
            Target Daily ($)
          </label>
          <input
            id="scale-target-daily"
            type="number"
            min={10}
            max={50000}
            step={50}
            value={targetDaily}
            onChange={(e) => setTargetDaily(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          />
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="scale-bankroll" className="text-xs text-ink-3">
            Starting Pot ($)
          </label>
          <input
            id="scale-bankroll"
            type="number"
            min={50}
            max={1000000}
            step={100}
            value={bankroll}
            onChange={(e) => setBankroll(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          />
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="scale-slippage" className="text-xs text-ink-3">
            Slippage (0–5c)
          </label>
          <input
            id="scale-slippage"
            type="number"
            min={0}
            max={5}
            step={1}
            value={slippageCents}
            onChange={(e) => setSlippageCents(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          />
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="scale-max-contracts" className="text-xs text-ink-3">
            Max Cts / Window
          </label>
          <input
            id="scale-max-contracts"
            type="number"
            min={1}
            max={10000}
            step={25}
            value={maxContracts}
            onChange={(e) => setMaxContracts(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          />
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="scale-series-count" className="text-xs text-ink-3">
            Active Series
          </label>
          <select
            id="scale-series-count"
            value={seriesCount}
            onChange={(e) => setSeriesCount(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          >
            <option value="1">1 · BTC only</option>
            <option value="2">2 · BTC + ETH</option>
            <option value="3">3 · BTC–SOL</option>
            <option value="4">4 · All 4 series</option>
          </select>
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="scale-sizing-mode" className="text-xs text-ink-3">
            Sizing Mode
          </label>
          <select
            id="scale-sizing-mode"
            value={sizingMode}
            onChange={(e) => setSizingMode(e.target.value as SizingMode)}
            className="min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          >
            <option value="equal_slots">Equal Slots</option>
            <option value="quarter_kelly">Quarter-Kelly</option>
            <option value="half_kelly">Half-Kelly</option>
          </select>
        </div>

        <div className="col-span-2 flex flex-col gap-1 sm:col-span-1">
          <label htmlFor="scale-assumed-edge" className="text-xs text-ink-3">
            What-If Edge (c)
          </label>
          <input
            id="scale-assumed-edge"
            type="number"
            min={0}
            max={20}
            step={0.5}
            placeholder="Empirical"
            value={assumedEdge}
            onChange={(e) => setAssumedEdge(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink placeholder:text-ink-3 outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          />
        </div>
      </div>

      {/* Honest non-positive OOS edge callout */}
      {plan && plan.negativeEdgeReason && (
        <div
          id="scale-negative-edge-banner"
          role="status"
          className="mt-3 flex flex-wrap items-start gap-2 rounded-lg border border-hairline bg-board px-3.5 py-2.5 text-xs text-warning-foreground"
        >
          <ShieldAlert className="mt-0.5 size-4 shrink-0" aria-hidden />
          <span>{plan.negativeEdgeReason}</span>
        </div>
      )}

      {/* Main 2-column body: Trajectory + Required Bankroll/Liquidity left, 5-Gate Readiness Scorecard right */}
      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-12">
        {/* Left: Capital Sizing, Liquidity Ceiling & 180-Day Trajectory Chart */}
        <div className="space-y-3 lg:col-span-7">
          {/* Required Bankroll & Empirical Daily Run-Rate */}
          <dl className="grid grid-cols-2 gap-2 sm:grid-cols-4">
            <div className="rounded-lg border border-hairline bg-board p-2.5">
              <dt className="text-xs text-ink-3">Empirical Daily</dt>
              <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                {plan ? `${moneySigned(plan.currentEmpiricalDailyDollars)}/d` : "-"}
              </dd>
              <span className="mono text-xs text-ink-3">
                Target {plan ? `${moneyWhole(plan.targetDailyDollars)}/d` : "$500/d"}
              </span>
            </div>

            <div className="rounded-lg border border-hairline bg-board p-2.5">
              <dt className="text-xs text-ink-3">Equal Slots Pot</dt>
              <dd id="scale-req-equal" className="mono mt-0.5 text-sm font-semibold text-ink">
                {plan
                  ? plan.requiredBankrollEqualSlots !== null
                    ? moneyWhole(plan.requiredBankrollEqualSlots)
                    : "N/A"
                  : "-"}
              </dd>
              <span className="mono text-xs text-ink-3">
                {plan && plan.requiredBankrollEqualSlots === null ? "Non-positive edge" : "4-slot split"}
              </span>
            </div>

            <div className="rounded-lg border border-hairline bg-board p-2.5">
              <dt className="text-xs text-ink-3">Quarter-Kelly</dt>
              <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                {plan
                  ? plan.requiredBankrollQuarterKelly !== null
                    ? moneyWhole(plan.requiredBankrollQuarterKelly)
                    : "N/A"
                  : "-"}
              </dd>
              <span className="mono text-xs text-ink-3">0.25x Kelly</span>
            </div>

            <div className="rounded-lg border border-hairline bg-board p-2.5">
              <dt className="text-xs text-ink-3">Half-Kelly</dt>
              <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                {plan
                  ? plan.requiredBankrollHalfKelly !== null
                    ? moneyWhole(plan.requiredBankrollHalfKelly)
                    : "N/A"
                  : "-"}
              </dd>
              <span className="mono text-xs text-ink-3">0.50x Kelly</span>
            </div>
          </dl>

          {/* Liquidity Ceiling bar */}
          {plan && (
            <div className="rounded-lg border border-hairline bg-board/60 p-3">
              <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
                <span className="inline-flex items-center gap-1.5 font-medium text-ink">
                  <Gauge className="size-3.5 text-ink-2" aria-hidden />
                  <span>Liquidity Capacity Ceiling</span>
                </span>
                <span className="mono text-ink-2">
                  Fill ratio at target:{" "}
                  <strong className={fillPct >= 75 ? "text-success-foreground" : "text-warning-foreground"}>
                    {fillPct}%
                  </strong>
                </span>
              </div>
              <div className="mt-2 h-2 w-full overflow-hidden rounded-full bg-track" aria-hidden="true">
                <div
                  className={fillPct >= 75 ? "h-full bg-dot-green" : "h-full bg-dot-amber"}
                  style={{ width: `${Math.min(100, Math.max(2, fillPct))}%` }}
                />
              </div>
              <div className="mt-2 flex flex-wrap items-center justify-between gap-2 font-mono text-xs text-ink-2">
                <span>1 series max: {moneyWhole(plan.liquidityCeiling.singleSeriesMaxDailyDollars)}/d</span>
                <span>
                  {plan.liquidityCeiling.activeSeriesCount} series max:{" "}
                  <strong className="text-ink">{moneyWhole(plan.liquidityCeiling.portfolioMaxDailyDollars)}/d</strong>
                </span>
                <span className="text-ink-3">
                  {plan.liquidityCeiling.maxContractsPerWindow} cts × {plan.liquidityCeiling.windowsPerDay}/d
                </span>
              </div>
            </div>
          )}

          {/* 180-Day Compounding Trajectory SVG */}
          <div className="rounded-lg border border-hairline bg-board/40 p-3">
            <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <h4 className="text-xs font-semibold text-ink">180-Day Compounding Trajectory (1,000 Paths)</h4>
              </div>
              <div className="flex items-center gap-1.5" role="group" aria-label="Trajectory chart metric">
                {(
                  [
                    ["daily", "$/Day Run-Rate"],
                    ["equity", "Paper Equity"],
                  ] as const
                ).map(([id, label]) => (
                  <button
                    key={id}
                    type="button"
                    aria-pressed={chartMetric === id}
                    onClick={() => setChartMetric(id)}
                    className={
                      "min-h-11 cursor-pointer rounded-md border px-2.5 text-xs font-medium outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                      (chartMetric === id
                        ? "border-ink bg-ink text-canvas"
                        : "border-hairline bg-surface text-ink-2 hover:bg-board hover:text-ink")
                    }
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>

            {plan ? (
              <CompoundingTrajectoryChart plan={plan} metric={chartMetric} />
            ) : (
              <div className="h-56 animate-pulse rounded-md bg-track" />
            )}

            {plan && (
              <div className="mt-2.5 grid grid-cols-3 gap-2 border-t border-hairline pt-2.5 font-mono text-xs">
                <div>
                  <span className="text-ink-3">Reach Target</span>
                  <div className="font-semibold text-ink">
                    {(plan.compoundingProjection.probReachTarget * 100).toFixed(1)}%
                  </div>
                </div>
                <div>
                  <span className="text-ink-3">Median Days</span>
                  <div className="font-semibold text-ink">
                    {plan.compoundingProjection.medianDaysToTarget !== null
                      ? `${plan.compoundingProjection.medianDaysToTarget}d`
                      : "Not reached"}
                  </div>
                </div>
                <div>
                  <span className="text-ink-3">P(DD &ge; 25%)</span>
                  <div className="font-semibold text-destructive-foreground">
                    {(plan.compoundingProjection.probDdGte25Pct * 100).toFixed(1)}%
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Right: 5-Gate Mechanical Readiness Scorecard */}
        <div
          ref={scorecardRef}
          id="readiness-scorecard"
          data-readiness-scorecard
          className="flex flex-col justify-between gap-3 rounded-lg border border-hairline bg-board/40 p-3.5 lg:col-span-5"
        >
          <div>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h4 className="text-sm font-semibold text-ink">5-Gate Readiness Scorecard</h4>
              <Pill
                id="readiness-verdict-pill"
                variant="outline"
                className={
                  "min-h-6 px-2.5 py-0.5 text-xs font-medium " +
                  (isReadyForReview ? "text-success-foreground" : "text-warning-foreground")
                }
              >
                <PillStatus className="border-r-0 pr-0">
                  <PillIndicator variant={isReadyForReview ? "success" : "warning"} />
                  {isReadyForReview ? "Ready for Owner Review" : "Paper Research Only"}
                </PillStatus>
              </Pill>
            </div>
            <p className="mt-1 text-xs text-ink-2">
              {gatesPassed} of {gatesTotal} mechanical gates passed on OOS tape.
            </p>

            <ul className="mt-3 space-y-2">
              {(plan?.readinessGates.gates || []).map((g) => (
                <li
                  key={g.id}
                  data-readiness-gate={g.id}
                  className="rounded-lg border border-hairline bg-surface p-2.5 text-xs"
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="inline-flex items-center gap-1.5 font-medium text-ink">
                      {g.passed ? (
                        <CheckCircle2 className="size-4 shrink-0 text-success-foreground" aria-hidden />
                      ) : (
                        <XCircle className="size-4 shrink-0 text-destructive-foreground" aria-hidden />
                      )}
                      <span>{g.label}</span>
                    </span>
                    <Badge
                      variant="outline"
                      className={
                        "mono text-xs " +
                        (g.passed ? "text-success-foreground" : "text-destructive-foreground")
                      }
                    >
                      {g.actualDisplay}
                    </Badge>
                  </div>
                  <div className="mt-1 flex flex-wrap items-center justify-between gap-2 text-ink-2">
                    <span>{g.reason}</span>
                    <span className="mono text-ink-3">Gate: {g.thresholdDisplay}</span>
                  </div>
                </li>
              ))}
            </ul>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-2.5 text-xs text-ink-3">
            <span>Quant scaling engine · hard risk rails enforced on live orders</span>
            <span className="mono">
              {plan?.source === "server" ? "Server Scale Engine" : "Client scale fallback"}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}
