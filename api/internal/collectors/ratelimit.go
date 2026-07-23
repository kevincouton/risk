package collectors

import (
	"context"
	"time"
)

// RateLimiter paces source requests to at most perMin per minute.
// A perMin of 0 disables pacing.
type RateLimiter struct {
	interval time.Duration
	last     time.Time
}

func NewRateLimiter(perMin int) *RateLimiter {
	r := &RateLimiter{}
	if perMin > 0 {
		r.interval = time.Minute / time.Duration(perMin)
	}
	return r
}

// Wait blocks until the next permit is available or ctx is cancelled.
// The first call never blocks.
func (r *RateLimiter) Wait(ctx context.Context) error {
	if r.interval == 0 {
		return nil
	}
	now := time.Now()
	if !r.last.IsZero() {
		next := r.last.Add(r.interval)
		if next.After(now) {
			select {
			case <-ctx.Done():
				return ctx.Err()
			case <-time.After(next.Sub(now)):
			}
		}
	}
	r.last = time.Now()
	return nil
}
