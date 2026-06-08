package main

import (
	"log"
	"os"

	"risk.lucanian.app/api/internal/config"
	"risk.lucanian.app/api/internal/db"
	"risk.lucanian.app/api/internal/scoring"
)

func main() {
	config.MustLoad()
	if err := db.Init(); err != nil {
		log.Fatal(err)
	}
	if err := db.Migrate(); err != nil {
		log.Fatal(err)
	}

	var entityIDs []string
	if len(os.Args) > 1 && os.Args[1] == "--all" {
		rows, err := db.DB.Query("SELECT id FROM entities")
		if err != nil {
			log.Fatal(err)
		}
		defer rows.Close()
		for rows.Next() {
			var id string
			if err := rows.Scan(&id); err == nil {
				entityIDs = append(entityIDs, id)
			}
		}
	} else if len(os.Args) > 1 {
		var id string
		err := db.DB.QueryRow("SELECT id FROM entities WHERE full_name = ?", os.Args[1]).Scan(&id)
		if err != nil {
			log.Fatalf("Entity not found: %s", os.Args[1])
		}
		entityIDs = append(entityIDs, id)
	} else {
		log.Fatal("Usage: go run ./cmd/score --all  OR  go run ./cmd/score owner/name")
	}

	log.Printf("Scoring %d entities...", len(entityIDs))

	for _, entityID := range entityIDs {
		result, err := scoring.ScoreEntity(entityID, nil)
		if err != nil {
			log.Printf("Error scoring %s: %v", entityID, err)
			continue
		}

		if err := scoring.SaveScore(result); err != nil {
			log.Printf("Error saving score for %s: %v", entityID, err)
			continue
		}

		var fullName string
		_ = db.DB.QueryRow("SELECT full_name FROM entities WHERE id = ?", entityID).Scan(&fullName)

		log.Printf("  ✓ %s | Score: %d/100 | Verdict: %s | Trajectory: %s",
			fullName, result.CompositeScore, result.Verdict, result.Trajectory)
	}

	log.Println("Scoring complete")
}
