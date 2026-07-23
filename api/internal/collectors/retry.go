package collectors

import (
	"context"
	"time"
)

// withRetries calls fn up to 1+maxRetries times, backing off 1s, 2s, 4s, …
// (capped at 30s) between attempts. A maxRetries of 0 means a single attempt.
func withRetries(ctx context.Context, maxRetries int, fn func() error) error {
	var err error
	for attempt := 0; attempt <= maxRetries; attempt++ {
		if err = fn(); err == nil {
			return nil
		}
		if attempt == maxRetries {
			break
		}
		backoff := time.Duration(1<<attempt) * time.Second
		if backoff > 30*time.Second {
			backoff = 30 * time.Second
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(backoff):
		}
	}
	return err
}
