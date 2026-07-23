package collectors

import (
	"context"
	"database/sql"
	"errors"
	"path/filepath"
	"testing"
	"time"

	templatedb "risk.lucanian.app/api/internal/db"
	_ "modernc.org/sqlite"
)

// fakeCollector returns two entities, failing Fetch on the first call.
type fakeCollector struct {
	calls int
	fail  bool
}

func (f *fakeCollector) Name() string { return "fake" }

func (f *fakeCollector) Fetch(ctx context.Context) ([]RawEntity, error) {
	f.calls++
	if f.fail && f.calls == 1 {
		return nil, errors.New("transient source error")
	}
	return []RawEntity{
		{SourceID: "a", Raw: []byte(`{"x":1}`)},
		{SourceID: "b", Raw: []byte(`{"x":2}`)},
	}, nil
}

func (f *fakeCollector) Normalize(r RawEntity) (Entity, error) {
	return Entity{
		Platform: "test", Slug: "owner", Name: r.SourceID,
		FullName: "owner/" + r.SourceID, Description: "fake", Category: "general",
		Metadata: string(r.Raw),
	}, nil
}

func openTestDB(t *testing.T) *sql.DB {
	t.Helper()
	dsn := filepath.Join(t.TempDir(), "test.db")
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	if err := templatedb.MigrateWith(db); err != nil {
		t.Fatalf("migrate: %v", err)
	}
	return db
}

func TestRunUpsertsAllEntities(t *testing.T) {
	db := openTestDB(t)
	fc := &fakeCollector{}
	res, err := Run(context.Background(), fc, db, RunOptions{RateLimitPerMin: 0, MaxRetries: 0, BatchSize: 10})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if res.Fetched != 2 || res.Upserted != 2 || res.Failed != 0 {
		t.Fatalf("got %+v, want {2 2 0}", res)
	}
	var n int
	if err := db.QueryRow("SELECT COUNT(*) FROM entities WHERE platform = 'test'").Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Fatalf("entities = %d, want 2", n)
	}
	// Second run: idempotent upsert, still 2 rows.
	if _, err := Run(context.Background(), fc, db, RunOptions{BatchSize: 10}); err != nil {
		t.Fatalf("second Run: %v", err)
	}
	if err := db.QueryRow("SELECT COUNT(*) FROM entities WHERE platform = 'test'").Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Fatalf("after re-run entities = %d, want 2 (upsert must be idempotent)", n)
	}
}

func TestRunRetriesFailedFetchAndLogsRun(t *testing.T) {
	db := openTestDB(t)
	fc := &fakeCollector{fail: true}
	res, err := Run(context.Background(), fc, db, RunOptions{MaxRetries: 2, BatchSize: 10})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if fc.calls != 2 {
		t.Fatalf("Fetch calls = %d, want 2 (one retry)", fc.calls)
	}
	if res.Upserted != 2 {
		t.Fatalf("Upserted = %d, want 2", res.Upserted)
	}
	var runs int
	var errText sql.NullString
	if err := db.QueryRow("SELECT COUNT(*), error FROM collector_runs WHERE collector = 'fake'").Scan(&runs, &errText); err != nil {
		t.Fatal(err)
	}
	if runs != 1 {
		t.Fatalf("collector_runs rows = %d, want 1", runs)
	}
}

// errCollector always fails; Normalize must never be called with partial data.
type errCollector struct{}

func (e *errCollector) Name() string { return "err" }
func (e *errCollector) Fetch(ctx context.Context) ([]RawEntity, error) {
	return nil, errors.New("permanent failure")
}
func (e *errCollector) Normalize(r RawEntity) (Entity, error) { return Entity{}, nil }

func TestRunExhaustsRetriesAndRecordsError(t *testing.T) {
	db := openTestDB(t)
	_, err := Run(context.Background(), &errCollector{}, db, RunOptions{MaxRetries: 1, BatchSize: 10})
	if err == nil {
		t.Fatal("Run should return the fetch error after exhausting retries")
	}
	var errText string
	if err := db.QueryRow("SELECT error FROM collector_runs WHERE collector = 'err'").Scan(&errText); err != nil {
		t.Fatalf("run must be logged even on failure: %v", err)
	}
	if errText == "" {
		t.Fatal("collector_runs.error must contain the failure message")
	}
	var n int
	_ = db.QueryRow("SELECT COUNT(*) FROM entities").Scan(&n)
	if n != 0 {
		t.Fatalf("entities = %d, want 0 (no partial commit)", n)
	}
}

// batchFailCollector: Normalize fails on the second entity; the batch must roll back.
type batchFailCollector struct{}

func (b *batchFailCollector) Name() string { return "batchfail" }
func (b *batchFailCollector) Fetch(ctx context.Context) ([]RawEntity, error) {
	return []RawEntity{{SourceID: "ok"}, {SourceID: "bad"}}, nil
}
func (b *batchFailCollector) Normalize(r RawEntity) (Entity, error) {
	if r.SourceID == "bad" {
		return Entity{}, errors.New("normalize failed")
	}
	return Entity{Platform: "test", Slug: "o", Name: "ok", FullName: "o/ok"}, nil
}

func TestRunRollsBackFailedBatch(t *testing.T) {
	db := openTestDB(t)
	_, err := Run(context.Background(), &batchFailCollector{}, db, RunOptions{BatchSize: 10})
	if err == nil {
		t.Fatal("Run should return the normalize error")
	}
	var n int
	_ = db.QueryRow("SELECT COUNT(*) FROM entities WHERE platform = 'test'").Scan(&n)
	if n != 0 {
		t.Fatalf("entities = %d, want 0 (failed batch must not partially commit)", n)
	}
}

func TestNewIDIsUnique(t *testing.T) {
	seen := map[string]bool{}
	for i := 0; i < 1000; i++ {
		id := templatedb.NewID()
		if seen[id] {
			t.Fatalf("NewID returned duplicate %q at iteration %d", id, i)
		}
		seen[id] = true
	}
}

func TestRateLimiterPaces(t *testing.T) {
	rl := NewRateLimiter(120) // 2 per second
	start := time.Now()
	rl.Wait(context.Background())
	rl.Wait(context.Background())
	rl.Wait(context.Background())
	if elapsed := time.Since(start); elapsed < 900*time.Millisecond {
		t.Fatalf("3 permits at 120/min took %v, want >= ~1s of pacing", elapsed)
	}
}
