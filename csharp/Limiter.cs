namespace QuotaGate;

public sealed class Decision
{
    public bool Allowed { get; init; }

    public string Model { get; init; } = "";

    public double RetryAfter { get; init; }

    public LimitRule? TrippedRule { get; init; }

    public Scope? Scope { get; init; }

    public IReadOnlyDictionary<string, double> Remaining { get; init; } = new Dictionary<string, double>();

    public string? SuggestedFallback { get; init; }
}

public sealed class Reservation
{
    public required string Model { get; init; }

    public double Tokens { get; set; }

    public double Cost { get; set; }

    public required List<(string Key, object Handle)> Handles { get; set; }

    public bool Committed { get; set; }
}

/// <summary>A held concurrency slot. Dispose (or Release) to give it back.</summary>
public sealed class Slot : IDisposable
{
    private readonly Limiter _limiter;
    private List<string> _keys;

    internal Slot(Limiter limiter, List<string> keys, bool ok, LimitRule? tripped = null)
    {
        _limiter = limiter;
        _keys = keys;
        Ok = ok;
        TrippedRule = tripped;
    }

    public bool Ok { get; }

    public LimitRule? TrippedRule { get; }

    public void Release()
    {
        foreach (var k in _keys)
        {
            _limiter.Store.ReleaseConcurrency(k);
        }

        _keys = new List<string>();
    }

    public void Dispose() => Release();
}

/// <summary>
/// The gate: evaluate provider-style limits before a call, reserve/reconcile
/// tokens, cap concurrency, and emit standard back-pressure signals.
/// </summary>
public sealed class Limiter
{
    private readonly List<LimitRule> _rules;
    private readonly Func<double> _clock;
    private readonly Dictionary<string, string> _fallbacks;
    private readonly Dictionary<string, List<(int Index, LimitRule Rule)>> _byModel = new();

    public Limiter(IEnumerable<LimitRule> rules, IStore? store = null,
        Func<double>? clock = null, IReadOnlyDictionary<string, string>? fallbacks = null)
    {
        _rules = rules.ToList();
        Store = store ?? new InMemoryStore();
        _clock = clock ?? (() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() / 1000.0);
        _fallbacks = fallbacks is null ? new() : new Dictionary<string, string>(fallbacks);
        for (int i = 0; i < _rules.Count; i++)
        {
            if (!_byModel.TryGetValue(_rules[i].Model, out var list))
            {
                list = new List<(int, LimitRule)>();
                _byModel[_rules[i].Model] = list;
            }

            list.Add((i, _rules[i]));
        }
    }

    public IStore Store { get; }

    private List<(int Index, LimitRule Rule)> Applicable(string model)
    {
        var result = new List<(int, LimitRule)>();
        if (_byModel.TryGetValue(model, out var exact))
        {
            result.AddRange(exact);
        }

        if (_byModel.TryGetValue("*", out var wild))
        {
            result.AddRange(wild);
        }

        return result;
    }

    private static string Key(int index, LimitRule r, string? tenant, string? user) => r.Scope switch
    {
        Scope.Global => $"{index}|g",
        Scope.Tenant => $"{index}|t|{tenant}",
        _ => $"{index}|u|{tenant}|{user}",
    };

    public Decision TryAcquire(string model, double tokens = 0, double cost = 0,
        string? tenant = null, string? user = null, double? now = null, bool record = true)
    {
        double t = now ?? _clock();
        var rules = Applicable(model);

        double worstRetry = -1;
        LimitRule? worstRule = null;
        foreach (var (i, r) in rules)
        {
            if (!r.HasUsageLimit)
            {
                continue;
            }

            string key = Key(i, r, tenant, user);
            var (usedT, usedR, usedC) = Store.Snapshot(key, t, r.WindowSeconds, r.BucketSeconds);
            var breached = new List<(int Dim, double Over)>();
            if (r.MaxTokens is double mt && usedT + tokens > mt + 1e-9)
            {
                breached.Add((Dim.Tokens, usedT + tokens - mt));
            }

            if (r.MaxRequests is double mr && usedR + 1 > mr + 1e-9)
            {
                breached.Add((Dim.Requests, usedR + 1 - mr));
            }

            if (r.MaxCost is double mc && usedC + cost > mc + 1e-9)
            {
                breached.Add((Dim.Cost, usedC + cost - mc));
            }

            if (breached.Count > 0)
            {
                double ra = 0;
                foreach (var (dim, over) in breached)
                {
                    ra = Math.Max(ra, Store.TimeToFree(key, t, r.WindowSeconds, r.BucketSeconds, over, dim));
                }

                if (ra > worstRetry)
                {
                    worstRetry = ra;
                    worstRule = r;
                }
            }
        }

        if (worstRule is not null)
        {
            return new Decision
            {
                Allowed = false,
                Model = model,
                RetryAfter = worstRetry,
                TrippedRule = worstRule,
                Scope = worstRule.Scope,
                SuggestedFallback = _fallbacks.GetValueOrDefault(model),
            };
        }

        if (record)
        {
            foreach (var (i, r) in rules)
            {
                if (!r.HasUsageLimit)
                {
                    continue;
                }

                Store.Add(Key(i, r, tenant, user), t, r.WindowSeconds, r.BucketSeconds, tokens, 1.0, cost);
            }
        }

        return new Decision
        {
            Allowed = true,
            Model = model,
            Remaining = Remaining(model, tenant, user, t),
        };
    }

