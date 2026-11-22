# Changelog

Each release is tagged `vMAJOR.MINOR.PATCH` and published on GitHub Releases with
binaries and a `SHA256SUMS` file. The running desk shows its version and can update
itself from the newest release (see README, "Versions and updates").

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
