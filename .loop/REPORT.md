# Ultra-loop report — OpenBell (Rust + Rune rebuild)

## The bar
`.loop/BAR.md` C1–C12: the fence as a mechanical scan (C2), venue
truth or "unavailable" on every number (C3/C6), the slot system
with compounding equity-split sizing (C4, amended in-bar with the
reason recorded), the promotion standard stated in code (C5),
collector health on the home screen (C6/C10), equity curves that
reconcile (C7), the daily report a user would read (C8), themes
(C9), judge (C11), owner (C12).

## Baseline
As-found commit 8bee2fb; `verify[baseline] pass=3 fail=0`
(build + 4 unit tests + proof). The shipped baseline already had
the shadow scorer, a 2-strategy library, and the collector; its
known soft spots were the hardcoded empty positions (fake $0.00
at risk), no slot system, and a report whose first line could lie
about why the venue was unavailable.

## What the scale-up built (B1–B7, phase 1)
The slot system in the shadow scorer (one slot per library
strategy, consecutive-loss strikes, rotation preferring a
strategy not already running, per-stint attribution, sizing =
per-window re-split of current total paper equity with fees on
top, slot equities summing to the pot); /api/slots + /api/equity;
the strategy library grown 2 → 5 Rune shapes; collector health on
the home screen and in the report; the equity chart; the daily
report rewritten around venue truth; light + dark themes (dark
default, toggle in Settings).

## Review rounds
- **Round 1** (standard + adversarial + checks-finder, frozen @
  a814299): the fence held under a broader manual sweep than the
  kit's; the headline defects were the pre-existing fake-$0.00
  positions, slot stint misattribution, the bar-vs-code sizing
  divergence (bar amended, not weakened — compounding verified
  against an independent recomputation), a 1TB-body process
  abort, tape splicing under concurrent collects, silent dropped
  lines, a non-terminating strategy wedging all scoring, and a
  vacuous proof.sh. Fixed as B8–B19; kit 12/12.
- **Round 2** (frozen @ d14289a): standard graded the bar MET
  with two naming caveats (both since aligned); the adversarial
  pass found the aggregate-budget hole (37s surfaces), the
  error-envelope collect reading as success, unvalidated venue
  records taped verbatim, and the fence scan's denylist evadable
  by an import alias + unlisted crates. Fixed as B21–B22; the
  fence's dependency check is now an exact allowlist with the
  process surface pinned to the file's shape.
- **Round 3** (confirmation, frozen @ 443837c): kit 12/12 solo;
  B21/B22 claims confirmed nearly in full (fence negative tests,
  cache invalidation, window truth, contract enforcement all
  reproduced). Two findings in the fixes themselves: the
  wall-clock deadline let one looper spend every honest
  strategy's allowance and zero the report's verdicts (HIGH,
  both-directions), and ?dataset=live silently served an empty
  tape labelled live. Fixed as B23 (consumed-time metering; the
  reviewer's repro is now a permanent robust.sh phase). Round
  cap reached; B23 is kit-verified, not re-reviewed.

## Final state
`bash .loop/verify final` → **pass=12 fail=0** (build, 11 unique
unit tests, 9 check scripts, proof) at HEAD 026bcab.
Mutation spot-check: removing the tradeable-price band in a
scratch copy makes robust.sh fail at the window-truth phase —
after the phase's own fixture was corrected (its dated window
originally lost the whole pot before the sub-cent window
arrived, masking the band; the same artifact bit the round-3
reviewer's first probe).

## The collector
openbell-collector.service runs the final binary; at the closing
check the tape held 107,873 lines and was growing
(~505/pass), collecting:true, dropped_lines:0,
conflicting_settlements:0, 650 settled windows across 655
markets. The VM was replaced three times during the run; each
time the unit + /etc/openbell-collector.env were restored from
the repo copy + the ambient proxy variables and collection
resumed (ledger ENV-1/2/3). **Those downtime windows are
unrecoverable** — uncollected windows cannot be backfilled —
and any future VM replacement will down the collector again
until someone restores it; the restore procedure is in the
ledger.

## Weakest remaining point
A Rune strategy can allocate unbounded memory inside a small
instruction count (round-2 D2: ~24 instructions → 600MB+ and a
wedged surface on a 2-line tape). Rune's budget counts
instructions, not bytes, and there is no in-process allocation
cap; the mitigations that exist are the per-call budget, the
consumed-time deadline, the 256KB file cap, and the fact that
the library is the owner's own files. The durable fix is
isolating scoring from collecting (a scoring crash must never
take the tape), which is an architecture step beyond this run.
Behind it: the simulation re-runs per request (parse/compile
are cached; the sim is not), so surface latency grows slowly
with the tape.
