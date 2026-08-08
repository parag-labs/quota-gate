"""Optional helper to turn token counts into a dollar cost for cost-based rules.

Kept deliberately small - this mirrors the pricing table in token-lens so a
``max_cost`` rule can cap spend, not just token volume. Pass the result as
``cost=`` to the limiter, or supply your own number.
"""

from __future__ import annotations

# Illustrative list prices (USD per 1M tokens): (input, output).
PRICES: dict[str, tuple[float, float]] = {
    "gpt-4o": (2.50, 10.00),
    "gpt-4o-mini": (0.15, 0.60),
    "o3-mini": (1.10, 4.40),
    "claude-3.7-sonnet": (3.00, 15.00),
    "claude-3.5-haiku": (0.80, 4.00),
    "gemini-1.5-pro": (1.25, 5.00),
    "llama-3.3-70b": (0.20, 0.20),
}


class UnknownModelError(KeyError):
    pass


def estimate_cost(model: str, input_tokens: int, output_tokens: int) -> float:
    price = PRICES.get(model)
    if price is None:
        raise UnknownModelError(model)
    return round(input_tokens / 1_000_000 * price[0] + output_tokens / 1_000_000 * price[1], 6)
