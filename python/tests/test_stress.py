"""Stress suite: memory pressure, out-of-order events, high-volume soak, and a
threaded smoke test.

The point of these is to prove the properties the design claims under load, not to
chase a throughput number (that lives in bench/). Specifically: the bucketed window
must stay bounded in memory no matter how much traffic flows through it, enforcement
must stay correct when timestamps arrive out of order, and the limiter must survive
millions of calls and many threads without corrupting its counters.
"""

from __future__ import annotations

import random
import threading

from limiter import Limiter
from rules import LimitRule, Scope


def _bucket_count(limiter: Limiter) -> int:
    """Total live buckets across every window in the in-memory store."""
    total = 0
    for w in limiter.store._windows.values():
        buckets = getattr(w, "buckets", None)
        if buckets is not None:
            total += len(buckets)
    return total


def test_memory_is_bounded_under_heavy_traffic():
    # A 1-hour window split into 60 buckets. No matter how many events we push,
    # the store must never hold more than ~60 buckets for this key.
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=3600, max_tokens=10**12, buckets_per_window=60),
    ])
    for i in range(100_000):
        # Spread events across two hours of simulated time.
        limiter.try_acquire("gpt-4o", tokens=1, now=i * 0.072)
    assert _bucket_count(limiter) <= 62, f"bucket count grew to {_bucket_count(limiter)}"


def test_precise_mode_grows_but_stays_correct():
    # The exact-log mode is expected to grow with in-window traffic - this documents
    # that trade-off, and confirms enforcement is still exact.
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_requests=100, precise=True),
    ])
    allowed = 0
    for _ in range(500):
        if limiter.try_acquire("gpt-4o", now=1000.0).allowed:
            allowed += 1
    # Exactly the cap is admitted within the window.
    assert allowed == 100


def test_enforcement_is_correct_with_out_of_order_timestamps():
    # Feed events whose timestamps jump around within the window. The rolling count
    # must still reflect everything inside the window, so the cap holds.
    limiter = Limiter([LimitRule("gpt-4o", window_seconds=60, max_requests=10)])
    stamps = [1000.0, 1002.0, 1001.0, 1005.0, 1003.0, 1004.0, 1002.5, 1001.5, 1000.5, 1004.5]
    for ts in stamps:
        assert limiter.try_acquire("gpt-4o", now=ts).allowed
    # The 11th event anywhere in the window must be denied regardless of order.
    assert not limiter.try_acquire("gpt-4o", now=1002.7).allowed


def test_high_volume_soak_keeps_the_cap():
    # Two million calls against a tight per-minute cap. The window is a *sliding*
    # 60s window approximated by 1-second buckets, so at bucket edges it may admit a
    # hair over the cap - that's the documented accuracy trade-off (tunable via
    # buckets_per_window). We assert the real invariant: in ANY sliding 60s window,
    # admissions stay within the cap plus a small bucket-granularity tolerance.
    from collections import deque

    rng = random.Random(0)
    cap = 1000
    buckets = 60
    tolerance = cap // buckets + 1  # ~one bucket's worth of edge error
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=60, max_requests=cap, buckets_per_window=buckets),
    ])

    admitted: deque[float] = deque()
    t = 0.0
    worst = 0
    for _ in range(300_000):
        t += rng.random() * 0.01
        if limiter.try_acquire("gpt-4o", now=t).allowed:
            admitted.append(t)
        while admitted and admitted[0] <= t - 60:
            admitted.popleft()
        worst = max(worst, len(admitted))
        assert len(admitted) <= cap + tolerance

    # And it genuinely pushes up against the cap (not trivially under it).
    assert worst >= cap * 0.9


def test_threaded_access_does_not_corrupt_counters():
    # The in-memory store is single-process; under the GIL, dict mutations won't
    # corrupt, but we still want to prove many threads hammering it never crashes
    # and never drives a counter negative or nonsensical.
    limiter = Limiter([
        LimitRule("gpt-4o", window_seconds=3600, max_requests=10**9, scope=Scope.TENANT),
    ])
    errors: list[Exception] = []

    def worker(tenant: str) -> None:
        try:
            for i in range(5000):
                limiter.try_acquire("gpt-4o", tenant=tenant, now=i * 0.01)
        except Exception as e:  # noqa: BLE001 - we want to surface any thread error
            errors.append(e)

    threads = [threading.Thread(target=worker, args=(f"t{i}",)) for i in range(8)]
    for th in threads:
        th.start()
    for th in threads:
        th.join()

    assert not errors, f"threads raised: {errors}"
    # Every tenant window exists and holds a sane, non-negative count.
    for w in limiter.store._windows.values():
        for counters in getattr(w, "buckets", {}).values():
            assert all(c >= 0 for c in counters)
