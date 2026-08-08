"""Rule definitions: what to limit, over which window, at which scope."""

from __future__ import annotations

import json
from dataclasses import dataclass
from enum import Enum


class Scope(str, Enum):
    """Where a limit is enforced. A request is keyed differently per scope so the
    same rule isolates one global fleet, one tenant, or one user."""

    GLOBAL = "global"
    TENANT = "tenant"
    USER = "user"


@dataclass(frozen=True)
class LimitRule:
    """One provider-style limit. Any subset of the max_* dimensions may be set;
    a request must stay under every one that is."""

    model: str
    window_seconds: float
    max_tokens: float | None = None
    max_requests: float | None = None
    max_cost: float | None = None
    max_concurrent: int | None = None
    scope: Scope = Scope.GLOBAL
    buckets_per_window: int = 60
    precise: bool = False
    name: str | None = None

    @property
    def bucket_seconds(self) -> float:
        """0 selects the exact per-event log; otherwise window/buckets_per_window."""
        if self.precise:
            return 0.0
        return self.window_seconds / max(1, self.buckets_per_window)

    @property
    def label(self) -> str:
        return self.name or f"{self.model}:{self.scope.value}:{int(self.window_seconds)}s"


def rules_from_json(text: str) -> list[LimitRule]:
    doc = json.loads(text)
    out: list[LimitRule] = []
    for row in doc["rules"]:
        out.append(
            LimitRule(
                model=row["model"],
                window_seconds=float(row["window_seconds"]),
                max_tokens=row.get("max_tokens"),
                max_requests=row.get("max_requests"),
                max_cost=row.get("max_cost"),
                max_concurrent=row.get("max_concurrent"),
                scope=Scope(row.get("scope", "global")),
                buckets_per_window=int(row.get("buckets_per_window", 60)),
                precise=bool(row.get("precise", False)),
                name=row.get("name"),
            )
        )
    return out
