#!/bin/bash
# C15: stage-target tracking. The paper pot's progress toward the
# $1,000 stage-one target is computed from the paper record and
# labelled paper on every surface that carries it: /api/slots,
# /api/home, and the daily report.
set -u
cd "$(dirname "$0")/../.."
PORT=8502
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
mkdir -p "$WORK/data"
# Named dataset: the collector only ever appends to the default
# tape, so the fixture record is exactly the fixture.
python3 gen_fixture.py "$WORK/data/stage-snapshots.jsonl" >/dev/null

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done

slots=$(curl -s "http://127.0.0.1:$PORT/api/slots?dataset=stage")
echo "$slots" | jq -e '
  .stage_target.target_dollars == 1000
  and .stage_target.start_dollars == 100
  and (.stage_target.paper_equity_dollars - .paper_equity_dollars | fabs < 0.001)
  and (.stage_target.progress_pct - (.paper_equity_dollars / 10) | fabs < 0.05)
  and (.stage_target.remaining_dollars - (1000 - .paper_equity_dollars) | fabs < 0.01)
  and (.stage_target.reached == (.paper_equity_dollars >= 1000))' >/dev/null \
  || { echo "FAIL: /api/slots stage_target missing or inconsistent with the paper record: $slots"; exit 1; }
home=$(curl -s "http://127.0.0.1:$PORT/api/home")
echo "$home" | jq -e '.stage_target.target_dollars == 1000 and .stage_target.start_dollars == 100' >/dev/null \
  || { echo "FAIL: /api/home does not carry the stage target: $home"; exit 1; }
report=$(curl -s "http://127.0.0.1:$PORT/api/report" | jq -r '.report')
echo "$report" | grep -q 'Stage target (paper):' \
  || { echo "FAIL: daily report has no stage-target line"; echo "$report"; exit 1; }
echo "$report" | grep 'Stage target (paper):' | grep -q 'Paper only, not venue money' \
  || { echo "FAIL: stage-target line is not labelled paper: $(echo "$report" | grep 'Stage target')"; exit 1; }

echo "STAGE OK: stage-one target tracked from the paper record, labelled paper everywhere"
