import { describe, expect, it } from "vitest";

import { Limiter } from "./limiter";
import { LimitRule, Scope } from "./rules";
import { InMemoryStore } from "./store";

// Stress suite: memory pressure, out-of-order events, high-volume soak, and a
// single-threaded per-worker sharding smoke test. These prove the properties the
// design claims under load.

/** A tiny deterministic mulberry32 PRNG so the soak test is reproducible. */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe("stress", () => {
  it("keeps memory bounded under heavy traffic", () => {
    // A 1-hour window split into 60 buckets. No matter how many events we push,
    // the store must never hold more than ~60 buckets for this key.
    const store = new InMemoryStore();
    const l = new Limiter([new LimitRule("gpt-4o", 3600, { maxTokens: 1e12, bucketsPerWindow: 60 })], { store });
    for (let i = 0; i < 100_000; i++) {
      l.tryAcquire("gpt-4o", { tokens: 1, now: i * 0.072 });
    }
    expect(store.totalBuckets()).toBeLessThanOrEqual(62);
  });

  it("grows but stays correct in precise mode", () => {
    // The exact-log mode grows with in-window traffic but stays exact.
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 100, precise: true })]);
    let allowed = 0;
    for (let i = 0; i < 500; i++) {
      if (l.tryAcquire("gpt-4o", { now: 1000 }).allowed) {
        allowed++;
      }
    }
    expect(allowed).toBe(100);
  });

  it("enforces correctly with out-of-order timestamps", () => {
    // Feed events whose timestamps jump around within the window. The rolling count
    // must still reflect everything inside the window, so the cap holds.
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 10 })]);
    const stamps = [1000, 1002, 1001, 1005, 1003, 1004, 1002.5, 1001.5, 1000.5, 1004.5];
    for (const ts of stamps) {
      expect(l.tryAcquire("gpt-4o", { now: ts }).allowed).toBe(true);
    }
    // The 11th event anywhere in the window must be denied regardless of order.
    expect(l.tryAcquire("gpt-4o", { now: 1002.7 }).allowed).toBe(false);
  });

  it("keeps the cap in a high-volume soak", () => {
    // A tight per-minute cap approximated by 1-second buckets. In ANY sliding 60s
    // window, admissions stay within the cap plus a small bucket-granularity error.
    const rng = mulberry32(1);
    const cap = 1000;
    const buckets = 60;
    const tolerance = Math.floor(cap / buckets) + 1; // ~one bucket's worth of edge error
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: cap, bucketsPerWindow: buckets })]);

    const admitted: number[] = [];
    let head = 0;
    let t = 0;
    let worst = 0;
    for (let i = 0; i < 300_000; i++) {
      t += rng() * 0.01;
      if (l.tryAcquire("gpt-4o", { now: t }).allowed) {
        admitted.push(t);
      }
      while (head < admitted.length && admitted[head] <= t - 60) {
        head++;
      }
      const live = admitted.length - head;
      worst = Math.max(worst, live);
      expect(live).toBeLessThanOrEqual(cap + tolerance);
    }
    // And it genuinely pushes up against the cap (not trivially under it).
    expect(worst).toBeGreaterThanOrEqual(Math.floor(cap * 0.9));
  });

  it("keeps per-worker limiters exactly consistent (sharding)", () => {
    // The in-memory store is single-process and not thread-safe by construction;
    // the design recommends sharding per worker. JS is single-threaded, so we run
    // eight independent per-worker limiters and prove each stays exactly consistent.
    for (let i = 0; i < 8; i++) {
      const l = new Limiter([new LimitRule("gpt-4o", 3600, { maxRequests: 1e9, scope: Scope.Tenant })]);
      const tenant = `t${i}`;
      let allowed = 0;
      for (let j = 0; j < 5000; j++) {
        if (l.tryAcquire("gpt-4o", { tenant, now: j * 0.01 }).allowed) {
          allowed++;
        }
      }
      expect(allowed).toBe(5000);
    }
  });
});
