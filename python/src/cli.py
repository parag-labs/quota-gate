"""Tiny demo: load a limit config and replay a usage stream, reporting how many
calls the gate would allow vs shed. Not the product - just a way to eyeball it.

Usage:
    python src/cli.py limits.sample.json usage.jsonl

Each usage line is a JSON object: {"model": "...", "tokens": 1200, "tenant": "acme",
"user": "parag", "ts": 1712345678.0}. ts is optional (defaults to a monotonic clock).
"""

from __future__ import annotations

import json
import sys

from limiter import Limiter, rate_limit_headers
from rules import rules_from_json


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(__doc__)
        return 2
    with open(argv[1], encoding="utf-8") as fh:
        rules = rules_from_json(fh.read())
    limiter = Limiter(rules)

    allowed = denied = 0
    t = 0.0
    with open(argv[2], encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            t = float(row.get("ts", t + 0.001))
            d = limiter.try_acquire(
                row["model"],
                tokens=float(row.get("tokens", 0)),
                cost=float(row.get("cost", 0)),
                tenant=row.get("tenant"),
                user=row.get("user"),
                now=t,
            )
            if d.allowed:
                allowed += 1
            else:
                denied += 1
                headers = rate_limit_headers(d)
                print(f"deny {row['model']:<16} rule={d.tripped_rule.label} "
                      f"retry_after={d.retry_after:.1f}s {headers}")

    print(f"\nallowed={allowed} denied={denied}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
