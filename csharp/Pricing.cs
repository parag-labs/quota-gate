namespace QuotaGate;

/// <summary>
/// Optional helper to turn token counts into a dollar cost for cost-based rules.
/// Mirrors the token-lens price table so a MaxCost rule can cap spend, not just
/// token volume. Pass the result as cost: to the limiter, or supply your own.
/// </summary>
public static class Pricing
{
    // Illustrative list prices (USD per 1M tokens): (input, output).
    public static readonly IReadOnlyDictionary<string, (double Input, double Output)> Prices =
        new Dictionary<string, (double, double)>
        {
            ["gpt-4o"] = (2.50, 10.00),
            ["gpt-4o-mini"] = (0.15, 0.60),
            ["o3-mini"] = (1.10, 4.40),
            ["claude-3.7-sonnet"] = (3.00, 15.00),
            ["claude-3.5-haiku"] = (0.80, 4.00),
            ["gemini-1.5-pro"] = (1.25, 5.00),
            ["llama-3.3-70b"] = (0.20, 0.20),
        };

    public static double EstimateCost(string model, long inputTokens, long outputTokens)
    {
        if (!Prices.TryGetValue(model, out var p))
        {
            throw new KeyNotFoundException($"unknown model '{model}'");
        }

        var cost = inputTokens / 1_000_000.0 * p.Input + outputTokens / 1_000_000.0 * p.Output;
        return Math.Round(cost, 6);
    }
}
