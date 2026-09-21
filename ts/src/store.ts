/**
 * Bounded-memory sliding-window counters behind a pluggable store.
 *
 * The default engine keeps fixed-size time *buckets* per key, so memory is bounded
 * by `window / bucket` regardless of traffic - not one entry per request. A 24h
 * window with 60 buckets is 60 counters whether you serve 10 requests or 10
 * million. The trailing (oldest) bucket is prorated by how much of it still lies
 * inside the window, so accuracy is tunable via `bucketsPerWindow`.
 *
 * A precise per-event log is available (`bucket <= 0`) for low-QPS callers who
 * want microsecond-exact windows and don't mind memory that grows with traffic.
 *
 * {@link Store} is the extension seam. {@link InMemoryStore} is the single-process
 * default; a distributed deployment implements the same interface over Redis
 * (atomic INCR plus TTL per bucket) so counters coordinate across replicas.
 */

/** Token dimension index. */
export const TOKENS = 0;
/** Request dimension index. */
export const REQUESTS = 1;
/** Cost dimension index. */
export const COST = 2;

/**
 * An opaque handle to a recorded entry, returned by {@link Store.add} and passed
 * to {@link Store.adjust} to reconcile it later. A bucket window returns its
 * bucket index; a precise window returns the event tuple itself.
 */
export type Handle = number | number[];

interface Window {
  snapshot(now: number): [number, number, number];
  add(now: number, tokens: number, requests: number, cost: number): Handle;
  adjust(handle: Handle, dt: number, dr: number, dc: number): void;
  timeToFree(now: number, over: number, dim: number): number;
  bucketCount(): number | null;
}

/** Fixed-size time buckets. Memory is bounded to ~window/bucket entries. */
class BucketWindow implements Window {
  private readonly window: number;
  private readonly bucket: number;
  private readonly buckets = new Map<number, [number, number, number]>();

  constructor(window: number, bucket: number) {
    this.window = window;
    this.bucket = Math.max(bucket, 1e-9);
  }

  private evict(now: number): void {
    const lo = now - this.window;
    for (const idx of [...this.buckets.keys()]) {
      if ((idx + 1) * this.bucket <= lo) {
        this.buckets.delete(idx);
      }
    }
  }

  snapshot(now: number): [number, number, number] {
    const lo = now - this.window;
    let t = 0;
    let r = 0;
    let c = 0;
    for (const [idx, v] of this.buckets) {
      const bStart = idx * this.bucket;
      const bEnd = bStart + this.bucket;
      if (bEnd <= lo) {
        continue;
      }
      // Newest/interior buckets count fully; only the straddling oldest bucket is
      // prorated by the fraction of it still inside the window.
      const frac = bStart >= lo ? 1.0 : (bEnd - lo) / this.bucket;
      t += v[TOKENS] * frac;
      r += v[REQUESTS] * frac;
      c += v[COST] * frac;
    }
    return [t, r, c];
  }

  add(now: number, tokens: number, requests: number, cost: number): Handle {
    this.evict(now);
    const idx = Math.floor(now / this.bucket);
    let v = this.buckets.get(idx);
    if (v === undefined) {
      v = [0, 0, 0];
      this.buckets.set(idx, v);
    }
    v[TOKENS] += tokens;
    v[REQUESTS] += requests;
    v[COST] += cost;
    return idx;
  }

  adjust(handle: Handle, dt: number, dr: number, dc: number): void {
    const v = this.buckets.get(handle as number);
    if (v !== undefined) {
      v[TOKENS] += dt;
      v[REQUESTS] += dr;
      v[COST] += dc;
    }
  }

  timeToFree(now: number, over: number, dim: number): number {
    const lo = now - this.window;
    const entries: Array<[number, number]> = [];
    for (const [idx, v] of this.buckets) {
      const bEnd = (idx + 1) * this.bucket;
      if (bEnd <= lo || v[dim] <= 0) {
        continue;
      }
      entries.push([bEnd, v[dim]]);
    }
    entries.sort((a, b) => a[0] - b[0]);
    let freed = 0;
    for (const [bEnd, amt] of entries) {
      freed += amt;
      if (freed >= over - 1e-9) {
        // This bucket fully exits the window at bEnd + window.
        return Math.max(0, bEnd + this.window - now);
      }
    }
    return this.window;
  }

  bucketCount(): number {
    return this.buckets.size;
  }
}

/** Exact per-event log. Memory grows with in-window traffic. */
class PreciseWindow implements Window {
  private readonly window: number;
  private events: number[][] = []; // [ts, tokens, requests, cost]

