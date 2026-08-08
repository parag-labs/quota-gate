package com.quotagate;

import java.util.Map;

/**
 * Optional helper to turn token counts into a dollar cost for cost-based rules.
 * Mirrors the token-lens price table so a maxCost rule can cap spend, not just
 * token volume.
 */
public final class Pricing {

    private Pricing() {
    }

    // Illustrative list prices (USD per 1M tokens): {input, output}.
    private static final Map<String, double[]> PRICES = Map.ofEntries(
            // OpenAI
            Map.entry("gpt-4o", new double[] {2.5, 10.0}),
            Map.entry("gpt-4o-mini", new double[] {0.15, 0.6}),
            Map.entry("gpt-4.1", new double[] {2.0, 8.0}),
            Map.entry("gpt-4.1-mini", new double[] {0.4, 1.6}),
            Map.entry("gpt-4.1-nano", new double[] {0.1, 0.4}),
            Map.entry("o3", new double[] {2.0, 8.0}),
            Map.entry("o3-mini", new double[] {1.1, 4.4}),
            Map.entry("o4-mini", new double[] {1.1, 4.4}),
            // Anthropic
            Map.entry("claude-opus-4", new double[] {15.0, 75.0}),
            Map.entry("claude-sonnet-4", new double[] {3.0, 15.0}),
            Map.entry("claude-3.7-sonnet", new double[] {3.0, 15.0}),
            Map.entry("claude-3.5-sonnet", new double[] {3.0, 15.0}),
            Map.entry("claude-3.5-haiku", new double[] {0.8, 4.0}),
            Map.entry("claude-3-haiku", new double[] {0.25, 1.25}),
            // Google
            Map.entry("gemini-2.5-pro", new double[] {1.25, 10.0}),
            Map.entry("gemini-2.5-flash", new double[] {0.3, 2.5}),
            Map.entry("gemini-2.0-flash", new double[] {0.1, 0.4}),
            Map.entry("gemini-1.5-pro", new double[] {1.25, 5.0}),
            Map.entry("gemini-1.5-flash", new double[] {0.075, 0.3}),
            // Meta Llama
            Map.entry("llama-3.3-70b", new double[] {0.2, 0.2}),
            Map.entry("llama-3.1-405b", new double[] {3.5, 3.5}),
            Map.entry("llama-3.1-8b", new double[] {0.05, 0.05}),
            // Mistral
            Map.entry("mistral-large", new double[] {2.0, 6.0}),
            Map.entry("mistral-small", new double[] {0.2, 0.6}),
            // DeepSeek
            Map.entry("deepseek-chat", new double[] {0.27, 1.1}),
            Map.entry("deepseek-reasoner", new double[] {0.55, 2.19}),
            // xAI
            Map.entry("grok-2", new double[] {2.0, 10.0}));

    public static double estimateCost(String model, long inputTokens, long outputTokens) {
        double[] p = PRICES.get(model);
        if (p == null) {
            throw new IllegalArgumentException("unknown model '" + model + "'");
        }
        double cost = inputTokens / 1_000_000.0 * p[0] + outputTokens / 1_000_000.0 * p[1];
        return Math.round(cost * 1_000_000.0) / 1_000_000.0;
    }
}
