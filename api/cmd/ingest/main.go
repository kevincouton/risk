package main

import (
	"encoding/json"
	"flag"
	"log"
	"os"
	"strings"
	"time"

	"risk.lucanian.app/api/internal/config"
	"risk.lucanian.app/api/internal/db"
)

func main() {
	platformFlag := flag.String("platform", "default", "Source platform")
	flag.Parse()

	config.MustLoad()
	if err := db.Init(); err != nil {
		log.Fatal(err)
	}
	if err := db.Migrate(); err != nil {
		log.Fatal(err)
	}

	var slugs []string
	args := flag.Args()
	if len(args) > 0 {
		slugs = strings.Split(args[0], ",")
	}

	if len(slugs) == 0 {
		log.Fatal("Usage: go run ./cmd/ingest [--platform=default] slug/name,slug/name")
	}

	log.Printf("Ingesting %d entities from %s...", len(slugs), *platformFlag)

	for _, fullName := range slugs {
		parts := strings.Split(strings.TrimSpace(fullName), "/")
		if len(parts) != 2 {
			log.Printf("Skipping invalid: %s", fullName)
			continue
		}
		owner, name := parts[0], parts[1]

		// TODO: Implement platform-specific ingestion
		// For now, insert a placeholder entity
		_, err := db.DB.Exec(`
			INSERT INTO entities (id, platform, slug, name, full_name, description, category, score_value, metadata)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
			ON CONFLICT(platform, slug) DO UPDATE SET
				score_value = excluded.score_value,
				metadata = excluded.metadata,
				scraped_at = datetime('now')
		`, db.NewID(), *platformFlag, owner, name, fullName, "Ingested entity", "general", 0, "{}")

		if err != nil {
			log.Printf("Error inserting %s: %v", fullName, err)
			continue
		}

		log.Printf("  ✓ %s", fullName)
		time.Sleep(500 * time.Millisecond)
	}

	log.Println("Ingest complete")
}

func mustJSON(v interface{}) string {
	b, _ := json.Marshal(v)
	return string(b)
}
