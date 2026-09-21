//! Sliding-window storage: bounded-memory buckets vs an exact per-event log,
//! behind a `Store` seam that can be re-implemented over Redis for a fleet.

use std::collections::HashMap;

/// Token dimension index.
pub const TOKENS: usize = 0;
/// Request dimension index.
pub const REQUESTS: usize = 1;
/// Cost dimension index.
pub const COST: usize = 2;

/// An opaque handle to a recorded entry, returned by [`Store::add`] and passed to
/// [`Store::adjust`] to reconcile it later.
#[derive(Debug, Clone, Copy)]
pub enum Handle {
    /// A bucket index in a bucketed window.
    Bucket(i64),
    /// An event id in a precise window.
    Precise(u64),
}

/// A per-key sliding-window aggregate. Amounts and deltas are `[tokens, requests,
/// cost]` triples indexed by the dimension constants.
trait Window: Send {
    fn snapshot(&self, now: f64) -> (f64, f64, f64);
    fn add(&mut self, now: f64, amounts: [f64; 3]) -> Handle;
    fn adjust(&mut self, handle: &Handle, deltas: [f64; 3]);
    fn time_to_free(&self, now: f64, over: f64, dim: usize) -> f64;
    fn bucket_count(&self) -> Option<usize>;
}

/// Fixed-size time buckets. Memory is bounded to ~window/bucket entries
/// regardless of traffic; the trailing bucket is prorated by the fraction of it
/// still inside the window.
struct BucketWindow {
    window: f64,
    bucket: f64,
    buckets: HashMap<i64, [f64; 3]>,
}

impl BucketWindow {
    fn new(window: f64, bucket: f64) -> Self {
        BucketWindow {
            window,
            bucket: if bucket < 1e-9 { 1e-9 } else { bucket },
            buckets: HashMap::new(),
        }
    }

    fn evict(&mut self, now: f64) {
        let lo = now - self.window;
        let bucket = self.bucket;
        self.buckets
            .retain(|idx, _| (*idx as f64 + 1.0) * bucket > lo);
    }
}

impl Window for BucketWindow {
    fn snapshot(&self, now: f64) -> (f64, f64, f64) {
        let lo = now - self.window;
        let (mut t, mut r, mut c) = (0.0, 0.0, 0.0);
        for (idx, v) in &self.buckets {
            let b_start = *idx as f64 * self.bucket;
            let b_end = b_start + self.bucket;
            if b_end <= lo {
                continue;
            }
            let frac = if b_start < lo {
                (b_end - lo) / self.bucket
            } else {
                1.0
            };
            t += v[TOKENS] * frac;
            r += v[REQUESTS] * frac;
            c += v[COST] * frac;
        }
        (t, r, c)
    }

    fn add(&mut self, now: f64, amounts: [f64; 3]) -> Handle {
        self.evict(now);
        let idx = (now / self.bucket).floor() as i64;
        let v = self.buckets.entry(idx).or_insert([0.0; 3]);
        for (slot, a) in v.iter_mut().zip(amounts) {
            *slot += a;
        }
        Handle::Bucket(idx)
    }

    fn adjust(&mut self, handle: &Handle, deltas: [f64; 3]) {
        if let Handle::Bucket(idx) = handle {
            if let Some(v) = self.buckets.get_mut(idx) {
                for (slot, d) in v.iter_mut().zip(deltas) {
                    *slot += d;
                }
            }
        }
    }

    fn time_to_free(&self, now: f64, over: f64, dim: usize) -> f64 {
        let lo = now - self.window;
        let mut entries: Vec<(f64, f64)> = Vec::new();
        for (idx, v) in &self.buckets {
            let b_end = (*idx as f64 + 1.0) * self.bucket;
            if b_end <= lo || v[dim] <= 0.0 {
                continue;
            }
            entries.push((b_end, v[dim]));
        }
        entries.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut freed = 0.0;
        for (b_end, amt) in entries {
            freed += amt;
            if freed >= over - 1e-9 {
                return (b_end + self.window - now).max(0.0);
            }
        }
        self.window
    }

    fn bucket_count(&self) -> Option<usize> {
        Some(self.buckets.len())
    }
}

#[derive(Clone)]
struct Event {
    id: u64,
    ts: f64,
    tokens: f64,
    requests: f64,
    cost: f64,
}

/// An exact per-event log. Memory grows with in-window traffic, in exchange for
/// exact enforcement and retry timing.
struct PreciseWindow {
    window: f64,
    events: Vec<Event>,
    next_id: u64,
}

impl PreciseWindow {
    fn new(window: f64) -> Self {
        PreciseWindow {
            window,
            events: Vec::new(),
            next_id: 0,
        }
    }

    fn evict(&mut self, now: f64) {
        let lo = now - self.window;
        self.events.retain(|e| e.ts > lo);
    }
}

impl Window for PreciseWindow {
    fn snapshot(&self, now: f64) -> (f64, f64, f64) {
        let lo = now - self.window;
        let (mut t, mut r, mut c) = (0.0, 0.0, 0.0);
        for e in &self.events {
            if e.ts > lo {
                t += e.tokens;
                r += e.requests;
                c += e.cost;
            }
        }
        (t, r, c)
    }

