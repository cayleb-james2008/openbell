// Hero Telemetry & Compounding Strip (first fold at 1440x900 & 390x844).
// 4 SlidingNumber KPI tiles in Geist Mono + Paper Equity Curve + Tremor Tracker & CategoryBar.
import { BorderTrail } from "@/components/ui/border-trail";
import { SlidingNumber } from "@/components/ui/sliding-number";
import { ProgressCircle } from "@/components/ProgressCircle/ProgressCircle";
import { Tracker, type TrackerBlockProps } from "@/components/Tracker/Tracker";
import { CategoryBar } from "@/components/CategoryBar/CategoryBar";
import { HeroEquityChart, ProvenanceTag, TremorSpark, validPoints } from "@/Charts";
import {
  type Dataset,
  type Equity,
  type Home,
  type Market,
  type Read,
  type Slots,
  type StandingInfo,
  ageWords,
  isNum,
  marketIsOpen,
  money,
  moneySigned,
  moneyWhole,
  seriesShort,
} from "@/lib/desk";

const SERIES_ORDER = ["BTC", "ETH", "SOL", "XRP", "DOGE"] as const;

function splitMoneyParts(v: number): { neg: boolean; whole: number; cents: number } {
  const neg = v < 0;
  const rounded = Math.round(Math.abs(v) * 100);
  return { neg, whole: Math.floor(rounded / 100), cents: rounded % 100 };
}

function SkeletonLine({ w = "w-24" }: { w?: string }) {
  return <span aria-hidden="true" className={`inline-block h-7 rounded-md bg-track ${w}`} />;
}

