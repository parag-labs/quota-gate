package quotagate

import (
	"math/rand"
	"sync"
	"testing"
)

// Stress suite: memory pressure, out-of-order events, high-volume soak, and a
// threaded smoke test. These prove the properties the design claims under load.

// bucketCountOf totals the live buckets across every bucketed window in a store.
func bucketCountOf(l *Limiter) int {
	total := 0
	for _, w := range l.Store().(*InMemoryStore).windows {
		if bw, ok := w.(*bucketWindow); ok {
			total += bw.bucketCount()
		}
	}
	return total
}

func TestMemoryIsBoundedUnderHeavyTraffic(t *testing.T) {
	// A 1-hour window split into 60 buckets. No matter how many events we push,
	// the store must never hold more than ~60 buckets for this key.
	l := NewLimiter([]LimitRule{
		NewLimitRule("gpt-4o", 3600, WithMaxTokens(1e12), WithBucketsPerWindow(60)),
	})
	for i := 0; i < 100_000; i++ {
		l.TryAcquire("gpt-4o", WithTokens(1), WithNow(float64(i)*0.072))
	}
	if c := bucketCountOf(l); c > 62 {
		t.Fatalf("bucket count grew to %d", c)
	}
}

func TestPreciseModeGrowsButStaysCorrect(t *testing.T) {
	// The exact-log mode grows with in-window traffic but stays exact.
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(100), WithPrecise(true))})
	allowed := 0
	for i := 0; i < 500; i++ {
		if l.TryAcquire("gpt-4o", WithNow(1000.0)).Allowed {
			allowed++
		}
	}
	if allowed != 100 {
		t.Fatalf("expected exactly 100 admitted, got %d", allowed)
	}
}

func TestEnforcementIsCorrectWithOutOfOrderTimestamps(t *testing.T) {
	// Feed events whose timestamps jump around within the window. The rolling count
	// must still reflect everything inside the window, so the cap holds.
	l := NewLimiter([]LimitRule{NewLimitRule("gpt-4o", 60, WithMaxRequests(10))})
	stamps := []float64{1000.0, 1002.0, 1001.0, 1005.0, 1003.0, 1004.0, 1002.5, 1001.5, 1000.5, 1004.5}
	for _, ts := range stamps {
		if !l.TryAcquire("gpt-4o", WithNow(ts)).Allowed {
			t.Fatalf("event at %v should be allowed", ts)
		}
	}
	// The 11th event anywhere in the window must be denied regardless of order.
	if l.TryAcquire("gpt-4o", WithNow(1002.7)).Allowed {
		t.Fatal("the 11th in-window event must be denied")
	}
}

func TestHighVolumeSoakKeepsTheCap(t *testing.T) {
	// A tight per-minute cap approximated by 1-second buckets. In ANY sliding 60s
	// window, admissions stay within the cap plus a small bucket-granularity error.
	rng := rand.New(rand.NewSource(0))
	cap := 1000
	buckets := 60
	tolerance := cap/buckets + 1 // ~one bucket's worth of edge error
	l := NewLimiter([]LimitRule{
		NewLimitRule("gpt-4o", 60, WithMaxRequests(float64(cap)), WithBucketsPerWindow(buckets)),
	})

	admitted := make([]float64, 0, cap+tolerance+8)
	head := 0
	t0 := 0.0
	worst := 0
	for i := 0; i < 300_000; i++ {
		t0 += rng.Float64() * 0.01
		if l.TryAcquire("gpt-4o", WithNow(t0)).Allowed {
			admitted = append(admitted, t0)
		}
		for head < len(admitted) && admitted[head] <= t0-60 {
			head++
		}
		live := len(admitted) - head
		if live > worst {
			worst = live
		}
		if live > cap+tolerance {
			t.Fatalf("admitted %d in a 60s window exceeds cap+tolerance %d", live, cap+tolerance)
		}
		if head > cap+tolerance+4 { // compact occasionally to bound memory
			admitted = append(admitted[:0], admitted[head:]...)
			head = 0
		}
	}
	// And it genuinely pushes up against the cap (not trivially under it).
	if worst < int(float64(cap)*0.9) {
		t.Fatalf("worst-case admissions %d never approached the cap", worst)
	}
}

func TestThreadedAccessDoesNotCorruptCounters(t *testing.T) {
	// The in-memory store is single-process and not thread-safe by construction; the
	// design recommends sharding per worker. Prove that per-worker limiters survive
	// many goroutines hammering them and never drive a counter negative.
	const workers = 8
	limiters := make([]*Limiter, workers)
	for i := range limiters {
		limiters[i] = NewLimiter([]LimitRule{
			NewLimitRule("gpt-4o", 3600, WithMaxRequests(1e9), WithScope(Tenant)),
		})
	}
	var wg sync.WaitGroup
	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func(l *Limiter, tenant string) {
			defer wg.Done()
			for j := 0; j < 5000; j++ {
				l.TryAcquire("gpt-4o", WithTenant(tenant), WithNow(float64(j)*0.01))
			}
		}(limiters[i], "t"+string(rune('0'+i)))
	}
	wg.Wait()

	// Every window holds sane, non-negative counters.
	for _, l := range limiters {
		for _, w := range l.Store().(*InMemoryStore).windows {
			bw, ok := w.(*bucketWindow)
			if !ok {
				continue
			}
			for _, counters := range bw.buckets {
				for _, c := range counters {
					if c < 0 {
						t.Fatalf("counter went negative: %v", c)
					}
				}
			}
		}
	}
}
