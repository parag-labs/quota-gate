using Xunit;

namespace QuotaGate.Tests;

public class QuotaGateTests
{
    private static Limiter TokenLimiter()
        => new(new[] { new LimitRule("gpt-4o", WindowSeconds: 60, MaxTokens: 1000, Scope: Scope.Global) });

    // ---- window engine ----

    [Fact]
    public void BucketMemoryIsBoundedRegardlessOfTraffic()
    {
        var w = new BucketWindow(window: 100, bucket: 1);
        for (int ts = 0; ts < 150; ts++)
        {
            for (int j = 0; j < 1000; j++)
            {
                w.Add(ts, 10, 1, 0);
            }
        }

        Assert.True(w.BucketCount <= 102);
    }

    [Fact]
    public void BucketCountsRecentAndDropsOld()
    {
        var w = new BucketWindow(window: 60, bucket: 1);
        w.Add(0, 100, 1, 0);
        Assert.Equal(100, w.Snapshot(30).Item1);
        Assert.Equal(0, w.Snapshot(61).Item1);
    }

    [Fact]
    public void PreciseWindowIsExact()
    {
        var w = new PreciseWindow(window: 60);
        w.Add(0, 10, 1, 0);
        w.Add(59, 10, 1, 0);
        Assert.Equal(2, w.Snapshot(59.5).Item2);
        Assert.Equal(1, w.Snapshot(60.5).Item2);
    }

    // ---- basic allow / deny ----

    [Fact]
    public void AllowsUnderTheLimit()
    {
        var d = TokenLimiter().TryAcquire("gpt-4o", tokens: 100, now: 0);
        Assert.True(d.Allowed);
        Assert.Equal(900, d.Remaining["tokens"]);
    }

    [Fact]
    public void DeniesOverTokenCapAndRecoversAfterRetryAfter()
    {
        var limiter = new Limiter(new[] { new LimitRule("gpt-4o", WindowSeconds: 60, MaxTokens: 100) });
        Assert.True(limiter.TryAcquire("gpt-4o", tokens: 100, now: 1000).Allowed);
        var d = limiter.TryAcquire("gpt-4o", tokens: 1, now: 1000);
        Assert.False(d.Allowed);
        Assert.True(d.RetryAfter > 0);
        Assert.True(limiter.TryAcquire("gpt-4o", tokens: 1, now: 1000 + d.RetryAfter).Allowed);
    }

