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
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server.log" 2>&1 &
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
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server2.log" 2>&1 &
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
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server-env.log" 2>&1 &
PIDS="$PIDS $!"
PATH="$WORK/shim-garbage:$PATH" OPENBELL_DATA="$WORK/data-garbage" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=8493 \
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server-garbage.log" 2>&1 &
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
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server-half.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:8492/api/report" >/dev/null 2>&1 && break; sleep 0.5; done
curl -s "http://127.0.0.1:8492/api/report" | jq -r '.report' | head -1 | grep -q 'no private key path is set' \
  || { echo "FAIL: a credential ID without a key path does not get its true reason"; curl -s "http://127.0.0.1:8492/api/report" | jq -r '.report' | head -1; exit 1; }

# --- B22: the strategy contract is enforced, and call failures are
# named rather than read as quiet strategies (D8).
mkdir -p "$WORK/data2" "$WORK/strats2"
cp "$WORK/data/fixture-snapshots.jsonl" "$WORK/data2/snapshots.jsonl"
cat > "$WORK/strats2/banana.rn" <<'RN'
pub fn name() { "banana-side" }
pub fn side() { "banana" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { true }
pub fn describe() { "side() returns neither yes nor no" }
RN
cat > "$WORK/strats2/dup-a.rn" <<'RN'
pub fn name() { "same-name" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { true }
pub fn describe() { "first of two strategies sharing a name" }
RN
cat > "$WORK/strats2/dup-b.rn" <<'RN'
pub fn name() { "same-name" }
pub fn side() { "no" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { true }
pub fn describe() { "second of two strategies sharing a name" }
RN
cat > "$WORK/strats2/stringret.rn" <<'RN'
pub fn name() { "string-return" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { "yes please" }
pub fn describe() { "allow_entry returns a string, not a bool" }
RN
cat > "$WORK/strats2/valid.rn" <<'RN'
pub fn name() { "valid-one" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { yes_ask <= 0.90 }
pub fn describe() { "a well-formed strategy" }
RN
OPENBELL_DATA="$WORK/data2" OPENBELL_STRATEGIES="$WORK/strats2" \
OPENBELL_COLLECT_SECS=3600 PORT=8491 \
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server2.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:8491/api/home" >/dev/null 2>&1 && break; sleep 0.5; done
curl -s "http://127.0.0.1:8491/api/home" > "$WORK/home2.json"
jq -e '[.library_errors[].error] | any(test("side\\(\\) must return"))' "$WORK/home2.json" >/dev/null \
  || { echo "FAIL: a side() of 'banana' is not rejected with its reason"; cat "$WORK/home2.json" | jq .library_errors; exit 1; }
jq -e '[.library_errors[].error] | any(test("duplicate strategy name"))' "$WORK/home2.json" >/dev/null \
  || { echo "FAIL: duplicate strategy names are not rejected"; exit 1; }
curl -s "http://127.0.0.1:8491/api/strategies" > "$WORK/strats2.json"
jq -e 'length == 3 and ([.[].strategy] | unique | length == 3)' "$WORK/strats2.json" >/dev/null \
  || { echo "FAIL: expected the 3 loadable strategies (first same-name, string-return, valid-one) with unique names, got:"; cat "$WORK/strats2.json" | jq -r '.[].strategy'; exit 1; }
jq -e '.[] | select(.strategy=="string-return") | .call_failures > 0' "$WORK/strats2.json" >/dev/null \
  || { echo "FAIL: a non-boolean allow_entry is not counted as call failures"; cat "$WORK/strats2.json"; exit 1; }
curl -s "http://127.0.0.1:8491/api/slots" | jq -e '.dataset == "live" and ([.notes[]] | any(test("allow_entry failed")))' >/dev/null \
  || { echo "FAIL: slots do not name the string-return strategy's call failures, or the dataset is unlabelled"; exit 1; }
code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:8491/api/slots?dataset=nosuch")
[ "$code" = "404" ] || { echo "FAIL: an unknown dataset got HTTP $code, not 404"; exit 1; }

# --- B22: window truth — conflicts unscored, the settled record's
# own close time orders the compounding, sub-cent prices never size
# a trade (D4/D6/D7). The honestly-dated window WINS on purpose: a
# full-pot loss there would exhaust the pot before the sub-cent
# window arrives and mask the price band entirely.
mkdir -p "$WORK/data3" "$WORK/strats3"
cat > "$WORK/data3/snapshots.jsonl" <<'JSON'
{"series":"KXBTC15M","ticker":"KXBTC15M-C1","event_ticker":"KXBTC15M-C1","status":"active","close_time":"2030-01-01T00:15:00Z","yes_ask":0.5,"no_ask":0.5,"result":"yes","fetched_at":1893455000}
{"series":"KXBTC15M","ticker":"KXBTC15M-C1","event_ticker":"KXBTC15M-C1","status":"active","close_time":"2030-01-01T00:15:00Z","yes_ask":0.5,"no_ask":0.5,"result":"no","fetched_at":1893455010}
{"series":"KXBTC15M","ticker":"KXBTC15M-C2","event_ticker":"KXBTC15M-C2","status":"active","close_time":"not-a-date","yes_ask":0.5,"no_ask":0.5,"result":"","fetched_at":1893455020}
{"series":"KXBTC15M","ticker":"KXBTC15M-C2","event_ticker":"KXBTC15M-C2","status":"active","close_time":"2030-01-01T01:00:00Z","yes_ask":0.5,"no_ask":0.5,"result":"yes","fetched_at":1893455030}
{"series":"KXBTC15M","ticker":"KXBTC15M-C3","event_ticker":"KXBTC15M-C3","status":"active","close_time":"2030-01-01T02:00:00Z","yes_ask":0.001,"no_ask":0.999,"result":"yes","fetched_at":1893455040}
JSON
cat > "$WORK/strats3/always.rn" <<'RN'
pub fn name() { "always-yes" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { yes_ask <= 0.99 }
pub fn describe() { "enters every window on the yes side" }
RN
OPENBELL_DATA="$WORK/data3" OPENBELL_STRATEGIES="$WORK/strats3" \
OPENBELL_COLLECT_SECS=3600 PORT=8490 \
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server3.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:8490/api/home" >/dev/null 2>&1 && break; sleep 0.5; done
curl -s "http://127.0.0.1:8490/api/home" | jq -e '.collector.conflicting_settlements == 1' >/dev/null \
  || { echo "FAIL: home does not count the conflicting settlement"; exit 1; }
curl -s "http://127.0.0.1:8490/api/slots" > "$WORK/slots3.json"
jq -e '.conflicted_windows == 1' "$WORK/slots3.json" >/dev/null \
  || { echo "FAIL: the slot sim does not report the conflicted window"; cat "$WORK/slots3.json"; exit 1; }
jq -e '.trades | length == 1' "$WORK/slots3.json" >/dev/null \
  || { echo "FAIL: expected exactly one trade (the honestly-dated window); the conflict and the sub-cent window must not trade"; cat "$WORK/slots3.json" | jq .trades; exit 1; }
jq -e '.trades[0].close_epoch == 1893459600' "$WORK/slots3.json" >/dev/null \
  || { echo "FAIL: the trade's epoch is not the settled record's own close time"; cat "$WORK/slots3.json" | jq .trades; exit 1; }

# --- B22: one future-dated line mid-tape cannot make a dead
# collector read alive (D9). The last line rules freshness.
mkdir -p "$WORK/data4"
NOW=$(date +%s)
python3 - "$WORK/data4/snapshots.jsonl" "$NOW" <<'PY'
import json, sys
path, now = sys.argv[1], int(sys.argv[2])
def line(fetched, result):
    return json.dumps({"series":"KXBTC15M","ticker":"KXBTC15M-H1","event_ticker":"KXBTC15M-H1","status":"active","close_time":"2030-01-01T00:15:00Z","yes_ask":0.5,"no_ask":0.5,"result":result,"fetched_at":fetched})
open(path, "w").write("\n".join([
    line(now - 7200, "yes"),
    line(now + 315360000, "yes"),
    line(now - 7200, "yes"),
]) + "\n")
PY
OPENBELL_DATA="$WORK/data4" OPENBELL_STRATEGIES="$WORK/strats3" \
OPENBELL_COLLECT_SECS=60 PORT=8489 \
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server4.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:8489/api/home" >/dev/null 2>&1 && break; sleep 0.5; done
curl -s "http://127.0.0.1:8489/api/home" > "$WORK/home4.json"
jq -e '.collector.collecting == false and .collector.newest_snapshot_age_secs > 7000' "$WORK/home4.json" >/dev/null \
  || { echo "FAIL: a future-dated mid-tape line makes a stale tape read as collecting, or the age is not the last line's"; cat "$WORK/home4.json" | jq .collector; exit 1; }

# --- B23: one looping strategy must not spend the honest
# strategies' allowances (round 3, Finding 1: the deadline metered
# the pass's wall clock, so the looper's burn exhausted everyone,
# and the report's verdicts zeroed). Two servers, same fixture
# tape: clean library vs clean + looper. Honest totals must be
# IDENTICAL, and the looper server's report verdicts must match
# its own /api/strategies.
mkdir -p "$WORK/data5" "$WORK/strats5"
cp "$WORK/data/fixture-snapshots.jsonl" "$WORK/data5/snapshots.jsonl"
cp strategies/*.rn "$WORK/strats5/"
mkdir -p "$WORK/strats5l"
cp strategies/*.rn "$WORK/strats5l/"
cp "$WORK/strats/looper.rn" "$WORK/strats5l/"
OPENBELL_DATA="$WORK/data5" OPENBELL_STRATEGIES="$WORK/strats5" \
OPENBELL_COLLECT_SECS=3600 PORT=8488 \
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server5.log" 2>&1 &
PIDS="$PIDS $!"
OPENBELL_DATA="$WORK/data5" OPENBELL_STRATEGIES="$WORK/strats5l" \
OPENBELL_COLLECT_SECS=3600 PORT=8487 \
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server5l.log" 2>&1 &
PIDS="$PIDS $!"
for p in 8488 8487; do for i in $(seq 1 20); do curl -s "http://127.0.0.1:$p/api/home" >/dev/null 2>&1 && break; sleep 0.5; done; done
curl -s "http://127.0.0.1:8488/api/slots" | jq '[.strategy_totals[] | {strategy, windows_traded}] | sort_by(.strategy)' > "$WORK/totals-clean.json"
curl -s "http://127.0.0.1:8487/api/slots" | jq '[.strategy_totals[] | select(.strategy != "looper") | {strategy, windows_traded}] | sort_by(.strategy)' > "$WORK/totals-looper.json"
diff -q "$WORK/totals-clean.json" "$WORK/totals-looper.json" >/dev/null \
  || { echo "FAIL: a looping strategy changed the honest strategies' slot totals"; diff "$WORK/totals-clean.json" "$WORK/totals-looper.json"; exit 1; }
curl -s "http://127.0.0.1:8487/api/slots" | jq -e '[.notes[]] | any(test("looper exhausted")) and (any(test("favourite-late exhausted")) | not)' >/dev/null \
  || { echo "FAIL: the exhaustion notes do not name the looper alone"; curl -s "http://127.0.0.1:8487/api/slots" | jq .notes; exit 1; }
rep_traded=$(curl -s "http://127.0.0.1:8487/api/report" | jq -r '.report' | grep -o 'favourite-late (yes): [0-9]* windows traded' | grep -o '[0-9]*' | head -1)
api_traded=$(curl -s "http://127.0.0.1:8487/api/strategies" | jq '[.[] | select(.strategy=="favourite-late")][0].windows_traded')
[ -n "$rep_traded" ] && [ "$rep_traded" = "$api_traded" ] \
  || { echo "FAIL: the report's favourite-late verdict ($rep_traded traded) disagrees with /api/strategies ($api_traded) on the looper server"; exit 1; }

echo "ROBUST OK: oversized bodies refused, looping strategies degrade alone, concurrent collects cannot splice the tape, env values clamped, strategy contract enforced, window truth defended, freshness cannot be forged"


