//! The sliding-window engines exercised through the public store API.

use quota_gate::{InMemoryStore, Store, REQUESTS};

#[test]
fn bucket_memory_is_bounded_regardless_of_traffic() {
    // 100s window, 1s buckets -> at most ~100 buckets even under heavy traffic.
    let mut store = InMemoryStore::new();
    for ts in 0..150 {
        for _ in 0..1000 {
            store.add("k", ts as f64, 100.0, 1.0, [10.0, 1.0, 0.0]);
        }
    }
    assert!(
        store.total_buckets() <= 102,
        "bucket count grew to {}",
        store.total_buckets()
    );
}

#[test]
fn bucket_counts_recent_and_drops_old() {
    let mut store = InMemoryStore::new();
    store.add("k", 0.0, 60.0, 1.0, [100.0, 1.0, 0.0]);
    let (t, _, _) = store.snapshot("k", 30.0, 60.0, 1.0);
    assert_eq!(t, 100.0);
    // The event at t=0 has fully aged out by t=61.
    let (t, _, _) = store.snapshot("k", 61.0, 60.0, 1.0);
    assert_eq!(t, 0.0);
}

#[test]
fn bucket_prorates_the_straddling_edge() {
    // One big bucket spanning [0,10); at now=65 the window [5,65] covers half of it.
    let mut store = InMemoryStore::new();
    store.add("k", 0.0, 60.0, 10.0, [100.0, 0.0, 0.0]);
    let (t, _, _) = store.snapshot("k", 65.0, 60.0, 10.0);
    assert!(
        (40.0..=60.0).contains(&t),
        "expected ~half prorated, got {t}"
    );
}

#[test]
fn precise_window_is_exact() {
    // A zero bucket width selects the exact per-event log.
    let mut store = InMemoryStore::new();
    store.add("k", 0.0, 60.0, 0.0, [10.0, 1.0, 0.0]);
    store.add("k", 59.0, 60.0, 0.0, [10.0, 1.0, 0.0]);
    let (_, r, _) = store.snapshot("k", 59.5, 60.0, 0.0);
    assert_eq!(r, 2.0);
    // First event drops at t>60.
    let (_, r, _) = store.snapshot("k", 60.5, 60.0, 0.0);
    assert_eq!(r, 1.0);
}

#[test]
fn adjust_updates_a_recorded_entry() {
    let mut store = InMemoryStore::new();
    let handle = store.add("k", 0.0, 60.0, 1.0, [100.0, 1.0, 1.0]);
    store.adjust("k", &handle, [-40.0, 0.0, -0.4]);
    let (t, _, c) = store.snapshot("k", 1.0, 60.0, 1.0);
    assert_eq!(t, 60.0);
    assert_eq!((c * 1e6).round() / 1e6, 0.6);
}

#[test]
fn time_to_free_reports_when_headroom_returns() {
    // One request at t=0 in a 60s precise window frees at t=60.
    let mut store = InMemoryStore::new();
    store.add("k", 0.0, 60.0, 0.0, [0.0, 1.0, 0.0]);
    let ttf = store.time_to_free("k", 0.0, 60.0, 0.0, 1.0, REQUESTS);
    assert_eq!(ttf, 60.0);
}
