package collectors

import (
	"context"
	"database/sql"
	"fmt"
	"log"

	"risk.lucanian.app/api/internal/db"
)

// RunOptions tunes a single collection run.
type RunOptions struct {
	RateLimitPerMin int // source requests per minute; 0 disables pacing
	MaxRetries      int // fetch retries after the first attempt
	BatchSize       int // entities per upsert transaction; <=0 means 100
}

// RunResult summarizes a completed run.
type RunResult struct {
	Fetched  int
	Upserted int
	Failed   int
}

// Run executes one collection: fetch (with retry) → normalize → upsert in
// per-batch transactions, logging the outcome to collector_runs. A failed
// batch rolls back entirely — runs never partially commit (spec §5.2).
func Run(ctx context.Context, c Collector, conn *sql.DB, opts RunOptions) (RunResult, error) {
	if opts.BatchSize <= 0 {
		opts.BatchSize = 100
	}
	res := RunResult{}
	rl := startRun(conn, c.Name())

	var raw []RawEntity
	fetchErr := withRetries(ctx, opts.MaxRetries, func() error {
		if err := NewRateLimiter(opts.RateLimitPerMin).Wait(ctx); err != nil {
			return err
		}
		var err error
		raw, err = c.Fetch(ctx)
		return err
	})
	if fetchErr != nil {
		rl.finish(res, fetchErr.Error())
		return res, fmt.Errorf("collector %s fetch: %w", c.Name(), fetchErr)
	}
	res.Fetched = len(raw)

	for start := 0; start < len(raw); start += opts.BatchSize {
		end := start + opts.BatchSize
		if end > len(raw) {
			end = len(raw)
		}
		n, err := upsertBatch(ctx, conn, c, raw[start:end])
		if err != nil {
			res.Failed += (end - start) - n
			rl.finish(res, err.Error())
			return res, fmt.Errorf("collector %s upsert batch: %w", c.Name(), err)
		}
		res.Upserted += n
	}

	rl.finish(res, "")
	log.Printf("collector %s: fetched=%d upserted=%d failed=%d", c.Name(), res.Fetched, res.Upserted, res.Failed)
	return res, nil
}

// upsertBatch normalizes and upserts one batch in a single transaction.
// On any normalize/upsert error the whole batch rolls back.
func upsertBatch(ctx context.Context, conn *sql.DB, c Collector, raws []RawEntity) (int, error) {
	tx, err := conn.BeginTx(ctx, nil)
	if err != nil {
		return 0, err
	}
	defer tx.Rollback()

	n := 0
	for _, r := range raws {
		e, err := c.Normalize(r)
		if err != nil {
			return n, err
		}
		if _, err := tx.ExecContext(ctx, `
			INSERT INTO entities (id, platform, slug, name, full_name, description, category, score_value, metadata)
			VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?)
			ON CONFLICT(platform, full_name) DO UPDATE SET
				name = excluded.name,
				full_name = excluded.full_name,
				description = excluded.description,
				category = excluded.category,
				metadata = excluded.metadata,
				scraped_at = datetime('now')
		`, db.NewID(), e.Platform, e.Slug, e.Name, e.FullName, e.Description, e.Category, e.Metadata); err != nil {
			return n, err
		}
		n++
	}
	return n, tx.Commit()
}
