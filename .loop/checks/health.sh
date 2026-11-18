#!/bin/bash
# C6: collector health on /api/home matches an independent count of the
# tape file, in both directions: a stale tape reads as not collecting,
# and after a real collection pass the same surface reads fresh.
set -u
cd "$(dirname "$0")/../.."
PORT=8491
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
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.5; done

# Independent count, straight from the file with jq.
exp_snaps=$(jq -s 'length' "$WORK/data/snapshots.jsonl")
exp_markets=$(jq -s '[.[].ticker] | unique | length' "$WORK/data/snapshots.jsonl")
exp_settled=$(jq -s '[.[] | select(.result != "") | .ticker] | unique | length' "$WORK/data/snapshots.jsonl")

home=$(curl -s "http://127.0.0.1:$PORT/api/home")
echo "$home" | jq -e --argjson s "$exp_snaps" --argjson m "$exp_markets" --argjson w "$exp_settled" '
  .collector.snapshots_on_disk == $s
  and .collector.markets_seen == $m
  and .collector.settled_windows == $w' >/dev/null \
  || { echo "FAIL: collector block disagrees with an independent count (expected $exp_snaps/$exp_markets/$exp_settled): $(echo "$home" | jq -c .collector)"; exit 1; }
# The fixture tape is historical: the surface must honestly read stale.
echo "$home" | jq -e '.collector.collecting == false and .collector.newest_snapshot_age_secs > 3600' >/dev/null \
  || { echo "FAIL: stale fixture tape does not read as not-collecting: $(echo "$home" | jq -c .collector)"; exit 1; }

# A real collection pass (public venue data) freshens the same surface.
curl -s -X POST "http://127.0.0.1:$PORT/api/collect" >/dev/null
home2=$(curl -s "http://127.0.0.1:$PORT/api/home")
echo "$home2" | jq -e --argjson s "$exp_snaps" '
  .collector.snapshots_on_disk > $s
  and .collector.newest_snapshot_age_secs < 300
  and .collector.collecting == true' >/dev/null \
  || { echo "FAIL: after a collection pass the surface does not read fresh: $(echo "$home2" | jq -c .collector)"; exit 1; }

echo "HEALTH OK: collector block matches an independent tape count; stale reads stale, a fresh pass reads collecting"
