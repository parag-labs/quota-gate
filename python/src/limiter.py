"""The gate: evaluate provider-style limits before a call, reserve/reconcile
tokens, cap concurrency, and emit standard back-pressure signals.

Design mirrors how model-serving APIs meter in practice:

- Multiple limits per model enforced *together* (RPM + TPM + daily + cost).
- Hierarchical *scopes*: a request must clear the global fleet cap, its tenant's
  cap, and the user's cap - the toughest applicable rule wins.
- Reserve on estimated ``max_tokens``, then commit the actual usage (or refund a
  failed call), so a large requested completion counts against you immediately
  and is reconciled afterwards.
- Concurrency slots cap in-flight requests per scope.
- On denial, a precise ``retry_after`` and an optional cheaper-model fallback, so
  callers can degrade gracefully instead of hard-failing.
"""

from __future__ import annotations

import math
import time
from dataclasses import dataclass, field
from typing import Self

from rules import LimitRule, Scope
from store import COST, REQUESTS, TOKENS, InMemoryStore, Store


@dataclass
class Decision:
    allowed: bool
    model: str
    retry_after: float = 0.0
    tripped_rule: LimitRule | None = None
    scope: Scope | None = None
    remaining: dict[str, float] = field(default_factory=dict)
    suggested_fallback: str | None = None


@dataclass
class Reservation:
    model: str
    tokens: float
    cost: float
    handles: list  # [(key, handle), ...]
    committed: bool = False


@dataclass
class Slot:
    limiter: Limiter
    keys: list
    ok: bool = True
    tripped_rule: LimitRule | None = None

    def release(self) -> None:
        for k in self.keys:
            self.limiter.store.release_concurrency(k)
        self.keys = []

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *exc) -> None:
        self.release()


