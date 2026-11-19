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
# Direct dependencies are ALLOWLISTED, not denylisted (round-2
# adversarial: a denylist waved socket2 + ed25519-dalek straight
# through — an unlisted crate is exactly the hole). The app's
# direct deps are rune + serde + serde_json, plus sha2 (checksums for the
# self-update in src/update.rs, added in 0.2.0 at the owner's request),
# nothing else, ever.
deps=$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f && /^[a-z]/ {split($0,a,"="); gsub(/ /,"",a[1]); print a[1]}' Cargo.toml | sort | tr '\n' ' ')
if [ "$deps" != "rune serde serde_json sha2 " ]; then
  echo "FAIL: Cargo.toml direct dependencies are [$deps], expected exactly [rune serde serde_json sha2]"; fail=1
fi
# The self-update (src/update.rs) is the second and last place a process
# may start: curl for the GitHub release check and download, the downloaded
# build run once with --version to prove it, and the relaunch after the
# user restarts (the relaunch has a Unix and a Windows form). Pinned to exactly those call sites, no request
# bodies or methods, and no host other than GitHub's API in the file.
nupd=$(grep -c 'Command::new' src/update.rs)
[ "$nupd" = "4" ] || { echo "FAIL: src/update.rs has $nupd Command::new call sites, expected exactly 4 (curl, --version proof, relaunch on Unix, relaunch on Windows)"; fail=1; }
if grep -n '"-X"\|"--data"\|"-d"\|"--upload-file"\|"-T"' src/update.rs >/dev/null; then
  echo "FAIL: HTTP method/body flags present in src/update.rs"; fail=1
fi
if grep -no 'https\?://[A-Za-z0-9.-]*' src/update.rs | grep -v 'https\?://$\|api.github.com\|127.0.0.1\|localhost\|example.com\|https://x$\|http://x$' | grep -q .; then
  echo "FAIL: src/update.rs names a host other than GitHub's"; fail=1
fi
# The process surface is pinned to this file's exact shape: the
# plain import line, and exactly two Command tokens in the whole
# source (the import + the collector's call). An aliased import
# (Command as Proc) adds a token and trips this — round 2 proved
# the substring count alone could not see it.
[ "$(grep -c 'use std::process::Command;' src/main.rs)" = "1" ] || { echo "FAIL: the Command import line is not the plain one"; fail=1; }
ntok=$(grep -o 'Command' src/main.rs | wc -l)
[ "$ntok" = "2" ] || { echo "FAIL: src/main.rs has $ntok Command tokens, expected exactly 2 (import + collector call)"; fail=1; }
[ $fail -eq 0 ] && echo "FENCE OK: no order-placement surface in source or binary; venue traffic is the collector's public GET only"
exit $fail
