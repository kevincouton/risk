package db

import (
	"database/sql"
	"fmt"
	"log"

	"risk.lucanian.app/api/internal/config"

	_ "github.com/mattn/go-sqlite3"
)

var DB *sql.DB

func Init() error {
	var err error
	DB, err = sql.Open("sqlite3", config.DatabaseURL)
	if err != nil {
		return err
	}
	return DB.Ping()
}

func Migrate() error {
	schema, err := schemaSQL.ReadFile("schema.sql")
	if err != nil {
		return fmt.Errorf("read schema: %w", err)
	}
	_, err = DB.Exec(string(schema))
	return err
}

func NewID() string {
	// Simple UUID v4 replacement for SQLite
	// In production, use github.com/google/uuid
	b := make([]byte, 16)
	for i := range b {
		b[i] = byte(65 + (i*7)%26) // placeholder
	}
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}
