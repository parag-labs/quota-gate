"""Benchmark harness for quota-gate.

Measures the two things that matter for a rate limiter on the hot path:

1. Decision throughput and latency - how fast try_acquire runs, and its P99/P99.9
   tail, since it sits in front of every model call.
2. Memory - the whole reason for the bucketed window. We compare the bounded
   bucketed engine against the exact per-event log as in-window traffic grows, and
   show that bucket memory stays flat while the log grows linearly.

Wall-clock timing here is real (these are tight Python loops), so absolute numbers
will vary by machine, but the shape - flat memory, stable tail latency - is the
point. Run: python bench/benchmark.py
"""

from __future__ import annotations

import gc
import json
import sys
import time
import tracemalloc
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python" / "src"))

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from limiter import Limiter
from rules import LimitRule

RESULTS = Path(__file__).resolve().parent / "results"
RESULTS.mkdir(exist_ok=True)


def pctile(data: list[float], p: float) -> float:
    s = sorted(data)
    k = min(len(s) - 1, round((p / 100) * (len(s) - 1)))
    return s[k]


def bench_decision_latency() -> dict:
    # One realistic multi-rule model: per-minute TPM+RPM, a daily cap, and a spend
    # cap - so each call evaluates several rules, like production.
    def fresh() -> Limiter:
        return Limiter([
            LimitRule("gpt-4o", window_seconds=60, max_tokens=10**9, max_requests=10**9, name="min"),
            LimitRule("gpt-4o", window_seconds=86_400, max_tokens=10**12, name="day"),
            LimitRule("gpt-4o", window_seconds=86_400, max_cost=10**9, name="spend"),
        ])

    # Pass 1: clean loop, no per-call instrumentation -> honest throughput.
    limiter = fresh()
    n = 200_000
    gc.disable()
    t = 0.0
    start = time.perf_counter()
    for _ in range(n):
        t += 0.001
        limiter.try_acquire("gpt-4o", tokens=100, cost=0.001, now=t)
    total = time.perf_counter() - start

    # Pass 2: a smaller sampled loop with per-call timing -> latency percentiles.
    limiter = fresh()
    latencies: list[float] = []
    t = 0.0
    for _ in range(50_000):
        t += 0.001
        s = time.perf_counter()
        limiter.try_acquire("gpt-4o", tokens=100, cost=0.001, now=t)
        latencies.append((time.perf_counter() - s) * 1e6)  # microseconds
    gc.enable()

    result = {
        "decisions": n,
        "seconds": round(total, 4),
        "decisions_per_sec": int(n / total),
        "p50_us": round(pctile(latencies, 50), 3),
        "p99_us": round(pctile(latencies, 99), 3),
        "p99_9_us": round(pctile(latencies, 99.9), 3),
        "note": "throughput from a clean loop; percentiles from a separately sampled loop with per-call timing overhead",
    }

    fig, ax = plt.subplots(figsize=(6.5, 4))
    labels = ["P50", "P99", "P99.9"]
    values = [result["p50_us"], result["p99_us"], result["p99_9_us"]]
    ax.bar(labels, values, color=["tab:green", "tab:orange", "tab:red"])
    for i, v in enumerate(values):
        ax.text(i, v, f"{v:.2f} us", ha="center", va="bottom")
    ax.set_ylabel("per-decision latency (microseconds)")
    ax.set_title(f"quota-gate: decision latency ({result['decisions_per_sec']:,}/sec)")
    ax.grid(True, axis="y", alpha=0.3)
    fig.tight_layout()
    fig.savefig(RESULTS / "decision_latency.png", dpi=110)
    plt.close(fig)
    return result


def _peak_kb(build) -> float:
    gc.collect()
    tracemalloc.start()
    obj = build()  # noqa: F841 - keep it alive during the measurement
    _, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    return peak / 1024


def bench_memory_bucket_vs_log() -> dict:
    volumes = [500, 1_000, 2_000, 4_000, 8_000]
    bucket_kb, log_kb = [], []

    for vol in volumes:
        def build_bucketed(v=vol):
            lim = Limiter([LimitRule("m", window_seconds=3600, max_requests=10**12, buckets_per_window=60)])
            for i in range(v):
                lim.try_acquire("m", now=i * (3600 / v))
            return lim

        def build_log(v=vol):
            lim = Limiter([LimitRule("m", window_seconds=3600, max_requests=10**12, precise=True)])
            for i in range(v):
                lim.try_acquire("m", now=i * (3600 / v))
            return lim

        bucket_kb.append(_peak_kb(build_bucketed))
        log_kb.append(_peak_kb(build_log))

    fig, ax = plt.subplots(figsize=(6.5, 4))
    ax.plot(volumes, log_kb, "s--", color="tab:red", label="exact per-event log")
    ax.plot(volumes, bucket_kb, "o-", color="tab:blue", label="bucketed window (default)")
    ax.set_xlabel("events inside the window")
    ax.set_ylabel("peak memory (KB)")
    ax.set_title("quota-gate: bounded bucket memory vs a growing log")
    ax.legend()
    ax.grid(True, alpha=0.3)
    fig.tight_layout()
    fig.savefig(RESULTS / "memory_bucket_vs_log.png", dpi=110)
    plt.close(fig)

    return {
        "events": volumes,
        "bucketed_kb": [round(x, 1) for x in bucket_kb],
        "log_kb": [round(x, 1) for x in log_kb],
    }


def main() -> None:
    summary = {
        "decision_latency": bench_decision_latency(),
        "memory": bench_memory_bucket_vs_log(),
    }
    (RESULTS / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
