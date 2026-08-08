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
            // OpenAI
            ["gpt-4o"] = (2.50, 10.00),
            ["gpt-4o-mini"] = (0.15, 0.60),
            ["gpt-4.1"] = (2.00, 8.00),
            ["gpt-4.1-mini"] = (0.40, 1.60),
            ["gpt-4.1-nano"] = (0.10, 0.40),
            ["o3"] = (2.00, 8.00),
            ["o3-mini"] = (1.10, 4.40),
            ["o4-mini"] = (1.10, 4.40),
            // Anthropic
            ["claude-opus-4"] = (15.00, 75.00),
            ["claude-sonnet-4"] = (3.00, 15.00),
            ["claude-3.7-sonnet"] = (3.00, 15.00),
            ["claude-3.5-sonnet"] = (3.00, 15.00),
            ["claude-3.5-haiku"] = (0.80, 4.00),
            ["claude-3-haiku"] = (0.25, 1.25),
            // Google
            ["gemini-2.5-pro"] = (1.25, 10.00),
            ["gemini-2.5-flash"] = (0.30, 2.50),
            ["gemini-2.0-flash"] = (0.10, 0.40),
            ["gemini-1.5-pro"] = (1.25, 5.00),
            ["gemini-1.5-flash"] = (0.075, 0.30),
            // Meta Llama
            ["llama-3.3-70b"] = (0.20, 0.20),
            ["llama-3.1-405b"] = (3.50, 3.50),
            ["llama-3.1-8b"] = (0.05, 0.05),
            // Mistral
            ["mistral-large"] = (2.00, 6.00),
            ["mistral-small"] = (0.20, 0.60),
            // DeepSeek
            ["deepseek-chat"] = (0.27, 1.10),
            ["deepseek-reasoner"] = (0.55, 2.19),
            // xAI
            ["grok-2"] = (2.00, 10.00),
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