    [Fact]
    public void DeniesOverRequestCap()
    {
        var limiter = new Limiter(new[] { new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 1) });
        Assert.True(limiter.TryAcquire("gpt-4o", now: 0).Allowed);
        Assert.False(limiter.TryAcquire("gpt-4o", now: 0).Allowed);
    }

    [Fact]
    public void MultipleWindowsMinuteOkButDailyTrips()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 5, Name: "per-min"),
            new LimitRule("gpt-4o", WindowSeconds: 86_400, MaxRequests: 8, Name: "per-day"),
        });
        for (int i = 0; i < 5; i++)
        {
            Assert.True(limiter.TryAcquire("gpt-4o", now: 0).Allowed);
        }

        Assert.False(limiter.TryAcquire("gpt-4o", now: 0).Allowed);

        for (int i = 0; i < 3; i++)
        {
            Assert.True(limiter.TryAcquire("gpt-4o", now: 61 + i).Allowed);
        }

        var d = limiter.TryAcquire("gpt-4o", now: 64);
        Assert.False(d.Allowed);
        Assert.Equal("per-day", d.TrippedRule!.Name);
    }

    [Fact]
    public void RetryAfterIsPreciseInExactMode()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 1, Precise: true),
        });
        limiter.TryAcquire("gpt-4o", now: 1000);
        var d = limiter.TryAcquire("gpt-4o", now: 1000);
        Assert.True(Math.Abs(d.RetryAfter - 60) < 1e-6);
    }

    // ---- scopes ----

    [Fact]
    public void TenantScopeIsolatesTenants()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 2, Scope: Scope.Tenant),
        });
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0).Allowed);
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0).Allowed);
        Assert.False(limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0).Allowed);
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "globex", now: 0).Allowed);
    }

    [Fact]
    public void UserScopeIsolatesUsersWithinATenant()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 1, Scope: Scope.User),
        });
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "acme", user: "ann", now: 0).Allowed);
        Assert.False(limiter.TryAcquire("gpt-4o", tenant: "acme", user: "ann", now: 0).Allowed);
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "acme", user: "bob", now: 0).Allowed);
    }

    [Fact]
    public void ToughestApplicableRuleWins()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 1000, Scope: Scope.Tenant, Name: "tenant"),
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 1, Scope: Scope.Global, Name: "fleet"),
        });
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0).Allowed);
        var d = limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0);
        Assert.False(d.Allowed);
        Assert.Equal("fleet", d.TrippedRule!.Name);
        Assert.Equal(Scope.Global, d.Scope);
    }

    // ---- reserve -> commit / refund ----

    [Fact]
    public void ReserveConsumesEstimatedHeadroom()
    {
        var limiter = TokenLimiter();
        var (decision, res) = limiter.Reserve("gpt-4o", tokens: 800, now: 0);
        Assert.True(decision.Allowed);
        Assert.NotNull(res);
        Assert.False(limiter.TryAcquire("gpt-4o", tokens: 300, now: 0).Allowed);
    }

    [Fact]
    public void CommitReconcilesDownAndFreesCapacity()
    {
        var limiter = TokenLimiter();
        var (_, res) = limiter.Reserve("gpt-4o", tokens: 800, now: 0);
        limiter.Commit(res!, actualTokens: 100);
        Assert.True(limiter.TryAcquire("gpt-4o", tokens: 300, now: 0).Allowed);
    }

    [Fact]
    public void CommitReconcilesUp()
    {
        var limiter = TokenLimiter();
        var (_, res) = limiter.Reserve("gpt-4o", tokens: 100, now: 0);
        limiter.Commit(res!, actualTokens: 900);
        Assert.False(limiter.TryAcquire("gpt-4o", tokens: 200, now: 0).Allowed);
    }

    [Fact]
    public void RefundReturnsEverythingOnAFailedCall()
    {
        var limiter = TokenLimiter();
        var (_, res) = limiter.Reserve("gpt-4o", tokens: 900, now: 0);
        limiter.Refund(res!);
        Assert.True(limiter.TryAcquire("gpt-4o", tokens: 1000, now: 0).Allowed);
    }

    // ---- concurrency ----

    [Fact]
    public void SlotsCapInFlightRequests()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxConcurrent: 2, Scope: Scope.Tenant),
        });
        var a = limiter.AcquireSlot("gpt-4o", tenant: "acme");
        var b = limiter.AcquireSlot("gpt-4o", tenant: "acme");
        var c = limiter.AcquireSlot("gpt-4o", tenant: "acme");
        Assert.True(a.Ok);
        Assert.True(b.Ok);
        Assert.False(c.Ok);
    }

    [Fact]
    public void ReleasingASlotFreesCapacity()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxConcurrent: 1, Scope: Scope.Tenant),
        });
        var a = limiter.AcquireSlot("gpt-4o", tenant: "acme");
        Assert.False(limiter.AcquireSlot("gpt-4o", tenant: "acme").Ok);
        a.Release();
        Assert.True(limiter.AcquireSlot("gpt-4o", tenant: "acme").Ok);
    }

    [Fact]
    public void SlotDisposeReleases()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxConcurrent: 1, Scope: Scope.Tenant),
        });
        using (limiter.AcquireSlot("gpt-4o", tenant: "acme"))
        {
            Assert.False(limiter.AcquireSlot("gpt-4o", tenant: "acme").Ok);
        }

        Assert.True(limiter.AcquireSlot("gpt-4o", tenant: "acme").Ok);
    }

    // ---- headers, cost, fallback, config ----

    [Fact]
    public void HeadersRenderBackpressure()
    {
        var limiter = new Limiter(new[]
        {
            new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 1, MaxTokens: 1000),
        });
        limiter.TryAcquire("gpt-4o", tokens: 10, now: 0);
        var d = limiter.TryAcquire("gpt-4o", tokens: 10, now: 0);
        var headers = Limiter.RateLimitHeaders(d);
        Assert.Contains("Retry-After", headers.Keys);
        Assert.Equal("1", headers["X-RateLimit-Limit-Requests"]);
        Assert.Equal("1000", headers["X-RateLimit-Limit-Tokens"]);
    }

    [Fact]
    public void CostBasedRuleCapsSpend()
    {
        var limiter = new Limiter(new[] { new LimitRule("gpt-4o", WindowSeconds: 60, MaxCost: 1.0) });
        var cost = Pricing.EstimateCost("gpt-4o", inputTokens: 0, outputTokens: 200_000); // $2.00
        var d = limiter.TryAcquire("gpt-4o", cost: cost, now: 0);
        Assert.False(d.Allowed);
    }

    [Fact]
    public void FallbackIsSuggestedAndUsedOnDenial()
    {
        var limiter = new Limiter(
            new[]
            {
                new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 0),
                new LimitRule("gpt-4o-mini", WindowSeconds: 60, MaxRequests: 100),
            },
            fallbacks: new Dictionary<string, string> { ["gpt-4o"] = "gpt-4o-mini" });
        var d = limiter.TryAcquire("gpt-4o", now: 0);
        Assert.False(d.Allowed);
        Assert.Equal("gpt-4o-mini", d.SuggestedFallback);

        var (used, chosen) = limiter.AcquireOrFallback("gpt-4o", now: 0);
        Assert.True(used.Allowed);
        Assert.Equal("gpt-4o-mini", chosen);
    }

    [Fact]
    public void UnknownModelHasNoRulesAndIsAllowed()
    {
        var limiter = new Limiter(new[] { new LimitRule("gpt-4o", WindowSeconds: 60, MaxRequests: 1) });
        Assert.True(limiter.TryAcquire("some-other-model", tokens: 10_000, now: 0).Allowed);
    }

    [Fact]
    public void RulesLoadFromJson()
    {
        const string text = """
        { "rules": [ { "model": "gpt-4o", "scope": "tenant", "window_seconds": 60, "max_requests": 2 } ] }
        """;
        var rules = RuleLoader.FromJson(text);
        Assert.Single(rules);
        Assert.Equal(Scope.Tenant, rules[0].Scope);
        var limiter = new Limiter(rules);
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0).Allowed);
        Assert.True(limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0).Allowed);
        Assert.False(limiter.TryAcquire("gpt-4o", tenant: "acme", now: 0).Allowed);
    }
}
