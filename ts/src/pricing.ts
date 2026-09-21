/**
 * Optional helper to turn token counts into a dollar cost for cost-based rules.
 *
 * Mirrors the pricing table in token-lens so a `maxCost` rule can cap spend, not
 * just token volume. Pass the result as `cost` to the limiter, or supply your own.
 *
 * Prices are illustrative published list prices (USD per 1M tokens) and drift as
 * vendors change them - treat this table as a starting default, not a source of
 * truth.
 */

/** Illustrative list prices (USD per 1M tokens): `[input, output]`. */
export const PRICES: Record<string, [number, number]> = {
  // OpenAI
  "gpt-4o": [2.5, 10.0],
  "gpt-4o-mini": [0.15, 0.6],
  "gpt-4.1": [2.0, 8.0],
  "gpt-4.1-mini": [0.4, 1.6],
  "gpt-4.1-nano": [0.1, 0.4],
  o3: [2.0, 8.0],
  "o3-mini": [1.1, 4.4],
  "o4-mini": [1.1, 4.4],
  // Anthropic
  "claude-opus-4": [15.0, 75.0],
  "claude-sonnet-4": [3.0, 15.0],
  "claude-3.7-sonnet": [3.0, 15.0],
  "claude-3.5-sonnet": [3.0, 15.0],
  "claude-3.5-haiku": [0.8, 4.0],
  "claude-3-haiku": [0.25, 1.25],
  // Google
  "gemini-2.5-pro": [1.25, 10.0],
  "gemini-2.5-flash": [0.3, 2.5],
  "gemini-2.0-flash": [0.1, 0.4],
  "gemini-1.5-pro": [1.25, 5.0],
  "gemini-1.5-flash": [0.075, 0.3],
  // Meta Llama
  "llama-3.3-70b": [0.2, 0.2],
  "llama-3.1-405b": [3.5, 3.5],
  "llama-3.1-8b": [0.05, 0.05],
  // Mistral
  "mistral-large": [2.0, 6.0],
  "mistral-small": [0.2, 0.6],
  // DeepSeek
  "deepseek-chat": [0.27, 1.1],
  "deepseek-reasoner": [0.55, 2.19],
  // xAI
  "grok-2": [2.0, 10.0],
};

/** Raised when a model has no entry in {@link PRICES}. */
export class UnknownModelError extends Error {
  constructor(model: string) {
    super(model);
    this.name = "UnknownModelError";
  }
}

/**
 * Estimates the dollar cost of a call from its input/output token counts,
 * rounded to 6 decimal places. Throws {@link UnknownModelError} for an unpriced
 * model.
 */
export function estimateCost(model: string, inputTokens: number, outputTokens: number): number {
  const price = PRICES[model];
  if (price === undefined) {
    throw new UnknownModelError(model);
  }
  const raw = (inputTokens / 1_000_000) * price[0] + (outputTokens / 1_000_000) * price[1];
  return Math.round(raw * 1e6) / 1e6;
}
