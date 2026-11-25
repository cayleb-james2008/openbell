// 1-Click Full Quant Edge Audit Scorecard (v0.9.0):
// Evaluates 12 strategies -> OOS Tournament -> 4-Slot Uncorrelated Lineup (|r| <= 0.65)
// -> PAV Isotonic Recalibration -> Kalshi Fee & Slippage Net EV -> Monte Carlo DD & 5-Gate Readiness.
import { type RefObject, useCallback, useEffect, useState } from "react";
import { motion, useReducedMotion } from "motion/react";
import { CheckCircle2, RefreshCw, ShieldAlert, ShieldCheck, XCircle } from "lucide-react";
import { toast } from "sonner";
import { Badge, Button, StatusPill } from "@/components/ui";
import { type Dataset, type Home, type Market, type Slots, type Strategy, money, moneySigned } from "@/lib/desk";
import { type EdgeAuditData, fetchEdgeAudit } from "@/lib/edgeAudit";
import { reveal } from "@/motion";

function cleanJoin(s: string): string {
  return s.replace(/\s*·\s*/g, ", ");
}

function fmtCents(v: number | null): string {
  if (v === null || !Number.isFinite(v)) return "-";
  return `${v >= 0 ? "+" : ""}${v.toFixed(2)}c`;
}

function fmtPct(v: number | null, digits = 1): string {
  if (v === null || !Number.isFinite(v)) return "-";
  return `${v.toFixed(digits)}%`;
}