    fn add(&mut self, now: f64, amounts: [f64; 3]) -> Handle {
        self.evict(now);
        let id = self.next_id;
        self.next_id += 1;
        self.events.push(Event {
            id,
            ts: now,
            tokens: amounts[TOKENS],
            requests: amounts[REQUESTS],
            cost: amounts[COST],
        });
        Handle::Precise(id)
    }

    fn adjust(&mut self, handle: &Handle, deltas: [f64; 3]) {
        if let Handle::Precise(id) = handle {
            if let Some(e) = self.events.iter_mut().find(|e| e.id == *id) {
                e.tokens += deltas[TOKENS];
                e.requests += deltas[REQUESTS];
                e.cost += deltas[COST];
            }
        }
    }

    fn time_to_free(&self, now: f64, over: f64, dim: usize) -> f64 {
        let lo = now - self.window;
        let mut live: Vec<&Event> = self.events.iter().filter(|e| e.ts > lo).collect();
        live.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        let mut freed = 0.0;
        for e in live {
            let amt = match dim {
                TOKENS => e.tokens,
                REQUESTS => e.requests,
                _ => e.cost,
            };
            freed += amt;
            if freed >= over - 1e-9 {
                return (e.ts + self.window - now).max(0.0);
            }
        }
        self.window
    }

    fn bucket_count(&self) -> Option<usize> {
        None
    }
}

/// The persistence seam. Implement it over Redis (atomic INCR plus a TTL per
/// bucket) for distributed enforcement across replicas.
pub trait Store: Send {
    /// Returns the current `(tokens, requests, cost)` totals for a key.
    fn snapshot(&mut self, key: &str, now: f64, window: f64, bucket: f64) -> (f64, f64, f64);
    /// Records usage `[tokens, requests, cost]` and returns a handle for later
    /// adjustment.
    fn add(&mut self, key: &str, now: f64, window: f64, bucket: f64, amounts: [f64; 3]) -> Handle;
    /// Reconciles a previously recorded entry by `[tokens, requests, cost]` deltas.
    fn adjust(&mut self, key: &str, handle: &Handle, deltas: [f64; 3]);
    /// Returns how long until `over` units of a dimension age out.
    fn time_to_free(
        &mut self,
        key: &str,
        now: f64,
        window: f64,
        bucket: f64,
        over: f64,
        dim: usize,
    ) -> f64;
    /// Returns the current in-flight count for a key.
    fn concurrency(&self, key: &str) -> i64;
    /// Increments the in-flight count if it is below the limit.
    fn try_add_concurrency(&mut self, key: &str, limit: i64) -> bool;
    /// Decrements the in-flight count, never below zero.
    fn release_concurrency(&mut self, key: &str);
    /// Total live buckets across every bucketed window (introspection/monitoring).
    fn total_buckets(&self) -> usize {
        0
    }
}

/// The single-process default. One window object and one counter per key. Not
/// safe for concurrent use; shard per worker or plug in a shared store for a
/// multi-threaded server.
#[derive(Default)]
pub struct InMemoryStore {
    windows: HashMap<String, Box<dyn Window>>,
    concurrency: HashMap<String, i64>,
}

impl InMemoryStore {
    /// Returns an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    fn window_for(&mut self, key: &str, win: f64, bucket: f64) -> &mut Box<dyn Window> {
        self.windows.entry(key.to_string()).or_insert_with(|| {
            if bucket <= 0.0 {
                Box::new(PreciseWindow::new(win))
            } else {
                Box::new(BucketWindow::new(win, bucket))
            }
        })
    }
}

impl Store for InMemoryStore {
    fn snapshot(&mut self, key: &str, now: f64, window: f64, bucket: f64) -> (f64, f64, f64) {
        self.window_for(key, window, bucket).snapshot(now)
    }

    fn add(&mut self, key: &str, now: f64, window: f64, bucket: f64, amounts: [f64; 3]) -> Handle {
        self.window_for(key, window, bucket).add(now, amounts)
    }

    fn adjust(&mut self, key: &str, handle: &Handle, deltas: [f64; 3]) {
        if let Some(w) = self.windows.get_mut(key) {
            w.adjust(handle, deltas);
        }
    }

    fn time_to_free(
        &mut self,
        key: &str,
        now: f64,
        window: f64,
        bucket: f64,
        over: f64,
        dim: usize,
    ) -> f64 {
        self.window_for(key, window, bucket)
            .time_to_free(now, over, dim)
    }

    fn concurrency(&self, key: &str) -> i64 {
        *self.concurrency.get(key).unwrap_or(&0)
    }

    fn try_add_concurrency(&mut self, key: &str, limit: i64) -> bool {
        let c = self.concurrency.entry(key.to_string()).or_insert(0);
        if *c >= limit {
            return false;
        }
        *c += 1;
        true
    }

    fn release_concurrency(&mut self, key: &str) {
        if let Some(c) = self.concurrency.get_mut(key) {
            if *c > 0 {
                *c -= 1;
            }
        }
    }

    fn total_buckets(&self) -> usize {
        self.windows.values().filter_map(|w| w.bucket_count()).sum()
    }
}
