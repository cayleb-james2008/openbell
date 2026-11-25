// Quant Lab (4th workbench tab): Calibration Reliability SVG with 95% Wilson error bars + Brier/ECE,
// 1,000-path Monte Carlo Bootstrap Fan Chart with Run/Reseed, and Walk-Forward In-Sample vs Out-of-Sample Table.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Dices, RefreshCw, ShieldCheck } from "lucide-react";
import { toast } from "sonner";
import {
  Badge,
  Button,
  StatusPill,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { CalibrationReliabilityChart, MonteCarloFanChart, ProvenanceTag } from "@/Charts";
import { EdgeAuditPanel } from "@/EdgeAuditPanel";
import { ScalePlannerPanel } from "@/ScalePlannerPanel";
import { RecalibrationPanel } from "@/RecalibrationPanel";
import { TournamentPanel } from "@/TournamentPanel";
import {
  type Dataset,
  type Home,
  type Market,
  type Read,
  type Slots,
  type Strategy,
  money,
  moneySigned,
} from "@/lib/desk";
import {
  type CalibrationData,
  type MonteCarloData,
  type WalkForwardData,
  fetchCalibration,
  fetchWalkForward,
  runMonteCarlo,
} from "@/lib/quant";

function fmtRatio(v: number | null): string {
  return v === null || !Number.isFinite(v) ? "-" : v.toFixed(2);
}

export type QuantSectionId =
  | "audit"
  | "scale"
  | "scorecard"
  | "tournament"
  | "correlation"
  | "regime"
  | "recalibration"
  | "calibration"
  | "montecarlo"
  | "walkforward";

export function QuantLabTab({
  dataset,
  home,
  marketsRead,
  slotsRead,
  strategiesRead,
  search,
  focusSection,
  reseedSignal,
  auditSignal,
  onClearFocus,
}: {
  dataset: Dataset;
  home: Home | null;
  marketsRead?: Read<Market[]>;
  slotsRead?: Read<Slots>;
  strategiesRead?: Read<Strategy[]>;
  search: string;
  focusSection?: QuantSectionId | null;
  reseedSignal?: number;
  auditSignal?: number;
  onClearFocus?: () => void;
}) {
  const [calibration, setCalibration] = useState<CalibrationData | null>(null);
  const [walkForward, setWalkForward] = useState<WalkForwardData | null>(null);
  const [mc, setMc] = useState<MonteCarloData | null>(null);
  const [seed, setSeed] = useState<number>(42);
  const [seedInput, setSeedInput] = useState<string>("42");
  const [runningMc, setRunningMc] = useState(false);
  const [wfScope, setWfScope] = useState<"all" | "portfolio" | "strategies">("all");

  const auditRef = useRef<HTMLDivElement>(null);
  const scaleRef = useRef<HTMLDivElement>(null);
  const scorecardRef = useRef<HTMLDivElement>(null);
  const tournamentRef = useRef<HTMLDivElement>(null);
  const correlationRef = useRef<HTMLDivElement>(null);
  const regimeRef = useRef<HTMLDivElement>(null);
  const recalRef = useRef<HTMLDivElement>(null);
  const calRef = useRef<HTMLDivElement>(null);
  const mcRef = useRef<HTMLDivElement>(null);
  const wfRef = useRef<HTMLDivElement>(null);

  const markets = useMemo(
    () => (marketsRead?.ok ? marketsRead.v.filter((m): m is Market => !!m && typeof m === "object") : []),
    [marketsRead],
  );
  const strategies = useMemo(
    () => (strategiesRead?.ok ? strategiesRead.v.filter((s): s is Strategy => !!s && typeof s === "object") : []),
    [strategiesRead],
  );
  const slots = slotsRead?.ok ? slotsRead.v : null;

  useEffect(() => {
    let alive = true;
    fetchCalibration(dataset, home, markets, slots).then((res) => {
      if (alive) setCalibration(res);
    });
    fetchWalkForward(dataset, home, slots).then((res) => {
      if (alive) setWalkForward(res);
    });
    return () => {
      alive = false;
    };
  }, [dataset, home, markets, slots]);

  const executeMc = useCallback(
    async (nextSeed: number, notify = false) => {
      setRunningMc(true);
      try {
        const res = await runMonteCarlo(dataset, home, slots, nextSeed, 1000);
        setMc(res);
        setSeed(res.seed);
        setSeedInput(String(res.seed));
        if (notify) {
          toast.success(
            `Simulated ${res.paths.toLocaleString()} bootstrap paths (seed ${res.seed}), P50 ${money(res.terminal.p50)}.`,
          );
        }
      } finally {
        setRunningMc(false);
      }
    },
    [dataset, home, slots],
  );

  useEffect(() => {
    void executeMc(seed, false);
  }, [dataset, home, slots]);

  const scrollToSection = useCallback((sec: QuantSectionId) => {
    const node =
      sec === "audit"
        ? auditRef.current
        : sec === "scale"
          ? scaleRef.current
        : sec === "scorecard"
          ? scorecardRef.current
          : sec === "tournament"
            ? tournamentRef.current
            : sec === "correlation"
              ? correlationRef.current
              : sec === "regime"
                ? regimeRef.current
                : sec === "recalibration"
                  ? recalRef.current
                  : sec === "calibration"
                    ? calRef.current
                    : sec === "montecarlo"
                      ? mcRef.current
                      : wfRef.current;
    node?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  // Respond to Cmd+K jump requests
  useEffect(() => {
    if (!focusSection) return;
    scrollToSection(focusSection);
    onClearFocus?.();
  }, [focusSection, scrollToSection, onClearFocus]);

  useEffect(() => {
    if (reseedSignal && reseedSignal > 0) {
      const next = ((seed * 1664525 + 1013904223) >>> 0) % 99991 + 1;
      void executeMc(next, true);
    }
  }, [reseedSignal]);

  const filteredWfRows = useMemo(() => {
    if (!walkForward) return [];
    const q = search.trim().toLowerCase();
    return walkForward.rows.filter((r) => {
      if (wfScope === "portfolio" && !r.isPortfolio) return false;
      if (wfScope === "strategies" && r.isPortfolio) return false;
      if (q && !r.strategy.toLowerCase().includes(q)) return false;
      return true;
    });
  }, [walkForward, wfScope, search]);

  const eceBadge = useMemo(() => {
    if (!calibration || calibration.ece === null || calibration.totalWindows === 0) {
      return { variant: "info" as const, label: "Needs sample" };
    }
    if (calibration.ece <= 0.05) return { variant: "success" as const, label: "Well-calibrated" };
    if (calibration.ece <= 0.1) return { variant: "warning" as const, label: "Moderate drift" };
    return { variant: "error" as const, label: "High drift" };
  }, [calibration]);

  return (
    <section id="quant-lab" aria-label="Quant Lab statistical diagnostics" className="space-y-4">
      {/* Compact in-tab section jump pills */}
      <div className="flex flex-wrap items-center gap-1.5" role="navigation" aria-label="Quant Lab sections">
        {(
          [
            ["audit", "Edge Audit"],
            ["scale", "$500/Day Scale Planner"],
            ["scorecard", "5-Gate Scorecard"],
            ["tournament", "Tournament"],
            ["correlation", "12×12 Corr"],
            ["regime", "Regime Map"],
            ["recalibration", "Isotonic PAV"],
            ["calibration", "Calibration"],
            ["montecarlo", "Monte Carlo"],
            ["walkforward", "Walk-Forward"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            onClick={() => scrollToSection(id)}
            className="min-h-11 cursor-pointer rounded-md border border-hairline bg-surface px-3 text-xs font-medium text-ink-2 outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) hover:bg-board hover:text-ink focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
          >
            {label}
          </button>
        ))}
      </div>

      {/* 1-Click Full Quant Edge Audit Scorecard */}
      <EdgeAuditPanel
        dataset={dataset}
        home={home}
        strategies={strategies}
        markets={markets}
        slots={slots}
        auditSignal={auditSignal}
        panelRef={auditRef}
      />

      {/* $500/Day Paper Capital Scale Planner, 180-Day Trajectory & 5-Gate Readiness Scorecard */}
      <ScalePlannerPanel
        dataset={dataset}
        home={home}
        markets={markets}
        slots={slots}
        panelRef={scaleRef}
        scorecardRef={scorecardRef}
      />

      {/* OOS Strategy Tournament, Optimal Uncorrelated 4-Slot Lineup, 12x12 Correlation & Edge-by-Regime Heatmap */}
      <TournamentPanel
        dataset={dataset}
        home={home}
        strategies={strategies}
        markets={markets}
        slots={slots}
        search={search}
        tournamentRef={tournamentRef}
        correlationRef={correlationRef}
        regimeRef={regimeRef}
      />

      {/* PAV Isotonic Probability Recalibration & EV-After-Fees Entry Gate */}
      <RecalibrationPanel
        dataset={dataset}
        home={home}
        markets={markets}
        slots={slots}
        panelRef={recalRef}
      />

      {/* Row 1: Calibration Reliability SVG Chart + Monte Carlo 1,000-Path Fan Chart */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-12">
        {/* Calibration Reliability Card */}
        <div
          ref={calRef}
          id="quant-calibration"
          className="scroll-mt-16 flex flex-col justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-6"
        >
          <div>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <h3 className="text-sm font-semibold text-ink">Calibration Reliability</h3>
                <ProvenanceTag kind="tape" />
              </div>
              <StatusPill
                tone={eceBadge.variant === "error" ? "error" : eceBadge.variant}
                className="min-h-6 px-2 py-0.5 text-xs text-ink-2"
              >
                {eceBadge.label}
              </StatusPill>
            </div>
            <p className="mt-1 text-xs text-ink-2">
              Implied YES probability vs realized settlement rate with 95% Wilson bounds.
            </p>

            {/* Brier, ECE, Sample KPIs */}
            <dl className="mt-3 grid grid-cols-3 divide-x divide-hairline border-y border-hairline">
              <div className="p-2.5">
                <dt className="text-xs text-ink-3">Brier Score</dt>
                <dd id="quant-brier" className="mono mt-0.5 text-sm font-semibold text-ink">
                  {calibration?.brierScore !== null && calibration?.brierScore !== undefined
                    ? calibration.brierScore.toFixed(4)
                    : "-"}
                </dd>
              </div>
              <div className="p-2.5">
                <dt className="text-xs text-ink-3">ECE</dt>
                <dd id="quant-ece" className="mono mt-0.5 text-sm font-semibold text-ink">
                  {calibration?.ece !== null && calibration?.ece !== undefined
                    ? `${(calibration.ece * 100).toFixed(2)}%`
                    : "-"}
                </dd>
              </div>
              <div className="p-2.5">
                <dt className="text-xs text-ink-3">Settled Tape</dt>
                <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                  {calibration ? calibration.totalWindows.toLocaleString() : "-"}
                </dd>
              </div>
            </dl>
          </div>

          <div className="mt-1">
            {calibration ? (
              <CalibrationReliabilityChart buckets={calibration.buckets} />
            ) : (
              <div className="h-56 animate-pulse rounded-lg bg-track" />
            )}
          </div>

          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-2.5 text-xs text-ink-3">
            <span>Dashed line = ideal calibration (y = x)</span>
            <span className="mono">
              {calibration?.source === "server" ? "Server Quant Engine" : "Tape-derived fallback"}
            </span>
          </div>
        </div>

        {/* Monte Carlo Bootstrap Fan Card */}
        <div
          ref={mcRef}
          id="quant-montecarlo"
          className="scroll-mt-16 flex flex-col justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-6"
        >
          <div>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <h3 className="text-sm font-semibold text-ink">Monte Carlo Fan (1,000 Paths)</h3>
                <ProvenanceTag kind="paper" />
              </div>

              <div className="flex items-center gap-1.5">
                <label htmlFor="mc-seed-input" className="text-xs text-ink-3">
                  Seed
                </label>
                <input
                  id="mc-seed-input"
                  aria-label="Monte Carlo random seed"
                  type="number"
                  min={1}
                  max={9999999}
                  value={seedInput}
                  onChange={(e) => setSeedInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const n = Math.max(1, Math.floor(Number(seedInput) || 42));
                      void executeMc(n, true);
                    }
                  }}
                  className="mono min-h-11 w-20 rounded-md border border-hairline bg-board px-2 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                />
                <Button
                  id="mc-reseed-btn"
                  variant="outline"
                  className="min-h-11 gap-1.5 px-2.5 text-xs sm:min-h-8"
                  disabled={runningMc}
                  onClick={() => {
                    const parsed = Math.floor(Number(seedInput));
                    const next =
                      Number.isFinite(parsed) && parsed > 0 && parsed !== seed
                        ? parsed
                        : ((seed * 1664525 + 1013904223) >>> 0) % 99991 + 1;
                    void executeMc(next, true);
                  }}
                >
                  {runningMc ? (
                    <RefreshCw className="size-3.5 animate-spin" aria-hidden />
                  ) : (
                    <Dices className="size-3.5" aria-hidden />
                  )}
                  <span>{runningMc ? "Running…" : "Run / Reseed"}</span>
                </Button>
              </div>
            </div>

            <p className="mt-1 text-xs text-ink-2">
              Bootstrap P5, P25, P50, P75, and P95 paper equity bands over settled trade returns.
            </p>

            {/* Terminal & Drawdown Exceedance Readouts */}
            <dl className="mt-3 grid grid-cols-2 divide-y divide-hairline border-y border-hairline sm:grid-cols-4 sm:divide-x sm:divide-y-0">
              <div className="p-2.5">
                <dt className="text-xs text-ink-3">P50 Median</dt>
                <dd id="mc-p50" className="mono mt-0.5 text-sm font-semibold text-info-foreground">
                  {mc ? money(mc.terminal.p50) : "-"}
                </dd>
              </div>
              <div className="p-2.5">
                <dt className="text-xs text-ink-3">P5 / P95</dt>
                <dd className="mono mt-0.5 text-xs font-semibold text-ink">
                  {mc ? `${money(mc.terminal.p5)} / ${money(mc.terminal.p95)}` : "-"}
                </dd>
              </div>
              <div className="p-2.5">
                <dt className="text-xs text-ink-3">P(DD &ge; 10%)</dt>
                <dd className="mono mt-0.5 text-sm font-semibold text-warning-foreground">
                  {mc ? `${(mc.ddProb10 * 100).toFixed(1)}%` : "-"}
                </dd>
              </div>
              <div className="p-2.5">
                <dt className="text-xs text-ink-3">P(DD &ge; 25%)</dt>
                <dd className="mono mt-0.5 text-sm font-semibold text-destructive-foreground">
                  {mc ? `${(mc.ddProb25 * 100).toFixed(1)}%` : "-"}
                </dd>
              </div>
            </dl>
          </div>

          <div className="mt-1">
            {mc ? (
              <MonteCarloFanChart mc={mc} />
            ) : (
              <div className="h-56 animate-pulse rounded-lg bg-track" />
            )}
          </div>

          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-2.5 text-xs text-ink-3">
            <span>Seed {mc?.seed ?? seed} ({mc ? `${mc.elapsedMs}ms` : "pending"})</span>
            <span className="mono">
              {mc?.source === "server" ? "Server Bootstrap Engine" : "Seeded client fallback"}
            </span>
          </div>
        </div>
      </div>

      {/* Row 2: Walk-Forward In-Sample vs Out-of-Sample Table */}
      <div
        ref={wfRef}
        id="quant-walkforward"
        className="scroll-mt-16 overflow-hidden rounded-[10px] border border-hairline bg-surface"
      >
        <div className="flex flex-wrap items-center justify-between gap-2.5 border-b border-hairline bg-board/50 px-4 py-3">
          <div>
            <div className="flex flex-wrap items-center gap-2">
              <ShieldCheck className="size-4 text-info-foreground" aria-hidden />
              <h3 className="text-sm font-semibold text-ink">Walk-Forward Split (In-Sample vs Out-of-Sample)</h3>
              <ProvenanceTag kind="paper" />
            </div>
            <p className="mt-0.5 text-xs text-ink-2">
              Chronological {Math.round((walkForward?.inSampleRatio ?? 0.6) * 100)}% In-Sample vs{" "}
              {Math.round((1 - (walkForward?.inSampleRatio ?? 0.6)) * 100)}% Out-of-Sample paper diagnostics.
            </p>
          </div>

          <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter walk-forward scope">
            {(
              [
                ["all", "All"],
                ["portfolio", "Portfolio"],
                ["strategies", "Strategies"],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                aria-pressed={wfScope === id}
                onClick={() => setWfScope(id)}
                className={
                  "min-h-11 min-w-11 cursor-pointer rounded-md border px-3 text-xs font-medium outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 sm:min-w-0 " +
                  (wfScope === id
                    ? "border-ink bg-ink text-canvas"
                    : "border-hairline bg-surface text-ink-2 hover:bg-board hover:text-ink")
                }
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {filteredWfRows.length === 0 ? (
          <p role="status" className="p-5 text-sm text-ink-2">
            No settled paper trade splits match the current filter yet.
          </p>
        ) : (
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Strategy / Scope</TableHead>
                <TableHead>Split</TableHead>
                <TableHead className="text-right">Sharpe</TableHead>
                <TableHead className="text-right">Sortino</TableHead>
                <TableHead className="text-right">Profit Factor</TableHead>
                <TableHead className="text-right">Max DD</TableHead>
                <TableHead className="text-right">Fee Drag</TableHead>
                <TableHead className="text-right">Win / Loss Streak</TableHead>
                <TableHead className="text-right">Paper P&amp;L</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {filteredWfRows.flatMap((row) =>
                (
                  [
                    ["IS", row.inSample],
                    ["OOS", row.outOfSample],
                  ] as const
                ).map(([splitTag, m]) => (
                  <TableRow
                    key={`${row.strategy}-${splitTag}`}
                    className={row.isPortfolio ? "bg-board/40" : undefined}
                  >
                    <TableCell className="m-grow max-w-[240px]">
                      <div className="flex items-center gap-2">
                        <span className={"break-all text-sm " + (row.isPortfolio ? "font-semibold text-ink" : "font-medium text-ink")}>
                          {row.strategy}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell className="m-right">
                      <Badge
                        variant="outline"
                        className={
                          "mono text-xs " +
                          (splitTag === "OOS" ? "border-blue bg-blue-soft text-info-foreground" : "text-ink-2")
                        }
                      >
                        {splitTag} ({m.windows}w)
                      </Badge>
                    </TableCell>
                    <TableCell className="mono text-right text-xs font-medium text-ink">
                      {fmtRatio(m.sharpe)}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      {fmtRatio(m.sortino)}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink">
                      {fmtRatio(m.profitFactor)}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-destructive-foreground">
                      -{m.maxDrawdownPct.toFixed(2)}%
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      {m.feeDragPct.toFixed(1)}%
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      <span className="text-success-foreground">+{m.maxWinStreak}</span>
                      {" / "}
                      <span className="text-destructive-foreground">-{m.maxLossStreak}</span>
                    </TableCell>
                    <TableCell
                      className={
                        "mono text-right text-xs font-semibold " +
                        (m.netPnlDollars > 0
                          ? "text-success-foreground"
                          : m.netPnlDollars < 0
                            ? "text-destructive-foreground"
                            : "text-ink")
                      }
                    >
                      {moneySigned(m.netPnlDollars)}
                    </TableCell>
                  </TableRow>
                )),
              )}
            </TableBody>
          </Table>
        )}
      </div>
    </section>
  );
}
