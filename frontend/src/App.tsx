// OpenBell Desktop Quant Workbench shell (v0.9.0):
// Persistent quiet Risk / Kill-Switch header strip (fail-closed DISARMED badge + 1-click Kill Switch),
// slim left desk rail (15m Tape, Strategies, Paper Slots, Quant Lab & Tournament, Live & Rails),
// 1-click Full Quant Edge Audit trigger, and collapsible Order Ticket & Strategy Rule Editor drawers.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Drawer } from "vaul";
import { Toaster, toast } from "sonner";
import {
  Activity,
  AlertOctagon,
  Code2,
  FlaskConical,
  Layers,
  ListChecks,
  Lock,
  RefreshCw,
  Search,
  Settings,
  ShieldAlert,
  SlidersHorizontal,
  Sparkles,
  Unlock,
  X,
} from "lucide-react";
import { Badge, Button, Kbd, StatusPill } from "@/components/ui";
import { CommandPalette, type DeskTabId } from "@/CommandPalette";
import {
  ArmLiveModal,
  LiveAccountStrip,
  LiveDeskWorkspace,
} from "@/LiveTradingPanel";
import { QuantLabTab, type QuantSectionId } from "@/QuantLabTab";
import { RuneSandbox } from "@/RuneSandbox";
import { SettingsSheet } from "@/SettingsSheet";
import { SlotsTab } from "@/SlotsTab";
import { StrategiesTab } from "@/StrategiesTab";
import { TapeTab } from "@/TapeTab";
import { UpdateStrip, useUpdates } from "@/Update";
import {
  type CollectorScheduleStatus,
  type Dataset,
  type Home,
  ageWords,
  computeStanding,
  exportDailyReport,
  fetchCollectorStatus,
  isNum,
  marketIsOpen,
  money,
  moneySigned,
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
import { useWide } from "@/lib/media";

function initialDataset(): Dataset {
  const qs = new URLSearchParams(location.search);
  return qs.get("dataset") === "fixture" ? "fixture" : "live";
}

function collectorText(home: Home | null): { fresh: boolean; label: string; detail: string } {
  const c = home?.collector;
  if (!c) {
    return {
      fresh: false,
      label: "Collector idle",
      detail: "Collector unavailable on this view",
    };
  }
  const fresh = !!c.collecting;
  const age = ageWords(c.newest_snapshot_age_secs);
  const failed = isNum(c.series_failed_last_pass) ? c.series_failed_last_pass : 0;
  const failNote = failed > 0 ? ` (${failed} market read${failed === 1 ? "" : "s"} failed)` : "";
  return {
    fresh,
    label: fresh ? `Live (${age})` : `Stale (${age})`,
    detail: (fresh ? `Collector active, newest snapshot ${age}` : `Collector stale, newest snapshot ${age}`) + failNote,
  };
}

function initialLiveBundle(): LiveDeskBundle {
  return {
    status: defaultLiveStatus("client"),
    account: {
      configured: false,
      connected: false,
      balanceDollars: null,
      portfolioValueDollars: null,
      dailyPnlDollars: null,
      note: "Checking live desk state...",
    },
    positions: [],
    orders: [],
    fills: [],
  };
}

export function App() {
  const wide = useWide();
  const [dataset, setDataset] = useState<Dataset>(initialDataset);
  const [theme, setTheme] = useState<"light" | "dark">(() =>
    localStorage.getItem("openbell-theme") === "dark" ? "dark" : "light",
  );
  const [tab, setTab] = useState<DeskTabId>("tape");
  const [search, setSearch] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [cmdOpen, setCmdOpen] = useState(false);
  const [collectingNow, setCollectingNow] = useState(false);
  const [collectNotice, setCollectNotice] = useState("");
  const [jumpTicker, setJumpTicker] = useState<string | null>(null);
  const [jumpStrategy, setJumpStrategy] = useState<string | null>(null);
  const [sandboxSignal, setSandboxSignal] = useState(0);
  const [quantFocus, setQuantFocus] = useState<QuantSectionId | null>(null);
  const [reseedSignal, setReseedSignal] = useState(0);
  const [auditSignal, setAuditSignal] = useState(0);
  const [collectorStatus, setCollectorStatus] = useState<CollectorScheduleStatus | null>(null);
  const [updatingSchedule, setUpdatingSchedule] = useState(false);

  // Live Kalshi v2 trading state (FAIL-CLOSED DISARMED by default)
  const [liveBundle, setLiveBundle] = useState<LiveDeskBundle>(initialLiveBundle);
  const [liveBusy, setLiveBusy] = useState(false);
  const [savingLiveConfig, setSavingLiveConfig] = useState(false);
  const [armModalOpen, setArmModalOpen] = useState(false);

  // Collapsible workbench drawers
  const [ticketDrawerOpen, setTicketDrawerOpen] = useState(false);
  const [editorDrawerOpen, setEditorDrawerOpen] = useState(false);
  const [editorInitStrategy, setEditorInitStrategy] = useState<string | null>(null);
  const [ticketTicker, setTicketTicker] = useState<string | null>(null);
  const [ticketPriceCents, setTicketPriceCents] = useState<number | null>(null);

  const searchRef = useRef<HTMLInputElement>(null);
  const { reads, stream, load: reload } = useDesk(dataset);
  const upd = useUpdates();

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("openbell-theme", theme);
  }, [theme]);

  const home = reads?.home?.ok ? reads.home.v : null;
  const slots = reads?.slots?.ok ? reads.slots.v : null;
  const markets = useMemo(
    () => (reads?.markets?.ok ? reads.markets.v.filter((m) => !!m && typeof m === "object") : []),
    [reads?.markets],
  );
  const strategies = useMemo(
    () => (reads?.strategies?.ok ? reads.strategies.v.filter((s) => !!s && typeof s === "object") : []),
    [reads?.strategies],
  );

  const openMarketsCount = useMemo(() => markets.filter(marketIsOpen).length, [markets]);
  const settledMarketsCount = Math.max(0, markets.length - openMarketsCount);
  const standing = useMemo(() => computeStanding(reads?.strategies), [reads?.strategies]);
  const collector = collectorText(home);
  const versionText = home?.version || upd.version?.version || "0.9.0";

  const potValue = slots && isNum(slots.paper_equity_dollars) ? slots.paper_equity_dollars : null;
  const potStart = slots && isNum(slots.paper_pot_start) ? slots.paper_pot_start : 100;
  const potDelta = potValue !== null ? potValue - potStart : null;
  const potStage =
    home?.mode === "example_only"
      ? "Example only"
      : home?.mode === "collecting"
        ? "Collecting"
        : "Paper simulation";

  const standingTitle =
    standing.state === "loading"
      ? "Strategies"
      : standing.state === "error"
        ? "Strategy state unavailable"
        : standing.state === "empty"
          ? "No strategies in the library"
          : standing.provenForward > 0
            ? "Forward proven"
            : "Evidence standing";
  const standingFigure =
    standing.state === "loading"
      ? "Loading..."
      : standing.state === "error"
        ? "Unavailable"
        : `${standing.provenForward} of ${standing.total}`;
  const standingLines =
    standing.lines.length > 0
      ? standing.lines
      : [standing.commonMinWindows ? `Target ${standing.commonMinWindows} windows` : "Collecting OOS windows"];

  const anyReadError = useMemo(() => {
    const entries: Array<[string, { ok: boolean; msg?: string } | undefined]> = [
      ["home", reads?.home],
      ["slots", reads?.slots],
      ["markets", reads?.markets],
      ["strategies", reads?.strategies],
    ];
    for (const [k, r] of entries) {
      if (r && !r.ok) return `${k}: ${r.msg || "unavailable"}`;
    }
    return "";
  }, [reads]);

  const refreshCollectorStatus = useCallback(async () => {
    const st = await fetchCollectorStatus(home, settledMarketsCount);
    setCollectorStatus(st);
  }, [home, settledMarketsCount]);

  const refreshLive = useCallback(async () => {
    const b = await fetchLiveBundle(home);
    setLiveBundle(b);
  }, [home]);

  useEffect(() => {
    void refreshCollectorStatus();
    void refreshLive();
  }, [refreshCollectorStatus, refreshLive]);

  const runCollect = useCallback(async () => {
    if (collectingNow) return;
    setCollectingNow(true);
    setCollectNotice("Collecting 15m Kalshi markets...");
    try {
      const r = await fetch("/api/collect", {
        method: "POST",
        headers: { "X-OpenBell-CSRF": "1" },
      });
      if (!r.ok) {
        const msg = `Nothing was written. Collection failed (${r.status}).`;
        setCollectNotice(msg);
        toast.error(msg);
      } else {
        const body = (await r.json().catch(() => null)) as { snapshots_written?: number; errors?: unknown[] } | null;
        const errCount = Array.isArray(body?.errors) ? body.errors.length : 0;
        if (errCount > 0) {
          const msg = `${errCount} market read${errCount === 1 ? "" : "s"} failed (${body?.snapshots_written ?? 0} written).`;
          setCollectNotice(msg);
          toast.warning(msg);
        } else {
          setCollectNotice("Latest 15m snapshot recorded.");
          toast.success("Collected latest 15m Kalshi market snapshot.");
        }
        await reload();
        await refreshCollectorStatus();
      }
    } catch {
      const msg = "Nothing was written. Collection request could not reach the server.";
      setCollectNotice(msg);
      toast.error(msg);
    } finally {
      setCollectingNow(false);
    }
  }, [collectingNow, reload, refreshCollectorStatus]);

  const handleSelectSchedule = useCallback(
    async (enabled: boolean, intervalSecs: number) => {
      if (updatingSchedule) return;
      setUpdatingSchedule(true);
      try {
        const next = await setCollectorSchedule(enabled, intervalSecs, home, settledMarketsCount);
        setCollectorStatus(next);
        toast.success(
          enabled
            ? `Auto-collector scheduled every ${next.intervalSecs}s.`
            : "Background auto-collector paused.",
        );
      } catch (e) {
        toast.error(`Schedule update failed: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setUpdatingSchedule(false);
      }
    },
    [updatingSchedule, home, settledMarketsCount],
  );

  const handleSaveLiveConfig = useCallback(
    async (payload: LiveConfigPayload) => {
      setSavingLiveConfig(true);
      try {
        await saveLiveConfig(payload, home);
        await refreshLive();
        toast.success("Kalshi API settings and hard risk rails saved (0600 permissions).");
      } catch (e) {
        toast.error(`Could not save live config: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setSavingLiveConfig(false);
      }
    },
    [home, refreshLive],
  );

  const handleArmLive = useCallback(async () => {
    if (liveBusy) return;
    setLiveBusy(true);
    try {
      await armLiveTrading(home);
      await refreshLive();
      toast.success("Live Kalshi v2 order routing armed.");
    } catch (e) {
      toast.error(`Arm live failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLiveBusy(false);
    }
  }, [liveBusy, home, refreshLive]);

  const handleDisarmLive = useCallback(async () => {
    if (liveBusy) return;
    setLiveBusy(true);
    try {
      await disarmLiveTrading(home);
      await refreshLive();
      toast.success("Live trading disarmed. Returned to paper mode.");
    } catch (e) {
      toast.error(`Disarm failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLiveBusy(false);
    }
  }, [liveBusy, home, refreshLive]);

  const handleSubmitLiveOrder = useCallback(
    async (payload: LiveOrderPayload) => {
      if (liveBusy) return;
      setLiveBusy(true);
      try {
        const res = await submitLiveOrder(payload, home);
        await refreshLive();
        toast.success(
          `Order accepted (${res.orderId}, ${res.status}): ${payload.action.toUpperCase()} ${payload.side.toUpperCase()} ${payload.count}x ${payload.ticker} @ ${payload.price_cents}c.`,
        );
      } catch (e) {
        toast.error(`Order rejected: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setLiveBusy(false);
      }
    },
    [liveBusy, home, refreshLive],
  );

  const handleCancelLiveOrder = useCallback(
    async (orderId: string) => {
      if (liveBusy) return;
      setLiveBusy(true);
      try {
        await cancelLiveOrder(orderId, home);
        await refreshLive();
        toast.success(`Canceled resting order ${orderId}.`);
      } catch (e) {
        toast.error(`Cancel failed: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setLiveBusy(false);
      }
    },
    [liveBusy, home, refreshLive],
  );

  const handleKillSwitch = useCallback(async () => {
    if (liveBusy) return;
    setLiveBusy(true);
    try {
      const res = await triggerKillSwitch(home);
      await refreshLive();
      toast.error(
        `Emergency Kill Switch triggered: ${res.cancelledCount} resting order(s) canceled, live routing disarmed.`,
      );
    } catch (e) {
      toast.error(`Kill switch error: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLiveBusy(false);
    }
  }, [liveBusy, home, refreshLive]);

  const handlePaletteExport = useCallback(
    async (format: "md" | "csv") => {
      try {
        const rep = reads?.report?.ok ? reads.report.v.report : "";
        const res = await exportDailyReport(format, dataset, home, rep, slots);
        toast.success(`Exported ${res.filename} (${res.bytes.toLocaleString()} bytes).`);
      } catch (e) {
        toast.error(`Export failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [dataset, home, reads?.report, slots],
  );

  const handleRunEdgeAudit = useCallback(() => {
    setTab("quant");
    setQuantFocus("audit");
    setAuditSignal((s) => s + 1);
  }, []);

  // Global keyboard shortcuts: Cmd/Ctrl+K, /, and 1-5 view switching
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      const inInput = tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target?.isContentEditable;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCmdOpen((v) => !v);
        return;
      }
      if (inInput) return;
      if (e.key === "/" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "1") setTab("tape");
      else if (e.key === "2") setTab("strategies");
      else if (e.key === "3") setTab("slots");
      else if (e.key === "4") setTab("quant");
      else if (e.key === "5") setTab("live");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const navItems: Array<{
    id: DeskTabId;
    label: string;
    shortLabel: string;
    meta: string;
    icon: typeof Activity;
  }> = [
    {
      id: "tape",
      label: "15m Tape",
      shortLabel: "Tape",
      meta: `${openMarketsCount} open`,
      icon: Activity,
    },
    {
      id: "strategies",
      label: "Strategies",
      shortLabel: "Strategies",
      meta: `${strategies.length || 12}`,
      icon: ListChecks,
    },
    {
      id: "slots",
      label: "Paper Slots",
      shortLabel: "Slots",
      meta: `${slots?.slots?.length ?? 4} active`,
      icon: Layers,
    },
    {
      id: "quant",
      label: "Quant Lab & Tournament",
      shortLabel: "Quant Lab",
      meta: "Audit + OOS",
      icon: FlaskConical,
    },
    {
      id: "live",
      label: "Live & Rails",
      shortLabel: "Live & Rails",
      meta: liveBundle.status.liveArmed ? "ARMED" : "DISARMED",
      icon: ShieldAlert,
    },
  ];

  const liveStatus = liveBundle.status;
  const liveModeTone = liveStatus.killed
    ? "error"
    : liveStatus.liveArmed
      ? "warning"
      : "neutral";
  const liveModeLabel = liveStatus.killed
    ? "KILLED"
    : liveStatus.liveArmed
      ? `ARMED (${liveStatus.environment.toUpperCase()})`
      : "DISARMED (PAPER)";

  return (
    <div className="min-h-screen bg-canvas text-ink">
      {/* Persistent Quiet Risk / Kill-Switch Header Bar (48px desktop) */}
      <header className="sticky top-0 z-30 border-b border-hairline bg-surface">
        <div className="mx-auto flex h-auto min-h-12 max-w-[1600px] flex-wrap items-center justify-between gap-2 px-3 py-1.5 sm:px-5 lg:flex-nowrap lg:py-0">
          {/* Left: Brand + Fail-Closed Live State Badge */}
          <div className="flex items-center gap-2.5">
            <span className="inline-flex items-center gap-2 text-base font-semibold tracking-tight text-ink">
              <span aria-hidden className="inline-block size-2 rounded-full bg-dot-green" />
              <span>OpenBell</span>
            </span>
            <span className="mono hidden text-xs text-ink-3 sm:inline">v{versionText}</span>
            {dataset === "fixture" && (
              <Badge variant="outline" className="mono text-xs text-warning-foreground">
                Fixture
              </Badge>
            )}
            <button
              id="live-mode-pill"
              type="button"
              onClick={() => {
                if (liveStatus.liveArmed) {
                  setTab("live");
                } else {
                  setArmModalOpen(true);
                }
              }}
              title={
                liveStatus.liveArmed
                  ? "Live Kalshi v2 order routing is armed. Click to open Live & Rails."
                  : "Fail-closed DISARMED (Paper Mode). Click to inspect or arm live routing."
              }
              className="inline-flex min-h-11 cursor-pointer items-center rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-7"
            >
              <StatusPill
                tone={liveModeTone}
                className={
                  "min-h-7 px-2.5 py-0.5 text-xs font-medium " +
                  (liveStatus.killed
                    ? "border-dot-red/60 text-destructive-foreground"
                    : liveStatus.liveArmed
                      ? "border-dot-amber/70 text-warning-foreground"
                      : "text-ink-2")
                }
              >
                {liveStatus.liveArmed ? (
                  <Unlock className="size-3" strokeWidth={1.5} aria-hidden />
                ) : (
                  <Lock className="size-3" strokeWidth={1.5} aria-hidden />
                )}
                <span className="mono">{liveModeLabel}</span>
              </StatusPill>
            </button>
          </div>

          {/* Right: Filter + 1-Click Edge Audit + Collect + Persistent Kill Switch + Settings */}
          <div className="flex flex-wrap items-center gap-1.5 sm:gap-2">
            <label className="relative flex items-center" aria-label="Filter markets, strategies, and slots">
              <Search className="pointer-events-none absolute left-2.5 size-3.5 text-ink-3" strokeWidth={1.5} aria-hidden />
              <input
                ref={searchRef}
                id="desk-search"
                type="search"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="Filter desk"
                className="min-h-11 w-40 rounded-md border border-[var(--input)] bg-board pr-8 pl-8 text-xs text-ink placeholder:text-ink-3 outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 sm:w-44"
              />
              <span className="pointer-events-none absolute right-2 hidden sm:inline-flex">
                <Kbd>/</Kbd>
              </span>
            </label>

            <Button
              id="run-edge-audit-btn"
              variant="default"
              size="sm"
              onClick={handleRunEdgeAudit}
              title="Run 1-click Full Quant Edge Audit across all 12 strategies"
            >
              <Sparkles className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Edge Audit</span>
            </Button>

            <Button
              id="collect-btn"
              variant="outline"
              size="sm"
              disabled={collectingNow}
              onClick={() => void runCollect()}
              aria-label={collectingNow ? "Collecting live tape" : "Collect live tape"}
              title="Fetch the latest 15m Kalshi market snapshot"
              className="min-w-11 sm:min-w-0"
            >
              <RefreshCw
                className={`size-3.5 ${collectingNow ? "animate-spin" : ""}`}
                strokeWidth={1.5}
                aria-hidden
              />
              <span className="hidden sm:inline">{collectingNow ? "Collecting…" : "Collect"}</span>
            </Button>

            <Button
              id="kill-switch-btn"
              variant="danger"
              size="sm"
              disabled={liveBusy}
              onClick={() => void handleKillSwitch()}
              title="Cancel all resting Kalshi orders immediately and disarm live trading"
            >
              <AlertOctagon className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Kill Switch</span>
            </Button>

            <Button
              id="cmdk-btn"
              variant="outline"
              size="sm"
              onClick={() => setCmdOpen(true)}
              aria-label="Open command palette"
              className="hidden md:inline-flex"
            >
              <span>Jump</span>
              <Kbd>⌘K</Kbd>
            </Button>

            <Button
              id="settings-btn"
              variant="outline"
              size="icon"
              aria-label="Open desk settings"
              onClick={() => setSettingsOpen(true)}
              className="size-11 sm:size-8"
            >
              <Settings className="size-4" strokeWidth={1.5} aria-hidden />
            </Button>
          </div>
        </div>
      </header>

      <UpdateStrip u={upd} />

      {/* Desktop Workbench Split: Slim Left Desk Rail + Main Tabular Workspace */}
      <div className="mx-auto flex max-w-[1600px] flex-col lg:flex-row">
        {/* Left Desk Navigation Rail */}
        <aside
          aria-label="Desk sidebar"
          className="flex flex-col justify-start border-b border-hairline bg-surface lg:sticky lg:top-12 lg:max-h-[calc(100vh-49px)] lg:w-56 lg:shrink-0 lg:self-start lg:overflow-y-auto lg:border-r lg:border-b-0"
        >
          <div className="p-2.5 lg:p-3">
            {/* Primary 5 Desk Views */}
            <nav aria-label="Desk views">
              <div
                role="tablist"
                aria-label="Desk sections"
                className="flex gap-1 overflow-x-auto lg:flex-col"
              >
                {navItems.map((item) => {
                  const Icon = item.icon;
                  const active = tab === item.id;
                  return (
                    <button
                      key={item.id}
                      id={`tab-${item.id}`}
                      role="tab"
                      type="button"
                      aria-selected={active}
                      onClick={() => setTab(item.id)}
                      className={
                        "flex min-h-11 shrink-0 cursor-pointer items-center justify-between gap-2 rounded-md border px-2.5 py-1.5 text-left text-sm font-medium outline-none transition-[background-color,color,border-color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring lg:min-h-9 lg:w-full " +
                        (active
                          ? "border-hairline bg-board text-ink shadow-xs"
                          : "border-transparent bg-transparent text-ink-2 hover:bg-board/60 hover:text-ink")
                      }
                    >
                      <span className="flex items-center gap-2">
                        <Icon
                          className={`size-4 shrink-0 ${active ? "text-info-foreground" : "text-ink-3"}`}
                          strokeWidth={1.5}
                          aria-hidden
                        />
                        <span className="lg:hidden">{item.shortLabel}</span>
                        <span className="hidden lg:inline">{item.label}</span>
                      </span>
                      <span className="mono hidden text-xs text-ink-3 lg:inline">{item.meta}</span>
                    </button>
                  );
                })}
              </div>
            </nav>

            {/* Collapsible Workbench Drawer Triggers */}
            <div className="mt-2 flex flex-wrap gap-1.5 border-t border-hairline pt-2.5 lg:mt-4 lg:flex-col">
              <Button
                id="drawer-order-ticket-btn"
                variant={ticketDrawerOpen ? "default" : "outline"}
                size="sm"
                className="justify-start lg:w-full"
                onClick={() => setTicketDrawerOpen(true)}
              >
                <SlidersHorizontal className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
                <span>Order Ticket</span>
              </Button>
              <Button
                id="drawer-rule-editor-btn"
                variant={editorDrawerOpen ? "default" : "outline"}
                size="sm"
                className="justify-start lg:w-full"
                onClick={() => {
                  setEditorInitStrategy(null);
                  setEditorDrawerOpen(true);
                }}
              >
                <Code2 className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
                <span>Rule Editor</span>
              </Button>
            </div>
          </div>

          {/* Quiet Telemetry Readout (#readings) */}
          <div
            id="readings"
            aria-label="Desk status telemetry"
            className="grid grid-cols-1 gap-2.5 border-t border-hairline px-3 py-2.5 text-xs sm:grid-cols-3 lg:grid-cols-1 lg:space-y-3 lg:py-3"
          >
            <div id="pot">
              <div className="text-ink-3">Paper Pot</div>
              <div className="mt-0.5 flex items-baseline justify-between gap-2">
                <span id="pot-figure" className="mono text-base font-semibold text-ink">
                  {potValue !== null ? money(potValue) : "$100.00"}
                </span>
                <span
                  id="pot-change"
                  className={
                    "mono text-xs font-medium " +
                    (potDelta !== null && potDelta > 0
                      ? "text-success-foreground"
                      : potDelta !== null && potDelta < 0
                        ? "text-destructive-foreground"
                        : "text-ink-2")
                  }
                >
                  {potDelta !== null ? moneySigned(potDelta) : "$0.00"}
                </span>
              </div>
              <div id="stage-figure" className="text-xs text-ink-3">
                {potStage}
              </div>
            </div>

            <div id="standing" className="border-hairline pt-2 sm:border-l sm:pl-3 sm:pt-0 lg:border-t lg:border-l-0 lg:pl-0 lg:pt-2.5">
              <div id="standing-label" className="text-ink-3">
                {standingTitle}
              </div>
              <div id="standing-figure" className="mono mt-0.5 text-sm font-semibold text-ink">
                {standingFigure}
              </div>
              <ul id="standing-detail" className="mt-0.5 space-y-0.5 text-xs text-ink-3">
                {standingLines.map((line, i) => (
                  <li key={i} className="break-words">{line}</li>
                ))}
              </ul>
            </div>

            <div id="chip" className="border-hairline pt-2 sm:border-l sm:pl-3 sm:pt-0 lg:border-t lg:border-l-0 lg:pl-0 lg:pt-2.5">
              <div className="flex items-center gap-1.5">
                <span
                  id="chip-dot"
                  aria-hidden
                  className={`size-2 rounded-full ${collector.fresh ? "bg-dot-green" : "bg-dot-amber"}`}
                />
                <span id="chip-text" className="mono text-xs font-medium text-ink">
                  {collector.label}
                </span>
              </div>
              <div id="collect-status" role="status" className="mt-0.5 text-xs text-ink-3">
                {collectNotice || collector.detail}
              </div>
            </div>
          </div>
        </aside>

        {/* Main Tabular Workspace */}
        <main className="min-w-0 flex-1 space-y-4 px-3 py-4 sm:px-5 lg:px-6">
          {dataset === "fixture" && (
            <div
              role="status"
              className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-hairline bg-surface px-3.5 py-2 text-xs text-ink-2"
            >
              <span>Example tape. Loaded from bundled offline fixture snapshots for testing.</span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  const url = new URL(location.href);
                  url.searchParams.delete("dataset");
                  history.replaceState(null, "", url.pathname + url.search + url.hash);
                  setDataset("live");
                }}
              >
                Return to live tape
              </Button>
            </div>
          )}
          {anyReadError && (
            <div
              id="load-error"
              role="alert"
              className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-dot-red/50 bg-surface px-4 py-2.5 text-sm text-destructive-foreground"
            >
              <span>Could not reach one or more desk feeds ({anyReadError}).</span>
              <Button variant="outline" size="sm" onClick={() => void reload()}>
                Retry
              </Button>
            </div>
          )}

          {tab === "tape" && (
            <TapeTab
              marketsRead={reads?.markets}
              home={home}
              dataset={dataset}
              search={search}
              stream={stream}
              collectorStatus={collectorStatus}
              updatingSchedule={updatingSchedule}
              onSelectSchedule={(enabled, secs) => void handleSelectSchedule(enabled, secs)}
              onOpenScorecard={() => {
                setTab("quant");
                setQuantFocus("scorecard");
              }}
              onTradeMarket={(ticker, askCents) => {
                setTicketTicker(ticker);
                setTicketPriceCents(askCents);
                setTicketDrawerOpen(true);
              }}
              onRetry={() => void reload()}
              onCollect={() => void runCollect()}
              collectingNow={collectingNow}
              jumpTicker={jumpTicker}
              onClearJump={() => setJumpTicker(null)}
            />
          )}

          {tab === "strategies" && (
            <StrategiesTab
              strategiesRead={reads?.strategies}
              slotsRead={reads?.slots}
              equityRead={reads?.equity}
              marketsRead={reads?.markets}
              home={home}
              dataset={dataset}
              search={search}
              onRetry={() => void reload()}
              isActiveTab={tab === "strategies"}
              jumpStrategy={jumpStrategy}
              sandboxSignal={sandboxSignal}
              onClearJump={() => setJumpStrategy(null)}
            />
          )}

          {tab === "slots" && (
            <SlotsTab
              slotsRead={reads?.slots}
              equityRead={reads?.equity}
              homeRead={reads?.home}
              reportRead={reads?.report}
              liveBundle={liveBundle}
              onOpenOrderTicket={() => setTicketDrawerOpen(true)}
              onOpenSettings={() => setSettingsOpen(true)}
              dataset={dataset}
              search={search}
              onRetry={() => void reload()}
            />
          )}

          {tab === "quant" && (
            <QuantLabTab
              dataset={dataset}
              home={home}
              marketsRead={reads?.markets}
              slotsRead={reads?.slots}
              strategiesRead={reads?.strategies}
              search={search}
              focusSection={quantFocus}
              reseedSignal={reseedSignal}
              auditSignal={auditSignal}
              onClearFocus={() => setQuantFocus(null)}
            />
          )}

          {tab === "live" && (
            <div className="space-y-4">
              <LiveAccountStrip
                bundle={liveBundle}
                busy={liveBusy}
                workspaceOpen={true}
                onToggleWorkspace={() => setTicketDrawerOpen(true)}
                onArmConfirm={() => void handleArmLive()}
                onDisarm={() => void handleDisarmLive()}
                onKillSwitch={() => void handleKillSwitch()}
                onRefresh={() => void refreshLive()}
                onOpenSettings={() => setSettingsOpen(true)}
              />
              <LiveDeskWorkspace
                bundle={liveBundle}
                markets={markets}
                prefillTicker={ticketTicker}
                prefillPriceCents={ticketPriceCents}
                busy={liveBusy}
                onSubmitOrder={handleSubmitLiveOrder}
                onCancelOrder={handleCancelLiveOrder}
                onRequestArm={() => setArmModalOpen(true)}
                onOpenSettings={() => setSettingsOpen(true)}
              />
            </div>
          )}

          <footer className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-3 pb-4 text-xs text-ink-3">
            <span className="mono">
              OpenBell v{versionText} — Kalshi 15m Quant &amp; Execution Workbench (Hard Risk Rails Enforced)
            </span>
            <span className="mono">
              {liveStatus.liveArmed
                ? `Kalshi ${liveStatus.environment.toUpperCase()} Live Armed`
                : "Fail-Closed Disarmed (Paper Mode)"}
            </span>
          </footer>
        </main>
      </div>

      {/* Collapsible Live Order Ticket Drawer */}
      <Drawer.Root
        open={ticketDrawerOpen}
        onOpenChange={setTicketDrawerOpen}
        direction={wide ? "right" : "bottom"}
      >
        <Drawer.Portal>
          <Drawer.Overlay className="fixed inset-0 z-40 bg-ink/30" />
          <Drawer.Content
            id="order-ticket-drawer"
            data-order-ticket-drawer
            className={
              wide
                ? "fixed inset-y-0 right-0 z-50 flex w-full max-w-[920px] flex-col border-l border-hairline bg-canvas text-ink shadow-(--elev-lift) outline-none"
                : "fixed inset-x-0 bottom-0 z-50 flex max-h-[90dvh] flex-col rounded-t-[14px] border-t border-hairline bg-canvas text-ink shadow-(--elev-lift) outline-none"
            }
          >
            <div className="flex items-center justify-between gap-3 border-b border-hairline bg-surface px-5 py-3.5">
              <div>
                <Drawer.Title className="text-base font-semibold text-ink">
                  Order Ticket &amp; Live Rails
                </Drawer.Title>
                <Drawer.Description className="text-xs text-ink-2">
                  Pre-flight risk rail verification before any Kalshi v2 order submission.
                </Drawer.Description>
              </div>
              <Button
                variant="ghost"
                size="icon"
                aria-label="Close order ticket drawer"
                onClick={() => setTicketDrawerOpen(false)}
              >
                <X className="size-4" strokeWidth={1.5} aria-hidden />
              </Button>
            </div>
            <div className="flex-1 overflow-y-auto p-4 sm:p-5">
              <LiveDeskWorkspace
                bundle={liveBundle}
                markets={markets}
                prefillTicker={ticketTicker}
                prefillPriceCents={ticketPriceCents}
                busy={liveBusy}
                onSubmitOrder={handleSubmitLiveOrder}
                onCancelOrder={handleCancelLiveOrder}
                onRequestArm={() => setArmModalOpen(true)}
                onOpenSettings={() => {
                  setTicketDrawerOpen(false);
                  setSettingsOpen(true);
                }}
              />
            </div>
          </Drawer.Content>
        </Drawer.Portal>
      </Drawer.Root>

      {/* Collapsible Strategy Rule Editor Drawer */}
      <Drawer.Root
        open={editorDrawerOpen}
        onOpenChange={setEditorDrawerOpen}
        direction={wide ? "right" : "bottom"}
      >
        <Drawer.Portal>
          <Drawer.Overlay className="fixed inset-0 z-40 bg-ink/30" />
          <Drawer.Content
            id="rule-editor-drawer"
            data-rule-editor-drawer
            className={
              wide
                ? "fixed inset-y-0 right-0 z-50 flex w-full max-w-[960px] flex-col border-l border-hairline bg-canvas text-ink shadow-(--elev-lift) outline-none"
                : "fixed inset-x-0 bottom-0 z-50 flex max-h-[90dvh] flex-col rounded-t-[14px] border-t border-hairline bg-canvas text-ink shadow-(--elev-lift) outline-none"
            }
          >
            <div className="flex items-center justify-between gap-3 border-b border-hairline bg-surface px-5 py-3.5">
              <div>
                <Drawer.Title className="text-base font-semibold text-ink">
                  Strategy Rule Editor &amp; Backtester
                </Drawer.Title>
                <Drawer.Description className="text-xs text-ink-2">
                  Validate syntax and score strategy rule modifications directly against the active 15m tape.
                </Drawer.Description>
              </div>
              <Button
                variant="ghost"
                size="icon"
                aria-label="Close rule editor drawer"
                onClick={() => setEditorDrawerOpen(false)}
              >
                <X className="size-4" strokeWidth={1.5} aria-hidden />
              </Button>
            </div>
            <div className="flex-1 overflow-y-auto p-4 sm:p-5">
              <RuneSandbox
                strategies={strategies}
                markets={markets}
                slots={slots}
                home={home}
                dataset={dataset}
                initialStrategyName={editorInitStrategy}
                onClose={() => setEditorDrawerOpen(false)}
              />
            </div>
          </Drawer.Content>
        </Drawer.Portal>
      </Drawer.Root>

      <ArmLiveModal
        open={armModalOpen}
        onOpenChange={setArmModalOpen}
        status={liveStatus}
        busy={liveBusy}
        onConfirmArm={() => void handleArmLive()}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <SettingsSheet
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        theme={theme}
        onThemeChange={setTheme}
        home={home}
        collectorStatus={collectorStatus}
        updatingSchedule={updatingSchedule}
        onSelectSchedule={(enabled, secs) => void handleSelectSchedule(enabled, secs)}
        liveStatus={liveStatus}
        savingLiveConfig={savingLiveConfig}
        onSaveLiveConfig={handleSaveLiveConfig}
        collectingNow={collectingNow}
        onCollect={() => void runCollect()}
        upd={upd}
      />

      <CommandPalette
        open={cmdOpen}
        onOpenChange={setCmdOpen}
        markets={markets}
        strategies={strategies}
        slots={slots?.slots || []}
        onSelectTab={setTab}
        onJumpMarket={(ticker) => {
          setTab("tape");
          setJumpTicker(ticker);
        }}
        onJumpStrategy={(name) => {
          setTab("strategies");
          setJumpStrategy(name);
        }}
        onOpenSandbox={() => {
          setEditorInitStrategy(null);
          setEditorDrawerOpen(true);
          setSandboxSignal((s) => s + 1);
        }}
        onJumpSlot={(_slotNumber, strategyName) => {
          setTab("slots");
          if (strategyName) setSearch(strategyName);
        }}
        onJumpQuantSection={(section) => {
          setTab("quant");
          setQuantFocus(section);
          if (section === "audit") setAuditSignal((s) => s + 1);
        }}
        onCollect={() => void runCollect()}
        onExportReport={(fmt) => void handlePaletteExport(fmt)}
        onReseedMonteCarlo={() => {
          setTab("quant");
          setQuantFocus("montecarlo");
          setReseedSignal((s) => s + 1);
        }}
        onOpenOrderTicket={() => setTicketDrawerOpen(true)}
        onKillSwitch={() => void handleKillSwitch()}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <Toaster position="bottom-right" />
    </div>
  );
}

export default App;
