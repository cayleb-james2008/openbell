#!/usr/bin/env python3
"""Generate a deterministic fixture forward tape for the shadow scorer.

120 settled KXBTC15M-style windows. In each window the YES ask cycles
0.63..0.81 and YES settles in 8 of every 10 windows, so the favourite-late
Rune strategy (buy YES <= 0.85 late) earns on paper in both chronological
halves, while longshot-late (buy NO <= 0.40, true in every window here)
enters all 120 and loses — a formal NOT PROVEN. This fixture exists so
the promotion standard can be exercised end-to-end at its real settings
(100 forward windows) — it is labelled fixture data everywhere it is used
and is never mixed into the collected tape.
"""
import json
import sys
from datetime import datetime, timedelta, timezone

BASE = datetime(2026, 11, 10, 0, 0, tzinfo=timezone.utc)


def iso(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def main(path):
    rows = []
    for i in range(120):
        close = BASE + timedelta(minutes=15 * i)
        close_epoch = int(close.timestamp())
        yes_ask = round(0.63 + (i % 10) * 0.02, 2)
        yes_bid = round(yes_ask - 0.02, 2)
        no_ask = round(1.0 - yes_bid, 2)
        no_bid = round(1.0 - yes_ask, 2)
        result = "yes" if (i % 10) < 8 else "no"
        ticker = f"KXBTC15M-FIX{i:03d}"
        rows.append({
            "fetched_at": close_epoch - 240,
            "ticker": ticker,
            "event_ticker": f"KXBTC15M-FIX{i:03d}",
            "series": "KXBTC15M",
            "close_time": iso(close),
            "status": "active",
            "result": "",
            "yes_bid": yes_bid,
            "yes_ask": yes_ask,
            "no_bid": no_bid,
            "no_ask": no_ask,
            "last_price": yes_bid,
        })
        rows.append({
            "fetched_at": close_epoch + 60,
            "ticker": ticker,
            "event_ticker": f"KXBTC15M-FIX{i:03d}",
            "series": "KXBTC15M",
            "close_time": iso(close),
            "status": "settled",
            "result": result,
            "yes_bid": None,
            "yes_ask": None,
            "no_bid": None,
            "no_ask": None,
            "last_price": 1.0 if result == "yes" else 0.0,
        })
    with open(path, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    print(f"wrote {len(rows)} snapshots ({len(rows)//2} windows) to {path}")


if __name__ == "__main__":
    main(sys.argv[1])
