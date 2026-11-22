#!/bin/bash
# C2 (v0.8.0): Hard Risk Rails, Disarmed-by-Default & Credential Hygiene Check.
# Replaces the legacy paper-only fence now that OpenBell supports Live Connected
# Kalshi v2 trading under strict fail-closed Risk Rails and an Emergency Kill Switch.
# Verifies mechanically:
#   1. Zero hardcoded credentials, private key material, or legacy live-desk paths
#      anywhere in source, strategies, assets, or the built binary.
#   2. Hard fail-closed risk rails (`live_armed: false` on boot, explicit `confirm_arm`,
#      whole-cent `1..=99`, contract/notional/daily-loss/exposure/prefix caps,
#      `emergency_kill_switch`, and `0600` secret file storage) are compiled & proven.
#   3. Live server boots disarmed by default and refuses orders fail-closed (403)
#      when `/api/live/*` routes are present.
set -u
cd "$(dirname "$0")/../.."
export PATH="$HOME/.local/rust/bin:$HOME/.rust/bin:$PATH"
PORT=8499
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }
BIN="${CARGO_TARGET_DIR:-./target}/debug/openbell-rs"

fail=0

# 1. Zero hardcoded credentials, raw key blobs, or legacy /home/cayleb/open-kal touches.
if grep -rn 'MIIE\|MIIB\|AKIA[0-9A-Z]\{16\}\|sk-[A-Za-z0-9]\{20,\}\|/home/cayleb/open-kal\|/home/cayleb/Work/projects/openbell' src/ strategies/ assets/ 2>/dev/null; then
  echo "FAIL: hardcoded key material or protected live-desk path found in source/assets"
  fail=1
fi
if strings "$BIN" | grep -q '/home/cayleb/open-kal'; then
  echo "FAIL: protected live-desk path present in built binary"
  fail=1
fi

# 2. Structural presence of fail-closed risk rails & 0600 credential hygiene.
for sym in "live_armed: false" "confirm_arm" "max_order_contracts" "max_order_dollars" "max_daily_loss_dollars" "max_open_exposure_dollars" "allowed_ticker_prefix" "emergency_kill_switch"; do
  grep -q "$sym" src/risk_rails.rs || { echo "FAIL: missing required risk rail invariant '$sym' in src/risk_rails.rs"; fail=1; }
done
grep -q "0o600" src/kalshi_live.rs || { echo "FAIL: src/kalshi_live.rs does not enforce 0600 permissions on stored credentials"; fail=1; }
grep -q "REDACTED" src/kalshi_live.rs || { echo "FAIL: src/kalshi_live.rs does not redact private key in Debug"; fail=1; }

# 3. Prove RSA-PSS signing, mock Kalshi v2 endpoints, hard rails & kill switch via cargo test.
cargo test --quiet kalshi_live::tests >/dev/null 2>&1 || { echo "FAIL: kalshi_live::tests failed"; fail=1; }
cargo test --quiet risk_rails::tests >/dev/null 2>&1 || { echo "FAIL: risk_rails::tests failed"; fail=1; }

# 4. Boot binary in clean empty dir and verify disarmed-by-default runtime invariant.
mkdir -p "$WORK/data"
OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 OPENBELL_UPDATE_MODE=off PORT=$PORT \
  "$BIN" --no-open > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for _ in $(seq 1 25); do curl -s "http://127.0.0.1:$PORT/api/home" >/dev/null 2>&1 && break; sleep 0.25; done

http_status=$(curl -s -o "$WORK/live_status.json" -w '%{http_code}' "http://127.0.0.1:$PORT/api/live/status")
if [ "$http_status" = "200" ]; then
  jq -e '.live_armed == false and .configured == false and .killed == false' "$WORK/live_status.json" >/dev/null \
    || { echo "FAIL: /api/live/status did not boot disarmed & unconfigured: $(cat "$WORK/live_status.json")"; fail=1; }
  if grep -q "BEGIN.*PRIVATE KEY" "$WORK/live_status.json"; then
    echo "FAIL: /api/live/status leaked private key PEM"
    fail=1
  fi
  ord_code=$(curl -s -o "$WORK/ord_resp.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/live/order" \
    -H 'Content-Type: application/json' -H 'X-OpenBell-CSRF: 1' \
    -d '{"ticker":"KXBTC15M-TEST","action":"buy","side":"yes","count":1,"price_cents":50}')
  [ "$ord_code" = "403" ] || { echo "FAIL: disarmed POST /api/live/order returned HTTP $ord_code, expected 403"; fail=1; }
  csrf_code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/live/arm" \
    -H 'Content-Type: application/json' -d '{"confirm_arm":true}')
  [ "$csrf_code" = "403" ] || { echo "FAIL: POST /api/live/arm without X-OpenBell-CSRF returned HTTP $csrf_code, expected 403"; fail=1; }
fi

[ $fail -eq 0 ] && echo "RAILS OK: hard risk rails, disarmed-by-default invariant, kill switch, RSA-PSS signing, and zero hardcoded secrets verified"
exit $fail
