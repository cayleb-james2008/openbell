// Global Command Palette (Cmd+K / Ctrl+K) built on native accessible modal + cmdk.
// Jumps directly to any live 15m market ticker, strategy, active slot, Quant Lab panel, or paper-only action.
import { useEffect } from "react";
import { Command } from "cmdk";
import {
  Activity,
  AlertOctagon,
  BarChart3,
  Code2,
  Dices,
  Download,
  FlaskConical,
  Grid3X3,
  Layers,
  RefreshCw,
  Search,
  Settings,
  ShieldCheck,
  SlidersHorizontal,
  TrendingUp,
  Trophy,
  X,
} from "lucide-react";
import { Badge, Kbd } from "@/components/ui";
import type { QuantSectionId } from "@/QuantLabTab";
import {
  type Market,
  type Slot,
  type Strategy,
  cents,
  classifyVerdict,
  cleanStr,
  impliedYesProb,
  marketIsOpen,
  moneySigned,
  seriesShort,
} from "@/lib/desk";

export type DeskTabId = "tape" | "strategies" | "slots" | "quant" | "live";

export function CommandPalette({
  open,
  onOpenChange,
  markets,
  strategies,
  slots,
  onSelectTab,
  onJumpMarket,
  onJumpStrategy,
  onOpenSandbox,
  onJumpSlot,
  onJumpQuantSection,
  onCollect,
  onExportReport,
  onReseedMonteCarlo,
  onOpenOrderTicket,
  onKillSwitch,
  onOpenSettings,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  markets: Market[];
  strategies: Strategy[];
  slots: Slot[];
  onSelectTab: (tab: DeskTabId) => void;
  onJumpMarket: (ticker: string) => void;
  onJumpStrategy: (name: string) => void;
  onOpenSandbox: () => void;
  onJumpSlot: (slotNumber: number, strategyName: string) => void;
  onJumpQuantSection: (section: QuantSectionId) => void;
  onCollect: () => void;
  onExportReport: (format: "md" | "csv") => void;
  onReseedMonteCarlo: () => void;
  onOpenOrderTicket: () => void;
  onKillSwitch: () => void;
  onOpenSettings: () => void;
}) {
  const closeAnd = (fn: () => void) => {
    onOpenChange(false);
    fn();
  };

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onOpenChange(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onOpenChange]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[10vh]">
      <div
        aria-hidden="true"
        className="fixed inset-0 bg-ink/35"
        onClick={() => onOpenChange(false)}
      />
      <div
        id="command-palette"
        data-command-palette
        role="dialog"
        aria-modal="true"
        aria-label="OpenBell Command Palette"
        className="relative z-10 w-[calc(100%-24px)] max-w-[640px] overflow-hidden rounded-[12px] border border-hairline bg-surface text-ink shadow-(--elev-lift) outline-none"
      >
        <Command label="OpenBell Command Palette" className="flex flex-col">
          <div className="flex items-center gap-2 border-b border-hairline px-3.5 py-2.5 focus-within:ring-2 focus-within:ring-inset focus-within:ring-ring">
            <Search className="size-4 shrink-0 text-ink-3" aria-hidden />
            <Command.Input
              id="cmdk-input"
              data-cmdk-input
              autoFocus
              aria-label="Search tickers, strategies, slots, or Quant Lab"
              placeholder="Jump to ticker, strategy, slot, or Quant Lab…"
              className="min-h-11 flex-1 bg-transparent text-sm text-ink placeholder:text-ink-3 outline-none focus-visible:outline-none sm:min-h-8"
            />
              <button
                type="button"
                aria-label="Close command palette"
                onClick={() => onOpenChange(false)}
                className="inline-flex size-11 cursor-pointer items-center justify-center rounded-md text-ink-2 hover:bg-board hover:text-ink sm:size-8"
              >
                <X className="size-4" aria-hidden />
              </button>
            </div>

            <Command.List className="max-h-[62dvh] overflow-y-auto p-2">
              <Command.Empty className="py-8 text-center text-sm text-ink-2">
                No matching ticker, strategy, slot, or view.
              </Command.Empty>

              <Command.Group
                heading="Workbench Views"
                className="mb-2 [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-semibold [&_[cmdk-group-heading]]:text-ink-2"
              >
                <Command.Item
                  value="Live and Risk Rails Kalshi account orders positions"
                  onSelect={() => closeAnd(() => onSelectTab("live"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <ShieldCheck className="size-4 text-ink-2" strokeWidth={1.5} aria-hidden />
                    <span>Live &amp; Rails</span>
                  </span>
                  <Kbd>5</Kbd>
                </Command.Item>
                <Command.Item
                  value="Full Quant Edge Audit 1-Click Scorecard Go No-Go"
                  onSelect={() => closeAnd(() => onJumpQuantSection("audit"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <ShieldCheck className="size-4 text-info-foreground" strokeWidth={1.5} aria-hidden />
                    <span>Full Quant Edge Audit (Go / No-Go)</span>
                  </span>
                  <span className="mono text-xs text-info-foreground">1-Click</span>
                </Command.Item>
                <Command.Item
                  value="Live 15m Tape markets quotes"
                  onSelect={() => closeAnd(() => onSelectTab("tape"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Activity className="size-4 text-ink-2" aria-hidden />
                    <span>Live 15m Tape</span>
                  </span>
                  <Kbd>1</Kbd>
                </Command.Item>
                <Command.Item
                  value="Strategies leaderboard"
                  onSelect={() => closeAnd(() => onSelectTab("strategies"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Code2 className="size-4 text-ink-2" aria-hidden />
                    <span>Strategies</span>
                  </span>
                  <Kbd>2</Kbd>
                </Command.Item>
                <Command.Item
                  value="Open Strategy Sandbox editor backtest"
                  onSelect={() => closeAnd(onOpenSandbox)}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Code2 className="size-4 text-info-foreground" aria-hidden />
                    <span>Strategy Sandbox &amp; Backtester</span>
                  </span>
                  <span className="mono text-xs text-ink-3">Sandbox</span>
                </Command.Item>
                <Command.Item
                  value="Slot Desk compounding equity drawdown strikes"
                  onSelect={() => closeAnd(() => onSelectTab("slots"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Layers className="size-4 text-ink-2" aria-hidden />
                    <span>Slot Desk</span>
                  </span>
                  <Kbd>3</Kbd>
                </Command.Item>
                <Command.Item
                  value="Quant Lab calibration reliability brier ece monte carlo walk forward"
                  onSelect={() => closeAnd(() => onSelectTab("quant"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <FlaskConical className="size-4 text-ink-2" aria-hidden />
                    <span>Quant Lab</span>
                  </span>
                  <Kbd>4</Kbd>
                </Command.Item>
                <Command.Item
                  value="Quant Lab 500 Day Scale Planner Bankroll Kelly Liquidity"
                  onSelect={() => closeAnd(() => onJumpQuantSection("scale"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <TrendingUp className="size-4 text-info-foreground" aria-hidden />
                    <span>Quant Lab: $500/Day Scale Planner</span>
                  </span>
                  <span className="mono text-xs text-ink-3">180d</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab 5-Gate Readiness Scorecard Paper Research"
                  onSelect={() => closeAnd(() => onJumpQuantSection("scorecard"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <ShieldCheck className="size-4 text-ink-2" aria-hidden />
                    <span>Quant Lab: 5-Gate Readiness Scorecard</span>
                  </span>
                  <span className="mono text-xs text-ink-3">5 gates</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab Strategy Tournament Optimal 4-Slot Lineup OOS Expectancy"
                  onSelect={() => closeAnd(() => onJumpQuantSection("tournament"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Trophy className="size-4 text-info-foreground" aria-hidden />
                    <span>Quant Lab: Strategy Tournament &amp; 4-Slot Lineup</span>
                  </span>
                  <span className="mono text-xs text-ink-3">12 strategies</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab 12x12 Return Correlation Matrix Pearson Heatmap"
                  onSelect={() => closeAnd(() => onJumpQuantSection("correlation"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Grid3X3 className="size-4 text-ink-2" aria-hidden />
                    <span>Quant Lab: 12&times;12 Correlation Matrix</span>
                  </span>
                  <span className="mono text-xs text-ink-3">|r| &le; 0.65</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab Edge by Regime Heatmap Entry Price Seconds to Close"
                  onSelect={() => closeAnd(() => onJumpQuantSection("regime"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Grid3X3 className="size-4 text-success-foreground" aria-hidden />
                    <span>Quant Lab: Edge-by-Regime Heatmap</span>
                  </span>
                  <span className="mono text-xs text-ink-3">5&times;4 grid</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab Isotonic Recalibration PAV EV After Fees Gate"
                  onSelect={() => closeAnd(() => onJumpQuantSection("recalibration"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <SlidersHorizontal className="size-4 text-ink-2" aria-hidden />
                    <span>Quant Lab: Isotonic Recalibration (PAV)</span>
                  </span>
                  <span className="mono text-xs text-ink-3">IS/OOS</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab Calibration Reliability Chart Wilson Brier ECE"
                  onSelect={() => closeAnd(() => onJumpQuantSection("calibration"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <BarChart3 className="size-4 text-ink-2" aria-hidden />
                    <span>Quant Lab: Calibration Reliability</span>
                  </span>
                  <span className="mono text-xs text-ink-3">Wilson 95%</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab Monte Carlo Bootstrap Fan 1000 paths"
                  onSelect={() => closeAnd(() => onJumpQuantSection("montecarlo"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Dices className="size-4 text-ink-2" aria-hidden />
                    <span>Quant Lab: Monte Carlo Fan (1,000 Paths)</span>
                  </span>
                  <span className="mono text-xs text-ink-3">P5–P95</span>
                </Command.Item>
                <Command.Item
                  value="Quant Lab Walk Forward In-Sample Out-of-Sample Sharpe Sortino"
                  onSelect={() => closeAnd(() => onJumpQuantSection("walkforward"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <BarChart3 className="size-4 text-ink-2" aria-hidden />
                    <span>Quant Lab: Walk-Forward IS vs OOS</span>
                  </span>
                  <span className="mono text-xs text-ink-3">60 / 40</span>
                </Command.Item>
              </Command.Group>

              <Command.Group
                heading="Desk & Live Kalshi Actions"
                className="mb-2 [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-semibold [&_[cmdk-group-heading]]:text-ink-2"
              >
                <Command.Item
                  value="Open Live Kalshi Order Ticket Positions Orders Fills"
                  onSelect={() => closeAnd(onOpenOrderTicket)}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <SlidersHorizontal className="size-4 text-info-foreground" aria-hidden />
                    <span>Open Live Order Ticket &amp; Portfolio</span>
                  </span>
                  <span className="mono text-xs text-ink-3">Kalshi v2</span>
                </Command.Item>
                <Command.Item
                  value="Emergency Kill Switch cancel all resting orders disarm live"
                  onSelect={() => closeAnd(onKillSwitch)}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-destructive-foreground outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <AlertOctagon className="size-4 text-destructive-foreground" aria-hidden />
                    <span>Emergency Kill Switch (Cancel All &amp; Disarm)</span>
                  </span>
                  <span className="mono text-xs text-destructive-foreground">Kill Switch</span>
                </Command.Item>
                <Command.Item
                  value="Collect live Kalshi tape now"
                  onSelect={() => closeAnd(onCollect)}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <RefreshCw className="size-4 text-ink-2" aria-hidden />
                    <span>Collect Live Tape Now</span>
                  </span>
                  <span className="mono text-xs text-ink-3">Public GET</span>
                </Command.Item>
                <Command.Item
                  value="Export Daily Report Markdown md"
                  onSelect={() => closeAnd(() => onExportReport("md"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Download className="size-4 text-ink-2" aria-hidden />
                    <span>Export Daily Report (.md)</span>
                  </span>
                  <span className="mono text-xs text-ink-3">Markdown</span>
                </Command.Item>
                <Command.Item
                  value="Export Daily Report CSV spreadsheet"
                  onSelect={() => closeAnd(() => onExportReport("csv"))}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Download className="size-4 text-ink-2" aria-hidden />
                    <span>Export Daily Report (.csv)</span>
                  </span>
                  <span className="mono text-xs text-ink-3">CSV</span>
                </Command.Item>
                <Command.Item
                  value="Reseed Monte Carlo 1000 paths bootstrap"
                  onSelect={() => closeAnd(onReseedMonteCarlo)}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Dices className="size-4 text-ink-2" aria-hidden />
                    <span>Reseed Monte Carlo (1,000 Paths)</span>
                  </span>
                  <span className="mono text-xs text-ink-3">Bootstrap</span>
                </Command.Item>
                <Command.Item
                  value="Desk Settings Appearance Theme Updates Collector"
                  onSelect={() => closeAnd(onOpenSettings)}
                  className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                >
                  <span className="flex items-center gap-2">
                    <Settings className="size-4 text-ink-2" aria-hidden />
                    <span>Open Desk Settings &amp; Appearance</span>
                  </span>
                  <span className="mono text-xs text-ink-3">Settings</span>
                </Command.Item>
              </Command.Group>

              {strategies.length > 0 && (
                <Command.Group
                  heading={`Strategies (${strategies.length})`}
                  className="mb-2 [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-semibold [&_[cmdk-group-heading]]:text-ink-2"
                >
                  {strategies.map((s, idx) => {
                    const name = cleanStr(s.strategy, `strategy-${idx}`);
                    const v = classifyVerdict(s.verdict);
                    return (
                      <Command.Item
                        key={`${name}-${idx}`}
                        value={`strategy ${name} ${cleanStr(s.side, "")} ${v.label}`}
                        onSelect={() => closeAnd(() => onJumpStrategy(name))}
                        className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                      >
                        <span className="truncate font-medium">{name}</span>
                        <span className="flex shrink-0 items-center gap-1.5">
                          <Badge variant="outline" className="mono text-xs">
                            {cleanStr(s.side, "-").toUpperCase()}
                          </Badge>
                          <span className="mono text-xs text-ink-2">{v.label}</span>
                        </span>
                      </Command.Item>
                    );
                  })}
                </Command.Group>
              )}

              {slots.length > 0 && (
                <Command.Group
                  heading={`Active Slots (${slots.length})`}
                  className="mb-2 [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-semibold [&_[cmdk-group-heading]]:text-ink-2"
                >
                  {slots.map((s, idx) => {
                    const name = cleanStr(s.strategy, "unnamed");
                    return (
                      <Command.Item
                        key={`slot-${s.slot}-${idx}`}
                        value={`slot #${s.slot} ${name}`}
                        onSelect={() => closeAnd(() => onJumpSlot(s.slot, name))}
                        className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                      >
                        <span className="truncate">
                          <span className="mono mr-2 text-ink-3">#{s.slot}</span>
                          {name}
                        </span>
                        <span className="mono shrink-0 text-xs text-ink-2">
                          {s.strikes ?? 0}/3 {moneySigned(s.stint_pnl_dollars ?? s.paper_pnl_dollars)}
                        </span>
                      </Command.Item>
                    );
                  })}
                </Command.Group>
              )}

              {markets.length > 0 && (
                <Command.Group
                  heading={`15m Markets (${Math.min(25, markets.length)} shown)`}
                  className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-semibold [&_[cmdk-group-heading]]:text-ink-2"
                >
                  {markets.slice(0, 25).map((m, idx) => {
                    const ticker = cleanStr(m.ticker, `market-${idx}`);
                    const sym = seriesShort(m.series, m.ticker);
                    const prob = impliedYesProb(m);
                    return (
                      <Command.Item
                        key={`${ticker}-${idx}`}
                        value={`market ${ticker} ${sym} ${marketIsOpen(m) ? "open" : "settled"}`}
                        onSelect={() => closeAnd(() => onJumpMarket(ticker))}
                        className="flex min-h-11 cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-2 text-sm text-ink outline-none data-[selected=true]:bg-blue-soft sm:min-h-9"
                      >
                        <span className="mono truncate text-xs font-medium">{ticker}</span>
                        <span className="mono flex shrink-0 items-center gap-2 text-xs text-ink-2">
                          <Badge variant="outline" className="mono text-xs">
                            {sym}
                          </Badge>
                          <span>{prob !== null ? `YES ${cents(prob)}` : "-"}</span>
                        </span>
                      </Command.Item>
                    );
                  })}
                </Command.Group>
              )}
            </Command.List>

            <div className="flex flex-wrap items-center justify-between gap-2 border-t border-hairline bg-board/60 px-3.5 py-2 text-xs text-ink-3">
              <span>Live Kalshi Connected Trading &amp; Quant Desk</span>
              <span className="flex items-center gap-1.5">
                <Kbd>↑↓</Kbd>
                <span>navigate</span>
                <Kbd>↵</Kbd>
                <span>open</span>
                <Kbd>Esc</Kbd>
                <span>close</span>
              </span>
            </div>
          </Command>
      </div>
    </div>
  );
}
