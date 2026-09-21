package quotagate

import "testing"

func ptr(f float64) *float64 { return &f }

// Reserve on the estimate, then reconcile to the actual - the standard meter model.

func reserveLimiter() *Limiter {
	return NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxTokens(1000), WithScope(Global))})
}

func TestReserveConsumesTheEstimatedHeadroom(t *testing.T) {
	l := reserveLimiter()
	decision, res := l.Reserve("gpt-4o", WithTokens(800), WithNow(0))
	if !decision.Allowed || res == nil {
		t.Fatal("reserve should be allowed and return a reservation")
	}
	// 800 is reserved, so a 300-token call no longer fits.
	if l.TryAcquire("gpt-4o", WithTokens(300), WithNow(0)).Allowed {
		t.Fatal("300 more tokens should not fit after reserving 800")
	}
}

func TestCommitReconcilesDownAndFreesCapacity(t *testing.T) {
	l := reserveLimiter()
	_, res := l.Reserve("gpt-4o", WithTokens(800), WithNow(0))
	// The call actually used only 100 tokens.
	l.Commit(res, ptr(100), nil)
	// 900 tokens of headroom are back.
	if !l.TryAcquire("gpt-4o", WithTokens(300), WithNow(0)).Allowed {
		t.Fatal("300 tokens should fit after committing down to 100")
	}
}

func TestCommitReconcilesUp(t *testing.T) {
	l := reserveLimiter()
	_, res := l.Reserve("gpt-4o", WithTokens(100), WithNow(0))
	l.Commit(res, ptr(900), nil)
	// Now 900 are used; a 200-token call trips the cap.
	if l.TryAcquire("gpt-4o", WithTokens(200), WithNow(0)).Allowed {
		t.Fatal("200 tokens should not fit after committing up to 900")
	}
}

func TestRefundReturnsEverythingOnAFailedCall(t *testing.T) {
	l := reserveLimiter()
	_, res := l.Reserve("gpt-4o", WithTokens(900), WithNow(0))
	l.Refund(res)
	if !l.TryAcquire("gpt-4o", WithTokens(1000), WithNow(0)).Allowed {
		t.Fatal("the full 1000 should fit again after a refund")
	}
}

func TestDoubleCommitIsANoop(t *testing.T) {
	l := reserveLimiter()
	_, res := l.Reserve("gpt-4o", WithTokens(100), WithNow(0))
	l.Commit(res, ptr(500), nil)
	l.Commit(res, ptr(999), nil) // ignored
	// Used is 500, so 400 still fits but 600 does not.
	if !l.TryAcquire("gpt-4o", WithTokens(400), WithNow(0)).Allowed {
		t.Fatal("400 tokens should still fit (used is 500)")
	}
	if l.TryAcquire("gpt-4o", WithTokens(600), WithNow(0)).Allowed {
		t.Fatal("600 tokens should not fit (used is 500)")
	}
}
