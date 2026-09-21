package quotagate

import "testing"

// Concurrency slots: cap the number of in-flight requests per scope.

func concurrencyLimiter() *Limiter {
	return NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxConcurrent(2), WithScope(Tenant))})
}

func TestSlotsCapInFlightRequests(t *testing.T) {
	l := concurrencyLimiter()
	a := l.AcquireSlot("gpt-4o", WithTenant("acme"))
	b := l.AcquireSlot("gpt-4o", WithTenant("acme"))
	c := l.AcquireSlot("gpt-4o", WithTenant("acme"))
	if !a.OK || !b.OK {
		t.Fatal("the first two slots should be acquired")
	}
	if c.OK {
		t.Fatal("the third slot should be denied")
	}
	if c.TrippedRule == nil {
		t.Fatal("the denied slot should carry the tripped rule")
	}
}

func TestReleasingASlotFreesCapacity(t *testing.T) {
	l := concurrencyLimiter()
	a := l.AcquireSlot("gpt-4o", WithTenant("acme"))
	l.AcquireSlot("gpt-4o", WithTenant("acme"))
	if l.AcquireSlot("gpt-4o", WithTenant("acme")).OK {
		t.Fatal("the third slot should be denied while two are held")
	}
	a.Release()
	if !l.AcquireSlot("gpt-4o", WithTenant("acme")).OK {
		t.Fatal("a slot should be free again after release")
	}
}

func TestSlotIsReleasedOnScopeExit(t *testing.T) {
	l := concurrencyLimiter()
	func() {
		a := l.AcquireSlot("gpt-4o", WithTenant("acme"))
		defer a.Release()
		b := l.AcquireSlot("gpt-4o", WithTenant("acme"))
		defer b.Release()
		if l.AcquireSlot("gpt-4o", WithTenant("acme")).OK {
			t.Fatal("no third slot while both are held")
		}
	}()
	// Both scoped slots released on exit.
	if !l.AcquireSlot("gpt-4o", WithTenant("acme")).OK {
		t.Fatal("a slot should be free after the scope exits")
	}
}

func TestConcurrencyIsScopedPerTenant(t *testing.T) {
	l := concurrencyLimiter()
	l.AcquireSlot("gpt-4o", WithTenant("acme"))
	l.AcquireSlot("gpt-4o", WithTenant("acme"))
	if l.AcquireSlot("gpt-4o", WithTenant("acme")).OK {
		t.Fatal("acme is at its concurrency cap")
	}
	// A different tenant has its own concurrency budget.
	if !l.AcquireSlot("gpt-4o", WithTenant("globex")).OK {
		t.Fatal("globex should have its own concurrency budget")
	}
}
