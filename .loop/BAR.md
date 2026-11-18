# Bar — openbell-rs scale-up

Kind: code + interface (a local web app: Rust binary serving the desk's
home screen) over a live data collector.
Delivered means: commits on `loop/work` in this repo; nothing pushed.
Facts never invented: venue account figures (unavailable unless the venue
record says otherwise), strategy verdicts (only the promotion standard in
code), paper figures (always labelled paper, always recomputed from the
tape they claim to summarise).

## Hard fence (not a condition — a law over every condition)

No order-placement code path exists or may be introduced, in any form: no
order endpoints, no signed trading requests, no arming. Kalshi is the only
source for any number shown; where the venue record is silent the screen
says unavailable.

## Conditions

- C1 (cmd): `bash .loop/verify final` passes end to end — cargo build,
  cargo test, every `.loop/checks/*.sh`, and `proof.sh` reaching
  PROOF DONE.
- C2 (cmd): The fence holds mechanically: a source and binary scan finds
  no order-placement surface (no `/orders` calls, no order-placement
  identifiers, no request signing for trading endpoints) anywhere in the
  tree or the built binary. (`.loop/checks/fence.sh`)
- C3 (cmd): The slot system runs in the shadow scorer. Over the fixture
  tape, the simulation reports per-slot state: each slot names its
  strategy, carries a strike count, a status (active or retired), and a
  paper equity share; a slot whose strategy takes three losing windows
  accumulates three strikes and is retired, and the next library strategy
  takes its place. (Rust tests + `.loop/checks/slots.sh` over the API)
- C4 (cmd): Paper sizing is equity-split and recomputes. The sizing
  base is the desk's total paper equity as it stands at the start of
  each window (the owner's recorded baseline: sizing base = total
  equity, split evenly across active slots at 100% of share — the pot
  compounds). With current equity E and N active slots, each paper
  trade's contracts equal floor((E/N) / entry price) (minimum 0), and
  the reported paper P&L in dollars equals an independent
  recomputation from the same tape. (Amended during the B11 fix: the
  original text froze E at the starting pot, which contradicts the
  desk baseline and de-compounds the desk; the implemented rule was
  verified against an independent recomputation to the cent.)
  (Rust tests)
- C5 (cmd): The Rune strategy library holds at least four strategies;
  every one loads, and the scorer's entries for each are driven by its
  Rune `allow_entry` — mutating one script's threshold in a scratch copy
  changes that strategy's traded-window count and no other's.
  (`.loop/checks/library.sh`)
- C6 (cmd): Collector health is on the home data. `/api/home` carries a
  collector block — snapshots on disk, distinct markets, distinct settled
  windows, seconds since the newest snapshot — and every figure matches
  an independent count of the data dir. (`.loop/checks/health.sh`)
- C7 (cmd): The equity curve is real. `/api/equity` returns, per
  strategy, cumulative paper P&L points over settled windows in
  chronological order; each series' final point equals that strategy's
  total paper P&L from the slot simulation over the same tape.
  (`.loop/checks/equity.sh`)
- C8 (cmd): The daily report reads like a report: line 1 is the venue day
  P&L — signed when the venue provides it, the honest unavailable line
  when it does not; the body carries per-slot lines (strategy, status,
  strikes, paper P&L) and collector health; no figure appears that is
  neither venue, tape, nor labelled paper. (`.loop/checks/report.sh`)
- C9 (cmd): The served page carries light and dark themes: both variable
  sets present, dark the default, a theme toggle in a settings area,
  choice persisted (localStorage); no emoji used as icons.
  (`.loop/checks/theme.sh`)
- C10 (cmd): Account honesty is preserved: with no credential in the
  environment, and equally with a dummy credential present, account
  figures render as unavailable and no code path attempts a signed read.
  (covered by C2 scan + existing tests)
- C11 (judge): "Would this screen still fit another product's name?"
  Pass example: the OpenBell home screen, whose report leads with the
  venue P&L line and whose slots carry strikes — only this desk talks
  that way. Fail example: the same layout renamed to a generic crypto
  dashboard with the desk's own words removed.
- C12 (owner): The look and feel of the finished screen. The loop never
  marks this passed.
- C13 (cmd): Strategy evaluation is memory-bounded: a hostile Rune
  strategy that tries to allocate ~600MB inside a few instructions
  fails its calls, trades nothing, and leaves an honest neighbour's
  record and the server's health untouched (peak RSS < 400MB).
  (`.loop/checks/memory.sh`)
- C14 (cmd): Report latency on a full-size tape (~225k snapshots,
  ~750 settled windows): a warm report answers in under 2s, a
  recompute after append-only growth costs a small fraction of the
  cold pass, and cold / warm / post-append reports of the same
  record are identical. (`.loop/checks/latency.sh`)
- C15 (cmd): Stage-target tracking: the paper pot's progress toward
  the $1,000 stage-one target is computed from the paper record and
  labelled paper on /api/slots, /api/home and the daily report.
  (`.loop/checks/stage.sh`)

## DIRECTION v1 (interface)

- Product: Cayleb's Kalshi compounding desk at home — tape health, slot
  by slot paper performance, and the venue's own word on the account,
  with nothing dressed up.
- Character: blunt, exact, unsentimental. A desk book, not a brochure.
- World: a bookmaker's ledger crossed with a night instrument panel. The
  neutrals are tinted toward deep green-slate; the one accent is the
  bell amber, spent on the report headline and the proven verdicts only.
- Moments: the daily report block (the thing he actually reads) and the
  equity curve over the tape.
- Signature: line 1 of the report is always the venue day P&L, signed —
  or the word unavailable, never a substitute number. Only this desk
  leads with its own record against itself.
