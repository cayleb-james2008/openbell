#!/usr/bin/env python3
"""Generate the slot-system fixture tape: 12 settled windows with a
tradeable snapshot 480s and 240s before each close (yes_ask 0.70 /
no_ask 0.32). The first three windows settle NO, so the favourite (buys
YES) takes three consecutive strikes, retires, and rotates; the rest
alternate YES/NO so no shape compounds a rigged record. Labelled
fixture data; never mixed into the collected tape."""
import json
import sys
from datetime import datetime, timedelta, timezone

BASE = datetime(2026, 11, 12, 0, 0, tzinfo=timezone.utc)


def iso(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def snap(close, fetched_epoch, result, ya, na):
    return {
        "fetched_at": fetched_epoch,
        "ticker": f"KXBTC15M-SLOT{int(close.timestamp())}",
        "event_ticker": "KXBTC15M-SLOT",
        "series": "KXBTC15M",
        "close_time": iso(close),
        "status": "settled" if result else "active",
        "result": result,
        "yes_bid": (ya - 0.02) if ya is not None else None,
        "yes_ask": ya,
        "no_bid": (na - 0.02) if na is not None else None,
        "no_ask": na,
        "last_price": None,
    }


RESULTS = ["no", "no", "no", "yes", "no", "yes", "no", "yes", "no", "yes", "no", "yes"]


def main(path):
    rows = []
    for i in range(12):
        close = BASE + timedelta(minutes=15 * i)
        epoch = int(close.timestamp())
        rows.append(snap(close, epoch - 480, "", 0.70, 0.32))
        rows.append(snap(close, epoch - 240, "", 0.70, 0.32))
        rows.append(snap(close, epoch + 60, RESULTS[i], None, None))
    with open(path, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    print(f"wrote {len(rows)} fixture snapshots to {path}")


if __name__ == "__main__":
    main(sys.argv[1])
