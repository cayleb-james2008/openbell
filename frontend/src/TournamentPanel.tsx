// Quant Lab: Out-of-Sample Strategy Tournament, Optimal Uncorrelated 4-Slot Lineup (|r| <= 0.65),
// 12x12 Return Correlation Matrix SVG, and Entry-Price x Time-to-Close Edge-by-Regime Heatmap.
// Strictly paper-only; zero user-facing internal engine jargon.
import { type RefObject, useCallback, useEffect, useMemo, useState } from "react";
import { AlertTriangle, Grid3X3, RefreshCw, Sparkles, Trophy } from "lucide-react";
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
import { CorrelationMatrixChart, ProvenanceTag, RegimeHeatmapChart } from "@/Charts";
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  type Strategy,
  moneySigned,
} from "@/lib/desk";
import { type TournamentData, fetchTournament } from "@/lib/tournament";

function fmtCents(v: number | null): string {
  if (v === null || !Number.isFinite(v)) return "-";
  return `${v >= 0 ? "+" : ""}${v.toFixed(2)}c`;
}

export function TournamentPanel({
  dataset,
  home,
  strategies,
  markets,
  slots,
  search,
  tournamentRef,
  correlationRef,
  regimeRef,
}: {
  dataset: Dataset;
  home: Home | null;
  strategies: Strategy[];
  markets: Market[];
  slots: Slots | null;
  search: string;
  tournamentRef?: RefObject<HTMLDivElement | null>;
  correlationRef?: RefObject<HTMLDivElement | null>;
  regimeRef?: RefObject<HTMLDivElement | null>;
}) {
  const [slippage, setSlippage] = useState<number>(1);
  const [data, setData] = useState<TournamentData | null>(null);
  const [loading, setLoading] = useState(false);

  const loadTournament = useCallback(
    async (slip: number) => {
      setLoading(true);
      try {
        const res = await fetchTournament(dataset, slip, home, strategies, markets, slots);
        setData(res);
      } finally {
        setLoading(false);
      }
    },
    [dataset, home, strategies, markets, slots],
  );

  useEffect(() => {
    void loadTournament(slippage);
  }, [loadTournament, slippage]);

  const lineupSet = useMemo(
    () => new Set(data?.optimalLineup.strategies || []),
    [data],
  );

  const filteredRankings = useMemo(() => {
    if (!data) return [];
    const q = search.trim().toLowerCase();
    if (!q) return data.rankings;
    return data.rankings.filter(
      (r) =>
        r.strategy.toLowerCase().includes(q) ||
        r.side.toLowerCase().includes(q) ||
        (r.inOptimalLineup && "lineup".includes(q)),
    );
  }, [data, search]);

  const lineupRows = useMemo(() => {
    if (!data) return [];
    const byName = new Map(data.rankings.map((r) => [r.strategy, r]));
    return data.optimalLineup.strategies.map((name) => byName.get(name)).filter(Boolean);
  }, [data]);

  return (
    <div className="space-y-4">
      {/* Main Tournament + Optimal Uncorrelated 4-Slot Lineup Card */}
      <div
        ref={tournamentRef}
        id="quant-tournament"
        className="scroll-mt-16 overflow-hidden rounded-[10px] border border-hairline bg-surface"
      >
        <div className="flex flex-wrap items-start justify-between gap-3 border-b border-hairline bg-board/50 p-4">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <Trophy className="size-4 text-info-foreground" aria-hidden />
              <h3 className="text-sm font-semibold text-ink">Paper Strategy Tournament &amp; 4-Slot Lineup</h3>
              <ProvenanceTag kind="paper" />
            </div>
            <p className="mt-1 text-xs text-ink-2">
              Ranks all strategies strictly on held-out OOS (40%) expectancy after fees and slippage.
            </p>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <label htmlFor="tournament-slippage" className="text-xs text-ink-3">
              Slippage
            </label>
            <select
              id="tournament-slippage"
              value={String(slippage)}
              onChange={(e) => setSlippage(Number(e.target.value))}
              className="mono min-h-11 rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
            >
              <option value="0">0c</option>
              <option value="1">1c</option>
              <option value="2">2c</option>
              <option value="3">3c</option>
              <option value="5">5c</option>
            </select>
            <Button
              id="tournament-refresh-btn"
              variant="outline"
              className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-8"
              disabled={loading}
              onClick={() => void loadTournament(slippage)}
            >
              <RefreshCw className={`size-3.5 ${loading ? "animate-spin" : ""}`} aria-hidden />
              <span>Refresh</span>
            </Button>
          </div>
        </div>

        {/* Optimal Uncorrelated 4-Slot Lineup Strip */}
        <div id="tournament-lineup" className="border-b border-hairline bg-board/30 p-4">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <Sparkles className="size-3.5 text-info-foreground" aria-hidden />
              <h4 className="text-xs font-semibold text-ink">
                Optimal Uncorrelated 4-Slot Lineup (|r| &le; {(data?.optimalLineup.maxCorrelationLimit ?? 0.65).toFixed(2)})
              </h4>
            </div>
            <StatusPill
              id="tournament-lineup-pill"
              tone={data && !data.optimalLineup.isFallback ? "success" : "warning"}
              className={
                "min-h-6 px-2.5 py-0.5 text-xs " +
                (data && !data.optimalLineup.isFallback
                  ? "text-success-foreground"
                  : "text-warning-foreground")
              }
            >
              {data ? `${data.optimalLineup.strategies.length}/4 slots qualified` : "Evaluating…"}
            </StatusPill>
          </div>

          {data?.optimalLineup.isFallback && (
            <div
              role="status"
              className="mt-2.5 flex items-start gap-2 rounded-lg border border-dot-amber/40 bg-board px-3 py-2 text-xs text-warning-foreground"
            >
              <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
              <span>{data.optimalLineup.honestyNote}</span>
            </div>
          )}

          {lineupRows.length > 0 && (
            <div className="mt-3 grid grid-cols-1 divide-y divide-hairline rounded-lg border border-hairline bg-surface sm:grid-cols-2 sm:divide-x lg:grid-cols-4 lg:divide-y-0">
              {lineupRows.map((row, idx) =>
                row ? (
                  <div
                    key={row.strategy}
                    data-lineup-slot={idx + 1}
                    className="p-3"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="mono text-xs font-semibold text-info-foreground">Slot #{idx + 1}</span>
                      <Badge variant="outline" className="mono text-xs">
                        {row.side.toUpperCase()}
                      </Badge>
                    </div>
                    <p className="mt-1 truncate text-xs font-semibold text-ink">{row.strategy}</p>
                    <div className="mono mt-1.5 flex items-center justify-between text-xs">
                      <span className="font-semibold text-success-foreground">
                        {fmtCents(row.oosNetExpectancyCents)}/ct
                      </span>
                      <span className="text-ink-3">
                        {row.oosSharpe !== null ? `SR ${row.oosSharpe.toFixed(2)}` : `${row.oosWindows}w`}
                      </span>
                    </div>
                  </div>
                ) : null,
              )}
            </div>
          )}

          {data && !data.optimalLineup.isFallback && (
            <p className="mt-2 text-xs text-ink-2">{data.optimalLineup.honestyNote}</p>
          )}
        </div>

        {/* 12-Strategy OOS Tournament Ranking Table */}
        <div id="tournament-table">
          {!data ? (
            <div className="p-4">
              <div className="h-44 animate-pulse rounded-lg bg-track" />
            </div>
          ) : filteredRankings.length === 0 ? (
            <p role="status" className="p-5 text-sm text-ink-2">
              No strategies match the active filter.
            </p>
          ) : (
            <Table className="stack">
              <TableHeader>
                <TableRow>
                  <TableHead className="w-12">#</TableHead>
                  <TableHead>Strategy</TableHead>
                  <TableHead>Side</TableHead>
                  <TableHead className="text-right">OOS Exp</TableHead>
                  <TableHead className="text-right">Sharpe</TableHead>
                  <TableHead className="text-right">Max DD</TableHead>
                  <TableHead className="text-right">OOS n</TableHead>
                  <TableHead className="text-right">OOS P&amp;L</TableHead>
                  <TableHead>Series (BTC / ETH / SOL / XRP)</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {filteredRankings.map((row) => (
                  <TableRow
                    key={row.strategy}
                    data-tournament-row={row.strategy}
                    className={row.inOptimalLineup ? "bg-blue-soft/35" : undefined}
                  >
                    <TableCell className="mono text-xs font-semibold text-ink-3">#{row.rank}</TableCell>
                    <TableCell className="m-grow max-w-[220px]">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span className="break-all text-xs font-semibold text-ink sm:text-sm">{row.strategy}</span>
                        {row.inOptimalLineup && (
                          <Badge variant="outline" className="border-blue bg-blue-soft text-xs text-info-foreground">
                            Lineup
                          </Badge>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="m-right">
                      <Badge variant="outline" className="mono text-xs">
                        {row.side.toUpperCase()}
                      </Badge>
                    </TableCell>
                    <TableCell
                      className={
                        "mono text-right text-xs font-semibold " +
                        (row.oosNetExpectancyCents !== null && row.oosNetExpectancyCents > 0
                          ? "text-success-foreground"
                          : row.oosNetExpectancyCents !== null && row.oosNetExpectancyCents < 0
                            ? "text-destructive-foreground"
                            : "text-ink-3")
                      }
                    >
                      {fmtCents(row.oosNetExpectancyCents)}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink">
                      {row.oosSharpe !== null ? row.oosSharpe.toFixed(2) : "-"}
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-destructive-foreground">
                      -{row.oosMaxDrawdownPct.toFixed(1)}%
                    </TableCell>
                    <TableCell className="mono text-right text-xs text-ink-2">
                      {row.oosWindows}w
                    </TableCell>
                    <TableCell
                      className={
                        "mono text-right text-xs font-semibold " +
                        (row.oosNetPnlDollars > 0
                          ? "text-success-foreground"
                          : row.oosNetPnlDollars < 0
                            ? "text-destructive-foreground"
                            : "text-ink-2")
                      }
                    >
                      {moneySigned(row.oosNetPnlDollars)}
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-wrap items-center gap-1">
                        {row.seriesBreakdown.filter((s) => s.windows > 0).length > 0 ? (
                          row.seriesBreakdown
                            .filter((s) => s.windows > 0)
                            .map((sb) => (
                              <span
                                key={sb.series}
                                className={
                                  "mono rounded border border-hairline bg-board px-1.5 py-0.5 text-xs " +
                                  (sb.netExpectancyCents !== null && sb.netExpectancyCents > 0
                                    ? "text-success-foreground"
                                    : sb.netExpectancyCents !== null && sb.netExpectancyCents < 0
                                      ? "text-destructive-foreground"
                                      : "text-ink-2")
                                }
                              >
                                {sb.series} {sb.netExpectancyCents !== null ? `${sb.netExpectancyCents >= 0 ? "+" : ""}${sb.netExpectancyCents.toFixed(1)}c` : `${sb.windows}w`}
                              </span>
                            ))
                        ) : (
                          <span className="mono text-xs text-ink-3">No OOS series fills</span>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </div>

        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline px-4 py-2.5 text-xs text-ink-3">
          <span>{data?.honestyNote || "Chronological 60% IS / 40% OOS paper tournament."}</span>
          <span className="mono">
            {data?.source === "server" ? "Server Tournament Engine" : "Client tournament fallback"}
          </span>
        </div>
      </div>

      {/* 12x12 Correlation Matrix SVG + Edge-by-Regime Heatmap SVG */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-12">
        <div
          ref={correlationRef}
          id="quant-correlation"
          className="scroll-mt-16 flex flex-col justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-6"
        >
          <div>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <Grid3X3 className="size-4 text-info-foreground" aria-hidden />
                <h3 className="text-sm font-semibold text-ink">12&times;12 Return Correlation Matrix</h3>
                <ProvenanceTag kind="paper" />
              </div>
              <span className="mono text-xs text-ink-3">Cap |r| &le; {(data?.correlation.threshold ?? 0.65).toFixed(2)}</span>
            </div>
            <p className="mt-1 text-xs text-ink-2">
              Pairwise Pearson correlation on shared OOS windows; amber border flags |r| &gt; 0.65.
            </p>
          </div>

          {data ? (
            <CorrelationMatrixChart data={data.correlation} lineupSet={lineupSet} />
          ) : (
            <div className="h-64 animate-pulse rounded-lg bg-track" />
          )}

          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-2.5 text-xs text-ink-3">
            <span>Blue = positive r, Red = inverse r</span>
            <span className="mono">{data?.correlation.strategies.length ?? 0} strategies compared</span>
          </div>
        </div>

        <div
          ref={regimeRef}
          id="quant-regime-heatmap"
          className="scroll-mt-16 flex flex-col justify-between gap-3 rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-6"
        >
          <div>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <Grid3X3 className="size-4 text-success-foreground" aria-hidden />
                <h3 className="text-sm font-semibold text-ink">Edge-by-Regime Heatmap</h3>
                <ProvenanceTag kind="tape" />
              </div>
              <span className="mono text-xs text-ink-3">Price &times; Time to close</span>
            </div>
            <p className="mt-1 text-xs text-ink-2">
              Net paper expectancy after fees and {slippage}c slippage; amber dot flags n &lt; 20.
            </p>
          </div>

          {data ? (
            <RegimeHeatmapChart
              priceBuckets={data.regimePriceBuckets}
              timeBuckets={data.regimeTimeBuckets}
              cells={data.regimeCells}
            />
          ) : (
            <div className="h-64 animate-pulse rounded-lg bg-track" />
          )}

          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-2.5 text-xs text-ink-3">
            <span>Green = positive net edge, Red = fee/edge drag</span>
            <span className="mono">Wilson 95% CI on hover</span>
          </div>
        </div>
      </div>
    </div>
  );
}
