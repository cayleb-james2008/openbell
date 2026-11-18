#!/bin/bash
# C13: strategy evaluation is memory-bounded. A hostile strategy
# whose allow_entry tries to allocate ~600MB inside a handful of
# instructions (Rune budgets instructions, not bytes) must fail its
# calls and trade nothing, while an honest neighbour scores its
# full record in the same pass and the server process stays small.
set -u
cd "$(dirname "$0")/../.."
PORT=8501
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
mkdir -p "$WORK/data" "$WORK/strats"
# Named dataset: the collector only ever appends to the default
# tape, so the fixture record is exactly the fixture. (chmod would
# not isolate it — the checks run as root, and root appends anyway.)
python3 gen_fixture.py "$WORK/data/mem-snapshots.jsonl" >/dev/null
cp strategies/favourite_late.rn "$WORK/strats/"
cat > "$WORK/strats/hostile.rn" <<'RN'
pub fn name() { "hostile" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
    let s = format!("{:>600000000}", "x");
    s.len() > 0 && yes_ask <= 0.99
}
RN

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$WORK/strats" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
MPID=$!
PIDS="$PIDS $MPID"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done

strats=$(curl -s "http://127.0.0.1:$PORT/api/strategies?dataset=mem")
echo "$strats" | jq -e '.[] | select(.strategy=="hostile") | select(.windows_traded==0) | select(.call_failures>0)' >/dev/null \
  || { echo "FAIL: hostile strategy traded or its allocation failures went uncounted: $strats"; exit 1; }
echo "$strats" | jq -e '.[] | select(.strategy=="favourite-late") | select(.windows_traded==120)' >/dev/null \
  || { echo "FAIL: the honest neighbour's record was harmed by the hostile strategy: $strats"; exit 1; }
curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 \
  || { echo "FAIL: server unhealthy after hostile evaluation"; exit 1; }
hwm=$(awk '/VmHWM/ {print $2}' "/proc/$MPID/status")
[ -n "$hwm" ] && [ "$hwm" -lt 409600 ] \
  || { echo "FAIL: server peak RSS ${hwm}kB exceeds 400MB under a 600MB-allocation strategy"; exit 1; }

echo "MEMORY OK: hostile allocation bounded (peak RSS ${hwm}kB), honest neighbour unharmed"
