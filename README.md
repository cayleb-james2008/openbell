# openbell-rs

**Goal: Cayleb's Kalshi compounding desk — the slot system compounding his
pot toward its targets, venue truth on every number.**

A fresh Rust + Rune rebuild, apart from the original OpenBell. This v1 is
the measuring machine the compounding stands on:

- **Venue-truth home screen** — balance, open positions, day P&L come from
  the venue record or are shown as **unavailable**. Nothing is fabricated.
  Account reads are read-only and only attempted when a credential is
  already present in the environment (`KALSHI_API_KEY_ID` /
  `KALSHI_PRIVATE_KEY_PATH`); this build never hunts for one.
- **Forward collector** — polls Kalshi's *public* market data (no auth) for
  the 15-minute crypto markets the desk trades and records snapshots to
  disk (`snapshots.jsonl`).
- **Shadow scorer** — slot shapes are Rune scripts in
  [`strategies/`](strategies/); they are scored on paper against collected
  forward data. The desk's **promotion standard is code**: a strategy is
  "proven" only with ≥100 forward windows traded and *both* chronological
  halves positive at ≥ +3.5¢ per contract after fees.

## Hard fence

This build **never places, arms, or simulates placing a live order**. There
is no order-placement code path in v1 at all. Kalshi is the only source for
any number shown.

## Run

```sh
cargo run            # http://127.0.0.1:8472, collector runs on an interval
OPENBELL_COLLECT_SECS=60 PORT=8472 cargo run
```

## Proof

```sh
cargo test
./proof.sh   # real public snapshots collected; Rune strategies scored over
             # a labelled fixture tape at the real 100-window standard
```
