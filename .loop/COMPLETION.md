# Completion gap list — openbell-rs (18 Nov 2026)

Cayleb's order: complete OpenBell to its full objective inside its fences.
The objective (approved 17 Nov): Cayleb's Kalshi compounding desk — the
slot system compounding his pot toward its targets, venue truth on every
number. This build is the desk's measuring machine: forward collector,
paper slot system over the tape, daily report. The fences are law: no
order-placement code path exists or may be introduced; Kalshi is the
only source for desk facts; the running collector is never disturbed
(all work here happens in this worktree with its own data dir).

Baseline: HEAD 25c6680 (verify[final] pass=12 fail=0 at ed55e5c).
Status 18 Nov: G1–G3 built, verify extended to 15 checks (C13–C15);
G4 remains, sequenced last by the owner's order.
Live tape at completion start: 223,527 lines / 59 MB and growing.

## Gaps

### G1 — Strategy evaluation has no memory bound (known item a) — DONE
Every strategy call now runs under rune::alloc::limit (8 MiB per
call) composed with the instruction budget; Vm reuse and the 3s→30s
deadline recalibration documented in code. Proven by unit test +
.loop/checks/memory.sh (C13): hostile strategy trades 0 with its
failures counted, honest neighbour unharmed, peak RSS 29 MB.
The instruction budget (250k) and the 3s consumed-time deadline bound
*steps* and *time*, not *bytes*: a Rune strategy can allocate ~600 MB
inside ~24 instructions (one big array/repeat allocation), because
Rune budgets instructions, not memory. On the desk's own machine that
is an availability hole in every scoring surface — and the collector
shares the process, so a hostile or merely careless strategy file can
take the tape down with it.
Fix: wrap every strategy call in rune::alloc::limit (a hard byte cap
per call, composed with the existing instruction budget). A breach
must degrade exactly like any other failed call — counted, noted,
treated as no-entry — never abort or wedge the process.
Proof: a hostile fixture strategy that attempts the ~600 MB
allocation is scored over a fixture tape; the surfaces answer, the
strategy is treated as no-entry with the failure counted, and the
process peak RSS stays bounded.

### G2 — Report/scoring latency on the real tape (known item b) — DONE
Shipped: per-window entry index + scan-resume (prefix) cache,
window-facts cache, tape-entry with seam-verified incremental
extension and folded health aggregates, app-level analysis cache,
shared per-window fingerprints across sim + scores. Measured on the
full-size live-tape copy (~240k lines): cold ~13–25 s once, warm
~0.001 s, post-append recompute ~1 s (was: every request 20–25 s).
Proven continuously by .loop/checks/latency.sh (C14). Finding along
the way: the old binary's full-tape verdicts were deadline-truncation
artefacts — its score passes re-burned the 3s allowance the sim had
already spent and silently scored prefixes of the tape. With the
full computation restored, an independent Python recomputation of
the whole record (transcribed predicates, scoring and fee rules)
matches the server exactly on all five strategies, and the slot
equity reconciles to the cent (checker round, 18 Nov, on a
252,312-line tape copy: paper pot $0.07 of its $100 start; every
strategy NOT PROVEN or INSUFFICIENT EVIDENCE on that tape state).
Verdicts are live-computed from the forward tape and move as it
grows — an earlier tape state briefly read one strategy PROVEN
before later windows flipped its second half negative — so no
verdict is quoted as a standing fact here or in any public copy;
the surfaces themselves are the record. A companion fix from the
same round: a non-terminating strategy is now named exhausted on
consecutive instruction-budget deaths (32), not only on elapsed
burn — with cheaper calls the clock alone no longer names it on
small tapes (robust.sh B16/B23).
Only the parsed tape is cached. Every /api/report, /api/slots,
/api/equity and /api/strategies request re-runs the full simulation
plus five full scorings — six passes over every window, each window
scan calling Rune per snapshot. Measured at scale-up close: ~10 s
cold at ~108k tape lines; the tape is now 223k+ lines and grows
~505 lines a minute, so every surface pays it again after every
collect pass.
Fix (incremental aggregation, two layers):
  1. A per-window entry index: a window's entry price for a strategy
     is a pure function of (strategy source, the window's snapshot
     sequence). Cache it under a content fingerprint of exactly the
     fields window_entry reads. Only windows whose snapshots changed
     (in practice: the currently-open ones) ever re-run Rune.
  2. An analysis cache keyed by the tape's exact file state (path,
     byte length, mtime — the same discipline as the parsed-tape
     cache): repeat requests against an unchanged tape serve the
     computed simulation + scores with no recompute at all.
Collector health's whole-file line count gets the same file-state
keying. Nothing cached is ever keyed by less than the content it
claims to summarise.
Proof: timings on a full-size copy of the live tape (223k+ lines) —
cold report, warm report, and report-after-append (one collect pass
appended) all measured; the after-append and warm figures must be a
small fraction of cold.

### G3 — No stage-target tracking (known item c) — DONE
stage_progress() computed from the sim's paper record; carried on
/api/slots and /api/home as stage_target and printed in the daily
report, labelled paper in fields and words. Proven by
.loop/checks/stage.sh (C15) + unit test.
The desk's stage one is a number — compound the pot to $1,000 — and
no surface says where the paper pot stands against it. The report
prints the pot and stops.
Fix: stage progress computed from the paper simulation's own record
(current paper equity vs the $1,000 stage-one target: percent of the
way, dollars to go, reached or not), carried on /api/home and
/api/slots and printed in the daily report — labelled PAPER in every
place it appears, in field names and in words. It is a paper figure
about a paper pot; it must never read as venue money or as a result.

### G4 — Front-end elevation (ordered 18 Nov 06:26 ET, sequenced LAST) — DONE
Elevated under the Hallmark-Rune skill: standard tokens on the desk
anchor, the pot curve leads with the slot strip beneath it, the
stage track inside the pot panel, venue figures demoted to a
labelled row, the daily report verbatim behind a disclosure, error
state with retry. Rendered lint: FLOOR 0 FAIL / WARN 0 at 8 widths
× both themes on full-tape and young-tape instances, error state
captured. Fresh-eyes checker PASS (kept-check: all 21 pre-elevation
element ids and every API call preserved; field truth cross-checked
against live responses; template test passed). Known polish items
left standing, named by the checker: the equity chart shares one
unlabelled Y axis between the pot curve and the strategy P&L
curves, and the retired-slot cards flood on a mature tape.
### G4 — original brief
Cayleb graded the front ends "very underwhelming". After G1–G3 land
and pass, elevate the whole UI with the Hallmark-Rune skill
(~/workspace/custom-skills/hallmark-rune/) — trading-desk vernacular:
diagrams and curves lead, numbers follow. The honesty fences bind
the design: a losing paper record and an unavailable figure are
shown as exactly those, never dressed. Checker rounds grade both
directions: the new look at 8 widths in both themes, AND every
functionality check from G1–G3 still passing.

## Explicitly out of scope / fenced off
- Live trading, order placement, arming: forbidden, and the fence
  check must still pass on the final tree and binary.
- Touching the running collector, its service, its binary or its
  tape: forbidden. All building/testing in this worktree.
- New dependencies beyond the allowlist {rune, serde, serde_json}:
  none needed for G1–G3 (rune::alloc::limit ships inside rune);
  if that changes, stop and report instead.
- Public claims: no profitability or win-rate claim may appear
  anywhere. The paper record is currently losing and is shown
  as-is; every strategy verdict stays INSUFFICIENT EVIDENCE until
  the forward evidence meets the standard in code.
