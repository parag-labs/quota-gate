package com.quotagate;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Bounded-memory sliding-window counters behind a pluggable store.
 *
 * <p>The default engine keeps fixed-size time buckets per key, so memory is bounded
 * by {@code window / bucket} regardless of traffic. A precise per-event log is
 * available for low-QPS callers who want microsecond-exact windows. {@link Store}
 * is the extension seam; {@link InMemoryStore} is the single-process default and a
 * Redis-backed implementation would satisfy the same interface for a fleet.
 */
public final class Store {

    // Dimension indices shared across the codebase.
    public static final int TOKENS = 0;
    public static final int REQUESTS = 1;
    public static final int COST = 2;

    private Store() {
    }

    /** A per-key sliding-window aggregate. */
    public interface Window {
        double[] snapshot(double now); // {tokens, requests, cost}

        Object add(double now, double tokens, double requests, double cost);

        void adjust(Object handle, double dt, double dr, double dc);

        double timeToFree(double now, double over, int dim);
    }

    /** Fixed-size time buckets. Memory is bounded to ~window/bucket entries. */
    public static final class BucketWindow implements Window {
        private final double window;
        private final double bucket;
        private final Map<Long, double[]> buckets = new LinkedHashMap<>();

        public BucketWindow(double window, double bucket) {
            this.window = window;
            this.bucket = Math.max(bucket, 1e-9);
        }

        public int bucketCount() {
            return buckets.size();
        }

        private void evict(double now) {
            double lo = now - window;
            buckets.entrySet().removeIf(e -> (e.getKey() + 1) * bucket <= lo);
        }

        @Override
        public double[] snapshot(double now) {
            double lo = now - window;
            double t = 0;
            double r = 0;
            double c = 0;
            for (Map.Entry<Long, double[]> e : buckets.entrySet()) {
                double bStart = e.getKey() * bucket;
                double bEnd = bStart + bucket;
                if (bEnd <= lo) {
                    continue;
                }
                double frac = bStart >= lo ? 1.0 : (bEnd - lo) / bucket;
                double[] v = e.getValue();
                t += v[TOKENS] * frac;
                r += v[REQUESTS] * frac;
                c += v[COST] * frac;
            }
            return new double[] {t, r, c};
        }

        @Override
        public Object add(double now, double tokens, double requests, double cost) {
            evict(now);
            long idx = (long) Math.floor(now / bucket);
            double[] v = buckets.computeIfAbsent(idx, k -> new double[3]);
            v[TOKENS] += tokens;
            v[REQUESTS] += requests;
            v[COST] += cost;
            return idx;
        }

        @Override
        public void adjust(Object handle, double dt, double dr, double dc) {
            double[] v = buckets.get((Long) handle);
            if (v != null) {
                v[TOKENS] += dt;
                v[REQUESTS] += dr;
                v[COST] += dc;
            }
        }

        @Override
        public double timeToFree(double now, double over, int dim) {
            double lo = now - window;
            List<double[]> entries = new ArrayList<>(); // {bEnd, amt}
            for (Map.Entry<Long, double[]> e : buckets.entrySet()) {
                double bEnd = (e.getKey() + 1) * bucket;
                double amt = e.getValue()[dim];
                if (bEnd <= lo || amt <= 0) {
                    continue;
                }
                entries.add(new double[] {bEnd, amt});
            }
            entries.sort(Comparator.comparingDouble(a -> a[0]));
            double freed = 0;
            for (double[] e : entries) {
                freed += e[1];
                if (freed >= over - 1e-9) {
                    return Math.max(0.0, e[0] + window - now);
                }
            }
            return window;
        }
    }

    /** Exact per-event log. Memory grows with in-window traffic. */
    public static final class PreciseWindow implements Window {
        private final double window;
        private List<double[]> events = new ArrayList<>(); // {ts, tokens, requests, cost}

        public PreciseWindow(double window) {
            this.window = window;
        }

        private void evict(double now) {
            double lo = now - window;
            List<double[]> live = new ArrayList<>();
            for (double[] e : events) {
                if (e[0] > lo) {
                    live.add(e);
                }
            }
            events = live;
        }

        @Override
        public double[] snapshot(double now) {
            double lo = now - window;
            double t = 0;
            double r = 0;
            double c = 0;
            for (double[] e : events) {
                if (e[0] > lo) {
                    t += e[1];
                    r += e[2];
                    c += e[3];
                }
            }
            return new double[] {t, r, c};
        }

        @Override
        public Object add(double now, double tokens, double requests, double cost) {
            evict(now);
            double[] e = {now, tokens, requests, cost};
            events.add(e);
            return e;
        }

        @Override
        public void adjust(Object handle, double dt, double dr, double dc) {
            double[] e = (double[]) handle;
            e[1] += dt;
            e[2] += dr;
            e[3] += dc;
        }

        @Override
        public double timeToFree(double now, double over, int dim) {
            double lo = now - window;
            List<double[]> live = new ArrayList<>();
            for (double[] e : events) {
                if (e[0] > lo) {
                    live.add(e);
                }
            }
            live.sort(Comparator.comparingDouble(a -> a[0]));
            double freed = 0;
            for (double[] e : live) {
                freed += e[dim + 1];
                if (freed >= over - 1e-9) {
                    return Math.max(0.0, e[0] + window - now);
                }
            }
            return window;
        }
    }

    /** The persistence seam. Implement over Redis for distributed enforcement. */
    public interface Backend {
        double[] snapshot(String key, double now, double window, double bucket);

        Object add(String key, double now, double window, double bucket,
                   double tokens, double requests, double cost);

        void adjust(String key, Object handle, double dt, double dr, double dc);

        double timeToFree(String key, double now, double window, double bucket, double over, int dim);

        int concurrency(String key);

        boolean tryAddConcurrency(String key, int limit);

        void releaseConcurrency(String key);
    }

    /** Single-process default. One window object and one counter per key. */
    public static final class InMemoryStore implements Backend {
        private final Map<String, Window> windows = new LinkedHashMap<>();
        private final Map<String, Integer> concurrency = new LinkedHashMap<>();

        private Window window(String key, double window, double bucket) {
            return windows.computeIfAbsent(key,
                    k -> bucket <= 0 ? new PreciseWindow(window) : new BucketWindow(window, bucket));
        }

        @Override
        public double[] snapshot(String key, double now, double window, double bucket) {
            return window(key, window, bucket).snapshot(now);
        }

        @Override
        public Object add(String key, double now, double window, double bucket,
                          double tokens, double requests, double cost) {
            return window(key, window, bucket).add(now, tokens, requests, cost);
        }

        @Override
        public void adjust(String key, Object handle, double dt, double dr, double dc) {
            Window w = windows.get(key);
            if (w != null) {
                w.adjust(handle, dt, dr, dc);
            }
        }

        @Override
        public double timeToFree(String key, double now, double window, double bucket, double over, int dim) {
            return window(key, window, bucket).timeToFree(now, over, dim);
        }

        @Override
        public int concurrency(String key) {
            return concurrency.getOrDefault(key, 0);
        }

        @Override
        public boolean tryAddConcurrency(String key, int limit) {
            int cur = concurrency(key);
            if (cur >= limit) {
                return false;
            }
            concurrency.put(key, cur + 1);
            return true;
        }

        @Override
        public void releaseConcurrency(String key) {
            int cur = concurrency(key);
            if (cur > 0) {
                concurrency.put(key, cur - 1);
            }
        }
    }
}
