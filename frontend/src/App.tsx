// OpenBell v0.8.0 — Live Kalshi Connected Trading & 15-Minute Quant Desk.
// Light default + full Dark inside Settings Vaul sheet ONLY (#67); opaque sticky header (zero backdrop-blur);
// Live Kalshi v2 account/positions/orders/fills + Hard Risk Rails + Emergency Kill Switch + 4-tab workbench + Cmd+K.
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
  type CollectorScheduleStatus,
  type Dataset,
  type Home,
  type Market,
  type Read,
  type Slot,
  type Strategy,
  ageWords,
  computeStanding,
  exportDailyReport,
  fetchCollectorStatus,
  isNum,
  setCollectorSchedule,
  useDesk,
} from "@/lib/desk";
import {
  type LiveConfigPayload,
  type LiveDeskBundle,
  type LiveOrderPayload,
  armLiveTrading,
  cancelLiveOrder,
  defaultLiveStatus,
  disarmLiveTrading,
  fetchLiveBundle,
  saveLiveConfig,
  submitLiveOrder,
  triggerKillSwitch,
} from "@/lib/live";
import { HeroStrip } from "@/HeroStrip";
import { LiveAccountStrip, LiveDeskWorkspace } from "@/LiveTradingPanel";
import { TapeTab } from "@/TapeTab";
import { StrategiesTab } from "@/StrategiesTab";
import { SlotsTab } from "@/SlotsTab";
import { QuantLabTab, type QuantSectionId } from "@/QuantLabTab";
import { CommandPalette, type DeskTabId } from "@/CommandPalette";
import { SettingsSheet } from "@/SettingsSheet";
import { UpdateStrip, useUpdates } from "@/Update";

const ok = <T,>(r: Read<T> | undefined): T | null => (r && r.ok ? r.v : null);

function readDataset(): Dataset {
  return new URLSearchParams(location.search).get("dataset") === "fixture" ? "fixture" : "live";
}

