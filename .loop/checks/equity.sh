#!/bin/bash
# C7: the equity curve is real. Per-strategy cumulative paper P&L points
# are chronological, and each series' final point equals that strategy's
# total paper P&L from the slot simulation over the same tape; the pot
# curve ends at the simulation's paper equity.
set -u
cd "$(dirname "$0")/../.."
PORT=8489
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
mkdir -p "$WORK/data"
python3 gen_fixture.py "$WORK/data/fixture-snapshots.jsonl" >/dev/null
python3 gen_slot_fixture.py "$WORK/data/slot-snapshots.jsonl" >/dev/null

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done

for ds in fixture slot; do
  eq=$(curl -s "http://127.0.0.1:$PORT/api/equity?dataset=$ds")
  sl=$(curl -s "http://127.0.0.1:$PORT/api/slots?dataset=$ds")
  echo "$eq" | jq -e --argjson sl "$sl" '
    ([.series[] | select(.points | length > 0)] | length) >= 1
  ' >/dev/null || { echo "FAIL [$ds]: no non-empty equity series: $eq"; exit 1; }
  echo "$eq" | jq -e '
    [.series[].points | map(.[0]) | . == sort] | all
  ' >/dev/null || { echo "FAIL [$ds]: a series is not chronological: $eq"; exit 1; }
  # Final point of each non-empty series == strategy total from /api/slots.
  mismatch=$(echo "$eq" | jq -r --argjson sl "$sl" '
    .series[] | select(.points | length > 0) | .strategy as $name
    | ($sl.strategy_totals[] | select(.strategy == $name) | .paper_pnl_dollars) as $total
    | select((.points[-1][1] - $total) | fabs > 0.01)
    | $name')
  [ -z "$mismatch" ] || { echo "FAIL [$ds]: equity final points disagree with slot totals for: $mismatch"; exit 1; }
  echo "$eq" | jq -e --argjson sl "$sl" '
    ((.pot[-1][1] // .paper_pot_start) - $sl.paper_equity_dollars) | fabs < 0.01
  ' >/dev/null || { echo "FAIL [$ds]: pot curve does not end at the simulation's paper equity"; exit 1; }
done

# The page draws the curves: it fetches /api/equity, builds an inline
# SVG (no external chart library), and labels the figures paper.
page=$(curl -s "http://127.0.0.1:$PORT/")
echo "$page" | grep -q '/api/equity' || { echo "FAIL: the page never fetches /api/equity"; exit 1; }
echo "$page" | grep -q '<svg' || { echo "FAIL: the page builds no SVG chart"; exit 1; }
echo "$page" | grep -qi 'paper' || { echo "FAIL: the chart is not labelled paper"; exit 1; }
# Only a loaded resource counts (src/href/url()/@import to http(s)). The React bundle carries the
# harmless SVG namespace string http://www.w3.org/2000/svg, so a bare http:// match is not a fetch.
if echo "$page" | grep -qiE '(src|href)=["'"'"']?https?://|url\(["'"'"']?https?://|@import|cdn\.' ; then echo "FAIL: the page pulls an external resource"; exit 1; fi

# Interior points: recompute favourite-late's whole curve from the
# trade ledger the slots endpoint exposes, and compare every point.
slots_fx=$(curl -s "http://127.0.0.1:$PORT/api/slots?dataset=fixture")
echo "$slots_fx" > "$WORK/slots.json"
curl -s "http://127.0.0.1:$PORT/api/equity?dataset=fixture" > "$WORK/eq.json"
python3 - "$WORK/slots.json" "$WORK/eq.json" <<'PYEOF2'
import json, sys
slots = json.load(open(sys.argv[1]))
eq = json.load(open(sys.argv[2]))
name = "favourite-late"
by_epoch = {}
for t in slots["trades"]:
    if t["strategy"] == name:
        by_epoch[t["close_epoch"]] = by_epoch.get(t["close_epoch"], 0.0) + t["pnl_dollars"]
run, expected = 0.0, []
for epoch in sorted(by_epoch):
    run += by_epoch[epoch]
    expected.append([epoch, run])
series = [s for s in eq["series"] if s["strategy"] == name]
assert series, "favourite-late missing from the equity series"
points = series[0]["points"]
# points[0] is the disclosed starting point (first epoch, 0.0).
assert points[0][1] == 0.0 and points[0][0] == expected[0][0], f"bad start point {points[0]}"
actual = points[1:]
assert len(actual) == len(expected), f"curve has {len(actual)} traded points, ledger says {len(expected)}"
for (e_epoch, e_val), (a_epoch, a_val) in zip(expected, actual):
    assert e_epoch == a_epoch and abs(e_val - a_val) < 1e-6, f"curve point mismatch: ledger {(e_epoch, e_val)} vs curve {(a_epoch, a_val)}"
print(f"interior points verified: {len(actual)}")
PYEOF2
[ $? -eq 0 ] || { echo "FAIL: a strategy curve disagrees with the trade ledger at an interior point"; exit 1; }

echo "EQUITY OK: curves are chronological and reconcile with the slot simulation on both tapes; the page draws them as labelled paper SVG"
