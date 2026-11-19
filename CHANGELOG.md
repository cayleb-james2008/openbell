# Changelog

Each release is tagged `vMAJOR.MINOR.PATCH` and published on GitHub Releases with
binaries and a `SHA256SUMS` file. The running desk shows its version and can update
itself from the newest release (see README, "Versions and updates").

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
