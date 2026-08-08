using Xunit;

namespace QuotaGate.Tests;

// Stress suite: prove the design's load-bearing properties in the C# port too -
// bounded memory under heavy traffic, correct enforcement with out-of-order
// timestamps, and a high-volume sliding-window soak.
public class QuotaGateStressTests
{
    [Fact]
    public void BucketMemoryIsBoundedUnderHeavyTraffic()
    {
        // A 1-hour window in 60 buckets: memory must stay ~60 counters no matter
        // how many events flow through, so we exercise the window directly.
        var w = new BucketWindow(window: 3600, bucket: 60);
        for (var i = 0; i < 200_000; i++)
        {
            w.Add(i * 0.036, 1, 1, 0);
        }

        Assert.True(w.BucketCount <= 62, $"bucket count grew to {w.BucketCount}");
    }

    [Fact]
    public void EnforcementIsCorrectWithOutOfOrderTimestamps()
    {
        var limiter = new Limiter(new[] { new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 10) });
        double[] stamps = { 1000.0, 1002.0, 1001.0, 1005.0, 1003.0, 1004.0, 1002.5, 1001.5, 1000.5, 1004.5 };
        foreach (var ts in stamps)
        {
            Assert.True(limiter.TryAcquire("gpt-4o", now: ts).Allowed);
        }

        // The 11th event anywhere in the window is denied regardless of arrival order.
        Assert.False(limiter.TryAcquire("gpt-4o", now: 1002.7).Allowed);
    }

    [Fact]
    public void PreciseModeAdmitsExactlyTheCap()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 100, Precise: true),
        });
        var allowed = 0;
        for (var i = 0; i < 500; i++)
        {
            if (limiter.TryAcquire("gpt-4o", now: 1000.0).Allowed)
            {
                allowed++;
            }
        }

        Assert.Equal(100, allowed);
    }

    [Fact]
    public void HighVolumeSoakKeepsTheSlidingCap()
    {
        // A sliding 60s window approximated by 1s buckets may admit a hair over the
        // cap at bucket edges - the documented accuracy trade-off. We assert the real
        // invariant: in any sliding 60s window, admissions stay within cap+tolerance.
        const int cap = 1000;
        const int buckets = 60;
        const int tolerance = cap / buckets + 1;
        var rng = new Random(0);
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: cap, BucketsPerWindow: buckets),
        });

        var admitted = new Queue<double>();
        var worst = 0;
        var t = 0.0;
        for (var i = 0; i < 300_000; i++)
        {
            t += rng.NextDouble() * 0.01;
            if (limiter.TryAcquire("gpt-4o", now: t).Allowed)
            {
                admitted.Enqueue(t);
            }

            while (admitted.Count > 0 && admitted.Peek() <= t - 60)
            {
                admitted.Dequeue();
            }

            worst = Math.Max(worst, admitted.Count);
            Assert.True(admitted.Count <= cap + tolerance);
        }

        Assert.True(worst >= cap * 0.9); // genuinely pushes against the cap
    }
}
