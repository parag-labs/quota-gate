package quotagate

import (
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"
)

// Decision is the result of an admission check.
type Decision struct {
	Allowed           bool
	Model             string
	RetryAfter        float64
	TrippedRule       *LimitRule
	Scope             *Scope
	Remaining         map[string]float64
	SuggestedFallback string
}

// Reservation records counters up front so the actual usage can be reconciled
// (commit) or released (refund) afterwards.
type Reservation struct {
	Model     string
	Tokens    float64
	Cost      float64
	Committed bool
	handles   []reservationHandle
}

type reservationHandle struct {
	key    string
	handle any
}

// Slot is a held concurrency reservation. Release (typically via defer) to give
// the in-flight slots back.
type Slot struct {
	limiter     *Limiter
	keys        []string
	OK          bool
	TrippedRule *LimitRule
}

// Release returns every concurrency slot the Slot holds.
func (s *Slot) Release() {
	for _, k := range s.keys {
		s.limiter.store.ReleaseConcurrency(k)
	}
	s.keys = nil
}

type ruleIndex struct {
	index int
	rule  LimitRule
}

// Limiter is the gate: it evaluates limits before a call, reserves/reconciles
// tokens, caps concurrency, and emits standard back-pressure signals.
type Limiter struct {
	rules     []LimitRule
	store     Store
	clock     func() float64
	fallbacks map[string]string
	byModel   map[string][]ruleIndex
}

// LimiterOption customises a Limiter built with NewLimiter.
type LimiterOption func(*Limiter)

// WithStore plugs in a custom store (the default is an InMemoryStore).
func WithStore(s Store) LimiterOption { return func(l *Limiter) { l.store = s } }

// WithClock overrides the wall clock used when a call passes no explicit time.
func WithClock(c func() float64) LimiterOption { return func(l *Limiter) { l.clock = c } }

// WithFallbacks configures per-model cheaper-model fallbacks for graceful
// degradation on denial.
func WithFallbacks(f map[string]string) LimiterOption {
	return func(l *Limiter) {
		l.fallbacks = make(map[string]string, len(f))
		for k, v := range f {
			l.fallbacks[k] = v
		}
	}
}

// NewLimiter builds a Limiter from a set of rules and options.
func NewLimiter(rules []LimitRule, opts ...LimiterOption) *Limiter {
	l := &Limiter{
		rules:     append([]LimitRule(nil), rules...),
		fallbacks: map[string]string{},
		byModel:   map[string][]ruleIndex{},
	}
	for _, o := range opts {
		o(l)
	}
	if l.store == nil {
		l.store = NewInMemoryStore()
	}
	if l.clock == nil {
		l.clock = func() float64 { return float64(time.Now().UnixNano()) / 1e9 }
	}
	for i, r := range l.rules {
		l.byModel[r.Model] = append(l.byModel[r.Model], ruleIndex{index: i, rule: r})
	}
	return l
}

// Store returns the backing store, so callers can inspect or share it.
func (l *Limiter) Store() Store { return l.store }

// acquireOptions collects the optional arguments of an admission call.
type acquireOptions struct {
	tokens float64
	cost   float64
	tenant string
	user   string
	now    *float64
	record bool
}

// AcquireOption customises a TryAcquire, Reserve, AcquireSlot, or
// AcquireOrFallback call.
type AcquireOption func(*acquireOptions)

// WithTokens sets the token estimate for the call.
func WithTokens(v float64) AcquireOption { return func(o *acquireOptions) { o.tokens = v } }

// WithCost sets the dollar-cost estimate for the call.
func WithCost(v float64) AcquireOption { return func(o *acquireOptions) { o.cost = v } }

// WithTenant scopes the call to a tenant.
func WithTenant(t string) AcquireOption { return func(o *acquireOptions) { o.tenant = t } }

// WithUser scopes the call to a user (within a tenant).
func WithUser(u string) AcquireOption { return func(o *acquireOptions) { o.user = u } }

// WithNow pins the call to an explicit timestamp instead of the wall clock.
func WithNow(n float64) AcquireOption { return func(o *acquireOptions) { o.now = &n } }

func (l *Limiter) now(override *float64) float64 {
	if override != nil {
		return *override
	}
	return l.clock()
}

func (l *Limiter) applicable(model string) []ruleIndex {
	out := make([]ruleIndex, 0, len(l.byModel[model])+len(l.byModel["*"]))
	out = append(out, l.byModel[model]...)
	out = append(out, l.byModel["*"]...)
	return out
}

