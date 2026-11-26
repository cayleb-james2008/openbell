# Changelog

Each release is tagged `vMAJOR.MINOR.PATCH` and published on GitHub Releases with
binaries and a `SHA256SUMS` file. The running desk shows its version and can update
itself from the newest release (see README, "Versions and updates").

## 0.24.0 - 2026-11-25

- Added Causal Liquidity Depth & Scalable Bankroll Capacity Check (`compute_liquidity_capacity`, `LiquidityCapacityReport`, `LiquidityCapacityLeg`, `LiquidityScaleScenario` in `src/auto_allocator.rs`) backed by pre-settlement orderbook depth extraction (`window_depth_contracts` in `src/tournament.rs`) to evaluate `1x`, `2x`, and `5x` sizing partial-fill ratios, convex queue impact drag, `LIQUID` / `TIGHT` / `CROWDED` capacity tiers, and exact `max_scalable_dollars` and `max_scalable_bankroll` ceilings before impact erases post-fee OOS expectancy, while keeping `live_armed: false`.
- Wired the `#allocator-liquidity-capacity` card, 1x/2x/5x scaling stress cards, per-leg capacity table, and `makeOfflineLiquidityCapacity` parity into `frontend/src/AutoAllocatorPanel.tsx` and `frontend/src/lib/allocator.ts`.
- Replaced Linux `sha2` crate hashing in `src/update.rs` with `ring::digest::SHA256` (moving `sha2` alongside `num-bigint` into dev and Windows target dependencies in `Cargo.toml`), deduplicated JSON object body parsing via `parse_json_object_body` and `publish_live_status_json` in `src/main.rs`, and eliminated `serde_json::from_value` and unused `Deserialize` derives in `src/edge_audit.rs` (`5.84 MiB` / `6,120,800 B` release binary, `3.73 ms` cold boot, `17.39 MB` / `17,804 KB` Peak `VmHWM`, `6,761.8 req/s` `3.29 ms` p95 across 16 workers and 5 SSE streams).
- Updated `README.md` and refreshed light and dark `docs/screenshots/*.png`, verified `128/128` Rust tests, `25/25` raw-TCP security fuzz checks, `gate 101/101`, `replay 18/18`, and `sloplint 0/0/0`.

## 0.23.0 - 2026-11-25

- Added an execution-ready dry-run order plan and fee/spread sanity audit (`compute_dry_run_order_plan`, `DryRunOrderPlan`, `DryRunOrderPlanLeg`) in `src/auto_allocator.rs` backed by pre-settlement bid-ask spread tracking (`window_spread_cents` in `src/tournament.rs`) that computes exact whole-cent Kalshi maker (`0.0175 * p * (1 - p)`) and taker (`0.07 * p * (1 - p)`) fee drag, maker vs taker spread crossing cost, minimum contract threshold (`1..=250`) to clear whole-cent rounding with positive net OOS expectancy after slippage, and correlated cluster cap enforcement while keeping `live_armed: false`.
- Wired the `#allocator-dry-run-plan` read-only card and Copy Dry-Run Plan JSON action (`#allocator-copy-dry-run-btn`) into `frontend/src/AutoAllocatorPanel.tsx` alongside `DryRunOrderPlan` normalization, `makeOfflineDryRunPlan` parity, and `buildBriefingPayload` integration in `frontend/src/lib/allocator.ts`.
- Eliminated `serde_json::json!` value allocations from `collector_status_json`, `collector_health`, `/api/live/{account,positions,orders,fills}`, and kill-switch SSE events in `src/main.rs` via typed `serde::Serialize` structs and static empty responses, and removed unused `Deserialize` derives from read-only report structs in `src/auto_allocator.rs` (`5.84 MiB` / `6,124,896 B` release binary, `6.17 ms` cold boot, `17.52 MB` / `17,944 KB` Peak `VmHWM`, `4,662.3 req/s` `4.85 ms` p95 across 16 workers and 5 SSE streams).
- Updated `README.md` and refreshed light and dark `docs/screenshots/*.png`, verified `127/127` Rust tests, `25/25` raw-TCP security fuzz checks, `gate 101/101`, `replay 18/18`, and `sloplint 0/0/0`.

## 0.22.0 - 2026-11-25

- Added a causal correlated regime drawdown and exposure concentration ceiling guard in `src/auto_allocator.rs` (`apply_diversification_guard`, `DiversificationGuardReport`, `SessionBriefingReport`) that groups correlated winners across both multi-asset and same-asset strategy pairs (`|r| >= 0.55` cross-asset or `r >= 0.35` same-asset), computes a causal High-Vol loss-streak multiplier (`hv_regime_mult`), and caps cluster exposure at `regime_drawdown_ceiling_pct` bounded by `rails.max_daily_loss_dollars` while keeping `live_armed: false`.
- Surfaced the `#allocator-briefing-ceiling-pill` status badge, ceiling headroom metrics in `#allocator-session-briefing`, and High-Vol cluster ceiling details in `#allocator-forward-envelope` inside `frontend/src/AutoAllocatorPanel.tsx`, with matching `DiversificationGuardReport` / `SessionBriefingReport` types and offline defaults in `frontend/src/lib/allocator.ts`.
- Replaced `ureq`'s `json` feature and default `base64`/`sha2` `std` features, switched `itoa`/`ryu` to `opt-level = "z"`, eliminated seven `serde_json::to_value` DOM allocations and repeated `serde_json::json!` error maps via typed `Serialize` helpers (`#[inline(never)]`) in `src/main.rs`, deduplicated `Strategy::load` script preparation, added `EINTR`-safe socket reads (`read_retry`), and tuned worker stack (`1 MB`), `mallopt` mmap threshold (`16 KB`), and burst pool bounds (`24` max workers) (`5.84 MiB` / `6,128,992 B` release binary, `7.36 ms` cold boot, `17.49 MB` / `17,908 KB` Peak `VmHWM`, `3,239.5 req/s` `7.37 ms` p95 under 16-worker + 5-SSE contention).
- Cleaned prose across `README.md` and this changelog entry, refreshed light and dark screenshots in `docs/screenshots/`, and verified `126/126` Rust tests and `25/25` raw-TCP security fuzz checks.

