"""Bounded-memory sliding-window counters behind a pluggable store.

The default engine keeps fixed-size time *buckets* per key, so memory is bounded
by ``window / bucket`` regardless of traffic - not one entry per request. A 24h
window with 60 buckets is 60 counters whether you serve 10 requests or 10 million.
The trailing (oldest) bucket is prorated by how much of it still lies inside the
window, so accuracy is tunable via ``buckets_per_window``.

A precise per-event log is available (``bucket_seconds <= 0``) for low-QPS callers
who want microsecond-exact windows and don't mind memory that grows with traffic.

``Store`` is the extension seam. ``InMemoryStore`` is the single-process default;
a distributed deployment implements the same interface over Redis (atomic INCR
plus TTL per bucket) so counters coordinate across replicas.
"""

from __future__ import annotations

from abc import ABC, abstractmethod

# Dimension indices shared across the codebase.
TOKENS = 0
REQUESTS = 1
COST = 2


class _BucketWindow:
    """Fixed-size time buckets. Memory is bounded to ~window/bucket entries."""

    __slots__ = ("bucket", "buckets", "window")

    def __init__(self, window: float, bucket: float) -> None:
        self.window = window
        self.bucket = max(bucket, 1e-9)
        self.buckets: dict[int, list[float]] = {}

    def _evict(self, now: float) -> None:
        lo = now - self.window
        for idx in list(self.buckets):
            if (idx + 1) * self.bucket <= lo:  # bucket entirely past the window
                del self.buckets[idx]

    def snapshot(self, now: float) -> tuple[float, float, float]:
        lo = now - self.window
        t = r = c = 0.0
        for idx, v in self.buckets.items():
            b_start = idx * self.bucket
            b_end = b_start + self.bucket
            if b_end <= lo:
                continue
            # Newest/interior buckets count fully; only the straddling oldest
            # bucket is prorated by the fraction of it still inside the window.
            frac = 1.0 if b_start >= lo else (b_end - lo) / self.bucket
            t += v[TOKENS] * frac
            r += v[REQUESTS] * frac
            c += v[COST] * frac
        return t, r, c

    def add(self, now: float, tokens: float, requests: float, cost: float) -> int:
        self._evict(now)
        idx = int(now // self.bucket)
        v = self.buckets.get(idx)
        if v is None:
            v = [0.0, 0.0, 0.0]
            self.buckets[idx] = v
        v[TOKENS] += tokens
        v[REQUESTS] += requests
        v[COST] += cost
        return idx

    def adjust(self, handle: int, dt: float, dr: float, dc: float) -> None:
        v = self.buckets.get(handle)
        if v is not None:
            v[TOKENS] += dt
            v[REQUESTS] += dr
            v[COST] += dc

    def time_to_free(self, now: float, over: float, dim: int) -> float:
        lo = now - self.window
        entries = []
        for idx, v in self.buckets.items():
            b_end = (idx + 1) * self.bucket
            if b_end <= lo or v[dim] <= 0:
                continue
            entries.append((b_end, v[dim]))
        entries.sort()
        freed = 0.0
        for b_end, amt in entries:
            freed += amt
            if freed >= over - 1e-9:
                # This bucket fully exits the window at b_end + window.
                return max(0.0, b_end + self.window - now)
        return self.window


class _PreciseWindow:
    """Exact per-event log. Memory grows with in-window traffic."""

    __slots__ = ("events", "window")

    def __init__(self, window: float) -> None:
        self.window = window
        self.events: list[list[float]] = []  # [ts, tokens, requests, cost]

    def _evict(self, now: float) -> None:
        lo = now - self.window
        self.events = [e for e in self.events if e[0] > lo]

    def snapshot(self, now: float) -> tuple[float, float, float]:
        lo = now - self.window
        t = r = c = 0.0
        for e in self.events:
            if e[0] > lo:
                t += e[1]
                r += e[2]
                c += e[3]
        return t, r, c

    def add(self, now: float, tokens: float, requests: float, cost: float) -> list[float]:
        self._evict(now)
        e = [now, tokens, requests, cost]
        self.events.append(e)
        return e

    def adjust(self, handle: list[float], dt: float, dr: float, dc: float) -> None:
        handle[1] += dt
        handle[2] += dr
        handle[3] += dc

    def time_to_free(self, now: float, over: float, dim: int) -> float:
        lo = now - self.window
        live = sorted((e for e in self.events if e[0] > lo), key=lambda e: e[0])
        freed = 0.0
        col = dim + 1  # events store ts at index 0, dims shifted by one
        for e in live:
            freed += e[col]
            if freed >= over - 1e-9:
                return max(0.0, e[0] + self.window - now)
        return self.window


class Store(ABC):
    """The persistence seam. Implement over Redis for distributed enforcement."""

    @abstractmethod
    def snapshot(self, key, now: float, window: float, bucket: float) -> tuple[float, float, float]:
        ...

    @abstractmethod
    def add(self, key, now: float, window: float, bucket: float,
            tokens: float, requests: float, cost: float):
        ...

    @abstractmethod
    def adjust(self, key, handle, dt: float, dr: float, dc: float) -> None:
        ...

    @abstractmethod
    def time_to_free(self, key, now: float, window: float, bucket: float,
                     over: float, dim: int) -> float:
        ...

    @abstractmethod
    def concurrency(self, key) -> int:
        ...

    @abstractmethod
    def try_add_concurrency(self, key, limit: int) -> bool:
        ...

    @abstractmethod
    def release_concurrency(self, key) -> None:
        ...


class InMemoryStore(Store):
    """Single-process default. One window object and one counter per key."""

    def __init__(self) -> None:
        self._windows: dict[object, object] = {}
        self._concurrency: dict[object, int] = {}

    def _window(self, key, window: float, bucket: float):
        w = self._windows.get(key)
        if w is None:
            w = _PreciseWindow(window) if bucket <= 0 else _BucketWindow(window, bucket)
            self._windows[key] = w
        return w

    def snapshot(self, key, now, window, bucket):
        return self._window(key, window, bucket).snapshot(now)

    def add(self, key, now, window, bucket, tokens, requests, cost):
        return self._window(key, window, bucket).add(now, tokens, requests, cost)

    def adjust(self, key, handle, dt, dr, dc):
        w = self._windows.get(key)
        if w is not None:
            w.adjust(handle, dt, dr, dc)

    def time_to_free(self, key, now, window, bucket, over, dim):
        return self._window(key, window, bucket).time_to_free(now, over, dim)

    def concurrency(self, key):
        return self._concurrency.get(key, 0)

    def try_add_concurrency(self, key, limit):
        cur = self._concurrency.get(key, 0)
        if cur >= limit:
            return False
        self._concurrency[key] = cur + 1
        return True

    def release_concurrency(self, key):
        cur = self._concurrency.get(key, 0)
        if cur > 0:
            self._concurrency[key] = cur - 1
