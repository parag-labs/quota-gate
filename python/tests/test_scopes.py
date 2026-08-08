"""Hierarchical scopes: global fleet, per-tenant, per-user - all enforced together."""

from __future__ import annotations

from limiter import Limiter
from rules import LimitRule, Scope


def test_tenant_scope_isolates_tenants():
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_requests=2, scope=Scope.TENANT),
    ])
    assert limiter.try_acquire("gpt-4o", tenant="acme", now=0).allowed
    assert limiter.try_acquire("gpt-4o", tenant="acme", now=0).allowed
    # acme is now full...
    assert not limiter.try_acquire("gpt-4o", tenant="acme", now=0).allowed
    # ...but a different tenant has its own budget.
    assert limiter.try_acquire("gpt-4o", tenant="globex", now=0).allowed


def test_user_scope_isolates_users_within_a_tenant():
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_requests=1, scope=Scope.USER),
    ])
    assert limiter.try_acquire("gpt-4o", tenant="acme", user="ann", now=0).allowed
    assert not limiter.try_acquire("gpt-4o", tenant="acme", user="ann", now=0).allowed
    # Same tenant, different user.
    assert limiter.try_acquire("gpt-4o", tenant="acme", user="bob", now=0).allowed


def test_global_scope_is_shared_across_everyone():
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_requests=2, scope=Scope.GLOBAL),
    ])
    assert limiter.try_acquire("gpt-4o", tenant="acme", user="ann", now=0).allowed
    assert limiter.try_acquire("gpt-4o", tenant="globex", user="bob", now=0).allowed
    # The fleet cap is hit no matter who is calling.
    assert not limiter.try_acquire("gpt-4o", tenant="new", user="cat", now=0).allowed


def test_toughest_applicable_rule_wins():
    # A generous per-tenant cap but a tiny global fleet cap.
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_requests=1000, scope=Scope.TENANT, name="tenant"),
        LimitRule("gpt-4o", window_seconds=60, max_requests=1, scope=Scope.GLOBAL, name="fleet"),
    ])
    assert limiter.try_acquire("gpt-4o", tenant="acme", now=0).allowed
    d = limiter.try_acquire("gpt-4o", tenant="acme", now=0)
    assert not d.allowed
    assert d.tripped_rule.name == "fleet"
    assert d.scope is Scope.GLOBAL
