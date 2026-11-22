#!/bin/bash
# C14: report latency on a full-size tape. Builds a synthetic tape
# the size of the live one (~225k snapshots, 750 settled windows of
# ~300 snapshots each — the live tape's shape) and proves: a warm
# report is fast, a recompute after an append-only growth is a
# small fraction of the cold cost, and the numbers never change
# between cold, warm and post-append reads of the same record.
# Work lives under .loop/tmp (workspace disk), never /tmp.
set -u
cd "$(dirname "$0")/../.."
PORT=8503
WORK="$PWD/.loop/tmp/latency-$$"
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
mkdir -p "$WORK/data"
python3 - "$WORK/data/snapshots.jsonl" <<'PY'
import json, sys
from datetime import datetime, timedelta, timezone
path = sys.argv[1]
BASE = datetime(2026, 11, 10, 0, 0, tzinfo=timezone.utc)
def iso(dt): return dt.strftime("%Y-%m-%dT%H:%M:%SZ")
rows = []
for w in range(750):
    close = BASE + timedelta(minutes=15 * w)
    ce = int(close.timestamp())
    ya = round(0.63 + (w % 10) * 0.02, 2)
    result = "yes" if (w % 10) < 8 else "no"
    ticker = f"KXLAT15M-{w:04d}"
    for k in range(300):
        rows.append({
            "fetched_at": ce - 900 + 3 * k, "ticker": ticker,
            "event_ticker": ticker, "series": "KXLAT15M",
            "close_time": iso(close), "status": "active", "result": "",
            "yes_bid": round(ya - 0.02, 2), "yes_ask": ya,
            "no_bid": round(1.0 - ya, 2), "no_ask": round(1.0 - ya + 0.02, 2),
            "last_price": round(ya - 0.02, 2)})
    rows.append({
        "fetched_at": ce + 60, "ticker": ticker, "event_ticker": ticker,
        "series": "KXLAT15M", "close_time": iso(close), "status": "settled",
        "result": result, "yes_bid": None, "yes_ask": None,
        "no_bid": None, "no_ask": None, "last_price": None})
with open(path, "w") as f:
    for r in rows:
        f.write(json.dumps(r) + "\n")
print(f"wrote {len(rows)} snapshots")
PY

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  "${CARGO_TARGET_DIR:-./target}/debug/openbell-rs" > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 40); do curl -s "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && break; sleep 0.5; done
# The startup collection pass appends real venue snapshots to the
# default tape (the report's dataset). Wait — by watching the file,
# never an analysis endpoint, so the first /api/report below is a
# genuinely cold pass — until the tape has been stable a moment.
prev=-1; stable=0
for i in $(seq 1 60); do
  cur=$(wc -l < "$WORK/data/snapshots.jsonl")
  if [ "$cur" = "$prev" ]; then stable=$((stable+1)); else stable=0; fi
  if [ $i -ge 25 ] && [ $stable -ge 2 ]; then break; fi
  prev=$cur; sleep 1
done

tcold=$(curl -s -o "$WORK/cold.json" -w '%{time_total}' "http://127.0.0.1:$PORT/api/report")
twarm=$(curl -s -o "$WORK/warm.json" -w '%{time_total}' "http://127.0.0.1:$PORT/api/report")
# Append-only growth: five brand-new windows, all timestamps later.
python3 - "$WORK/data/snapshots.jsonl" <<'PY'
import json, sys
from datetime import datetime, timedelta, timezone
path = sys.argv[1]
BASE = datetime(2026, 11, 10, 0, 0, tzinfo=timezone.utc)
def iso(dt): return dt.strftime("%Y-%m-%dT%H:%M:%SZ")
with open(path, "a") as f:
    for w in range(750, 755):
        close = BASE + timedelta(minutes=15 * w)
        ce = int(close.timestamp())
        ya = 0.65
        ticker = f"KXLAT15M-{w:04d}"
        for k in range(300):
            f.write(json.dumps({
                "fetched_at": ce - 900 + 3 * k, "ticker": ticker,
                "event_ticker": ticker, "series": "KXLAT15M",
                "close_time": iso(close), "status": "active", "result": "",
                "yes_bid": 0.63, "yes_ask": ya, "no_bid": 0.35,
                "no_ask": 0.37, "last_price": 0.63}) + "\n")
        f.write(json.dumps({
            "fetched_at": ce + 60, "ticker": ticker, "event_ticker": ticker,
            "series": "KXLAT15M", "close_time": iso(close), "status": "settled",
            "result": "yes", "yes_bid": None, "yes_ask": None,
            "no_bid": None, "no_ask": None, "last_price": None}) + "\n")
print("appended 5 windows")
PY
tapp=$(curl -s -o "$WORK/app.json" -w '%{time_total}' "http://127.0.0.1:$PORT/api/report")

norm() { jq -r '.report' "$1" | grep -v '^As of ' | grep -v '^Collector:'; }
if ! diff <(norm "$WORK/cold.json") <(norm "$WORK/warm.json") >/dev/null; then
  echo "FAIL: warm report differs from the cold report on the same tape"; exit 1
fi
python3 - "$tcold" "$twarm" "$tapp" <<'PY'
import sys
cold, warm, app = (float(x) for x in sys.argv[1:4])
print(f"cold={cold:.2f}s warm={warm:.3f}s post-append={app:.2f}s")
assert warm < 2.0, f"warm report too slow: {warm:.3f}s"
assert app < max(2.0, cold / 3), f"post-append recompute is not a small fraction of cold: {app:.2f}s vs cold {cold:.2f}s"
PY
[ $? -eq 0 ] || exit 1

echo "LATENCY OK: full-size tape — warm and post-append reports are fast, numbers identical"
