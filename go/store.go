package quotagate

import (
	"math"
	"sort"
)

// Dimension indices shared across the codebase.
const (
	TOKENS   = 0
	REQUESTS = 1
	COST     = 2
)

// window is a per-key sliding-window aggregate.
type window interface {
	snapshot(now float64) (t, r, c float64)
	add(now, tokens, requests, cost float64) any
	adjust(handle any, dt, dr, dc float64)
	timeToFree(now, over float64, dim int) float64
}

// bucketWindow keeps fixed-size time buckets. Memory is bounded to ~window/bucket
// entries regardless of traffic; the trailing bucket is prorated by the fraction
// of it still inside the window.
type bucketWindow struct {
	window  float64
	bucket  float64
	buckets map[int64]*[3]float64
}

func newBucketWindow(win, bucket float64) *bucketWindow {
	if bucket < 1e-9 {
		bucket = 1e-9
	}
	return &bucketWindow{window: win, bucket: bucket, buckets: map[int64]*[3]float64{}}
}

func (w *bucketWindow) evict(now float64) {
	lo := now - w.window
	for idx := range w.buckets {
		if float64(idx+1)*w.bucket <= lo {
			delete(w.buckets, idx)
		}
	}
}

func (w *bucketWindow) snapshot(now float64) (float64, float64, float64) {
	lo := now - w.window
	var t, r, c float64
	for idx, v := range w.buckets {
		bStart := float64(idx) * w.bucket
		bEnd := bStart + w.bucket
		if bEnd <= lo {
			continue
		}
		frac := 1.0
		if bStart < lo {
			frac = (bEnd - lo) / w.bucket
		}
		t += v[TOKENS] * frac
		r += v[REQUESTS] * frac
		c += v[COST] * frac
	}
	return t, r, c
}

func (w *bucketWindow) add(now, tokens, requests, cost float64) any {
	w.evict(now)
	idx := int64(math.Floor(now / w.bucket))
	v := w.buckets[idx]
	if v == nil {
		v = &[3]float64{}
		w.buckets[idx] = v
	}
	v[TOKENS] += tokens
	v[REQUESTS] += requests
	v[COST] += cost
	return idx
}

func (w *bucketWindow) adjust(handle any, dt, dr, dc float64) {
	idx := handle.(int64)
	if v, ok := w.buckets[idx]; ok {
		v[TOKENS] += dt
		v[REQUESTS] += dr
		v[COST] += dc
	}
}

func (w *bucketWindow) timeToFree(now, over float64, dim int) float64 {
	lo := now - w.window
	type entry struct {
		bEnd float64
		amt  float64
	}
	var entries []entry
	for idx, v := range w.buckets {
		bEnd := float64(idx+1) * w.bucket
		if bEnd <= lo || v[dim] <= 0 {
			continue
		}
		entries = append(entries, entry{bEnd, v[dim]})
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].bEnd < entries[j].bEnd })
	freed := 0.0
	for _, e := range entries {
		freed += e.amt
		if freed >= over-1e-9 {
			return math.Max(0.0, e.bEnd+w.window-now)
		}
	}
	return w.window
}

// bucketCount reports the number of live buckets, used by the stress tests.
func (w *bucketWindow) bucketCount() int { return len(w.buckets) }

// preciseWindow keeps an exact per-event log. Memory grows with in-window traffic.
type preciseWindow struct {
	window float64
	events []*[4]float64 // {ts, tokens, requests, cost}
}

func newPreciseWindow(win float64) *preciseWindow {
	return &preciseWindow{window: win}
}

func (w *preciseWindow) evict(now float64) {
	lo := now - w.window
	live := make([]*[4]float64, 0, len(w.events))
	for _, e := range w.events {
		if e[0] > lo {
			live = append(live, e)
		}
	}
	w.events = live
}

func (w *preciseWindow) snapshot(now float64) (float64, float64, float64) {
	lo := now - w.window
	var t, r, c float64
	for _, e := range w.events {
		if e[0] > lo {
			t += e[1]
			r += e[2]
			c += e[3]
		}
	}
	return t, r, c
}

func (w *preciseWindow) add(now, tokens, requests, cost float64) any {
	w.evict(now)
	e := &[4]float64{now, tokens, requests, cost}
	w.events = append(w.events, e)
	return e
}

