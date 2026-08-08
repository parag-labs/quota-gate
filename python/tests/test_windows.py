"""The sliding-window engines: bounded-memory buckets vs the exact log."""

from __future__ import annotations

from store import COST, REQUESTS, TOKENS, _BucketWindow, _PreciseWindow


def test_bucket_memory_is_bounded_regardless_of_traffic():
    # 100s window, 1s buckets -> at most ~100 buckets even under heavy traffic.
    w = _BucketWindow(window=100, bucket=1)
    for ts in range(150):
        for _ in range(1000):  # 150k events total
            w.add(ts, tokens=10, requests=1, cost=0.0)
    assert len(w.buckets) <= 102


def test_bucket_counts_recent_and_drops_old():
    w = _BucketWindow(window=60, bucket=1)
    w.add(0, tokens=100, requests=1, cost=0.0)
    # Still inside the window.
    assert w.snapshot(30)[TOKENS] == 100
    # The event at t=0 has fully aged out by t=61.
    assert w.snapshot(61)[TOKENS] == 0


def test_bucket_prorates_the_straddling_edge():
    # One big bucket spanning [0,10); at now=65 the window [5,65] covers half of it.
    w = _BucketWindow(window=60, bucket=10)
    w.add(0, tokens=100, requests=0, cost=0.0)
    got = w.snapshot(65)[TOKENS]
    assert 40 <= got <= 60  # ~half, prorated


def test_precise_window_is_exact():
    w = _PreciseWindow(window=60)
    w.add(0.0, tokens=10, requests=1, cost=0.0)
    w.add(59.0, tokens=10, requests=1, cost=0.0)
    assert w.snapshot(59.5)[REQUESTS] == 2
    # First event drops at t>60.
    assert w.snapshot(60.5)[REQUESTS] == 1


def test_adjust_updates_a_recorded_entry():
    w = _BucketWindow(window=60, bucket=1)
    h = w.add(0, tokens=100, requests=1, cost=1.0)
    w.adjust(h, dt=-40, dr=0.0, dc=-0.4)
    assert w.snapshot(1)[TOKENS] == 60
    assert round(w.snapshot(1)[COST], 6) == 0.6
