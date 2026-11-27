// OpenBell v0.8.0 Live Kalshi v2 Account Strip, Paper/Live Mode Switcher, Arm Confirmation Modal,
// Live Order Ticket with Real-Time Hard Risk Rails, Positions/Orders/Fills tables, Kill Switch,
// and Settings API & Risk Rails configuration panel. NEVER echoes private key PEM bytes.
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  Activity,
  AlertOctagon,
  AlertTriangle,
  Check,
  CheckCircle2,
  KeyRound,
  Layers,
  Lock,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
  SlidersHorizontal,
  Unlock,
  X,
  XCircle,
} from "lucide-react";
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
import { OrderbookLadder } from "@/OrderbookLadder";
import {
  type Market,
  ageWords,
  cleanStr,
  isNum,
  money,
  moneySigned,
  timeUtcFullLabel,
} from "@/lib/desk";
import {
  type AmendLiveOrderPayload,
  type FlattenLivePositionPayload,
  type LiveConfigPayload,
  type LiveDeskBundle,
  type LiveEnvironment,
  type LiveOrder,
  type LiveOrderPayload,
  type LivePosition,
  type LiveReconcileResponse,
  type LiveStatus,
  type VenueSyncMode,
  amendLiveOrder,
  defaultLiveReconcile,
  evaluateOrderRails,
  fetchLiveReconcile,
  flattenLivePosition,
  resetKillSwitch,
  triggerLiveReconcile,
} from "@/lib/live";
import { deriveSideBookLevels, useOrderbook, walkBookLevels } from "@/lib/orderbook";
import type { RunnerMode } from "@/lib/runner";

function shortTs(iso: string): string {
  if (!iso) return "-";
  return iso.length >= 19 ? iso.slice(5, 19).replace("T", " ") + "Z" : iso;
}

function validEpochSecs(raw: number | null | undefined): number | null {
  return isNum(raw) && raw > 0 && raw <= 8_640_000_000_000 ? raw : null;
}

function epochAgeSecs(epoch: number | null, nowMs: number): number | null {
  if (epoch === null || !isNum(nowMs)) return null;
  return Math.max(0, Math.floor(nowMs / 1000 - epoch));
}

function useClockTickMs(resetKey: string): number {
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    setNowMs(Date.now());
    const id = window.setInterval(() => setNowMs(Date.now()), 15000);
    return () => window.clearInterval(id);
  }, [resetKey]);
  return nowMs;
}

function venueSyncBadgeInfo(status: LiveStatus): {
  mode: VenueSyncMode;
  label: string;
  tone: "success" | "info" | "neutral";
  pillClassName: string;
  hint: string;
} {
  const mode: VenueSyncMode =
    status.venueSyncMode === "ws_stream" || status.privateWsConnected
      ? "ws_stream"
      : status.venueSyncMode === "rest_poll"
        ? "rest_poll"
        : "offline";
  if (mode === "ws_stream") {
    return {
      mode,
      label: "WS Stream",
      tone: "success",
      pillClassName: "min-h-7 border-dot-green/50 px-2.5 py-0.5 text-xs text-success-foreground",
      hint: "Private Kalshi WebSocket execution stream active",
    };
  }
  if (mode === "rest_poll") {
    return {
      mode,
      label: "REST Poll",
      tone: "info",
      pillClassName: "min-h-7 px-2.5 py-0.5 text-xs text-info-foreground",
      hint: "REST polling fallback (private WS disconnected)",
    };
  }
  return {
    mode: "offline",
    label: "Offline",
    tone: "neutral",
    pillClassName: "min-h-7 px-2.5 py-0.5 text-xs text-ink-2",
    hint: "Private venue stream offline",
  };
}

// ---------------------------------------------------------------------------
// Arm Live Trading Explicit Confirmation Modal (Radix Dialog)
// ---------------------------------------------------------------------------