    private Dictionary<string, double> Remaining(string model, string? tenant, string? user, double now)
    {
        var rem = new Dictionary<string, double>();

        void Tighten(string name, double value)
        {
            rem[name] = rem.TryGetValue(name, out var cur) ? Math.Min(cur, value) : value;
        }

        foreach (var (i, r) in Applicable(model))
        {
            string key = Key(i, r, tenant, user);
            var (usedT, usedR, usedC) = Store.Snapshot(key, now, r.WindowSeconds, r.BucketSeconds);
            if (r.MaxTokens is double mt)
            {
                Tighten("tokens", mt - usedT);
            }

            if (r.MaxRequests is double mr)
            {
                Tighten("requests", mr - usedR);
            }

            if (r.MaxCost is double mc)
            {
                Tighten("cost", mc - usedC);
            }
        }

        return rem.ToDictionary(kv => kv.Key, kv => Math.Max(0.0, kv.Value));
    }

    // ---- reserve -> commit / refund ----

    public (Decision Decision, Reservation? Reservation) Reserve(string model, double tokens = 0, double cost = 0,
        string? tenant = null, string? user = null, double? now = null)
    {
        double t = now ?? _clock();
        var decision = TryAcquire(model, tokens, cost, tenant, user, t, record: false);
        if (!decision.Allowed)
        {
            return (decision, null);
        }

        var handles = new List<(string, object)>();
        foreach (var (i, r) in Applicable(model))
        {
            if (!r.HasUsageLimit)
            {
                continue;
            }

            string key = Key(i, r, tenant, user);
            var h = Store.Add(key, t, r.WindowSeconds, r.BucketSeconds, tokens, 1.0, cost);
            handles.Add((key, h));
        }

        return (decision, new Reservation { Model = model, Tokens = tokens, Cost = cost, Handles = handles });
    }

    public void Commit(Reservation reservation, double? actualTokens = null, double? actualCost = null)
    {
        if (reservation.Committed)
        {
            return;
        }

        double dt = actualTokens is null ? 0.0 : actualTokens.Value - reservation.Tokens;
        double dc = actualCost is null ? 0.0 : actualCost.Value - reservation.Cost;
        foreach (var (key, handle) in reservation.Handles)
        {
            Store.Adjust(key, handle, dt, 0.0, dc);
        }

        reservation.Tokens += dt;
        reservation.Cost += dc;
        reservation.Committed = true;
    }

    public void Refund(Reservation reservation)
    {
        if (reservation.Committed)
        {
            return;
        }

        foreach (var (key, handle) in reservation.Handles)
        {
            Store.Adjust(key, handle, -reservation.Tokens, -1.0, -reservation.Cost);
        }

        reservation.Handles = new List<(string, object)>();
        reservation.Committed = true;
    }

    // ---- concurrency slots ----

    public Slot AcquireSlot(string model, string? tenant = null, string? user = null)
    {
        var acquired = new List<string>();
        foreach (var (i, r) in Applicable(model))
        {
            if (r.MaxConcurrent is not int limit)
            {
                continue;
            }

            string key = "conc|" + Key(i, r, tenant, user);
            if (!Store.TryAddConcurrency(key, limit))
            {
                foreach (var k in acquired)
                {
                    Store.ReleaseConcurrency(k);
                }

                return new Slot(this, new List<string>(), ok: false, tripped: r);
            }

            acquired.Add(key);
        }

        return new Slot(this, acquired, ok: true);
    }

    // ---- graceful degradation ----

    public (Decision Decision, string Model) AcquireOrFallback(string model, double tokens = 0, double cost = 0,
        string? tenant = null, string? user = null, double? now = null)
    {
        var decision = TryAcquire(model, tokens, cost, tenant, user, now);
        if (decision.Allowed)
        {
            return (decision, model);
        }

        if (_fallbacks.TryGetValue(model, out var fallback))
        {
            var alt = TryAcquire(fallback, tokens, cost, tenant, user, now);
            if (alt.Allowed)
            {
                return (alt, fallback);
            }
        }

        return (decision, model);
    }

    public static Dictionary<string, string> RateLimitHeaders(Decision decision, LimitRule? rule = null)
    {
        rule ??= decision.TrippedRule;
        var headers = new Dictionary<string, string>();
        if (decision.RetryAfter > 0)
        {
            headers["Retry-After"] = ((long)Math.Ceiling(decision.RetryAfter)).ToString();
        }

        if (rule is not null)
        {
            if (rule.MaxRequests is double mr)
            {
                headers["X-RateLimit-Limit-Requests"] = ((long)mr).ToString();
            }

            if (rule.MaxTokens is double mt)
            {
                headers["X-RateLimit-Limit-Tokens"] = ((long)mt).ToString();
            }
        }

        foreach (var (name, value) in decision.Remaining)
        {
            headers[$"X-RateLimit-Remaining-{char.ToUpperInvariant(name[0]) + name[1..]}"] = ((long)value).ToString();
        }

        return headers;
    }
}
