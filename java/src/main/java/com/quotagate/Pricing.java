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
    private static final Map<String, double[]> PRICES = Map.of(
            "gpt-4o", new double[] {2.50, 10.00},
            "gpt-4o-mini", new double[] {0.15, 0.60},
            "o3-mini", new double[] {1.10, 4.40},
            "claude-3.7-sonnet", new double[] {3.00, 15.00},
            "claude-3.5-haiku", new double[] {0.80, 4.00},
            "gemini-1.5-pro", new double[] {1.25, 5.00},
            "llama-3.3-70b", new double[] {0.20, 0.20});

    public static double estimateCost(String model, long inputTokens, long outputTokens) {
        double[] p = PRICES.get(model);
        if (p == null) {
            throw new IllegalArgumentException("unknown model '" + model + "'");
        }
        double cost = inputTokens / 1_000_000.0 * p[0] + outputTokens / 1_000_000.0 * p[1];
        return Math.round(cost * 1_000_000.0) / 1_000_000.0;
    }
}
