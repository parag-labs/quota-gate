"""Concurrency slots: cap the number of in-flight requests per scope."""

from __future__ import annotations

from limiter import Limiter
from rules import LimitRule, Scope


def _limiter():
    return Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_concurrent=2, scope=Scope.TENANT),
    ])


def test_slots_cap_in_flight_requests():
    limiter = _limiter()
    a = limiter.acquire_slot("gpt-4o", tenant="acme")
    b = limiter.acquire_slot("gpt-4o", tenant="acme")
    c = limiter.acquire_slot("gpt-4o", tenant="acme")
    assert a.ok and b.ok
    assert not c.ok
    assert c.tripped_rule is not None


def test_releasing_a_slot_frees_capacity():
    limiter = _limiter()
    a = limiter.acquire_slot("gpt-4o", tenant="acme")
    limiter.acquire_slot("gpt-4o", tenant="acme")
    assert not limiter.acquire_slot("gpt-4o", tenant="acme").ok
    a.release()
    assert limiter.acquire_slot("gpt-4o", tenant="acme").ok


def test_slot_is_a_context_manager():
    limiter = _limiter()
    with limiter.acquire_slot("gpt-4o", tenant="acme"), limiter.acquire_slot("gpt-4o", tenant="acme"):
        assert not limiter.acquire_slot("gpt-4o", tenant="acme").ok
    # Both context slots released on exit.
    assert limiter.acquire_slot("gpt-4o", tenant="acme").ok


def test_concurrency_is_scoped_per_tenant():
    limiter = _limiter()
    limiter.acquire_slot("gpt-4o", tenant="acme")
    limiter.acquire_slot("gpt-4o", tenant="acme")
    assert not limiter.acquire_slot("gpt-4o", tenant="acme").ok
    # A different tenant has its own concurrency budget.
    assert limiter.acquire_slot("gpt-4o", tenant="globex").ok
