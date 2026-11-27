// Public Read-Only Auto-Collector Schedule Control Strip (Off / 60s / 180s), 200-Window Readiness Gate Progress Bar,
// Live L2 Multi-Ticker WS/Trade Telemetry Strip, and Live Spot Oracle Strip.
// Rendered both inside Live 15m Tape and synchronized inside the Settings Vaul sheet. Strictly public GET only.
import { Activity, AlertTriangle, Clock, ShieldCheck } from "lucide-react";
import { StatusPill } from "@/components/ui";
import { type CollectorScheduleStatus, type Market, ageWords, isNum, seriesShort } from "@/lib/desk";
import type { OrderbookStreamStatus } from "@/lib/orderbook";
import {
  type SpotStatusResponse,
  computeSpotKalshiDivergence,
  formatBps,
  formatSpotUsd,
} from "@/lib/spot";

type CadenceChoice = "off" | "60" | "180";

function activeChoice(status: CollectorScheduleStatus | null): CadenceChoice {
  if (!status || !status.enabled) return "off";
  return status.intervalSecs >= 120 ? "180" : "60";
}

function l2FeedBadge(status?: OrderbookStreamStatus | null): {
  tone: "success" | "info" | "warning" | "neutral";
  label: string;
} {
  const active = status?.active_books ?? 0;
  if (status?.ws_connected || status?.mode === "ws") {
    return status.stale_books > 0 && status.stale_books >= active && active > 0
      ? { tone: "warning", label: `L2 WS Stale (${active})` }
      : { tone: "success", label: `L2 WS (${active})` };
  }
  if (status?.mode === "rest") {
    return { tone: "info", label: `L2 REST (${active})` };
  }
  return { tone: "neutral", label: "L2 Offline" };
}

function formatSyncHz(rateHz: number | undefined): string {
  const rate = typeof rateHz === "number" && Number.isFinite(rateHz) && rateHz >= 0 ? rateHz : 0;
  return rate > 0 && rate < 1 ? `${rate.toFixed(2)} Hz` : `${rate.toFixed(1)} Hz`;
}

