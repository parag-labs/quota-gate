package com.quotagate;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import com.quotagate.Rules.LimitRule;
import com.quotagate.Rules.Scope;
import com.quotagate.Store.Backend;
import com.quotagate.Store.InMemoryStore;

/**
 * The gate: evaluate provider-style limits before a call, reserve/reconcile
 * tokens, cap concurrency, and emit standard back-pressure signals. Mirrors how
 * model-serving APIs meter in practice: many limits per model enforced together,
 * across global/tenant/user scopes, with reserve-then-reconcile token accounting.
 */
public final class Limiter {

    public static final class Decision {
        public final boolean allowed;
        public final String model;
        public final double retryAfter;
        public final LimitRule trippedRule;
        public final Scope scope;
        public final Map<String, Double> remaining;
        public final String suggestedFallback;

        Decision(boolean allowed, String model, double retryAfter, LimitRule trippedRule,
                 Scope scope, Map<String, Double> remaining, String suggestedFallback) {
            this.allowed = allowed;
            this.model = model;
            this.retryAfter = retryAfter;
            this.trippedRule = trippedRule;
            this.scope = scope;
            this.remaining = remaining;
            this.suggestedFallback = suggestedFallback;
        }
    }

    public static final class Reservation {
        final String model;
        double tokens;
        double cost;
        List<Object[]> handles; // {key, handle}
        boolean committed;

        Reservation(String model, double tokens, double cost, List<Object[]> handles) {
            this.model = model;
            this.tokens = tokens;
            this.cost = cost;
            this.handles = handles;
        }
    }

    /** A held concurrency slot. Close (try-with-resources) or release to give it back. */
    public static final class Slot implements AutoCloseable {
        private final Limiter limiter;
        private List<String> keys;
        public final boolean ok;
        public final LimitRule trippedRule;

        Slot(Limiter limiter, List<String> keys, boolean ok, LimitRule trippedRule) {
            this.limiter = limiter;
            this.keys = keys;
            this.ok = ok;
            this.trippedRule = trippedRule;
        }

        public void release() {
            for (String k : keys) {
                limiter.store.releaseConcurrency(k);
            }
            keys = new ArrayList<>();
        }

        @Override
        public void close() {
            release();
        }
    }

    public interface Clock {
        double now();
    }

    private final List<LimitRule> rules;
    private final Backend store;
    private final Clock clock;
    private final Map<String, String> fallbacks;
    private final Map<String, List<int[]>> indexByModel = new LinkedHashMap<>();

    public Limiter(List<LimitRule> rules) {
        this(rules, new InMemoryStore(), null, null);
    }

    public Limiter(List<LimitRule> rules, Backend store, Clock clock, Map<String, String> fallbacks) {
        this.rules = new ArrayList<>(rules);
        this.store = store != null ? store : new InMemoryStore();
        this.clock = clock != null ? clock : () -> System.currentTimeMillis() / 1000.0;
        this.fallbacks = fallbacks != null ? new LinkedHashMap<>(fallbacks) : new LinkedHashMap<>();
        for (int i = 0; i < this.rules.size(); i++) {
            indexByModel.computeIfAbsent(this.rules.get(i).model, k -> new ArrayList<>()).add(new int[] {i});
        }
    }

    Backend store() {
        return store;
    }

    private List<int[]> applicable(String model) {
        List<int[]> out = new ArrayList<>();
        out.addAll(indexByModel.getOrDefault(model, List.of()));
        out.addAll(indexByModel.getOrDefault("*", List.of()));
        return out;
    }

    private String key(int index, LimitRule r, String tenant, String user) {
        return switch (r.scope) {
            case GLOBAL -> index + "|g";
            case TENANT -> index + "|t|" + tenant;
            case USER -> index + "|u|" + tenant + "|" + user;
        };
    }

    public Decision tryAcquire(String model, double tokens, double cost,
                               String tenant, String user, Double now, boolean record) {
        double t = now != null ? now : clock.now();
        List<int[]> rs = applicable(model);

        double worstRetry = -1;
        LimitRule worstRule = null;
        for (int[] pair : rs) {
            LimitRule r = rules.get(pair[0]);
            if (!r.hasUsageLimit()) {
                continue;
            }
            String key = key(pair[0], r, tenant, user);
            double[] used = store.snapshot(key, t, r.windowSeconds, r.bucketSeconds());
            List<double[]> breached = new ArrayList<>(); // {dim, over}
            if (r.maxTokens != null && used[Store.TOKENS] + tokens > r.maxTokens + 1e-9) {
                breached.add(new double[] {Store.TOKENS, used[Store.TOKENS] + tokens - r.maxTokens});
            }
            if (r.maxRequests != null && used[Store.REQUESTS] + 1 > r.maxRequests + 1e-9) {
                breached.add(new double[] {Store.REQUESTS, used[Store.REQUESTS] + 1 - r.maxRequests});
            }
            if (r.maxCost != null && used[Store.COST] + cost > r.maxCost + 1e-9) {
                breached.add(new double[] {Store.COST, used[Store.COST] + cost - r.maxCost});
            }
            if (!breached.isEmpty()) {
                double ra = 0;
                for (double[] b : breached) {
                    ra = Math.max(ra, store.timeToFree(key, t, r.windowSeconds, r.bucketSeconds(),
                            b[1], (int) b[0]));
                }
                if (ra > worstRetry) {
                    worstRetry = ra;
                    worstRule = r;
                }
            }
        }

        if (worstRule != null) {
            return new Decision(false, model, worstRetry, worstRule, worstRule.scope,
                    Map.of(), fallbacks.get(model));
        }

        if (record) {
            for (int[] pair : rs) {
                LimitRule r = rules.get(pair[0]);
                if (!r.hasUsageLimit()) {
                    continue;
                }
                store.add(key(pair[0], r, tenant, user), t, r.windowSeconds, r.bucketSeconds(),
                        tokens, 1.0, cost);
            }
        }

        return new Decision(true, model, 0.0, null, null, remaining(model, tenant, user, t), null);
    }

