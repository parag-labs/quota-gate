package quotagate

import "testing"

// Hierarchical scopes: global fleet, per-tenant, per-user - all enforced together.

func TestTenantScopeIsolatesTenants(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(2), WithScope(Tenant))})
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0)).Allowed {
		t.Fatal("acme call 1 should be allowed")
	}
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0)).Allowed {
		t.Fatal("acme call 2 should be allowed")
	}
	if l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0)).Allowed {
		t.Fatal("acme is now full")
	}
	// A different tenant has its own budget.
	if !l.TryAcquire("gpt-4o", WithTenant("globex"), WithNow(0)).Allowed {
		t.Fatal("globex should have its own budget")
	}
}

func TestUserScopeIsolatesUsersWithinATenant(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(1), WithScope(User))})
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithUser("ann"), WithNow(0)).Allowed {
		t.Fatal("ann call 1 should be allowed")
	}
	if l.TryAcquire("gpt-4o", WithTenant("acme"), WithUser("ann"), WithNow(0)).Allowed {
		t.Fatal("ann is now full")
	}
	// Same tenant, different user.
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithUser("bob"), WithNow(0)).Allowed {
		t.Fatal("bob should have his own budget")
	}
}

func TestGlobalScopeIsSharedAcrossEveryone(t *testing.T) {
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(2), WithScope(Global))})
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithUser("ann"), WithNow(0)).Allowed {
		t.Fatal("first global call should be allowed")
	}
	if !l.TryAcquire("gpt-4o", WithTenant("globex"), WithUser("bob"), WithNow(0)).Allowed {
		t.Fatal("second global call should be allowed")
	}
	// The fleet cap is hit no matter who is calling.
	if l.TryAcquire("gpt-4o", WithTenant("new"), WithUser("cat"), WithNow(0)).Allowed {
		t.Fatal("the fleet cap should be hit regardless of caller")
	}
}

func TestToughestApplicableRuleWins(t *testing.T) {
	// A generous per-tenant cap but a tiny global fleet cap.
	l := NewLimiter([]LimitRule{
		NewLimitRule("gpt-4o", 60, WithMaxRequests(1000), WithScope(Tenant), WithName("tenant")),
		NewLimitRule("gpt-4o", 60, WithMaxRequests(1), WithScope(Global), WithName("fleet")),
	})
	if !l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0)).Allowed {
		t.Fatal("first call should be allowed")
	}
	d := l.TryAcquire("gpt-4o", WithTenant("acme"), WithNow(0))
	if d.Allowed {
		t.Fatal("the tiny fleet cap should trip")
	}
	if d.TrippedRule == nil || d.TrippedRule.Name != "fleet" {
		t.Fatalf("expected fleet tripped, got %+v", d.TrippedRule)
	}
	if d.Scope == nil || *d.Scope != Global {
		t.Fatalf("expected global scope, got %v", d.Scope)
	}
}
