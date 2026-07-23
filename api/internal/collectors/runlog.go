package collectors

import (
	"database/sql"
	"time"

	"risk.lucanian.app/api/internal/db"
)

// runLogger persists one row per Run into collector_runs.
type runLogger struct {
	conn      *sql.DB
	id        string
	collector string
	started   time.Time
}

func startRun(conn *sql.DB, collector string) *runLogger {
	r := &runLogger{conn: conn, id: db.NewID(), collector: collector, started: time.Now().UTC()}
	return r
}

// finish records the outcome; errorText is empty on success.
func (r *runLogger) finish(res RunResult, errorText string) {
	var errVal sql.NullString
	if errorText != "" {
		errVal = sql.NullString{String: errorText, Valid: true}
	}
	_, _ = r.conn.Exec(`
		INSERT INTO collector_runs (id, collector, started_at, finished_at, fetched, upserted, error)
		VALUES (?, ?, ?, ?, ?, ?, ?)
	`, r.id, r.collector, r.started.Format(time.RFC3339), time.Now().UTC().Format(time.RFC3339),
		res.Fetched, res.Upserted, errVal)
}
