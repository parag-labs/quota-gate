package com.quotagate;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.ArrayDeque;
import java.util.Deque;
import java.util.List;
import java.util.Random;

import org.junit.jupiter.api.Test;

import com.quotagate.Rules.LimitRule;
import com.quotagate.Store.BucketWindow;

/**
 * Stress suite: prove the design's load-bearing properties in the Java port too -
 * bounded memory under heavy traffic, correct enforcement with out-of-order
 * timestamps, and a high-volume sliding-window soak.
 */
class QuotaGateStressTest {

    @Test
    void bucketMemoryIsBoundedUnderHeavyTraffic() {
        // A 1-hour window in 60 buckets: memory stays ~60 counters regardless of
        // traffic, so we exercise the window directly.
        BucketWindow w = new BucketWindow(3600, 60);
        for (int i = 0; i < 200_000; i++) {
            w.add(i * 0.036, 1, 1, 0);
        }
        assertTrue(w.bucketCount() <= 62, "bucket count grew to " + w.bucketCount());
    }

    @Test
    void enforcementIsCorrectWithOutOfOrderTimestamps() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(10).build()));
        double[] stamps = {1000.0, 1002.0, 1001.0, 1005.0, 1003.0, 1004.0, 1002.5, 1001.5, 1000.5, 1004.5};
        for (double ts : stamps) {
            assertTrue(limiter.tryAcquire("gpt-4o", 0, 0, null, null, ts).allowed);
        }
        // The 11th event anywhere in the window is denied regardless of arrival order.
        assertFalse(limiter.tryAcquire("gpt-4o", 0, 0, null, null, 1002.7).allowed);
    }

    @Test
    void preciseModeAdmitsExactlyTheCap() {
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(100).precise(true).build()));
        int allowed = 0;
        for (int i = 0; i < 500; i++) {
            if (limiter.tryAcquire("gpt-4o", 0, 0, null, null, 1000.0).allowed) {
                allowed++;
            }
        }
        assertTrue(allowed == 100);
    }

    @Test
    void highVolumeSoakKeepsTheSlidingCap() {
        // A sliding 60s window approximated by 1s buckets may admit a hair over the
        // cap at bucket edges - the documented accuracy trade-off. Assert the real
        // invariant: in any sliding 60s window, admissions stay within cap+tolerance.
        final int cap = 1000;
        final int buckets = 60;
        final int tolerance = cap / buckets + 1;
        Random rng = new Random(0);
        Limiter limiter = new Limiter(List.of(
                LimitRule.builder("gpt-4o", 60).maxRequests(cap).bucketsPerWindow(buckets).build()));

        Deque<Double> admitted = new ArrayDeque<>();
        int worst = 0;
        double t = 0.0;
        for (int i = 0; i < 300_000; i++) {
            t += rng.nextDouble() * 0.01;
            if (limiter.tryAcquire("gpt-4o", 0, 0, null, null, t).allowed) {
                admitted.addLast(t);
            }
            while (!admitted.isEmpty() && admitted.peekFirst() <= t - 60) {
                admitted.pollFirst();
            }
            worst = Math.max(worst, admitted.size());
            assertTrue(admitted.size() <= cap + tolerance);
        }
        assertTrue(worst >= cap * 0.9); // genuinely pushes against the cap
    }
}
