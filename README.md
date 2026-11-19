# OpenBell

A compounding desk for Kalshi's 15-minute markets, rebuilt in Rust. OpenBell
collects the venue's public market data around the clock, scores a library of
strategies against that tape on paper, and writes a daily report whose first
line is the venue's own word on the day — or an honest "unavailable".

It places no orders. There is no order-placement code path in this build, in
any form, and nothing in it can arm or trade live. That is a design rule, not
a missing feature: a mechanical scan of the source and the compiled binary
runs in the check kit on every change.

## The slot system

The desk's strategies run as **slots** — one slot per strategy in the library,
each scored against the same tape:

- **Sizing is an equity split.** At the start of each settled window, the
  current total paper equity is split evenly across the slots, and each slot
  stakes its share at its strategy's entry price, fees included. The pot
  compounds: a winning window grows the next window's stakes, a losing one
  shrinks them. Only a whole-cent price between 0.01 and 0.99 sizes a trade.
  Kalshi trades whole cents, so any other quote (0.4567, or a sub-cent 0.004)
  is treated as no market, never as a five-figure paper win.
- **Strikes are consecutive losses.** Three in a row retire the strategy from
  its slot, and the next library strategy that is not already running takes
  its place. If every other strategy is already running, the next in order
  takes the slot and the duplication is disclosed — a strategy running in two
  slots stakes two shares, and the report says so.
- **Attribution is per stint.** A slot's line separates the current strategy's
  own record (windows and P&L since it took the slot) from the slot's
  cumulative total, and every retirement records the strategy it actually
  rotated to. Nothing is credited with a predecessor's results.
- **The venue's record has to agree with itself.** Windows are ordered by
  the settled record's own close time. A window whose records carry
  conflicting settlements is not scored at all — the disagreement is
  counted on the home screen and in the simulation's notes, never
  silently resolved in anyone's favour.

