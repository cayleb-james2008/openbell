// OpenBell v0.4.0 — Kalshi 15-Minute Quant Research & Paper Compounding Desk.
// Light default + full Dark inside Settings Vaul sheet ONLY (#67); zero fixture badge in live mode (#61, #68).
import { type CSSProperties, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MotionConfig } from "motion/react";
import { Bell, RefreshCw, Search, Settings, X } from "lucide-react";
import { Toaster, toast } from "sonner";
import { AnimatedBackground } from "@/components/ui/animated-background";
import { TextMorph } from "@/components/ui/text-morph";
import { TextShimmer } from "@/components/ui/text-shimmer";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { Kbd } from "@/components/ui/kbd";
import { Tabs, TabsList, TabsPanel, TabsTab } from "@/components/ui/tabs";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { sectionGlide } from "@/motionProps";
import {
  type Dataset,
  type Home,
  type Read,
  ageWords,
  computeStanding,
  isNum,
  useDesk,
} from "@/lib/desk";
import { HeroStrip } from "@/HeroStrip";
import { TapeTab } from "@/TapeTab";
import { StrategiesTab } from "@/StrategiesTab";
import { SlotsTab } from "@/SlotsTab";
import { SettingsSheet } from "@/SettingsSheet";
import { UpdateStrip, useUpdates } from "@/Update";

type TabId = "tape" | "strategies" | "slots";

const ok = <T,>(r: Read<T> | undefined): T | null => (r && r.ok ? r.v : null);

function readDataset(): Dataset {
  return new URLSearchParams(location.search).get("dataset") === "fixture" ? "fixture" : "live";
}

function readInitialTab(): TabId {
  const t = new URLSearchParams(location.search).get("tab");
  if (t === "strategies" || t === "slots" || t === "tape") return t;
  return "tape";
}