func (w *preciseWindow) adjust(handle any, dt, dr, dc float64) {
	e := handle.(*[4]float64)
	e[1] += dt
	e[2] += dr
	e[3] += dc
}

func (w *preciseWindow) timeToFree(now, over float64, dim int) float64 {
	lo := now - w.window
	var live []*[4]float64
	for _, e := range w.events {
		if e[0] > lo {
			live = append(live, e)
		}
	}
	sort.Slice(live, func(i, j int) bool { return live[i][0] < live[j][0] })
	freed := 0.0
	col := dim + 1
	for _, e := range live {
		freed += e[col]
		if freed >= over-1e-9 {
			return math.Max(0.0, e[0]+w.window-now)
		}
	}
	return w.window
}

// Store is the persistence seam. Implement it over Redis (atomic INCR plus TTL per
// bucket) for distributed enforcement across replicas.
type Store interface {
	// Snapshot returns the current (tokens, requests, cost) totals for a key.
	Snapshot(key string, now, window, bucket float64) (t, r, c float64)
	// Add records usage and returns an opaque handle for later adjustment.
	Add(key string, now, window, bucket, tokens, requests, cost float64) any
	// Adjust reconciles a previously recorded entry by the given deltas.
	Adjust(key string, handle any, dt, dr, dc float64)
	// TimeToFree returns how long until "over" units of a dimension age out.
	TimeToFree(key string, now, window, bucket, over float64, dim int) float64
	// Concurrency returns the current in-flight count for a key.
	Concurrency(key string) int
	// TryAddConcurrency increments the in-flight count if below the limit.
	TryAddConcurrency(key string, limit int) bool
	// ReleaseConcurrency decrements the in-flight count.
	ReleaseConcurrency(key string)
}

// InMemoryStore is the single-process default. One window object and one counter
// per key. It is not safe for concurrent use; shard per worker or plug in a shared
// store for a multi-threaded server.
type InMemoryStore struct {
	windows     map[string]window
	concurrency map[string]int
}

// NewInMemoryStore returns an empty in-memory store.
func NewInMemoryStore() *InMemoryStore {
	return &InMemoryStore{windows: map[string]window{}, concurrency: map[string]int{}}
}

func (s *InMemoryStore) windowFor(key string, win, bucket float64) window {
	w, ok := s.windows[key]
	if !ok {
		if bucket <= 0 {
			w = newPreciseWindow(win)
		} else {
			w = newBucketWindow(win, bucket)
		}
		s.windows[key] = w
	}
	return w
}

// Snapshot returns the current totals for a key.
func (s *InMemoryStore) Snapshot(key string, now, win, bucket float64) (float64, float64, float64) {
	return s.windowFor(key, win, bucket).snapshot(now)
}

// Add records usage for a key and returns an opaque handle.
func (s *InMemoryStore) Add(key string, now, win, bucket, tokens, requests, cost float64) any {
	return s.windowFor(key, win, bucket).add(now, tokens, requests, cost)
}

// Adjust reconciles a recorded entry by the given deltas.
func (s *InMemoryStore) Adjust(key string, handle any, dt, dr, dc float64) {
	if w, ok := s.windows[key]; ok {
		w.adjust(handle, dt, dr, dc)
	}
}

// TimeToFree returns how long until "over" units of a dimension age out of the key.
func (s *InMemoryStore) TimeToFree(key string, now, win, bucket, over float64, dim int) float64 {
	return s.windowFor(key, win, bucket).timeToFree(now, over, dim)
}

// Concurrency returns the current in-flight count for a key.
func (s *InMemoryStore) Concurrency(key string) int { return s.concurrency[key] }

// TryAddConcurrency increments the in-flight count if it is below the limit.
func (s *InMemoryStore) TryAddConcurrency(key string, limit int) bool {
	if s.concurrency[key] >= limit {
		return false
	}
	s.concurrency[key]++
	return true
}

// ReleaseConcurrency decrements the in-flight count, never below zero.
func (s *InMemoryStore) ReleaseConcurrency(key string) {
	if s.concurrency[key] > 0 {
		s.concurrency[key]--
	}
}
