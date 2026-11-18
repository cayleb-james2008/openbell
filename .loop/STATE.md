# Loop state — OpenBell

- Status: CLOSED. verify[final] pass=12 fail=0 at HEAD 026bcab
  (branch loop/work). Three review rounds run (cap reached);
  B23 verified by the kit + mutation spot-check.
- The live collector runs the final binary
  (openbell-collector.service, port 8472); tape growing,
  dropped_lines 0. Restore-after-VM-replacement procedure:
  .loop/LEDGER.md rows ENV-1..3.
- Replacement staging: ~/workspace/restart-20261117/replacement/openbell/
  (README.md, openbell.html, CLAIMS.md — 29 claims).
- Backlog (unbuilt, in .loop/BACKLOG.md): scoring/collecting
  process isolation (the D2 durable fix), simulation result
  caching, /api/strategies dataset labelling without breaking
  its array shape.
