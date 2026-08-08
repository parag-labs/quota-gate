package com.quotagate;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.Map;

import org.junit.jupiter.api.Test;

import com.quotagate.Limiter.Decision;
import com.quotagate.Limiter.Reservation;
import com.quotagate.Limiter.Slot;
import com.quotagate.Rules.LimitRule;
import com.quotagate.Rules.Scope;
import com.quotagate.Store.BucketWindow;
import com.quotagate.Store.PreciseWindow;

class QuotaGateTest {

    private static Limiter tokenLimiter() {
        return new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxTokens(1000).scope(Scope.GLOBAL).build()));
    }

    // ---- window engine ----

    @Test
    void bucketMemoryIsBoundedRegardlessOfTraffic() {
        BucketWindow w = new BucketWindow(100, 1);
        for (int ts = 0; ts < 150; ts++) {
            for (int j = 0; j < 1000; j++) {
                w.add(ts, 10, 1, 0);
            }
        }
        assertTrue(w.bucketCount() <= 102);
    }

    @Test
    void bucketCountsRecentAndDropsOld() {
        BucketWindow w = new BucketWindow(60, 1);
        w.add(0, 100, 1, 0);
        assertEquals(100, w.snapshot(30)[Store.TOKENS]);
        assertEquals(0, w.snapshot(61)[Store.TOKENS]);
    }

    @Test
    void preciseWindowIsExact() {
        PreciseWindow w = new PreciseWindow(60);
        w.add(0, 10, 1, 0);
        w.add(59, 10, 1, 0);
        assertEquals(2, w.snapshot(59.5)[Store.REQUESTS]);
        assertEquals(1, w.snapshot(60.5)[Store.REQUESTS]);
    }

    // ---- basic allow / deny ----

    @Test
    void allowsUnderTheLimit() {
        Decision d = tokenLimiter().tryAcquire("gpt-4o", 100, 0, null, null, 0.0);
        assertTrue(d.allowed);
        assertEquals(900, d.remaining.get("tokens"));
    }

    @Test
    void deniesOverTokenCapAndRecoversAfterRetryAfter() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxTokens(100).build()));
        assertTrue(limiter.tryAcquire("gpt-4o", 100, 0, null, null, 1000.0).allowed);
        Decision d = limiter.tryAcquire("gpt-4o", 1, 0, null, null, 1000.0);
        assertFalse(d.allowed);
        assertTrue(d.retryAfter > 0);
        assertTrue(limiter.tryAcquire("gpt-4o", 1, 0, null, null, 1000.0 + d.retryAfter).allowed);
    }

    @Test
    void deniesOverRequestCap() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(1).build()));
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, null, null, 0.0).allowed);
        assertFalse(limiter.tryAcquire("gpt-4o", 0, 0, null, null, 0.0).allowed);
    }

    @Test
    void multipleWindowsMinuteOkButDailyTrips() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(5).name("per-min").build(),
                LimitRule.builder("gpt-4o", 86_400).maxRequests(8).name("per-day").build()));
        for (int i = 0; i < 5; i++) {
            assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, null, null, 0.0).allowed);
        }
        assertFalse(limiter.tryAcquire("gpt-4o", 0, 0, null, null, 0.0).allowed);
        for (int i = 0; i < 3; i++) {
            assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, null, null, 61.0 + i).allowed);
        }
        Decision d = limiter.tryAcquire("gpt-4o", 0, 0, null, null, 64.0);
        assertFalse(d.allowed);
        assertEquals("per-day", d.trippedRule.name);
    }

    @Test
    void retryAfterIsPreciseInExactMode() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(1).precise(true).build()));
        limiter.tryAcquire("gpt-4o", 0, 0, null, null, 1000.0);
        Decision d = limiter.tryAcquire("gpt-4o", 0, 0, null, null, 1000.0);
        assertTrue(Math.abs(d.retryAfter - 60) < 1e-6);
    }

    // ---- scopes ----

    @Test
    void tenantScopeIsolatesTenants() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(2).scope(Scope.TENANT).build()));
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0).allowed);
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0).allowed);
        assertFalse(limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0).allowed);
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "globex", null, 0.0).allowed);
    }

    @Test
    void userScopeIsolatesUsersWithinATenant() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(1).scope(Scope.USER).build()));
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "acme", "ann", 0.0).allowed);
        assertFalse(limiter.tryAcquire("gpt-4o", 0, 0, "acme", "ann", 0.0).allowed);
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "acme", "bob", 0.0).allowed);
    }

    @Test
    void toughestApplicableRuleWins() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(1000).scope(Scope.TENANT).name("tenant").build(),
                LimitRule.builder("gpt-4o", 60).maxRequests(1).scope(Scope.GLOBAL).name("fleet").build()));
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0).allowed);
        Decision d = limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0);
        assertFalse(d.allowed);
        assertEquals("fleet", d.trippedRule.name);
        assertEquals(Scope.GLOBAL, d.scope);
    }

    // ---- reserve -> commit / refund ----

    @Test
    void reserveConsumesEstimatedHeadroom() {
        Limiter limiter = tokenLimiter();
        Reservation res = limiter.reserve("gpt-4o", 800, 0, null, null, 0.0);
        assertNotNull(res);
        assertFalse(limiter.tryAcquire("gpt-4o", 300, 0, null, null, 0.0).allowed);
    }

    @Test
    void commitReconcilesDownAndFreesCapacity() {
        Limiter limiter = tokenLimiter();
        Reservation res = limiter.reserve("gpt-4o", 800, 0, null, null, 0.0);
        limiter.commit(res, 100.0, null);
        assertTrue(limiter.tryAcquire("gpt-4o", 300, 0, null, null, 0.0).allowed);
    }

    @Test
    void commitReconcilesUp() {
        Limiter limiter = tokenLimiter();
        Reservation res = limiter.reserve("gpt-4o", 100, 0, null, null, 0.0);
        limiter.commit(res, 900.0, null);
        assertFalse(limiter.tryAcquire("gpt-4o", 200, 0, null, null, 0.0).allowed);
    }

    @Test
    void refundReturnsEverythingOnAFailedCall() {
        Limiter limiter = tokenLimiter();
        Reservation res = limiter.reserve("gpt-4o", 900, 0, null, null, 0.0);
        limiter.refund(res);
        assertTrue(limiter.tryAcquire("gpt-4o", 1000, 0, null, null, 0.0).allowed);
    }

    // ---- concurrency ----

    @Test
    void slotsCapInFlightRequests() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxConcurrent(2).scope(Scope.TENANT).build()));
        assertTrue(limiter.acquireSlot("gpt-4o", "acme", null).ok);
        assertTrue(limiter.acquireSlot("gpt-4o", "acme", null).ok);
        assertFalse(limiter.acquireSlot("gpt-4o", "acme", null).ok);
    }

    @Test
    void releasingASlotFreesCapacity() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxConcurrent(1).scope(Scope.TENANT).build()));
        Slot a = limiter.acquireSlot("gpt-4o", "acme", null);
        assertFalse(limiter.acquireSlot("gpt-4o", "acme", null).ok);
        a.release();
        assertTrue(limiter.acquireSlot("gpt-4o", "acme", null).ok);
    }

    @Test
    void slotCloseReleases() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxConcurrent(1).scope(Scope.TENANT).build()));
        try (Slot ignored = limiter.acquireSlot("gpt-4o", "acme", null)) {
            assertFalse(limiter.acquireSlot("gpt-4o", "acme", null).ok);
        }
        assertTrue(limiter.acquireSlot("gpt-4o", "acme", null).ok);
    }

    // ---- headers, cost, fallback, config ----

    @Test
    void headersRenderBackpressure() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(1).maxTokens(1000).build()));
        limiter.tryAcquire("gpt-4o", 10, 0, null, null, 0.0);
        Decision d = limiter.tryAcquire("gpt-4o", 10, 0, null, null, 0.0);
        Map<String, String> headers = Limiter.rateLimitHeaders(d, null);
        assertTrue(headers.containsKey("Retry-After"));
        assertEquals("1", headers.get("X-RateLimit-Limit-Requests"));
        assertEquals("1000", headers.get("X-RateLimit-Limit-Tokens"));
    }

    @Test
    void costBasedRuleCapsSpend() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxCost(1.0).build()));
        double cost = Pricing.estimateCost("gpt-4o", 0, 200_000); // $2.00
        Decision d = limiter.tryAcquire("gpt-4o", 0, cost, null, null, 0.0);
        assertFalse(d.allowed);
    }

    @Test
    void fallbackIsSuggestedAndUsedOnDenial() {
        Limiter limiter = new Limiter(
                List.of(
                        LimitRule.builder("gpt-4o", 60).maxRequests(0).build(),
                        LimitRule.builder("gpt-4o-mini", 60).maxRequests(100).build()),
                new Store.InMemoryStore(), null, Map.of("gpt-4o", "gpt-4o-mini"));
        Decision d = limiter.tryAcquire("gpt-4o", 0, 0, null, null, 0.0);
        assertFalse(d.allowed);
        assertEquals("gpt-4o-mini", d.suggestedFallback);

        Map.Entry<Decision, String> chosen = limiter.acquireOrFallback("gpt-4o", 0, 0, null, null, 0.0);
        assertTrue(chosen.getKey().allowed);
        assertEquals("gpt-4o-mini", chosen.getValue());
    }

    @Test
    void unknownModelHasNoRulesAndIsAllowed() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(1).build()));
        assertTrue(limiter.tryAcquire("some-other-model", 10_000, 0, null, null, 0.0).allowed);
    }

    @Test
    void rulesLoadFromJson() {
        String text = "{ \"rules\": [ { \"model\": \"gpt-4o\", \"scope\": \"tenant\", "
                + "\"window_seconds\": 60, \"max_requests\": 2 } ] }";
        List<LimitRule> rules = Rules.fromJson(text);
        assertEquals(1, rules.size());
        assertEquals(Scope.TENANT, rules.get(0).scope);
        Limiter limiter = new Limiter(rules);
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0).allowed);
        assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0).allowed);
        assertFalse(limiter.tryAcquire("gpt-4o", 0, 0, "acme", null, 0.0).allowed);
    }

    @Test
    void reserveReturnsNullWhenDenied() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(0).build()));
        assertNull(limiter.reserve("gpt-4o", 0, 0, null, null, 0.0));
    }
}
