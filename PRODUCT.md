# OpenBell — product context

_Inferred from the owner's explicit brief (17 Nov 2026 scale-up order and
the approved goal), labelled as inference per impeccable init rules; no
interview was run because this is a scoped scale-up of an existing
surface._

## What it is

OpenBell is Cayleb's Kalshi compounding desk. A system of slots trades the
15-minute crypto markets on Kalshi; each slot runs one shape, sized from
the pot split evenly across active slots, and a shape that keeps losing is
struck out and replaced. This build is the desk's measuring machine and
home screen: a forward collector recording Kalshi's public market data, a
shadow scorer that runs the slot system on paper against that tape, and a
daily report in plain words. It places no orders — there is no
order-placement code path in it at all.

## Who it is for

Cayleb, checking in on the desk: is the tape healthy, what is each slot
doing on paper, what would the pot be doing, and what does the venue
record say — with nothing dressed up. A losing or unproven record reads as
losing or unproven.

## Surface mode

Operate/Read. The daily report is the headline; the tables and the equity
chart serve it.

## Rules the surface lives by

- Venue truth on every number: every figure comes from Kalshi's record,
  from the collected tape, or from the paper simulation — and each kind is
  labelled as what it is. Anything else is shown as unavailable.
- Paper figures are always labelled paper. A strategy is "proven" only by
  the promotion standard in code, on forward evidence.
- Light and dark themes, toggle in settings, dark is the default.
- Icons are drawn symbols, never letters, never emoji.
- No order placement exists in this build, in any form.
