// Package collectors is the niche data-ingestion framework every platform
// pipeline builds on. A Collector fetches raw items from one source and
// normalizes them into entities; Run orchestrates fetch → normalize →
// upsert with retry, rate limiting, batch transactions, and run logging.
package collectors

import (
	"context"
	"sort"
	"sync"
)

// RawEntity is one unprocessed item from a source.
type RawEntity struct {
	SourceID string
	Raw      []byte
}

// Entity is the normalized form upserted into the entities table.
// Slug holds the owner, Name the item name, FullName is "owner/name"
// (existing template convention used by scoring and the API).
type Entity struct {
	Platform    string
	Slug        string
	Name        string
	FullName    string
	Description string
	Category    string
	Metadata    string
}

// Collector pulls entities from one niche source.
type Collector interface {
	Name() string
	Fetch(ctx context.Context) ([]RawEntity, error)
	Normalize(RawEntity) (Entity, error)
}

var (
	registryMu sync.RWMutex
	registry   = map[string]Collector{}
)

// Register makes a collector available to cmd/ingest by name.
// It panics on duplicate names (programmer error at init time).
func Register(c Collector) {
	registryMu.Lock()
	defer registryMu.Unlock()
	if _, dup := registry[c.Name()]; dup {
		panic("collectors: duplicate registration " + c.Name())
	}
	registry[c.Name()] = c
}

// Get returns a registered collector by name.
func Get(name string) (Collector, bool) {
	registryMu.RLock()
	defer registryMu.RUnlock()
	c, ok := registry[name]
	return c, ok
}

// List returns the names of all registered collectors, sorted.
func List() []string {
	registryMu.RLock()
	defer registryMu.RUnlock()
	names := make([]string, 0, len(registry))
	for n := range registry {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}
