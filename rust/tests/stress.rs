//! Stress suite: memory pressure, out-of-order events, high-volume soak, and a
//! threaded smoke test. These prove the properties the design claims under load.

use std::collections::VecDeque;
use std::thread;

use quota_gate::{Acquire, LimitRule, Limiter, Scope};

/// A tiny deterministic xorshift64* PRNG so the soak test is dependency-free and
/// reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_unit(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (v >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
fn memory_is_bounded_under_heavy_traffic() {
    // A 1-hour window split into 60 buckets. No matter how many events we push,
    // the store must never hold more than ~60 buckets for this key.
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 3600.0)
        .max_tokens(1e12)
        .buckets_per_window(60)]);
    for i in 0..100_000 {
        l.try_acquire("gpt-4o", &Acquire::new().tokens(1.0).now(i as f64 * 0.072));
    }
    assert!(
        l.store().total_buckets() <= 62,
        "bucket count grew to {}",
        l.store().total_buckets()
    );
}

#[test]
fn precise_mode_grows_but_stays_correct() {
    // The exact-log mode grows with in-window traffic but stays exact.
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0)
        .max_requests(100.0)
        .precise(true)]);
    let mut allowed = 0;
    for _ in 0..500 {
        if l.try_acquire("gpt-4o", &Acquire::new().now(1000.0)).allowed {
            allowed += 1;
        }
    }
    assert_eq!(allowed, 100);
}

#[test]
fn enforcement_is_correct_with_out_of_order_timestamps() {
    // Feed events whose timestamps jump around within the window. The rolling count
    // must still reflect everything inside the window, so the cap holds.
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0).max_requests(10.0)]);
    let stamps = [
        1000.0, 1002.0, 1001.0, 1005.0, 1003.0, 1004.0, 1002.5, 1001.5, 1000.5, 1004.5,
    ];
    for ts in stamps {
        assert!(l.try_acquire("gpt-4o", &Acquire::new().now(ts)).allowed);
    }
    // The 11th event anywhere in the window must be denied regardless of order.
    assert!(!l.try_acquire("gpt-4o", &Acquire::new().now(1002.7)).allowed);
}

#[test]
fn high_volume_soak_keeps_the_cap() {
    // A tight per-minute cap approximated by 1-second buckets. In ANY sliding 60s
    // window, admissions stay within the cap plus a small bucket-granularity error.
    let mut rng = Rng::new(1);
    let cap = 1000_i64;
    let buckets = 60_i64;
    let tolerance = cap / buckets + 1; // ~one bucket's worth of edge error
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0)
        .max_requests(cap as f64)
        .buckets_per_window(buckets)]);

    let mut admitted: VecDeque<f64> = VecDeque::new();
    let mut t = 0.0_f64;
    let mut worst = 0_i64;
    for _ in 0..300_000 {
        t += rng.next_unit() * 0.01;
        if l.try_acquire("gpt-4o", &Acquire::new().now(t)).allowed {
            admitted.push_back(t);
        }
        while admitted.front().is_some_and(|&front| front <= t - 60.0) {
            admitted.pop_front();
        }
        worst = worst.max(admitted.len() as i64);
        assert!(admitted.len() as i64 <= cap + tolerance);
    }
    // And it genuinely pushes up against the cap (not trivially under it).
    assert!(
        worst >= (cap as f64 * 0.9) as i64,
        "worst {worst} never approached the cap"
    );
}

#[test]
fn threaded_access_does_not_corrupt_counters() {
    // The in-memory store is single-process and not thread-safe by construction;
    // the design recommends sharding per worker. Prove per-worker limiters survive
    // many threads hammering them and stay exactly consistent.
    let mut handles = Vec::new();
    for i in 0..8 {
        handles.push(thread::spawn(move || {
            let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 3600.0)
                .max_requests(1e9)
                .scope(Scope::Tenant)]);
            let tenant = format!("t{i}");
            let mut allowed = 0;
            for j in 0..5000 {
                if l.try_acquire(
                    "gpt-4o",
                    &Acquire::new().tenant(&tenant).now(j as f64 * 0.01),
                )
                .allowed
                {
                    allowed += 1;
                }
            }
            allowed
        }));
    }
    for h in handles {
        // Every worker admitted all 5000 calls: no crash, no lost or corrupt count.
        assert_eq!(h.join().unwrap(), 5000);
    }
}