class Limiter:
    def __init__(self, rules, store: Store | None = None, clock=None, fallbacks=None) -> None:
        self.rules: list[LimitRule] = list(rules)
        self.store: Store = store or InMemoryStore()
        self.clock = clock or time.time
        self.fallbacks: dict[str, str] = dict(fallbacks or {})
        self._by_model: dict[str, list[tuple[int, LimitRule]]] = {}
        for i, r in enumerate(self.rules):
            self._by_model.setdefault(r.model, []).append((i, r))

    def _applicable(self, model: str) -> list[tuple[int, LimitRule]]:
        return self._by_model.get(model, []) + self._by_model.get("*", [])

    def _key(self, index: int, rule: LimitRule, tenant, user):
        if rule.scope is Scope.GLOBAL:
            return (index, "g")
        if rule.scope is Scope.TENANT:
            return (index, "t", tenant)
        return (index, "u", tenant, user)

    def try_acquire(self, model: str, *, tokens: float = 0.0, cost: float = 0.0,
                    tenant=None, user=None, now=None, record: bool = True) -> Decision:
        now = self.clock() if now is None else now
        rules = self._applicable(model)

        worst: tuple[float, LimitRule] | None = None
        for i, r in rules:
            if r.max_tokens is None and r.max_requests is None and r.max_cost is None:
                continue  # e.g. a concurrency-only rule
            key = self._key(i, r, tenant, user)
            used_t, used_r, used_c = self.store.snapshot(key, now, r.window_seconds, r.bucket_seconds)
            breached = []
            if r.max_tokens is not None and used_t + tokens > r.max_tokens + 1e-9:
                breached.append((TOKENS, used_t + tokens - r.max_tokens))
            if r.max_requests is not None and used_r + 1 > r.max_requests + 1e-9:
                breached.append((REQUESTS, used_r + 1 - r.max_requests))
            if r.max_cost is not None and used_c + cost > r.max_cost + 1e-9:
                breached.append((COST, used_c + cost - r.max_cost))
            if breached:
                ra = 0.0
                for dim, over in breached:
                    ra = max(ra, self.store.time_to_free(
                        key, now, r.window_seconds, r.bucket_seconds, over, dim))
                if worst is None or ra > worst[0]:
                    worst = (ra, r)

        if worst is not None:
            ra, rule = worst
            return Decision(False, model, retry_after=ra, tripped_rule=rule, scope=rule.scope,
                            suggested_fallback=self.fallbacks.get(model))

        if record:
            for i, r in rules:
                if r.max_tokens is None and r.max_requests is None and r.max_cost is None:
                    continue
                key = self._key(i, r, tenant, user)
                self.store.add(key, now, r.window_seconds, r.bucket_seconds, tokens, 1.0, cost)

        return Decision(True, model, remaining=self._remaining(model, tenant, user, now))

    def _remaining(self, model, tenant, user, now) -> dict[str, float]:
        rem: dict[str, float] = {}

        def tighten(name, value):
            rem[name] = min(rem.get(name, float("inf")), value)

        for i, r in self._applicable(model):
            key = self._key(i, r, tenant, user)
            used_t, used_r, used_c = self.store.snapshot(key, now, r.window_seconds, r.bucket_seconds)
            if r.max_tokens is not None:
                tighten("tokens", r.max_tokens - used_t)
            if r.max_requests is not None:
                tighten("requests", r.max_requests - used_r)
            if r.max_cost is not None:
                tighten("cost", r.max_cost - used_c)
        return {k: max(0.0, v) for k, v in rem.items()}

    # ---- reserve -> commit / refund ----

    def reserve(self, model: str, *, tokens: float = 0.0, cost: float = 0.0,
                tenant=None, user=None, now=None) -> tuple[Decision, Reservation | None]:
        now = self.clock() if now is None else now
        decision = self.try_acquire(model, tokens=tokens, cost=cost, tenant=tenant,
                                    user=user, now=now, record=False)
        if not decision.allowed:
            return decision, None
        handles = []
        for i, r in self._applicable(model):
            if r.max_tokens is None and r.max_requests is None and r.max_cost is None:
                continue
            key = self._key(i, r, tenant, user)
            h = self.store.add(key, now, r.window_seconds, r.bucket_seconds, tokens, 1.0, cost)
            handles.append((key, h))
        return decision, Reservation(model=model, tokens=tokens, cost=cost, handles=handles)

    def commit(self, reservation: Reservation, *, actual_tokens=None, actual_cost=None) -> None:
        if reservation.committed:
            return
        dt = 0.0 if actual_tokens is None else actual_tokens - reservation.tokens
        dc = 0.0 if actual_cost is None else actual_cost - reservation.cost
        for key, handle in reservation.handles:
            self.store.adjust(key, handle, dt, 0.0, dc)
        reservation.tokens += dt
        reservation.cost += dc
        reservation.committed = True

    def refund(self, reservation: Reservation) -> None:
        if reservation.committed:
            return
        for key, handle in reservation.handles:
            self.store.adjust(key, handle, -reservation.tokens, -1.0, -reservation.cost)
        reservation.handles = []
        reservation.committed = True

    # ---- concurrency slots ----

    def acquire_slot(self, model: str, *, tenant=None, user=None) -> Slot:
        acquired = []
        for i, r in self._applicable(model):
            if r.max_concurrent is None:
                continue
            key = ("conc",) + self._key(i, r, tenant, user)
            if not self.store.try_add_concurrency(key, r.max_concurrent):
                for k in acquired:
                    self.store.release_concurrency(k)
                return Slot(self, [], ok=False, tripped_rule=r)
            acquired.append(key)
        return Slot(self, acquired, ok=True)

    # ---- graceful degradation ----

    def acquire_or_fallback(self, model: str, **kwargs) -> tuple[Decision, str]:
        decision = self.try_acquire(model, **kwargs)
        if decision.allowed:
            return decision, model
        fallback = self.fallbacks.get(model)
        if fallback:
            alt = self.try_acquire(fallback, **kwargs)
            if alt.allowed:
                return alt, fallback
        return decision, model


def rate_limit_headers(decision: Decision, rule: LimitRule | None = None) -> dict[str, str]:
    """Render a decision as the ``X-RateLimit-*`` / ``Retry-After`` headers real
    providers return, so you can pass back-pressure straight to your caller."""
    rule = rule or decision.tripped_rule
    headers: dict[str, str] = {}
    if decision.retry_after > 0:
        headers["Retry-After"] = str(math.ceil(decision.retry_after))
    if rule is not None:
        if rule.max_requests is not None:
            headers["X-RateLimit-Limit-Requests"] = str(int(rule.max_requests))
        if rule.max_tokens is not None:
            headers["X-RateLimit-Limit-Tokens"] = str(int(rule.max_tokens))
    for name, value in (decision.remaining or {}).items():
        headers[f"X-RateLimit-Remaining-{name.capitalize()}"] = str(int(value))
    return headers