export function AutoCollectorStrip({
  status,
  updating,
  onSelectSchedule,
  onOpenScorecard,
  l2StreamStatus,
  selectedL2Ticker = "",
  onSelectL2Ticker,
  onInspectL2Ticker,
  nowMs,
  variant = "banner",
}: {
  status: CollectorScheduleStatus | null;
  updating: boolean;
  onSelectSchedule: (enabled: boolean, intervalSecs: number) => void;
  onOpenScorecard?: () => void;
  l2StreamStatus?: OrderbookStreamStatus | null;
  selectedL2Ticker?: string;
  onSelectL2Ticker?: (ticker: string) => void;
  onInspectL2Ticker?: (ticker: string) => void;
  nowMs?: number;
  variant?: "banner" | "compact";
}) {
  const choice = activeChoice(status);
  const settled = isNum(status?.settledWindows) ? Math.max(0, status.settledWindows) : 0;
  const target =
    isNum(status?.readinessTargetWindows) && status.readinessTargetWindows > 0
      ? status.readinessTargetWindows
      : 200;
  const pct = Math.min(100, Math.max(0, Math.round((settled / Math.max(1, target)) * 100)));
  const remaining = Math.max(0, target - settled);

  const badgeTone = status?.rateLimited
    ? "warning"
    : status?.enabled
      ? "success"
      : "info";
  const badgeText = status?.rateLimited
    ? `429 backoff ${status.backoffSecs || status.effectiveIntervalSecs}s`
    : status?.enabled
      ? `Auto ${status.intervalSecs}s`
      : "Auto-collect off";

  const choices: readonly [CadenceChoice, string, boolean, number][] = [
    ["off", "Off", false, 60],
    ["60", "60s", true, 60],
    ["180", "180s", true, 180],
  ];

  if (variant === "compact") {
    return (
      <div
        id="settings-auto-collector"
        className="space-y-3 rounded-[10px] border border-hairline bg-board p-3"
      >
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-1.5">
            <Clock className="size-3.5 text-info-foreground" aria-hidden />
            <span className="text-xs font-semibold text-ink">Auto-Collector Schedule</span>
          </div>
          <StatusPill tone={badgeTone}>
            <span className="mono">{badgeText}</span>
          </StatusPill>
        </div>

        <div className="grid grid-cols-3 gap-1.5" role="group" aria-label="Auto-collector cadence">
          {choices.map(([id, label, enabled, secs]) => (
            <button
              key={id}
              type="button"
              data-settings-collector-mode={id}
              aria-pressed={choice === id}
              disabled={updating}
              onClick={() => onSelectSchedule(enabled, secs)}
              className={
                "mono min-h-11 cursor-pointer rounded-md border px-2 text-xs font-medium outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring " +
                (choice === id
                  ? "border-ink bg-ink text-canvas"
                  : "border-hairline bg-surface text-ink-2 hover:bg-canvas hover:text-ink")
              }
            >
              {label}
            </button>
          ))}
        </div>

        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-2 font-mono text-xs">
            <span className="text-ink-2">200-Window Readiness Gate</span>
            <span className="font-semibold text-ink">
              {settled}/{target} ({pct}%)
            </span>
          </div>
          <div
            role="progressbar"
            aria-label="Progress toward 200 settled windows readiness gate"
            aria-valuenow={Math.min(settled, target)}
            aria-valuemin={0}
            aria-valuemax={target}
            className="h-2 w-full overflow-hidden rounded-full bg-track"
          >
            <div
              className={"h-full rounded-full " + (pct >= 100 ? "bg-dot-green" : "bg-blue")}
              style={{ width: `${Math.max(pct, settled > 0 ? 3 : 0)}%` }}
            />
          </div>
          <p className="text-xs text-ink-3">
            {remaining === 0
              ? "Sample depth gate cleared via public read-only tape."
              : `${remaining} more settled windows needed for readiness gate.`}
          </p>
        </div>
      </div>
    );
  }

  const l2Tickers = l2StreamStatus
    ? (l2StreamStatus.active_l2_tickers.length > 0
        ? l2StreamStatus.active_l2_tickers
        : l2StreamStatus.subscribed_tickers)
    : [];
  const effectiveL2Ticker =
    selectedL2Ticker && l2Tickers.includes(selectedL2Ticker)
      ? selectedL2Ticker
      : (l2Tickers[0] ?? "");
  const l2Badge = l2FeedBadge(l2StreamStatus);
  const autoSubscribedCount = isNum(l2StreamStatus?.auto_subscribed_count)
    ? Math.max(0, l2StreamStatus.auto_subscribed_count)
    : 0;
  const effectiveNowMs = isNum(nowMs) ? nowMs : Date.now();
  const rawLastTradeMs = l2StreamStatus?.last_trade_ms;
  const lastTradeAge =
    isNum(rawLastTradeMs) && rawLastTradeMs > 0 && rawLastTradeMs <= 8_640_000_000_000_000
      ? ageWords(Math.max(0, (effectiveNowMs - rawLastTradeMs) / 1000))
      : "none yet";
  const syncHzLabel = formatSyncHz(l2StreamStatus?.tape_sync_rate_hz);

  return (
    <div
      id="auto-collector-strip"
      className="min-w-0 space-y-2.5 rounded-[10px] border border-hairline bg-surface px-4 py-3"
    >
      <div className="flex min-w-0 flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
        {/* Left: Auto-collector cadence toggle */}
        <div className="flex min-w-0 flex-wrap items-center gap-2.5">
          <div className="flex items-center gap-1.5">
            <Clock className="size-4 text-info-foreground" aria-hidden />
            <span className="text-xs font-semibold text-ink">Auto-Collector</span>
          </div>

          <div className="flex items-center gap-1" role="group" aria-label="Background public tape collector cadence">
            {choices.map(([id, label, enabled, secs]) => (
              <button
                key={id}
                type="button"
                data-collector-mode={id}
                aria-pressed={choice === id}
                disabled={updating}
                onClick={() => onSelectSchedule(enabled, secs)}
                className={
                  "mono min-h-11 min-w-11 cursor-pointer rounded-md border px-2.5 text-xs font-medium outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 sm:min-w-0 " +
                  (choice === id
                    ? "border-ink bg-ink text-canvas"
                    : "border-hairline bg-board text-ink-2 hover:bg-surface hover:text-ink")
                }
              >
                {label}
              </button>
            ))}
          </div>

          <StatusPill id="auto-collector-status-pill" tone={badgeTone}>
            <span className="mono">{badgeText}</span>
          </StatusPill>

          {status?.rateLimited && (
            <span className="inline-flex items-center gap-1 text-xs text-warning-foreground">
              <AlertTriangle className="size-3.5" aria-hidden />
              Adaptive 429 backoff active
            </span>
          )}
        </div>

        {/* Right: 200-Window Readiness Gate Progress Bar */}
        <div id="readiness-progress" className="flex min-w-0 flex-1 flex-wrap items-center gap-3 lg:max-w-[460px] lg:justify-end">
          <div className="min-w-[160px] max-w-full flex-1 space-y-1">
            <div className="flex items-center justify-between gap-2 font-mono text-xs">
              <span className="inline-flex items-center gap-1 text-ink-2">
                <ShieldCheck className="size-3.5 text-info-foreground" aria-hidden />
                <span>Readiness Gate</span>
              </span>
              <span className="font-semibold text-ink">
                {settled} / {target} windows ({pct}%)
              </span>
            </div>
            <div
              role="progressbar"
              aria-label="Settled tape progress toward 200-window readiness gate"
              aria-valuenow={Math.min(settled, target)}
              aria-valuemin={0}
              aria-valuemax={target}
              className="h-2 w-full overflow-hidden rounded-full bg-track"
            >
              <div
                className={"h-full rounded-full " + (pct >= 100 ? "bg-dot-green" : "bg-blue")}
                style={{ width: `${Math.max(pct, settled > 0 ? 3 : 0)}%` }}
              />
            </div>
          </div>

          {onOpenScorecard && (
            <button
              type="button"
              onClick={onOpenScorecard}
              className="min-h-11 shrink-0 cursor-pointer rounded-md border border-hairline bg-board px-2.5 text-xs font-medium text-ink-2 outline-none transition-[color,background-color] duration-(--dur-press) hover:bg-surface hover:text-ink focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
            >
              5-Gate View
            </button>
          )}
        </div>
      </div>

      {/* Bottom: Live L2 multi-ticker feed pill & WS/trade telemetry */}
      <div
        id="l2-telemetry-strip"
        data-l2-telemetry-strip
        className="flex min-w-0 flex-col gap-2 border-t border-hairline pt-2.5 lg:flex-row lg:items-center lg:justify-between"
      >
        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1.5 text-xs">
          <span data-l2-feed-pill className="inline-flex">
            <StatusPill
              id="l2-feed-pill"
              tone={l2Badge.tone}
              title={l2StreamStatus?.last_error ?? undefined}
            >
              <Activity className="size-3 shrink-0" strokeWidth={1.5} aria-hidden />
              <span className="mono">{l2Badge.label}</span>
            </StatusPill>
          </span>

          <span data-l2-autosub-count className="mono text-ink-2">
            Auto-sub <strong className="font-semibold text-ink">{autoSubscribedCount}</strong>
          </span>

          <span data-l2-last-trade className="mono text-ink-2">
            Last trade <strong className="font-semibold text-ink">{lastTradeAge}</strong>
          </span>

          <span data-l2-sync-hz className="mono text-ink-2">
            Sync <strong className="font-semibold text-ink">{syncHzLabel}</strong>
          </span>
        </div>

        <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
          <label htmlFor="l2-active-ticker-select" className="shrink-0 text-xs font-medium text-ink-2">
            Active L2
          </label>
          <select
            id="l2-active-ticker-select"
            data-l2-ticker-select
            aria-label="Active L2 tickers"
            value={effectiveL2Ticker}
            disabled={l2Tickers.length === 0}
            onChange={(e) => onSelectL2Ticker?.(e.target.value)}
            className="mono min-h-11 w-full min-w-0 max-w-full cursor-pointer rounded-md border border-ink-3 bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:bg-track disabled:text-ink-2 sm:min-h-8 sm:w-auto sm:max-w-[240px]"
          >
            {l2Tickers.length === 0 ? (
              <option value="">No active tickers</option>
            ) : (
              l2Tickers.map((t) => (
                <option key={t} value={t}>
                  {t}
                </option>
              ))
            )}
          </select>
          {onInspectL2Ticker && effectiveL2Ticker && (
            <button
              type="button"
              id="l2-inspect-ticker-btn"
              onClick={() => onInspectL2Ticker(effectiveL2Ticker)}
              className="min-h-11 shrink-0 cursor-pointer rounded-md border border-hairline bg-board px-2.5 text-xs font-medium text-ink-2 outline-none transition-[color,background-color] duration-(--dur-press) hover:bg-surface hover:text-ink focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
            >
              Inspect L2
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

export function SpotOracleStrip({
  spotStatus,
  featuredMarket,
  selectedAsset = "All",
  onSelectAsset,
}: {
  spotStatus: SpotStatusResponse;
  featuredMarket: Market | null;
  selectedAsset?: string;
  onSelectAsset?: (asset: string) => void;
}) {
  const hasLive = !spotStatus.offline && spotStatus.assets.some((a) => a.price !== null && !a.stale);
  const hasAnyPrice = spotStatus.assets.some((a) => a.price !== null);
  const staleCount = spotStatus.assets.filter((a) => a.stale || a.price === null).length;
  const statusTone = hasLive ? "success" : hasAnyPrice ? "warning" : "neutral";
  const statusLabel = hasLive ? "Spot Live" : hasAnyPrice ? "Spot Stale" : "Spot Offline";

  const featuredAsset = featuredMarket ? seriesShort(featuredMarket.series, featuredMarket.ticker) : "";
  const featuredSpot = spotStatus.assets.find((a) => a.asset === featuredAsset) ?? null;
  const divergence = computeSpotKalshiDivergence(
    featuredSpot?.price ?? null,
    featuredMarket,
    featuredSpot?.vol_bps ?? null,
  );
  const degradedReason =
    !hasAnyPrice
      ? "Spot Oracle offline: external crypto spot feeds unreachable; displaying N/A until reconnected."
      : !hasLive
        ? "Spot Oracle stale: spot feed updates paused or delayed; strike divergence may lag."
        : staleCount > 0
          ? `Spot Oracle degraded: ${staleCount} asset feed${staleCount === 1 ? " is" : "s are"} stale or offline.`
          : null;

  return (
    <div
      id="spot-oracle-strip"
      data-spot-oracle-strip
      className="min-w-0 space-y-2 rounded-[10px] border border-hairline bg-surface px-3.5 py-2.5"
    >
      <div className="flex min-w-0 flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 flex-wrap items-center gap-2">
          <span data-spot-status-pill className="inline-flex">
            <StatusPill id="spot-oracle-status-pill" tone={statusTone}>
              <span>{statusLabel}</span>
            </StatusPill>
          </span>
          <span className="text-xs font-semibold text-ink">Spot Oracle</span>
        </div>

        <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs">
          <span data-spot-strike-readout className="mono tabular-nums text-ink-2">
            Strike <strong className="font-semibold text-ink">{formatSpotUsd(divergence.floorStrike)}</strong>
          </span>
          <span data-spot-divergence-badge className="inline-flex">
            <StatusPill tone={divergence.tone}>
              <span>
                Div <span className="mono tabular-nums font-semibold">{divergence.label}</span>
                {divergence.distanceBps !== null ? (
                  <span className="tabular-nums">{` (${formatBps(divergence.distanceBps)})`}</span>
                ) : null}
              </span>
            </StatusPill>
          </span>
        </div>
      </div>

      {degradedReason && (
        <div
          id="spot-oracle-degraded-banner"
          data-spot-oracle-banner
          role="status"
          className={
            "flex min-w-0 flex-wrap items-center gap-1.5 rounded-md border border-hairline px-2.5 py-1.5 text-xs " +
            (hasAnyPrice ? "bg-pill-warn-bg/40 text-warning-foreground" : "bg-board/70 text-ink-2")
          }
        >
          <AlertTriangle className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
          <span className="min-w-0 break-words">{degradedReason}</span>
        </div>
      )}

      <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-3 lg:grid-cols-5">
        {spotStatus.assets.map((a) => {
          const active = selectedAsset === a.asset;
          const delta = isNum(a.delta_30s_bps) ? a.delta_30s_bps : null;
          const deltaCls =
            delta === null || Math.abs(delta) < 0.05
              ? "text-ink-2"
              : delta > 0
                ? "text-success-foreground"
                : "text-destructive-foreground";
          const sourceLabel = a.price === null ? "N/A" : a.stale ? "stale" : a.source;
          return (
            <button
              key={a.asset}
              type="button"
              data-spot-asset={a.asset}
              aria-pressed={active}
              onClick={() => onSelectAsset?.(a.asset)}
              className={
                "flex min-h-11 min-w-0 cursor-pointer flex-col justify-between gap-0.5 rounded-md border px-2.5 py-1.5 text-left outline-none transition-[background-color,border-color,color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring " +
                (active
                  ? "border-blue/60 bg-blue-soft"
                  : "border-hairline bg-board/60 [@media(hover:hover)_and_(pointer:fine)]:hover:bg-board")
              }
            >
              <div className="flex w-full min-w-0 items-center justify-between gap-1.5">
                <span className="mono shrink-0 text-xs font-semibold text-ink">{a.asset}</span>
                <span data-spot-delta={a.asset} className={`mono min-w-0 truncate text-xs font-medium tabular-nums ${deltaCls}`}>
                  {formatBps(a.delta_30s_bps)}
                </span>
              </div>
              <div className="flex w-full min-w-0 items-center justify-between gap-1.5">
                <span data-spot-price={a.asset} className="mono min-w-0 truncate text-xs font-semibold tabular-nums text-ink">
                  {formatSpotUsd(a.price)}
                </span>
                <span className="min-w-0 truncate text-xs text-ink-2">{sourceLabel}</span>
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