func (l *Limiter) key(index int, r LimitRule, tenant, user string) string {
	switch r.Scope {
	case Global:
		return fmt.Sprintf("%d|g", index)
	case Tenant:
		return fmt.Sprintf("%d|t|%s", index, tenant)
	default:
		return fmt.Sprintf("%d|u|%s|%s", index, tenant, user)
	}
}

// TryAcquire evaluates a call against every applicable rule and, unless denied,
// records it. On denial the Decision carries the retry_after, the tripped rule,
// its scope, and any configured fallback.
func (l *Limiter) TryAcquire(model string, opts ...AcquireOption) Decision {
	o := acquireOptions{record: true}
	for _, opt := range opts {
		opt(&o)
	}
	return l.tryAcquire(model, o)
}

func (l *Limiter) tryAcquire(model string, o acquireOptions) Decision {
	now := l.now(o.now)
	rules := l.applicable(model)

	worstRetry := -1.0
	var worstRule *LimitRule
	for _, ri := range rules {
		r := ri.rule
		if !r.HasUsageLimit() {
			continue
		}
		key := l.key(ri.index, r, o.tenant, o.user)
		usedT, usedR, usedC := l.store.Snapshot(key, now, r.WindowSeconds, r.BucketSeconds())
		type breach struct {
			dim  int
			over float64
		}
		var breached []breach
		if r.MaxTokens != nil && usedT+o.tokens > *r.MaxTokens+1e-9 {
			breached = append(breached, breach{TOKENS, usedT + o.tokens - *r.MaxTokens})
		}
		if r.MaxRequests != nil && usedR+1 > *r.MaxRequests+1e-9 {
			breached = append(breached, breach{REQUESTS, usedR + 1 - *r.MaxRequests})
		}
		if r.MaxCost != nil && usedC+o.cost > *r.MaxCost+1e-9 {
			breached = append(breached, breach{COST, usedC + o.cost - *r.MaxCost})
		}
		if len(breached) > 0 {
			ra := 0.0
			for _, b := range breached {
				ra = math.Max(ra, l.store.TimeToFree(key, now, r.WindowSeconds, r.BucketSeconds(), b.over, b.dim))
			}
			if ra > worstRetry {
				worstRetry = ra
				captured := r
				worstRule = &captured
			}
		}
	}

	if worstRule != nil {
		scope := worstRule.Scope
		return Decision{
			Allowed:           false,
			Model:             model,
			RetryAfter:        worstRetry,
			TrippedRule:       worstRule,
			Scope:             &scope,
			SuggestedFallback: l.fallbacks[model],
		}
	}

	if o.record {
		for _, ri := range rules {
			r := ri.rule
			if !r.HasUsageLimit() {
				continue
			}
			l.store.Add(l.key(ri.index, r, o.tenant, o.user), now, r.WindowSeconds, r.BucketSeconds(), o.tokens, 1.0, o.cost)
		}
	}

	return Decision{
		Allowed:   true,
		Model:     model,
		Remaining: l.remaining(model, o.tenant, o.user, now),
	}
}

func (l *Limiter) remaining(model, tenant, user string, now float64) map[string]float64 {
	rem := map[string]float64{}
	tighten := func(name string, value float64) {
		if cur, ok := rem[name]; ok {
			rem[name] = math.Min(cur, value)
		} else {
			rem[name] = value
		}
	}
	for _, ri := range l.applicable(model) {
		r := ri.rule
		key := l.key(ri.index, r, tenant, user)
		usedT, usedR, usedC := l.store.Snapshot(key, now, r.WindowSeconds, r.BucketSeconds())
		if r.MaxTokens != nil {
			tighten("tokens", *r.MaxTokens-usedT)
		}
		if r.MaxRequests != nil {
			tighten("requests", *r.MaxRequests-usedR)
		}
		if r.MaxCost != nil {
			tighten("cost", *r.MaxCost-usedC)
		}
	}
	for k, v := range rem {
		rem[k] = math.Max(0.0, v)
	}
	return rem
}

// ---- reserve -> commit / refund ----

