// OpenBell Shadow & Fail-Closed Auto-Live Runner Control Deck & Persistent Ledger UI.
// Supports mode control (Stopped / Shadow / confirmation-gated Live Auto), slot config,
// live resting quotes with queue-ahead bar & fill ETA, strategy stats rollup, and filterable ledger.
import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  ShieldAlert,
  ShieldBan,
  Square,
  Trash2,
  X,
  Zap,
} from "lucide-react";
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
import {
  type Dataset,
  type Home,
  type Market,
  type Strategy,
  cleanStr,
  isNum,
  money,
  moneySigned,
  signedCents,
} from "@/lib/desk";
import type { LiveStatus } from "@/lib/live";
import {
  AVAILABLE_SERIES,
  type ExecutionHealthResponse,
  type ExecutionHealthStatus,
  RunnerApiError,
  type RunnerMode,
  type RunnerSlotConfig,
  type RunnerStatus,
  type RunnerLedgerResponse,
  buildPreviewExecutionHealth,
  buildPreviewRunnerState,
  fetchExecutionHealth,
  fetchRunnerLedger,
  fetchRunnerStatus,
  overrideQuarantine,
  saveRunnerConfig,
  startRunner,
  stopRunner,
  subscribeRunnerStream,
  supportsShadowRunner,
} from "@/lib/runner";

function parseOptSlotInt(raw: string, minVal: number, maxVal: number): number | undefined {
  const trimmed = raw.trim();
  if (!trimmed) return undefined;
  const n = Number(trimmed);
  if (!Number.isFinite(n)) return undefined;
  return Math.min(maxVal, Math.max(minVal, Math.round(n)));
}

function toSlotConfig(s: RunnerSlotConfig): RunnerSlotConfig {
  return {
    slot_id: s.slot_id,
    strategy: s.strategy,
    series: s.series,
    max_contracts: s.max_contracts,
    execution_mode: s.execution_mode,
    auto_pause_drawdown_dollars: s.auto_pause_drawdown_dollars,
    enabled: s.enabled,
    ...(isNum(s.reprice_cooldown_secs) ? { reprice_cooldown_secs: s.reprice_cooldown_secs } : {}),
    ...(isNum(s.max_replaces_per_window) ? { max_replaces_per_window: s.max_replaces_per_window } : {}),
    ...(isNum(s.take_profit_cents) && s.take_profit_cents > 0 ? { take_profit_cents: s.take_profit_cents } : {}),
    ...(isNum(s.stop_loss_cents) && s.stop_loss_cents > 0 ? { stop_loss_cents: s.stop_loss_cents } : {}),
    ...(isNum(s.exit_before_close_secs) ? { exit_before_close_secs: s.exit_before_close_secs } : {}),
  };
}

function shortUtcTime(iso: string, epoch: number): string {
  if (iso && iso.length >= 19) {
    return iso.slice(5, 19).replace("T", " ") + "Z";
  }
  if (isNum(epoch) && epoch > 0) {
    return new Date(epoch * 1000).toISOString().slice(5, 19).replace("T", " ") + "Z";
  }
  return "-";
}

function formatEta(secs: number | null): string {
  if (!isNum(secs) || secs < 0) return "-";
  if (secs < 60) return `~${Math.round(secs)}s`;
  const m = Math.floor(secs / 60);
  const s = Math.round(secs % 60);
  return `~${m}m ${s}s`;
}

function modeBadgeProps(mode: RunnerMode, killed: boolean): {
  label: string;
  tone: "neutral" | "info" | "warning" | "error";
} {
  if (killed) return { label: "Halted (Killed)", tone: "error" };
  if (mode === "live_auto") return { label: "Live Auto", tone: "warning" };
  if (mode === "shadow") return { label: "Shadow", tone: "info" };
  return { label: "Stopped", tone: "neutral" };
}

function healthStatusProps(status: ExecutionHealthStatus): {
  label: "Active" | "Throttled" | "Quarantined";
  tone: "success" | "warning" | "error";
} {
  if (status === "quarantined") return { label: "Quarantined", tone: "error" };
  if (status === "throttled") return { label: "Throttled", tone: "warning" };
  return { label: "Active", tone: "success" };
}

function fmtOptCents(v: number | null | undefined): string {
  if (!isNum(v) || !Number.isFinite(v)) return "N/A";
  return `${v >= 0 ? "+" : ""}${v.toFixed(2)}c`;
}

function fmtOptProbPct(v: number | null | undefined): string {
  if (!isNum(v) || !Number.isFinite(v)) return "N/A";
  const pct = v >= 0 && v <= 1 ? v * 100 : v;
  return `${pct.toFixed(1)}%`;
}

function fmtOptPct(v: number | null | undefined): string {
  if (!isNum(v) || !Number.isFinite(v)) return "N/A";
  return `${v.toFixed(1)}%`;
}

function fmtOptPp(v: number | null | undefined): string {
  if (!isNum(v) || !Number.isFinite(v)) return "N/A";
  return `${v >= 0 ? "+" : ""}${v.toFixed(1)}pp`;
}

function MarkoutSparkline({
  m10,
  m30,
  m60,
}: {
  m10: number | null;
  m30: number | null;
  m60: number | null;
}) {
  const n10 = isNum(m10) ? m10 : null;
  const n30 = isNum(m30) ? m30 : null;
  const n60 = isNum(m60) ? m60 : null;
  if (n10 === null && n30 === null && n60 === null) {
    return (
      <svg
        width={56}
        height={24}
        viewBox="0 0 56 24"
        aria-hidden="true"
        className="shrink-0 text-ink-3"
      >
        <line
          x1={2}
          y1={12}
          x2={54}
          y2={12}
          stroke="currentColor"
          strokeOpacity={0.35}
          strokeDasharray="2 2"
          strokeWidth={1}
        />
      </svg>
    );
  }
  const first = n10 ?? n30 ?? n60 ?? 0;
  const second = n30 ?? first;
  const third = n60 ?? second;
  const vals = [first, second, third];
  const minV = Math.min(-1, ...vals);
  const maxV = Math.max(1, ...vals);
  const span = Math.max(0.5, maxV - minV);
  const mapY = (v: number) => 20 - ((v - minV) / span) * 16;
  const zeroY = mapY(0);
  const pts = `4,${mapY(vals[0]).toFixed(1)} 28,${mapY(vals[1]).toFixed(1)} 52,${mapY(vals[2]).toFixed(1)}`;
  const last = vals[2];
  const strokeCls = last >= 0 ? "text-success-foreground" : "text-destructive-foreground";
  return (
    <svg
      width={56}
      height={24}
      viewBox="0 0 56 24"
      aria-hidden="true"
      className={`shrink-0 ${strokeCls}`}
    >
      <line
        x1={2}
        y1={zeroY}
        x2={54}
        y2={zeroY}
        stroke="currentColor"
        strokeOpacity={0.25}
        strokeDasharray="2 2"
        strokeWidth={1}
      />
      <polyline
        fill="none"
        stroke="currentColor"
        strokeWidth={1.75}
        strokeLinecap="round"
        strokeLinejoin="round"
        points={pts}
      />
      {n10 !== null && <circle cx={4} cy={mapY(vals[0])} r={1.8} fill="currentColor" />}
      {n30 !== null && <circle cx={28} cy={mapY(vals[1])} r={1.8} fill="currentColor" />}
      {n60 !== null && <circle cx={52} cy={mapY(vals[2])} r={2.2} fill="currentColor" />}
    </svg>
  );
}

