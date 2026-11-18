#!/bin/bash
# C3/C4 (API level): the slot system runs over a fixture tape where YES
# loses every window — the favourite's slot takes three consecutive
# strikes, retires the strategy, and rotates; slot equities sum to the
# paper pot's equity.
set -u
cd "$(dirname "$0")/../.."
PORT=8488
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
[ -x ./target/debug/openbell-rs ] || cargo build >/dev/null 2>&1
mkdir -p "$WORK/data"
python3 gen_slot_fixture.py "$WORK/data/slot-snapshots.jsonl" >/dev/null

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done

nlib=$(ls strategies/*.rn | wc -l)
slots=$(curl -s "http://127.0.0.1:$PORT/api/slots?dataset=slot")
echo "$slots" | jq -e --argjson n "$nlib" '.slots | length == $n' >/dev/null \
  || { echo "FAIL: expected $nlib slots (one per library strategy): $slots"; exit 1; }
echo "$slots" | jq -e '.retired[] | select(.strategy=="favourite-late") | select(.strikes==3) | select(.windows_traded==3)' >/dev/null \
  || { echo "FAIL: favourite-late was not retired on exactly 3 strikes / 3 windows: $slots"; exit 1; }
slotidx=$(echo "$slots" | jq -r '.retired[] | select(.strategy=="favourite-late") | .slot' | head -1)
echo "$slots" | jq -e --argjson i "$slotidx" '.slots[$i] | select(.strategy != "favourite-late") | select(.rotations == 1)' >/dev/null \
  || { echo "FAIL: slot $slotidx did not rotate to the next strategy: $slots"; exit 1; }
echo "$slots" | jq -e '(.strategy_totals | length) == (.slots | length)' >/dev/null \
  || { echo "FAIL: strategy totals missing or mismatched: $slots"; exit 1; }
echo "$slots" | jq -e '([.slots[].equity_dollars] | add) as $sum | ($sum - .paper_equity_dollars) | fabs < 0.01' >/dev/null \
  || { echo "FAIL: slot equities do not sum to the paper equity: $slots"; exit 1; }
echo "$slots" | jq -e '.paper_pot_start == 100' >/dev/null \
  || { echo "FAIL: paper pot is not the stated default: $slots"; exit 1; }

echo "SLOTS OK: strikes accumulate, three in a row retires and rotates, equities reconcile"
