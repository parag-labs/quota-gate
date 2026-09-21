import { describe, expect, it } from "vitest";

import { InMemoryStore, REQUESTS } from "./store";

// The sliding-window engines exercised through the public store API.

describe("windows", () => {
  it("keeps bucket memory bounded regardless of traffic", () => {
    // 100s window, 1s buckets -> at most ~100 buckets even under heavy traffic.
    const store = new InMemoryStore();
    for (let ts = 0; ts < 150; ts++) {
      for (let i = 0; i < 1000; i++) {
        store.add("k", ts, 100, 1, 10, 1, 0);
      }
    }
    expect(store.totalBuckets()).toBeLessThanOrEqual(102);
  });

  it("counts recent buckets and drops old ones", () => {
    const store = new InMemoryStore();
    store.add("k", 0, 60, 1, 100, 1, 0);
    expect(store.snapshot("k", 30, 60, 1)[0]).toBe(100);
    // The event at t=0 has fully aged out by t=61.
    expect(store.snapshot("k", 61, 60, 1)[0]).toBe(0);
  });

  it("prorates the straddling edge bucket", () => {
    // One big bucket spanning [0,10); at now=65 the window [5,65] covers half of it.
    const store = new InMemoryStore();
    store.add("k", 0, 60, 10, 100, 0, 0);
    const t = store.snapshot("k", 65, 60, 10)[0];
    expect(t).toBeGreaterThanOrEqual(40);
    expect(t).toBeLessThanOrEqual(60);
  });

  it("is exact in precise mode", () => {
    // A zero bucket width selects the exact per-event log.
    const store = new InMemoryStore();
    store.add("k", 0, 60, 0, 10, 1, 0);
    store.add("k", 59, 60, 0, 10, 1, 0);
    expect(store.snapshot("k", 59.5, 60, 0)[REQUESTS]).toBe(2);
    // First event drops at t>60.
    expect(store.snapshot("k", 60.5, 60, 0)[REQUESTS]).toBe(1);
  });

  it("adjusts a recorded entry", () => {
    const store = new InMemoryStore();
    const handle = store.add("k", 0, 60, 1, 100, 1, 1);
    store.adjust("k", handle, -40, 0, -0.4);
    const [t, , c] = store.snapshot("k", 1, 60, 1);
    expect(t).toBe(60);
    expect(Math.round(c * 1e6) / 1e6).toBe(0.6);
  });

  it("reports when headroom returns via timeToFree", () => {
    // One request at t=0 in a 60s precise window frees at t=60.
    const store = new InMemoryStore();
    store.add("k", 0, 60, 0, 0, 1, 0);
    expect(store.timeToFree("k", 0, 60, 0, 1, REQUESTS)).toBe(60);
  });
});
