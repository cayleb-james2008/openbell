#!/bin/bash
# C8: the daily report leads with the venue truth. Line 1 is the signed
# venue day P&L or the honest unavailable line (this environment has no
# venue credential, so the unavailable line is the truthful one here);
# per-slot paper lines follow, paper figures are labelled paper, the
# collector's state is stated, and no balance is invented.
set -u
cd "$(dirname "$0")/../.."
PORT=8492
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build"; exit 1; }
mkdir -p "$WORK/data"
python3 gen_fixture.py "$WORK/data/snapshots.jsonl" >/dev/null

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/report" >/dev/null 2>&1 && break; sleep 0.5; done
# The server's startup collection pass writes to the same tape for its
# first seconds; wait until the file is stable so the report and the
# independent count read the same tape.
prev=-1
for i in $(seq 1 30); do
  cur=$(wc -l < "$WORK/data/snapshots.jsonl")
  [ "$cur" = "$prev" ] && break
  prev=$cur; sleep 2
done

report=$(curl -s "http://127.0.0.1:$PORT/api/report" | jq -r '.report')
first=$(echo "$report" | head -1)
case "$first" in
  "Day P&L (venue): unavailable"*) : ;;
  *) echo "FAIL: report line 1 is not the venue P&L line: $first"; exit 1;;
esac
echo "$report" | grep -q 'Balance (venue): unavailable' \
  || { echo "FAIL: balance is not honestly unavailable in this environment"; echo "$report"; exit 1; }
# Unknown is not zero: with no venue read, the position count is
# unavailable — never a fabricated 0, on the report or the home cards.
echo "$report" | grep -q 'Open positions (venue): unavailable' \
  || { echo "FAIL: open positions are not honestly unavailable (a fabricated 0 states a venue fact never obtained)"; echo "$report"; exit 1; }
home=$(curl -s "http://127.0.0.1:$PORT/api/home")
echo "$home" | jq -e '.account.open_positions == null' >/dev/null \
  || { echo "FAIL: /api/home open_positions is not null (unavailable): $(echo "$home" | jq -c .account)"; exit 1; }
if echo "$report" | grep -Eq 'Balance \(venue\): \$[0-9]'; then
  echo "FAIL: the report invents a venue balance"; echo "$report"; exit 1
fi
# The server's own startup pass adds a fresh snapshot to the tape, so
# counts are checked against an independent count of the file as it
# stands now, and the collector state word must be one of the two
# honest states.
cs=$(jq -s 'length' "$WORK/data/snapshots.jsonl")
cm=$(jq -s '[.[].ticker] | unique | length' "$WORK/data/snapshots.jsonl")
cw=$(jq -s '[.[] | select(.result != "") | .ticker] | unique | length' "$WORK/data/snapshots.jsonl")
echo "$report" | grep -Eq 'Collector: (collecting|NOT collecting) —' \
  || { echo "FAIL: the report does not state the collector's state"; echo "$report"; exit 1; }
echo "$report" | grep -q "$cs snapshots on disk across $cm markets, $cw distinct settled windows" \
  || { echo "FAIL: collector counts in the report disagree with the tape ($cs/$cm/$cw)"; echo "$report"; exit 1; }
nlib=$(ls strategies/*.rn | wc -l)
nslots=$(echo "$report" | grep -c '^  Slot [0-9]*:')
[ "$nslots" = "$nlib" ] || { echo "FAIL: report has $nslots slot lines for a $nlib-strategy library"; echo "$report"; exit 1; }
echo "$report" | grep -Eq 'Paper pot: -?\$[0-9]' \
  || { echo "FAIL: no paper pot line"; echo "$report"; exit 1; }
# Attribution honesty: a retired line names the successor chosen at
# retirement, and slot lines separate the current stint from the
# slot's cumulative record.
echo "$report" | grep -Eq 'Retired: .* — slot rotated to ' \
  || { echo "FAIL: no retirement records its successor"; echo "$report"; exit 1; }
echo "$report" | grep -q 'this stint:' \
  || { echo "FAIL: slot lines do not separate the current stint from the slot total"; echo "$report"; exit 1; }
if echo "$report" | grep -q '\$-'; then
  echo "FAIL: money is formatted \$-x instead of -\$x"; echo "$report"; exit 1
fi
echo "$report" | grep -q 'paper only, not venue money' \
  || { echo "FAIL: the paper pot is not labelled as paper, not venue money"; echo "$report"; exit 1; }
echo "$report" | grep -q 'favourite-late' \
  || { echo "FAIL: strategy verdicts missing from the report"; echo "$report"; exit 1; }

echo "REPORT OK: line 1 is the venue truth, slots and collector are stated, paper is labelled paper"
