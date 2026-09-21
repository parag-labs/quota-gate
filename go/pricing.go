package quotagate

import (
	"fmt"
	"math"
)

// Prices lists illustrative published list prices (USD per 1M tokens) as
// {input, output}. They drift as vendors change them - a starting default, not a
// source of truth.
var Prices = map[string][2]float64{
	// OpenAI
	"gpt-4o":       {2.5, 10.0},
	"gpt-4o-mini":  {0.15, 0.6},
	"gpt-4.1":      {2.0, 8.0},
	"gpt-4.1-mini": {0.4, 1.6},
	"gpt-4.1-nano": {0.1, 0.4},
	"o3":           {2.0, 8.0},
	"o3-mini":      {1.1, 4.4},
	"o4-mini":      {1.1, 4.4},
	// Anthropic
	"claude-opus-4":     {15.0, 75.0},
	"claude-sonnet-4":   {3.0, 15.0},
	"claude-3.7-sonnet": {3.0, 15.0},
	"claude-3.5-sonnet": {3.0, 15.0},
	"claude-3.5-haiku":  {0.8, 4.0},
	"claude-3-haiku":    {0.25, 1.25},
	// Google
	"gemini-2.5-pro":   {1.25, 10.0},
	"gemini-2.5-flash": {0.3, 2.5},
	"gemini-2.0-flash": {0.1, 0.4},
	"gemini-1.5-pro":   {1.25, 5.0},
	"gemini-1.5-flash": {0.075, 0.3},
	// Meta Llama
	"llama-3.3-70b":  {0.2, 0.2},
	"llama-3.1-405b": {3.5, 3.5},
	"llama-3.1-8b":   {0.05, 0.05},
	// Mistral
	"mistral-large": {2.0, 6.0},
	"mistral-small": {0.2, 0.6},
	// DeepSeek
	"deepseek-chat":     {0.27, 1.1},
	"deepseek-reasoner": {0.55, 2.19},
	// xAI
	"grok-2": {2.0, 10.0},
}

// UnknownModelError is returned by EstimateCost when a model is absent from Prices.
type UnknownModelError struct{ Model string }

func (e UnknownModelError) Error() string { return fmt.Sprintf("unknown model %q", e.Model) }

// EstimateCost turns token counts into a dollar cost using the Prices table,
// rounded to six decimal places. It returns an UnknownModelError for a model that
// is not in the table.
func EstimateCost(model string, inputTokens, outputTokens int) (float64, error) {
	price, ok := Prices[model]
	if !ok {
		return 0, UnknownModelError{Model: model}
	}
	cost := float64(inputTokens)/1_000_000*price[0] + float64(outputTokens)/1_000_000*price[1]
	return math.Round(cost*1e6) / 1e6, nil
}
