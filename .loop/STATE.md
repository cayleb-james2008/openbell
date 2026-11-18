status: running
next: phase 1 — B1+B2 slot system and paper sizing (one change: the simulation core)
phase: setup
last kept commit: b8c6e0a (main, as found; plus as-found commit 8bee2fb for the untracked unit file)
host labels: cargo 1.88.0 at ~/.rust/bin; helpers available (subagents); no browser tooling in this session — UI verified via served HTML/source checks and curl, renders not screenshotted (noted in reports)
live system: openbell-collector.service RUNS the binary at target/debug/openbell-rs, collecting into openbell-data/ via env in /etc/openbell-collector.env (never printed, never copied). Previous binary backed up at .loop/tmp/openbell-rs.prev-binary. After any rebuild: systemctl restart openbell-collector.service and verify tape growth within ~2 minutes. Never delete or truncate openbell-data/.
conditions: none judged yet
sealed checks hash: none yet
kept changes since last review: 0
checkpoint: none set by owner
last report commit: none

## Owner brief

Approved goal (owner's words, do not drift): "OPENBELL: Cayleb's Kalshi
compounding desk — the slot system compounding his pot toward its targets,
venue truth on every number."

Scale-up order (17 Nov 2026, ~19:14 ET): the Rust + Rune baselines are to
be SUPER-SCALED, aligned with the approved goal, using (a) an /improve
pass, (b) a thermonuclear code review, and (c) the ultra loop harness —
until ready to COMPLETELY REPLACE the old projects publicly (résumé site +
GitHub).

Scale-up for OpenBell means: the slot system expressed fully in the shadow
scorer (per-slot state, strikes, equity-split sizing on paper), a richer
strategy library in Rune, the daily report a user would actually read,
charts over the tape (equity curve of shadow strategies), and the
collector's health visible on the home screen. Familiar shell, light +
dark themes with the toggle in settings (dark default), icons are symbols
never letters, honest unavailable states.

HARD FENCES (money law, absolute): no order-placement code path may be
introduced — none, in any form. No live trading, no arming. Kalshi is the
only source for any number shown; unavailable stays "unavailable".
Original repos untouched; nothing pushed or published in this session.

## Needs from you

- N1: none yet.
