#!/bin/bash
# Z1: a first run needs no environment. The real binary, started in an
# empty environment (env -i): the default port falls back when busy, the
# built-in strategy library is seeded, an explicit busy PORT fails loudly,
# and the browser opener runs only when a person can see it. A fake
# xdg-open first on PATH records its call; nothing real is opened.
# Ports: 8472 (the default, held by a listener this check starts), 8473
# (the fallback the desk should take), and 9681-9686 (explicit PORT runs).
set -u
cd "$(dirname "$0")/../.."
WORK=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill -9 $p 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT
fail=0
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build"; exit 1; }
BIN="${CARGO_TARGET_DIR:-$PWD/target}/debug/openbell-rs"

mkdir -p "$WORK/bin" "$WORK/home" "$WORK/cwd"
cat > "$WORK/bin/xdg-open" <<EOF
#!/bin/sh
printf '%s\n' "\$@" >> "$WORK/opened.txt"
EOF
chmod +x "$WORK/bin/xdg-open"

# start_desk <data dir> <PORT or empty> <extra env words> [args...]
# Runs the binary as the only process of an env -i environment; the caller reads $! for its PID.
start_desk() {
  local data="$1" port="$2" extra="$3"; shift 3
  ( cd "$WORK/cwd" && exec env -i PATH="$WORK/bin:/usr/bin:/bin" HOME="$WORK/home" \
      OPENBELL_DATA="$data" OPENBELL_COLLECT_SECS=3600 OPENBELL_UPDATE_MODE=off \
      ${port:+PORT=$port} $extra "$BIN" "$@" >"$data.log" 2>&1 ) &
  PIDS="$PIDS $!"
}

# wait until the banner is printed or the process is gone
wait_banner() {
  local log="$1" pid="$2"
  for _ in $(seq 1 60); do
    grep -q 'Your desk:' "$log" 2>/dev/null && return 0
    kill -0 "$pid" 2>/dev/null || return 1
    sleep 0.25
  done
  return 1
}

desk_url() { sed -n 's/.*Your desk: *\(http[^ ]*\).*/\1/p' "$1" | head -1; }

opened_count() { [ -f "$WORK/opened.txt" ] && wc -l < "$WORK/opened.txt" || echo 0; }

# --- 1. default port busy: the desk takes the next free port, seeds the library, opens nothing without a display
node -e "require('net').createServer().listen(8472,'127.0.0.1')" &
HOLD=$!; PIDS="$PIDS $HOLD"; sleep 1
D1="$WORK/d1"; mkdir -p "$D1"
start_desk "$D1" "" ""
pid=$!
if wait_banner "$D1.log" "$pid"; then
  url=$(desk_url "$D1.log")
  port=${url##*:}
  if [ "$port" = "8472" ] || [ -z "$port" ]; then
    echo "FAIL: default port fallback: banner names $url"; fail=1
  elif ! curl -s -m 3 "$url/api/version" | jq -e '.name == "openbell-rs"' >/dev/null; then
    echo "FAIL: the fallback address $url does not answer /api/version"; fail=1
  else
    echo "ok: default port busy -> desk at $url"
  fi
  n=$(ls "$D1/strategies"/*.rn 2>/dev/null | wc -l)
  expected=$(ls strategies/*.rn 2>/dev/null | wc -l)
  [ "$n" = "$expected" ] && [ "$n" -ge 12 ] || { echo "FAIL: built-in library seeded $n strategy files, expected $expected (>= 12)"; fail=1; }
  [ "$(opened_count)" = "0" ] || { echo "FAIL: a browser was opened with no display"; fail=1; }
else
  echo "FAIL: the desk did not start with the default port busy"; tail -5 "$D1.log"; fail=1
fi
{ kill -9 "$pid"; wait "$pid"; } 2>/dev/null
{ kill -9 "$HOLD"; wait "$HOLD"; } 2>/dev/null
sleep 0.5

# --- 2. explicit PORT with a display: the page is opened once, at the real address
D2="$WORK/d2"; mkdir -p "$D2"
start_desk "$D2" 9681 "DISPLAY=:0"
pid=$!
if wait_banner "$D2.log" "$pid"; then
  for _ in $(seq 1 20); do [ "$(opened_count)" -ge 1 ] && break; sleep 0.25; done
  if grep -qx 'http://127.0.0.1:9681' "$WORK/opened.txt" 2>/dev/null && [ "$(opened_count)" = "1" ]; then
    echo "ok: display present -> opened once at http://127.0.0.1:9681"
  else
    echo "FAIL: expected exactly one open of http://127.0.0.1:9681, got: $(cat "$WORK/opened.txt" 2>/dev/null)"; fail=1
  fi
else
  echo "FAIL: explicit PORT 9681 did not start"; tail -5 "$D2.log"; fail=1
fi
{ kill -9 "$pid"; wait "$pid"; } 2>/dev/null; sleep 0.5

# --- 3. every opt-out wins (display present, port 9682..9685)
port=9682
for case_ in "--no-open|" "--service|" "|OPENBELL_NO_OPEN=1" "|INVOCATION_ID=kit"; do
  args="${case_%%|*}"; env_="${case_#*|}"
  rm -f "$WORK/opened.txt"
  D="$WORK/o$port"; mkdir -p "$D"
  start_desk "$D" $port "DISPLAY=:0 $env_" $args
pid=$!
  if wait_banner "$D.log" "$pid"; then
    sleep 1
    if [ "$(opened_count)" = "0" ]; then echo "ok: opt-out [$args $env_] opens nothing"
    else echo "FAIL: opt-out [$args $env_] still opened the browser"; fail=1; fi
  else
    echo "FAIL: desk did not start for opt-out [$args $env_]"; tail -3 "$D.log"; fail=1
  fi
  { kill -9 "$pid"; wait "$pid"; } 2>/dev/null; sleep 0.5
  port=$((port+1))
done

# --- 4. an explicit PORT that is busy fails loudly, never falls back
node -e "require('net').createServer().listen(9686,'127.0.0.1')" &
HOLD2=$!; PIDS="$PIDS $HOLD2"; sleep 1
D4="$WORK/d4"; mkdir -p "$D4"
start_desk "$D4" 9686 ""
pid=$!
for _ in $(seq 1 40); do kill -0 "$pid" 2>/dev/null || break; sleep 0.25; done
if kill -0 "$pid" 2>/dev/null; then
  echo "FAIL: a busy explicit PORT kept running instead of failing"; fail=1; { kill -9 "$pid"; wait "$pid"; } 2>/dev/null
else
  wait "$pid" 2>/dev/null; code=$?
  if [ "$code" != "0" ] && grep -q 'PORT 9686 is not free' "$D4.log"; then
    echo "ok: busy explicit PORT 9686 fails loudly (exit $code)"
  else
    echo "FAIL: busy explicit PORT did not fail with the stated message (exit $code)"; tail -3 "$D4.log"; fail=1
  fi
fi
{ kill -9 "$HOLD2"; wait "$HOLD2"; } 2>/dev/null

[ $fail -eq 0 ] && echo "FIRST RUN OK: default port fallback, seeded library, opener skip rules, loud explicit PORT"
exit $fail
