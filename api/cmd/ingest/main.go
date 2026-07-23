package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"time"

	"risk.lucanian.app/api/internal/collectors"
	"risk.lucanian.app/api/internal/config"
	"risk.lucanian.app/api/internal/db"
)

// The template ships no concrete collectors. Niche collectors register
// themselves via collectors.Register from package init() (e.g. a clone's
// api/collectors/ or an internal/ package imported here with a blank import).
func main() {
	collectorName := flag.String("collector", "", "Registered collector to run (see -list)")
	list := flag.Bool("list", false, "List registered collectors and exit")
	rateLimit := flag.Int("rate-limit", 60, "Source requests per minute")
	maxRetries := flag.Int("max-retries", 3, "Fetch retries after the first attempt")
	batchSize := flag.Int("batch-size", 100, "Entities per upsert transaction")
	flag.Parse()

	if *list {
		for _, name := range collectors.List() {
			fmt.Println(name)
		}
		return
	}
	if *collectorName == "" {
		fmt.Fprintf(os.Stderr, "Usage: go run ./cmd/ingest -collector <name> [-rate-limit N] [-max-retries N] [-batch-size N]\n")
		fmt.Fprintf(os.Stderr, "Registered collectors: %v\n", collectors.List())
		os.Exit(2)
	}

	c, ok := collectors.Get(*collectorName)
	if !ok {
		log.Fatalf("unknown collector %q; registered: %v", *collectorName, collectors.List())
	}

	config.MustLoad()
	if err := db.Init(); err != nil {
		log.Fatal(err)
	}
	if err := db.Migrate(); err != nil {
		log.Fatal(err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Minute)
	defer cancel()

	res, err := collectors.Run(ctx, c, db.DB, collectors.RunOptions{
		RateLimitPerMin: *rateLimit,
		MaxRetries:      *maxRetries,
		BatchSize:       *batchSize,
	})
	if err != nil {
		log.Fatalf("collector %s failed: %v (partial result: %+v)", *collectorName, err, res)
	}
	log.Printf("Ingest complete: %+v", res)
}
