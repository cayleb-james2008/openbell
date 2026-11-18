#!/bin/bash
# C2: the hard fence, mechanically. No order-placement surface may
# exist in the source or the built binary: no order endpoints, no
# order-placement identifiers, no signed trading requests, no POST to
# the venue. The only venue traffic this app may generate is the
# collector's unauthenticated GET of public market data.
set -u
cd "$(dirname "$0")/../.."
export PATH="$HOME/.rust/bin:$PATH"
cargo build >/dev/null 2>&1 || { echo "FAIL: cargo build failed"; exit 1; }

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
for ident in place_order create_order submit_order signed_order order_id; do
  if strings target/debug/openbell-rs | grep -q "$ident"; then
    echo "FAIL: '$ident' present in the built binary"; fail=1
  fi
done
# Exactly one process call site may exist in the whole program — the
# collector's curl. A second Command::new is where any other network
# or signing tool would have to enter.
ncmd=$(grep -c 'Command::new' src/main.rs)
[ "$ncmd" = "1" ] || { echo "FAIL: src/main.rs has $ncmd Command::new call sites, expected exactly 1 (the collector's curl)"; fail=1; }
# Direct dependencies are whitelisted: no networking, HTTP, or
# signing crate may be added quietly. (Transitive deps of rune are
# not the app's choice; direct ones are.)
if grep -Eq 'reqwest|hyper|tokio|openssl|ring|rsa|hmac|sha2|websocket|tungstenite|ureq|curl' Cargo.toml; then
  echo "FAIL: a networking/signing crate appears in Cargo.toml dependencies"; fail=1
fi
[ $fail -eq 0 ] && echo "FENCE OK: no order-placement surface in source or binary; venue traffic is the collector's public GET only"
exit $fail