function LiveAutoConfirmModal({
  open,
  onOpenChange,
  liveStatus,
  liveAutoAllowed,
  busy,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  liveStatus: LiveStatus;
  liveAutoAllowed: boolean;
  busy: boolean;
  onConfirm: () => void;
}) {
  const [checked, setChecked] = useState(false);
  const confirmLockRef = useRef(false);

  useEffect(() => {
    if (!open) {
      setChecked(false);
      confirmLockRef.current = false;
      return;
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onOpenChange(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, busy, onOpenChange]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[10vh]">
      <div
        aria-hidden="true"
        className="fixed inset-0 bg-ink/40"
        onClick={() => {
          if (!busy) onOpenChange(false);
        }}
      />
      <div
        id="runner-live-confirm-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="runner-live-confirm-title"
        className="relative z-10 max-h-[85dvh] w-[calc(100%-24px)] max-w-[500px] overflow-y-auto rounded-[12px] border border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
      >
        <div className="flex items-start justify-between gap-3 border-b border-hairline px-5 py-4">
          <div className="flex items-start gap-2.5">
            <ShieldAlert className="mt-0.5 size-5 shrink-0 text-warning-foreground" strokeWidth={1.5} aria-hidden />
            <div>
              <h2 id="runner-live-confirm-title" className="text-base font-semibold text-ink">
                Start Automated Live Routing
              </h2>
              <p className="mt-0.5 text-xs text-ink-2">
                Confirm live auto execution under active hard risk rails.
              </p>
            </div>
          </div>
          <Button
            variant="ghost"
            size="icon"
            aria-label="Close live auto confirmation"
            className="size-11 shrink-0 sm:size-9"
            onClick={() => onOpenChange(false)}
          >
            <X className="size-4" strokeWidth={1.5} aria-hidden />
          </Button>
        </div>

        <div className="space-y-3 px-5 py-4 text-xs">
          <dl className="grid grid-cols-2 gap-3 border-b border-hairline pb-3">
            <div className="min-w-0">
              <dt className="text-ink-3">Environment</dt>
              <dd className="mono mt-0.5 font-semibold text-ink">{liveStatus.environment.toUpperCase()}</dd>
            </div>
            <div className="min-w-0">
              <dt className="text-ink-3">Max Daily Loss</dt>
              <dd className="mono mt-0.5 font-semibold tabular-nums text-ink">{money(liveStatus.rails.maxDailyLossDollars)}</dd>
            </div>
            <div className="min-w-0">
              <dt className="text-ink-3">Max Order Cap</dt>
              <dd className="mono mt-0.5 font-semibold tabular-nums text-ink">
                {liveStatus.rails.maxOrderContracts} cts / {money(liveStatus.rails.maxOrderDollars)}
              </dd>
            </div>
            <div className="min-w-0">
              <dt className="text-ink-3">Max Exposure</dt>
              <dd className="mono mt-0.5 font-semibold tabular-nums text-ink">{money(liveStatus.rails.maxOpenExposureDollars)}</dd>
            </div>
          </dl>

          <label className="flex min-h-11 cursor-pointer items-start gap-2.5 rounded-lg border border-hairline bg-board/60 p-3 text-xs text-ink">
            <input
              id="runner-live-confirm-checkbox"
              type="checkbox"
              checked={checked}
              onChange={(e) => setChecked(e.target.checked)}
              className="mt-0.5 size-4 shrink-0 accent-blue"
            />
            <span>
              I confirm automated Live Auto execution on {liveStatus.environment.toUpperCase()} under hard risk rails.
            </span>
          </label>
        </div>

        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-hairline bg-board/50 px-5 py-3">
          <Button
            variant="outline"
            className="min-h-11 text-xs sm:min-h-9"
            onClick={() => onOpenChange(false)}
          >
            Cancel
          </Button>
          <Button
            id="runner-confirm-live-btn"
            variant="default"
            disabled={!checked || busy || !liveAutoAllowed}
            className="min-h-11 gap-1.5 text-xs sm:min-h-9"
            onClick={() => {
              if (confirmLockRef.current || busy || !checked || !liveAutoAllowed) return;
              confirmLockRef.current = true;
              onConfirm();
              onOpenChange(false);
            }}
          >
            <Zap className="size-3.5" strokeWidth={1.5} aria-hidden />
            <span>{busy ? "Starting..." : "Confirm Live Auto"}</span>
          </Button>
        </div>
      </div>
    </div>
  );
}

export function ShadowRunnerPanel({
  home,
  strategies,
  markets,
  liveStatus,
  dataset,
  search,
  refreshSignal = 0,
  onRunnerStatusChange,
  onRequestArm,
}: {
  home: Home | null;
  strategies: Strategy[];
  markets: Market[];
  liveStatus: LiveStatus;
  dataset: Dataset;
  search: string;
  refreshSignal?: number;
  onRunnerStatusChange?: (status: RunnerStatus) => void;
  onRequestArm?: () => void;
}) {
  const initialPreview = useMemo(
    () => buildPreviewRunnerState(strategies, markets, liveStatus.liveArmed, liveStatus.killed, dataset),
    [strategies, markets, liveStatus.liveArmed, liveStatus.killed, dataset],
  );

  const [status, setStatus] = useState<RunnerStatus>(initialPreview.status);
  const [ledger, setLedger] = useState<RunnerLedgerResponse>(initialPreview.ledger);
  const [draftSlots, setDraftSlots] = useState<RunnerSlotConfig[]>(() =>
    initialPreview.status.active_slots.map((s) => toSlotConfig(s)),
  );
  const [cancelBeforeCloseSecs, setCancelBeforeCloseSecs] = useState<string>(
    String(initialPreview.status.cancel_before_close_secs),
  );
  const [bankrollDollars, setBankrollDollars] = useState<string>(
    String(initialPreview.status.bankroll_dollars),
  );
  const [configDirty, setConfigDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [liveConfirmOpen, setLiveConfirmOpen] = useState(false);
  const [ledgerStrategyFilter, setLedgerStrategyFilter] = useState<string>("all");
  const [ledgerModeFilter, setLedgerModeFilter] = useState<"all" | "shadow" | "live">("all");
  const [lastSseEvent, setLastSseEvent] = useState<string | null>(null);
  const [runnerError, setRunnerError] = useState<{ status: number | null; message: string } | null>(null);
  const [executionHealth, setExecutionHealth] = useState<ExecutionHealthResponse>(() =>
    buildPreviewExecutionHealth(strategies),
  );
  const [overrideBusy, setOverrideBusy] = useState<string | null>(null);
  const [overrideError, setOverrideError] = useState<string | null>(null);
  const actionLockRef = useRef(false);
  const overrideLockRef = useRef(false);
  const mountedRef = useRef(true);
  const refreshSeqRef = useRef(0);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const serverSupported = supportsShadowRunner(home);

  const strategyChoices = useMemo(() => {
    const names = new Set<string>();
    for (const s of strategies) {
      const n = cleanStr(s?.strategy, "");
      if (n) names.add(n);
    }
    for (const s of draftSlots) {
      if (s.strategy) names.add(s.strategy);
    }
    return [...names].sort();
  }, [strategies, draftSlots]);

  const syncDraftFromStatus = useCallback((next: RunnerStatus) => {
    setDraftSlots(next.active_slots.map((s) => toSlotConfig(s)));
    setCancelBeforeCloseSecs(String(next.cancel_before_close_secs));
    setBankrollDollars(String(next.bankroll_dollars));
    setConfigDirty(false);
  }, []);

  const refreshAll = useCallback(
    async (syncConfig = false) => {
      const reqSeq = ++refreshSeqRef.current;
      const fb = {
        strategies,
        markets,
        liveArmed: liveStatus.liveArmed,
        killed: liveStatus.killed,
        dataset,
      };
      const [nextStatus, nextLedger, nextHealth] = await Promise.all([
        fetchRunnerStatus(home, fb),
        fetchRunnerLedger(
          home,
          { limit: 200, strategy: ledgerStrategyFilter, mode: ledgerModeFilter },
          fb,
        ),
        fetchExecutionHealth(dataset, strategies),
      ]);
      if (!mountedRef.current || reqSeq !== refreshSeqRef.current) return;
      setStatus(nextStatus);
      setLedger(nextLedger);
      setExecutionHealth(nextHealth);
      onRunnerStatusChange?.(nextStatus);
      if (syncConfig || !configDirty) {
        syncDraftFromStatus(nextStatus);
      }
    },
    [
      strategies,
      markets,
      liveStatus.liveArmed,
      liveStatus.killed,
      dataset,
      home,
      ledgerStrategyFilter,
      ledgerModeFilter,
      onRunnerStatusChange,
      configDirty,
      syncDraftFromStatus,
    ],
  );

  useEffect(() => {
    void refreshAll(false);
  }, [refreshAll, refreshSignal]);

  // Subscribe to SSE `event: runner` updates when the server runner is active.
  useEffect(() => {
    const unsub = subscribeRunnerStream(
      serverSupported && dataset !== "fixture",
      (ev) => {
        setLastSseEvent(ev.kind);
        void refreshAll(false);
      },
      () => {
        void refreshAll(false);
      },
    );
    return unsub;
  }, [serverSupported, dataset, refreshAll]);

  const effectiveLiveArmed =
    status.source === "server" ? status.live_armed && liveStatus.liveArmed : liveStatus.liveArmed;
  const effectiveKilled =
    status.source === "server" ? status.killed || liveStatus.killed : liveStatus.killed;
  const liveAutoAllowed = effectiveLiveArmed && !effectiveKilled;
  const modeMeta = modeBadgeProps(status.mode, effectiveKilled);

  useEffect(() => {
    if (!liveAutoAllowed && liveConfirmOpen) {
      setLiveConfirmOpen(false);
    }
  }, [liveAutoAllowed, liveConfirmOpen]);

  const handleRunnerMutationError = useCallback(
    (actionLabel: string, err: unknown) => {
      const httpStatus = err instanceof RunnerApiError ? err.status : null;
      const rawMsg = err instanceof Error ? err.message : String(err);
      let displayMsg = `${actionLabel} failed: ${rawMsg}`;
      if (httpStatus === 403) {
        displayMsg = `Blocked (403 Forbidden): ${rawMsg}. Live Auto stays locked until live trading is armed and kill switch is clear.`;
      } else if (httpStatus === 409) {
        displayMsg = `State conflict (409 Conflict): ${rawMsg}. Runner status re-synced with server.`;
      }
      setRunnerError({ status: httpStatus, message: displayMsg });
      toast.error(displayMsg);
      if (httpStatus === 403 || httpStatus === 409) {
        void refreshAll(false);
      }
    },
    [refreshAll],
  );

  const handleSaveConfig = async () => {
    if (actionLockRef.current || busy) return;
    if (draftSlots.length === 0) {
      const msg = "Configure at least one runner slot before saving.";
      setRunnerError({ status: 400, message: msg });
      toast.error(msg);
      return;
    }
    actionLockRef.current = true;
    refreshSeqRef.current += 1;
    setBusy(true);
    setRunnerError(null);
    try {
      const payload = {
        slots: draftSlots.map((s, idx) => ({
          ...toSlotConfig(s),
          slot_id: idx + 1,
          max_contracts: Math.max(1, Math.round(Number(s.max_contracts) || 1)),
          auto_pause_drawdown_dollars: Math.max(1, Number(s.auto_pause_drawdown_dollars) || 10),
          series: s.series.length > 0 ? s.series : ["KXBTC15M"],
        })),
        cancel_before_close_secs: Math.min(300, Math.max(5, Math.round(Number(cancelBeforeCloseSecs) || 30))),
        bankroll_dollars: Math.max(10, Number(bankrollDollars) || 250),
      };
      const next = await saveRunnerConfig(payload, home);
      if (!mountedRef.current) return;
      refreshSeqRef.current += 1;
      setStatus(next);
      syncDraftFromStatus(next);
      onRunnerStatusChange?.(next);
      toast.success(`Saved runner config (${next.active_slots.length} slots).`);
    } catch (e) {
      if (mountedRef.current) handleRunnerMutationError("Save runner config", e);
    } finally {
      actionLockRef.current = false;
      if (mountedRef.current) setBusy(false);
    }
  };

  const handleStartShadow = async () => {
    if (actionLockRef.current || busy || effectiveKilled) return;
    actionLockRef.current = true;
    refreshSeqRef.current += 1;
    setBusy(true);
    setRunnerError(null);
    try {
      const next = await startRunner({ mode: "shadow" }, home, status);
      if (!mountedRef.current) return;
      refreshSeqRef.current += 1;
      setStatus(next);
      onRunnerStatusChange?.(next);
      toast.success("Shadow runner started.");
      if (serverSupported) void refreshAll(false);
    } catch (e) {
      if (mountedRef.current) handleRunnerMutationError("Start shadow", e);
    } finally {
      actionLockRef.current = false;
      if (mountedRef.current) setBusy(false);
    }
  };

  const handleStartLiveAuto = async () => {
    if (actionLockRef.current || busy || !liveAutoAllowed) return;
    actionLockRef.current = true;
    refreshSeqRef.current += 1;
    setBusy(true);
    setRunnerError(null);
    try {
      const next = await startRunner({ mode: "live_auto", confirm_auto_live: true }, home, {
        ...status,
        live_armed: effectiveLiveArmed,
        killed: effectiveKilled,
      });
      if (!mountedRef.current) return;
      refreshSeqRef.current += 1;
      setStatus(next);
      onRunnerStatusChange?.(next);
      toast.success("Live Auto runner started under hard risk rails.");
      if (serverSupported) void refreshAll(false);
    } catch (e) {
      if (mountedRef.current) handleRunnerMutationError("Start Live Auto", e);
    } finally {
      actionLockRef.current = false;
      if (mountedRef.current) setBusy(false);
    }
  };

  const handleStopRunner = async () => {
    if (actionLockRef.current || busy) return;
    actionLockRef.current = true;
    refreshSeqRef.current += 1;
    setBusy(true);
    setRunnerError(null);
    try {
      const next = await stopRunner({ cancel_resting: true }, home, status);
      if (!mountedRef.current) return;
      refreshSeqRef.current += 1;
      setStatus(next);
      onRunnerStatusChange?.(next);
      toast.success("Runner stopped and resting quotes canceled.");
      if (serverSupported) void refreshAll(false);
    } catch (e) {
      if (mountedRef.current) handleRunnerMutationError("Stop runner", e);
    } finally {
      actionLockRef.current = false;
      if (mountedRef.current) setBusy(false);
    }
  };

  const updateSlot = (idx: number, patch: Partial<RunnerSlotConfig>) => {
    setConfigDirty(true);
    setDraftSlots((prev) => prev.map((s, i) => (i === idx ? { ...s, ...patch } : s)));
  };

  const addSlot = () => {
    setConfigDirty(true);
    setDraftSlots((prev) => {
      const nextId = prev.length > 0 ? Math.max(...prev.map((s) => s.slot_id)) + 1 : 1;
      const defaultStrat = strategyChoices[prev.length % Math.max(1, strategyChoices.length)] || "late-favourite-edge";
      return [
        ...prev,
        {
          slot_id: nextId,
          strategy: defaultStrat,
          series: ["KXBTC15M"],
          max_contracts: 10,
          execution_mode: "maker",
          auto_pause_drawdown_dollars: 25,
          enabled: true,
        },
      ];
    });
  };

  const removeSlot = (idx: number) => {
    setConfigDirty(true);
    setDraftSlots((prev) => prev.filter((_, i) => i !== idx));
  };

  const q = search.trim().toLowerCase();

  const filteredQuotes = useMemo(() => {
    return status.open_quotes.filter(
      (ord) =>
        !q ||
        ord.ticker.toLowerCase().includes(q) ||
        ord.strategy.toLowerCase().includes(q) ||
        `slot ${ord.slot_id}`.includes(q),
    );
  }, [status.open_quotes, q]);

  const ledgerStrategyOptions = useMemo(() => {
    const set = new Set<string>(strategyChoices);
    for (const e of ledger.entries) set.add(e.strategy);
    for (const s of ledger.summary_by_strategy) set.add(s.strategy);
    return [...set].filter(Boolean).sort();
  }, [strategyChoices, ledger.entries, ledger.summary_by_strategy]);

  const filteredLedgerEntries = useMemo(() => {
    return ledger.entries.filter((e) => {
      if (ledgerStrategyFilter !== "all" && e.strategy !== ledgerStrategyFilter) return false;
      if (ledgerModeFilter !== "all" && e.mode !== ledgerModeFilter) return false;
      if (
        q &&
        !e.ticker.toLowerCase().includes(q) &&
        !e.strategy.toLowerCase().includes(q) &&
        !e.event.toLowerCase().includes(q)
      ) {
        return false;
      }
      return true;
    });
  }, [ledger.entries, ledgerStrategyFilter, ledgerModeFilter, q]);

  const runtimeBySlotId = useMemo(() => {
    const map = new Map<number, (typeof status.active_slots)[number]>();
    for (const s of status.active_slots) {
      map.set(s.slot_id, s);
    }
    return map;
  }, [status.active_slots]);

  const filteredHealthRows = useMemo(() => {
    return executionHealth.strategies.filter(
      (s) =>
        !q ||
        s.strategy.toLowerCase().includes(q) ||
        s.status.toLowerCase().includes(q) ||
        s.reason.toLowerCase().includes(q),
    );
  }, [executionHealth.strategies, q]);

  const handleOverrideQuarantine = useCallback(
    async (strategy: string, action: "reset" | "quarantine") => {
      if (overrideLockRef.current || overrideBusy !== null) return;
      overrideLockRef.current = true;
      refreshSeqRef.current += 1;
      setOverrideBusy(strategy);
      setOverrideError(null);
      try {
        const nextHealth = await overrideQuarantine(strategy, action, "1", undefined, dataset);
        if (!mountedRef.current) return;
        refreshSeqRef.current += 1;
        setExecutionHealth(nextHealth);
        toast.success(
          action === "quarantine"
            ? `Quarantined ${strategy}: runner slot paused and allocator weight zeroed.`
            : `Reset ${strategy} back to active execution.`,
        );
        void refreshAll(false);
      } catch (e) {
        if (!mountedRef.current) return;
        const msg = e instanceof Error ? e.message : String(e);
        setOverrideError(msg);
        toast.error(`Quarantine override failed: ${msg}`);
      } finally {
        overrideLockRef.current = false;
        if (mountedRef.current) setOverrideBusy(null);
      }
    },
    [overrideBusy, refreshAll, dataset],
  );

  return (
    <section
      id="shadow-runner-deck"
      aria-label="Shadow and auto-live runner control deck"
      className="space-y-4"
    >
      {/* Runner Control Header & Aggregate Execution KPIs */}
      <div className="rounded-[10px] border border-hairline bg-surface">
        <div className="flex flex-col gap-3 border-b border-hairline p-4 lg:flex-row lg:items-center lg:justify-between">
          <div className="space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="text-base font-semibold text-ink">Shadow &amp; Auto-Live Runner</h2>
              <StatusPill id="runner-mode-badge" tone={modeMeta.tone}>
                <span>{modeMeta.label}</span>
              </StatusPill>
              <Badge variant="outline" className="mono text-xs">
                {status.source === "server" ? "Server Runner" : "Preview State"}
              </Badge>
              {lastSseEvent && (
                <Badge variant="secondary" className="mono text-xs">
                  SSE: {lastSseEvent}
                </Badge>
              )}
            </div>
            <p className="text-xs text-ink-2">
              Tick-driven quote matching, queue depletion tracking, and fail-closed auto-live routing.
            </p>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button
              id="runner-start-shadow-btn"
              variant={status.mode === "shadow" ? "default" : "outline"}
              size="sm"
              disabled={busy || effectiveKilled}
              onClick={() => void handleStartShadow()}
            >
              <Play className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Start Shadow</span>
            </Button>

            <Button
              id="runner-stop-btn"
              variant="outline"
              size="sm"
              disabled={busy || status.mode === "stopped"}
              onClick={() => void handleStopRunner()}
            >
              <Square className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Stop Runner</span>
            </Button>

            <Button
              id="runner-start-live-btn"
              variant={status.mode === "live_auto" ? "default" : "outline"}
              size="sm"
              disabled={busy || !liveAutoAllowed}
              onClick={() => setLiveConfirmOpen(true)}
              title={
                liveAutoAllowed
                  ? "Open confirmation dialog to start Live Auto execution"
                  : "Arm live trading and clear kill switch to unlock Live Auto"
              }
            >
              <Zap className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Live Auto</span>
            </Button>

            <Button
              id="runner-refresh-btn"
              variant="ghost"
              size="icon"
              aria-label="Refresh runner status"
              disabled={busy}
              onClick={() => {
                setRunnerError(null);
                void refreshAll(false);
              }}
              className="size-11 sm:size-8"
            >
              <RefreshCw className={`size-3.5 ${busy ? "animate-spin" : ""}`} strokeWidth={1.5} aria-hidden />
            </Button>
          </div>
        </div>

        {(runnerError || status.last_error) && (
          <div
            id="runner-error-banner"
            role="alert"
            className="flex flex-wrap items-center justify-between gap-2 border-b border-dot-red/40 bg-board/70 px-4 py-2.5 text-xs text-destructive-foreground"
          >
            <span className="flex items-center gap-1.5 font-medium">
              <AlertTriangle className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
              <span>{runnerError ? runnerError.message : status.last_error}</span>
            </span>
            <div className="flex flex-wrap items-center gap-1.5">
              {runnerError?.status === 403 && !liveAutoAllowed && onRequestArm && (
                <button
                  type="button"
                  onClick={onRequestArm}
                  className="min-h-11 cursor-pointer rounded border border-hairline bg-surface px-2.5 text-xs font-medium text-ink hover:bg-canvas sm:min-h-7"
                >
                  Arm Live
                </button>
              )}
              <button
                type="button"
                id="runner-error-sync-btn"
                onClick={() => {
                  setRunnerError(null);
                  void refreshAll(true);
                }}
                className="min-h-11 cursor-pointer rounded border border-hairline bg-surface px-2.5 text-xs font-medium text-ink hover:bg-canvas sm:min-h-7"
              >
                Sync Status
              </button>
              {runnerError && (
                <button
                  type="button"
                  onClick={() => setRunnerError(null)}
                  className="min-h-11 cursor-pointer rounded border border-hairline bg-surface px-2.5 text-xs font-medium text-ink-2 hover:text-ink sm:min-h-7"
                >
                  Dismiss
                </button>
              )}
            </div>
          </div>
        )}

        {!liveAutoAllowed && (
          <div
            role="status"
            className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/60 px-4 py-2 text-xs text-ink-2"
          >
            <span className="flex items-center gap-1.5">
              <AlertTriangle className="size-3.5 shrink-0 text-warning-foreground" strokeWidth={1.5} aria-hidden />
              <span>
                {effectiveKilled
                  ? "Kill switch is active. Re-arm live trading before starting Shadow or Live Auto."
                  : "Live Auto is locked while live trading is disarmed."}
              </span>
            </span>
            {onRequestArm && (
              <button
                type="button"
                onClick={onRequestArm}
                className="min-h-11 cursor-pointer rounded border border-hairline bg-surface px-2.5 text-xs font-medium text-ink hover:bg-canvas sm:min-h-7"
              >
                Arm Live
              </button>
            )}
          </div>
        )}

        <dl className="grid grid-cols-2 divide-y divide-hairline sm:grid-cols-3 sm:divide-x sm:divide-y-0 lg:grid-cols-6">
          <div className="min-w-0 px-4 py-3">
            <dt className="text-xs text-ink-3">Quotes Posted</dt>
            <dd id="runner-stat-quotes" className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
              {status.stats.quotes_posted}
            </dd>
          </div>
          <div className="min-w-0 px-4 py-3">
            <dt className="text-xs text-ink-3">Fills (Rate)</dt>
            <dd id="runner-stat-fills" className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
              {status.stats.fills} ({status.stats.fill_rate_pct.toFixed(1)}%)
            </dd>
          </div>
          <div className="min-w-0 px-4 py-3">
            <dt className="text-xs text-ink-3">Realized PnL</dt>
            <dd
              id="runner-stat-pnl"
              className={
                "mono mt-0.5 text-sm font-semibold tabular-nums " +
                (status.stats.realized_pnl_dollars > 0
                  ? "text-success-foreground"
                  : status.stats.realized_pnl_dollars < 0
                    ? "text-destructive-foreground"
                    : "text-ink")
              }
            >
              {moneySigned(status.stats.realized_pnl_dollars)}
            </dd>
          </div>
          <div className="min-w-0 px-4 py-3">
            <dt className="text-xs text-ink-3">Open Exposure</dt>
            <dd className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
              {money(status.stats.unrealized_exposure_dollars)}
            </dd>
          </div>
          <div className="min-w-0 px-4 py-3">
            <dt className="text-xs text-ink-3">Avg Slippage</dt>
            <dd className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
              {signedCents(status.stats.avg_fill_slippage_cents)}
            </dd>
          </div>
          <div className="min-w-0 px-4 py-3">
            <dt className="text-xs text-ink-3">Avg Queue Wait</dt>
            <dd className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
              {Math.round(status.stats.avg_queue_wait_secs)}s
            </dd>
          </div>
        </dl>
      </div>

      {/* Slot Configuration & Runtime Status Table */}
      <div id="runner-slots-card" className="overflow-hidden rounded-[10px] border border-hairline bg-surface">
        <div className="flex flex-col gap-3 border-b border-hairline bg-board/50 p-4 lg:flex-row lg:items-center lg:justify-between">
          <div>
            <h3 className="text-sm font-semibold text-ink">Slot Configuration</h3>
            <p className="mt-0.5 text-xs text-ink-2">
              Assign strategy scripts, market series, size caps, and drawdown limits per slot.
            </p>
          </div>

          <div className="flex flex-wrap items-end gap-2">
            <div>
              <label htmlFor="runner-cancel-secs-input" className="block text-xs text-ink-3">
                Cancel Guard (s)
              </label>
              <input
                id="runner-cancel-secs-input"
                type="number"
                min={5}
                max={300}
                value={cancelBeforeCloseSecs}
                onChange={(e) => {
                  setConfigDirty(true);
                  setCancelBeforeCloseSecs(e.target.value);
                }}
                className="mono mt-1 min-h-11 w-24 rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
              />
            </div>
            <div>
              <label htmlFor="runner-bankroll-input" className="block text-xs text-ink-3">
                Bankroll ($)
              </label>
              <input
                id="runner-bankroll-input"
                type="number"
                min={10}
                max={1000000}
                step={10}
                value={bankrollDollars}
                onChange={(e) => {
                  setConfigDirty(true);
                  setBankrollDollars(e.target.value);
                }}
                className="mono mt-1 min-h-11 w-28 rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
              />
            </div>
            <Button
              id="runner-add-slot-btn"
              variant="outline"
              size="sm"
              disabled={busy || draftSlots.length >= 12}
              onClick={addSlot}
            >
              <Plus className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Add Slot</span>
            </Button>
            <Button
              id="runner-save-config-btn"
              variant="default"
              size="sm"
              disabled={busy}
              onClick={() => void handleSaveConfig()}
            >
              <Save className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Save Config</span>
            </Button>
          </div>
        </div>

        <Table className="stack">
          <TableHeader>
            <TableRow>
              <TableHead>Slot</TableHead>
              <TableHead>Strategy</TableHead>
              <TableHead>Series</TableHead>
              <TableHead className="text-right">Max Cts</TableHead>
              <TableHead>Exec Mode</TableHead>
              <TableHead className="text-right">Pause DD ($)</TableHead>
              <TableHead>Enabled</TableHead>
              <TableHead className="text-right">Telemetry</TableHead>
              <TableHead className="text-right">Remove</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {draftSlots.map((slot, idx) => {
              const runtime = runtimeBySlotId.get(slot.slot_id);
              const isPaused = Boolean(runtime?.paused);
              const hasLifecycleBadges =
                isNum(slot.reprice_cooldown_secs) ||
                isNum(slot.max_replaces_per_window) ||
                (isNum(slot.take_profit_cents) && slot.take_profit_cents > 0) ||
                (isNum(slot.stop_loss_cents) && slot.stop_loss_cents > 0) ||
                isNum(slot.exit_before_close_secs);
              return (
                <Fragment key={`slot-${slot.slot_id}-${idx}`}>
                <TableRow data-runner-slot-row={slot.slot_id}>
                  <TableCell className="text-xs font-semibold text-ink">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="mono tabular-nums">#{slot.slot_id}</span>
                      {isPaused ? (
                        <span data-slot-paused-badge={slot.slot_id} className="inline-flex">
                          <StatusPill
                            tone="error"
                            className="min-h-6 px-2 py-0.5 text-xs"
                            title={runtime?.pause_reason || "Auto-paused"}
                          >
                            Paused
                          </StatusPill>
                        </span>
                      ) : slot.enabled ? (
                        <StatusPill tone="success" className="min-h-6 px-2 py-0.5 text-xs">
                          Active
                        </StatusPill>
                      ) : (
                        <StatusPill tone="neutral" className="min-h-6 px-2 py-0.5 text-xs">
                          Off
                        </StatusPill>
                      )}
                    </div>
                    {hasLifecycleBadges && (
                      <div
                        data-slot-lifecycle-badges={slot.slot_id}
                        className="mt-1.5 flex flex-wrap items-center gap-1 font-normal"
                      >
                        {isNum(slot.reprice_cooldown_secs) && (
                          <Badge variant="outline" className="text-xs">
                            Reprice {slot.reprice_cooldown_secs}s
                          </Badge>
                        )}
                        {isNum(slot.max_replaces_per_window) && (
                          <Badge variant="outline" className="text-xs">
                            Max {slot.max_replaces_per_window}x
                          </Badge>
                        )}
                        {isNum(slot.take_profit_cents) && slot.take_profit_cents > 0 && (
                          <Badge variant="success" className="text-xs">
                            TP +{slot.take_profit_cents}c
                          </Badge>
                        )}
                        {isNum(slot.stop_loss_cents) && slot.stop_loss_cents > 0 && (
                          <Badge variant="error" className="text-xs">
                            SL -{slot.stop_loss_cents}c
                          </Badge>
                        )}
                        {isNum(slot.exit_before_close_secs) && (
                          <Badge variant="secondary" className="text-xs">
                            Exit {slot.exit_before_close_secs}s
                          </Badge>
                        )}
                      </div>
                    )}
                    {runtime?.pause_reason && (
                      <div
                        data-slot-pause-reason={slot.slot_id}
                        className="mt-1 text-xs font-normal text-destructive-foreground"
                      >
                        {runtime.pause_reason}
                      </div>
                    )}
                  </TableCell>
                  <TableCell className="m-grow">
                    <select
                      aria-label={`Slot ${slot.slot_id} strategy`}
                      value={slot.strategy}
                      onChange={(e) => updateSlot(idx, { strategy: e.target.value })}
                      className="mono min-h-11 w-full rounded-md border border-hairline bg-board px-2 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 sm:max-w-[220px]"
                    >
                      {strategyChoices.map((st) => (
                        <option key={st} value={st}>
                          {st}
                        </option>
                      ))}
                    </select>
                  </TableCell>
                  <TableCell className="m-full">
                    <div className="flex flex-wrap items-center gap-1" role="group" aria-label={`Slot ${slot.slot_id} series`}>
                      {AVAILABLE_SERIES.map((ser) => {
                        const active = slot.series.includes(ser);
                        return (
                          <button
                            key={ser}
                            type="button"
                            aria-pressed={active}
                            onClick={() => {
                              const next = active
                                ? slot.series.filter((x) => x !== ser)
                                : [...slot.series, ser];
                              updateSlot(idx, { series: next.length > 0 ? next : [ser] });
                            }}
                            className={
                              "mono min-h-11 min-w-11 cursor-pointer rounded border px-2 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-7 sm:min-w-0 " +
                              (active
                                ? "border-blue bg-blue-soft text-info-foreground"
                                : "border-hairline bg-board text-ink-2 hover:text-ink")
                            }
                          >
                            {ser.replace(/^KX/, "").replace(/15M$/, "")}
                          </button>
                        );
                      })}
                    </div>
                  </TableCell>
                  <TableCell className="text-right">
                    <input
                      type="number"
                      aria-label={`Slot ${slot.slot_id} max contracts`}
                      min={1}
                      max={1000}
                      value={slot.max_contracts}
                      onChange={(e) =>
                        updateSlot(idx, { max_contracts: Math.max(1, Math.round(Number(e.target.value) || 1)) })
                      }
                      className="mono min-h-11 w-20 rounded-md border border-hairline bg-board px-2 text-right text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                    />
                  </TableCell>
                  <TableCell>
                    <div className="inline-flex max-w-full flex-wrap rounded-md border border-hairline bg-board p-0.5" role="group" aria-label={`Slot ${slot.slot_id} execution mode`}>
                      {(["maker", "taker"] as const).map((m) => (
                        <button
                          key={m}
                          type="button"
                          aria-pressed={slot.execution_mode === m}
                          onClick={() => updateSlot(idx, { execution_mode: m })}
                          className={
                            "min-h-11 min-w-11 cursor-pointer rounded px-2 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-7 sm:min-w-0 " +
                            (slot.execution_mode === m ? "bg-ink text-canvas" : "text-ink-2 hover:text-ink")
                          }
                        >
                          {m === "maker" ? "Maker" : "Taker"}
                        </button>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell className="text-right">
                    <input
                      type="number"
                      aria-label={`Slot ${slot.slot_id} drawdown limit`}
                      min={1}
                      max={10000}
                      step={1}
                      value={slot.auto_pause_drawdown_dollars}
                      onChange={(e) =>
                        updateSlot(idx, {
                          auto_pause_drawdown_dollars: Math.max(1, Number(e.target.value) || 1),
                        })
                      }
                      className="mono min-h-11 w-20 rounded-md border border-hairline bg-board px-2 text-right text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                    />
                  </TableCell>
                  <TableCell>
                    <button
                      type="button"
                      data-slot-toggle={slot.slot_id}
                      aria-pressed={slot.enabled}
                      onClick={() => updateSlot(idx, { enabled: !slot.enabled })}
                      className={
                        "min-h-11 cursor-pointer rounded-md border px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                        (slot.enabled
                          ? "border-dot-green bg-pill-active-bg text-pill-active-fg"
                          : "border-hairline bg-board text-ink-2")
                      }
                    >
                      {slot.enabled ? "Enabled" : "Disabled"}
                    </button>
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {runtime ? (
                      <div className="space-y-0.5">
                        <div className={runtime.realized_pnl_dollars >= 0 ? "text-success-foreground" : "text-destructive-foreground"}>
                          {moneySigned(runtime.realized_pnl_dollars)}
                        </div>
                        <div className="text-ink-3">
                          {runtime.fills}/{runtime.quotes_posted} fills
                        </div>
                      </div>
                    ) : (
                      "-"
                    )}
                  </TableCell>
                  <TableCell className="m-right text-right">
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={`Remove slot ${slot.slot_id}`}
                      disabled={draftSlots.length <= 1}
                      onClick={() => removeSlot(idx)}
                      className="size-11 sm:size-8"
                    >
                      <Trash2 className="size-3.5 text-ink-2" strokeWidth={1.5} aria-hidden />
                    </Button>
                  </TableCell>
                </TableRow>
                <TableRow data-slot-lifecycle-row={slot.slot_id}>
                  <td colSpan={9} className="m-full bg-board/40 px-3 py-2.5 align-middle text-sm text-ink">
                    <div className="grid w-full min-w-0 grid-cols-2 gap-2.5 sm:grid-cols-3 lg:grid-cols-5">
                      <div className="min-w-0">
                        <label
                          htmlFor={`slot-${slot.slot_id}-reprice-cooldown`}
                          className="block text-xs text-ink-3"
                        >
                          Reprice Cooldown (s)
                        </label>
                        <input
                          id={`slot-${slot.slot_id}-reprice-cooldown`}
                          data-slot-reprice-cooldown={slot.slot_id}
                          type="number"
                          min={0}
                          max={300}
                          step={1}
                          placeholder="Off"
                          value={isNum(slot.reprice_cooldown_secs) ? slot.reprice_cooldown_secs : ""}
                          onChange={(e) =>
                            updateSlot(idx, {
                              reprice_cooldown_secs: parseOptSlotInt(e.target.value, 0, 300),
                            })
                          }
                          className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                        />
                      </div>
                      <div>
                        <label
                          htmlFor={`slot-${slot.slot_id}-max-replaces`}
                          className="block text-xs text-ink-3"
                        >
                          Max Replaces / Win
                        </label>
                        <input
                          id={`slot-${slot.slot_id}-max-replaces`}
                          data-slot-max-replaces={slot.slot_id}
                          type="number"
                          min={0}
                          max={50}
                          step={1}
                          placeholder="Off"
                          value={isNum(slot.max_replaces_per_window) ? slot.max_replaces_per_window : ""}
                          onChange={(e) =>
                            updateSlot(idx, {
                              max_replaces_per_window: parseOptSlotInt(e.target.value, 0, 50),
                            })
                          }
                          className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                        />
                      </div>
                      <div>
                        <label
                          htmlFor={`slot-${slot.slot_id}-take-profit`}
                          className="block text-xs text-ink-3"
                        >
                          Take Profit (c)
                        </label>
                        <input
                          id={`slot-${slot.slot_id}-take-profit`}
                          data-slot-take-profit={slot.slot_id}
                          type="number"
                          min={1}
                          max={99}
                          step={1}
                          placeholder="Off"
                          value={isNum(slot.take_profit_cents) && slot.take_profit_cents > 0 ? slot.take_profit_cents : ""}
                          onChange={(e) =>
                            updateSlot(idx, {
                              take_profit_cents: parseOptSlotInt(e.target.value, 1, 99),
                            })
                          }
                          className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                        />
                      </div>
                      <div>
                        <label
                          htmlFor={`slot-${slot.slot_id}-stop-loss`}
                          className="block text-xs text-ink-3"
                        >
                          Stop Loss (c)
                        </label>
                        <input
                          id={`slot-${slot.slot_id}-stop-loss`}
                          data-slot-stop-loss={slot.slot_id}
                          type="number"
                          min={1}
                          max={99}
                          step={1}
                          placeholder="Off"
                          value={isNum(slot.stop_loss_cents) && slot.stop_loss_cents > 0 ? slot.stop_loss_cents : ""}
                          onChange={(e) =>
                            updateSlot(idx, {
                              stop_loss_cents: parseOptSlotInt(e.target.value, 1, 99),
                            })
                          }
                          className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                        />
                      </div>
                      <div>
                        <label
                          htmlFor={`slot-${slot.slot_id}-exit-before-close`}
                          className="block text-xs text-ink-3"
                        >
                          Exit Before Close (s)
                        </label>
                        <input
                          id={`slot-${slot.slot_id}-exit-before-close`}
                          data-slot-exit-before-close={slot.slot_id}
                          type="number"
                          min={0}
                          max={600}
                          step={1}
                          placeholder="Off"
                          value={isNum(slot.exit_before_close_secs) ? slot.exit_before_close_secs : ""}
                          onChange={(e) =>
                            updateSlot(idx, {
                              exit_before_close_secs: parseOptSlotInt(e.target.value, 0, 600),
                            })
                          }
                          className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                        />
                      </div>
                    </div>
                  </td>
                </TableRow>
                </Fragment>
              );
            })}
          </TableBody>
        </Table>
      </div>

      {/* Execution Health & Adverse-Selection Markout Telemetry */}
      <div
        id="execution-health-panel"
        data-testid="execution-health-panel"
        className="overflow-hidden rounded-[10px] border border-hairline bg-surface"
      >
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-3">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="text-sm font-semibold text-ink">Execution Health &amp; Markout</h3>
              <StatusPill
                id="exec-health-summary-pill"
                tone={
                  executionHealth.overall.quarantined_count > 0
                    ? "error"
                    : executionHealth.overall.throttled_count > 0
                      ? "warning"
                      : "success"
                }
              >
                {executionHealth.overall.quarantined_count > 0
                  ? `${executionHealth.overall.quarantined_count} Quarantined`
                  : executionHealth.overall.throttled_count > 0
                    ? `${executionHealth.overall.throttled_count} Throttled`
                    : "Healthy"}
              </StatusPill>
            </div>
            <p className="mt-0.5 text-xs text-ink-2">
              Post-fill +10s/+30s/+60s markout curves, predicted vs realized win and fill residuals, and quarantine overrides.
            </p>
          </div>
          <Button
            id="exec-health-refresh-btn"
            variant="outline"
            size="sm"
            disabled={busy || overrideBusy !== null}
            onClick={() => void refreshAll(false)}
          >
            <RefreshCw className={`size-3.5 ${busy ? "animate-spin" : ""}`} strokeWidth={1.5} aria-hidden />
            <span>Refresh Health</span>
          </Button>
        </div>

        <dl className="grid grid-cols-2 divide-y divide-hairline border-b border-hairline bg-board/25 text-xs sm:grid-cols-4 sm:divide-x sm:divide-y-0">
          <div className="min-w-0 px-4 py-2.5">
            <dt className="text-ink-2">Strategy Health</dt>
            <dd id="exec-health-counts" className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
              {executionHealth.overall.active_count}A / {executionHealth.overall.throttled_count}T / {executionHealth.overall.quarantined_count}Q
            </dd>
          </div>
          <div className="min-w-0 px-4 py-2.5">
            <dt className="text-ink-2">Rolling Fills</dt>
            <dd id="exec-health-fills" className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
              {executionHealth.overall.rolling_fills}
            </dd>
          </div>
          <div className="min-w-0 px-4 py-2.5">
            <dt className="text-ink-2">Avg +30s Markout</dt>
            <dd
              id="exec-health-avg-markout"
              className={
                "mono mt-0.5 text-sm font-semibold tabular-nums " +
                (executionHealth.overall.avg_markout_30s_cents === null
                  ? "text-ink-3"
                  : executionHealth.overall.avg_markout_30s_cents >= 0
                    ? "text-success-foreground"
                    : "text-destructive-foreground")
              }
            >
              {fmtOptCents(executionHealth.overall.avg_markout_30s_cents)}
            </dd>
          </div>
          <div className="min-w-0 px-4 py-2.5">
            <dt className="text-ink-2">Avg Slippage Residual</dt>
            <dd
              id="exec-health-avg-slip-res"
              className={
                "mono mt-0.5 text-sm font-semibold tabular-nums " +
                (executionHealth.overall.avg_slippage_residual_cents === null
                  ? "text-ink-3"
                  : executionHealth.overall.avg_slippage_residual_cents <= 0
                    ? "text-success-foreground"
                    : "text-destructive-foreground")
              }
            >
              {fmtOptCents(executionHealth.overall.avg_slippage_residual_cents)}
            </dd>
          </div>
        </dl>

        {overrideError && (
          <div
            id="exec-health-error"
            role="alert"
            className="border-b border-hairline bg-board px-4 py-2.5 text-xs text-destructive-foreground"
          >
            Quarantine override failed: {overrideError}
          </div>
        )}

        {filteredHealthRows.length === 0 ? (
          <p role="status" className="p-4 text-xs text-ink-2">
            No execution health telemetry recorded for the current filter yet.
          </p>
        ) : (
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Strategy</TableHead>
                <TableHead className="text-right">Win Rate (Pred / Real)</TableHead>
                <TableHead className="text-right">Fill Rate (Assumed / Actual)</TableHead>
                <TableHead className="text-right">Slippage (Exp / Real / Res)</TableHead>
                <TableHead>Markout (+10s / +30s / +60s)</TableHead>
                <TableHead className="text-right">Fills / Net PnL</TableHead>
                <TableHead className="text-right">Override</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {filteredHealthRows.map((row) => {
                const badgeMeta = healthStatusProps(row.status);
                const winPredPct =
                  row.predicted_prob !== null
                    ? row.predicted_prob <= 1
                      ? row.predicted_prob * 100
                      : row.predicted_prob
                    : null;
                const winRealPct =
                  row.realized_win_rate !== null
                    ? row.realized_win_rate <= 1
                      ? row.realized_win_rate * 100
                      : row.realized_win_rate
                    : null;
                const winDeltaPp =
                  winPredPct !== null && winRealPct !== null ? winRealPct - winPredPct : null;
                const fillDeltaPp =
                  row.assumed_fill_rate_pct !== null && row.realized_fill_rate_pct !== null
                    ? row.realized_fill_rate_pct - row.assumed_fill_rate_pct
                    : null;
                const slipResidual =
                  row.expected_slippage_cents !== null && row.realized_slippage_cents !== null
                    ? row.realized_slippage_cents - row.expected_slippage_cents
                    : null;
                const rowBusy = overrideBusy === row.strategy;
                return (
                  <TableRow key={row.strategy} data-health-row={row.strategy}>
                    <TableCell className="m-grow max-w-[240px] text-xs">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span className="font-semibold text-ink break-all">{row.strategy}</span>
                        <span data-health-status-badge={row.strategy} className="inline-flex">
                          <StatusPill
                            tone={badgeMeta.tone}
                            className="min-h-6 px-2 py-0.5 text-xs"
                          >
                            {badgeMeta.label}
                          </StatusPill>
                        </span>
                        {row.status === "throttled" && (
                          <span className="mono tabular-nums text-xs text-warning-foreground">
                            {row.throttle_mult.toFixed(2)}x
                          </span>
                        )}
                      </div>
                      {row.reason && (
                        <div className="mt-0.5 break-words text-xs text-ink-2">{row.reason}</div>
                      )}
                    </TableCell>
                    <TableCell className="mono tabular-nums text-right text-xs">
                      <div className="text-ink">
                        {fmtOptProbPct(row.predicted_prob)} / {fmtOptProbPct(row.realized_win_rate)}
                      </div>
                      <div
                        className={
                          winDeltaPp === null
                            ? "text-ink-3"
                            : winDeltaPp >= 0
                              ? "text-success-foreground"
                              : "text-destructive-foreground"
                        }
                      >
                        {fmtOptPp(winDeltaPp)}
                      </div>
                    </TableCell>
                    <TableCell className="mono tabular-nums text-right text-xs">
                      <div className="text-ink">
                        {fmtOptPct(row.assumed_fill_rate_pct)} / {fmtOptPct(row.realized_fill_rate_pct)}
                      </div>
                      <div
                        className={
                          fillDeltaPp === null
                            ? "text-ink-3"
                            : fillDeltaPp >= -5
                              ? "text-success-foreground"
                              : "text-destructive-foreground"
                        }
                      >
                        {fmtOptPp(fillDeltaPp)}
                      </div>
                    </TableCell>
                    <TableCell className="mono tabular-nums text-right text-xs">
                      <div className="text-ink">
                        {fmtOptCents(row.expected_slippage_cents)} / {fmtOptCents(row.realized_slippage_cents)}
                      </div>
                      <div
                        className={
                          slipResidual === null
                            ? "text-ink-3"
                            : slipResidual <= 0
                              ? "text-success-foreground"
                              : "text-destructive-foreground"
                        }
                      >
                        Res {fmtOptCents(slipResidual)}
                      </div>
                    </TableCell>
                    <TableCell className="m-full text-xs">
                      <div className="flex min-w-0 flex-wrap items-center gap-2">
                        <MarkoutSparkline
                          m10={row.markout_10s_cents}
                          m30={row.markout_30s_cents}
                          m60={row.markout_60s_cents}
                        />
                        <div className="mono min-w-0 tabular-nums space-y-0.5">
                          <div className="text-ink">
                            {fmtOptCents(row.markout_10s_cents)} /{" "}
                            <span
                              className={
                                row.markout_30s_cents === null
                                  ? "text-ink-3"
                                  : row.markout_30s_cents >= 0
                                    ? "font-semibold text-success-foreground"
                                    : "font-semibold text-destructive-foreground"
                              }
                            >
                              {fmtOptCents(row.markout_30s_cents)}
                            </span>{" "}
                            / {fmtOptCents(row.markout_60s_cents)}
                          </div>
                          {row.settlement_markout_cents !== null && (
                            <div className="text-ink-2">
                              Settle {fmtOptCents(row.settlement_markout_cents)}
                            </div>
                          )}
                        </div>
                      </div>
                    </TableCell>
                    <TableCell className="mono tabular-nums text-right text-xs">
                      <div className="text-ink-2">{row.rolling_fills} fills</div>
                      <div
                        className={
                          row.rolling_net_pnl_dollars >= 0
                            ? "font-semibold text-success-foreground"
                            : "font-semibold text-destructive-foreground"
                        }
                      >
                        {moneySigned(row.rolling_net_pnl_dollars)}
                      </div>
                    </TableCell>
                    <TableCell className="m-right text-right">
                      <div className="inline-flex flex-wrap items-center justify-end gap-1.5">
                        {row.status !== "active" && (
                          <Button
                            data-reset-quarantine-btn={row.strategy}
                            variant="outline"
                            size="sm"
                            disabled={overrideBusy !== null || rowBusy}
                            onClick={() => void handleOverrideQuarantine(row.strategy, "reset")}
                          >
                            <RotateCcw className="size-3.5" strokeWidth={1.5} aria-hidden />
                            <span>Reset</span>
                          </Button>
                        )}
                        {row.status !== "quarantined" && (
                          <Button
                            data-quarantine-btn={row.strategy}
                            variant="danger"
                            size="sm"
                            disabled={overrideBusy !== null || rowBusy}
                            onClick={() => void handleOverrideQuarantine(row.strategy, "quarantine")}
                          >
                            <ShieldBan className="size-3.5" strokeWidth={1.5} aria-hidden />
                            <span>Quarantine</span>
                          </Button>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </div>

      {/* Live Resting Quotes with Queue-Ahead Bar & Fill ETA */}
      <div id="runner-open-quotes-card" className="overflow-hidden rounded-[10px] border border-hairline bg-surface">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-3">
          <div>
            <h3 className="text-sm font-semibold text-ink">Live Resting Quotes</h3>
            <p className="mt-0.5 text-xs text-ink-2">
              Active limit orders with queue-ahead depth depletion and estimated fill time.
            </p>
          </div>
          <span className="mono text-xs text-ink-3">{filteredQuotes.length} resting</span>
        </div>

        {filteredQuotes.length === 0 ? (
          <p role="status" className="p-4 text-xs text-ink-2">
            No active resting quotes right now. Start Shadow or Live Auto to stage slot quotes.
          </p>
        ) : (
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Ticker</TableHead>
                <TableHead>Slot / Strategy</TableHead>
                <TableHead>Side / Mode</TableHead>
                <TableHead className="text-right">Limit / Qty</TableHead>
                <TableHead>Queue Ahead</TableHead>
                <TableHead className="text-right">Fill ETA</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {filteredQuotes.map((ord) => {
                const qInit = Math.max(1, ord.queue_ahead_initial || ord.queue_ahead_remaining || 1);
                const qRem = Math.max(0, ord.queue_ahead_remaining);
                const depletedPct = Math.min(100, Math.max(0, Math.round(((qInit - qRem) / qInit) * 100)));
                return (
                  <TableRow key={ord.order_id} data-runner-quote-row={ord.order_id}>
                    <TableCell className="mono m-grow max-w-[210px] break-all text-xs font-semibold text-ink">
                      {ord.ticker}
                    </TableCell>
                    <TableCell className="text-xs text-ink-2">
                      <span className="mono font-medium text-ink">#{ord.slot_id}</span>{" "}
                      <span>{ord.strategy}</span>
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-wrap items-center gap-1.5">
                        <Badge
                          variant={ord.action === "sell" ? "warning" : "outline"}
                          data-quote-action={ord.order_id}
                          className="text-xs"
                        >
                          {ord.action === "sell" ? "SELL" : "BUY"}
                        </Badge>
                        <Badge variant="outline" className="text-xs">
                          {ord.side.toUpperCase()}
                        </Badge>
                        <Badge variant={ord.mode === "live" ? "warning" : "secondary"} className="text-xs">
                          {ord.mode === "live" ? "Live" : "Shadow"}
                        </Badge>
                        <Badge
                          variant={ord.replace_count > 0 ? "warning" : "outline"}
                          data-quote-replace-count={ord.order_id}
                          className="text-xs"
                          title={`Order repriced ${ord.replace_count} time${ord.replace_count === 1 ? "" : "s"}`}
                        >
                          {ord.replace_count > 0 ? `Repriced ${ord.replace_count}x` : "0 reprices"}
                        </Badge>
                      </div>
                    </TableCell>
                    <TableCell className="mono tabular-nums text-right text-xs font-semibold text-ink">
                      {ord.limit_cents}c x {ord.remaining_contracts}/{ord.contracts}
                    </TableCell>
                    <TableCell className="m-full min-w-[160px]">
                      <div className="space-y-1">
                        <div className="flex items-center justify-between gap-2 text-xs">
                          <span className="mono tabular-nums text-ink-2">{qRem} / {ord.queue_ahead_initial} ahead</span>
                          <span className="mono tabular-nums text-ink-3">{depletedPct}%</span>
                        </div>
                        <div
                          role="progressbar"
                          aria-label={`Queue ahead for ${ord.ticker}`}
                          aria-valuenow={depletedPct}
                          aria-valuemin={0}
                          aria-valuemax={100}
                          className="h-1.5 w-full overflow-hidden rounded-full bg-track"
                        >
                          <div
                            className="h-full rounded-full bg-blue"
                            style={{ width: `${Math.max(4, depletedPct)}%` }}
                          />
                        </div>
                      </div>
                    </TableCell>
                    <TableCell className="mono text-right text-xs font-medium text-ink">
                      {formatEta(ord.est_fill_eta_secs)}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </div>

      {/* Strategy Execution Stats Rollup */}
      <div id="runner-strategy-stats-card" className="overflow-hidden rounded-[10px] border border-hairline bg-surface">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-3">
          <div>
            <h3 className="text-sm font-semibold text-ink">Strategy Execution Rollup</h3>
            <p className="mt-0.5 text-xs text-ink-2">
              Fill rate, slippage, queue wait time, and settled PnL per strategy.
            </p>
          </div>
          <span className="mono text-xs text-ink-3">{ledger.summary_by_strategy.length} strategies</span>
        </div>

        {ledger.summary_by_strategy.length === 0 ? (
          <p role="status" className="p-4 text-xs text-ink-2">
            No strategy execution stats recorded yet.
          </p>
        ) : (
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Strategy</TableHead>
                <TableHead className="text-right">Quotes</TableHead>
                <TableHead className="text-right">Fills</TableHead>
                <TableHead className="text-right">Fill Rate</TableHead>
                <TableHead className="text-right">Settled / W-L</TableHead>
                <TableHead className="text-right">Avg Slip</TableHead>
                <TableHead className="text-right">Avg Wait</TableHead>
                <TableHead className="text-right">Fees</TableHead>
                <TableHead className="text-right">Realized PnL</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {ledger.summary_by_strategy.map((st) => (
                <TableRow key={st.strategy} data-runner-stat-strategy={st.strategy}>
                  <TableCell className="mono m-grow max-w-[220px] break-all text-xs font-semibold text-ink">
                    {st.strategy}
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">{st.quotes_posted}</TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink">{st.fills}</TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink">{st.fill_rate_pct.toFixed(1)}%</TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {st.windows_settled > 0 ? `${st.windows_settled} (${st.wins}/${st.losses})` : `${st.wins}/${st.losses}`}
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {signedCents(st.avg_fill_slippage_cents)}
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {Math.round(st.avg_queue_wait_secs)}s
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {money(st.total_fees_dollars)}
                  </TableCell>
                  <TableCell
                    className={
                      "mono tabular-nums text-right text-xs font-semibold " +
                      (st.realized_pnl_dollars > 0
                        ? "text-success-foreground"
                        : st.realized_pnl_dollars < 0
                          ? "text-destructive-foreground"
                          : "text-ink-2")
                    }
                  >
                    {moneySigned(st.realized_pnl_dollars)}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>

      {/* Persistent Fill & Settlement Ledger */}
      <div id="runner-ledger-card" className="overflow-hidden rounded-[10px] border border-hairline bg-surface">
        <div className="flex flex-col gap-3 border-b border-hairline bg-board/50 p-4 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h3 className="text-sm font-semibold text-ink">Persistent Fill &amp; Settlement Ledger</h3>
            <p className="mt-0.5 text-xs text-ink-2">
              Append-only record of quotes, amendments, fills, exits, cancels, settlements, and slot pauses.
            </p>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <select
              id="runner-ledger-strategy-filter"
              aria-label="Filter ledger by strategy"
              value={ledgerStrategyFilter}
              onChange={(e) => setLedgerStrategyFilter(e.target.value)}
              className="mono min-h-11 rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
            >
              <option value="all">All strategies</option>
              {ledgerStrategyOptions.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>

            <div className="inline-flex rounded-md border border-hairline bg-surface p-0.5" role="group" aria-label="Filter ledger by mode">
              {(
                [
                  ["all", "All"],
                  ["shadow", "Shadow"],
                  ["live", "Live"],
                ] as const
              ).map(([m, label]) => (
                <button
                  key={m}
                  type="button"
                  data-runner-ledger-mode={m}
                  aria-pressed={ledgerModeFilter === m}
                  onClick={() => setLedgerModeFilter(m)}
                  className={
                    "min-h-11 min-w-11 cursor-pointer rounded px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-7 sm:min-w-0 " +
                    (ledgerModeFilter === m ? "bg-ink text-canvas" : "text-ink-2 hover:text-ink")
                  }
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
        </div>

        {filteredLedgerEntries.length === 0 ? (
          <p role="status" className="p-4 text-xs text-ink-2">
            No ledger entries match the current filter.
          </p>
        ) : (
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Time (UTC)</TableHead>
                <TableHead>Event</TableHead>
                <TableHead>Mode</TableHead>
                <TableHead>Slot / Strategy</TableHead>
                <TableHead>Ticker</TableHead>
                <TableHead className="text-right">Quote / Qty</TableHead>
                <TableHead className="text-right">Slip / Wait</TableHead>
                <TableHead className="text-right">Fee</TableHead>
                <TableHead className="text-right">PnL</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {filteredLedgerEntries.slice(0, 100).map((ev) => (
                <TableRow key={ev.id} data-runner-ledger-row={ev.id}>
                  <TableCell className="mono text-xs text-ink-2">
                    {shortUtcTime(ev.ts_iso, ev.ts_epoch)}
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant={
                        ev.event === "SlotPaused"
                          ? "error"
                          : ev.event === "OrderAmended"
                            ? "warning"
                            : ev.event === "PositionExited"
                              ? isNum(ev.pnl_dollars) && ev.pnl_dollars < 0
                                ? "error"
                                : "success"
                              : ev.event === "OrderFilled" || ev.event === "WindowSettled"
                                ? "success"
                                : "outline"
                      }
                      data-ledger-event={ev.event}
                      className="text-xs"
                    >
                      {ev.event}
                    </Badge>
                  </TableCell>
                  <TableCell>
                    <Badge variant={ev.mode === "live" ? "warning" : "secondary"} className="text-xs">
                      {ev.mode === "live" ? "Live" : "Shadow"}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-xs text-ink-2">
                    <span className="mono font-medium text-ink">#{ev.slot_id}</span> {ev.strategy}
                  </TableCell>
                  <TableCell className="mono m-grow max-w-[200px] break-all text-xs text-ink">
                    {ev.ticker}
                  </TableCell>
                  <TableCell className="mono text-right text-xs text-ink">
                    {ev.action ? `${ev.action.toUpperCase()} ` : ""}
                    {ev.side ? `${ev.side.toUpperCase()} ` : ""}
                    {isNum(ev.price_cents) ? `${Math.round(ev.price_cents)}c` : "-"}
                    {isNum(ev.contracts) ? ` x ${ev.contracts}` : ""}
                    {isNum(ev.replace_count) && ev.replace_count > 0 ? ` (R${ev.replace_count})` : ""}
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {isNum(ev.slippage_cents) ? signedCents(ev.slippage_cents) : "-"} /{" "}
                    {isNum(ev.queue_wait_secs) ? `${Math.round(ev.queue_wait_secs)}s` : "-"}
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {isNum(ev.fee_dollars) ? money(ev.fee_dollars) : "-"}
                  </TableCell>
                  <TableCell
                    className={
                      "mono text-right text-xs font-semibold " +
                      (isNum(ev.pnl_dollars)
                        ? ev.pnl_dollars > 0
                          ? "text-success-foreground"
                          : ev.pnl_dollars < 0
                            ? "text-destructive-foreground"
                            : "text-ink-2"
                        : "text-ink-3")
                    }
                  >
                    {isNum(ev.pnl_dollars) ? moneySigned(ev.pnl_dollars) : ev.reason || "-"}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>

      <LiveAutoConfirmModal
        open={liveConfirmOpen}
        onOpenChange={setLiveConfirmOpen}
        liveStatus={liveStatus}
        liveAutoAllowed={liveAutoAllowed}
        busy={busy}
        onConfirm={() => void handleStartLiveAuto()}
      />
    </section>
  );
}
