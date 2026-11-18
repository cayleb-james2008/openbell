#!/bin/bash
# Robustness: the server survives hostile requests and hostile
# strategies, concurrent collection cannot splice the tape, and
# absurd environment values are clamped to stated behaviour.
set -u
cd "$(dirname "$0")/../.."
export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
WORK=$(mktemp -d)
PIDS=""
trap 'kill $PIDS 2>/dev/null; rm -rf "$WORK"' EXIT

mkdir -p "$WORK/data" "$WORK/strats"
python3 gen_fixture.py "$WORK/data/fixture-snapshots.jsonl" >/dev/null
cp "$WORK/data/fixture-snapshots.jsonl" "$WORK/data/snapshots.jsonl"
# A strategy that never terminates inside allow_entry.
cat > "$WORK/strats/looper.rn" <<'RN'
pub fn name() { "looper" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
    let x = 0;
    while true { x += 1; }
    x > 0
}
pub fn describe() { "looper — never returns from allow_entry" }
RN
cp strategies/*.rn "$WORK/strats/"

PORT=8496
OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$WORK/strats" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done

# A declared 1TB body is refused without being read, and the server
# answers the next request normally.
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/collect" -H 'Content-Length: 1000000000000' --max-time 10)
[ "$code" = "413" ] || { echo "FAIL: a 1TB declared body got HTTP $code, not 413"; exit 1; }
curl -s "http://127.0.0.1:$PORT/api/home" | jq -e '.mode' >/dev/null \
  || { echo "FAIL: the server did not survive the oversized request"; exit 1; }

# The looping strategy degrades to no-entry; scoring answers in
# bounded time instead of wedging every surface forever.
start=$(date +%s)
curl -s --max-time 30 "http://127.0.0.1:$PORT/api/strategies?dataset=fixture" > "$WORK/strats.json"
elapsed=$(( $(date +%s) - start ))
[ $elapsed -lt 30 ] || { echo "FAIL: scoring wedged on a non-terminating strategy (${elapsed}s)"; exit 1; }
jq -e '[.[] | select(.strategy == "looper") | .windows_traded] == [0]' "$WORK/strats.json" >/dev/null \
  || { echo "FAIL: the looping strategy traded windows instead of degrading to no-entry"; cat "$WORK/strats.json" | jq -c '.[] | select(.strategy=="looper")'; exit 1; }
# Its load failure is NOT silent: a broken sibling is named in home.
echo 'this is not rune' > "$WORK/strats/broken.rn"
curl -s "http://127.0.0.1:$PORT/api/home" | jq -e '[.library_errors[].file] | index("broken.rn") != null' >/dev/null \
  || { echo "FAIL: a broken strategy file is not named in /api/home library_errors"; curl -s "http://127.0.0.1:$PORT/api/home" | jq -c .library_errors; exit 1; }
rm "$WORK/strats/broken.rn"

# Concurrent collection passes cannot splice tape lines. Passes
# serialize on the app's collect lock, so at every instant — during
# the passes as well as after — the tape parses end to end. (Three
# passes, not five: the property is the lock, and each pass costs a
# full round of venue fetches.)
CP=""
for i in 1 2 3; do curl -s -X POST "http://127.0.0.1:$PORT/api/collect" >/dev/null & CP="$CP $!"; done
alive=1
while [ $alive = 1 ]; do
  alive=0
  for p in $CP; do kill -0 $p 2>/dev/null && alive=1; done
  [ $alive = 0 ] && break
  jq -s 'length > 0' "$WORK/data/snapshots.jsonl" >/dev/null \
    || { echo "FAIL: the tape did not parse DURING concurrent collection (spliced lines)"; exit 1; }
  sleep 2
done
wait $CP
jq -s 'length > 0' "$WORK/data/snapshots.jsonl" >/dev/null \
  || { echo "FAIL: concurrent collection passes left unparseable (spliced) tape lines"; exit 1; }

# Environment clamps: a 0s interval becomes the stated 5s floor, and
# an infinite pot falls back to the default 100.
OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=0 OPENBELL_PAPER_POT=inf PORT=8495 \
  ./target/debug/openbell-rs > "$WORK/server2.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:8495/api/home" >/dev/null 2>&1 && break; sleep 0.5; done
curl -s "http://127.0.0.1:8495/api/home" | jq -e '.collector.collect_interval_secs == 5' >/dev/null \
  || { echo "FAIL: OPENBELL_COLLECT_SECS=0 was not clamped to the 5s floor"; curl -s "http://127.0.0.1:8495/api/home" | jq -c .collector; exit 1; }
curl -s "http://127.0.0.1:8495/api/slots?dataset=fixture" | jq -e '.paper_pot_start == 100' >/dev/null \
  || { echo "FAIL: OPENBELL_PAPER_POT=inf did not fall back to the default pot"; exit 1; }

echo "ROBUST OK: oversized bodies refused, looping strategies degrade, concurrent collects cannot splice the tape, env values clamped"
