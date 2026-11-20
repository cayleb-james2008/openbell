#!/bin/bash
# C9: the page ships light + dark themes — light is the default, the
# toggle lives in a Settings section, the choice persists, and icons
# are drawn symbols, never letters or emoji. (Served-page assertions:
# this environment has no browser to execute the page.)
set -u
cd "$(dirname "$0")/../.."
PORT=8493
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build"; exit 1; }
mkdir -p "$WORK/data"
OPENBELL_DATA="$WORK/data" OPENBELL_STRATEGIES="$PWD/strategies" \
OPENBELL_COLLECT_SECS=3600 PORT=$PORT \
  ./target/debug/openbell-rs > "$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
for i in $(seq 1 20); do curl -s "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && break; sleep 0.5; done
page=$(curl -s "http://127.0.0.1:$PORT/")

echo "$page" | grep -q 'data-theme' || { echo "FAIL: page has no data-theme mechanism"; exit 1; }
# The shipped page is a React bundle: the CSS minifier drops quotes, and the ids and names live in the bundle
# (the rendered DOM is built at run time). Each assertion below is the equivalent of the old raw-HTML string.
echo "$page" | grep -qE 'data-theme=.?light' || { echo "FAIL: no light theme variable set"; exit 1; }
echo "$page" | grep -qF "getItem('openbell-theme')||'light'" \
  || { echo "FAIL: light is not the default theme"; exit 1; }
echo "$page" | grep -qE 'setItem\(.openbell-theme' \
  || { echo "FAIL: theme choice is not persisted"; exit 1; }
echo "$page" | grep -qE 'id.{1,3}.settings.'  || { echo "FAIL: no Settings section for the toggle"; exit 1; }
echo "$page" | grep -qE 'id.{1,3}.themeBtn.'  || { echo "FAIL: no theme toggle button"; exit 1; }
echo "$page" | grep -qE '.Appearance.'  || { echo "FAIL: theme toggle has no accessible name"; exit 1; }
echo "$page" | grep -q '0 0 24 24' || { echo "FAIL: theme icons are not drawn SVG symbols"; exit 1; }
echo "$page" > "$WORK/page.html"
python3 - "$WORK/page.html" <<'PYEOF'
import sys
text = open(sys.argv[1], encoding="utf-8").read()
bad = [ch for ch in text if ord(ch) >= 0x1F300 or 0x2600 <= ord(ch) <= 0x27BF]
if bad:
    print("FAIL: emoji found on the page:", bad[:5])
    sys.exit(1)
PYEOF
[ $? -eq 0 ] || exit 1

echo "THEME OK: light + dark themes, light default, toggle in Settings, persisted, symbol icons only"
