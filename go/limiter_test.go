package quotagate

import (
	"math"
	"testing"
)

// The limiter surface: multi-window enforcement, precise back-off, headers, cost
// rules, graceful fallback, and config loading.

func tokenLimiter() *Limiter {
	return NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxTokens(1000))})
}

func TestAllowsUnderTheLimit(t *testing.T) {
	l := tokenLimiter()
	d := l.TryAcquire("gpt-4o", WithTokens(100), WithNow(0))
	if !d.Allowed {
		t.Fatal("expected allowed")
	}
	if d.Remaining["tokens"] != 900 {
		t.Fatalf("expected 900 tokens remaining, got %v", d.Remaining["tokens"])
	}
}

func TestDeniesOverTokenCapAndRecoversAfterRetryAfter(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxTokens(100))})
	if !l.TryAcquire("gpt-4o", WithTokens(100), WithNow(1000)).Allowed {
		t.Fatal("first 100 should be allowed")
	}
	d := l.TryAcquire("gpt-4o", WithTokens(1), WithNow(1000))
	if d.Allowed {
		t.Fatal("expected denial over cap")
	}
	if d.RetryAfter <= 0 {
		t.Fatalf("expected positive retry_after, got %v", d.RetryAfter)
	}
	// Waiting exactly retry_after must actually clear the call.
	if !l.TryAcquire("gpt-4o", WithTokens(1), WithNow(1000+d.RetryAfter)).Allowed {
		t.Fatal("expected allowance after waiting retry_after")
	}
}

func TestDeniesOverRequestCap(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(1))})
	if !l.TryAcquire("gpt-4o", WithNow(0)).Allowed {
		t.Fatal("first request should be allowed")
	}
	if l.TryAcquire("gpt-4o", WithNow(0)).Allowed {
		t.Fatal("second request should be denied")
	}
}

func TestMultipleWindowsMinuteOkButDailyTrips(t *testing.T) {
	l := NewLimiter([]LimitRule{
		NewLimitRule("gpt-4o", 60, WithMaxRequests(5), WithName("per-min")),
		NewLimitRule("gpt-4o", 86_400, WithMaxRequests(8), WithName("per-day")),
	})
	for i := 0; i < 5; i++ {
		if !l.TryAcquire("gpt-4o", WithNow(0)).Allowed {
			t.Fatalf("call %d in first minute should be allowed", i)
		}
	}
	if l.TryAcquire("gpt-4o", WithNow(0)).Allowed {
		t.Fatal("per-min rule should trip on the 6th")
	}
	for i := 0; i < 3; i++ {
		if !l.TryAcquire("gpt-4o", WithNow(float64(61+i))).Allowed {
			t.Fatalf("post-minute call %d should be allowed", i)
		}
	}
	d := l.TryAcquire("gpt-4o", WithNow(64))
	if d.Allowed {
		t.Fatal("the 9th of the day should trip the daily rule")
	}
	if d.TrippedRule == nil || d.TrippedRule.Name != "per-day" {
		t.Fatalf("expected per-day tripped, got %+v", d.TrippedRule)
	}
}

func TestRetryAfterIsPreciseInExactMode(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(1), WithPrecise(true))})
	l.TryAcquire("gpt-4o", WithNow(1000))
	d := l.TryAcquire("gpt-4o", WithNow(1000))
	// Event at 1000 leaves the 60s window at 1060 -> retry_after == 60.
	if math.Abs(d.RetryAfter-60) >= 1e-6 {
		t.Fatalf("expected retry_after 60, got %v", d.RetryAfter)
	}
}

func TestHeadersRenderBackpressure(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(1), WithMaxTokens(1000))})
	l.TryAcquire("gpt-4o", WithTokens(10), WithNow(0))
	d := l.TryAcquire("gpt-4o", WithTokens(10), WithNow(0))
	headers := RateLimitHeaders(d, nil)
	if _, ok := headers["Retry-After"]; !ok {
		t.Fatal("expected Retry-After header")
	}
	if headers["X-RateLimit-Limit-Requests"] != "1" {
		t.Fatalf("expected requests limit 1, got %q", headers["X-RateLimit-Limit-Requests"])
	}
	if headers["X-RateLimit-Limit-Tokens"] != "1000" {
		t.Fatalf("expected tokens limit 1000, got %q", headers["X-RateLimit-Limit-Tokens"])
	}
}

func TestCostBasedRuleCapsSpend(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxCost(1.0))})
	// 200k output tokens on gpt-4o = 200000/1e6 * 10 = $2.00 -> over the $1 cap.
	cost, err := EstimateCost("gpt-4o", 0, 200_000)
	if err != nil {
		t.Fatalf("estimate cost: %v", err)
	}
	d := l.TryAcquire("gpt-4o", WithCost(cost), WithNow(0))
	if d.Allowed {
		t.Fatal("expected denial over the spend cap")
	}
	if d.TrippedRule == nil {
		t.Fatal("expected a tripped rule")
	}
}

func TestFallbackIsSuggestedAndUsedOnDenial(t *testing.T) {
	l := NewLimiter(
		[]LimitRule{
			NewLimitRule("gpt-4o", 60, WithMaxRequests(0)), // always full
			NewLimitRule("gpt-4o-mini", 60, WithMaxRequests(100)),
		},
		WithFallbacks(map[string]string{"gpt-4o": "gpt-4o-mini"}),
	)
	d := l.TryAcquire("gpt-4o", WithNow(0))
	if d.Allowed {
		t.Fatal("expected gpt-4o to be full")
	}
	if d.SuggestedFallback != "gpt-4o-mini" {
		t.Fatalf("expected fallback suggestion, got %q", d.SuggestedFallback)
	}
	used, chosen := l.AcquireOrFallback("gpt-4o", WithNow(0))
	if !used.Allowed {
		t.Fatal("expected the fallback to be admitted")
	}
	if chosen != "gpt-4o-mini" {
		t.Fatalf("expected gpt-4o-mini chosen, got %q", chosen)
	}
}

func TestUnknownModelHasNoRulesAndIsAllowed(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(1))})
	// A model with no configured rules passes through (fail-open by default).
	if !l.TryAcquire("some-other-model", WithTokens(10_000), WithNow(0)).Allowed {
		t.Fatal("unknown model should be allowed")
	}
}

func TestRulesLoadFromJSON(t *testing.T) {
	text := `{ "rules": [
		{ "model": "gpt-4o", "scope": "tenant", "window_seconds": 60, "max_requests": 2 }
	] }`
	rules, err := RulesFromJSON(text)
	if err != nil {
		t.Fatalf("parse rules: %v", err)
	}
	if len(rules) != 1 {
		t.Fatalf("expected 1 rule, got %d", len(rules))
	}
	if rules[0].Scope != Tenant {
		t.Fatalf("expected tenant scope, got %v", rules[0].Scope)
	}
	l := NewLimiter(rules)
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0)).Allowed {
		t.Fatal("first tenant call should be allowed")
	}
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0)).Allowed {
		t.Fatal("second tenant call should be allowed")
	}
	if l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0)).Allowed {
		t.Fatal("third tenant call should be denied")
	}
}