function readInitialTab(): DeskTabId {
  const t = new URLSearchParams(location.search).get("tab");
  if (t === "strategies" || t === "slots" || t === "tape" || t === "quant") return t;
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
  const [tab, setTab] = useState<DeskTabId>(readInitialTab);
  const [search, setSearch] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [cmdOpen, setCmdOpen] = useState(false);
  const [collectingNow, setCollectingNow] = useState(false);
  const [collectNotice, setCollectNotice] = useState<{ kind: "ok" | "warn" | "err"; text: string } | null>(null);
  const [jumpTicker, setJumpTicker] = useState<string | null>(null);
  const [jumpStrategy, setJumpStrategy] = useState<string | null>(null);
  const [sandboxSignal, setSandboxSignal] = useState(0);
  const [quantFocus, setQuantFocus] = useState<QuantSectionId | null>(null);
  const [reseedSignal, setReseedSignal] = useState(0);
  const [collectorStatus, setCollectorStatus] = useState<CollectorScheduleStatus | null>(null);
  const [updatingSchedule, setUpdatingSchedule] = useState(false);
  const [liveBundle, setLiveBundle] = useState<LiveDeskBundle>(() => ({
    status: defaultLiveStatus("client"),
    account: {
      configured: false,
      connected: false,
      balanceDollars: null,
      portfolioValueDollars: null,
      dailyPnlDollars: null,
      note: "Configure Kalshi v2 API credentials in Desk Settings to connect.",
    },
    positions: [],
    orders: [],
    fills: [],
  }));
  const [liveBusy, setLiveBusy] = useState(false);
  const [savingLiveConfig, setSavingLiveConfig] = useState(false);
  const [liveWorkspaceOpen, setLiveWorkspaceOpen] = useState(false);
  const [ticketTicker, setTicketTicker] = useState<string | null>(null);
  const [ticketPriceCents, setTicketPriceCents] = useState<number | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const { reads, load, stream } = useDesk(dataset);
  const upd = useUpdates();

  const home = ok(reads?.home);
  const slots = ok(reads?.slots);
  const markets = ok(reads?.markets);
  const strategies = ok(reads?.strategies);
  const reportText = ok(reads?.report)?.report || "";
  const standing = useMemo(() => computeStanding(reads?.strategies), [reads?.strategies]);
  const allFailed = !!reads && Object.values(reads).every((r) => !r.ok);
  const failMsg = reads && !reads.home.ok ? reads.home.msg : "";
  const isFixture = dataset === "fixture";
  const versionText = home?.version || upd.version?.version || "0.8.0";

  const validMarkets = useMemo(
    () => (markets ? markets.filter((m): m is Market => !!m && typeof m === "object") : []),
    [markets],
  );
  const validStrategies = useMemo(
    () => (strategies ? strategies.filter((s): s is Strategy => !!s && typeof s === "object") : []),
    [strategies],
  );
  const validSlots = useMemo(
    () => (slots ? slots.slots.filter((s): s is Slot => !!s && typeof s === "object") : []),
    [slots],
  );

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

  const handlePaletteExport = useCallback(
    async (format: "md" | "csv") => {
      try {
        const res = await exportDailyReport(format, dataset, home, reportText, slots);
        toast.success(`Exported ${res.filename} (${res.bytes.toLocaleString()} bytes).`);
      } catch (e) {
        toast.error(`Export failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [dataset, home, reportText, slots],
  );

  // Global keyboard shortcuts: Cmd/Ctrl+K opens Command Palette, `/` focuses filter, `1`-`4` switch tabs.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCmdOpen((v) => !v);
        return;
      }
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      const inField = tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || !!target?.isContentEditable;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "/" && !inField && !cmdOpen && !settingsOpen) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (!inField && !settingsOpen && !cmdOpen) {
        if (e.key === "1") setTab("tape");
        else if (e.key === "2") setTab("strategies");
        else if (e.key === "3") setTab("slots");
        else if (e.key === "4") setTab("quant");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [settingsOpen, cmdOpen]);

  const badge = collectorBadge(home, allFailed);
  const marketCount = markets ? validMarkets.length : null;
  const strategyCount = strategies ? validStrategies.length : null;
  const slotCount = slots ? validSlots.length : null;
  const settledCount = useMemo(
    () => validMarkets.filter((m) => m.result === "yes" || m.result === "no").length,
    [validMarkets],
  );

  const refreshCollectorStatus = useCallback(async () => {
    const next = await fetchCollectorStatus(home, settledCount);
    setCollectorStatus(next);
  }, [home, settledCount]);

  useEffect(() => {
    void refreshCollectorStatus();
  }, [refreshCollectorStatus]);

  const refreshLive = useCallback(async () => {
    const next = await fetchLiveBundle(home);
    setLiveBundle(next);
  }, [home]);

  useEffect(() => {
    void refreshLive();
  }, [refreshLive]);

  const handleSaveLiveConfig = useCallback(
    async (payload: LiveConfigPayload) => {
      setSavingLiveConfig(true);
      try {
        const nextStatus = await saveLiveConfig(payload, home);
        setLiveBundle((prev) => ({ ...prev, status: nextStatus }));
        toast.success(`Saved Kalshi ${nextStatus.environment.toUpperCase()} API & hard risk rails.`);
        await refreshLive();
      } catch (e) {
        toast.error(`Could not save Kalshi config: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setSavingLiveConfig(false);
      }
    },
    [home, refreshLive],
  );

  const handleArmLive = useCallback(async () => {
    setLiveBusy(true);
    try {
      const nextStatus = await armLiveTrading(home);
      setLiveBundle((prev) => ({ ...prev, status: nextStatus }));
      setLiveWorkspaceOpen(true);
      toast.warning(`Live Kalshi ${nextStatus.environment.toUpperCase()} trading ARMED with hard risk rails.`);
      await refreshLive();
    } catch (e) {
      toast.error(`Arm failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLiveBusy(false);
    }
  }, [home, refreshLive]);

  const handleDisarmLive = useCallback(async () => {
    setLiveBusy(true);
    try {
      const nextStatus = await disarmLiveTrading(home);
      setLiveBundle((prev) => ({ ...prev, status: nextStatus }));
      toast.success("Returned to Paper Mode (live order routing disarmed).");
      await refreshLive();
    } catch (e) {
      toast.error(`Disarm failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLiveBusy(false);
    }
  }, [home, refreshLive]);

  const handleSubmitLiveOrder = useCallback(
    async (payload: LiveOrderPayload) => {
      setLiveBusy(true);
      try {
        const res = await submitLiveOrder(payload, home);
        toast.success(
          `Order ${res.orderId} submitted (${payload.action.toUpperCase()} ${payload.count} ${payload.side.toUpperCase()} @ ${payload.price_cents}c).`,
        );
        await refreshLive();
      } catch (e) {
        toast.error(`Order blocked: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setLiveBusy(false);
      }
    },
    [home, refreshLive],
  );

  const handleCancelLiveOrder = useCallback(
    async (orderId: string) => {
      setLiveBusy(true);
      try {
        await cancelLiveOrder(orderId, home);
        toast.success(`Cancelled resting order ${orderId}.`);
        await refreshLive();
      } catch (e) {
        toast.error(`Cancel failed: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setLiveBusy(false);
      }
    },
    [home, refreshLive],
  );

  const handleKillSwitch = useCallback(async () => {
    setLiveBusy(true);
    try {
      const res = await triggerKillSwitch(home);
      setLiveBundle((prev) => ({ ...prev, status: res.status }));
      toast.error(
        `Emergency Kill Switch triggered — disarmed live trading and cancelled ${res.cancelledCount} resting order${res.cancelledCount === 1 ? "" : "s"}.`,
      );
      await refreshLive();
    } catch (e) {
      toast.error(`Kill switch error: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLiveBusy(false);
    }
  }, [home, refreshLive]);

  const handleSelectSchedule = useCallback(
    async (enabled: boolean, intervalSecs: number) => {
      setUpdatingSchedule(true);
      try {
        const next = await setCollectorSchedule(enabled, intervalSecs, home, settledCount);
        setCollectorStatus(next);
        toast.success(
          enabled
            ? `Auto-collector set to every ${next.intervalSecs}s (public read-only GET).`
            : "Background auto-collector paused.",
        );
      } catch (e) {
        toast.error(`Could not update auto-collector: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setUpdatingSchedule(false);
      }
    },
    [home, settledCount],
  );

  const searchPlaceholder =
    tab === "tape"
      ? "Filter 15m tickers…"
      : tab === "strategies"
        ? "Filter strategies…"
        : tab === "slots"
          ? "Filter slots…"
          : "Filter Quant Lab…";

  return (
    <TooltipProvider>
      <div className="flex min-h-dvh flex-col bg-canvas text-ink">
        {/* Opaque Sticky Command & Live Collector Bar — ZERO backdrop-blur, NO Light/Dark switch (#67) */}
        <header className="sticky top-0 z-30 border-b border-hairline bg-surface">
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
                id="live-mode-pill"
                variant="outline"
                className={
                  "hidden min-h-7 px-2.5 py-0.5 text-xs sm:inline-flex " +
                  (liveBundle.status.liveArmed
                    ? liveBundle.status.environment === "prod"
                      ? "border-dot-red text-destructive-foreground"
                      : "border-dot-amber text-warning-foreground"
                    : "text-ink-2")
                }
                title={
                  liveBundle.status.liveArmed
                    ? `Live Kalshi ${liveBundle.status.environment.toUpperCase()} order routing ARMED with hard risk rails`
                    : "Kalshi v2 connected desk · Disarmed (Paper Mode)"
                }
              >
                <PillStatus className="border-r-0 pr-0">
                  <PillIndicator
                    variant={
                      liveBundle.status.killed
                        ? "error"
                        : liveBundle.status.liveArmed
                          ? liveBundle.status.environment === "prod"
                            ? "error"
                            : "warning"
                          : "info"
                    }
                  />
                  {liveBundle.status.liveArmed
                    ? `Kalshi ${liveBundle.status.environment.toUpperCase()} · Live Armed`
                    : "Kalshi v2 · Paper Mode"}
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

          {/* Live Kalshi v2 Connected Account Strip, Paper/Live Switcher & Emergency Kill Switch */}
          <LiveAccountStrip
            bundle={liveBundle}
            busy={liveBusy}
            workspaceOpen={liveWorkspaceOpen}
            onToggleWorkspace={() => setLiveWorkspaceOpen((v) => !v)}
            onArmConfirm={() => void handleArmLive()}
            onDisarm={() => void handleDisarmLive()}
            onKillSwitch={() => void handleKillSwitch()}
            onRefresh={() => void refreshLive()}
            onOpenSettings={() => setSettingsOpen(true)}
          />

          {liveWorkspaceOpen && (
            <LiveDeskWorkspace
              bundle={liveBundle}
              markets={validMarkets}
              prefillTicker={ticketTicker}
              prefillPriceCents={ticketPriceCents}
              busy={liveBusy}
              onSubmitOrder={handleSubmitLiveOrder}
              onCancelOrder={handleCancelLiveOrder}
              onRequestArm={() => {
                document.getElementById("trading-mode-live")?.click();
              }}
              onOpenSettings={() => setSettingsOpen(true)}
            />
          )}

          {/* 4-Tab Institutional Quant Workbench: COSS UI Tabs + Motion Primitives AnimatedBackground */}
          <section aria-label="Quant research workbench" className="space-y-3">
            <Tabs value={tab} onValueChange={(v) => setTab(v as DeskTabId)}>
              <div className="flex flex-wrap items-center justify-between gap-3">
                <TabsList variant="plain" className="w-full max-w-full bg-track p-1 sm:w-fit">
                  <AnimatedBackground
                    defaultValue={tab}
                    onValueChange={(v) => {
                      if (v === "tape" || v === "strategies" || v === "slots" || v === "quant") setTab(v);
                    }}
                    className="rounded-md bg-raised shadow-(--elev-raised)"
                    {...sectionGlide}
                  >
                    <TabsTab
                      id="tab-tape"
                      value="tape"
                      data-id="tape"
                      className="min-h-11 flex-1 px-2 text-xs sm:min-h-9 sm:flex-none sm:px-3.5 sm:text-sm"
                    >
                      <span className="inline-flex items-center gap-1.5 sm:gap-2">
                        <span className="sm:hidden">Tape</span>
                        <span className="hidden sm:inline">Live 15m Tape</span>
                        {marketCount !== null && (
                          <Badge variant="outline" className="mono hidden text-xs sm:inline-flex">
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
                        <span className="hidden sm:inline">Strategies</span>
                        {strategyCount !== null && (
                          <Badge variant="outline" className="mono hidden text-xs sm:inline-flex">
                            {strategyCount}
                          </Badge>
                        )}
                      </span>
                    </TabsTab>
                    <TabsTab
                      id="tab-slots"
                      value="slots"
                      data-id="slots"
                      className="min-h-11 flex-1 px-2 text-xs sm:min-h-9 sm:flex-none sm:px-3.5 sm:text-sm"
                    >
                      <span className="inline-flex items-center gap-1.5 sm:gap-2">
                        <span className="sm:hidden">Slots</span>
                        <span className="hidden sm:inline">Slot Desk</span>
                        {slotCount !== null && (
                          <Badge variant="outline" className="mono hidden text-xs sm:inline-flex">
                            {slotCount}
                          </Badge>
                        )}
                      </span>
                    </TabsTab>
                    <TabsTab
                      id="tab-quant"
                      value="quant"
                      data-id="quant"
                      className="min-h-11 flex-1 px-2 text-xs sm:min-h-9 sm:flex-none sm:px-3.5 sm:text-sm"
                    >
                      <span className="inline-flex items-center gap-1.5 sm:gap-2">
                        <span className="sm:hidden">Quant</span>
                        <span className="hidden sm:inline">Quant Lab</span>
                        {settledCount > 0 && (
                          <Badge variant="outline" className="mono hidden text-xs sm:inline-flex">
                            {settledCount}
                          </Badge>
                        )}
                      </span>
                    </TabsTab>
                  </AnimatedBackground>
                </TabsList>

                <div className="flex items-center gap-2 text-xs text-ink-3">
                  <Button
                    id="cmdk-btn"
                    variant="outline"
                    className="min-h-11 gap-1.5 px-2.5 text-xs text-ink-2 sm:min-h-9"
                    aria-label="Open command palette"
                    onClick={() => setCmdOpen(true)}
                  >
                    <Search className="size-3.5" aria-hidden />
                    <span>Jump</span>
                    <Kbd>⌘K</Kbd>
                  </Button>
                  <div className="hidden items-center gap-1.5 lg:flex">
                    <Kbd>/</Kbd>
                    <span>filter</span>
                    <Kbd>1</Kbd>
                    <Kbd>2</Kbd>
                    <Kbd>3</Kbd>
                    <Kbd>4</Kbd>
                    <span>tabs</span>
                  </div>
                </div>
              </div>

              <TabsPanel value="tape" className="mt-3">
                <TapeTab
                  marketsRead={reads?.markets}
                  home={home}
                  dataset={dataset}
                  search={search}
                  stream={stream}
                  collectorStatus={collectorStatus}
                  updatingSchedule={updatingSchedule}
                  onSelectSchedule={(en, sec) => void handleSelectSchedule(en, sec)}
                  onOpenScorecard={() => {
                    setTab("quant");
                    setQuantFocus("scorecard");
                  }}
                  onTradeMarket={(ticker, askCents) => {
                    setTicketTicker(ticker);
                    setTicketPriceCents(askCents);
                    setLiveWorkspaceOpen(true);
                    document.getElementById("live-account-strip")?.scrollIntoView({ behavior: "smooth", block: "start" });
                  }}
                  onRetry={load}
                  onCollect={runCollect}
                  collectingNow={collectingNow}
                  jumpTicker={jumpTicker}
                  onClearJump={() => setJumpTicker(null)}
                />
              </TabsPanel>

              <TabsPanel value="strategies" className="mt-3">
                <StrategiesTab
                  strategiesRead={reads?.strategies}
                  slotsRead={reads?.slots}
                  equityRead={reads?.equity}
                  marketsRead={reads?.markets}
                  home={home}
                  dataset={dataset}
                  search={search}
                  onRetry={load}
                  isActiveTab={tab === "strategies"}
                  jumpStrategy={jumpStrategy}
                  sandboxSignal={sandboxSignal}
                  onClearJump={() => setJumpStrategy(null)}
                />
              </TabsPanel>

              <TabsPanel value="slots" className="mt-3">
                <SlotsTab
                  slotsRead={reads?.slots}
                  equityRead={reads?.equity}
                  homeRead={reads?.home}
                  reportRead={reads?.report}
                  liveBundle={liveBundle}
                  onOpenOrderTicket={() => {
                    setLiveWorkspaceOpen(true);
                    document.getElementById("live-account-strip")?.scrollIntoView({ behavior: "smooth", block: "start" });
                  }}
                  onOpenSettings={() => setSettingsOpen(true)}
                  dataset={dataset}
                  search={search}
                  onRetry={load}
                />
              </TabsPanel>

              <TabsPanel value="quant" className="mt-3">
                <QuantLabTab
                  dataset={dataset}
                  home={home}
                  marketsRead={reads?.markets}
                  slotsRead={reads?.slots}
                  strategiesRead={reads?.strategies}
                  search={search}
                  focusSection={quantFocus}
                  reseedSignal={reseedSignal}
                  onClearFocus={() => setQuantFocus(null)}
                />
              </TabsPanel>
            </Tabs>
          </section>
        </main>

        <footer className="mt-auto border-t border-hairline bg-surface">
          <div className="mx-auto flex max-w-[1360px] flex-wrap items-center justify-between gap-2 px-4 py-3 text-xs text-ink-2 sm:px-6">
            <span className="mono">OpenBell v{versionText} · Live Kalshi Connected Trading &amp; Quant Desk · Hard Risk Rails</span>
            <button
              type="button"
              onClick={() => setSettingsOpen(true)}
              className="inline-flex min-h-11 cursor-pointer items-center rounded-md px-2 text-xs font-medium text-ink underline-offset-4 hover:underline sm:min-h-8"
            >
              Settings, API Keys &amp; Risk Rails
            </button>
          </div>
        </footer>

        <CommandPalette
          open={cmdOpen}
          onOpenChange={setCmdOpen}
          markets={validMarkets}
          strategies={validStrategies}
          slots={validSlots}
          onSelectTab={(t) => setTab(t)}
          onJumpMarket={(ticker) => {
            setSearch("");
            setTab("tape");
            setJumpTicker(ticker);
          }}
          onJumpStrategy={(name) => {
            setSearch("");
            setTab("strategies");
            setJumpStrategy(name);
          }}
          onOpenSandbox={() => {
            setTab("strategies");
            setSandboxSignal((n) => n + 1);
          }}
          onJumpSlot={(_slotNum, strategyName) => {
            setTab("slots");
            setSearch(strategyName);
          }}
          onJumpQuantSection={(sec) => {
            setTab("quant");
            setQuantFocus(sec);
          }}
          onCollect={() => void runCollect()}
          onExportReport={(fmt) => void handlePaletteExport(fmt)}
          onReseedMonteCarlo={() => {
            setTab("quant");
            setQuantFocus("montecarlo");
            setReseedSignal((n) => n + 1);
          }}
          onOpenOrderTicket={() => {
            setLiveWorkspaceOpen(true);
            document.getElementById("live-account-strip")?.scrollIntoView({ behavior: "smooth", block: "start" });
          }}
          onKillSwitch={() => void handleKillSwitch()}
          onOpenSettings={() => setSettingsOpen(true)}
        />

        <SettingsSheet
          open={settingsOpen}
          onOpenChange={setSettingsOpen}
          theme={theme}
          onThemeChange={pickTheme}
          home={home}
          collectorStatus={collectorStatus}
          updatingSchedule={updatingSchedule}
          onSelectSchedule={(en, sec) => void handleSelectSchedule(en, sec)}
          liveStatus={liveBundle.status}
          savingLiveConfig={savingLiveConfig}
          onSaveLiveConfig={handleSaveLiveConfig}
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