## 0.21.0 - 2026-11-25

- Added causal slippage sensitivity ladder across `+0c`, `+1c`, `+2c`, and `+3c` adverse fill levels in `src/auto_allocator.rs` (`compute_slippage_ladder_and_cushion`, `SlippageLadderStep`), plus break-even slippage ceiling (`c`) and cushion status (`THIN` under `1c`, `MODERATE` `1c` to `2c`, `SAFE` at `>=2c` with `0.0%` daily rail breach).
- Wired `#allocator-briefing-cushion-pill` and the four-step `#allocator-slippage-ladder` strip into `frontend/src/AutoAllocatorPanel.tsx` along with leaderboard preset cushion pills, `buildBriefingPayload` fields, and offline client parity in `frontend/src/lib/allocator.ts`.
- Disabled default `rune` `emit` diagnostics formatting in `Cargo.toml` and `src/analytics.rs`, moved `num-bigint` to Windows/dev dependencies, shrunk `build_latest_markets_json` allocations and omitted zero depth fields in `src/main.rs`, and sized the HTTP pool to 16 workers with pre-framed hot close payloads (`5.88 MB` release binary, `18.23 MB` Peak `VmHWM`, `4,011.4 req/s` cold-close).
- Cleaned prose across `README.md`, this changelog entry, and UI strings in `AutoAllocatorPanel.tsx` and `LiveTradingPanel.tsx`.

## 0.20.0 - 2026-11-25

