#!/bin/bash
# openbell-rs proof:
#  1. the collector records REAL public Kalshi market snapshots;
#  2. the shadow scorer scores the Rune strategies over a labelled fixture
#     tape at the real promotion standard (100 windows) and renders
#     discriminating verdicts;
#  3. the home API renders honest unavailable account states.
set -u
cd "$(dirname "$0")"
PORT=8478
WORK=$(mktemp -d)
trap 'kill $SERVER_PID 2>/dev/null; rm -rf "$WORK"' EXIT

cargo build 2>&1 | tail -1
mkdir -p "$WORK/data"
python3 gen_fixture.py "$WORK/data/fixture-snapshots.jsonl"

OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
sleep 1

api() { curl -s "http://127.0.0.1:$PORT$1"; }

echo "== collect real public Kalshi snapshots =="
curl -s -X POST "http://127.0.0.1:$PORT/api/collect"; echo
wc -l "$WORK/data/snapshots.jsonl"
echo "sample collected snapshot (real venue data):"
head -1 "$WORK/data/snapshots.jsonl" | jq '{ticker, close_time, yes_ask, no_ask, last_price}'

echo "== home: account figures honest =="
api /api/home | jq .

echo "== markets endpoint serves collected venue data =="
api /api/markets | jq 'length as $n | "markets on screen: \($n)"'

echo "== shadow scorer over fixture tape (real standard: 100 windows, both halves > 0, >= 3.5c) =="
api '/api/strategies?dataset=fixture' | jq '.[] | {strategy, windows_traded, mean_cents_per_contract, first_half_cents, second_half_cents, verdict}'

echo "== shadow scorer over the live collected tape (young tape — honest verdict expected) =="
api /api/strategies | jq '.[] | {strategy, windows_traded, verdict}'

echo "== daily report =="
api /api/report | jq -r .report
echo "PROOF DONE"
