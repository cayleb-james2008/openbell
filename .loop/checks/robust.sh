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
jq -e '[.[] | select(.strategy == "looper") | .budget_exhausted] == [true]' "$WORK/strats.json" >/dev/null \
  || { echo "FAIL: the looping strategy's report does not name its budget exhaustion"; exit 1; }
# Its load failure is NOT silent: a broken sibling is named in home
# AND on the sizing surface (a silent drop would resize every slot).
echo 'this is not rune' > "$WORK/strats/broken.rn"
curl -s "http://127.0.0.1:$PORT/api/home" | jq -e '[.library_errors[].file] | index("broken.rn") != null' >/dev/null \
  || { echo "FAIL: a broken strategy file is not named in /api/home library_errors"; curl -s "http://127.0.0.1:$PORT/api/home" | jq -c .library_errors; exit 1; }
curl -s "http://127.0.0.1:$PORT/api/slots?dataset=fixture" | jq -e '[.library_errors[].file] | index("broken.rn") != null' >/dev/null \
  || { echo "FAIL: a broken strategy file is not named in /api/slots library_errors"; exit 1; }
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

mkdir -p "$WORK/shim-env" "$WORK/shim-garbage" "$WORK/data-env" "$WORK/data-garbage"
cat > "$WORK/shim-env/curl" <<'SHIM'
#!/bin/bash
echo '{"error":{"code":"unavailable","message":"fake venue is down"}}'
SHIM
cat > "$WORK/shim-garbage/curl" <<'SHIM'
#!/bin/bash
echo '{"markets":[{"ticker":"KXBTC15M-FAKE","event_ticker":"KXBTC15M","close_time":"not-a-date","status":"finalized","result":"yes","yes_bid_dollars":"0.40","yes_ask_dollars":"0.45","no_bid_dollars":"0.55","no_ask_dollars":"0.60","last_price_dollars":"0.45"}]}'
SHIM
chmod +x "$WORK/shim-env/curl" "$WORK/shim-garbage/curl"
PATH="$WORK/shim-env:$PATH" OPENBELL_DATA="$WORK/data-env" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=8494 \
  ./target/debug/openbell-rs > "$WORK/server-env.log" 2>&1 &
PIDS="$PIDS $!"
PATH="$WORK/shim-garbage:$PATH" OPENBELL_DATA="$WORK/data-garbage" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=8493 \
  ./target/debug/openbell-rs > "$WORK/server-garbage.log" 2>&1 &
PIDS="$PIDS $!"
for p in 8494 8493; do for i in $(seq 1 20); do curl -s "http://127.0.0.1:$p/api/home" >/dev/null 2>&1 && break; sleep 0.5; done; done
code=$(curl -s -o "$WORK/env-resp.json" -w '%{http_code}' -X POST "http://127.0.0.1:8494/api/collect")
[ "$code" = "502" ] || { echo "FAIL: a venue answering only error envelopes got HTTP $code, not 502: $(cat "$WORK/env-resp.json")"; exit 1; }
code=$(curl -s -o "$WORK/garbage-resp.json" -w '%{http_code}' -X POST "http://127.0.0.1:8493/api/collect")
[ "$code" = "502" ] || { echo "FAIL: a venue sending only invalid records got HTTP $code, not 502: $(cat "$WORK/garbage-resp.json")"; exit 1; }
[ ! -s "$WORK/data-garbage/snapshots.jsonl" ] \
  || { echo "FAIL: invalid venue records were written to the tape"; exit 1; }
curl -s "http://127.0.0.1:8493/api/home" | jq -e '.collector.markets_seen == 0 and .collector.settled_windows == 0' >/dev/null \
  || { echo "FAIL: invalid records counted as venue evidence on the home surface"; exit 1; }

# A half-present credential gets its true reason (D-R4).
OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=8492 KALSHI_API_KEY_ID=dummy-id \
  ./target/debug/openbell-rs > "$WORK/server-half.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:8492/api/report" >/dev/null 2>&1 && break; sleep 0.5; done
curl -s "http://127.0.0.1:8492/api/report" | jq -r '.report' | head -1 | grep -q 'no private key path is set' \
  || { echo "FAIL: a credential ID without a key path does not get its true reason"; curl -s "http://127.0.0.1:8492/api/report" | jq -r '.report' | head -1; exit 1; }

echo "ROBUST OK: oversized bodies refused, looping strategies degrade, concurrent collects cannot splice the tape, env values clamped"
