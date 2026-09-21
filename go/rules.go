// Package quotagate mirrors the provider-style rate limits that model-serving
// APIs enforce: many limits per model applied together, across global/tenant/user
// scopes, with reserve-then-reconcile token accounting and standard back-pressure
// signals. It is the Go port of the Python reference implementation.
package quotagate

import (
	"encoding/json"
	"fmt"
	"strings"
)

// Scope is where a limit is enforced. A request is keyed differently per scope so
// the same rule isolates one global fleet, one tenant, or one user.
type Scope int

const (
	// Global enforces a single limit shared across the whole fleet.
	Global Scope = iota
	// Tenant enforces a limit per tenant.
	Tenant
	// User enforces a limit per user within a tenant.
	User
)

// String renders the scope as its lowercase wire name ("global"/"tenant"/"user").
func (s Scope) String() string {
	switch s {
	case Tenant:
		return "tenant"
	case User:
		return "user"
	default:
		return "global"
	}
}

// ParseScope maps a wire name to a Scope, returning an error for unknown names.
func ParseScope(s string) (Scope, error) {
	switch strings.ToLower(s) {
	case "global":
		return Global, nil
	case "tenant":
		return Tenant, nil
	case "user":
		return User, nil
	default:
		return Global, fmt.Errorf("unknown scope %q", s)
	}
}

// LimitRule is one provider-style limit. Any subset of the Max* dimensions may be
// set; a request must stay under every one that is.
type LimitRule struct {
	Model            string
	WindowSeconds    float64
	MaxTokens        *float64
	MaxRequests      *float64
	MaxCost          *float64
	MaxConcurrent    *int
	Scope            Scope
	BucketsPerWindow int
	Precise          bool
	Name             string
}

// RuleOption customises a LimitRule built with NewLimitRule.
type RuleOption func(*LimitRule)

// WithMaxTokens caps token volume over the window.
func WithMaxTokens(v float64) RuleOption { return func(r *LimitRule) { r.MaxTokens = &v } }

// WithMaxRequests caps request count over the window.
func WithMaxRequests(v float64) RuleOption { return func(r *LimitRule) { r.MaxRequests = &v } }

// WithMaxCost caps dollar spend over the window.
func WithMaxCost(v float64) RuleOption { return func(r *LimitRule) { r.MaxCost = &v } }

// WithMaxConcurrent caps the number of in-flight requests.
func WithMaxConcurrent(v int) RuleOption { return func(r *LimitRule) { r.MaxConcurrent = &v } }

// WithScope sets the scope the rule is enforced at.
func WithScope(s Scope) RuleOption { return func(r *LimitRule) { r.Scope = s } }

// WithBucketsPerWindow tunes the memory/accuracy trade-off of the bucketed window.
func WithBucketsPerWindow(n int) RuleOption { return func(r *LimitRule) { r.BucketsPerWindow = n } }

// WithPrecise switches the rule to the exact per-event log.
func WithPrecise(p bool) RuleOption { return func(r *LimitRule) { r.Precise = p } }

// WithName sets a human-readable label for the rule.
func WithName(name string) RuleOption { return func(r *LimitRule) { r.Name = name } }

// NewLimitRule builds a LimitRule with the standard defaults (Global scope, 60
// buckets per window) and applies the given options.
func NewLimitRule(model string, windowSeconds float64, opts ...RuleOption) LimitRule {
	r := LimitRule{Model: model, WindowSeconds: windowSeconds, Scope: Global, BucketsPerWindow: 60}
	for _, o := range opts {
		o(&r)
	}
	return r
}

// BucketSeconds returns the bucket width: 0 selects the exact per-event log;
// otherwise WindowSeconds/BucketsPerWindow.
func (r LimitRule) BucketSeconds() float64 {
	if r.Precise {
		return 0.0
	}
	buckets := r.BucketsPerWindow
	if buckets < 1 {
		buckets = 1
	}
	return r.WindowSeconds / float64(buckets)
}

// Label returns the rule's name, or a derived "model:scope:Ns" label.
func (r LimitRule) Label() string {
	if r.Name != "" {
		return r.Name
	}
	return fmt.Sprintf("%s:%s:%ds", r.Model, r.Scope, int64(r.WindowSeconds))
}

// HasUsageLimit reports whether the rule caps tokens, requests, or cost (as
// opposed to being a concurrency-only rule).
func (r LimitRule) HasUsageLimit() bool {
	return r.MaxTokens != nil || r.MaxRequests != nil || r.MaxCost != nil
}

// RulesFromJSON parses a rules document (see limits.sample.json) into LimitRules.
func RulesFromJSON(text string) ([]LimitRule, error) {
	var doc struct {
		Rules []struct {
			Model            string   `json:"model"`
			WindowSeconds    float64  `json:"window_seconds"`
			MaxTokens        *float64 `json:"max_tokens"`
			MaxRequests      *float64 `json:"max_requests"`
			MaxCost          *float64 `json:"max_cost"`
			MaxConcurrent    *int     `json:"max_concurrent"`
			Scope            *string  `json:"scope"`
			BucketsPerWindow *int     `json:"buckets_per_window"`
			Precise          *bool    `json:"precise"`
			Name             *string  `json:"name"`
		} `json:"rules"`
	}
	if err := json.Unmarshal([]byte(text), &doc); err != nil {
		return nil, err
	}
	out := make([]LimitRule, 0, len(doc.Rules))
	for _, row := range doc.Rules {
		scope := Global
		if row.Scope != nil {
			parsed, err := ParseScope(*row.Scope)
			if err != nil {
				return nil, err
			}
			scope = parsed
		}
		buckets := 60
		if row.BucketsPerWindow != nil {
			buckets = *row.BucketsPerWindow
		}
		precise := false
		if row.Precise != nil {
			precise = *row.Precise
		}
		name := ""
		if row.Name != nil {
			name = *row.Name
		}
		out = append(out, LimitRule{
			Model:            row.Model,
			WindowSeconds:    row.WindowSeconds,
			MaxTokens:        row.MaxTokens,
			MaxRequests:      row.MaxRequests,
			MaxCost:          row.MaxCost,
			MaxConcurrent:    row.MaxConcurrent,
			Scope:            scope,
			BucketsPerWindow: buckets,
			Precise:          precise,
			Name:             name,
		})
	}
	return out, nil
}
