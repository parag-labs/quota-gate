# quota-gate: design, trade-offs, and non-goals

Status: accepted
Author: Parag Sawant

Why quota-gate looks the way it does. A rate limiter sits in front of every model
call you make, so two things matter more than anything: it has to be correct under
messy real-world timing, and it can't become the bottleneck or the memory hog it's
meant to protect you from. This document is about the choices that fall out of that.

## Problem and goals

Providers cap you per model on several axes at once - requests per minute, tokens
per minute, often a daily cap, sometimes a spend cap - and enforce those per org,
per project, per key. Hit any one and you get a `429`. The goal is to mirror those
limits on your side and decide *before* you send, so you shape traffic instead of
discovering the cap in production. Concretely:

1. Many limits per model, enforced together, across global / tenant / user scopes.
2. Correct enforcement even when calls arrive out of order.
3. Bounded, predictable memory regardless of traffic volume.
4. A pluggable store so the same logic works single-process or Redis-backed.

## Key design decision: bucketed sliding window as the default

A sliding-window limiter has to answer "how much happened in the last W seconds?"
The exact way is to keep every event's timestamp and sum the ones still in the
window. That's precise but its memory grows with in-window traffic - at high QPS on
a long window (a daily cap!) that's a lot of little objects.

So the default engine buckets time: a window of W seconds split into K fixed
buckets, each holding just a running count. Memory is then O(K) - bounded - no
matter how many events flow through. The trailing (oldest) bucket is prorated by how
much of it still lies inside the window, so the count stays accurate to within one
bucket's granularity.

**The trade-off, stated plainly:** at bucket edges the limiter can admit a hair over
the cap - up to about one bucket's worth. That's tunable with `buckets_per_window`
(more buckets = tighter, at more memory). This is a deliberate accuracy-for-memory
trade, and it's exactly what production limiters (and Redis-based ones) do. The
stress suite asserts the real invariant this creates: in any sliding window,
admissions stay within `cap + one-bucket tolerance`, never unbounded.

The exact per-event log is still available (`precise=True`) for callers who want
microsecond-exact windows at low QPS and don't mind the memory. The benchmarks show
both: bucket memory stays flat while the log grows linearly.

## Other decisions

- **Hierarchical scopes keyed by tuple.** A rule is enforced global (`model`),
  tenant (`tenant, model`), or user (`tenant, user, model`). A request must clear
  every rule that applies, and the toughest one wins - the decision reports which
  rule and scope tripped and the exact `retry_after`.
- **Reserve -> commit / refund.** You reserve on the estimated `max_tokens` up front
  (a big requested completion counts against you immediately), then reconcile to the
  actual usage or refund a failed call. This mirrors how the providers' own meters
  work and avoids briefly over-admitting on optimistic estimates.
- **Store is an interface, in-memory is the default.** The whole persistence surface
  is one small `Store`. The default is single-process; a Redis implementation (atomic
  INCR + TTL per bucket) satisfies the same interface for a horizontally-scaled fleet.

## Trade-offs and honest limitations

- **Single-process by default.** The in-memory store doesn't coordinate across
  replicas. Correct for one process; for a fleet you must back it with a shared store.
  Called out as the primary non-goal, with the interface built as the seam for it.
- **Not thread-safe by construction.** Under CPython's GIL the dict operations won't
  corrupt (the stress suite runs 8 threads to confirm no crash and no negative
  counters), but a real multi-threaded server should either shard limiters per worker
  or use the Redis store. I did not add locks to the hot path on purpose - they'd tax
  every call for a guarantee the intended deployments (per-worker or Redis) don't need.
- **Approximate at bucket edges.** See above; tunable, and the exact mode exists.

## Non-goals

- **Not a proxy or gateway.** It answers "may I send this?"; it doesn't make the call,
  retry, or queue for you. That's the caller's loop.
- **No distributed coordination in-box.** No gossip, no leader; that lives in the
  store implementation you plug in.
- **Not a billing system.** The optional cost helper is for `max_cost` rules, not an
  invoice of record.

## Benchmarks

See `BENCHMARKS.md`. Short version: the bucketed window holds flat at ~19.5 KB while
the exact log grows linearly to ~1 MB at 8k in-window events; decisions run at ~16k/s
in pure Python (the C# and Java ports are substantially faster) with a P50 of ~37 us
and P99 of ~147 us for a realistic three-rule model.