export function ArmLiveModal({
  open,
  onOpenChange,
  status,
  busy,
  onConfirmArm,
  onOpenSettings,
  onOpenResetKill,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  status: LiveStatus;
  busy: boolean;
  onConfirmArm: () => void;
  onOpenSettings: () => void;
  onOpenResetKill?: () => void;
}) {
  const [acknowledged, setAcknowledged] = useState(false);

  useEffect(() => {
    if (!open) {
      setAcknowledged(false);
      return;
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onOpenChange(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onOpenChange]);

  if (!open) return null;

  const envLabel = status.environment === "prod" ? "PRODUCTION (Real Kalshi Funds)" : "DEMO (demo-api.kalshi.co)";

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[10vh]">
      <div
        aria-hidden="true"
        className="fixed inset-0 bg-ink/40"
        onClick={() => onOpenChange(false)}
      />
      <div
        id="arm-live-modal"
        data-arm-live-modal
        role="dialog"
        aria-modal="true"
        aria-labelledby="arm-live-title"
        className="relative z-10 w-[calc(100%-24px)] max-w-[520px] overflow-hidden rounded-[12px] border border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
      >
        <div className="flex items-start justify-between gap-3 border-b border-hairline px-5 py-4">
          <div className="flex items-start gap-2.5">
            <ShieldAlert className="mt-0.5 size-5 shrink-0 text-warning-foreground" strokeWidth={1.5} aria-hidden />
            <div>
              <h2 id="arm-live-title" className="text-base font-semibold text-ink">
                Arm Live Kalshi v2 Trading
              </h2>
              <p className="mt-0.5 text-xs text-ink-2">
                Confirm active venue environment and hard risk rails before enabling order routing.
              </p>
            </div>
          </div>
          <Button
            variant="ghost"
            size="icon"
            aria-label="Close arm confirmation"
            className="size-11 shrink-0 sm:size-9"
            onClick={() => onOpenChange(false)}
          >
            <X className="size-4" strokeWidth={1.5} aria-hidden />
          </Button>
        </div>

        <div className="space-y-4 px-5 py-4">
          <div className="border-b border-hairline pb-3">
            <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
              <span className="text-ink-3">Venue Environment</span>
              <Badge
                variant="outline"
                className={
                  "mono text-xs " +
                  (status.environment === "prod"
                    ? "border-dot-red text-destructive-foreground"
                    : "border-blue bg-blue-soft text-info-foreground")
                }
              >
                {envLabel}
              </Badge>
            </div>
            <div className="mt-2 flex flex-wrap items-center justify-between gap-2 text-xs">
              <span className="text-ink-3">API Credentials</span>
              <span className="mono font-medium text-ink">
                {status.configured
                  ? `${status.keyIdMasked || "Configured"} (PEM loaded)`
                  : "Not configured yet"}
              </span>
            </div>
          </div>

          {status.killed ? (
            <div role="alert" className="border-b border-hairline pb-3 text-xs text-destructive-foreground">
              Kill switch latch is active{status.killReason ? ` (${status.killReason})` : ""}. Reset the kill latch first before arming live order routing; clearing the latch keeps live trading disarmed.
            </div>
          ) : !status.configured ? (
            <div role="alert" className="rounded-lg border border-dot-amber/50 bg-board p-3 text-xs text-warning-foreground">
              Kalshi v2 API credentials are not saved yet. Open Desk Settings to enter your API Key ID and local PEM file path before arming live trading.
            </div>
          ) : (
            <>
              <dl className="grid grid-cols-2 gap-3 border-b border-hairline pb-3 text-xs">
                <div>
                  <dt className="text-ink-3">Max Contracts / Order</dt>
                  <dd className="mono mt-0.5 font-semibold text-ink">{status.rails.maxOrderContracts}</dd>
                </div>
                <div>
                  <dt className="text-xs text-ink-3">Max Order Notional</dt>
                  <dd className="mono mt-0.5 font-semibold text-ink">{money(status.rails.maxOrderDollars)}</dd>
                </div>
                <div>
                  <dt className="text-ink-3">Max Daily Loss</dt>
                  <dd className="mono mt-0.5 font-semibold text-ink">{money(status.rails.maxDailyLossDollars)}</dd>
                </div>
                <div>
                  <dt className="text-ink-3">Max Open Exposure</dt>
                  <dd className="mono mt-0.5 font-semibold text-ink">{money(status.rails.maxOpenExposureDollars)}</dd>
                </div>
                <div className="col-span-2">
                  <dt className="text-ink-3">Allowed Ticker Prefix</dt>
                  <dd className="mono mt-0.5 font-semibold text-ink">
                    {status.rails.allowedTickerPrefix || "(any Kalshi ticker)"}
                  </dd>
                </div>
              </dl>

              <label className="flex min-h-11 cursor-pointer items-start gap-2.5 rounded-lg border border-hairline bg-board/60 p-3 text-xs text-ink">
                <input
                  id="arm-live-checkbox"
                  type="checkbox"
                  checked={acknowledged}
                  onChange={(e) => setAcknowledged(e.target.checked)}
                  className="mt-0.5 size-4 shrink-0 accent-blue"
                />
                <span>
                  I reviewed these hard risk rails and authorize live Kalshi v2 order routing ({status.environment.toUpperCase()}). Emergency Kill Switch remains one click away.
                </span>
              </label>
            </>
          )}
        </div>

        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-hairline bg-board/50 px-5 py-3">
          <Button
            variant="outline"
            className="min-h-11 text-xs sm:min-h-9"
            onClick={() => onOpenChange(false)}
          >
            Stay in Paper Mode
          </Button>
          {status.killed && onOpenResetKill ? (
            <Button
              variant="default"
              className="min-h-11 gap-1.5 text-xs sm:min-h-9"
              onClick={() => {
                onOpenChange(false);
                onOpenResetKill();
              }}
            >
              <ShieldCheck className="size-3.5" strokeWidth={1.5} aria-hidden />
              Reset Kill Latch
            </Button>
          ) : !status.configured ? (
            <Button
              variant="default"
              className="min-h-11 gap-1.5 text-xs sm:min-h-9"
              onClick={() => {
                onOpenChange(false);
                onOpenSettings();
              }}
            >
              <KeyRound className="size-3.5" strokeWidth={1.5} aria-hidden />
              Configure Kalshi API
            </Button>
          ) : (
            <Button
              id="confirm-arm-live-btn"
              variant="default"
              disabled={!acknowledged || busy || status.killed}
              className="min-h-11 gap-1.5 text-xs sm:min-h-9"
              onClick={() => {
                onConfirmArm();
                onOpenChange(false);
              }}
            >
              <Unlock className="size-3.5" strokeWidth={1.5} aria-hidden />
              {busy ? "Arming…" : "Confirm & Arm Live"}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Explicit Fail-Closed Kill Latch Reset Confirmation Modal
// ---------------------------------------------------------------------------

export function ResetKillLatchModal({
  open,
  onOpenChange,
  status,
  busy,
  onConfirmReset,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  status: LiveStatus;
  busy: boolean;
  onConfirmReset: () => Promise<void>;
}) {
  const [acknowledged, setAcknowledged] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const lockRef = useRef(false);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (!open) {
      setAcknowledged(false);
      setSubmitting(false);
      setErrorMsg(null);
      lockRef.current = false;
      return;
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onOpenChange(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onOpenChange]);

  if (!open) return null;

  const handleConfirm = async () => {
    if (!acknowledged || submitting || busy || lockRef.current) return;
    lockRef.current = true;
    setSubmitting(true);
    setErrorMsg(null);
    try {
      await onConfirmReset();
      if (mountedRef.current) onOpenChange(false);
    } catch (e) {
      if (mountedRef.current) {
        setErrorMsg(e instanceof Error ? e.message : String(e));
      }
    } finally {
      lockRef.current = false;
      if (mountedRef.current) setSubmitting(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[10vh]">
      <div
        aria-hidden="true"
        className="fixed inset-0 bg-ink/40"
        onClick={() => onOpenChange(false)}
      />
      <div
        id="reset-kill-modal"
        data-testid="reset-kill-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="reset-kill-title"
        className="relative z-10 w-[calc(100%-24px)] max-w-[500px] overflow-hidden rounded-[12px] border border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
      >
        <div className="flex items-start justify-between gap-3 border-b border-hairline px-5 py-4">
          <div className="flex items-start gap-2.5">
            <ShieldAlert className="mt-0.5 size-5 shrink-0 text-destructive-foreground" strokeWidth={1.5} aria-hidden />
            <div>
              <h2 id="reset-kill-title" className="text-base font-semibold text-ink">
                Reset Kill Switch Latch
              </h2>
              <p className="mt-0.5 text-xs text-ink-2">
                Clear the persistent kill latch while keeping live order routing disarmed.
              </p>
            </div>
          </div>
          <Button
            variant="ghost"
            size="icon"
            aria-label="Close reset kill latch modal"
            className="size-11 shrink-0 sm:size-9"
            onClick={() => onOpenChange(false)}
          >
            <X className="size-4" strokeWidth={1.5} aria-hidden />
          </Button>
        </div>

        <div className="space-y-3 px-5 py-4 text-xs">
          <dl className="grid grid-cols-2 gap-3 border-b border-hairline pb-3">
            <div className="min-w-0">
              <dt className="text-ink-2">Active Kill Reason</dt>
              <dd className="mt-0.5 break-words font-medium text-ink">{status.killReason || "operator_kill_switch"}</dd>
            </div>
            <div className="min-w-0">
              <dt className="text-ink-2">Post-Reset State</dt>
              <dd className="mt-0.5 font-medium text-ink">
                Paper Mode (<code className="text-xs">live_armed: false</code>)
              </dd>
            </div>
          </dl>

          <p className="text-ink-2">
            I verified open positions, resting orders, and UTC-day realized loss. Resetting the kill switch latch clears the halt state while keeping{" "}
            <code className="text-xs text-ink">live_armed: false</code> (Paper Mode), so live Kalshi order routing remains blocked until you explicitly arm live trading.
          </p>

          {errorMsg && (
            <div id="reset-kill-error" role="alert" className="border-l-2 border-dot-red bg-board px-3 py-2 text-destructive-foreground">
              {errorMsg}
            </div>
          )}

          <button
            id="reset-kill-checkbox"
            data-testid="reset-kill-checkbox"
            type="button"
            role="checkbox"
            aria-checked={acknowledged}
            onClick={() => setAcknowledged((v) => !v)}
            className="flex min-h-11 w-full cursor-pointer items-center gap-2.5 border-t border-hairline pt-3 text-left text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <span
              aria-hidden="true"
              className={
                "inline-flex size-4 shrink-0 items-center justify-center rounded border " +
                (acknowledged
                  ? "border-ink-button bg-ink-button text-ink-button-fg"
                  : "border-hairline bg-board text-transparent")
              }
            >
              <Check className="size-3" strokeWidth={2.25} />
            </span>
            <span>Acknowledge latch reset</span>
          </button>
        </div>

        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-hairline bg-board/50 px-5 py-3">
          <Button
            variant="outline"
            className="min-h-11 text-xs sm:min-h-9"
            onClick={() => onOpenChange(false)}
          >
            Keep Latch Active
          </Button>
          <Button
            id="confirm-reset-kill-btn"
            data-testid="confirm-reset-kill-btn"
            variant="default"
            disabled={!acknowledged || submitting || busy}
            className="min-h-11 gap-1.5 text-xs sm:min-h-9"
            onClick={() => void handleConfirm()}
          >
            <ShieldCheck className="size-3.5" strokeWidth={1.5} aria-hidden />
            <span>{submitting ? "Resetting..." : "Confirm Reset Kill Latch"}</span>
          </Button>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Closed-Loop Live Venue Reconciliation Panel
// ---------------------------------------------------------------------------

export function VenueReconciliationPanel({
  status,
  reconcile,
  reconciling,
  busy,
  reconcileError,
  onReconcileNow,
}: {
  status: LiveStatus;
  reconcile: LiveReconcileResponse;
  reconciling: boolean;
  busy: boolean;
  reconcileError: string | null;
  onReconcileNow: () => void;
}) {
  const rawSyncEpoch = reconcile.lastSyncEpoch ?? status.lastVenueSyncEpoch;
  const syncEpoch = validEpochSecs(rawSyncEpoch);
  const wsEventEpoch = validEpochSecs(status.privateWsLastEventEpoch);
  const nowMs = useClockTickMs(`${syncEpoch ?? ""}:${wsEventEpoch ?? ""}`);
  const isStale = Boolean(status.venueSyncStale || reconcile.syncStale);
  const syncAgeSecs = epochAgeSecs(syncEpoch, nowMs);
  const wsEventAgeSecs = epochAgeSecs(wsEventEpoch, nowMs);
  const syncBadge = venueSyncBadgeInfo(status);
  const rawUtcDayLoss =
    reconcile.source === "server" ? reconcile.utcDayLossDollars : status.dailyRealizedLossDollars;
  const utcDayLoss = isNum(rawUtcDayLoss) ? rawUtcDayLoss : 0;
  const rawOpenExposure =
    reconcile.source === "server" ? reconcile.openExposureDollars : status.openExposureDollars;
  const openExposure = isNum(rawOpenExposure) ? rawOpenExposure : 0;
  const executions = Array.isArray(reconcile.executions) ? reconcile.executions : [];
  const discrepancies = Array.isArray(reconcile.discrepancies) ? reconcile.discrepancies : [];
  const totalFees = useMemo(
    () =>
      isNum(reconcile.utcDayFeesDollars)
        ? reconcile.utcDayFeesDollars
        : executions.reduce((sum, ex) => sum + (isNum(ex.feeDollars) ? ex.feeDollars : 0), 0),
    [reconcile.utcDayFeesDollars, executions],
  );
  const totalRealizedPnl = useMemo(
    () =>
      isNum(reconcile.utcDayRealizedPnlDollars)
        ? reconcile.utcDayRealizedPnlDollars
        : executions.reduce(
            (sum, ex) => sum + (isNum(ex.realizedPnlDollars) ? ex.realizedPnlDollars : 0),
            0,
          ),
    [reconcile.utcDayRealizedPnlDollars, executions],
  );

  return (
    <section
      id="live-reconcile-panel"
      data-testid="live-reconcile-panel"
      aria-labelledby="live-reconcile-heading"
      className="overflow-hidden rounded-[10px] border border-hairline bg-surface"
    >
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <ShieldCheck className="size-4 shrink-0 text-info-foreground" strokeWidth={1.5} aria-hidden />
          <h3 id="live-reconcile-heading" className="text-sm font-semibold text-ink">
            Venue Reconciliation
          </h3>
          {isStale ? (
            <StatusPill
              id="live-reconcile-stale-badge"
              data-testid="stale-venue-sync-badge"
              tone="warning"
              className="min-h-7 px-2.5 py-0.5 text-xs text-warning-foreground"
            >
              Stale Venue Sync
            </StatusPill>
          ) : (
            <StatusPill
              id="live-reconcile-status-pill"
              tone={
                reconcile.discrepancies.length > 0
                  ? "warning"
                  : syncEpoch !== null
                    ? "success"
                    : "neutral"
              }
              className="min-h-7 px-2.5 py-0.5 text-xs text-ink-2"
            >
              {reconcile.discrepancies.length > 0
                ? "Discrepancy Alert"
                : syncEpoch !== null
                  ? "Venue Synced"
                  : "Awaiting Sync"}
            </StatusPill>
          )}
          <span
            data-reconcile-sync-mode-badge
            data-venue-sync-mode={syncBadge.mode}
            className="inline-flex"
          >
            <StatusPill
              id="reconcile-sync-mode-pill"
              tone={syncBadge.tone}
              title={
                wsEventEpoch !== null
                  ? `${syncBadge.hint} (${timeUtcFullLabel(wsEventEpoch)})`
                  : syncBadge.hint
              }
              className={syncBadge.pillClassName}
            >
              <Activity className="size-3 shrink-0" strokeWidth={1.5} aria-hidden />
              <span>{syncBadge.label}</span>
            </StatusPill>
          </span>
        </div>

        <Button
          id="reconcile-now-btn"
          data-testid="reconcile-now-btn"
          variant="outline"
          disabled={reconciling || busy}
          onClick={onReconcileNow}
          className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-8"
        >
          <RefreshCw
            className={"size-3.5 " + (reconciling ? "motion-safe:animate-spin" : "")}
            strokeWidth={1.5}
            aria-hidden
          />
          <span>{reconciling ? "Reconciling..." : "Reconcile Now"}</span>
        </Button>
      </div>

      <dl className="grid grid-cols-2 divide-y divide-hairline border-b border-hairline bg-board/25 text-xs sm:grid-cols-4 sm:divide-x sm:divide-y-0 lg:grid-cols-5">
        <div className="min-w-0 px-4 py-2.5">
          <dt className="text-ink-2">UTC-Day Realized Loss</dt>
          <dd
            id="reconcile-utc-day-loss"
            className={
              "mono mt-0.5 text-sm font-semibold tabular-nums " +
              (utcDayLoss >= status.rails.maxDailyLossDollars ? "text-destructive-foreground" : "text-ink")
            }
          >
            {money(utcDayLoss)}
          </dd>
          <dd className="mt-0.5 text-xs text-ink-3">
            Cap <span className="mono tabular-nums">{money(status.rails.maxDailyLossDollars)}</span>
          </dd>
        </div>

        <div className="min-w-0 px-4 py-2.5">
          <dt className="text-ink-2">Open Exposure</dt>
          <dd id="reconcile-open-exposure" className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
            {money(openExposure)}
          </dd>
          <dd className="mt-0.5 text-xs text-ink-3">
            Cap <span className="mono tabular-nums">{money(status.rails.maxOpenExposureDollars)}</span>
          </dd>
        </div>

        <div className="min-w-0 px-4 py-2.5">
          <dt className="text-ink-2">UTC-Day Fees / Net P&amp;L</dt>
          <dd id="reconcile-utc-fees" className="mono mt-0.5 text-sm font-semibold tabular-nums text-ink">
            {money(totalFees)}
          </dd>
          <dd className="mt-0.5 text-xs text-ink-3">
            Net{" "}
            <span
              className={
                "mono tabular-nums font-medium " +
                (totalRealizedPnl > 0
                  ? "text-success-foreground"
                  : totalRealizedPnl < 0
                    ? "text-destructive-foreground"
                    : "text-ink-2")
              }
            >
              {moneySigned(totalRealizedPnl)}
            </span>
          </dd>
        </div>

        <div className="min-w-0 px-4 py-2.5">
          <dt className="text-ink-2">Last Venue Sync</dt>
          <dd
            id="reconcile-sync-age"
            className={
              "mt-0.5 text-sm font-semibold " +
              (isStale ? "text-warning-foreground" : "text-ink")
            }
          >
            {syncEpoch !== null ? ageWords(syncAgeSecs) : "Not synced"}
          </dd>
          <dd className="mt-0.5 text-xs text-ink-3">
            {syncEpoch !== null ? (
              <span className="mono tabular-nums">{timeUtcFullLabel(syncEpoch)}</span>
            ) : (
              "Run Reconcile Now"
            )}
          </dd>
        </div>

        <div className="col-span-2 min-w-0 border-t border-hairline px-4 py-2.5 sm:col-span-4 lg:col-span-1 lg:border-t-0">
          <dt className="text-ink-2">Private WS Event</dt>
          <dd
            id="reconcile-ws-event-age"
            data-reconcile-ws-age
            className={
              "mt-0.5 text-sm font-semibold " +
              (syncBadge.mode === "ws_stream" ? "text-success-foreground" : "text-ink")
            }
          >
            {wsEventEpoch !== null ? ageWords(wsEventAgeSecs) : "N/A"}
          </dd>
          <dd className="mt-0.5 text-xs text-ink-3">
            {wsEventEpoch !== null ? (
              <span className="mono tabular-nums">{timeUtcFullLabel(wsEventEpoch)}</span>
            ) : syncBadge.mode === "ws_stream" ? (
              "Awaiting stream event"
            ) : syncBadge.mode === "rest_poll" ? (
              "REST polling fallback"
            ) : (
              "Stream offline"
            )}
          </dd>
        </div>
      </dl>

      {reconcileError && (
        <div
          id="live-reconcile-error"
          role="alert"
          className="border-b border-hairline bg-board px-4 py-2.5 text-xs text-destructive-foreground"
        >
          Venue reconciliation failed: {reconcileError}
        </div>
      )}

      {isStale && (
        <div
          id="live-reconcile-stale-warning"
          role="status"
          className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-pill-warn-bg/50 px-4 py-2 text-xs text-warning-foreground"
        >
          <span className="flex items-center gap-1.5 font-medium">
            <AlertTriangle className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
            Venue execution sync is stale. Trigger Reconcile Now to refresh Kalshi fills and UTC-day loss before routing live orders.
          </span>
        </div>
      )}

      {discrepancies.length > 0 && (
        <div
          id="live-reconcile-discrepancies"
          data-testid="reconcile-discrepancies"
          role="alert"
          className="border-b border-hairline bg-board/60 px-4 py-3 text-xs"
        >
          <div className="flex items-center gap-1.5 font-semibold text-warning-foreground">
            <AlertTriangle className="size-3.5 shrink-0" strokeWidth={1.5} aria-hidden />
            <span>
              {discrepancies.length} reconciliation discrepancy alert
              {discrepancies.length === 1 ? "" : "s"}
            </span>
          </div>
          <ul className="mt-2 divide-y divide-hairline border-t border-hairline">
            {discrepancies.map((d, idx) => (
              <li
                key={`${d.kind}-${idx}`}
                data-reconcile-discrepancy
                className="flex flex-wrap items-center justify-between gap-2 py-2"
              >
                <span className="flex min-w-0 flex-wrap items-center gap-2 text-ink">
                  <Badge variant="warning">{d.kind}</Badge>
                  <span className="min-w-0 break-words">{d.message}</span>
                </span>
                {(d.ticker || d.orderId) && (
                  <span className="mono break-all tabular-nums text-ink-2">
                    {[d.ticker, d.orderId].filter(Boolean).join(" / ")}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div id="live-executions-table" data-testid="live-executions-table">
        {executions.length === 0 ? (
          <p role="status" className="px-4 py-4 text-xs text-ink-2">
            No reconciled live executions recorded for the current UTC day.
          </p>
        ) : (
          <Table className="stack">
            <TableHeader>
              <TableRow>
                <TableHead>Time (UTC)</TableHead>
                <TableHead>Ticker</TableHead>
                <TableHead>Side / Kind</TableHead>
                <TableHead className="text-right">Qty @ Price</TableHead>
                <TableHead>Schedule</TableHead>
                <TableHead className="text-right">Fee</TableHead>
                <TableHead className="text-right">Realized P&amp;L</TableHead>
                <TableHead>Order / Trade</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {executions.slice(0, 25).map((ex) => (
                <TableRow
                  key={`${ex.tradeId}-${ex.orderId}-${ex.ts}`}
                  data-reconcile-execution-row={ex.tradeId}
                >
                  <TableCell className="mono tabular-nums text-xs text-ink-2">
                    {ex.ts > 0 ? timeUtcFullLabel(ex.ts) : "-"}
                  </TableCell>
                  <TableCell className="mono m-grow max-w-[190px] break-all text-xs font-semibold text-ink">
                    {ex.ticker}
                  </TableCell>
                  <TableCell className="text-xs text-ink-2">
                    <span className="inline-flex items-center gap-1.5">
                      <span className="font-medium text-ink">
                        {ex.action === "sell" ? "Sell" : "Buy"} {ex.side === "no" ? "No" : "Yes"}
                      </span>
                      <Badge variant="outline" className="text-xs">
                        {ex.kind}
                      </Badge>
                    </span>
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs font-semibold text-ink">
                    {isNum(ex.count) ? ex.count : 0} @ {isNum(ex.priceCents) ? ex.priceCents : 0}c
                  </TableCell>
                  <TableCell className="text-xs">
                    <Badge variant={ex.isTaker ? "warning" : "success"} className="text-xs">
                      {ex.isTaker ? "Taker" : "Maker"}
                    </Badge>
                  </TableCell>
                  <TableCell className="mono tabular-nums text-right text-xs text-ink-2">
                    {money(ex.feeDollars)}
                  </TableCell>
                  <TableCell
                    className={
                      "mono tabular-nums text-right text-xs font-semibold " +
                      (ex.realizedPnlDollars > 0
                        ? "text-success-foreground"
                        : ex.realizedPnlDollars < 0
                          ? "text-destructive-foreground"
                          : "text-ink-2")
                    }
                  >
                    {moneySigned(ex.realizedPnlDollars)}
                  </TableCell>
                  <TableCell className="mono break-all tabular-nums text-xs text-ink-2">
                    {ex.orderId || ex.tradeId || "-"}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Top Live Kalshi Account Strip + Paper/Live Mode Switcher + Kill Switch
// ---------------------------------------------------------------------------

export function LiveAccountStrip({
  bundle,
  busy,
  workspaceOpen,
  runnerMode,
  onToggleWorkspace,
  onArmConfirm,
  onDisarm,
  onKillSwitch,
  onRefresh,
  onOpenSettings,
}: {
  bundle: LiveDeskBundle;
  busy: boolean;
  workspaceOpen: boolean;
  runnerMode?: RunnerMode;
  onToggleWorkspace: () => void;
  onArmConfirm: () => void;
  onDisarm: () => void;
  onKillSwitch: () => void;
  onRefresh: () => void;
  onOpenSettings: () => void;
}) {
  const [armModalOpen, setArmModalOpen] = useState(false);
  const [resetKillModalOpen, setResetKillModalOpen] = useState(false);
  const [reconcileState, setReconcileState] = useState<LiveReconcileResponse>(
    () => bundle.reconcile ?? defaultLiveReconcile("client"),
  );
  const [reconciling, setReconciling] = useState(false);
  const [reconcileError, setReconcileError] = useState<string | null>(null);
  const reconcileLockRef = useRef(false);
  const resetKillLockRef = useRef(false);
  const mountedRef = useRef(true);
  const { status, account, positions, orders, fills } = bundle;
  const wsEventEpoch = validEpochSecs(status.privateWsLastEventEpoch);
  const nowMs = useClockTickMs(String(wsEventEpoch ?? ""));
  const wsEventAgeSecs = epochAgeSecs(wsEventEpoch, nowMs);
  const syncBadge = venueSyncBadgeInfo(status);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (bundle.reconcile && bundle.reconcile.source === "server") {
      setReconcileState(bundle.reconcile);
    }
  }, [bundle.reconcile]);

  useEffect(() => {
    if (bundle.reconcile?.source === "server") return;
    let active = true;
    void fetchLiveReconcile().then((res) => {
      if (active && res.source === "server") {
        setReconcileState(res);
      }
    });
    return () => {
      active = false;
    };
  }, [bundle.reconcile?.source, status.configured]);

  const handleReconcileNow = async () => {
    if (reconciling || reconcileLockRef.current) return;
    reconcileLockRef.current = true;
    setReconciling(true);
    setReconcileError(null);
    try {
      const res = await triggerLiveReconcile("1");
      if (!mountedRef.current) return;
      setReconcileState(res);
      onRefresh();
      if (res.discrepancies.length > 0) {
        toast.warning(`Venue synced with ${res.discrepancies.length} discrepancy alert(s).`);
      } else {
        toast.success("Kalshi venue executions reconciled.");
      }
    } catch (e) {
      if (!mountedRef.current) return;
      const msg = e instanceof Error ? e.message : String(e);
      setReconcileError(msg);
      toast.error(`Reconcile failed: ${msg}`);
    } finally {
      reconcileLockRef.current = false;
      if (mountedRef.current) setReconciling(false);
    }
  };

  const handleConfirmResetKill = async () => {
    if (resetKillLockRef.current) return;
    resetKillLockRef.current = true;
    try {
      await resetKillSwitch("1");
      if (!mountedRef.current) return;
      onRefresh();
      toast.success("Kill switch latch cleared. Live trading remains disarmed (Paper Mode).");
    } finally {
      resetKillLockRef.current = false;
    }
  };

  return (
    <>
      <section
        id="live-account-strip"
        aria-label="Live Kalshi account and trading mode controls"
        className="rounded-[10px] border border-hairline bg-surface p-3.5 sm:px-4 sm:py-3"
      >
        <div className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
          {/* Left: Paper / Live Mode Switcher + Connection Pill */}
          <div className="flex flex-wrap items-center gap-2.5">
            <div
              id="trading-mode-switch"
              role="group"
              aria-label="Paper or Live trading mode"
              className="inline-flex items-center rounded-md border border-hairline bg-board p-0.5"
            >
              <button
                id="trading-mode-paper"
                type="button"
                aria-pressed={!status.liveArmed}
                disabled={busy}
                onClick={() => {
                  if (status.liveArmed) onDisarm();
                }}
                className={
                  "inline-flex min-h-11 cursor-pointer items-center gap-1.5 rounded px-3 text-xs font-medium outline-none transition-[color,background-color] focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                  (!status.liveArmed
                    ? "bg-ink text-canvas"
                    : "text-ink-2 hover:text-ink")
                }
              >
                <Lock className="size-3.5" aria-hidden />
                <span>Paper</span>
              </button>
              <button
                id="trading-mode-live"
                type="button"
                aria-pressed={status.liveArmed}
                disabled={busy}
                onClick={() => {
                  if (!status.liveArmed) setArmModalOpen(true);
                }}
                className={
                  "inline-flex min-h-11 cursor-pointer items-center gap-1.5 rounded px-3 text-xs font-medium outline-none transition-[color,background-color] focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                  (status.liveArmed
                    ? "bg-blue-soft text-info-foreground"
                    : "text-ink-2 hover:text-ink")
                }
              >
                <Unlock className="size-3.5" aria-hidden />
                <span>{status.liveArmed ? `Live Armed (${status.environment.toUpperCase()})` : "Arm Live"}</span>
              </button>
            </div>

            <StatusPill
              id="live-connection-pill"
              tone={
                status.killed
                  ? "error"
                  : status.connected
                    ? "success"
                    : status.configured
                      ? "warning"
                      : "info"
              }
              className="min-h-7 px-2.5 py-0.5 text-xs text-ink-2"
            >
              <span>
                {status.killed
                  ? "Kill switch active"
                  : status.connected
                    ? `Kalshi ${status.environment.toUpperCase()} connected`
                    : status.configured
                      ? `Kalshi ${status.environment.toUpperCase()} saved`
                      : "Kalshi API not configured"}
              </span>
            </StatusPill>

            <span
              data-live-ws-sync-badge
              data-venue-sync-mode={syncBadge.mode}
              className="inline-flex"
            >
              <StatusPill
                id="live-strip-sync-mode-pill"
                tone={syncBadge.tone}
                title={
                  wsEventEpoch !== null
                    ? `${syncBadge.hint} (${timeUtcFullLabel(wsEventEpoch)})`
                    : syncBadge.hint
                }
                className={syncBadge.pillClassName}
              >
                <Activity className="size-3 shrink-0" strokeWidth={1.5} aria-hidden />
                <span>{syncBadge.label}</span>
              </StatusPill>
            </span>

            {runnerMode && (
              <StatusPill
                id="live-runner-mode-pill"
                tone={
                  status.killed
                    ? "error"
                    : runnerMode === "live_auto"
                      ? "warning"
                      : runnerMode === "shadow"
                        ? "info"
                        : "neutral"
                }
                className="min-h-7 px-2.5 py-0.5 text-xs text-ink-2"
              >
                <span>
                  Runner:{" "}
                  {status.killed
                    ? "Halted"
                    : runnerMode === "live_auto"
                      ? "Live Auto"
                      : runnerMode === "shadow"
                        ? "Shadow"
                        : "Stopped"}
                </span>
              </StatusPill>
            )}
          </div>

          {/* Center: Live Kalshi Account KPIs */}
          <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-xs sm:flex sm:flex-wrap sm:items-center sm:gap-5">
            <div>
              <dt className="text-ink-3">Kalshi Cash</dt>
              <dd id="live-balance-val" className="mono font-semibold text-ink">
                {account.balanceDollars !== null ? money(account.balanceDollars) : "Not linked"}
              </dd>
            </div>
            <div>
              <dt className="text-ink-3">Portfolio</dt>
              <dd id="live-portfolio-val" className="mono font-semibold text-ink">
                {account.portfolioValueDollars !== null ? money(account.portfolioValueDollars) : "-"}
              </dd>
            </div>
            <div>
              <dt className="text-ink-3">Positions</dt>
              <dd id="live-positions-count" className="mono font-semibold text-ink">{positions.length}</dd>
            </div>
            <div>
              <dt className="text-ink-3">Resting Orders</dt>
              <dd id="live-orders-count" className="mono font-semibold text-ink">{orders.length}</dd>
            </div>
            <div>
              <dt className="text-ink-3">Fills</dt>
              <dd id="live-fills-count" className="mono font-semibold text-ink">{fills.length}</dd>
            </div>
            <div>
              <dt className="text-ink-3">WS Event</dt>
              <dd
                id="live-ws-last-event-age"
                data-live-ws-age
                className={
                  "mono font-semibold " +
                  (syncBadge.mode === "ws_stream" && wsEventEpoch !== null
                    ? "text-success-foreground"
                    : "text-ink")
                }
              >
                {wsEventEpoch !== null ? ageWords(wsEventAgeSecs) : "N/A"}
              </dd>
            </div>
          </dl>

          {/* Right: Order Ticket, API Settings & Emergency Kill Switch */}
          <div className="flex flex-wrap items-center gap-2">
            <Button
              id="toggle-live-ticket-btn"
              variant={workspaceOpen ? "default" : "outline"}
              className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-8"
              onClick={onToggleWorkspace}
            >
              <SlidersHorizontal className="size-3.5" aria-hidden />
              <span>{workspaceOpen ? "Hide Live Desk" : "Order Ticket"}</span>
            </Button>

            {!status.configured && (
              <Button
                id="live-configure-btn"
                variant="outline"
                className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-8"
                onClick={onOpenSettings}
              >
                <KeyRound className="size-3.5" aria-hidden />
                <span>API Keys</span>
              </Button>
            )}

            <Button
              variant="ghost"
              size="icon"
              aria-label="Refresh live Kalshi account"
              className="size-11 sm:size-8"
              disabled={busy}
              onClick={onRefresh}
            >
              <RefreshCw className={`size-3.5 ${busy ? "motion-safe:animate-spin" : ""}`} aria-hidden />
            </Button>

            <Button
              id="live-strip-kill-switch-btn"
              variant="outline"
              disabled={busy}
              onClick={onKillSwitch}
              className="min-h-11 gap-1.5 border-dot-red bg-surface px-3 text-xs font-semibold text-destructive-foreground hover:bg-board sm:min-h-8"
              title="Cancel all resting Kalshi orders immediately and disarm live trading"
            >
              <AlertOctagon className="size-3.5" aria-hidden />
              <span>Kill Switch</span>
            </Button>
          </div>
        </div>

        {status.killed && (
          <div
            id="live-kill-latch-banner"
            role="alert"
            className="mt-3 flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-2.5 text-xs text-destructive-foreground"
          >
            <span className="flex min-w-0 items-center gap-1.5 font-medium">
              <AlertTriangle className="size-3.5 shrink-0" aria-hidden />
              <span className="min-w-0 break-words">
                Emergency Kill Switch triggered{status.killReason ? ` (${status.killReason})` : ""}. All live routing is disarmed (live_armed: false).
              </span>
            </span>
            <Button
              id="reset-kill-latch-btn"
              data-testid="reset-kill-latch-btn"
              variant="outline"
              disabled={busy}
              onClick={() => setResetKillModalOpen(true)}
              className="min-h-11 gap-1.5 border-hairline bg-surface px-3 text-xs font-medium text-ink sm:min-h-8"
            >
              <ShieldCheck className="size-3.5" strokeWidth={1.5} aria-hidden />
              <span>Reset Kill Latch</span>
            </Button>
          </div>
        )}
      </section>

      <VenueReconciliationPanel
        status={status}
        reconcile={reconcileState}
        reconciling={reconciling}
        busy={busy}
        reconcileError={reconcileError}
        onReconcileNow={() => void handleReconcileNow()}
      />

      <ArmLiveModal
        open={armModalOpen}
        onOpenChange={setArmModalOpen}
        status={status}
        busy={busy}
        onConfirmArm={onArmConfirm}
        onOpenSettings={onOpenSettings}
        onOpenResetKill={() => setResetKillModalOpen(true)}
      />

      <ResetKillLatchModal
        open={resetKillModalOpen}
        onOpenChange={setResetKillModalOpen}
        status={status}
        busy={busy}
        onConfirmReset={handleConfirmResetKill}
      />
    </>
  );
}

// ---------------------------------------------------------------------------
// Live Order Ticket + Positions / Resting Orders / Recent Fills Workspace
// ---------------------------------------------------------------------------

export function LiveDeskWorkspace({
  bundle,
  markets,
  prefillTicker,
  prefillPriceCents,
  prefillSide,
  prefillCount,
  prefillPostOnly,
  busy,
  onSubmitOrder,
  onCancelOrder,
  onAmendOrder,
  onFlattenPosition,
  onRefresh,
  onRequestArm,
  onOpenSettings,
}: {
  bundle: LiveDeskBundle;
  markets: Market[];
  prefillTicker?: string | null;
  prefillPriceCents?: number | null;
  prefillSide?: "yes" | "no" | null;
  prefillCount?: number | null;
  prefillPostOnly?: boolean | null;
  busy: boolean;
  onSubmitOrder: (payload: LiveOrderPayload) => Promise<void>;
  onCancelOrder: (orderId: string) => Promise<void>;
  onAmendOrder?: (orderId: string, payload: AmendLiveOrderPayload) => Promise<void>;
  onFlattenPosition?: (ticker: string, payload: FlattenLivePositionPayload) => Promise<void>;
  onRefresh?: () => void;
  onRequestArm: () => void;
  onOpenSettings: () => void;
}) {
  const { status, positions, orders, fills } = bundle;
  const [localOrders, setLocalOrders] = useState<LiveOrder[] | null>(null);
  const [localPositions, setLocalPositions] = useState<LivePosition[] | null>(null);

  useEffect(() => {
    setLocalOrders(null);
  }, [orders]);

  useEffect(() => {
    setLocalPositions(null);
  }, [positions]);

  const displayOrders = localOrders ?? orders;
  const displayPositions = localPositions ?? positions;
  const canMutateLive = status.liveArmed && !status.killed;
  const liveGuardReason = status.killed
    ? "Kill switch active: clear the kill latch and re-arm live trading before amending orders or flattening positions."
    : !status.liveArmed
      ? "Paper mode (live_armed: false): arm live trading to enable order amend and 1-click position flatten."
      : "";

  const defaultTicker = useMemo(() => {
    if (prefillTicker) return prefillTicker;
    const openFirst = markets.find((m) => m && !m.result && m.ticker);
    return cleanStr(openFirst?.ticker ?? markets[0]?.ticker, "KXBTC15M-26NOV22-T00");
  }, [prefillTicker, markets]);

  const [ticker, setTicker] = useState(defaultTicker);
  const [action, setAction] = useState<"buy" | "sell">("buy");
  const [side, setSide] = useState<"yes" | "no">("yes");
  const [countInput, setCountInput] = useState("5");
  const [priceInput, setPriceInput] = useState("55");
  const [postOnly, setPostOnly] = useState<boolean>(true);
  const [subTab, setSubTab] = useState<"orders" | "positions" | "fills">("orders");
  const [submitting, setSubmitting] = useState(false);
  const submitLockRef = useRef(false);

  const [amendingOrderId, setAmendingOrderId] = useState<string | null>(null);
  const [amendPriceInput, setAmendPriceInput] = useState("");
  const [amendCountInput, setAmendCountInput] = useState("");
  const [pendingAmendId, setPendingAmendId] = useState<string | null>(null);
  const [amendError, setAmendError] = useState<string | null>(null);
  const amendLockRef = useRef(false);

  const [pendingFlattenKey, setPendingFlattenKey] = useState<string | null>(null);
  const [flattenFeedback, setFlattenFeedback] = useState<{ tone: "ok" | "error"; message: string } | null>(null);
  const flattenLockRef = useRef<Set<string>>(new Set());
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (prefillTicker) setTicker(prefillTicker);
  }, [prefillTicker]);

  useEffect(() => {
    if (isNum(prefillPriceCents) && prefillPriceCents >= 1 && prefillPriceCents <= 99) {
      setPriceInput(String(Math.round(prefillPriceCents)));
    }
  }, [prefillPriceCents]);

  useEffect(() => {
    if (prefillSide === "yes" || prefillSide === "no") setSide(prefillSide);
  }, [prefillSide]);

  useEffect(() => {
    if (isNum(prefillCount) && prefillCount >= 1) {
      setCountInput(String(Math.floor(prefillCount)));
    }
  }, [prefillCount]);

  useEffect(() => {
    if (typeof prefillPostOnly === "boolean") setPostOnly(prefillPostOnly);
  }, [prefillPostOnly]);

  const parsedCount = Number(countInput);
  const parsedPrice = Number(priceInput);
  const cleanTicketTicker = ticker.trim().toUpperCase();
  const ticketOrderQty = Number.isFinite(parsedCount) && parsedCount >= 1 ? Math.floor(parsedCount) : 1;
  const orderbook = useOrderbook(cleanTicketTicker, {
    enabled: cleanTicketTicker.length >= 4,
    sseEnabled: true,
  });
  const ticketWalk = useMemo(
    () => walkBookLevels(deriveSideBookLevels(orderbook.snapshot, side, action), ticketOrderQty),
    [orderbook.snapshot, side, action, ticketOrderQty],
  );
  const railEval = useMemo(
    () => evaluateOrderRails(status, ticker, parsedCount, parsedPrice, action, postOnly),
    [status, ticker, parsedCount, parsedPrice, action, postOnly],
  );

  const tickerSuggestions = useMemo(() => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const m of markets) {
      const t = cleanStr(m?.ticker, "");
      if (t && !seen.has(t)) {
        seen.add(t);
        out.push(t);
      }
      if (out.length >= 25) break;
    }
    return out;
  }, [markets]);

  const doSubmitOrder = async () => {
    if (!railEval.allPassed || submitting || submitLockRef.current) return;
    submitLockRef.current = true;
    setSubmitting(true);
    try {
      await onSubmitOrder({
        ticker: ticker.trim().toUpperCase(),
        action,
        side,
        count: Math.floor(parsedCount),
        price_cents: Math.round(parsedPrice),
        post_only: postOnly ? true : undefined,
        client_order_id: `ob-${Date.now()}`,
      });
    } finally {
      submitLockRef.current = false;
      setSubmitting(false);
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    void doSubmitOrder();
  };

  const openAmendPopover = (ord: LiveOrder) => {
    if (!canMutateLive) return;
    if (amendingOrderId === ord.orderId) {
      setAmendingOrderId(null);
      setAmendError(null);
      return;
    }
    const safePrice = Number.isFinite(ord.priceCents) ? Math.min(99, Math.max(1, Math.round(ord.priceCents))) : 50;
    setAmendingOrderId(ord.orderId);
    setAmendPriceInput(String(safePrice));
    setAmendCountInput("");
    setAmendError(null);
  };

  const parseWholeInt = (raw: string): number | null => {
    const trimmed = raw.trim();
    if (!/^\d+$/.test(trimmed)) return null;
    const n = Number(trimmed);
    return Number.isSafeInteger(n) ? n : null;
  };

  const handleConfirmAmend = async (ord: LiveOrder) => {
    if (!canMutateLive || busy || amendLockRef.current || pendingAmendId !== null) return;
    const maxQty = Math.max(1, Number.isFinite(ord.remainingCount) && ord.remainingCount > 0 ? ord.remainingCount : ord.count);
    const nextPrice = parseWholeInt(amendPriceInput);
    if (nextPrice === null || nextPrice < 1 || nextPrice > 99) {
      setAmendError("Limit price must be a whole integer between 1 and 99 cents.");
      return;
    }
    let nextCount: number | undefined;
    if (amendCountInput.trim().length > 0) {
      const parsedQty = parseWholeInt(amendCountInput);
      if (parsedQty === null || parsedQty < 1 || parsedQty > maxQty) {
        setAmendError(`Reduced count must be a whole integer from 1 to ${maxQty}.`);
        return;
      }
      nextCount = parsedQty;
    }

    amendLockRef.current = true;
    setPendingAmendId(ord.orderId);
    setAmendError(null);
    try {
      const payload: AmendLiveOrderPayload = {
        price_cents: nextPrice,
        ...(nextCount !== undefined ? { count: nextCount } : {}),
      };
      if (onAmendOrder) {
        await onAmendOrder(ord.orderId, payload);
        if (mountedRef.current) setAmendingOrderId(null);
      } else {
        const res = await amendLiveOrder(ord.orderId, payload);
        if (!mountedRef.current) return;
        setLocalOrders((prev) =>
          (prev ?? orders).map((item) => {
            if (item.orderId !== ord.orderId) return item;
            if (res.order) return res.order;
            const updatedCount = res.count ?? item.count;
            const updatedRem = res.remainingCount ?? (res.count !== null ? Math.min(item.remainingCount, res.count) : item.remainingCount);
            return {
              ...item,
              priceCents: res.priceCents,
              count: updatedCount,
              remainingCount: updatedRem,
              status: res.status || item.status,
              replaceCount: (item.replaceCount ?? 0) + 1,
            };
          }),
        );
        setAmendingOrderId(null);
        toast.success(res.message);
        onRefresh?.();
      }
    } catch (e) {
      if (!mountedRef.current) return;
      const msg = e instanceof Error ? e.message : String(e);
      setAmendError(msg);
      toast.error(`Amend failed: ${msg}`);
    } finally {
      amendLockRef.current = false;
      if (mountedRef.current) setPendingAmendId(null);
    }
  };

  const handleFlatten = async (pos: LivePosition) => {
    const lockKey = `${pos.ticker}:${pos.side}`;
    if (!canMutateLive || busy || pos.position <= 0 || flattenLockRef.current.has(lockKey) || pendingFlattenKey === lockKey) {
      return;
    }
    flattenLockRef.current.add(lockKey);
    setPendingFlattenKey(lockKey);
    setFlattenFeedback(null);
    try {
      const payload: FlattenLivePositionPayload = { side: pos.side };
      if (onFlattenPosition) {
        await onFlattenPosition(pos.ticker, payload);
      } else {
        const res = await flattenLivePosition(pos.ticker, payload);
        if (!mountedRef.current) return;
        if (res.order && res.status === "resting") {
          setLocalOrders((prev) => [res.order!, ...(prev ?? orders).filter((o) => o.orderId !== res.order!.orderId)]);
        }
        if (res.status === "executed" || res.status === "filled") {
          setLocalPositions((prev) =>
            (prev ?? positions).filter((p) => !(p.ticker === pos.ticker && p.side === pos.side)),
          );
        }
        setFlattenFeedback({ tone: "ok", message: res.message });
        toast.success(res.message);
        onRefresh?.();
      }
    } catch (e) {
      if (!mountedRef.current) return;
      const msg = e instanceof Error ? e.message : String(e);
      setFlattenFeedback({ tone: "error", message: msg });
      toast.error(`Flatten failed: ${msg}`);
    } finally {
      flattenLockRef.current.delete(lockKey);
      if (mountedRef.current) setPendingFlattenKey(null);
    }
  };

  return (
    <section
      id="live-trading-workspace"
      aria-label="Live Kalshi order ticket and portfolio"
      className="grid grid-cols-1 gap-4 lg:grid-cols-12"
    >
      {/* Left: Live Order Ticket + Hard Risk Rails Checklist */}
      <form
        id="live-order-ticket"
        onSubmit={(e) => void handleSubmit(e)}
        className="flex flex-col justify-between gap-4 rounded-[10px] border border-hairline bg-surface p-4 lg:col-span-5"
      >
        <div className="space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <h3 className="text-sm font-semibold text-ink">Live Kalshi v2 Order Ticket</h3>
              <p className="mt-0.5 text-xs text-ink-2">
                Limit orders require explicit live arming and pass all hard risk rails.
              </p>
            </div>
            <Badge
              variant="outline"
              className={
                "mono text-xs " +
                (railEval.allPassed
                  ? "border-dot-green text-success-foreground"
                  : "border-dot-amber text-warning-foreground")
              }
            >
              {railEval.allPassed ? "Rails Pass" : "Rails Blocked"}
            </Badge>
          </div>

          <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
            <div className="sm:col-span-2">
              <label htmlFor="live-ticket-ticker" className="text-xs text-ink-3">
                Kalshi Market Ticker
              </label>
              <input
                id="live-ticket-ticker"
                list="live-kalshi-tickers"
                value={ticker}
                onChange={(e) => setTicker(e.target.value)}
                placeholder="KXBTC15M-..."
                className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-board px-3 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
              />
              <datalist id="live-kalshi-tickers">
                {tickerSuggestions.map((t) => (
                  <option key={t} value={t} />
                ))}
              </datalist>
            </div>

            <div>
              <span className="text-xs text-ink-3">Action</span>
              <div className="mt-1 grid grid-cols-2 gap-1" role="group" aria-label="Order action">
                {(["buy", "sell"] as const).map((act) => (
                  <button
                    key={act}
                    type="button"
                    data-order-action={act}
                    aria-pressed={action === act}
                    onClick={() => setAction(act)}
                    className={
                      "min-h-11 cursor-pointer rounded-md border px-2 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9 " +
                      (action === act
                        ? "border-ink bg-ink text-canvas"
                        : "border-hairline bg-board text-ink-2 hover:text-ink")
                    }
                  >
                    {act.toUpperCase()}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <span className="text-xs text-ink-3">Contract Side</span>
              <div className="mt-1 grid grid-cols-2 gap-1" role="group" aria-label="Order side">
                {(["yes", "no"] as const).map((s) => (
                  <button
                    key={s}
                    type="button"
                    data-order-side={s}
                    aria-pressed={side === s}
                    onClick={() => setSide(s)}
                    className={
                      "min-h-11 cursor-pointer rounded-md border px-2 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9 " +
                      (side === s
                        ? "border-blue bg-blue-soft text-info-foreground"
                        : "border-hairline bg-board text-ink-2 hover:text-ink")
                    }
                  >
                    {s.toUpperCase()}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <label htmlFor="live-ticket-count" className="text-xs text-ink-3">
                Contracts (max {status.rails.maxOrderContracts})
              </label>
              <input
                id="live-ticket-count"
                type="number"
                min={1}
                max={10000}
                step={1}
                value={countInput}
                onChange={(e) => setCountInput(e.target.value)}
                className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-board px-3 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
              />
            </div>

            <div>
              <label htmlFor="live-ticket-price" className="text-xs text-ink-3">
                Limit Price (1-99c)
              </label>
              <input
                id="live-ticket-price"
                type="number"
                min={1}
                max={99}
                step={1}
                value={priceInput}
                onChange={(e) => setPriceInput(e.target.value)}
                className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-board px-3 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
              />
            </div>
          </div>

          {/* Post-Only Maker Toggle */}
          <div className="flex items-center justify-between gap-2 rounded-lg border border-hairline bg-board/60 px-3 py-2 text-xs">
            <div>
              <div className="font-medium text-ink">Post-Only Maker Order</div>
              <div className="text-ink-3">1.75% maker fee schedule (4x lower than 7% taker)</div>
            </div>
            <button
              id="live-ticket-post-only"
              type="button"
              aria-pressed={postOnly}
              onClick={() => setPostOnly((v) => !v)}
              className={
                "inline-flex min-h-11 shrink-0 cursor-pointer items-center gap-1.5 rounded-md border px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                (postOnly
                  ? "border-blue bg-blue-soft text-info-foreground"
                  : "border-hairline bg-surface text-ink-2 hover:text-ink")
              }
            >
              <span
                aria-hidden="true"
                className={"size-2 rounded-full " + (postOnly ? "bg-blue" : "bg-ink-3")}
              />
              <span>{postOnly ? "Post-Only ON" : "Standard"}</span>
            </button>
          </div>

          {/* Notional & Fee Preview */}
          <dl className="grid grid-cols-2 divide-x divide-hairline border-y border-hairline py-2 text-xs">
            <div className="pr-3">
              <dt className="text-ink-3">Order Notional</dt>
              <dd className="mono mt-0.5 font-semibold text-ink">{money(railEval.orderNotionalDollars)}</dd>
            </div>
            <div className="pl-3">
              <dt className="text-ink-3">Est. Kalshi Fee ({postOnly ? "1.75% maker" : "7% taker"})</dt>
              <dd className="mono mt-0.5 font-semibold text-ink">{money(railEval.estFeeDollars)}</dd>
            </div>
          </dl>

          {/* Live L2 Book-Walk VWAP Impact */}
          <div
            id="live-ticket-vwap-impact"
            className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-hairline bg-board/60 px-3 py-2 text-xs"
          >
            <span className="text-ink-3">
              L2 VWAP ({ticketOrderQty}ct {action.toUpperCase()} {side.toUpperCase()})
            </span>
            <span className="mono font-semibold text-ink">
              {ticketWalk.vwapCents !== null
                ? `${ticketWalk.vwapCents.toFixed(2)}c (+${ticketWalk.slippageCents?.toFixed(2) ?? "0.00"}c slip)`
                : "No resting depth"}
            </span>
          </div>

          {/* Real-time Hard Risk Rails Indicator */}
          <div id="live-risk-rails-check" className="space-y-1.5 pt-1">
            <div className="flex items-center justify-between text-xs">
              <span className="font-semibold text-ink">Hard Risk Rails Pre-Check</span>
              <button
                type="button"
                onClick={onOpenSettings}
                className="min-h-11 cursor-pointer px-1 text-xs text-info-foreground hover:underline sm:min-h-6"
              >
                Edit Caps
              </button>
            </div>
            <ul className="space-y-1 text-xs">
              {railEval.checks.map((chk) => (
                <li
                  key={chk.id}
                  data-risk-rail={chk.id}
                  data-rail-passed={chk.passed ? "true" : "false"}
                  className="flex items-center justify-between gap-2"
                >
                  <span className="inline-flex items-center gap-1.5 text-ink-2">
                    {chk.passed ? (
                      <CheckCircle2 className="size-3.5 shrink-0 text-success-foreground" aria-hidden />
                    ) : (
                      <XCircle className="size-3.5 shrink-0 text-destructive-foreground" aria-hidden />
                    )}
                    <span>{chk.label}</span>
                  </span>
                  <span className={"mono text-right " + (chk.passed ? "text-ink" : "text-destructive-foreground")}>
                    {chk.detail}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        </div>

        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline pt-3">
          {!status.liveArmed ? (
            <Button
              type="button"
              variant="outline"
              className="min-h-11 w-full gap-1.5 text-xs sm:min-h-9"
              onClick={onRequestArm}
            >
              <ShieldCheck className="size-3.5" aria-hidden />
              <span>Disarmed: arm live trading to submit</span>
            </Button>
          ) : (
            <Button
              id="submit-live-order-btn"
              type="button"
              variant="default"
              disabled={!railEval.allPassed || submitting || busy}
              onClick={() => void doSubmitOrder()}
              className="min-h-11 w-full gap-1.5 text-xs font-semibold sm:min-h-9"
            >
              <span>
                {submitting
                  ? "Submitting signed order…"
                  : `Submit ${action.toUpperCase()} ${side.toUpperCase()} (${Math.max(0, Math.floor(parsedCount || 0))} @ ${Math.round(parsedPrice || 0)}c)`}
              </span>
            </Button>
          )}
        </div>
      </form>

      {/* Right: L2 Orderbook Ladder + Resting Orders, Open Positions, Recent Fills */}
      <div className="flex flex-col gap-4 lg:col-span-7">
        <OrderbookLadder
          ticker={cleanTicketTicker}
          snapshot={orderbook.snapshot}
          streamStatus={orderbook.streamStatus}
          orderQty={ticketOrderQty}
          side={side}
          action={action}
          selectedPriceCents={Number.isInteger(parsedPrice) ? parsedPrice : null}
          loading={orderbook.loading}
          error={orderbook.error}
          onSelectPrice={(priceCents) => {
            if (Number.isFinite(priceCents)) {
              const clamped = Math.min(99, Math.max(1, Math.round(priceCents)));
              setPriceInput(String(clamped));
            }
          }}
          onRefresh={orderbook.refresh}
        />

        <div
          id="live-portfolio-panel"
          className="flex flex-col justify-between overflow-hidden rounded-[10px] border border-hairline bg-surface"
        >
        <div>
          <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/50 px-4 py-3">
            <div className="flex items-center gap-2">
              <Layers className="size-4 text-info-foreground" aria-hidden />
              <h3 className="text-sm font-semibold text-ink">Connected Kalshi Portfolio</h3>
            </div>
            <div className="flex items-center gap-1" role="group" aria-label="Portfolio view">
              {(
                [
                  ["orders", `Orders (${displayOrders.length})`],
                  ["positions", `Positions (${displayPositions.length})`],
                  ["fills", `Fills (${fills.length})`],
                ] as const
              ).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  data-live-subtab={id}
                  aria-pressed={subTab === id}
                  onClick={() => setSubTab(id)}
                  className={
                    "min-h-11 cursor-pointer rounded-md border px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8 " +
                    (subTab === id
                      ? "border-ink bg-ink text-canvas"
                      : "border-hairline bg-surface text-ink-2 hover:text-ink")
                  }
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          {!canMutateLive && (subTab === "orders" || subTab === "positions") && (
            <div
              id="live-lifecycle-guard-banner"
              role="status"
              className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline bg-board/60 px-4 py-2 text-xs text-ink-2"
            >
              <span>{liveGuardReason}</span>
              {!status.killed && (
                <button
                  type="button"
                  onClick={onRequestArm}
                  className="min-h-11 cursor-pointer font-medium text-info-foreground hover:underline sm:min-h-7"
                >
                  Arm Live
                </button>
              )}
            </div>
          )}

          {subTab === "orders" && (
            <div id="live-orders-table">
              {displayOrders.length === 0 ? (
                <p role="status" className="p-5 text-xs text-ink-2">
                  No resting Kalshi orders right now.
                </p>
              ) : (
                <Table className="stack">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Ticker</TableHead>
                      <TableHead>Side</TableHead>
                      <TableHead className="text-right">Price</TableHead>
                      <TableHead className="text-right">Rem / Qty</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead className="text-right">Action</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {displayOrders.map((ord) => {
                      const isAmending = amendingOrderId === ord.orderId;
                      const isPendingAmend = pendingAmendId === ord.orderId;
                      const maxQty = Math.max(
                        1,
                        Number.isFinite(ord.remainingCount) && ord.remainingCount > 0 ? ord.remainingCount : ord.count,
                      );
                      const draftPrice = parseWholeInt(amendPriceInput);
                      const draftCountTrimmed = amendCountInput.trim();
                      const draftCount = draftCountTrimmed ? parseWholeInt(draftCountTrimmed) : undefined;
                      const amendValid =
                        draftPrice !== null &&
                        draftPrice >= 1 &&
                        draftPrice <= 99 &&
                        (draftCount === undefined || (draftCount !== null && draftCount >= 1 && draftCount <= maxQty));
                      return (
                        <Fragment key={ord.orderId}>
                          <TableRow data-live-order-row={ord.orderId}>
                            <TableCell className="mono m-grow max-w-[200px] break-all text-xs font-semibold text-ink">
                              {ord.ticker}
                            </TableCell>
                            <TableCell className="mono text-xs text-ink-2">
                              {`${ord.action} ${ord.side}`.toUpperCase()}
                            </TableCell>
                            <TableCell className="mono text-right text-xs font-semibold text-ink">
                              {Number.isFinite(ord.priceCents) ? `${ord.priceCents}c` : "-"}
                            </TableCell>
                            <TableCell className="mono text-right text-xs text-ink-2">
                              {Number.isFinite(ord.remainingCount) ? ord.remainingCount : 0}/
                              {Number.isFinite(ord.count) ? ord.count : 0}
                            </TableCell>
                            <TableCell>
                              <Badge variant="outline" className="text-xs">
                                {ord.status}
                              </Badge>
                            </TableCell>
                            <TableCell className="m-right text-right">
                              <div className="flex flex-wrap items-center justify-end gap-1.5">
                                <Button
                                  variant="outline"
                                  data-amend-order={ord.orderId}
                                  aria-expanded={isAmending}
                                  disabled={busy || !canMutateLive || isPendingAmend}
                                  title={canMutateLive ? "Amend resting order price or reduce count" : liveGuardReason}
                                  onClick={() => openAmendPopover(ord)}
                                  className="min-h-11 px-2.5 text-xs sm:min-h-8"
                                >
                                  Amend
                                </Button>
                                <Button
                                  variant="outline"
                                  data-cancel-order={ord.orderId}
                                  disabled={busy || isPendingAmend}
                                  onClick={() => void onCancelOrder(ord.orderId)}
                                  className="min-h-11 border-dot-red/60 px-2.5 text-xs text-destructive-foreground sm:min-h-8"
                                >
                                  Cancel
                                </Button>
                              </div>
                            </TableCell>
                          </TableRow>
                          {isAmending && (
                            <TableRow data-amend-popover={ord.orderId}>
                              <td colSpan={6} className="m-full bg-board/50 px-3 py-3 align-middle text-sm text-ink">
                                <div className="w-full space-y-2.5">
                                  <div className="flex flex-wrap items-end gap-2.5">
                                    <div className="min-w-[120px] flex-1">
                                      <label htmlFor={`amend-price-${ord.orderId}`} className="block text-xs text-ink-3">
                                        New Price (1-99c)
                                      </label>
                                      <input
                                        id={`amend-price-${ord.orderId}`}
                                        data-amend-price={ord.orderId}
                                        type="number"
                                        min={1}
                                        max={99}
                                        step={1}
                                        value={amendPriceInput}
                                        onChange={(e) => {
                                          setAmendPriceInput(e.target.value);
                                          setAmendError(null);
                                        }}
                                        className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                                      />
                                    </div>
                                    <div className="min-w-[140px] flex-1">
                                      <label htmlFor={`amend-count-${ord.orderId}`} className="block text-xs text-ink-3">
                                        Reduce Qty (opt, max {maxQty})
                                      </label>
                                      <input
                                        id={`amend-count-${ord.orderId}`}
                                        data-amend-count={ord.orderId}
                                        type="number"
                                        min={1}
                                        max={maxQty}
                                        step={1}
                                        placeholder={`Keep ${maxQty}`}
                                        value={amendCountInput}
                                        onChange={(e) => {
                                          setAmendCountInput(e.target.value);
                                          setAmendError(null);
                                        }}
                                        className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-8"
                                      />
                                    </div>
                                    <div className="flex flex-wrap items-center gap-1.5">
                                      <Button
                                        variant="default"
                                        data-confirm-amend={ord.orderId}
                                        disabled={busy || !canMutateLive || isPendingAmend || !amendValid}
                                        onClick={() => void handleConfirmAmend(ord)}
                                        className="min-h-11 px-3 text-xs sm:min-h-8"
                                      >
                                        {isPendingAmend ? "Amending..." : "Apply Amend"}
                                      </Button>
                                      <Button
                                        variant="ghost"
                                        data-close-amend={ord.orderId}
                                        disabled={isPendingAmend}
                                        onClick={() => {
                                          setAmendingOrderId(null);
                                          setAmendError(null);
                                        }}
                                        className="min-h-11 px-2.5 text-xs sm:min-h-8"
                                      >
                                        Close
                                      </Button>
                                    </div>
                                  </div>
                                  {!amendValid && !amendError && (
                                    <p className="text-xs text-warning-foreground">
                                      Enter a whole-cent limit from 1 to 99 and an optional count up to {maxQty}.
                                    </p>
                                  )}
                                  {amendError && (
                                    <div
                                      role="alert"
                                      data-amend-error={ord.orderId}
                                      className="border-l-2 border-dot-red bg-surface px-3 py-1.5 text-xs text-destructive-foreground"
                                    >
                                      {amendError}
                                    </div>
                                  )}
                                </div>
                              </td>
                            </TableRow>
                          )}
                        </Fragment>
                      );
                    })}
                  </TableBody>
                </Table>
              )}
            </div>
          )}

          {subTab === "positions" && (
            <div id="live-positions-table">
              {flattenFeedback && (
                <div
                  id="live-flatten-feedback"
                  role={flattenFeedback.tone === "error" ? "alert" : "status"}
                  className={
                    "border-b border-hairline px-4 py-2 text-xs " +
                    (flattenFeedback.tone === "error"
                      ? "bg-board text-destructive-foreground"
                      : "bg-board/50 text-success-foreground")
                  }
                >
                  {flattenFeedback.message}
                </div>
              )}
              {displayPositions.length === 0 ? (
                <p role="status" className="p-5 text-xs text-ink-2">
                  No open Kalshi positions on this account.
                </p>
              ) : (
                <Table className="stack">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Ticker</TableHead>
                      <TableHead>Side</TableHead>
                      <TableHead className="text-right">Contracts</TableHead>
                      <TableHead className="text-right">Exposure</TableHead>
                      <TableHead className="text-right">Realized P&amp;L</TableHead>
                      <TableHead className="text-right">Exit</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {displayPositions.map((pos) => {
                      const flattenKey = `${pos.ticker}:${pos.side}`;
                      const isFlattening = pendingFlattenKey === flattenKey;
                      const safeQty = Number.isFinite(pos.position) ? Math.max(0, Math.round(pos.position)) : 0;
                      return (
                        <TableRow key={`${pos.ticker}-${pos.side}`} data-live-position-row={pos.ticker}>
                          <TableCell className="mono m-grow max-w-[220px] break-all text-xs font-semibold text-ink">
                            {pos.ticker}
                          </TableCell>
                          <TableCell>
                            <Badge variant="outline" className="text-xs">
                              {pos.side.toUpperCase()}
                            </Badge>
                          </TableCell>
                          <TableCell className="mono text-right text-xs font-semibold text-ink">
                            {safeQty}
                          </TableCell>
                          <TableCell className="mono text-right text-xs text-ink-2">
                            {money(pos.marketExposureDollars)}
                          </TableCell>
                          <TableCell
                            className={
                              "mono text-right text-xs font-semibold " +
                              (pos.realizedPnlDollars > 0
                                ? "text-success-foreground"
                                : pos.realizedPnlDollars < 0
                                  ? "text-destructive-foreground"
                                  : "text-ink-2")
                            }
                          >
                            {moneySigned(pos.realizedPnlDollars)}
                          </TableCell>
                          <TableCell className="m-right text-right">
                            <Button
                              variant="outline"
                              data-flatten-position={pos.ticker}
                              data-flatten-side={pos.side}
                              disabled={busy || !canMutateLive || safeQty <= 0 || isFlattening}
                              title={
                                canMutateLive
                                  ? `Submit 1-click flatten sell order for ${safeQty} ${pos.side.toUpperCase()} contracts`
                                  : liveGuardReason
                              }
                              onClick={() => void handleFlatten(pos)}
                              className="min-h-11 border-dot-red/60 px-2.5 text-xs text-destructive-foreground sm:min-h-8"
                            >
                              {isFlattening ? "Flattening..." : "Flatten"}
                            </Button>
                          </TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              )}
            </div>
          )}

          {subTab === "fills" && (
            <div id="live-fills-table">
              {fills.length === 0 ? (
                <p role="status" className="p-5 text-xs text-ink-2">
                  No recent Kalshi fills recorded yet.
                </p>
              ) : (
                <Table className="stack">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Time (UTC)</TableHead>
                      <TableHead>Ticker</TableHead>
                      <TableHead>Side</TableHead>
                      <TableHead className="text-right">Qty @ Price</TableHead>
                      <TableHead className="text-right">Fee</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {fills.slice(0, 25).map((f) => (
                      <TableRow key={f.fillId} data-live-fill-row={f.fillId}>
                        <TableCell className="mono text-xs text-ink-2">{shortTs(f.createdTime)}</TableCell>
                        <TableCell className="mono m-grow max-w-[200px] break-all text-xs font-semibold text-ink">
                          {f.ticker}
                        </TableCell>
                        <TableCell className="mono text-xs text-ink-2">
                          {`${f.action} ${f.side}`.toUpperCase()}
                        </TableCell>
                        <TableCell className="mono text-right text-xs font-semibold text-ink">
                          {f.count} @ {f.priceCents}c
                        </TableCell>
                        <TableCell className="mono text-right text-xs text-ink-2">
                          {money(f.feeDollars)}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </div>
          )}
        </div>

        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline px-4 py-2.5 text-xs text-ink-3">
          <span>{status.apiBaseUrl}</span>
          <span className="mono">
            Daily loss {money(status.dailyRealizedLossDollars)} / {money(status.rails.maxDailyLossDollars)}
          </span>
        </div>
        </div>
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Kalshi API & Risk Rails Settings Panel (inside #settings Vaul sheet)
// ---------------------------------------------------------------------------

export function LiveSettingsSection({
  status,
  saving,
  onSaveConfig,
}: {
  status: LiveStatus;
  saving: boolean;
  onSaveConfig: (payload: LiveConfigPayload) => Promise<void>;
}) {
  const [env, setEnv] = useState<LiveEnvironment>(status.environment);
  const [apiKeyId, setApiKeyId] = useState("");
  const [pemPath, setPemPath] = useState("");
  const [maxContracts, setMaxContracts] = useState(String(status.rails.maxOrderContracts));
  const [maxOrderDollars, setMaxOrderDollars] = useState(String(status.rails.maxOrderDollars));
  const [maxDailyLoss, setMaxDailyLoss] = useState(String(status.rails.maxDailyLossDollars));
  const [maxExposure, setMaxExposure] = useState(String(status.rails.maxOpenExposureDollars));
  const [prefix, setPrefix] = useState(status.rails.allowedTickerPrefix);

  useEffect(() => {
    setEnv(status.environment);
    setMaxContracts(String(status.rails.maxOrderContracts));
    setMaxOrderDollars(String(status.rails.maxOrderDollars));
    setMaxDailyLoss(String(status.rails.maxDailyLossDollars));
    setMaxExposure(String(status.rails.maxOpenExposureDollars));
    setPrefix(status.rails.allowedTickerPrefix);
  }, [
    status.environment,
    status.rails.maxOrderContracts,
    status.rails.maxOrderDollars,
    status.rails.maxDailyLossDollars,
    status.rails.maxOpenExposureDollars,
    status.rails.allowedTickerPrefix,
  ]);

  const doSave = async () => {
    const payload: LiveConfigPayload = {
      environment: env,
      rails: {
        max_order_contracts: Math.max(1, Math.round(Number(maxContracts) || 25)),
        max_order_dollars: Math.max(1, Number(maxOrderDollars) || 25),
        max_daily_loss_dollars: Math.max(1, Number(maxDailyLoss) || 50),
        max_open_exposure_dollars: Math.max(1, Number(maxExposure) || 100),
        allowed_ticker_prefix: prefix.trim().toUpperCase(),
      },
    };
    if (apiKeyId.trim()) payload.api_key_id = apiKeyId.trim();
    if (pemPath.trim()) payload.private_key_path = pemPath.trim();
    await onSaveConfig(payload);
    setApiKeyId("");
    setPemPath("");
  };

  const handleSave = (e: React.FormEvent) => {
    e.preventDefault();
    void doSave();
  };

  return (
    <section
      id="settings-live-config"
      aria-labelledby="settings-live-heading"
      className="space-y-3 border-t border-hairline pt-5"
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          <KeyRound className="size-4 text-info-foreground" aria-hidden />
          <h3 id="settings-live-heading" className="text-sm font-semibold text-ink">
            Kalshi API &amp; Risk Rails
          </h3>
        </div>
        <Badge variant="outline" className="mono text-xs">
          {status.configured ? `${status.environment.toUpperCase()} saved` : "Not configured"}
        </Badge>
      </div>

      <p className="text-xs text-ink-2">
        Credentials are saved locally with 0600 permissions; private key bytes are never echoed back.
      </p>

      <form onSubmit={(e) => void handleSave(e)} className="space-y-3 rounded-[10px] border border-hairline bg-board p-3">
        {/* Environment toggle: Demo vs Production */}
        <div className="space-y-1">
          <span className="text-xs text-ink-3">Kalshi v2 Environment</span>
          <div className="grid grid-cols-2 gap-1.5" role="group" aria-label="Kalshi environment">
            {(
              [
                ["demo", "Demo (Sandbox)"],
                ["prod", "Production"],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                data-live-env={id}
                aria-pressed={env === id}
                onClick={() => setEnv(id)}
                className={
                  "min-h-11 cursor-pointer rounded-md border px-2 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring " +
                  (env === id
                    ? id === "prod"
                      ? "border-dot-red bg-surface text-destructive-foreground"
                      : "border-ink bg-ink text-canvas"
                    : "border-hairline bg-surface text-ink-2 hover:text-ink")
                }
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {/* API Key ID & Private Key PEM Path (never echoed) */}
        <div className="space-y-2">
          <div>
            <label htmlFor="live-api-key-id" className="text-xs text-ink-3">
              API Key ID {status.keyIdMasked ? `(saved: ${status.keyIdMasked})` : ""}
            </label>
            <input
              id="live-api-key-id"
              type="text"
              autoComplete="off"
              value={apiKeyId}
              onChange={(e) => setApiKeyId(e.target.value)}
              placeholder={status.keyIdMasked ? "Leave blank to keep saved key ID" : "Enter Kalshi API Key ID"}
              className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>

          <div>
            <label htmlFor="live-pem-path" className="text-xs text-ink-3">
              Private Key PEM File Path {status.keyPathConfigured ? "(PEM saved, never echoed)" : ""}
            </label>
            <input
              id="live-pem-path"
              type="text"
              autoComplete="off"
              value={pemPath}
              onChange={(e) => setPemPath(e.target.value)}
              placeholder={
                status.keyPathConfigured
                  ? "Leave blank to keep existing PEM"
                  : "/path/to/kalshi-rsa-private.pem"
              }
              className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>
        </div>

        {/* Hard Risk Rail Caps */}
        <div className="grid grid-cols-2 gap-2 border-t border-hairline pt-2.5">
          <div>
            <label htmlFor="rail-max-contracts" className="text-xs text-ink-3">
              Max Contracts
            </label>
            <input
              id="rail-max-contracts"
              type="number"
              min={1}
              max={10000}
              value={maxContracts}
              onChange={(e) => setMaxContracts(e.target.value)}
              className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>
          <div>
            <label htmlFor="rail-max-order-dollars" className="text-xs text-ink-3">
              Max Order ($)
            </label>
            <input
              id="rail-max-order-dollars"
              type="number"
              min={1}
              max={100000}
              step={1}
              value={maxOrderDollars}
              onChange={(e) => setMaxOrderDollars(e.target.value)}
              className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>
          <div>
            <label htmlFor="rail-max-daily-loss" className="text-xs text-ink-3">
              Max Daily Loss ($)
            </label>
            <input
              id="rail-max-daily-loss"
              type="number"
              min={1}
              max={100000}
              step={1}
              value={maxDailyLoss}
              onChange={(e) => setMaxDailyLoss(e.target.value)}
              className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>
          <div>
            <label htmlFor="rail-max-exposure" className="text-xs text-ink-3">
              Max Exposure ($)
            </label>
            <input
              id="rail-max-exposure"
              type="number"
              min={1}
              max={500000}
              step={5}
              value={maxExposure}
              onChange={(e) => setMaxExposure(e.target.value)}
              className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>
          <div className="col-span-2">
            <label htmlFor="rail-ticker-prefix" className="text-xs text-ink-3">
              Allowed Ticker Prefix
            </label>
            <input
              id="rail-ticker-prefix"
              type="text"
              value={prefix}
              onChange={(e) => setPrefix(e.target.value)}
              placeholder="KX"
              className="mono mt-1 min-h-11 w-full rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>
        </div>

        <Button
          id="save-live-config-btn"
          type="button"
          variant="default"
          disabled={saving}
          onClick={() => void doSave()}
          className="min-h-11 w-full text-xs font-medium sm:min-h-9"
        >
          {saving ? "Saving API & Risk Rails…" : "Save API & Risk Rails"}
        </Button>
      </form>
    </section>
  );
}