    public Decision tryAcquire(String model, double tokens, double cost,
                               String tenant, String user, Double now) {
        return tryAcquire(model, tokens, cost, tenant, user, now, true);
    }

    private Map<String, Double> remaining(String model, String tenant, String user, double now) {
        Map<String, Double> rem = new LinkedHashMap<>();
        for (int[] pair : applicable(model)) {
            LimitRule r = rules.get(pair[0]);
            String key = key(pair[0], r, tenant, user);
            double[] used = store.snapshot(key, now, r.windowSeconds, r.bucketSeconds());
            if (r.maxTokens != null) {
                tighten(rem, "tokens", r.maxTokens - used[Store.TOKENS]);
            }
            if (r.maxRequests != null) {
                tighten(rem, "requests", r.maxRequests - used[Store.REQUESTS]);
            }
            if (r.maxCost != null) {
                tighten(rem, "cost", r.maxCost - used[Store.COST]);
            }
        }
        rem.replaceAll((k, v) -> Math.max(0.0, v));
        return rem;
    }

    private static void tighten(Map<String, Double> rem, String name, double value) {
        rem.merge(name, value, Math::min);
    }

    // ---- reserve -> commit / refund ----

    public Reservation reserve(String model, double tokens, double cost,
                               String tenant, String user, Double now) {
        double t = now != null ? now : clock.now();
        Decision decision = tryAcquire(model, tokens, cost, tenant, user, t, false);
        if (!decision.allowed) {
            return null;
        }
        List<Object[]> handles = new ArrayList<>();
        for (int[] pair : applicable(model)) {
            LimitRule r = rules.get(pair[0]);
            if (!r.hasUsageLimit()) {
                continue;
            }
            String key = key(pair[0], r, tenant, user);
            Object h = store.add(key, t, r.windowSeconds, r.bucketSeconds(), tokens, 1.0, cost);
            handles.add(new Object[] {key, h});
        }
        return new Reservation(model, tokens, cost, handles);
    }

    public void commit(Reservation reservation, Double actualTokens, Double actualCost) {
        if (reservation.committed) {
            return;
        }
        double dt = actualTokens == null ? 0.0 : actualTokens - reservation.tokens;
        double dc = actualCost == null ? 0.0 : actualCost - reservation.cost;
        for (Object[] h : reservation.handles) {
            store.adjust((String) h[0], h[1], dt, 0.0, dc);
        }
        reservation.tokens += dt;
        reservation.cost += dc;
        reservation.committed = true;
    }

    public void refund(Reservation reservation) {
        if (reservation.committed) {
            return;
        }
        for (Object[] h : reservation.handles) {
            store.adjust((String) h[0], h[1], -reservation.tokens, -1.0, -reservation.cost);
        }
        reservation.handles = new ArrayList<>();
        reservation.committed = true;
    }

    // ---- concurrency slots ----

    public Slot acquireSlot(String model, String tenant, String user) {
        List<String> acquired = new ArrayList<>();
        for (int[] pair : applicable(model)) {
            LimitRule r = rules.get(pair[0]);
            if (r.maxConcurrent == null) {
                continue;
            }
            String key = "conc|" + key(pair[0], r, tenant, user);
            if (!store.tryAddConcurrency(key, r.maxConcurrent)) {
                for (String k : acquired) {
                    store.releaseConcurrency(k);
                }
                return new Slot(this, new ArrayList<>(), false, r);
            }
            acquired.add(key);
        }
        return new Slot(this, acquired, true, null);
    }

    // ---- graceful degradation ----

    public Map.Entry<Decision, String> acquireOrFallback(String model, double tokens, double cost,
                                                         String tenant, String user, Double now) {
        Decision decision = tryAcquire(model, tokens, cost, tenant, user, now);
        if (decision.allowed) {
            return Map.entry(decision, model);
        }
        String fallback = fallbacks.get(model);
        if (fallback != null) {
            Decision alt = tryAcquire(fallback, tokens, cost, tenant, user, now);
            if (alt.allowed) {
                return Map.entry(alt, fallback);
            }
        }
        return Map.entry(decision, model);
    }

    public static Map<String, String> rateLimitHeaders(Decision decision, LimitRule rule) {
        LimitRule r = rule != null ? rule : decision.trippedRule;
        Map<String, String> headers = new LinkedHashMap<>();
        if (decision.retryAfter > 0) {
            headers.put("Retry-After", Long.toString((long) Math.ceil(decision.retryAfter)));
        }
        if (r != null) {
            if (r.maxRequests != null) {
                headers.put("X-RateLimit-Limit-Requests", Long.toString(r.maxRequests.longValue()));
            }
            if (r.maxTokens != null) {
                headers.put("X-RateLimit-Limit-Tokens", Long.toString(r.maxTokens.longValue()));
            }
        }
        if (decision.remaining != null) {
            for (Map.Entry<String, Double> e : decision.remaining.entrySet()) {
                String name = e.getKey();
                String cap = Character.toUpperCase(name.charAt(0)) + name.substring(1);
                headers.put("X-RateLimit-Remaining-" + cap, Long.toString(e.getValue().longValue()));
            }
        }
        return headers;
    }
}
