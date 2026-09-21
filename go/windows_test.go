package quotagate

import (
	"math"
	"testing"
)

// The sliding-window engines: bounded-memory buckets vs the exact log.

func TestBucketMemoryIsBoundedRegardlessOfTraffic(t *testing.T) {
	// 100s window, 1s buckets -> at most ~100 buckets even under heavy traffic.
	w := newBucketWindow(100, 1)
	for ts := 0; ts < 150; ts++ {
		for j := 0; j < 1000; j++ { // 150k events total
			w.add(float64(ts), 10, 1, 0)
		}
	}
	if w.bucketCount() > 102 {
		t.Fatalf("bucket count grew to %d", w.bucketCount())
	}
}

func TestBucketCountsRecentAndDropsOld(t *testing.T) {
	w := newBucketWindow(60, 1)
	w.add(0, 100, 1, 0)
	if got, _, _ := w.snapshot(30); got != 100 {
		t.Fatalf("expected 100 tokens still in window, got %v", got)
	}
	// The event at t=0 has fully aged out by t=61.
	if got, _, _ := w.snapshot(61); got != 0 {
		t.Fatalf("expected 0 tokens after window, got %v", got)
	}
}

func TestBucketProratesTheStraddlingEdge(t *testing.T) {
	// One big bucket spanning [0,10); at now=65 the window [5,65] covers half of it.
	w := newBucketWindow(60, 10)
	w.add(0, 100, 0, 0)
	got, _, _ := w.snapshot(65)
	if got < 40 || got > 60 {
		t.Fatalf("expected ~half (40..60) prorated, got %v", got)
	}
}

func TestPreciseWindowIsExact(t *testing.T) {
	w := newPreciseWindow(60)
	w.add(0, 10, 1, 0)
	w.add(59, 10, 1, 0)
	if _, r, _ := w.snapshot(59.5); r != 2 {
		t.Fatalf("expected 2 requests in window, got %v", r)
	}
	// First event drops at t>60.
	if _, r, _ := w.snapshot(60.5); r != 1 {
		t.Fatalf("expected 1 request after first ages out, got %v", r)
	}
}

func TestAdjustUpdatesARecordedEntry(t *testing.T) {
	w := newBucketWindow(60, 1)
	h := w.add(0, 100, 1, 1.0)
	w.adjust(h, -40, 0, -0.4)
	if got, _, _ := w.snapshot(1); got != 60 {
		t.Fatalf("expected 60 tokens after adjust, got %v", got)
	}
	if _, _, c := w.snapshot(1); math.Round(c*1e6)/1e6 != 0.6 {
		t.Fatalf("expected cost 0.6 after adjust, got %v", c)
	}
}