function readTheme(): "light" | "dark" {
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

function collectorBadge(home: Home | null, allFailed: boolean): { dot: string; label: string; title: string } {
  if (allFailed) return { dot: "bg-unread", label: "Unavailable", title: "Could not reach the OpenBell server" };
  if (!home || !home.collector) return { dot: "bg-track", label: "Checking", title: "Checking collector status" };
  const c = home.collector;
  const failed = isNum(c.series_failed_last_pass) ? c.series_failed_last_pass : 0;
  const dropped = isNum(c.dropped_lines) ? c.dropped_lines : 0;
  const conflicts = isNum(c.conflicting_settlements) ? c.conflicting_settlements : 0;
  if (c.snapshots_on_disk === 0) {
    return { dot: "bg-dot-amber", label: "Ready to collect", title: "No snapshots on disk yet, click Collect now" };
  }
  const healthy = c.collecting && failed === 0 && dropped === 0 && conflicts === 0;
  const dot = c.collecting ? (healthy ? "bg-dot-green" : "bg-dot-amber") : "bg-unread";
  const label = c.collecting
    ? failed > 0
      ? `Partial (${failed} err)`
      : `Live · ${ageWords(c.newest_snapshot_age_secs)}`
    : `Stale · ${ageWords(c.newest_snapshot_age_secs)}`;
  const title = c.collecting
    ? `Collector active · newest snapshot ${ageWords(c.newest_snapshot_age_secs)}`
    : `Collector stale · newest snapshot ${ageWords(c.newest_snapshot_age_secs)}`;
  return { dot, label, title };
}

function Shell() {
  const [dataset, setDataset] = useState<Dataset>(readDataset);
  const [theme, setTheme] = useState<"light" | "dark">(readTheme);
  const [tab, setTab] = useState<TabId>(readInitialTab);
  const [search, setSearch] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [collectingNow, setCollectingNow] = useState(false);
  const [collectNotice, setCollectNotice] = useState<{ kind: "ok" | "warn" | "err"; text: string } | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const { reads, load } = useDesk(dataset);
  const upd = useUpdates();

  const home = ok(reads?.home);
  const slots = ok(reads?.slots);
  const markets = ok(reads?.markets);
  const strategies = ok(reads?.strategies);
  const standing = useMemo(() => computeStanding(reads?.strategies), [reads?.strategies]);
  const allFailed = !!reads && Object.values(reads).every((r) => !r.ok);
  const failMsg = reads && !reads.home.ok ? reads.home.msg : "";
  const isFixture = dataset === "fixture";
  const versionText = home?.version || upd.version?.version || "0.4.0";

  const pickTheme = (t: "light" | "dark") => {
    document.documentElement.dataset.theme = t;
    try {
      localStorage.setItem("openbell-theme", t);
    } catch {
      /* private mode: preference stays for this session */
    }
    setTheme(t);
  };

  const returnToLive = () => {
    setDataset("live");
    const url = new URL(location.href);
    url.searchParams.delete("dataset");
    history.replaceState(null, "", url);
  };

  const runCollect = useCallback(async () => {
    if (collectingNow) return;
    setCollectingNow(true);
    setCollectNotice(null);
    try {
      const r = await fetch("/api/collect", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
      });
      const data = (await r.json().catch(() => ({}))) as {
        snapshots_written?: number;
        written?: number;
        errors?: string[];
        error?: string;
      };
      if (!r.ok) {
        const msg = "Collection failed. Nothing was written. Try again in a moment.";
        setCollectNotice({ kind: "err", text: msg });
        toast.error(msg);
      } else {
        const n = isNum(data.snapshots_written) ? data.snapshots_written : isNum(data.written) ? data.written : 0;
        const errs = Array.isArray(data.errors) ? data.errors : [];
        if (errs.length > 0) {
          const msg = `Collected ${n.toLocaleString()} snapshots, but ${errs.length} market read${errs.length === 1 ? "" : "s"} failed.`;
          setCollectNotice({ kind: "warn", text: msg });
          toast.warning(msg);
        } else {
          const msg = n > 0
            ? `Collected ${n.toLocaleString()} live snapshots into the tape.`
            : "Collected one pass into the tape.";
          setCollectNotice({ kind: "ok", text: msg });
          toast.success(msg);
        }
        await load();
      }
    } catch {
      const msg = "Could not reach OpenBell to collect. Nothing was written. Try again.";
      setCollectNotice({ kind: "err", text: msg });
      toast.error(msg);
    } finally {
      setCollectingNow(false);
    }
  }, [collectingNow, load]);

  // Global keyboard shortcuts: `/` focuses command filter, `1`/`2`/`3` switch workbench tabs.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      const inField = tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || !!target?.isContentEditable;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "/" && !inField) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (!inField && !settingsOpen) {
        if (e.key === "1") setTab("tape");
        else if (e.key === "2") setTab("strategies");
        else if (e.key === "3") setTab("slots");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [settingsOpen]);

  const badge = collectorBadge(home, allFailed);
  const marketCount = markets ? markets.filter((m) => !!m && typeof m === "object").length : null;
  const strategyCount = strategies ? strategies.filter((s) => !!s && typeof s === "object").length : null;
  const slotCount = slots ? slots.slots.filter((s) => !!s && typeof s === "object").length : null;

  const searchPlaceholder =
    tab === "tape"
      ? "Filter 15m tickers or series…"
      : tab === "strategies"
        ? "Filter Rune strategies or verdicts…"
        : "Filter slots or assigned strategies…";

  return (
    <TooltipProvider>
      <div className="flex min-h-dvh flex-col bg-canvas text-ink">
        {/* Top Command & Live Collector Bar — NO Light/Dark switcher on bar (#67), NO fixture pill in live mode (#61/#68) */}
        <header className="sticky top-0 z-30 border-b border-hairline bg-surface/95 backdrop-blur-xs">
          <div className="mx-auto flex max-w-[1360px] flex-wrap items-center gap-x-3 gap-y-2 px-4 py-2.5 sm:px-6">
            {/* Brand + Paper-only provenance */}
            <div className="mr-auto flex items-center gap-2.5">
              <div className="flex items-center gap-2">
                <span className="inline-flex size-8 items-center justify-center rounded-lg border border-hairline bg-board text-ink">
                  <Bell className="size-4" strokeWidth={1.75} aria-hidden />
                </span>
                <div className="flex items-baseline gap-2">
                  <h1 className="text-base font-semibold tracking-tight text-ink">OpenBell</h1>
                  <span className="mono text-xs text-ink-3">v{versionText}</span>
                </div>
              </div>
              <Pill
                variant="outline"
                className="hidden min-h-7 px-2.5 py-0.5 text-xs text-ink-2 sm:inline-flex"
                title="Paper only: this build places no orders"
              >
                <PillStatus className="border-r-0 pr-0">
                  <PillIndicator variant="info" />
                  Paper-only · No orders
                </PillStatus>
              </Pill>
            </div>

            {/* Global `/` command filter */}
            <div className="order-last w-full sm:order-none sm:w-64 md:w-72">
              <InputGroup className="bg-board/70">
                <InputGroupAddon align="inline-start">
                  <Search className="size-4 text-ink-3" aria-hidden />
                </InputGroupAddon>
                <InputGroupInput
                  ref={searchRef}
                  id="desk-search"
                  type="search"
                  role="searchbox"
                  aria-label="Filter active workbench"
                  placeholder={searchPlaceholder}
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") {
                      if (search) setSearch("");
                      else searchRef.current?.blur();
                    }
                  }}
                />
                {search ? (
                  <InputGroupAddon align="inline-end">
                    <button
                      type="button"
                      aria-label="Clear filter"
                      onClick={() => setSearch("")}
                      className="inline-flex size-8 cursor-pointer items-center justify-center rounded text-ink-2 hover:text-ink"
                    >
                      <X className="size-3.5" aria-hidden />
                    </button>
                  </InputGroupAddon>
                ) : (
                  <InputGroupAddon align="inline-end">
                    <Kbd>/</Kbd>
                  </InputGroupAddon>
                )}
              </InputGroup>
            </div>

            {/* Live collector health + Collect now + Settings trigger */}
            <div className="flex items-center gap-2">
              <Pill
                id="chip"
                variant="outline"
                className="hidden min-h-8 max-w-full px-2.5 py-1 text-xs text-ink-2 md:inline-flex"
                title={badge.title}
              >
                <span id="chip-dot" aria-hidden="true" className={"size-2 shrink-0 rounded-full " + badge.dot} />
                <span id="chip-text" className="min-w-0 truncate">
                  {collectingNow ? (
                    <TextShimmer as="span" className="text-xs font-medium">
                      Collecting live…
                    </TextShimmer>
                  ) : (
                    <TextMorph as="span" className="text-xs font-medium text-ink">
                      {badge.label}
                    </TextMorph>
                  )}
                </span>
              </Pill>

              <Button
                id="collect-btn"
                variant="default"
                className="min-h-11 gap-2 px-3.5 text-xs font-medium sm:min-h-10"
                disabled={collectingNow}
                onClick={runCollect}
              >
                <RefreshCw className={`size-3.5 ${collectingNow ? "animate-spin" : ""}`} aria-hidden />
                <span>{collectingNow ? "Collecting…" : "Collect now"}</span>
              </Button>

              <Button
                id="settings-btn"
                variant="outline"
                size="icon"
                aria-label="Settings"
                className="size-11 shrink-0 sm:size-10"
                onClick={() => setSettingsOpen(true)}
              >
                <Settings className="size-4" strokeWidth={1.75} aria-hidden />
              </Button>
            </div>
          </div>
        </header>

        <main className="mx-auto flex w-full max-w-[1360px] flex-1 flex-col gap-4 px-4 py-4 sm:px-6 sm:py-5">
          {/* Self-update banner (only visible when update available/staged or dismissed pill) */}
          <UpdateStrip u={upd} />

          {/* Example fixture tape banner — NEVER rendered in live mode! */}
          {isFixture && (
            <div
              role="status"
              className="flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface px-4 py-2.5 text-xs"
            >
              <div className="flex flex-wrap items-center gap-2">
                <Pill variant="outline" className="min-h-6 px-2 py-0.5 text-xs text-warning-foreground">
                  <PillStatus className="border-r-0 pr-0">
                    <PillIndicator variant="warning" />
                    Example fixture
                  </PillStatus>
                </Pill>
                <span className="text-ink-2">
                  Example tape. Figures below are illustrative test data, never forward evidence.
                </span>
              </div>
              <Button variant="outline" className="min-h-11 text-xs sm:min-h-9" onClick={returnToLive}>
                Return to live tape
              </Button>
            </div>
          )}

          {/* Inline feedback from Collect now */}
          {collectNotice && (
            <div
              id="collect-status"
              role={collectNotice.kind === "err" ? "alert" : "status"}
              className={
                "flex flex-wrap items-center justify-between gap-2 rounded-[10px] border border-hairline bg-surface px-4 py-2.5 text-xs " +
                (collectNotice.kind === "err"
                  ? "text-destructive-foreground"
                  : collectNotice.kind === "warn"
                    ? "text-warning-foreground"
                    : "text-success-foreground")
              }
            >
              <span>{collectNotice.text}</span>
              <button
                type="button"
                aria-label="Dismiss collector status"
                onClick={() => setCollectNotice(null)}
                className="inline-flex min-h-11 cursor-pointer items-center px-2 text-xs text-ink-2 hover:text-ink sm:min-h-8"
              >
                Dismiss
              </button>
            </div>
          )}

          {/* Complete connectivity error banner */}
          {allFailed && (
            <div
              id="load-error"
              role="alert"
              className="flex flex-wrap items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface px-4 py-3"
            >
              <div>
                <p className="text-sm font-semibold text-destructive-foreground">Could not reach the OpenBell server.</p>
                <p className="mt-0.5 text-xs text-ink-2">{failMsg || "Check that openbell-rs is running, then retry."}</p>
              </div>
              <Button variant="outline" className="min-h-11" onClick={load}>
                Retry
              </Button>
            </div>
          )}

          {/* First-fold Hero Telemetry & Compounding Strip */}
          <HeroStrip
            dataset={dataset}
            homeRead={reads?.home}
            slotsRead={reads?.slots}
            equityRead={reads?.equity}
            marketsRead={reads?.markets}
            standing={standing}
            collectingNow={collectingNow}
          />

          {/* Real Trader Workbench: COSS UI Tabs + Motion Primitives AnimatedBackground glide */}
          <section aria-label="Quant research workbench" className="space-y-3">
            <Tabs value={tab} onValueChange={(v) => setTab(v as TabId)}>
              <div className="flex flex-wrap items-center justify-between gap-3">
                <TabsList variant="plain" className="w-full max-w-full bg-track p-1 sm:w-fit">
                  <AnimatedBackground
                    defaultValue={tab}
                    onValueChange={(v) => {
                      if (v === "tape" || v === "strategies" || v === "slots") setTab(v);
                    }}
                    className="rounded-md bg-raised shadow-(--elev-raised)"
                    {...sectionGlide}
                  >
                    <TabsTab id="tab-tape" value="tape" data-id="tape" className="min-h-11 flex-1 px-2 text-xs sm:min-h-9 sm:flex-none sm:px-3.5 sm:text-sm">
                      <span className="inline-flex items-center gap-1.5 sm:gap-2">
                        <span className="sm:hidden">Tape</span>
                        <span className="hidden sm:inline">Live 15m Tape</span>
                        {marketCount !== null && (
                          <Badge variant="outline" className="mono text-xs">
                            {marketCount}
                          </Badge>
                        )}
                      </span>
                    </TabsTab>
                    <TabsTab
                      id="tab-strategies"
                      value="strategies"
                      data-id="strategies"
                      className="min-h-11 flex-1 px-2 text-xs sm:min-h-9 sm:flex-none sm:px-3.5 sm:text-sm"
                    >
                      <span className="inline-flex items-center gap-1.5 sm:gap-2">
                        <span className="sm:hidden">Strategies</span>
                        <span className="hidden sm:inline">Rune Strategies</span>
                        {strategyCount !== null && (
                          <Badge variant="outline" className="mono text-xs">
                            {strategyCount}
                          </Badge>
                        )}
                      </span>
                    </TabsTab>
                    <TabsTab id="tab-slots" value="slots" data-id="slots" className="min-h-11 flex-1 px-2 text-xs sm:min-h-9 sm:flex-none sm:px-3.5 sm:text-sm">
                      <span className="inline-flex items-center gap-1.5 sm:gap-2">
                        <span className="sm:hidden">Slots</span>
                        <span className="hidden sm:inline">Slot Desk</span>
                        {slotCount !== null && (
                          <Badge variant="outline" className="mono text-xs">
                            {slotCount}
                          </Badge>
                        )}
                      </span>
                    </TabsTab>
                  </AnimatedBackground>
                </TabsList>

                <div className="hidden items-center gap-2 text-xs text-ink-3 md:flex">
                  <span>Shortcuts:</span>
                  <Kbd>/</Kbd>
                  <span>filter</span>
                  <Kbd>1</Kbd>
                  <Kbd>2</Kbd>
                  <Kbd>3</Kbd>
                  <span>tabs</span>
                </div>
              </div>

              <TabsPanel value="tape" className="mt-3">
                <TapeTab
                  marketsRead={reads?.markets}
                  dataset={dataset}
                  search={search}
                  onRetry={load}
                  onCollect={runCollect}
                  collectingNow={collectingNow}
                />
              </TabsPanel>

              <TabsPanel value="strategies" className="mt-3">
                <StrategiesTab
                  strategiesRead={reads?.strategies}
                  slotsRead={reads?.slots}
                  equityRead={reads?.equity}
                  search={search}
                  onRetry={load}
                  isActiveTab={tab === "strategies"}
                />
              </TabsPanel>

              <TabsPanel value="slots" className="mt-3">
                <SlotsTab
                  slotsRead={reads?.slots}
                  homeRead={reads?.home}
                  reportRead={reads?.report}
                  search={search}
                  onRetry={load}
                />
              </TabsPanel>
            </Tabs>
          </section>
        </main>

        <footer className="mt-auto border-t border-hairline bg-surface">
          <div className="mx-auto flex max-w-[1360px] flex-wrap items-center justify-between gap-2 px-4 py-3 text-xs text-ink-2 sm:px-6">
            <span className="mono">OpenBell v{versionText} · Paper-only Kalshi 15m research desk · Zero order paths</span>
            <button
              type="button"
              onClick={() => setSettingsOpen(true)}
              className="inline-flex min-h-11 cursor-pointer items-center rounded-md px-2 text-xs font-medium text-ink underline-offset-4 hover:underline sm:min-h-8"
            >
              Settings, Appearance &amp; Updates
            </button>
          </div>
        </footer>

        <SettingsSheet
          open={settingsOpen}
          onOpenChange={setSettingsOpen}
          theme={theme}
          onThemeChange={pickTheme}
          home={home}
          collectingNow={collectingNow}
          onCollect={runCollect}
          upd={upd}
        />

        <Toaster
          position="bottom-right"
          toastOptions={{
            style: {
              background: "var(--surface)",
              color: "var(--ink)",
              border: "1px solid var(--hairline)",
            } as CSSProperties,
          }}
        />
      </div>
    </TooltipProvider>
  );
}

export function App() {
  return (
    <MotionConfig reducedMotion="user">
      <Shell />
    </MotionConfig>
  );
}

export default App;
