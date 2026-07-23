package db

import (
	"crypto/rand"
	"database/sql"
	"embed"
	"fmt"

	"risk.lucanian.app/api/internal/config"
	_ "modernc.org/sqlite"
)

//go:embed schema.sql
var schemaFS embed.FS

var DB *sql.DB

func Init() error {
	var err error
	DB, err = sql.Open("sqlite", config.DatabaseURL)
	if err != nil {
		return fmt.Errorf("open db: %w", err)
	}
	return DB.Ping()
}

// Migrate applies schema.sql to the package-level DB.
func Migrate() error {
	return MigrateWith(DB)
}

// MigrateWith applies schema.sql (idempotent CREATE TABLE IF NOT EXISTS)
// plus guarded column migrations for databases created before schema v2.
func MigrateWith(conn *sql.DB) error {
	schema, err := schemaFS.ReadFile("schema.sql")
	if err != nil {
		return fmt.Errorf("read schema: %w", err)
	}
	if _, err := conn.Exec(string(schema)); err != nil {
		return fmt.Errorf("apply schema: %w", err)
	}
	// Guarded ALTERs for pre-v2 databases (SQLite has no ADD COLUMN IF NOT EXISTS).
	for _, m := range []struct{ table, column, ddl string }{
		{"entities", "last_pushed_at", "ALTER TABLE entities ADD COLUMN last_pushed_at TEXT"},
		{"entities", "open_issues", "ALTER TABLE entities ADD COLUMN open_issues INTEGER DEFAULT 0"},
		{"entity_scores", "release_velocity_days", "ALTER TABLE entity_scores ADD COLUMN release_velocity_days INTEGER"},
	} {
		exists, err := columnExists(conn, m.table, m.column)
		if err != nil {
			return err
		}
		if !exists {
			if _, err := conn.Exec(m.ddl); err != nil {
				return fmt.Errorf("add column %s.%s: %w", m.table, m.column, err)
			}
		}
	}
	// Pre-v2 databases keep the old table-level UNIQUE(platform, slug) and lack
	// a unique index on (platform, full_name), which the collector upsert uses
	// as its ON CONFLICT target. Create it if missing (no-op on fresh v2 DBs).
	if _, err := conn.Exec("CREATE UNIQUE INDEX IF NOT EXISTS ux_entities_platform_full_name ON entities(platform, full_name)"); err != nil {
		return fmt.Errorf("ensure entities unique index: %w", err)
	}
	return nil
}

func columnExists(conn *sql.DB, table, column string) (bool, error) {
	rows, err := conn.Query("PRAGMA table_info(" + table + ")")
	if err != nil {
		return false, fmt.Errorf("table_info %s: %w", table, err)
	}
	defer rows.Close()
	for rows.Next() {
		var cid int
		var name, ctype string
		var notnull int
		var dflt sql.NullString
		var pk int
		if err := rows.Scan(&cid, &name, &ctype, &notnull, &dflt, &pk); err != nil {
			return false, err
		}
		if name == column {
			return true, nil
		}
	}
	return false, rows.Err()
}

// NewID returns a random UUID v4.
func NewID() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(fmt.Sprintf("crypto/rand unavailable: %v", err))
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}
