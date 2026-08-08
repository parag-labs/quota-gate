"""The limiter surface: multi-window enforcement, precise back-off, headers,
cost rules, graceful fallback, and config loading."""

from __future__ import annotations

from limiter import Limiter, rate_limit_headers
from pricing import estimate_cost
from rules import LimitRule, Scope, rules_from_json


def test_allows_under_the_limit():
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_tokens=1000)])
    d = limiter.try_acquire("gpt-4o", tokens=100, now=0)
    assert d.allowed
    assert d.remaining["tokens"] == 900


def test_denies_over_token_cap_and_recovers_after_retry_after():
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_tokens=100)])
    assert limiter.try_acquire("gpt-4o", tokens=100, now=1000).allowed
    d = limiter.try_acquire("gpt-4o", tokens=1, now=1000)
    assert not d.allowed
    assert d.retry_after > 0
    # Waiting exactly retry_after must actually clear the call.
    assert limiter.try_acquire("gpt-4o", tokens=1, now=1000 + d.retry_after).allowed


def test_denies_over_request_cap():
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_requests=1)])
    assert limiter.try_acquire("gpt-4o", now=0).allowed
    assert not limiter.try_acquire("gpt-4o", now=0).allowed


def test_multiple_windows_minute_ok_but_daily_trips():
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_requests=5, name="per-min"),
        LimitRule("gpt-4o", window_seconds=86_400, max_requests=8, name="per-day"),
    ])
    # Five in the first minute fill the per-minute rule.
    for _ in range(5):
        assert limiter.try_acquire("gpt-4o", now=0).allowed
    assert not limiter.try_acquire("gpt-4o", now=0).allowed  # per-min tripped

    # Advance past the minute window: three more are fine (day allows 8 total)...
    for i in range(3):
        assert limiter.try_acquire("gpt-4o", now=61 + i).allowed
    # ...the ninth of the day trips the daily rule.
    d = limiter.try_acquire("gpt-4o", now=61 + 3)
    assert not d.allowed
    assert d.tripped_rule.name == "per-day"


def test_retry_after_is_precise_in_exact_mode():
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_requests=1, precise=True)])
    limiter.try_acquire("gpt-4o", now=1000)
    d = limiter.try_acquire("gpt-4o", now=1000)
    # Event at 1000 leaves the 60s window at 1060 -> retry_after == 60.
    assert abs(d.retry_after - 60) < 1e-6


def test_headers_render_backpressure():
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_requests=1, max_tokens=1000)])
    limiter.try_acquire("gpt-4o", tokens=10, now=0)
    d = limiter.try_acquire("gpt-4o", tokens=10, now=0)
    headers = rate_limit_headers(d)
    assert "Retry-After" in headers
    assert headers["X-RateLimit-Limit-Requests"] == "1"
    assert headers["X-RateLimit-Limit-Tokens"] == "1000"


def test_cost_based_rule_caps_spend():
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_cost=1.0)])
    # 200k output tokens on gpt-4o = 200000/1e6 * 10 = $2.00 -> over the $1 cap.
    cost = estimate_cost("gpt-4o", input_tokens=0, output_tokens=200_000)
    d = limiter.try_acquire("gpt-4o", cost=cost, now=0)
    assert not d.allowed
    assert d.tripped_rule is not None


def test_fallback_is_suggested_and_used_on_denial():
    limiter = Limiter(
        [
            LimitRule("gpt-4o", window_seconds=60, max_requests=0),  # always full
            LimitRule("gpt-4o-mini", window_seconds=60, max_requests=100),
        ],
        fallbacks={"gpt-4o": "gpt-4o-mini"},
    )
    d = limiter.try_acquire("gpt-4o", now=0)
    assert not d.allowed
    assert d.suggested_fallback == "gpt-4o-mini"

    used_decision, chosen = limiter.acquire_or_fallback("gpt-4o", now=0)
    assert used_decision.allowed
    assert chosen == "gpt-4o-mini"


def test_unknown_model_has_no_rules_and_is_allowed():
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_requests=1)])
    # A model with no configured rules passes through (fail-open by default).
    assert limiter.try_acquire("some-other-model", tokens=10_000, now=0).allowed


def test_rules_load_from_json():
    text = """
    { "rules": [
        { "model": "gpt-4o", "scope": "tenant", "window_seconds": 60, "max_requests": 2 }
    ] }
    """
    rules = rules_from_json(text)
    assert len(rules) == 1
    assert rules[0].scope is Scope.TENANT
    limiter = Limiter(rules)
    assert limiter.try_acquire("gpt-4o", tenant="acme", now=0).allowed
    assert limiter.try_acquire("gpt-4o", tenant="acme", now=0).allowed
    assert not limiter.try_acquire("gpt-4o", tenant="acme", now=0).allowed
