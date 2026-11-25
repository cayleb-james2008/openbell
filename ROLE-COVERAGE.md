# UI Architecture & Role Coverage — OpenBell v0.9.0

OpenBell v0.9.0 is a lean Desktop Quant Workbench built around a persistent 48px Risk / Kill-Switch header strip, a slim 224px left desk rail (`15m Tape`, `Strategies`, `Paper Slots`, `Quant Lab & Tournament`, `Live & Rails`), collapsible Order Ticket and Strategy Rule Editor drawers, and a 1-click Full Quant Edge Audit scorecard.

Unused wrapper bloat (`@base-ui/react`, `recharts`, `@number-flow/react`, `react-use-measure`, `@radix-ui/react-dialog`, `@dnd-kit/*`, `jotai`, `date-fns`, `react-virtuoso`, and `HeroStrip`) was removed so `assets/index.html` stays fast and self-contained.

| Layer / Library | Role in OpenBell v0.9.0 | Implementation |
| --- | --- | --- |
| **Lean Workbench Primitives** | `Button`, `Badge`, `StatusPill`, `Kbd`, hairline `Table*`, `TrackerBar`, and `Disclosure` with `tabular-nums` Geist Mono metrics | `frontend/src/components/ui.tsx` |
| **Custom Quant & Market SVGs** | Market Price + Spread crosshair chart, Equity + Underwater Drawdown, Calibration Reliability (95% Wilson CI), 1,000-path Monte Carlo fan, 180-day Scale Trajectory, PAV Isotonic curve, 12x12 Correlation Matrix, 5x4 Regime Heatmap, and native SVG mini sparklines | `frontend/src/Charts.tsx` |
| **Vaul Drawers** | Right-hand desktop / bottom mobile sheets for Desk Settings (`#67` sole Light/Dark Appearance switch), Order Ticket & Live Rails, Strategy Rule Editor, Market Inspector, and Strategy Inspector | `vaul` in `App.tsx`, `SettingsSheet.tsx`, `TapeTab.tsx`, `StrategiesTab.tsx` |
| **cmdk Command Palette** | Global `Cmd+K` / `Ctrl+K` quick-jump across live 15m tickers, strategies, slots, Quant Lab panels, Full Quant Edge Audit, and emergency desk actions | `cmdk` in `frontend/src/CommandPalette.tsx` |
| **Sonner Notifications** | Non-blocking operational toasts for tape collection, Full Quant Edge Audit runs, live Kalshi order/kill-switch events, rule validation, and report exports | `sonner` in `frontend/src/App.tsx`, `EdgeAuditPanel.tsx`, `RuneSandbox.tsx`, `SlotsTab.tsx`, `QuantLabTab.tsx`, `ScalePlannerPanel.tsx`, `Update.tsx` |
| **Lucide Icons** | Consistent `1.5px`-stroke vector iconography (`lucide-react`), zero emojis | `lucide-react` across `frontend/src/*.tsx` |
| **Motion** | Reduced-motion-respecting single-pass SVG `pathLength` line drawing on analytics charts (`useReducedMotion`) | `motion/react` in `frontend/src/Charts.tsx`, `motionProps.ts` |
