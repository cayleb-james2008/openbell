// Public Read-Only Auto-Collector Schedule Control Strip (Off / 60s / 180s) + 200-Window Readiness Gate Progress Bar.
// Rendered both inside Live 15m Tape and synchronized inside the Settings Vaul sheet. Strictly public GET only.
import { AlertTriangle, Clock, ShieldCheck } from "lucide-react";
import { StatusPill } from "@/components/ui";
import type { CollectorScheduleStatus } from "@/lib/desk";

type CadenceChoice = "off" | "60" | "180";

function activeChoice(status: CollectorScheduleStatus | null): CadenceChoice {
  if (!status || !status.enabled) return "off";
  return status.intervalSecs >= 120 ? "180" : "60";
}

export function AutoCollectorStrip({
  status,
  updating,
  onSelectSchedule,
  onOpenScorecard,
  variant = "banner",
}: {
  status: CollectorScheduleStatus | null;
  updating: boolean;
  onSelectSchedule: (enabled: boolean, intervalSecs: number) => void;
  onOpenScorecard?: () => void;
  variant?: "banner" | "compact";
}) {
  const choice = activeChoice(status);
  const settled = status?.settledWindows ?? 0;
  const target = status?.readinessTargetWindows ?? 200;
  const pct = Math.min(100, Math.round((settled / Math.max(1, target)) * 100));
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

  return (
    <div
      id="auto-collector-strip"
      className="flex flex-col gap-3 rounded-[10px] border border-hairline bg-surface px-4 py-3 lg:flex-row lg:items-center lg:justify-between"
    >
      {/* Left: Auto-collector cadence toggle */}
      <div className="flex flex-wrap items-center gap-2.5">
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
          <span className="inline-flex items-center gap-1 font-mono text-xs text-warning-foreground">
            <AlertTriangle className="size-3.5" aria-hidden />
            Adaptive 429 backoff active
          </span>
        )}
      </div>

      {/* Right: 200-Window Readiness Gate Progress Bar */}
      <div id="readiness-progress" className="flex flex-1 flex-wrap items-center gap-3 lg:max-w-[460px] lg:justify-end">
        <div className="min-w-[180px] flex-1 space-y-1">
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
  );
}
