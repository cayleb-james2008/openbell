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
  shrinks them. Only a price the venue could actually trade sizes a trade —
  Kalshi trades whole cents, so a sub-cent quote is treated as no market,
  never as a five-figure paper win.
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

## Status

The forward evidence is young: the tape has been collecting since
17 November 2026, so no strategy in the library has the forward record the
promotion standard asks for, and this page makes no performance claim. The
slot system is a paper scorer. Whether any strategy ever earns promotion is
a question the tape will answer over time; the desk's job until then is to
keep collecting and keep the numbers honest.

## Running it

```sh
cargo run            # serves the desk on $PORT (default 8472)
```

Environment: `OPENBELL_DATA` (tape directory), `OPENBELL_STRATEGIES`
(strategy library directory), `OPENBELL_COLLECT_SECS` (collector interval),
`OPENBELL_PAPER_POT` (paper pot for the slot simulation, default $100 —
always labelled a simulation parameter, never venue money).

## Checks

`bash .loop/verify` builds the binary, runs the Rust tests, and runs the
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
- `assets/index.html` — the desk's single page: account, collector health,
  the paper equity chart, and the daily report. Light and dark themes, dark
  by default, the switch in the settings footer.