// Reserve records the estimate up front and returns a Reservation to reconcile
// later. If the call would be denied, it returns the denied Decision and a nil
// Reservation.
func (l *Limiter) Reserve(model string, opts ...AcquireOption) (Decision, *Reservation) {
	o := acquireOptions{record: true}
	for _, opt := range opts {
		opt(&o)
	}
	now := l.now(o.now)
	o.now = &now
	o.record = false
	decision := l.tryAcquire(model, o)
	if !decision.Allowed {
		return decision, nil
	}
	var handles []reservationHandle
	for _, ri := range l.applicable(model) {
		r := ri.rule
		if !r.HasUsageLimit() {
			continue
		}
		key := l.key(ri.index, r, o.tenant, o.user)
		h := l.store.Add(key, now, r.WindowSeconds, r.BucketSeconds(), o.tokens, 1.0, o.cost)
		handles = append(handles, reservationHandle{key: key, handle: h})
	}
	return decision, &Reservation{Model: model, Tokens: o.tokens, Cost: o.cost, handles: handles}
}

// Commit reconciles a reservation to the actual usage. A nil actualTokens or
// actualCost leaves that dimension at the reserved estimate. A committed
// reservation is frozen and further commits/refunds are no-ops.
func (l *Limiter) Commit(res *Reservation, actualTokens, actualCost *float64) {
	if res.Committed {
		return
	}
	dt := 0.0
	if actualTokens != nil {
		dt = *actualTokens - res.Tokens
	}
	dc := 0.0
	if actualCost != nil {
		dc = *actualCost - res.Cost
	}
	for _, h := range res.handles {
		l.store.Adjust(h.key, h.handle, dt, 0.0, dc)
	}
	res.Tokens += dt
	res.Cost += dc
	res.Committed = true
}

// Refund releases everything a reservation recorded, e.g. after a failed call.
func (l *Limiter) Refund(res *Reservation) {
	if res.Committed {
		return
	}
	for _, h := range res.handles {
		l.store.Adjust(h.key, h.handle, -res.Tokens, -1.0, -res.Cost)
	}
	res.handles = nil
	res.Committed = true
}

// ---- concurrency slots ----

// AcquireSlot reserves one concurrency slot per applicable max-concurrent rule.
// If any rule is full, all partially-acquired slots are released and the returned
// Slot has OK=false with the tripped rule.
func (l *Limiter) AcquireSlot(model string, opts ...AcquireOption) *Slot {
	var o acquireOptions
	for _, opt := range opts {
		opt(&o)
	}
	var acquired []string
	for _, ri := range l.applicable(model) {
		r := ri.rule
		if r.MaxConcurrent == nil {
			continue
		}
		key := "conc|" + l.key(ri.index, r, o.tenant, o.user)
		if !l.store.TryAddConcurrency(key, *r.MaxConcurrent) {
			for _, k := range acquired {
				l.store.ReleaseConcurrency(k)
			}
			captured := r
			return &Slot{limiter: l, OK: false, TrippedRule: &captured}
		}
		acquired = append(acquired, key)
	}
	return &Slot{limiter: l, keys: acquired, OK: true}
}

// ---- graceful degradation ----

// AcquireOrFallback tries the model, then its configured fallback on denial, and
// reports which model was ultimately chosen.
func (l *Limiter) AcquireOrFallback(model string, opts ...AcquireOption) (Decision, string) {
	decision := l.TryAcquire(model, opts...)
	if decision.Allowed {
		return decision, model
	}
	if fb, ok := l.fallbacks[model]; ok {
		alt := l.TryAcquire(fb, opts...)
		if alt.Allowed {
			return alt, fb
		}
	}
	return decision, model
}

// RateLimitHeaders renders a decision as the X-RateLimit-* / Retry-After headers
// real providers return, so back-pressure can be passed straight to the caller.
func RateLimitHeaders(decision Decision, rule *LimitRule) map[string]string {
	if rule == nil {
		rule = decision.TrippedRule
	}
	headers := map[string]string{}
	if decision.RetryAfter > 0 {
		headers["Retry-After"] = strconv.FormatInt(int64(math.Ceil(decision.RetryAfter)), 10)
	}
	if rule != nil {
		if rule.MaxRequests != nil {
			headers["X-RateLimit-Limit-Requests"] = strconv.FormatInt(int64(*rule.MaxRequests), 10)
		}
		if rule.MaxTokens != nil {
			headers["X-RateLimit-Limit-Tokens"] = strconv.FormatInt(int64(*rule.MaxTokens), 10)
		}
	}
	for name, value := range decision.Remaining {
		capitalized := strings.ToUpper(name[:1]) + name[1:]
		headers["X-RateLimit-Remaining-"+capitalized] = strconv.FormatInt(int64(value), 10)
	}
	return headers
}
