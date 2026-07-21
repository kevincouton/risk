package scoring

import (
	"path/filepath"
	"testing"
	"time"

	"risk.lucanian.app/api/internal/config"
	"risk.lucanian.app/api/internal/db"
)

func setupTestDB(t *testing.T) {
	t.Helper()
	config.DatabaseURL = "file:" + filepath.Join(t.TempDir(), "test.db")
	if err := db.Init(); err != nil {
		t.Fatalf("Init: %v", err)
	}
	t.Cleanup(func() { db.DB.Close() })
	if err := db.Migrate(); err != nil {
		t.Fatalf("Migrate: %v", err)
	}
}

func TestCalculateTrajectorySignals(t *testing.T) {
	setupTestDB(t)
	pushed := time.Now().Add(-10 * 24 * time.Hour).UTC().Format(time.RFC3339)
	if _, err := db.DB.Exec(
		"INSERT INTO entities (id, platform, slug, name, full_name, score_value, last_pushed_at, open_issues) VALUES ('e1','github','a/b','b','a/b', 1000, ?, 20)",
		pushed,
	); err != nil {
		t.Fatalf("insert entity: %v", err)
	}

	signals, err := CalculateTrajectorySignals("e1")
	if err != nil {
		t.Fatalf("CalculateTrajectorySignals: %v", err)
	}
	if signals.DaysSinceLastPush < 9 || signals.DaysSinceLastPush > 11 {
		t.Fatalf("DaysSinceLastPush = %d, want ~10", signals.DaysSinceLastPush)
	}
	if signals.OpenIssuesRatio != 0.02 {
		t.Fatalf("OpenIssuesRatio = %v, want 0.02", signals.OpenIssuesRatio)
	}
	if signals.ReleaseVelocityDays != nil {
		t.Fatalf("ReleaseVelocityDays = %v, want nil (no releases)", *signals.ReleaseVelocityDays)
	}
}

func TestTrajectoryScoreBounds(t *testing.T) {
	hot := &TrajectorySignals{DaysSinceLastPush: 5, OpenIssuesRatio: 0.01}
	if s := TrajectoryScore(hot); s <= 50 || s > 100 {
		t.Fatalf("hot entity scored %v, want (50, 100]", s)
	}
	cold := &TrajectorySignals{DaysSinceLastPush: 200, OpenIssuesRatio: 0.5}
	if s := TrajectoryScore(cold); s >= 50 || s < 0 {
		t.Fatalf("cold entity scored %v, want [0, 50)", s)
	}
}
