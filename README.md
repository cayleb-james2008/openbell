# OpenBell

![version](https://img.shields.io/badge/version-v0.36.0-0969da?style=flat-square)
![stack](https://img.shields.io/badge/stack-Rust%20%2B%20React%2019-111827?style=flat-square)
![tests](https://img.shields.io/badge/tests-194%2F194%20pass-16a34a?style=flat-square)
![binary](https://img.shields.io/badge/release%20binary-6.61%20MiB-0969da?style=flat-square)
![safety](https://img.shields.io/badge/live__armed-false%20on%20boot-16a34a?style=flat-square)

OpenBell is a local Rust trading workbench for 15-minute Kalshi prediction markets. It includes a Kalshi Trade API v2 client (`live_armed: false` on every boot, seven hard risk rails, fail-closed `0600` runtime persistence, resting order amendment and 1-click position flattening, an authenticated read-only private WebSocket execution stream with 60-second REST reconciler backstop and SSE `event: live_exec` fan-out, closed-loop fill and settlement reconciliation, an emergency kill switch, and explicit kill latch reset), a live WebSocket and REST L2 orderbook stream (`OrderbookStore` auto-subscription up to 32 active markets, trade channel ingestion, bounded snapshot history, book-walk VWAP sizing, and DOM depth ladder), a real-time external crypto spot oracle (`SpotFeedStore` tracking `BTC`, `ETH`, `SOL`, `XRP`, and `DOGE`, offline by default, feeding rolling delta and volatility plus Kalshi strike threshold divergence into `TickContext` and interactive chart overlays), a tick-driven shadow orderbook runner (active quote repricing with cooldown and per-window replace caps, take-profit, stop-loss, and pre-close exit routing, slot inventory injection into `TickContext`, direct L2 touch/cross/trade matching, idempotent settlement logging, fail-closed stale-sync gating, persistent slot pause guards, and restart clamping), an adverse-selection markout telemetry and auto-quarantine feedback loop (`execution_audit` + `AutoAllocator` runner sync), a causal Strategy Runtime v2 with confined script library CRUD and a walk-forward BH-FDR edge synthesizer, a deduplicating public 15-minute tape collector, and a post-only maker allocator.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/hero-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/hero-light.png">
  <img alt="OpenBell v0.36.0 Desktop Quant Workbench" src="docs/screenshots/hero-light.png">
</picture>

## Feature gallery

<table>
  <tr>
    <td width="50%" valign="top">
      <strong>Causal Edge Quality Screener and Dry-Run Order Plan</strong><br>
      <sub>Ranks strategies and 15-minute Kalshi series by maker net expectancy, fee savings, break-even queue fill headroom, walk-forward fold consistency, and <code>$500/day</code> contribution share alongside per-leg whole-cent fee, spread-crossing, and minimum-fill dry-run checks.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/screener-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/screener-light.png">
        <img alt="Causal Edge Quality Screener and Dry-Run Order Plan Preview" src="docs/screenshots/screener-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <strong>Dry-Run Order Plan, 1x/2x/5x Liquidity Capacity, and Preset Leaderboard</strong><br>
      <sub>Audits exact Kalshi whole-cent maker vs taker fee drag and minimum contract thresholds, tests <code>1x</code>, <code>2x</code>, and <code>5x</code> orderbook partial-fill impact (<code>LIQUID</code>, <code>TIGHT</code>, or <code>CROWDED</code> and <code>max_scalable_dollars</code>), and checks every preset against a <code>0.0%</code> daily rail-breach gate.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/stress-test-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/stress-test-light.png">
        <img alt="Dry-Run Order Plan Preview and Causal Liquidity Capacity Check" src="docs/screenshots/stress-test-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Tick-Driven Shadow Orderbook Runner and Fail-Closed Live Auto Gate (<code>v0.25.0</code>)</strong><br>
      <sub>Runs continuous multi-slot paper shadow execution against live 15-minute orderbooks with <code>entry_quote()</code> resting quotes, queue-ahead depletion tracking, persistent <code>shadow_ledger.jsonl</code> settlement grading, drawdown pause guards, and fail-closed Live Auto gating (<code>403</code> until explicitly armed).</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/shadow-runner-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/shadow-runner-light.png">
        <img alt="Shadow and Auto-Live Runner Control Deck" src="docs/screenshots/shadow-runner-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Live L2 Orderbook and VWAP Depth Ladder (<code>v0.26.0</code>)</strong><br>
      <sub>Streams Kalshi Trade API v2 L2 snapshots and deltas with exponential-backoff REST fallback, walks bid and ask levels to price order VWAP and slippage in <code>ShadowRunner</code> and the post-only allocator, and embeds clickable depth ladders in the Tape Market Inspector and Live Order Ticket.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/orderbook-ladder-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/orderbook-ladder-light.png">
        <img alt="OpenBell v0.26.0 L2 Orderbook Depth Ladder and VWAP Impact Preview" src="docs/screenshots/orderbook-ladder-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Strategy Runtime v2 CRUD and Walk-Forward Edge Synthesizer (<code>v0.27.0</code>)</strong><br>
      <sub>Runs chronological 60% in-sample discovery and 40% held-out out-of-sample screening across parameterized <code>evaluate(ctx)</code> strategy families with Benjamini-Hochberg FDR control, Deflated Sharpe filtering, library correlation caps, 1-bar causal canary checks, and atomic confined library saves and deletes.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/synthesizer-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/synthesizer-light.png">
        <img alt="OpenBell v0.27.0 Walk-Forward Edge Synthesizer and Strategy Sandbox CRUD" src="docs/screenshots/synthesizer-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Interactive Strategy Sandbox v2 and Persistent Library CRUD (<code>v0.27.0</code>)</strong><br>
      <sub>Provides Save, Save As, Clone, and Delete controls backed by CWE-22/CWE-59 confined endpoints alongside a clickable 20-field causal <code>TickContext</code> strip and side-by-side paper tape backtest equity curve.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/sandbox-crud-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/sandbox-crud-light.png">
        <img alt="OpenBell v0.27.0 Interactive Strategy Sandbox CRUD and TickContext" src="docs/screenshots/sandbox-crud-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Hardened Runner, L2 Orderbook Recovery, and Fail-Closed Live Rails (<code>v0.28.0</code>)</strong><br>
      <sub>Clamps tampered live-auto state on restart with <code>live_armed: false</code>, enforces idempotent fill/cancel accounting and WebSocket frame bounds, surfaces inline <code>403</code>/<code>409</code>/<code>415</code>/<code>400</code> recovery banners, and detects crossed or stale L2 orderbook feeds.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/hardening-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/hardening-light.png">
        <img alt="OpenBell v0.28.0 Hardened Shadow Runner, L2 Orderbook Ladder, and Risk Rails" src="docs/screenshots/hardening-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>L2 Auto-Subscription Telemetry and Snapshot History Scrubber (<code>v0.29.0</code>)</strong><br>
      <sub>Automatically subscribes active open 15-minute markets to L2 depth and trade streams (up to 32 tickers), decouples <code>ShadowRunner</code> resting-quote fills and expiry checks from cursor position, and renders a live L2 telemetry strip plus a historical L2 depth scrubber.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/l2-autosub-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/l2-autosub-light.png">
        <img alt="OpenBell v0.29.0 L2 Auto-Subscription Telemetry and Snapshot History Scrubber" src="docs/screenshots/l2-autosub-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Venue Reconciliation and Persistent Risk Rails (<code>v0.30.0</code>)</strong><br>
      <sub>Reconciles Kalshi v2 fills, settlements, and open exposure into an idempotent <code>live_executions.jsonl</code> ledger, persists UTC-day realized loss and kill latches in <code>risk_rails_runtime.json</code> across restarts, blocks stale live auto-routing fail-closed, and adds explicit kill latch unlatching while keeping <code>live_armed: false</code>.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/reconcile-rails-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/reconcile-rails-light.png">
        <img alt="OpenBell v0.30.0 Venue Reconciliation and Persistent Risk Rails" src="docs/screenshots/reconcile-rails-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Execution Health, Adverse-Selection Markout, and Auto-Quarantine (<code>v0.31.0</code>)</strong><br>
      <sub>Tracks post-fill <code>+10s/+30s/+60s</code> and settlement markout curves, predicted vs realized win and fill residuals, persistent <code>0600</code> <code>quarantine_state.json</code> decay throttling, persisted slot <code>pause_reason</code> state across restarts, and 1-click <code>Sync to Runner</code> from <code>AutoAllocatorPanel</code> while keeping <code>live_armed: false</code> and preventing <code>live_auto</code> on sync.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/markout-quarantine-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/markout-quarantine-light.png">
        <img alt="OpenBell v0.31.0 Execution Health, Markout and Auto-Quarantine" src="docs/screenshots/markout-quarantine-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Cycle 3 Fail-Closed Hardening and Corrupt-File Recovery (<code>v0.32.0</code>)</strong><br>
      <sub>Quarantines corrupt <code>risk_rails_runtime.json</code> files with <code>killed: true</code> and <code>live_armed: false</code>, recovers truncated or non-UTF-8 lines across <code>orderbooks.jsonl</code> and <code>live_executions.jsonl</code>, caps venue pagination cursors at 50 pages, normalizes <code>NaN</code>/<code>Infinity</code> markout values to clean <code>N/A</code> UI fallbacks, aborts stale L2 history fetches, and locks submission buttons against double clicks.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/hardening-c3-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/hardening-c3-light.png">
        <img alt="OpenBell v0.32.0 Cycle 3 Fail-Closed Hardening" src="docs/screenshots/hardening-c3-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Active Quote Lifecycle, Order Amendment, and Intra-Window Exits (<code>v0.33.0</code>)</strong><br>
      <sub>Adds resting order amendment (<code>POST /api/live/orders/{id}/amend</code>) and 1-click position flattening (<code>POST /api/live/positions/{ticker}/flatten</code>) behind fail-closed <code>live_armed</code>, stale-sync, and kill-switch checks, injects live per-slot inventory into the 24-field causal <code>TickContext</code>, and equips <code>ShadowRunner</code> slots with cooldown-paced quote repricing, take-profit, stop-loss, and pre-close exit routing plus inline Amend, Flatten, and lifecycle badges.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/lifecycle-c4e1-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/lifecycle-c4e1-light.png">
        <img alt="OpenBell v0.33.0 Active Quote Lifecycle, Order Amend, and Position Flatten" src="docs/screenshots/lifecycle-c4e1-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>External Crypto Spot Oracle and Strike Divergence Overlay (<code>v0.34.0</code>)</strong><br>
      <sub>Tracks public unauthenticated spot ticks for <code>BTC</code>, <code>ETH</code>, <code>SOL</code>, <code>XRP</code>, and <code>DOGE</code> in <code>SpotFeedStore</code> (offline by default via <code>OPENBELL_SPOT_OFFLINE=1</code>), persists ticks with 8 MiB rotation and CWE-59 symlink checks, streams throttled SSE <code>event: spot</code> updates, parses Kalshi <code>floor_strike</code>, <code>cap_strike</code>, and <code>spot_price</code> snapshots, extends causal <code>TickContext</code> to 29 fields, and renders the Tape Spot Oracle strip alongside the right-hand USD spot and strike threshold overlay.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/spot-oracle-c4e2-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/spot-oracle-c4e2-light.png">
        <img alt="OpenBell v0.34.0 External Crypto Spot Oracle and Strike Price Overlay" src="docs/screenshots/spot-oracle-c4e2-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Private Venue WebSocket Execution Stream and Live SSE Reconciliation (<code>v0.35.0</code>)</strong><br>
      <sub>Maintains a read-only authenticated RFC 6455 Kalshi private WebSocket stream across <code>fill</code>, <code>market_positions</code>, <code>order_group_updates</code>, and <code>user_orders</code> (offline by default when unconfigured or when <code>OPENBELL_PRIVATE_WS_OFFLINE=1</code>), reconciles fills and exposure idempotently into <code>live_executions.jsonl</code> and <code>ShadowRunner</code>, broadcasts <code>event: live_exec</code> SSE updates to Live &amp; Rails tables, scales the REST reconciler to a 60-second backstop while connected with immediate fallback on disconnect or sequence gap, and shows live <code>WS Stream</code>, <code>REST Poll</code>, and <code>Offline</code> telemetry in the account strip and reconciliation panel.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/private-ws-c4e3-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/private-ws-c4e3-light.png">
        <img alt="OpenBell v0.35.0 Private Venue WebSocket Execution Stream and Venue Reconciliation" src="docs/screenshots/private-ws-c4e3-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <strong>Cycle 4 Wave H1 Hardening Across Live Desk, Spot Oracle, and Private WS (<code>v0.36.0</code>)</strong><br>
      <sub>Hardens Cycle 4 E1 through E3 surfaces with zero new features: RFC 6455 fragmented frame, interleaved ping/pong/close, and jittered reconnect bounds in <code>private_ws.rs</code>, warm spot-cache recovery, outlier/clock-skew checks, and symlink guards in <code>spot_feed.rs</code>, fail-closed tampered risk-rail recovery and in-flight <code>409 Conflict</code> amend/flatten locks in <code>risk_rails.rs</code> and <code>main.rs</code>, plus exponential-backoff SSE recovery, monotonic frame guards, inline error alerts, and responsive <code>390x844</code> wrapping in the live desk and runner UI.</sub>
      <br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/hardening-c4h1-1440-dark.png">
        <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/hardening-c4h1-1440-light.png">
        <img alt="OpenBell v0.36.0 Cycle 4 Wave H1 Hardening" src="docs/screenshots/hardening-c4h1-1440-light.png">
      </picture>
    </td>
  </tr>
</table>

## Causal execution and fail-closed risk pipeline

```mermaid
flowchart LR
  T["15m Public Tape (snapshots.jsonl)"] --> S["Strategy Runtime v1/v2 & Causal OOS Split (60% IS / 40% OOS)"]
  S --> Y["Walk-Forward Synthesizer (BH-FDR + Deflated Sharpe + 1-bar Canary)"]
  Y --> Q["PAV Isotonic Recalibration & Tournament (|r| <= 0.65)"]
  Q --> A["Fractional-Kelly Post-Only Auto-Allocator + Slippage Ladder (+0c..+3c)"]
  A --> B["1,000-Trial 30d Bootstrap Stress (+0c / +1c)"]
  B --> P["Dry-Run Order Plan + 1x/2x/5x Liquidity Capacity (live_armed = false)"]
  P --> R{"7 Hard Risk Rails + X-OpenBell-CSRF + Loopback-Only"}
  R -- "Fail Closed (403)" --> D["Disarmed Paper Mode / Emergency Kill Switch"]
  R -- "confirm_arm = true" --> K["Kalshi Trade API v2 (RSA-PSS SHA-256)"]
```

## Verified baseline (`v0.36.0`)

| Metric | `v0.10.0` Baseline | `v0.36.0` Verified | Delta |
| --- | --- | --- | --- |
| Release binary size (`strip`, `lto = "fat"`) | `10.66 MB` (`11,179,360 B`) | **`6.61 MiB`** (`6,935,912 B`) | **`-38.0%`** |
| Initial `GET /` shell (raw / gzip) | `1,152,017 B` / `423,382 B` | **`101,964 B`** (`100 kB`) / **`23,478 B`** (`23 kB`, `25` chunks) | **`-94.5%` wire bytes** |
| Cold boot to first `HTTP 200` | `9.11 ms` | **`4.19 ms`** | **`-54.0%`** |
| Peak RSS (`VmHWM`) | `125.55 MB` (`128,560 KB`) | **`25.25 MB`** (`25,856 KB`) | **`-79.9%`** |
| Concurrent HTTP throughput (`16w`, `5/5` SSE) | `4,921.3 req/s` | **`7,194.0 req/s`** (`3.04 ms` p95, `0/17,996` errors) | **`+46.2%`** |
| Security and test verification | `49/49` fuzz | **`194/194` Rust tests**, `25/25` raw-TCP fuzz, `gate 147/147`, `replay 18/18`, `sloplint 0/0/0`, `RAILS OK`, `EQUITY OK` | Clean |

## Quick start (synthetic paper mode)

```sh
# 1. Build frontend assets and release binary
cd frontend && npm run build && cd ..
cargo build --release --locked

# 2. Launch with synthetic/local paper data (always boots live_armed: false)
OPENBELL_DATA=./frontend/e2e/fixtures/data OPENBELL_COLLECT_SECS=3600 PORT=18440 ./target/release/openbell-rs
```

Open `http://127.0.0.1:18440/?dataset=fixture` to inspect the 15-minute fixture tape, causal edge screener, slippage sensitivity ladder, walk-forward edge synthesizer, and 30-day bootstrap simulator without live credentials or external orders.

## Architecture and guardrails

### Fail-closed hard risk rails and live Kalshi v2 (`src/risk_rails.rs`, `src/kalshi_live.rs`)

Every launch and restart boots with `live_armed: false`. Routing an order requires configured Kalshi v2 API credentials stored with `0600` permissions inside `OPENBELL_DATA`, explicit `confirm_arm: true` over loopback with `X-OpenBell-CSRF: 1`, and passing all seven hard risk rails (`max_order_contracts`, `max_order_dollars`, `max_daily_loss_dollars`, `max_open_exposure_dollars`, `allowed_ticker_prefix`, whole-cent `1..=99` price check, and kill-switch clear). Risk runtime state (`killed`, `kill_reason`, UTC-day realized loss, open exposure, and `last_venue_sync_epoch`) persists atomically with `0600` permissions in `risk_rails_runtime.json`, while `GET/POST /api/live/reconcile` syncs fills and settlements into `live_executions.jsonl`. Posting to `POST /api/live/kill-switch` disarms `live_armed`, latches `killed: true`, and runs `cancel_all_orders()`, and clearing the latch requires an explicit `POST /api/live/reset-kill` (`confirm_reset_kill: true`), which leaves `live_armed: false`.

HTTP requests enforce Host-header loopback confinement (`403` on non-loopback hosts), `Content-Type: application/json` (`415`), prompt raw-TCP `413 Payload Too Large` responses before allocation, `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, strong SHA-256 `ETag` `304` handling, and CWE-22/CWE-59 symlink and path confinement on strategy library mutations (`POST /api/strategies/save`, `DELETE /api/strategies/{name}`, `POST /api/research/synthesize`).

### Causal paper analytics, v2 scripts, and slot simulation

The strategy runtime (`src/strategy_v2.rs`) supports both v1 `allow_entry(...)` scripts and v2 `evaluate(ctx)` scripts reading a 24-field strictly causal `TickContext` (`ticks[..=t]` plus in-sample-only PAV posterior probabilities, expected values, and live per-slot inventory fields), enforced by a 1-bar causal lookahead canary check before saving to disk. The walk-forward synthesizer (`src/synthesizer.rs`) searches `momentum_microstructure`, `mean_reversion_pav`, and `maker_imbalance` parameter grids on the chronological 60% in-sample window slice, evaluates held-out 40% out-of-sample returns with Kalshi maker/taker fees, and screens candidates with Benjamini-Hochberg FDR control, Deflated Sharpe Ratio, and library return-correlation caps.

The auto-allocator (`src/auto_allocator.rs`) splits settled windows chronologically into 60% in-sample and 40% out-of-sample periods, freezes replay weights on in-sample data when in-sample history exists, enforces a correlated regime drawdown ceiling across multi-asset and same-asset strategy clusters bounded by `max_daily_loss_dollars`, and models Kalshi maker fees (`0.0175 * p * (1 - p)`, one quarter of taker `0.07 * p * (1 - p)`) rounded up to whole cents per order (`ceil(rate * C * p * (1 - p) * 100) / 100`). Across `strategies/*.rn`, it runs the 16 built-in paper strategies (`bid_ask_imbalance`, `favourite_floor`, `favourite_late`, `late_convergence_cap`, `longshot_late`, `maker_late_theta_no`, `maker_spread_capture`, `mean_reversion_fade`, `mid_window_momentum`, `no_favourite`, `opening_range_break`, `spot_lead_momentum_yes`, `spread_compression`, `tail_overreaction_fade`, `time_decay_harvest`, and `volatility_squeeze`), partitions epochs into Calm and High-Vol regimes causally, and computes the `+0c` through `+3c` slippage ladder with break-even cushion status.

Static assets (`assets/index.html.gz` and embedded `/chunks/*` routes) stay gzip-compressed in `.rodata` and inflate via pure-Rust RFC 1951/1952 `inflate_gzip` (`src/gzip_inflate.rs`) for `identity` HTTP clients, while `TapePoolGuard`, zero-capacity-trimmed market JSON, and `mallopt` bounds keep peak RSS low.

### Verification suite

- `cargo clippy --all-targets -- -D warnings`
- `cargo test -- --test-threads=1`
- `cargo check --target x86_64-pc-windows-msvc`
- `bash .loop/checks/rails.sh && bash .loop/checks/equity.sh`
- `cd frontend && npm run check:ui -- --skip-build --lints`

### Environment variables

| Variable | Default |
| --- | --- |
| `OPENBELL_DATA` | `$XDG_DATA_HOME/openbell` or `./openbell-data` when present |
| `OPENBELL_STRATEGIES` | `strategies/` inside `OPENBELL_DATA` (or `./strategies`) |
| `OPENBELL_COLLECT_SECS` | `60` (collector freshness interval, `5..=3600`s) |
| `OPENBELL_AUTO_COLLECT` | `0` (`1` or `--auto-collect` starts the public 15m collector) |
| `OPENBELL_SERIES` | `KXBTC15M,KXETH15M,KXSOL15M,KXXRP15M,KXDOGE15M` |
| `OPENBELL_PAPER_POT` | `100` (paper simulation starting pot) |
| `OPENBELL_ALLOWED_HOSTS` | none (comma-separated extra hosts allowed past loopback guard) |
| `PORT` | `8472` (falls back across `8472..=8492` unless explicit `PORT` is set) |
| `OPENBELL_NO_OPEN` | unset (`1` or `--no-open` suppresses auto-opening browser) |