Strategies are small [Rune](https://rune-rs.github.io/) scripts in
`strategies/` — the library ships five shapes (favourite late, favourite
floor, longshot late, mid-window momentum, no-favourite). Editing one file
changes only that strategy's results; the check kit proves it. The contract
is enforced, not assumed: a strategy whose `side()` returns anything but
"yes" or "no", a duplicate strategy name, or a file past the size cap is
rejected and named on every surface; a strategy whose `allow_entry` errors
or returns a non-boolean is counted and named, never read as a quiet one;
and a strategy that never returns degrades to no-entry under an aggregate
time budget, with the degradation stated in the simulation's notes.

## Same-site guard

Every request must name a local Host: `localhost`, `127.0.0.1` or `[::1]`, or
a host name listed in `OPENBELL_ALLOWED_HOSTS` (comma-separated, no port).
Other Hosts are refused with 403. A state-changing request (any method but
GET or HEAD) is refused with 403 when the browser says it is cross-site, or
when its Origin names a different authority than its Host. A request with a
body must be `application/json` (415). Refusals are plain JSON errors, and
reads stay open to same-host clients.

## Venue truth, or "unavailable"

Every number on the home screen and in the daily report is one of three
kinds, and it is labelled as such: **venue** (read from Kalshi), **tape**
(counted from the collected snapshots), or **paper** (the slot simulation).
Anything the desk has not actually obtained is shown as **unavailable** —
never as zero, never estimated. Signed venue reads are not enabled in this
build, so the account figures read "unavailable" and the report's first line
says why.

## The collector

A background pass collects Kalshi's **public** market data — the only
outbound call in the program is an unauthenticated GET of public markets —
for five 15-minute series (BTC, ETH, SOL, XRP, DOGE) and appends each pass to
a local tape in a single write, so a reader never sees a torn half-pass. The
home screen shows the collector's health next to everything else: tape
lines on disk, distinct markets seen, distinct settled windows, tape lines
that failed to parse (counted as dropped, never silently skipped), windows
whose settlements conflict, and the age of the newest snapshot, with a
plain "collecting / NOT collecting" state — because an uncollected window
can never be backfilled. Freshness is read from the tape's last line, so a
single future-dated record cannot make a stopped collector read as alive.
A collect pass that fails, or writes nothing, is reported as a failure.

The tape is read as bytes. `snapshots_on_disk` counts valid snapshots only;
a line that is not valid UTF-8 or not a snapshot is counted in
`dropped_lines` and never hides its neighbours. `lines_on_disk` is every
line in the tape file: `snapshots_on_disk + dropped_lines`. A torn last line (no final
newline, left by a crash mid-append) is kept as its own dropped line: the
next pass starts on a fresh line, so its snapshots are not lost.

When reads fail on a pass, `/api/home` carries `series_failed_last_pass`
(failed reads on the last pass), `series_total` (10: open and settled reads
for five series) and `series_failed_names` (the series behind them). The
report's Collector line then reads, for example, `4 of 10 market reads failed
on the last pass (5 series, open and settled): KXBTC15M, KXSOL15M.`

## The daily report

One short page, written to be read:

1. The venue day P&L, signed — or the honest unavailable line, with the real
   reason.
2. An as-of line naming the moment and the basis: everything below is
   cumulative over the collected tape.
3. Balance and open positions, each venue-labelled or unavailable.
4. Collector state and counts, checked against the tape itself.
5. Per-slot lines: current strategy, strikes, the stint's record and the
   slot's total, each retirement with its successor and its own P&L.
6. The paper pot, labelled paper, with the starting pot beside it.
7. Per-strategy verdicts against a stated promotion standard: a strategy is
   a candidate only after 100+ forward windows, both chronological halves
   positive, at +3.5¢ per contract or better after the stated fee model.
   Until then the verdict is "insufficient evidence" — which, on a young
   tape, is most of them, and the report says so.

## The example tape

`dataset=fixture` reads a labelled example tape written for tests. The page
groups its figures apart from the live desk under an "example tape" tag, and
its verdict reads `PROVEN on the example tape (not forward evidence, paper
only - no orders placed)`. The live desk's verdict stays `PROVEN on forward
evidence (paper only — no orders placed)`. The daily report always reads the
live desk.

## Status

The forward evidence is young: the tape has been collecting since
17 November 2026, so no strategy in the library has the forward record the
promotion standard asks for, and this page makes no performance claim. The
slot system is a paper scorer. Whether any strategy ever earns promotion is
a question the tape will answer over time; the desk's job until then is to
keep collecting and keep the numbers honest.

## Versions and updates

The running version is shown on the page, in `GET /api/version` and `GET /api/home`, and by `openbell-rs --version`. Every release is tagged `vX.Y.Z` on GitHub, with a changelog entry in `CHANGELOG.md`, binaries for each platform and a `SHA256SUMS` file; pushing a tag runs `.github/workflows/release.yml`, which refuses a tag that does not match the version in `Cargo.toml`.

Self-update (`src/update.rs`): the desk asks the newest GitHub release of its own repository whether a newer version exists (shortly after start and every 6 hours). `GET /api/update` says what it found; `POST /api/update/check` asks now. `POST /api/update/apply` downloads the binary for this platform, checks its SHA-256 against the release's `SHA256SUMS`, runs it with `--version` to confirm it is the build it claims to be, and only then swaps it in. `POST /api/update/restart` then starts the new build. Any failed step leaves the installed program untouched. Downloads go through `curl`, and the only URLs fetched are https (plain http only for a loopback address).

Updating is refused for a cargo-built binary, from any Host other than a loopback name (`OPENBELL_ALLOWED_HOSTS` does not permit it), and a restart is refused with 409 while a collection pass is running or when no update is waiting.

| Var | Default |
| --- | --- |
| `OPENBELL_UPDATE_MODE` | `notify` (check and tell); `auto` also downloads and installs (a restart is still yours to press); `off` never contacts GitHub |
| `OPENBELL_UPDATE_REPO` | `cayleb-james2008/openbell` |
| `OPENBELL_UPDATE_API` | `https://api.github.com` (plain http only for a loopback address) |
| `OPENBELL_UPDATE_DEV` | `1` allows replacing a cargo-built binary (tests only) |

## Running it

```sh
cargo run            # serves the desk on $PORT (default 8472)
```

Environment: `OPENBELL_DATA` (tape directory), `OPENBELL_STRATEGIES`
(strategy library directory), `OPENBELL_COLLECT_SECS` (collector interval),
`OPENBELL_PAPER_POT` (paper pot for the slot simulation, default $100 —
always labelled a simulation parameter, never venue money),
`OPENBELL_ALLOWED_HOSTS` (extra host names allowed past the same-site guard,
see above), `PORT` (default 8472).

## Checks

`cargo test` runs the unit tests and the `--version` integration test. `bash .loop/verify` builds the binary, runs
the Rust tests, and runs the
check kit: the order-path fence, slot rotation and reconciliation against
independent recomputation, collector health counted both ways, the equity
curve reconciled against the slot report, the daily report's honesty
assertions, the strategy-library mutation isolation, and both themes. The
kit is the bar: it is expected to pass end to end, and it is re-run after
every change.

## Layout

- `src/main.rs` — the whole server: collector, shadow scorer, slot system,
  report, and the HTTP API.
- `strategies/*.rn` — the strategy library (Rune).
- `assets/index.html` — the desk's single page: a purpose line and a "How to
  read this" line, account, collector health, the paper equity chart, the
  strategies, and the daily report. Each panel shows its own error. Light and
  dark themes, dark by default, the switch in the settings footer.
