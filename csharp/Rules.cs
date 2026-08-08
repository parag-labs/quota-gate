using System.Text.Json;

namespace QuotaGate;

/// <summary>Where a limit is enforced. A request is keyed differently per scope.</summary>
public enum Scope
{
    Global,
    Tenant,
    User,
}

/// <summary>One provider-style limit. Any subset of the Max* dimensions may be set.</summary>
public sealed record LimitRule(
    string Model,
    double WindowSeconds,
    double? MaxTokens = null,
    double? MaxRequests = null,
    double? MaxCost = null,
    int? MaxConcurrent = null,
    Scope Scope = Scope.Global,
    int BucketsPerWindow = 60,
    bool Precise = false,
    string? Name = null)
{
    /// <summary>0 selects the exact per-event log; otherwise window/BucketsPerWindow.</summary>
    public double BucketSeconds => Precise ? 0.0 : WindowSeconds / Math.Max(1, BucketsPerWindow);

    public string Label => Name ?? $"{Model}:{Scope.ToString().ToLowerInvariant()}:{(long)WindowSeconds}s";

    public bool HasUsageLimit => MaxTokens is not null || MaxRequests is not null || MaxCost is not null;
}

public static class RuleLoader
{
    public static IReadOnlyList<LimitRule> FromJson(string text)
    {
        using var doc = JsonDocument.Parse(text);
        var rules = new List<LimitRule>();
        foreach (var row in doc.RootElement.GetProperty("rules").EnumerateArray())
        {
            rules.Add(new LimitRule(
                Model: row.GetProperty("model").GetString()!,
                WindowSeconds: row.GetProperty("window_seconds").GetDouble(),
                MaxTokens: OptDouble(row, "max_tokens"),
                MaxRequests: OptDouble(row, "max_requests"),
                MaxCost: OptDouble(row, "max_cost"),
                MaxConcurrent: OptInt(row, "max_concurrent"),
                Scope: row.TryGetProperty("scope", out var s)
                    ? Enum.Parse<Scope>(Capitalize(s.GetString()!), ignoreCase: true)
                    : Scope.Global,
                BucketsPerWindow: OptInt(row, "buckets_per_window") ?? 60,
                Precise: row.TryGetProperty("precise", out var p) && p.GetBoolean(),
                Name: row.TryGetProperty("name", out var n) ? n.GetString() : null));
        }

        return rules;
    }

    private static double? OptDouble(JsonElement row, string name)
        => row.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetDouble() : null;

    private static int? OptInt(JsonElement row, string name)
        => row.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetInt32() : null;

    private static string Capitalize(string s) => char.ToUpperInvariant(s[0]) + s[1..];
}
