# OpenBell — Job

OpenBell is a local Kalshi 15-minute quant research and paper compounding desk built in Rust + Rune: it collects live public Kalshi 15m market tape (BTC, ETH, SOL, XRP, DOGE), scores Rune strategies on compound-sized paper slots, and publishes an honest daily audit trail — with zero order-placement capability anywhere in the binary.

A quant operator opens this screen to monitor the live 15-minute tape, track the paper compounding pot and Stage 1 ($1,000) trajectory, inspect Rune strategy promotion evidence and `.rn` entry rules, and verify slot allocations and 3-strike rotations. A generic dashboard without live Kalshi 15m implied probability bars, `venue`/`tape`/`paper` provenance tags, 3-strike slot rotation, Rune script inspection, and the 100-window forward promotion bar would fail this job.

1. What is the paper compounding pot, its curve, and its progress toward the $1,000 Stage 1 target?
2. Is the live Kalshi 15m collector healthy and fresh, and what are the open 15-minute quotes right now?
3. How is each Rune strategy performing on paper, what are its entry rules, and is any strategy proven on forward evidence?
4. How is paper capital allocated across compound-sized slots, what is each slot's 3-strike standing, and what does the daily audit report say?
5. What does the read-only venue account report, or why are venue figures unavailable?