export function HeroStrip({
  dataset,
  homeRead,
  slotsRead,
  equityRead,
  marketsRead,
  standing,
  collectingNow,
}: {
  dataset: Dataset;
  homeRead?: Read<Home>;
  slotsRead?: Read<Slots>;
  equityRead?: Read<Equity>;
  marketsRead?: Read<Market[]>;
  standing: StandingInfo;
  collectingNow: boolean;
}) {
  const home = homeRead?.ok ? homeRead.v : null;
  const slots = slotsRead?.ok ? slotsRead.v : null;
  const equity = equityRead?.ok ? equityRead.v : null;
  const markets = marketsRead?.ok ? marketsRead.v.filter((m): m is Market => !!m && typeof m === "object") : null;

  const potVal = slots && isNum(slots.paper_equity_dollars) ? slots.paper_equity_dollars : null;
  const potStart = slots && isNum(slots.paper_pot_start) ? slots.paper_pot_start : 100;
  const potDelta = potVal !== null ? potVal - potStart : null;
  const potParts = potVal !== null ? splitMoneyParts(potVal) : null;
  const stageTarget =
    (slots?.stage_target && isNum(slots.stage_target.target_dollars) && slots.stage_target.target_dollars) ||
    (home?.stage_target && isNum(home.stage_target.target_dollars) && home.stage_target.target_dollars) ||
    1000;
  const stagePct = potVal !== null && stageTarget > 0 ? Math.min(100, Math.max(0, (potVal / stageTarget) * 100)) : 0;

  const openMarketsCount = markets ? markets.filter(marketIsOpen).length : null;
  const totalMarketsCount = markets ? markets.length : isNum(home?.collector?.markets_seen) ? (home?.collector?.markets_seen as number) : null;
  const snapshotsCount = home && isNum(home.collector?.snapshots_on_disk) ? home.collector.snapshots_on_disk : null;
  const settledWindows = home && isNum(home.collector?.settled_windows) ? home.collector.settled_windows : null;

  const potPoints = validPoints(equity?.pot);
  const trades = slots?.trades && Array.isArray(slots.trades) ? slots.trades : [];

  // Build 24-block Tremor Tracker from recent settled trades, falling back to recently settled market windows on live tape.
  const trackerData: TrackerBlockProps[] = [];
  if (trades.length > 0) {
    const recent = trades.slice(-24);
    for (const t of recent) {
      const pnl = isNum(t.pnl_dollars) ? t.pnl_dollars : 0;
      trackerData.push({
        color: pnl > 0 ? "bg-dot-green" : pnl < 0 ? "bg-dot-red" : "bg-disabled",
        tooltip: `${t.strategy} · ${moneySigned(pnl)} (${t.contracts}x @ ${Math.round((t.price || 0) * 100)}c)`,
      });
    }
  } else if (markets && markets.length > 0) {
    const settled = markets
      .filter((m) => m.result === "yes" || m.result === "no")
      .slice(-24);
    for (const m of settled) {
      trackerData.push({
        color: m.result === "yes" ? "bg-dot-green" : "bg-blue",
        tooltip: `${m.ticker || "window"} settled ${String(m.result).toUpperCase()}`,
      });
    }
  }

  // Series mix for Tremor CategoryBar across BTC/ETH/SOL/XRP/DOGE.
  const seriesCounts = SERIES_ORDER.map((sym) =>
    markets ? markets.filter((m) => seriesShort(m.series, m.ticker) === sym).length : 0,
  );
  const seriesSum = seriesCounts.reduce((a, b) => a + b, 0);

  return (
    <section
      id="readings"
      aria-label="Desk telemetry and paper compounding summary"
      className="relative overflow-hidden rounded-[12px] border border-hairline bg-surface p-4 shadow-(--elev-card) sm:p-5"
    >
      {collectingNow && <BorderTrail size={72} className="bg-blue" />}

      {/* Row 1: Four SlidingNumber KPI tiles in tabular Geist Mono */}
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {/* Tile 1: Paper Compounding Pot */}
        <div id="pot" className="flex flex-col justify-between gap-2 rounded-[10px] border border-hairline bg-board/60 p-3.5">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-ink-2">Paper Compounding Pot</span>
            <ProvenanceTag kind="paper" />
          </div>
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              {!slotsRead ? (
                <SkeletonLine w="w-28" />
              ) : potParts ? (
                <div
                  id="pot-figure"
                  aria-label={`Paper compounding pot ${money(potVal)}`}
                  className="mono flex items-baseline text-2xl font-semibold tracking-tight text-ink"
                >
                  <span>{potParts.neg ? "-$" : "$"}</span>
                  <SlidingNumber value={potParts.whole} group="," />
                  <span>.</span>
                  <SlidingNumber value={potParts.cents} padStart />
                </div>
              ) : (
                <div id="pot-figure" className="mono text-lg font-medium text-ink-2">
                  unavailable
                </div>
              )}
              <div id="pot-change" className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs tabular-nums">
                {potDelta !== null ? (
                  <span className={potDelta >= 0 ? "text-success-foreground" : "text-destructive-foreground"}>
                    {moneySigned(potDelta)} vs {moneyWhole(potStart)}
                  </span>
                ) : (
                  <span className="text-ink-3">{slotsRead && !slotsRead.ok ? slotsRead.msg : "waiting"}</span>
                )}
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <ProgressCircle value={stagePct} radius={20} strokeWidth={4} showAnimation={false}>
                <span id="stage-figure" className="font-mono text-xs font-medium text-ink">
                  {Math.round(stagePct)}%
                </span>
              </ProgressCircle>
            </div>
          </div>
          <div id="stage-line" className="flex items-center justify-between gap-2 text-xs text-ink-3">
            <span>Stage 1 target {moneyWhole(stageTarget)}</span>
            {potPoints.length >= 2 && (
              <TremorSpark points={potPoints} start={potStart} label="Paper pot mini spark" className="h-5 w-16" />
            )}
          </div>
        </div>

        {/* Tile 2: Proven Strategies (preserves standing-line honesty & null/shape guards) */}
        <div id="standing" className="flex flex-col justify-between gap-2 rounded-[10px] border border-hairline bg-board/60 p-3.5">
          <div className="flex items-center justify-between gap-2">
            <span id="standing-label" className="text-xs font-medium text-ink-2">
              {standing.state === "error"
                ? "Strategy state unavailable"
                : standing.state === "empty"
                  ? "No strategies in the library"
                  : "Proven Strategies"}
            </span>
            <ProvenanceTag kind="paper" />
          </div>
          <div className="min-w-0">
            {standing.state === "loading" ? (
              <SkeletonLine w="w-20" />
            ) : standing.state === "ready" ? (
              <div className="flex items-baseline gap-1.5">
                <span id="standing-figure" className="sr-only">
                  {standing.provenForward} of {standing.total}
                </span>
                <div
                  aria-hidden="true"
                  className="mono flex items-baseline text-2xl font-semibold tracking-tight text-ink"
                >
                  <SlidingNumber value={standing.provenForward} />
                </div>
                <span aria-hidden="true" className="mono text-sm font-medium text-ink-2">
                  of {standing.total}
                </span>
              </div>
            ) : (
              <div className="text-sm font-medium text-ink-2">
                {standing.state === "error" ? "Could not score library" : "Add .rn files to strategies/"}
              </div>
            )}
          </div>
          <ul id="standing-detail" className="flex flex-col gap-0.5 text-xs text-ink-2">
            {standing.state === "ready" &&
              standing.lines.map((line, idx) => (
                <li key={idx} className="min-w-0">
                  <span className="block truncate">{line}</span>
                </li>
              ))}
            {standing.state === "error" && (
              <li className="text-destructive-foreground">Strategy read failed; retry in Strategies.</li>
            )}
            {standing.state === "empty" && <li className="text-ink-3">0 strategy scripts loaded</li>}
            {standing.state === "loading" && <li className="text-ink-3">Loading strategy verdicts…</li>}
          </ul>
        </div>

        {/* Tile 3: Active 15m Markets */}
        <div id="markets-kpi" className="flex flex-col justify-between gap-2 rounded-[10px] border border-hairline bg-board/60 p-3.5">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-ink-2">Active 15m Markets</span>
            <ProvenanceTag kind="venue" />
          </div>
          <div className="min-w-0">
            {!marketsRead && !homeRead ? (
              <SkeletonLine w="w-16" />
            ) : openMarketsCount !== null ? (
              <div className="flex items-baseline gap-2">
                <div
                  aria-label={`${openMarketsCount} active 15m markets`}
                  className="mono flex items-baseline text-2xl font-semibold tracking-tight text-ink"
                >
                  <SlidingNumber value={openMarketsCount} />
                </div>
                <span className="mono text-xs text-ink-2">open live</span>
              </div>
            ) : (
              <div className="mono text-lg font-medium text-ink-2">unavailable</div>
            )}
          </div>
          <div className="flex items-center justify-between gap-2 text-xs text-ink-2">
            <span className="mono shrink-0">
              {totalMarketsCount !== null ? `${totalMarketsCount.toLocaleString()} tracked` : "No tape yet"}
            </span>
            <span className="mono text-ink-3">5 crypto series</span>
          </div>
        </div>

        {/* Tile 4: Live Tape Snapshots */}
        <div id="snapshots-kpi" className="flex flex-col justify-between gap-2 rounded-[10px] border border-hairline bg-board/60 p-3.5">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-ink-2">Live Tape Snapshots</span>
            <ProvenanceTag kind="tape" />
          </div>
          <div className="min-w-0">
            {!homeRead ? (
              <SkeletonLine w="w-24" />
            ) : snapshotsCount !== null ? (
              <div
                aria-label={`${snapshotsCount.toLocaleString()} snapshots on disk`}
                className="mono flex items-baseline text-2xl font-semibold tracking-tight text-ink"
              >
                <SlidingNumber value={snapshotsCount} group="," />
              </div>
            ) : (
              <div className="mono text-lg font-medium text-ink-2">unavailable</div>
            )}
          </div>
          <div className="flex items-center justify-between gap-2 text-xs text-ink-2">
            <span className="mono">
              {settledWindows !== null ? `${settledWindows.toLocaleString()} settled` : "-"}
            </span>
            <span className="mono text-ink-3">
              {home?.collector
                ? home.collector.snapshots_on_disk === 0
                  ? "Fresh tape · Collect now"
                  : `Updated ${ageWords(home.collector.newest_snapshot_age_secs)}`
                : "Checking"}
            </span>
          </div>
        </div>
      </div>

      {/* Row 2: Paper Compounding Curve + Recent Window Outcomes Tracker */}
      <div className="mt-4 grid grid-cols-1 gap-3 lg:grid-cols-12">
        <div className="rounded-[10px] border border-hairline bg-board/40 p-3.5 lg:col-span-8">
          <div className="mb-1 flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-semibold text-ink">
              {dataset === "fixture" ? "Paper Compounding Curve (Example Tape)" : "Paper Compounding Curve"}
            </h2>
            <span className="font-mono text-xs text-ink-3">
              <span className="hidden sm:inline">Hover to inspect · </span>Paper sim
            </span>
          </div>
          <HeroEquityChart
            points={potPoints}
            start={potStart}
            target={stageTarget}
            liveWarming={dataset === "live" && (snapshotsCount ?? 0) > 0}
            snapshots={snapshotsCount ?? 0}
            openMarkets={openMarketsCount ?? 0}
          />
        </div>

        <div className="flex flex-col justify-between gap-3 rounded-[10px] border border-hairline bg-board/40 p-3.5 lg:col-span-4">
          <div>
            <div className="flex items-center justify-between gap-2">
              <h2 className="text-sm font-semibold text-ink">
                {trades.length > 0 ? "Recent Paper Trades" : "Recent Settled Windows"}
              </h2>
              <span className="font-mono text-xs text-ink-3">Last {trackerData.length || 0}</span>
            </div>
            <p className="mt-1 text-xs text-ink-2">
              {trades.length > 0
                ? "Green = net gain after fees · Red = paper loss."
                : "Green = YES settle · Blue = NO settle on 15m tape."}
            </p>
            <div className="mt-3">
              {trackerData.length > 0 ? (
                <Tracker data={trackerData} hoverEffect />
              ) : (
                <div className="rounded-md border border-dashed border-hairline bg-surface px-3 py-4 text-xs text-ink-2">
                  Run <span className="font-medium text-ink">Collect now</span> to pull live Kalshi 15m quotes.
                </div>
              )}
            </div>
          </div>

          <div className="border-t border-hairline pt-3">
            <div className="mb-1.5 flex items-center justify-between text-xs">
              <span className="font-medium text-ink">5-Series Tape Coverage</span>
              <span className="font-mono text-ink-3">{seriesSum.toLocaleString()} markets</span>
            </div>
            {seriesSum > 0 ? (
              <>
                <CategoryBar
                  values={seriesCounts}
                  colors={["blue", "emerald", "violet", "amber", "cyan"]}
                  showLabels={false}
                />
                <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs text-ink-2">
                  {SERIES_ORDER.map((sym, idx) => (
                    <span key={sym}>
                      {sym} <strong className="font-medium text-ink">{seriesCounts[idx]}</strong>
                    </span>
                  ))}
                </div>
              </>
            ) : (
              <p className="text-xs text-ink-3">Watching KXBTC15M, KXETH15M, KXSOL15M, KXXRP15M, KXDOGE15M.</p>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}
