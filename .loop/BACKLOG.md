# Backlog — openbell-rs

| id | what is wrong / unmet | reproduce | severity | status |
|---|---|---|---|---|
| B1 | The shadow scorer scores strategies in isolation — there is no slot system: no per-slot state, no strikes, no retirement/replacement, no shared pot. Bar C3. | Score the fixture tape: output is per-strategy means only; nothing carries strikes or a slot status. | high | open |
| B2 | Paper results are cents-per-contract only — no equity-split sizing, no paper pot, no dollar P&L. Bar C4. | Read ScoreReport: no contracts, no dollars, no pot. | high | open |
| B3 | Strategy library is two Rune scripts. The desk's book needs a richer library for slots to rotate through. Bar C5. | `ls strategies/` → favourite_late.rn, longshot_late.rn only. | medium | open |
| B4 | Collector health is invisible: nothing on the home screen says whether the tape is fresh, how big it is, or how many windows have settled. Bar C6. | GET /api/home: account + series only. | medium | open |
| B5 | No chart over the tape: paper performance has no equity curve, only summary numbers. Bar C7. | No /api/equity route exists. | medium | open |
| B6 | The daily report is a debug dump: counts and one-line verdicts, no per-slot story, no collector health, no pot. Bar C8. | GET /api/report and read it as the desk's owner. | medium | open |
| B7 | UI is dark-only with uppercase micro-labels; no settings, no theme toggle. Bars C9, C11. | Serve the page: single dark theme, no settings area. | medium | open |

## Leads

- The live tape (openbell-data/) is young: strategies will honestly read INSUFFICIENT EVIDENCE on it. That is correct behaviour, not a defect; fixture-tape checks carry the standard at full size.
- gen_fixture.py's tape makes favourite-late win and longshot-late lose by construction; slot rotation checks need a tape where the first library strategy loses three straight — extend the fixture generator or build the case in Rust tests.
