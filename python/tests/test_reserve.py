"""Reserve on the estimate, then reconcile to the actual - the standard meter model."""

from __future__ import annotations

from limiter import Limiter
from rules import LimitRule, Scope


def _limiter():
    return Limiter([LimitRule("gpt-4o", window_seconds=60, max_tokens=1000, scope=Scope.GLOBAL)])


def test_reserve_consumes_the_estimated_headroom():
    limiter = _limiter()
    decision, res = limiter.reserve("gpt-4o", tokens=800, now=0)
    assert decision.allowed and res is not None
    # 800 is reserved, so a 300-token call no longer fits.
    assert not limiter.try_acquire("gpt-4o", tokens=300, now=0).allowed


def test_commit_reconciles_down_and_frees_capacity():
    limiter = _limiter()
    _, res = limiter.reserve("gpt-4o", tokens=800, now=0)
    # The call actually used only 100 tokens.
    limiter.commit(res, actual_tokens=100)
    # 900 tokens of headroom are back.
    assert limiter.try_acquire("gpt-4o", tokens=300, now=0).allowed


def test_commit_reconciles_up():
    limiter = _limiter()
    _, res = limiter.reserve("gpt-4o", tokens=100, now=0)
    limiter.commit(res, actual_tokens=900)
    # Now 900 are used; a 200-token call trips the cap.
    assert not limiter.try_acquire("gpt-4o", tokens=200, now=0).allowed


def test_refund_returns_everything_on_a_failed_call():
    limiter = _limiter()
    _, res = limiter.reserve("gpt-4o", tokens=900, now=0)
    limiter.refund(res)
    assert limiter.try_acquire("gpt-4o", tokens=1000, now=0).allowed


def test_double_commit_is_a_noop():
    limiter = _limiter()
    _, res = limiter.reserve("gpt-4o", tokens=100, now=0)
    limiter.commit(res, actual_tokens=500)
    limiter.commit(res, actual_tokens=999)  # ignored
    # Used is 500, so 400 still fits but 600 does not.
    assert limiter.try_acquire("gpt-4o", tokens=400, now=0).allowed
    assert not limiter.try_acquire("gpt-4o", tokens=600, now=0).allowed
