#!/bin/bash
# B10: rotation prefers a strategy that is not already running, and
# when that is impossible (slots == library size, every other strategy
# running) the duplication is disclosed in the simulation's notes —
# never silent. Fixture: a 3-strategy scratch library over a tape of
# three NO windows followed by YES windows; favourite-late strikes out
# while both other strategies are running.
set -u
cd "$(dirname "$0")/../.."
PORT=8494
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
[ -x ./target/debug/openbell-rs ] || cargo build >/dev/null 2>&1
mkdir -p "$WORK/data" "$WORK/strats"
cp strategies/favourite_late.rn strategies/longshot_late.rn strategies/no_favourite.rn "$WORK/strats/"
python3 gen_slot_fixture.py "$WORK/data/slot-snapshots.jsonl" >/dev/null

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$WORK/strats" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done

slots=$(curl -s "http://127.0.0.1:$PORT/api/slots?dataset=slot")
echo "$slots" | jq -e '.retired[] | select(.strategy=="favourite-late")' >/dev/null \
  || { echo "FAIL: favourite-late never retired on this tape: $slots"; exit 1; }
# Whatever the end state, if any strategy runs in more than one slot,
# the notes must say so, naming it.
echo "$slots" | jq -e '
  .slots | group_by(.strategy) | map(select(length > 1) | .[0].strategy) as $dups
  | ($dups | length == 0) or
    ($dups | all(. as $name | ($ARGS.named.notes | map(select(test($name))) | length) >= 1))
' --argjson notes "$(echo "$slots" | jq '.notes')" >/dev/null \
  || { echo "FAIL: a strategy runs in multiple slots without disclosure: $slots"; exit 1; }
# And on this tape a duplication DOES occur (3 slots, 3 strategies,
# favourite-late strikes out while the others run), so the notes must
# be non-empty.
echo "$slots" | jq -e '.notes | length >= 1' >/dev/null \
  || { echo "FAIL: expected a duplication disclosure on this tape, notes are empty: $slots"; exit 1; }

echo "ROTATION OK: successors avoid running strategies where possible; unavoidable duplication is disclosed"