- **Walk-Forward Regime Stability Validator (`src/tournament.rs`, `src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Partitioned settled 15m tape windows strictly causally into **Calm** vs **High-Vol / Wide-Spread** regimes using a backward-looking trailing 60th-percentile threshold (`< close_epoch`, zero future lookahead) over pre-settlement quote spread and price range (`compute_window_vol_score` + `classify_causal_epoch_regimes`).
  - Evaluated per-preset regime net expectancy and `+1c` adverse-fill survival across both regimes (`evaluate_regime_stability`) and surfaced crisp **Regime Stability** badges (`ALL-REGIME` vs `CALM-ONLY` / `VOL-SENSITIVE`) inside the Strategy Leaderboard, `RegimeStabilityReport`, and Session Briefing while preserving `live_armed: false` and the `0.0%` daily rail-breach hard gate.
- **Persistent HTTP Keep-Alive, Zero-Copy Queue & `<20 MB` RSS Diet (`src/main.rs`):**
  - Removed 4 KB per-connection buffer copies from `QueuedConn`, added bounded `150 ms` `live_hot_cache` zero-stat fast-paths invalidated on mutations, enabled adaptive HTTP/1.1 keep-alive with load shedding under pool saturation, bound `TcpListener` ahead of worker/background spawns, and tuned glibc `M_ARENA_MAX=1` + `M_MMAP_THRESHOLD=32KB`.

## 0.19.0 - 2026-11-25

- **1-Click Session Briefing & Slippage Attribution Card (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added causal `SessionBriefingReport` (`compute_session_briefing`) detailing expected vs realized fill edge, weighted maker/taker fee drag & maker fee savings, cross-asset correlation cluster heat trim penalty, and daily loss rail dollar/percentage headroom against worst 1-day (`<=96` window) stressed OOS drawdowns (`PASS_READY` / `HOLD_PAPER` / `NO_DATA`, strictly preserving `live_armed: false`).
  - Added `#allocator-session-briefing` card with 1-click **Copy Briefing JSON** (`#allocator-copy-briefing-btn`) and **Download Briefing JSON** (`#allocator-download-briefing-btn`).
- **Zero-Copy HTTP & Cache Fast-Path (`src/main.rs`):**
  - Added `write_all_vectored` (`IoSlice` `writev` zero-copy JSON & asset responses), precomputed `OnceLock` `/api/version`, typed `HomeBody` serialization, `read_request` stack buffer + allowlisted header parsing (eliminating per-request heap allocs for ignored headers), O(1) `TapeEntry.conflicting_settlements` + `tape_latest_json`, `symlink_metadata`-only `strategies_dir_key`, and 256 KB-stack HTTP workers — recovering `>4,500 req/s` throughput while keeping release binary `<5.95 MB` and Core Peak `VmHWM < 22 MB`.

## 0.18.0 - 2026-11-25

- **Multi-Asset Correlation & Portfolio Diversification Guard (`src/auto_allocator.rs`, `src/main.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added causal cross-asset (`BTC`/`ETH`/`SOL`/`XRP`/`DOGE`) window-return correlation clustering (`|r| >= 0.55` across settled windows, with IS-only clustering on historical OOS replay for zero lookahead) and automatic correlated-cluster heat capping (`cluster_heat_cap_pct = max(max_slot_risk_pct, 0.65 * max_total_heat_pct)`) before Hard Risk Rails can breach (`apply_diversification_guard`).
  - Added the **`Diversified Safe Basket`** (`8%` slot / `30%` heat, cross-asset cluster guard, `75%` maker QK) preset (`0.0%` daily rail breach hard gate, `live_armed: false`), **`Cluster Guard: ON/OFF`** toggle (`#allocator-diversification-toggle`), compact **Diversified / Cluster Capped** pill (`#allocator-diversification-pill`), cluster breakdown strip in `#allocator-forward-envelope`, and 1-click **`Diversified Safe Basket`** button (`#allocator-diversified-basket-btn`).
- **Sub-6.00 MB Binary & Sub-20 MB Core Peak RSS Diet (`Cargo.toml`, `src/main.rs`):**
  - Switched release crate codegen to `opt-level = "z"` (keeping `serde`, `serde_json`, `memchr`, `itoa`, and `ryu` at `opt-level = 3`), replaced the persistent `SHARED_RUNE` compiler registry with a transient `CompileCtxGuard` + `Weak<RuntimeContext>` and `trim_heap()` before simulation, keyed `PrefixCache` by `Sym(u32)`, and skipped `PREFIX_CACHE` inserts for already-settled windows.

## 0.17.0 - 2026-11-25

- **Paper Forward-Test Envelope Drift Monitor & 1-Click Safe Rotation (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added causal `ForwardEnvelopeMonitor` (`compute_forward_envelope_monitor`) comparing ongoing synthetic/paper forward-test expectancy, slippage drift, and realized equity against the active preset's predicted bootstrap `P5–P50` trajectory (`ON_TRACK` vs `DRIFT_DEMOTE` on `P5` floor violation, `DECAY_CRITICAL`, `>2.5c` slippage drift, or `>0%` rail breach) and selecting the highest-ranked `0%`-rail-breach rotation candidate (`next_safe_preset_id`).
  - Added `#allocator-forward-envelope` strip with **`ON TRACK`** / **`DRIFT / DEMOTE`** badge and 1-click **Rotate Safe** button (`#allocator-rotate-safe-btn`), strictly preserving `live_armed: false`.
- **Sub-6.20 MB Binary & Sub-29 MB Core Peak RSS Diet (`Cargo.toml`, `src/main.rs`):**
  - Replaced `6 x Arc<str>` (`96 B/snapshot`) on `Snapshot` with a 4-byte lock-free-fast-path `Sym(u32)` interner (`64 B` cache-line `Snapshot`, down from `200 B` in `v0.15.0` -> `-68%` struct footprint) plus `SettledKey` / `ResultTrack` `Sym` compaction, and set `[profile.release] opt-level = "s"` (keeping `serde` and `serde_json` at `opt-level = 3`), cutting release binary size and core 102k-tick `VmHWM` cleanly under `6.20 MB` and `29 MB`.
- **Typography Ratio Cleanup (`frontend/src/TapeTab.tsx`, `frontend/src/TapeCharts.tsx`):**
  - Kept `Geist Mono` focused on numerical prices, spreads, and KPI figures while rendering tickers, series badges, and bar labels in `Geist` (`tabular-nums`), clearing `impeccable detect` `overused-font` (`DETECT []`).

## 0.16.0 - 2026-11-25

- **Zero-Config Strategy Leaderboard, 1-Click Safe Paper Preset & Audit JSON Export (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added deterministic `StrategyPresetCandidate` leaderboard (`compute_preset_leaderboard`) evaluating 5 calibrated maker/taker + Kelly/equal-weight + edge-floor presets across OOS and `+1c` adverse-fill bootstrap trials (`500` trials per preset) with a hard `daily_rail_breach_prob == 0.0%` gate and net-of-fee risk-adjusted scoring (`top_safe_preset`).
  - Added the **Zero-Config Strategy Leaderboard** panel in `AutoAllocatorPanel` with 1-click **Apply -> Paper Forward-Test** (strictly keeping `live_armed: false`) and 1-click **Export Audit JSON** download.
- **Sub-8.0 MB Binary & Sub-36 MB Core RSS Allocation Diet (`Cargo.toml`, `src/main.rs`, `src/analytics.rs`, `src/recalibration.rs`, `src/tournament.rs`):**
  - Compacted `Snapshot` prices (`OptPrice(u32)` exact-cent/f32 encoding) and top-of-book depths (`f32`), shared a single `rune::Context`/`RuntimeContext` across all 16 strategies, replaced `serde_json::to_value` intermediates in `desk_json_shared` with direct typed serialization, tuned GNU/Linux `mallopt` + `malloc_trim(0)` after heavy tape/analytics passes, and enabled dependency size optimization (`opt-level = "z"` on third-party crates while keeping `openbell-rs`, `serde`, and `serde_json` at `opt-level = 3`), cutting release binary size below `6.5 MB` and core 102k-tick `VmHWM` below `35 MB`.
- **`#154` Zero-GPU E2E Isolation (`frontend/e2e/fixture.mjs`, `frontend/e2e/lints.sh`):**
  - Added software-rendering Chromium flags (`--disable-gpu`, `--disable-gpu-compositing`, `--disable-gpu-sandbox`, `--disable-accelerated-2d-canvas`, `--disable-accelerated-video-decode`, `--num-raster-threads=1`) and `LIBGL_ALWAYS_SOFTWARE=1 VK_ICD_FILENAMES=/dev/null`.

## 0.15.0 - 2026-11-25

- **Causal Bootstrap Monte-Carlo Equity & Drawdown Stress Simulator (`src/auto_allocator.rs`, `frontend/src/Charts.tsx`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added `BootstrapStressSummary` (`compute_bootstrap_stress_sim`): deterministically resamples strictly causal OOS window returns across `1,000` trials (`30` trading days × `96` windows/day) under both `+0c` baseline and `+1c` adverse-fill stress to report `P5 / P50 / P95` 30-day equity trajectories, `P95` max drawdown, daily Hard-Risk-Rail breach probability (`daily_rail_breach_prob_pct`), and `P(>=$500/day)` probability (`prob_500_day_pct`).
  - Added interactive zero-bloat custom SVG `BootstrapStressFanChart` inside `AutoAllocatorPanel` plotting the `P5–P95` confidence band, `P50` trajectory, `+1c` stress trajectory, and tail-risk readouts before live arming.

## 0.14.0 - 2026-11-25

- **Multi-Market Spread / Fee / Edge Quality Screener & 1-Click Basket Preflight (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added strictly causal `edge_screener` ranking discovered strategies and dominant 15m Kalshi market series (`KXBTC15M`, `KXETH15M`, `KXSOL15M`, `KXXRP15M`) by fee-adjusted maker expectancy, taker-vs-maker fee edge saved, break-even queue fill headroom, walk-forward fold consistency, `$500/day` daily contribution share, and composite quality score.
  - Added causal `best_basket` assembly and 1-click **Stage Best Basket** dry-run preflight (`POST /api/live/order` with `dry_run: true`) verifying multi-leg Hard Risk Rail compliance before staging into the Order Ticket while keeping `live_armed: false` on boot.
- **Binary Size & `.rodata` Diet (`Cargo.toml`, `src/gzip_inflate.rs`, `frontend/vite.config.ts`, `src/main.rs`):**
  - Enabled `lto = "fat"` and `panic = "abort"` in `[profile.release]`, and eliminated ~787 KB of duplicate uncompressed HTML/JS `.rodata` by embedding pre-gzipped assets once (`index.html.gz` + `/chunks/*.js.gz`) with build-time SHA-256 `INDEX_HTML_ETAG` and a verified pure-Rust RFC 1951/1952 `inflate_gzip` fallback for `identity` HTTP clients.

## 0.13.0 - 2026-11-25

- **Initial-Shell `React.lazy` `/chunks/*` Code-Splitting (`frontend/vite.config.ts`, `src/embedded_chunks.rs`, `src/main.rs`):**
  - Split secondary tabs, Quant Lab diagnostics, Command Palette, Order Ticket, and Settings into 21 compile-time embedded pre-gzipped `/chunks/*` assets with strong SHA-256 `ETag` + `304`, `Vary: Accept-Encoding`, `Cache-Control: public, max-age=31536000, immutable`, `SEC_HEADERS`, CWE-22 traversal guard, and zero entry back-imports.
  - Reduced initial `GET /` shell from `1,054 KB` (`363 KB` gzip) down to `92 KB` raw / `21 KB` gzip.
- **Maker Queue Fill-Rate Sensitivity & Shadow Drift Gauge (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added causal break-even maker fill-rate (`breakeven_fill_rate_pct`), configurable `min_fill_rate_pct` queue-fill gate (default `40%`), `25% / 50% / 75% / 100%` daily profit fill sensitivity table, and live-shadow vs paper expectancy drift gauge (`drift_gauge`) with concrete scaling blockers.
- **Peak RSS (`VmHWM`) Optimization (`src/main.rs`, `src/tournament.rs`):**
  - Replaced post-deserialize `Snapshot` interning with thread-local RAII `TapePoolGuard` for zero-throwaway `Arc<str>` interning during JSON parsing, switched `PREFIX_CACHE` keys from `String` to shared `Arc<str>`, and shrunk `TournamentTapeContext` allocations.

## 0.12.0 - 2026-11-25

- **Causal Regime-Aware Kelly Throttle & Edge Attribution (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Added strictly causal rolling drawdown and adverse-streak Kelly throttling (`enable_regime_throttle`, default `true`) in OOS replay and forward allocation (`0.35x–1.00x`), preserving zero lookahead across settled epochs while cutting drawdown during adverse regimes.
  - Added per-winner `gross_expectancy_cents` and `fee_drag_cents` plus portfolio **OOS Regime & Fee-Drag Attribution** (`Underdog <45c`, `Balanced 45-69c`, `Favorite >=70c`) and a 1-click **Regime Throttle** toggle in `AutoAllocatorPanel`.
- **Peak RSS (`VmHWM`) `Arc<str>` Interning & Bundle Diet (`src/main.rs`, `frontend/src/fonts.css`, `frontend/src/{App,TapeTab,EdgeAuditPanel,Charts}.tsx`):**
  - Converted `Snapshot` string fields to interned `Arc<str>` in `App::tape` / `extend_tape` and removed redundant `markets: HashSet<String>` in `TapeEntry`, slashing 100k-tick Peak RSS (`VmHWM`) by ~20+ MB down below `62 MB`.
  - Consolidated `fonts.css` (`Geist` `400` + `500..600` weight range, eliminating static `Geist-SemiBold.woff2`) and switched `motion/react` to `<LazyMotion features={domAnimation}>` + `<m.*>`, shrinking `assets/index.html` to `1,029 kB` raw / `355 kB` gzip.

## 0.11.0 - 2026-11-25

- **Rolling Walk-Forward Stress Guard & 1-Click Dry-Run Preflight (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/live.ts`):**
  - Added chronological 3-fold walk-forward stability and `+0c / +1c / +2c` adverse-fill slippage stress (`StressProfile`) to the causal Autonomous Edge Allocator (`require_stress_resilience`, default `true`), automatically quarantining brittle 1-fold flukes and sub-1c noise edges while reporting portfolio `+1c` / `+2c` stress daily projections.
  - Added 1-click **Preflight** (`POST /api/live/order` with `dry_run: true`) and **Rail Headroom Preview** directly inside `AutoAllocatorPanel`, surfacing server-verified order notional, Kalshi fee, and exposure headroom before staging into the Order Ticket while keeping `live_armed: false` on boot.
- **Peak RSS (`VmHWM`) Streaming Tape Diet & Chart Render Optimization (`src/main.rs`, `frontend/src/Charts.tsx`, `frontend/vite.config.ts`):**
  - Replaced full-file tape reads and duplicate flat snapshot buffers in `App::tape` and `existing_settled_keys` with streaming 64 KB `BufReader::read_until`, folding collector health inline and shrinking per-window `Vec<Snapshot>` allocations.
  - Memoized `AllocatorComparisonChart` geometry/paths with `useMemo` to eliminate hover re-render overhead.

## 0.10.0 - 2026-11-25

- **Autonomous Edge & Capital Allocator (`src/auto_allocator.rs`, `frontend/src/AutoAllocatorPanel.tsx`, `frontend/src/lib/allocator.ts`):**
  - Integrated 4 new post-only maker, momentum, and fade strategies (`maker_late_theta_no.rn`, `maker_spread_capture.rn`, `spot_lead_momentum_yes.rn`, `tail_overreaction_fade.rn`) expanding the built-in library to **16 strategies**.
  - Added `GET /api/allocator` and `POST /api/allocator/plan` with zero-lookahead causal In-Sample (`close_epoch < split_epoch`) selection -> held-out OOS replay (`causal_is_to_oos`), `1/4` Kalshi maker fees (`0.0175 * p * (1 - p)` with whole-cent order fee rounding `ceil(rate * C * p * (1 - p) * 100) / 100`), pairwise correlation pruning (`|r| <= 0.65`), Fractional-Kelly (`quarter_kelly` / `half_kelly` / `equal_winners`) heat caps, automatic 0% quarantine of negative/thin/correlated shapes, `$500/day` Go/No-Go verdict & blockers, and 1-click order-ticket staging behind fail-closed Hard Risk Rails (`live_armed: false` on boot).
  - Added `post_only` maker limit support (`KalshiOrderRequest::maker_limit`, `POST /api/live/order`, Live Order Ticket `Post-Only ON` toggle) and the `#openbell-brand-mark` Gaussian bell SVG mark in the header, plus a zero-bloat custom SVG `AllocatorComparisonChart` in `frontend/src/Charts.tsx`.
- **Runtime Speed, Gzip + ETag/304 & Binary Diet (`Cargo.toml`, `frontend/vite.config.ts`, `src/main.rs`):**
  - Added `[profile.release]` (`lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`, `opt-level = 3`), pre-compressed `assets/index.html.gz` (`Accept-Encoding: gzip`), strong SHA-256 `ETag` + `304 Not Modified`, `Vary: Accept-Encoding`, and security headers (`X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`).
  - Replaced per-connection thread spawning with a bounded 32-worker HTTP pool (`set_nodelay(true)`), chunked buffered request header parsing (`read_request`), zero-copy `seam_of_bytes` + streaming `parse_tape_bytes`, and pre-serialized JSON caches (`desk_json_shared`, `allocator_shared`).

## 0.9.0 - 2026-11-25

- **Institutional Desktop Quant Workbench (`frontend/src/App.tsx`, `frontend/src/components/ui.tsx`, `frontend/src/EdgeAuditPanel.tsx` -> slimmed `assets/index.html`):**
  - Replaced stacked telemetry/hero cards with a persistent 48px opaque Risk / Kill-Switch header (`DISARMED` fail-closed status pill + 1-click **Emergency Kill Switch** + 1-click **Edge Audit**), a slim 224px left desk rail (`15m Tape`, `Strategies`, `Paper Slots`, `Quant Lab & Tournament`, `Live & Rails`), collapsible Vaul drawers for **Order Ticket** and **Strategy Rule Editor**, and above-the-fold market/strategy tables while keeping `#67` Settings-only Light/Dark theme and `#155` zero `"Rune"` jargon.
  - Replaced `recharts`, `@number-flow/react`, `react-use-measure`, `@base-ui/react`, and `@radix-ui/react-dialog` wrappers with lean SVG + DOM primitives (`frontend/src/components/ui.tsx`, native SVG `TremorSpark`), cutting `assets/index.html` by ~38% (`1,733 kB -> 1,081 kB`).
- **1-Click Full Quant Edge Audit & Causal Lookahead-Canary Tests (`src/edge_audit.rs`, `GET /api/analytics/edge-audit`, `tests/v090_edge_audit.rs`):**
  - Added `GET /api/analytics/edge-audit[?dataset=fixture&slippage_cents=0..5]` and the `EdgeAuditPanel` UI evaluating all 12 strategies -> OOS Tournament -> uncorrelated 4-slot lineup (`|r| <= 0.65`) -> PAV isotonic recalibration -> Kalshi fee & slippage-adjusted net EV -> Monte Carlo drawdown & 5-gate readiness in one click.
  - Added causal lookahead-canary unit & HTTP integration tests proving signal-bar truncation is strictly invariant to post-signal future bars while a 1-bar lookahead canary fails immediately.
- **Backend Security, Bounds & Tape-Caching Hardening (`src/kalshi_live.rs`, `src/risk_rails.rs`, `src/update.rs`, `src/main.rs`):**
  - Enforced `symlink_metadata` regular-file & 64 KB cap checks before reading PEM files, atomic `O_EXCL` (`create_new(true)`) `0600` credential writes, and private `0700` temp download directories for self-updates.
  - Added finite-bounds guards on risk-rail notional/exposure math, query-parameter-exact `dataset` parsing, malformed `Content-Length` `400` rejections, bounded HTTP connection concurrency (`128`), `SETTLED_KEYS_CACHE`, and singleflight `tape_compute_lock` / `edge_audit_compute_lock`.

## 0.8.0 - 2026-11-22

- **Live Connected Kalshi Trade API v2 Client & RSA-PSS SHA-256 Signer (`src/kalshi_live.rs`):**
  - Genuine Kalshi v2 REST integration for both Demo (`https://demo-api.kalshi.co/trade-api/v2`) and Production (`https://api.elections.kalshi.com/trade-api/v2`) with RSA-PSS SHA-256 request signing (`KALSHI-ACCESS-KEY`, `KALSHI-ACCESS-TIMESTAMP`, `KALSHI-ACCESS-SIGNATURE`), supporting `GET /portfolio/{balance,positions,orders,fills}`, `POST /portfolio/orders`, `DELETE /portfolio/orders/{id}`, and `cancel_all_orders()`.
  - Dual-target offline-verified cryptography (`ring` on Unix/macOS, pure-Rust RFC 8017 RSA-PSS on Windows `x86_64-pc-windows-msvc`), strict `0600` credential storage under `OPENBELL_DATA`, and redacted `Debug` output that never logs or echoes private key PEM bytes.
- **Fail-Closed Hard Risk Rails, Loopback+CSRF Guard & Emergency Kill Switch (`src/risk_rails.rs`, `/api/live/*`, `.loop/checks/rails.sh`):**
  - Always boots disarmed (`live_armed: false`), requiring configured credentials AND explicit `confirm_arm: true` via `POST /api/live/arm` before any live order can route.
  - Enforces hard caps on every order (`max_order_contracts`, `max_order_dollars`, `max_daily_loss_dollars`, `max_open_exposure_dollars`, `allowed_ticker_prefix`, whole-cent `1..=99` price check) with fail-closed HTTP `403` rejections, strict loopback-only + `X-OpenBell-CSRF: 1` protection across all `/api/live/*` routes, and an instant **Emergency Kill Switch** (`POST /api/live/kill-switch`) that disarms first and cancels all resting orders.
  - Replaced legacy `.loop/checks/fence.sh` with `.loop/checks/rails.sh` mechanically proving zero hardcoded secrets, disarmed-by-default boot, `403` fail-closed orders & CSRF enforcement, and RSA-PSS + risk-rail unit suites while preserving `.loop/checks/equity.sh` intact.
- **Live Kalshi Connected Trading & Quant Desk UI (`frontend/src/LiveTradingPanel.tsx`, `frontend/src/lib/live.ts` -> single-file `assets/index.html` `1733 kB`):**
  - Updated all desk copy from shadow/paper-only wording to truthful **Live Kalshi Connected Trading & Quant Desk** copy; added Live Account & Positions Strip (`Cash`, `Portfolio`, `Positions`, `Orders`, `Fills`), `Paper / Arm Live` switcher with explicit confirmation modal, Live Order Ticket with 7-point real-time Hard Risk Rails pre-check, 1-click `Trade` prefill from `Live 15m Tape`, per-order `Cancel`, crimson **Emergency Kill Switch**, and `#settings` Kalshi API & Risk Rails panel.
  - Preserved all `$500/Day` Capital Scale Planner, PAV Isotonic Recalibration, Strategy Tournament, `12×12` Correlation & `5×4` Regime Heatmaps, Auto-Collector, `#67` Settings-only Light/Dark theme, and `#155` plain wording.
- **Linux & Windows Standalone Desktop App Window (`src/open.rs`):** upgraded startup opener to launch a chromeless desktop app window (`chromium`/`google-chrome`/`brave` `--app=<url>` on Linux, `msedge.exe`/`chrome.exe --app=<url>` on Windows, honoring `--no-open` / `--service` / `OPENBELL_NO_OPEN=1`).

## 0.7.0 - 2026-11-22

- **Paper Strategy Tournament, 12×12 Correlation Matrix, Optimal Uncorrelated 4-Slot Lineup & 2D Regime Heatmap (`src/tournament.rs`, `GET /api/analytics/tournament[?dataset=fixture&slippage_cents=0..5]`):**
  - Ranks all 12 paper strategies strictly on held-out Out-of-Sample (last 40%) net expectancy after Kalshi fees and configurable slippage (`0¢..5¢`), with Sharpe, Sortino, Max Drawdown %, sample count, Wilson 95% confidence intervals, and per-series breakdown (`BTC`/`ETH`/`SOL`/`XRP`/`DOGE`).
  - Computes an exact symmetric `12×12` pairwise Pearson return correlation matrix across shared settled windows and runs exact combinatorial search for the highest-OOS-expectancy 4-slot portfolio subject to `|r| <= 0.65`, with explicit honest fallback disclosure whenever fewer than 4 strategies achieve positive OOS expectancy (never padding with negative-EV strategies).
  - Builds a 2D Entry Price Bucket (`10-30c`..`90-99c`) × Seconds-to-Close (`>600s`..`<120s`) Edge-by-Regime Heatmap reporting `n`, realized win rate, average fee, net paper expectancy (`¢/ct`), and Wilson sample-depth warnings (`n < 20`), backed by singleflight `RwLock` `TournamentTapeContext` caching.
- **Toggleable Background Public Tape Collector & 200-Window Readiness Gate (`--auto-collect`, `GET /api/collector/status`, `POST /api/collector/schedule`):**
  - Adds a safe toggleable background public tape collector (`--auto-collect` / `OPENBELL_AUTO_COLLECT=1` / runtime `POST /api/collector/schedule` `{ enabled, interval_secs: 30..=900 }`, parked idle by default with zero unsolicited venue calls) featuring adaptive HTTP 429 exponential backoff, live SSE `event: tape` broadcasts (`type: "schedule" | "collect"`), and `settled_windows / 200` Readiness Gate progress telemetry — strictly read-only public Kalshi GET endpoints only (`fence.sh` `FENCE OK`).
- **Quant Lab Tournament & Auto-Collector UI (`frontend/` -> single-file `assets/index.html`):** adds the Paper Strategy Tournament & Optimal 4-Slot Lineup panel, interactive `12×12` Correlation Matrix SVG, interactive `5×4` Edge-by-Regime Heatmap SVG, and synchronized `Off / 60s / 180s` Auto-Collector Control Strip + 200-Window Readiness progress bar inside `Live 15m Tape` and Desk Settings while preserving 4 clean tabs, opaque sticky header, Settings-only Light/Dark theme, zero new Cargo dependencies, and `.loop/checks/fence.sh` `FENCE OK` / `.loop/checks/equity.sh` `EQUITY OK`.

## 0.6.0 - 2026-11-22

- **Monotone PAV Isotonic Probability Recalibration & EV-After-Fees Gate (`src/recalibration.rs`, `GET /api/analytics/recalibration[?dataset=fixture&slippage_cents=0..5]`):**
  - Fits weighted Pool-Adjacent-Violators (PAV) isotonic regression strictly on chronological In-Sample (first 60%) settled windows and evaluates strictly on held-out Out-of-Sample (last 40%) windows with zero lookahead leakage.
  - Emits 20 five-point probability buckets (`0-5%`..`95-100%`) with exact 95% Wilson score confidence intervals, before-vs-after OOS Brier score, log loss, ECE, 80%-power binomial Minimum Detectable Edge, and a retrospective EV-After-Fees entry gate (`p_cal > entry_price + fee + slippage_margin`) showing accepted vs refused count, win rate, and paper P&L impact.
- **`$500/Day` Paper Capital Scaling & Liquidity Capacity Engine (`src/scaling.rs`, `POST /api/analytics/scale-plan[?dataset=fixture]`):**
  - Computes empirical daily paper run-rate vs target (`$500/day` default) strictly from OOS fee- and slippage-adjusted expectancy, returning `required_bankroll_equal_slots`, `required_bankroll_quarter_kelly`, and `required_bankroll_half_kelly` — explicitly `null` with an honest explanation whenever OOS net expectancy is `<= 0`.
  - Computes single-series (`96` 15m windows/day × `max_contracts_per_window`) and multi-series liquidity ceilings, exact `fill_ratio_at_target`, and incorporates live observed top-of-book depth from the public Kalshi collector (`yes_bid_size`/`no_bid_size`, `#[serde(default)]` `0.0` fallback).
  - Runs a deterministic 180-trading-day 1,000-path Xoshiro256++ Monte Carlo compounding trajectory (`P10`/`P50`/`P90` equity & daily run-rate, `prob_reach_target`, `median_days_to_target`, `prob_dd_gte_25_pct`) clearly separating `"Empirical Tape"` from optional `"Sensitivity (Assumed Edge)"`, plus a 5-gate mechanical readiness scorecard (`sample_depth >= 200`, `oos_net_expectancy > 0`, `oos_ece < 0.05`, `max_drawdown_pct < 20.0`, `liquidity_fill_ratio >= 0.75`) -> `READY_FOR_OWNER_REVIEW` vs `PAPER_RESEARCH_ONLY`.
- **Quant Lab `$500/Day` Scale Planner & Recalibration Panels (`frontend/` -> single-file `assets/index.html`):** adds interactive Scale Planner controls, Liquidity Capacity bar, 180-day Compounding Trajectory SVG with `$500/day` target reference line & hover crosshair, 5-Gate Readiness Scorecard, Before-vs-After PAV Isotonic Recalibration SVG with 95% Wilson whiskers, EV-After-Fees Gate impact table, and `Cmd+K` jump actions inside Quant Lab while preserving 4 clean tabs, opaque sticky header, Settings-only Light/Dark theme, zero new Cargo dependencies, and `.loop/checks/fence.sh` `FENCE OK` / `.loop/checks/equity.sh` `EQUITY OK`.

## 0.5.0 - 2026-11-22

- **12-Strategy Paper Library (`strategies/*.rn`):** expanded the seeded built-in library from 5 to 12 distinct paper strategies (`bid_ask_imbalance`, `favourite_floor`, `favourite_late`, `late_convergence_cap`, `longshot_late`, `mean_reversion_fade`, `mid_window_momentum`, `no_favourite`, `opening_range_break`, `spread_compression`, `time_decay_harvest`, `volatility_squeeze`) with whole-cent `0.01..=0.99` quote discipline and strict paper-only isolation.
- **Paper Quant Diagnostics Engine (`src/analytics.rs`) & 5 Quant HTTP Endpoints:**
  - `GET /api/analytics/calibration[?dataset=fixture]` — 10-decile reliability buckets, exact 95% Wilson score confidence intervals, Brier score, Brier skill score, Expected Calibration Error (ECE), and Maximum Calibration Error (MCE).
  - `GET /api/analytics/walkforward[?dataset=fixture]` — chronological In-Sample (60%) vs Out-of-Sample (40%) walk-forward attribution with Sharpe, Sortino, Profit Factor, Max Drawdown %, Calmar ratio, Expectancy, Fee Drag %, underwater drawdown curve, and win/loss streak histograms.
  - `POST /api/analytics/montecarlo` — deterministic seeded Xoshiro256++ bootstrap (1,000+ paths) producing P5/P25/P50/P75/P95 equity fan bands and `P(DD >= 10%)` / `P(DD >= 25%)` / `P(DD >= 50%)` drawdown probabilities.
  - `POST /api/strategy/validate` & `POST /api/backtest` — in-memory strategy sandbox compiler & backtester rejecting forbidden non-paper/network/credential patterns before compiling, bounding instruction/memory budgets, and scoring custom strategies across the full tape without touching disk or restarting the server.
- **True Server-Sent Events & Daily Report Exports:** `GET /api/tape/stream` streams an initial `event: snapshot` frame plus real-time `event: tape` broadcasts on every collector pass (capped at 64 concurrent subscribers with 15s keepalive heartbeats and clean client fallback), while `GET /api/export/report?format=md|csv` downloads Markdown or RFC-4180 CSV daily reports preserving the verbatim venue Day P&L on Line 1.
- **100,000-Tick / 2,000-Window Scale & Concurrency:** added cross-pass settled snapshot deduplication (`OPENBELL_DEDUP_SETTLED`), adaptive 429 rate-limit pace doubling, optional `OPENBELL_SERIES` filtering, pre-serialized `/api/markets` snapshots, `O(log M)` per-ticker `/api/markets/history`, and `RwLock` + singleflight caching across `tape_cache`, `analysis_cache`, `analytics_cache`, and `backtest_cache` — verified under 30 concurrent clients at `<300ms` p95 with zero new Cargo dependencies and `.loop/checks/fence.sh` `FENCE OK`.
- **Institutional 4-Tab Paper Quant Desk (`frontend/` -> single-file `assets/index.html`):** upgraded the House Frontend workbench (`8/8` libraries, Settings-only Light/Dark theme, opaque sticky header, self-hosted Geist/Geist Mono) with a 4th `Quant Lab` tab (Calibration reliability SVG + Wilson CIs, interactive Monte Carlo fan + seed runner, IS vs OOS Walk-Forward table), inline YES bid/ask & spread history charts with hover crosshairs, live SSE stream status pill, ticking 1s settlement countdowns, interactive Strategy Sandbox editor + "Backtest on Tape", synchronized Equity + Drawdown underwater subplot, `.md`/`.csv` report export buttons, and global `Cmd+K` / `Ctrl+K` Command Palette (`1`–`4`, `/`, `j`/`k`/`Enter`).

## 0.4.0 - 2026-11-21

- Complete Kalshi 15-Minute Quant Research & Paper Compounding Desk (`frontend/` rebuilt from scratch on Cayleb's House Frontend system, all 8 libraries actively wired in `ROLE-COVERAGE.md`, self-hosted Geist 400/500/600 & Geist Mono `.woff2`, calm Square UI nested hairline surfaces, zero fixture badge or switcher during live operation).
- Settings-only Appearance switch: Light default and full Dark second theme live exclusively inside the Desk Settings Vaul sheet (`Appearance` segmented control persisted in `localStorage('openbell-theme')`) alongside collector cadence, self-update checks, and provenance tags — removing theme clutter from the top command bar.
- Hero Telemetry & Compounding Strip: four tabular `Geist Mono` `SlidingNumber` KPI tiles (Paper Compounding Pot with Tremor `ProgressCircle` toward Stage 1 $1,000, Proven Strategies standing tally, Active 15m Markets, Live Tape Snapshots), interactive hover-inspectable Paper Compounding Curve, Tremor `Tracker` of recent settled outcomes, and 5-series crypto coverage `CategoryBar`, wrapped in live `BorderTrail` / `TextShimmer` motion strictly during `POST /api/collect`.
- Three-tab Trader Workbench (`COSS UI Tabs` + `AnimatedBackground` glide + global `/` command filter + `1`/`2`/`3` shortcuts): `Live 15m Tape` (open Kalshi 15m contracts sorted first with live close countdowns, YES/NO implied probability bars, bid/ask/spread, and clickable Vaul Market Inspector backed by real `GET /api/markets/history`), `Rune Strategies` (ranked leaderboard with Tremor `SparkAreaChart` & evidence `Tracker`, null/malformed guards, `j`/`k`/`Enter` navigation, and Vaul Strategy Inspector showing live `.rn` script source from `GET /api/strategy/source` inside a Cult UI `Expandable`), and `Slot Desk` (compound-sized slots, 3-strike `Tracker`, verbatim Daily Report `Expandable`, and read-only Venue Account).
- Live Kalshi collector rate-limit resilience: `fetch_markets` in `src/main.rs` adds bounded exponential backoff retry strictly on Kalshi `too_many_requests` envelopes plus polite inter-request pacing (`OPENBELL_COLLECT_PACE_MS`, default `250ms`), achieving 10/10 live series/status reads (`series_failed_last_pass: 0`) even on hosts running concurrent pollers while keeping `.loop/checks/fence.sh` strictly intact.

## 0.3.0 - 2026-11-21

- House Frontend instrument ledger (`frontend/` -> single-file `assets/index.html` embedded in the binary): warm ivory light default and near-black dark theme, self-hosted Geist and Geist Mono `.woff2` bundled inline with zero external requests, and all eight House Frontend libraries (`Motion Primitives`, `COSS UI`, `Cult UI`, `Kibo UI`, `Tremor`, `Vaul`, `Sonner`, `Motion`) documented in `JOB.md`, `ROLE-COVERAGE.md`, and `ALIVE.md`.
- Standing promotion tally beside the daily report (`Strategies proven on forward evidence`), derived directly from the server's verdict sentences; passes on the example tape are labelled as fixture-only and never counted as forward evidence.
- Strategy inspector drawer (`Vaul` right sheet on desktop, bottom drawer on phones, `250 ms` house enter/dismiss timing) with cumulative paper P&L curve, half-split figures against the 100-window/+3.5¢ bar, and recent paper trades; `/` focuses strategy or market search, `j` and `k` step rows, and `Enter` opens the drawer.
- Malformed-data guards across Strategies, Markets, Slots, and SVG curves so `null` list entries, non-string `verdict` values, or non-numeric fields render safely without uncaught errors or `NaN`.
- Update banner and version row hardened: `Update now` and `Check for updates` appear only on loopback hosts (shared addresses show plain machine instructions), `Dismiss` moves focus to `Show update` in the version row, `Update now` hands focus from banner status to `Restart now`, and failures map to plain guidance with raw server output behind a `Details` disclosure.
- Zero-setup startup and clean lints: platform data directory default, automatic seeding of the five built-in Rune strategies, port fallback across `8472..8492` when `PORT` is unset, display-aware browser launch (`--no-open`, `--service`, `OPENBELL_NO_OPEN=1`), `npm run check:ui` Playwright gate + replay suite, and zero `cargo clippy -- -D warnings` diagnostics.

## 0.2.0 - 2026-11-19

- Page redesign: a purpose line and a "How to read this" line, the example tape grouped apart from the live desk, a calm error per panel, and strategy files that fail to load listed by name.
- Tape fixes: the tape is read as bytes; a line that is not valid UTF-8 or not a snapshot is counted as dropped, never hides its neighbours; a torn last line no longer swallows the next pass; `snapshots_on_disk` counts valid snapshots only, and `lines_on_disk` gives every line in the file.
- Same-site guard on every request (Origin, Host, `Sec-Fetch-Site`, JSON-only bodies); `OPENBELL_ALLOWED_HOSTS` names extra host names allowed to read the desk.
- Collector failures are counted and named: `series_failed_last_pass`, `series_total` and `series_failed_names` on `/api/home`, and the report's Collector line says how many market reads failed.
- Trades and quotes: only whole-cent prices from 0.01 to 0.99 size a trade.
- The example tape's verdict reads "PROVEN on the example tape", never as forward evidence.
- `--version`, `GET /api/version`, and self-update from GitHub releases (`OPENBELL_UPDATE_MODE`).

## 0.1.0 - 2026-11-17

- First Rust build: Kalshi 15-minute collector, shadow scorer for Rune strategies, slot system, daily report and single-page desk.
