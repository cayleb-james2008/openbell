#!/bin/bash
# C5: the strategy library is real and plural. At least four strategies
# load from the Rune files, and mutating one strategy file changes only
# that strategy's traded windows — proof the scorer executes the files,
# not a hardcoded copy of them.
set -u
cd "$(dirname "$0")/../.."
PORT=8490
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
mkdir -p "$WORK/data" "$WORK/strats"
cp strategies/*.rn "$WORK/strats/"
python3 gen_fixture.py "$WORK/data/fixture-snapshots.jsonl" >/dev/null

start_server() {
  OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$WORK/strats" \
  OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
    ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
  PIDS="$PIDS $!"
  for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done
}
stop_server() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; PIDS=""; sleep 0.5; }

start_server
before=$(curl -s "http://127.0.0.1:$PORT/api/strategies?dataset=fixture")
n=$(echo "$before" | jq 'length')
[ "$n" -ge 4 ] || { echo "FAIL: only $n strategies loaded, expected at least 4"; exit 1; }
echo "$before" | jq -e '[.[].strategy] | contains(["favourite-late", "longshot-late", "favourite-floor", "no-favourite", "mid-window-momentum"])' >/dev/null \
  || { echo "FAIL: expected strategy names missing: $(echo "$before" | jq -c '[.[].strategy]')"; exit 1; }
stop_server

# Mutate ONE strategy in the scratch copy: favourite-late's ask cap
# 0.85 -> 0.50. The fixture's YES asks cycle 0.63..0.81, so it must
# stop trading entirely; every other strategy must be untouched.
sed -i 's/yes_ask <= 0.85/yes_ask <= 0.50/' "$WORK/strats/favourite_late.rn"
start_server
# Wait until the mutated server actually answers with strategies —
# under load a bare readiness probe can pass against a dying or
# not-yet-bound instance and the assertions read an empty response.
after=""
for i in $(seq 1 40); do
  after=$(curl -s "http://127.0.0.1:$PORT/api/strategies?dataset=fixture")
  echo "$after" | jq -e 'length >= 4' >/dev/null 2>&1 && break
  sleep 0.5
done
echo "$after" | jq -e 'length >= 4' >/dev/null \
  || { echo "FAIL: mutated server never returned the strategy list: $after"; exit 1; }
fav=$(echo "$after" | jq '[.[] | select(.strategy == "favourite-late")][0].windows_traded')
[ "$fav" = "0" ] || { echo "FAIL: mutated favourite-late still trades $fav windows — the scorer is not executing the file"; exit 1; }
diffcount=$(jq -n --argjson a "$before" --argjson b "$after" '
  [($a[] | .strategy) as $name
   | select(($a[] | select(.strategy == $name) | .windows_traded)
            != ($b[] | select(.strategy == $name) | .windows_traded))
   | $name] | length')
[ "$diffcount" = "1" ] || { echo "FAIL: mutating one strategy changed $diffcount strategies' traded windows, expected exactly 1"; exit 1; }

echo "LIBRARY OK: $n strategies load from Rune files; mutating one file changes only that strategy"
