// Interactive Rune Strategy Sandbox Editor & Paper Tape Backtester.
// Calls POST /api/strategy/validate and POST /api/backtest on v0.5.0+, falling back cleanly to the
// deterministic client Rune evaluator on loaded tape windows. Strictly paper-only.
import { useEffect, useMemo, useState } from "react";
import { CheckCircle2, Code2, Play, RotateCcw, ShieldCheck, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { StrategyEquityChart, ProvenanceTag } from "@/Charts";
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  type Strategy,
  cleanStr,
  dayLabel,
  fetchStrategySource,
  isNum,
  moneySigned,
  timeUtcLabel,
} from "@/lib/desk";
import {
  type BacktestResult,
  type RuneValidationResult,
  DEFAULT_SANDBOX_RUNE,
  runRuneBacktest,
  validateRuneStrategy,
} from "@/lib/quant";

export function RuneSandbox({
  strategies,
  markets,
  slots,
  home,
  dataset,
  initialStrategyName,
  onClose,
}: {
  strategies: Strategy[];
  markets: Market[];
  slots: Slots | null;
  home: Home | null;
  dataset: Dataset;
  initialStrategyName?: string | null;
  onClose?: () => void;
}) {
  const [source, setSource] = useState<string>(DEFAULT_SANDBOX_RUNE);
  const [loadedFrom, setLoadedFrom] = useState<string>("template");
  const [loadingTemplate, setLoadingTemplate] = useState(false);
  const [validating, setValidating] = useState(false);
  const [backtesting, setBacktesting] = useState(false);
  const [validation, setValidation] = useState<RuneValidationResult | null>(null);
  const [result, setResult] = useState<BacktestResult | null>(null);

  const strategyNames = useMemo(
    () =>
      strategies
        .map((s) => cleanStr(s.strategy, ""))
        .filter(Boolean)
        .sort((a, b) => a.localeCompare(b)),
    [strategies],
  );

  const loadLibraryStrategy = async (name: string) => {
    if (!name || name === "template") {
      setSource(DEFAULT_SANDBOX_RUNE);
      setLoadedFrom("template");
      setValidation(null);
      setResult(null);
      return;
    }
    setLoadingTemplate(true);
    try {
      const res = await fetchStrategySource(name);
      setSource(res.source);
      setLoadedFrom(name);
      setValidation(null);
      setResult(null);
    } catch (e) {
      toast.error(`Could not load ${name}: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLoadingTemplate(false);
    }
  };

  useEffect(() => {
    if (initialStrategyName) {
      void loadLibraryStrategy(initialStrategyName);
    }
  }, [initialStrategyName]);

  const baselineStats = useMemo(() => {
    if (!slots) return null;
    if (loadedFrom && loadedFrom !== "template") {
      const tot = slots.strategy_totals?.find((t) => t.strategy === loadedFrom);
      const row = strategies.find((s) => cleanStr(s.strategy, "") === loadedFrom);
      if (tot || row) {
        return {
          label: loadedFrom,
          pnl: tot && isNum(tot.paper_pnl_dollars) ? tot.paper_pnl_dollars : null,
          windows: row && isNum(row.windows_traded) ? row.windows_traded : tot?.windows_traded ?? null,
          meanCents: row && isNum(row.mean_cents_per_contract) ? row.mean_cents_per_contract : null,
        };
      }
    }
    const potDelta = isNum(slots.paper_equity_dollars) && isNum(slots.paper_pot_start)
      ? slots.paper_equity_dollars - slots.paper_pot_start
      : null;
    return {
      label: "Active Paper Pot",
      pnl: potDelta,
      windows: slots.trades ? slots.trades.length : null,
      meanCents: null,
    };
  }, [slots, strategies, loadedFrom]);

  const lineCount = useMemo(() => Math.max(1, source.split("\n").length), [source]);

  const handleValidate = async () => {
    setValidating(true);
    try {
      const res = await validateRuneStrategy(source, home);
      setValidation(res);
      if (res.valid) {
        toast.success(`Strategy syntax valid (${res.name} · ${res.side.toUpperCase()}).`);
      } else {
        toast.error(`Validation found ${res.diagnostics.filter((d) => d.level === "error").length} issue(s).`);
      }
    } finally {
      setValidating(false);
    }
  };

  const handleBacktest = async () => {
    setBacktesting(true);
    try {
      const startEq = slots && isNum(slots.paper_pot_start) ? slots.paper_pot_start : 100;
      const res = await runRuneBacktest(source, dataset, home, markets, slots, startEq);
      setResult(res);
      setValidation({
        valid: res.valid,
        name: res.strategy,
        side: res.side,
        diagnostics: res.diagnostics,
        elapsedMs: res.elapsedMs,
        source: res.source,
      });
      if (res.valid) {
        toast.success(
          `Backtest complete: ${res.summary.windowsTraded} trades, ${moneySigned(res.summary.netPnlDollars)} paper P&L.`,
        );
      } else {
        toast.error("Backtest halted: fix strategy compile diagnostics first.");
      }
    } finally {
      setBacktesting(false);
    }
  };

  return (
    <div
      id="rune-sandbox"
      data-rune-sandbox
      className="rounded-[10px] border border-hairline bg-surface p-4 shadow-(--elev-card)"
    >
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-hairline pb-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <Code2 className="size-4 text-info-foreground" aria-hidden />
            <h3 className="text-sm font-semibold text-ink">Interactive Strategy Sandbox</h3>
            <ProvenanceTag kind="paper" />
          </div>
          <p className="mt-0.5 text-xs text-ink-2">
            Edit strategy rules in memory, inspect diagnostics, and backtest on collected 15m tape windows.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <label className="sr-only" htmlFor="sandbox-template-select">
            Load strategy template
          </label>
          <select
            id="sandbox-template-select"
            aria-label="Load strategy template"
            value={loadedFrom}
            disabled={loadingTemplate}
            onChange={(e) => void loadLibraryStrategy(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          >
            <option value="template">Starter template</option>
            {strategyNames.map((n) => (
              <option key={n} value={n}>
                {n}.rn
              </option>
            ))}
          </select>

          {onClose && (
            <Button variant="ghost" className="min-h-11 px-3 text-xs sm:min-h-9" onClick={onClose}>
              Hide sandbox
            </Button>
          )}
        </div>
      </div>

      {/* Side-by-side Editor + Backtest Results */}
      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-12">
        {/* Left: Code Editor & Controls */}
        <div className="flex flex-col gap-3 lg:col-span-6">
          <div className="overflow-hidden rounded-lg border border-hairline bg-board">
            <div className="flex items-center justify-between border-b border-hairline bg-surface px-3 py-1.5 font-mono text-xs text-ink-3">
              <span>{loadedFrom === "template" ? "sandbox.rn" : `${loadedFrom}.rn`}</span>
              <span>{lineCount} lines · whole-cent sim</span>
            </div>
            <div className="flex">
              <div
                aria-hidden="true"
                className="mono select-none border-r border-hairline bg-surface/60 px-2.5 py-3 text-right text-xs leading-5 text-ink-3"
              >
                {Array.from({ length: lineCount }, (_, i) => (
                  <div key={i}>{i + 1}</div>
                ))}
              </div>
              <textarea
                id="rune-editor"
                aria-label="Strategy source code"
                spellCheck={false}
                value={source}
                onChange={(e) => setSource(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Tab") {
                    e.preventDefault();
                    const el = e.currentTarget;
                    const start = el.selectionStart;
                    const end = el.selectionEnd;
                    const next = source.slice(0, start) + "    " + source.slice(end);
                    setSource(next);
                    requestAnimationFrame(() => {
                      el.selectionStart = el.selectionEnd = start + 4;
                    });
                  }
                }}
                rows={12}
                className="mono min-h-[240px] w-full resize-y bg-transparent px-3 py-3 text-xs leading-5 text-ink outline-none"
              />
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button
              id="sandbox-backtest-btn"
              variant="default"
              className="min-h-11 gap-2 px-3.5 text-xs font-medium sm:min-h-9"
              disabled={backtesting || validating}
              onClick={() => void handleBacktest()}
            >
              <Play className="size-3.5" aria-hidden />
              <span>{backtesting ? "Backtesting…" : "Backtest on Tape"}</span>
            </Button>

            <Button
              id="sandbox-validate-btn"
              variant="outline"
              className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-9"
              disabled={validating || backtesting}
              onClick={() => void handleValidate()}
            >
              <ShieldCheck className="size-3.5" aria-hidden />
              <span>{validating ? "Validating…" : "Validate Strategy"}</span>
            </Button>

            <Button
              variant="ghost"
              className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-9"
              onClick={() => {
                setSource(DEFAULT_SANDBOX_RUNE);
                setLoadedFrom("template");
                setValidation(null);
                setResult(null);
              }}
            >
              <RotateCcw className="size-3.5" aria-hidden />
              <span>Reset</span>
            </Button>
          </div>

          {/* Compile diagnostics */}
          {validation && (
            <div
              id="sandbox-diagnostics"
              role={validation.valid ? "status" : "alert"}
              className="rounded-lg border border-hairline bg-board p-3 text-xs"
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="inline-flex items-center gap-1.5 font-semibold text-ink">
                  {validation.valid ? (
                    <CheckCircle2 className="size-4 text-success-foreground" aria-hidden />
                  ) : (
                    <TriangleAlert className="size-4 text-destructive-foreground" aria-hidden />
                  )}
                  <span>{validation.valid ? "Compile passed" : "Compile diagnostics"}</span>
                </span>
                <span className="mono text-ink-3">
                  {validation.source === "server" ? "Server Sandbox" : "Local evaluator"} · {validation.elapsedMs}ms
                </span>
              </div>
              <ul className="mt-2 space-y-1">
                {validation.diagnostics.map((d, idx) => (
                  <li
                    key={idx}
                    className={
                      "mono " +
                      (d.level === "error"
                        ? "text-destructive-foreground"
                        : d.level === "warn"
                          ? "text-warning-foreground"
                          : "text-ink-2")
                    }
                  >
                    {d.line ? `L${d.line}: ` : ""}
                    {d.message}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        {/* Right: Side-by-Side Backtest Results */}
        <div className="flex flex-col gap-3 lg:col-span-6" data-sandbox-results>
          {!result ? (
            <div className="flex flex-1 flex-col justify-between rounded-lg border border-dashed border-hairline bg-board/40 p-5">
              <div>
                <h4 className="text-sm font-semibold text-ink">Side-by-Side Paper Backtest Output</h4>
                <p className="mt-1 text-xs text-ink-2">
                  Press <strong className="font-medium text-ink">Backtest on Tape</strong> to score your strategy script against all settled 15m windows on the active tape.
                </p>
              </div>
              <dl className="mt-4 grid grid-cols-2 gap-2.5 text-xs">
                <div className="rounded-md border border-hairline bg-surface p-2.5">
                  <dt className="text-ink-3">Baseline Scope</dt>
                  <dd className="mono mt-0.5 font-medium text-ink">{baselineStats?.label || "Active Tape"}</dd>
                </div>
                <div className="rounded-md border border-hairline bg-surface p-2.5">
                  <dt className="text-ink-3">Baseline Paper P&amp;L</dt>
                  <dd className="mono mt-0.5 font-medium text-ink">
                    {baselineStats?.pnl !== null && baselineStats?.pnl !== undefined
                      ? moneySigned(baselineStats.pnl)
                      : "$0.00"}
                  </dd>
                </div>
              </dl>
            </div>
          ) : (
            <>
              {/* Metrics comparison cards */}
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex flex-wrap items-center gap-2">
                  <Badge variant="outline" className="mono text-xs">
                    {result.strategy}
                  </Badge>
                  <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-info-foreground">
                    <PillStatus className="border-r-0 pr-0">
                      <PillIndicator variant={result.valid ? "success" : "error"} />
                      {result.side.toUpperCase()}
                    </PillStatus>
                  </Pill>
                </div>
                <span className="mono text-xs text-ink-3">
                  {result.summary.windowsTraded} trades · {result.elapsedMs}ms
                </span>
              </div>

              <dl className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Paper Net P&amp;L</dt>
                  <dd
                    className={
                      "mono mt-0.5 text-sm font-semibold " +
                      (result.summary.netPnlDollars >= 0 ? "text-success-foreground" : "text-destructive-foreground")
                    }
                  >
                    {moneySigned(result.summary.netPnlDollars)}
                  </dd>
                  {baselineStats?.pnl !== null && baselineStats?.pnl !== undefined && (
                    <span className="mono text-xs text-ink-3">
                      vs {moneySigned(baselineStats.pnl)}
                    </span>
                  )}
                </div>
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Sharpe</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.sharpe !== null ? result.summary.sharpe.toFixed(2) : "-"}
                  </dd>
                  <span className="mono text-xs text-ink-3">
                    Sortino {result.summary.sortino !== null ? result.summary.sortino.toFixed(2) : "-"}
                  </span>
                </div>
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Max Drawdown</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-destructive-foreground">
                    -{result.summary.maxDrawdownPct.toFixed(2)}%
                  </dd>
                  <span className="mono text-xs text-ink-3">Fee drag {result.summary.feeDragPct.toFixed(1)}%</span>
                </div>
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Brier Score</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.brierScore !== null ? result.summary.brierScore.toFixed(4) : "-"}
                  </dd>
                </div>
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Win Rate</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.winRatePct.toFixed(1)}%
                  </dd>
                </div>
                <div className="rounded-lg border border-hairline bg-board p-2.5">
                  <dt className="text-xs text-ink-3">Mean / Contract</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.meanCentsPerContract >= 0 ? "+" : ""}
                    {result.summary.meanCentsPerContract.toFixed(2)}c
                  </dd>
                </div>
              </dl>

              {/* Equity curve */}
              <div className="rounded-lg border border-hairline bg-board/50 p-3">
                <StrategyEquityChart
                  points={result.equityCurve}
                  start={0}
                  height={146}
                  label={`Sandbox backtest paper curve for ${result.strategy}`}
                />
              </div>

              {/* Recent backtest paper trades */}
              <div>
                <div className="mb-1.5 flex items-center justify-between text-xs">
                  <span className="font-semibold text-ink">Backtest Trade List ({result.trades.length})</span>
                  <span className="mono text-ink-3">Latest 12 shown</span>
                </div>
                {result.trades.length === 0 ? (
                  <p className="rounded-md border border-hairline bg-board/40 px-3 py-2 text-xs text-ink-2">
                    Zero settled windows matched `allow_entry(...)` on this tape.
                  </p>
                ) : (
                  <ul className="max-h-44 divide-y divide-hairline overflow-y-auto rounded-lg border border-hairline bg-board/40">
                    {[...result.trades].reverse().slice(0, 12).map((t, idx) => (
                      <li key={idx} className="flex flex-wrap items-center justify-between gap-2 px-3 py-1.5 font-mono text-xs">
                        <span className="text-ink-2">
                          {dayLabel(t.closeEpoch)} {timeUtcLabel(t.closeEpoch)}
                        </span>
                        <span className="truncate text-ink">
                          {t.ticker} · {t.contracts}x @ {Math.round(t.price * 100)}c
                        </span>
                        <span className={t.pnlDollars >= 0 ? "font-medium text-success-foreground" : "font-medium text-destructive-foreground"}>
                          {moneySigned(t.pnlDollars)}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
