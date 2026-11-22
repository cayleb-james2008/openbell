# OpenBell — Job (v0.6.0)

OpenBell is a local Kalshi 15-minute institutional quant research and paper compounding desk built in Rust: it collects live public Kalshi 15m market tape across active crypto series (BTC, ETH, SOL, XRP, DOGE), streams updates over SSE, scores 12 strategies on compound-sized paper slots with live sandbox backtesting, computes statistical calibration, PAV isotonic recalibration, EV-after-fees entry gating, and `$500/day` paper capital scaling diagnostics in Quant Lab, and publishes an honest daily audit trail — with zero order-placement capability anywhere in the binary.

A quant operator opens this screen to monitor the live 15-minute tape with bid/ask spread history, track the paper compounding pot and Stage 1 ($1,000) trajectory, inspect and live-backtest strategy entry rules, audit slot allocations with drawdown underwater and 3-strike rotations, and verify calibration reliability (Wilson 95% CIs, Brier, ECE), PAV isotonic recalibration, 180-day paper scaling capacity & 5-gate readiness scorecard, 1,000-path Monte Carlo drawdown risk, and walk-forward IS/OOS stability.

1. What is the paper compounding pot, its curve, and its progress toward the $1,000 Stage 1 target?
2. Is the live Kalshi 15m collector healthy and fresh, and what are the open 15-minute quotes, spreads, and settlement countdowns right now?
3. How is each strategy performing on paper, what are its entry rules, and how do edited rules backtest on collected tape?
4. How is paper capital allocated across compound-sized slots, what are the underwater drawdown, stint attribution, 3-strike standings, and exportable daily audit report?
5. Are market probabilities well-calibrated before and after PAV isotonic recalibration, what do the `$500/day` scale planner, liquidity ceiling, 5-gate readiness scorecard, Monte Carlo paths, and walk-forward splits say, and what does the read-only venue account report?
