// Settings Vaul Sheet — the ONLY place Light/Dark Appearance lives (#67), plus Collector cadence, Updates, and Provenance legend.
import { Drawer } from "vaul";
import { Moon, RefreshCw, Sun, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { SegmentedControl, SegmentedControlItem } from "@/components/ui/segmented-control";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { useWide } from "@/lib/media";
import { type Home, ageWords, isNum } from "@/lib/desk";
import { VersionRow, type UpdatesHandle } from "@/Update";

export function SettingsSheet({
  open,
  onOpenChange,
  theme,
  onThemeChange,
  home,
  collectingNow,
  onCollect,
  upd,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  theme: "light" | "dark";
  onThemeChange: (next: "light" | "dark") => void;
  home: Home | null;
  collectingNow: boolean;
  onCollect: () => void;
  upd: UpdatesHandle;
}) {
  const wide = useWide();
  const c = home?.collector;
  const failed = c && isNum(c.series_failed_last_pass) ? c.series_failed_last_pass : 0;
  const failedNames = c && Array.isArray(c.series_failed_names) ? c.series_failed_names : [];
  const dropped = c && isNum(c.dropped_lines) ? c.dropped_lines : 0;
  const conflicts = c && isNum(c.conflicting_settlements) ? c.conflicting_settlements : 0;

  return (
    <Drawer.Root open={open} onOpenChange={onOpenChange} direction={wide ? "right" : "bottom"}>
      <Drawer.Portal>
        <Drawer.Overlay className="fixed inset-0 z-40 bg-ink/30" />
        <Drawer.Content
          id="settings"
          data-settings-drawer
          className={
            wide
              ? "fixed inset-y-0 right-0 z-50 flex w-full max-w-[440px] flex-col border-l border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
              : "fixed inset-x-0 bottom-0 z-50 flex max-h-[88dvh] flex-col rounded-t-[14px] border-t border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
          }
        >
          <div className="flex items-center justify-between gap-3 border-b border-hairline px-5 py-4">
            <div>
              <Drawer.Title className="text-base font-semibold text-ink">Desk Settings</Drawer.Title>
              <Drawer.Description className="text-xs text-ink-2">
                Appearance, live Kalshi collector health, updates, and provenance tags.
              </Drawer.Description>
            </div>
            <Button
              variant="ghost"
              size="icon"
              aria-label="Close settings"
              className="size-11 shrink-0"
              onClick={() => onOpenChange(false)}
            >
              <X className="size-4" strokeWidth={1.75} aria-hidden />
            </Button>
          </div>

          <div className="flex-1 space-y-6 overflow-y-auto px-5 py-5">
            {/* Appearance — Light default + full Dark inside Settings ONLY */}
            <section aria-labelledby="settings-appearance-heading" className="space-y-2.5">
              <div className="flex items-center justify-between gap-2">
                <div>
                  <h3 id="settings-appearance-heading" className="text-sm font-semibold text-ink">
                    Appearance
                  </h3>
                  <p className="text-xs text-ink-2">
                    Light is the default surface; Dark switches every panel, chart, and inspector.
                  </p>
                </div>
              </div>
              <SegmentedControl id="themeBtn" aria-label="Appearance" className="w-full">
                {(["light", "dark"] as const).map((t) => (
                  <SegmentedControlItem
                    key={t}
                    className="!h-11 flex-1 gap-2 !text-ink"
                    aria-pressed={theme === t}
                    data-checked={theme === t ? "" : undefined}
                    onClick={() => onThemeChange(t)}
                  >
                    {t === "light" ? (
                      <Sun className="size-4" strokeWidth={1.75} aria-hidden />
                    ) : (
                      <Moon className="size-4" strokeWidth={1.75} aria-hidden />
                    )}
                    <span>{t === "light" ? "Light" : "Dark"}</span>
                  </SegmentedControlItem>
                ))}
              </SegmentedControl>
            </section>

            {/* Live Kalshi Collector & Cadence */}
            <section aria-labelledby="settings-collector-heading" className="space-y-3 border-t border-hairline pt-5">
              <div className="flex items-center justify-between gap-2">
                <h3 id="settings-collector-heading" className="text-sm font-semibold text-ink">
                  Live Kalshi Collector
                </h3>
                <Button
                  variant="outline"
                  className="min-h-11 gap-2 text-xs"
                  disabled={collectingNow}
                  onClick={onCollect}
                >
                  <RefreshCw className={`size-3.5 ${collectingNow ? "animate-spin" : ""}`} aria-hidden />
                  {collectingNow ? "Collecting…" : "Collect now"}
                </Button>
              </div>
              <dl className="grid grid-cols-2 gap-2.5 rounded-[10px] border border-hairline bg-board p-3 text-xs">
                <div>
                  <dt className="text-ink-3">Status</dt>
                  <dd className="mt-0.5 font-mono font-medium text-ink">
                    {c ? (c.collecting ? "Fresh" : "Stale / idle") : "Unavailable"}
                  </dd>
                </div>
                <div>
                  <dt className="text-ink-3">Cadence</dt>
                  <dd className="mt-0.5 font-mono font-medium text-ink">
                    {c && isNum(c.collect_interval_secs) ? `Every ${c.collect_interval_secs}s` : "-"}
                  </dd>
                </div>
                <div>
                  <dt className="text-ink-3">Snapshots on disk</dt>
                  <dd className="mt-0.5 font-mono font-medium text-ink">
                    {c && isNum(c.snapshots_on_disk) ? c.snapshots_on_disk.toLocaleString() : "-"}
                  </dd>
                </div>
                <div>
                  <dt className="text-ink-3">Newest snapshot</dt>
                  <dd className="mt-0.5 font-mono font-medium text-ink">
                    {c ? ageWords(c.newest_snapshot_age_secs) : "-"}
                  </dd>
                </div>
                <div>
                  <dt className="text-ink-3">Dropped lines</dt>
                  <dd className={`mt-0.5 font-mono font-medium ${dropped > 0 ? "text-warning-foreground" : "text-ink"}`}>
                    {dropped}
                  </dd>
                </div>
                <div>
                  <dt className="text-ink-3">Settlement conflicts</dt>
                  <dd className={`mt-0.5 font-mono font-medium ${conflicts > 0 ? "text-destructive-foreground" : "text-ink"}`}>
                    {conflicts}
                  </dd>
                </div>
              </dl>
              {failed > 0 && (
                <p role="status" className="rounded-md border border-hairline bg-board px-3 py-2 text-xs text-warning-foreground">
                  {failed} series read{failed === 1 ? "" : "s"} failed on the last pass
                  {failedNames.length > 0 ? ` (${failedNames.join(", ")})` : ""}.
                </p>
              )}
            </section>

            {/* Software & Self-Update */}
            <section aria-labelledby="settings-updates-heading" className="space-y-2.5 border-t border-hairline pt-5">
              <h3 id="settings-updates-heading" className="text-sm font-semibold text-ink">
                Software &amp; Updates
              </h3>
              <VersionRow u={upd} />
            </section>

            {/* Provenance & Honesty Legend */}
            <section aria-labelledby="settings-legend-heading" className="space-y-3 border-t border-hairline pt-5">
              <h3 id="settings-legend-heading" className="text-sm font-semibold text-ink">
                How to read this
              </h3>
              <dl className="grid gap-3 text-xs">
                <div className="flex items-start gap-2.5">
                  <dt className="shrink-0">
                    <Pill variant="outline" className="text-info-foreground">
                      <PillStatus className="border-r-0 pr-0">
                        <PillIndicator variant="info" />
                        venue
                      </PillStatus>
                    </Pill>
                  </dt>
                  <dd className="text-ink-2">Public market quotes and settlement results fetched directly from Kalshi.</dd>
                </div>
                <div className="flex items-start gap-2.5">
                  <dt className="shrink-0">
                    <Pill variant="outline" className="text-ink-2">tape</Pill>
                  </dt>
                  <dd className="text-ink-2">Append-only snapshots recorded locally on disk by OpenBell&apos;s collector.</dd>
                </div>
                <div className="flex items-start gap-2.5">
                  <dt className="shrink-0">
                    <Pill variant="outline" className="text-warning-foreground">
                      <PillStatus className="border-r-0 pr-0">
                        <PillIndicator variant="warning" />
                        paper
                      </PillStatus>
                    </Pill>
                  </dt>
                  <dd className="text-ink-2">
                    Shadow compounding simulation scored over collected tape. Paper only: this binary has zero order-placement code.
                  </dd>
                </div>
              </dl>
              <p className="text-xs text-ink-3">
                Anything not obtained reads unavailable, with the reason. Press <kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">⌘K</kbd> for quick jump, <kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">/</kbd> to filter, <kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">1</kbd>–<kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">4</kbd> for tabs, and <kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">j</kbd>/<kbd className="rounded border border-hairline bg-track px-1 py-0.5 font-mono text-ink">k</kbd> to step strategies.
              </p>
            </section>
          </div>
        </Drawer.Content>
      </Drawer.Portal>
    </Drawer.Root>
  );
}
