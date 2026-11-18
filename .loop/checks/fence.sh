#!/bin/bash
# C2: the hard fence, mechanically. No order-placement surface may
# exist in the source or the built binary: no order endpoints, no
# order-placement identifiers, no signed trading requests, no POST to
# the venue. The only venue traffic this app may generate is the
# collector's unauthenticated GET of public market data.
set -u
cd "$(dirname "$0")/../.."
export PATH="$HOME/.rust/bin:$PATH"
[ -x ./target/debug/openbell-rs ] || cargo build >/dev/null 2>&1

fail=0
hits=$(grep -rn '/orders\|place_order\|create_order\|submit_order\|signed_order\|order_id' src/ strategies/ assets/ 2>/dev/null)
if [ -n "$hits" ]; then echo "FAIL: order-placement identifiers in source:"; echo "$hits"; fail=1; fi

curl_lines=$(grep -n '"curl"' src/main.rs)
if echo "$curl_lines" | grep -q .; then
  # Every curl invocation must be the collector's plain GET: no method
  # override, no request body flags anywhere in the file's curl args.
  if grep -n 'args(\["-s", "-m", "25"' src/main.rs >/dev/null; then :; else
    echo "FAIL: the curl invocation is not the known collector GET"; fail=1
  fi
fi
if grep -rn '"-X"\|"--data"\|"-d"' src/main.rs >/dev/null; then
  echo "FAIL: HTTP method/body flags present in source"; fail=1
fi
if grep -rn 'TcpStream::connect\|UdpSocket' src/main.rs >/dev/null; then
  echo "FAIL: outbound socket code present (only TcpListener may exist)"; fail=1
fi
if strings target/debug/openbell-rs | grep -q '/orders'; then
  echo "FAIL: '/orders' string present in the built binary"; fail=1
fi
if strings target/debug/openbell-rs | grep -q 'place_order\|create_order'; then
  echo "FAIL: order-placement identifier present in the built binary"; fail=1
fi
[ $fail -eq 0 ] && echo "FENCE OK: no order-placement surface in source or binary; venue traffic is the collector's public GET only"
exit $fail
