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

cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
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
echo "== assertions =="
fail=0
# The collected tape exists, is non-empty, and every line parses.
api /api/home >/dev/null
tape="$WORK/data/snapshots.jsonl"
[ -s "$tape" ] || { echo "FAIL: the collected tape is empty"; fail=1; }
jq -s 'length > 100' "$tape" >/dev/null || { echo "FAIL: collected tape does not parse end to end"; fail=1; }
# The account block is honestly unavailable in this environment.
api /api/home | jq -e '.account.status == "unavailable" and .account.balance == null' >/dev/null \
  || { echo "FAIL: account block is not honestly unavailable"; fail=1; }
# The fixture verdicts discriminate: at least one strategy clears
# the bar's stated standard on the favourable fixture, and the rest
# do not all clear it.
api '/api/strategies?dataset=fixture' | jq -e '
  (map(select(.verdict | startswith("PROVEN"))) | length) >= 1
  and (map(select(.verdict | startswith("PROVEN"))) | length) < length' >/dev/null \
  || { echo "FAIL: fixture verdicts do not discriminate"; api '/api/strategies?dataset=fixture' | jq -c '.[].verdict'; fail=1; }
# The young live tape earns no promotion — its verdicts say so.
api /api/strategies | jq -e 'all(.verdict | startswith("PROVEN") | not)' >/dev/null \
  || { echo "FAIL: the young live tape claims a promotion it cannot have earned"; fail=1; }
# Report line 1 is the venue line.
api /api/report | jq -e -r '.report | split("\n")[0] | startswith("Day P&L (venue):")' >/dev/null \
  || { echo "FAIL: report line 1 is not the venue day P&L line"; fail=1; }
[ $fail -eq 0 ] || { echo "PROOF FAILED"; exit 1; }
echo "PROOF DONE"