export function EdgeAuditPanel({
  dataset,
  home,
  strategies,
  markets,
  slots,
  auditSignal,
  panelRef,
}: {
  dataset: Dataset;
  home: Home | null;
  strategies: Strategy[];
  markets: Market[];
  slots: Slots | null;
  auditSignal?: number;
  panelRef?: RefObject<HTMLDivElement | null>;
}) {
  const reduce = useReducedMotion();
  const [slippage, setSlippage] = useState<number>(1);
  const [audit, setAudit] = useState<EdgeAuditData | null>(null);
  const [running, setRunning] = useState(false);

  const runAudit = useCallback(
    async (slip: number, notify = false) => {
      setRunning(true);
      try {
        const res = await fetchEdgeAudit(dataset, slip, home, strategies, markets, slots);
        setAudit(res);
        if (notify) {
          toast.success(`Full Quant Edge Audit complete (${res.verdict_label}) in ${res.elapsedMs}ms.`);
        }
      } finally {
        setRunning(false);
      }
    },
    [dataset, home, strategies, markets, slots],
  );

  useEffect(() => {
    void runAudit(slippage, false);
  }, [runAudit, slippage]);

  useEffect(() => {
    if (auditSignal && auditSignal > 0) {
      void runAudit(slippage, true);
    }
  }, [auditSignal, runAudit, slippage]);

  const isGo = audit?.verdict === "GO";

  return (
    <section
      ref={panelRef}
      id="quant-edge-audit"
      data-edge-audit
      aria-label="Full Quant Edge Audit Scorecard"
      className="scroll-mt-16 overflow-hidden rounded-[10px] border border-hairline bg-surface"
    >
      {/* Top Action Header */}
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-hairline bg-board/60 px-4 py-3.5">
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2.5">
            {isGo ? (
              <ShieldCheck className="size-5 shrink-0 text-success-foreground" strokeWidth={1.5} aria-hidden />
            ) : (
              <ShieldAlert className="size-5 shrink-0 text-warning-foreground" strokeWidth={1.5} aria-hidden />
            )}
            <h2 className="text-lg font-semibold text-ink">Full Quant Edge Audit</h2>
            {audit && (
              <StatusPill id="edge-audit-verdict" tone={isGo ? "success" : "warning"}>
                {audit.verdict_label}
              </StatusPill>
            )}
          </div>
          <p className="text-sm text-ink-2">
            One-click evaluation from all 12 strategies through OOS tournament, 4-slot decorrelation, PAV calibration, Kalshi fees, and Monte Carlo drawdown.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <label htmlFor="edge-audit-slippage" className="text-sm text-ink-2">
            Slippage
          </label>
          <select
            id="edge-audit-slippage"
            value={String(slippage)}
            onChange={(e) => setSlippage(Number(e.target.value))}
            className="mono min-h-11 rounded-md border border-[var(--input)] bg-surface px-2.5 text-sm text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          >
            <option value="0">0c</option>
            <option value="1">1c</option>
            <option value="2">2c</option>
            <option value="3">3c</option>
            <option value="5">5c</option>
          </select>

          <Button
            id="panel-run-edge-audit-btn"
            variant="default"
            disabled={running}
            onClick={() => void runAudit(slippage, true)}
          >
            <RefreshCw className={`size-4 ${running ? "animate-spin" : ""}`} strokeWidth={1.5} aria-hidden />
            <span>{running ? "Auditing…" : "Run Edge Audit"}</span>
          </Button>
        </div>
      </div>

      {!audit ? (
        <div className="p-4">
          <div className="h-36 animate-pulse rounded-md bg-track" />
        </div>
      ) : (
        <div className="divide-y divide-hairline">
          {/* Row 1: Headline Scorecard Strip (Flat hairline columns, zero nested cards) */}
          <dl className="grid grid-cols-2 divide-y divide-hairline sm:grid-cols-4 sm:divide-x sm:divide-y-0">
            <div className="px-4 py-3">
              <dt className="text-xs font-medium text-ink-2">Net EV After Fees</dt>
              <dd
                id="edge-audit-net-ev"
                className={
                  "mono mt-1 text-2xl font-semibold " +
                  (audit.fee_ev.net_ev_cents !== null && audit.fee_ev.net_ev_cents > 0
                    ? "text-success-foreground"
                    : "text-destructive-foreground")
                }
              >
                {fmtCents(audit.fee_ev.net_ev_cents)}
              </dd>
              <dd className="mono mt-0.5 text-xs text-ink-2">
                Fee {audit.fee_ev.kalshi_fee_drag_cents !== null ? `${audit.fee_ev.kalshi_fee_drag_cents.toFixed(2)}c` : "-"} plus {audit.slippage_cents.toFixed(1)}c slip
              </dd>
            </div>

            <div className="px-4 py-3">
              <dt className="text-xs font-medium text-ink-2">OOS Win vs Break-Even</dt>
              <dd className="mono mt-1 text-2xl font-semibold text-ink">
                {fmtPct(audit.fee_ev.realized_oos_win_rate_pct)}
              </dd>
              <dd className="mono mt-0.5 text-xs text-ink-2">
                Break-even {fmtPct(audit.fee_ev.break_even_win_rate_pct)}
              </dd>
            </div>

            <div className="px-4 py-3">
              <dt className="text-xs font-medium text-ink-2">Monte Carlo P50 / P(DD&ge;25%)</dt>
              <dd className="mono mt-1 text-2xl font-semibold text-ink">
                {money(audit.monte_carlo.p50_final_equity_dollars)}
              </dd>
              <dd className="mono mt-0.5 text-xs text-ink-2">
                Risk {(audit.monte_carlo.prob_dd_gte_25_pct * 100).toFixed(1)}% ({audit.monte_carlo.paths} paths)
              </dd>
            </div>

            <div className="px-4 py-3">
              <dt className="text-xs font-medium text-ink-2">Readiness Gates</dt>
              <dd className="mono mt-1 text-2xl font-semibold text-ink">
                {audit.readiness.gates_passed} of {audit.readiness.gates_total}
              </dd>
              <dd className="mono mt-0.5 text-xs text-ink-2">
                {audit.windows_settled_is} IS / {audit.windows_settled_oos} OOS windows
              </dd>
            </div>
          </dl>

          {/* Row 2: Architectural Hairline 5-Stage Validation Funnel Strip */}
          <div className="divide-y divide-hairline">
            <div className="flex flex-wrap items-center justify-between gap-2 bg-board/40 px-4 py-2 text-xs text-ink-2">
              <span className="font-medium text-ink">5-Stage Quant Validation Funnel</span>
              <span className="mono">
                {audit.funnel.filter((s) => s.passed).length} of {audit.funnel.length} stages cleared
              </span>
            </div>
            <div className="grid grid-cols-1 divide-y divide-hairline lg:grid-cols-5 lg:divide-x lg:divide-y-0">
              {audit.funnel.map((stage, idx) => (
                <motion.div
                  key={stage.id}
                  data-edge-audit-stage={stage.id}
                  initial={reduce ? false : { opacity: 0, y: 4 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={reduce ? { duration: 0 } : { ...reveal, delay: idx * 0.03 }}
                  className="relative flex flex-col justify-between gap-2 px-4 py-3.5"
                >
                  <div
                    aria-hidden
                    className={
                      "absolute inset-x-0 top-0 h-[2px] " +
                      (stage.passed ? "bg-dot-green" : "bg-dot-red")
                    }
                  />
                  <div className="space-y-1">
                    <div className="flex items-center justify-between gap-2">
                      <span className="mono text-xs font-medium text-ink-3">
                        0{idx + 1}
                      </span>
                      <span
                        className={
                          "inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs font-medium " +
                          (stage.passed
                            ? "bg-pill-active-bg text-pill-active-fg"
                            : "border border-dot-red/40 bg-surface text-destructive-foreground")
                        }
                      >
                        {stage.passed ? (
                          <CheckCircle2 className="size-3 shrink-0" strokeWidth={1.5} aria-hidden />
                        ) : (
                          <XCircle className="size-3 shrink-0" strokeWidth={1.5} aria-hidden />
                        )}
                        <span>{stage.passed ? "Pass" : "Gate"}</span>
                      </span>
                    </div>
                    <p className="text-sm font-semibold text-ink">{stage.title}</p>
                  </div>
                  <div className="space-y-0.5">
                    <p className="mono text-sm font-semibold text-info-foreground">{cleanJoin(stage.metric)}</p>
                    <p className="text-xs text-ink-2">{cleanJoin(stage.detail)}</p>
                  </div>
                </motion.div>
              ))}
            </div>
          </div>

          {/* Row 3: 4-Slot Lineup Summary + Blocking Reasons */}
          <div className="grid grid-cols-1 divide-y divide-hairline lg:grid-cols-12 lg:divide-x lg:divide-y-0">
            <div className="space-y-2.5 px-4 py-3.5 lg:col-span-7">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <h3 className="text-sm font-semibold text-ink">
                  Recommended 4-Slot Lineup (|r| &le; {audit.lineup.correlation_cap.toFixed(2)})
                </h3>
                <span className="mono text-xs text-ink-2">
                  {audit.lineup.max_abs_correlation !== null
                    ? `Max |r| ${audit.lineup.max_abs_correlation.toFixed(2)}`
                    : "Correlation pending sample depth"}
                </span>
              </div>
              {audit.lineup.slots.length === 0 ? (
                <p className="text-sm text-ink-2">No qualified OOS strategies on the current tape yet.</p>
              ) : (
                <div className="divide-y divide-hairline border-t border-hairline">
                  {audit.lineup.slots.map((s) => (
                    <div key={s.strategy} className="flex flex-wrap items-center justify-between gap-2 py-2 text-sm">
                      <div className="flex items-center gap-2">
                        <span className="mono text-xs font-semibold text-info-foreground">Slot {s.slot_number}</span>
                        <span className="font-medium text-ink">{s.strategy}</span>
                        <Badge variant="outline" className="mono">
                          {s.side.toUpperCase()}
                        </Badge>
                      </div>
                      <div className="mono flex items-center gap-4 text-xs">
                        <span className={s.oos_net_expectancy_cents >= 0 ? "font-semibold text-success-foreground" : "font-semibold text-destructive-foreground"}>
                          {fmtCents(s.oos_net_expectancy_cents)}/ct
                        </span>
                        <span className="text-ink-2">{s.oos_trades}w</span>
                        <span className="text-ink-2">
                          {s.oos_sharpe !== null ? `Sharpe ${s.oos_sharpe.toFixed(2)}` : "-"}
                        </span>
                      </div>
                    </div>
                  ))}
                </div>
              )}
              <p className="text-xs text-ink-2">
                EV Gate OOS impact: {audit.recalibration.ev_gate_accepted} of {audit.recalibration.ev_gate_total} quotes accepted, P&amp;L delta {moneySigned(audit.recalibration.ev_gate_pnl_delta_dollars)}
              </p>
            </div>

            <div className="space-y-2 px-4 py-3.5 lg:col-span-5">
              <h3 className="text-sm font-semibold text-ink">
                {audit.blocking_reasons.length === 0 ? "Audit Status" : "Go / No-Go Blockers"}
              </h3>
              {audit.blocking_reasons.length === 0 ? (
                <p className="text-sm text-success-foreground">
                  All readiness gates, OOS recalibration, and uncorrelated lineup checks passed.
                </p>
              ) : (
                <ul className="space-y-1.5 text-sm text-ink-2">
                  {audit.blocking_reasons.slice(0, 5).map((reason, idx) => (
                    <li key={idx} className="flex items-start gap-2">
                      <XCircle className="mt-0.5 size-4 shrink-0 text-destructive-foreground" strokeWidth={1.5} aria-hidden />
                      <span>{reason}</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-2 px-4 py-2.5 text-xs text-ink-3">
            <span>{audit.mode_notice}</span>
            <span className="mono">
              {audit.source === "server" ? "Server Edge Audit" : "Client Edge Audit"} ({audit.elapsedMs}ms)
            </span>
          </div>
        </div>
      )}
    </section>
  );
}