  constructor(window: number) {
    this.window = window;
  }

  private evict(now: number): void {
    const lo = now - this.window;
    this.events = this.events.filter((e) => e[0] > lo);
  }

  snapshot(now: number): [number, number, number] {
    const lo = now - this.window;
    let t = 0;
    let r = 0;
    let c = 0;
    for (const e of this.events) {
      if (e[0] > lo) {
        t += e[1];
        r += e[2];
        c += e[3];
      }
    }
    return [t, r, c];
  }

  add(now: number, tokens: number, requests: number, cost: number): Handle {
    this.evict(now);
    const e = [now, tokens, requests, cost];
    this.events.push(e);
    return e;
  }

  adjust(handle: Handle, dt: number, dr: number, dc: number): void {
    const e = handle as number[];
    e[1] += dt;
    e[2] += dr;
    e[3] += dc;
  }

  timeToFree(now: number, over: number, dim: number): number {
    const lo = now - this.window;
    const live = this.events.filter((e) => e[0] > lo).sort((a, b) => a[0] - b[0]);
    let freed = 0;
    const col = dim + 1; // events store ts at index 0, dims shifted by one
    for (const e of live) {
      freed += e[col];
      if (freed >= over - 1e-9) {
        return Math.max(0, e[0] + this.window - now);
      }
    }
    return this.window;
  }

  bucketCount(): null {
    return null;
  }
}

/** The persistence seam. Implement over Redis for distributed enforcement. */
export interface Store {
  /** Returns the current `[tokens, requests, cost]` totals for a key. */
  snapshot(key: string, now: number, window: number, bucket: number): [number, number, number];
  /** Records usage and returns a handle for later adjustment. */
  add(
    key: string,
    now: number,
    window: number,
    bucket: number,
    tokens: number,
    requests: number,
    cost: number,
  ): Handle;
  /** Reconciles a previously recorded entry by the given deltas. */
  adjust(key: string, handle: Handle, dt: number, dr: number, dc: number): void;
  /** Returns how long until `over` units of a dimension age out. */
  timeToFree(key: string, now: number, window: number, bucket: number, over: number, dim: number): number;
  /** Returns the current in-flight count for a key. */
  concurrency(key: string): number;
  /** Increments the in-flight count if it is below the limit. */
  tryAddConcurrency(key: string, limit: number): boolean;
  /** Decrements the in-flight count, never below zero. */
  releaseConcurrency(key: string): void;
}

/** Single-process default. One window object and one counter per key. */
export class InMemoryStore implements Store {
  private readonly windows = new Map<string, Window>();
  private readonly concurrencyCounts = new Map<string, number>();

  private windowFor(key: string, window: number, bucket: number): Window {
    let w = this.windows.get(key);
    if (w === undefined) {
      w = bucket <= 0 ? new PreciseWindow(window) : new BucketWindow(window, bucket);
      this.windows.set(key, w);
    }
    return w;
  }

  snapshot(key: string, now: number, window: number, bucket: number): [number, number, number] {
    return this.windowFor(key, window, bucket).snapshot(now);
  }

  add(
    key: string,
    now: number,
    window: number,
    bucket: number,
    tokens: number,
    requests: number,
    cost: number,
  ): Handle {
    return this.windowFor(key, window, bucket).add(now, tokens, requests, cost);
  }

  adjust(key: string, handle: Handle, dt: number, dr: number, dc: number): void {
    const w = this.windows.get(key);
    if (w !== undefined) {
      w.adjust(handle, dt, dr, dc);
    }
  }

  timeToFree(key: string, now: number, window: number, bucket: number, over: number, dim: number): number {
    return this.windowFor(key, window, bucket).timeToFree(now, over, dim);
  }

  concurrency(key: string): number {
    return this.concurrencyCounts.get(key) ?? 0;
  }

  tryAddConcurrency(key: string, limit: number): boolean {
    const cur = this.concurrencyCounts.get(key) ?? 0;
    if (cur >= limit) {
      return false;
    }
    this.concurrencyCounts.set(key, cur + 1);
    return true;
  }

  releaseConcurrency(key: string): void {
    const cur = this.concurrencyCounts.get(key) ?? 0;
    if (cur > 0) {
      this.concurrencyCounts.set(key, cur - 1);
    }
  }

  /** Total live buckets across every bucketed window (introspection/monitoring). */
  totalBuckets(): number {
    let total = 0;
    for (const w of this.windows.values()) {
      const c = w.bucketCount();
      if (c !== null) {
        total += c;
      }
    }
    return total;
  }
}
