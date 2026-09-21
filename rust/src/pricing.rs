//! Token-count to dollar-cost estimation using published list prices.

use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

/// Illustrative published list prices (USD per 1M tokens) as `(input, output)`.
/// They drift as vendors change them - a starting default, not a source of truth.
pub static PRICES: LazyLock<HashMap<&'static str, (f64, f64)>> = LazyLock::new(|| {
    HashMap::from([
        // OpenAI
        ("gpt-4o", (2.5, 10.0)),
        ("gpt-4o-mini", (0.15, 0.6)),
        ("gpt-4.1", (2.0, 8.0)),
        ("gpt-4.1-mini", (0.4, 1.6)),
        ("gpt-4.1-nano", (0.1, 0.4)),
        ("o3", (2.0, 8.0)),
        ("o3-mini", (1.1, 4.4)),
        ("o4-mini", (1.1, 4.4)),
        // Anthropic
        ("claude-opus-4", (15.0, 75.0)),
        ("claude-sonnet-4", (3.0, 15.0)),
        ("claude-3.7-sonnet", (3.0, 15.0)),
        ("claude-3.5-sonnet", (3.0, 15.0)),
        ("claude-3.5-haiku", (0.8, 4.0)),
        ("claude-3-haiku", (0.25, 1.25)),
        // Google
        ("gemini-2.5-pro", (1.25, 10.0)),
        ("gemini-2.5-flash", (0.3, 2.5)),
        ("gemini-2.0-flash", (0.1, 0.4)),
        ("gemini-1.5-pro", (1.25, 5.0)),
        ("gemini-1.5-flash", (0.075, 0.3)),
        // Meta Llama
        ("llama-3.3-70b", (0.2, 0.2)),
        ("llama-3.1-405b", (3.5, 3.5)),
        ("llama-3.1-8b", (0.05, 0.05)),
        // Mistral
        ("mistral-large", (2.0, 6.0)),
        ("mistral-small", (0.2, 0.6)),
        // DeepSeek
        ("deepseek-chat", (0.27, 1.1)),
        ("deepseek-reasoner", (0.55, 2.19)),
        // xAI
        ("grok-2", (2.0, 10.0)),
    ])
});

/// Returned by [`estimate_cost`] when a model is absent from [`PRICES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownModel(pub String);

impl fmt::Display for UnknownModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown model \"{}\"", self.0)
    }
}

impl std::error::Error for UnknownModel {}

/// Turns token counts into a dollar cost using [`PRICES`], rounded to six decimal
/// places. Returns an [`UnknownModel`] error for a model that is not in the table.
pub fn estimate_cost(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<f64, UnknownModel> {
    let price = PRICES
        .get(model)
        .ok_or_else(|| UnknownModel(model.to_string()))?;
    let cost =
        input_tokens as f64 / 1_000_000.0 * price.0 + output_tokens as f64 / 1_000_000.0 * price.1;
    Ok((cost * 1e6).round() / 1e6)
}
