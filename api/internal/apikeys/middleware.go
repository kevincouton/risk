package apikeys

import (
	"database/sql"
	"net/http"
	"strconv"

	"risk.lucanian.app/api/internal/db"
)

// KeyAuth rejects requests without a valid X-API-Key (401) and records
// usage for valid ones.
func KeyAuth(conn *sql.DB) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			keyID := lookupKey(r, conn)
			if keyID == "" {
				http.Error(w, `{"error":"invalid or missing API key"}`, http.StatusUnauthorized)
				return
			}
			_, _ = conn.ExecContext(r.Context(),
				"INSERT INTO api_usage (id, key_id, endpoint) VALUES (?, ?, ?)",
				db.NewID(), keyID, r.URL.Path)
			r.Header.Set("X-API-Key-ID", keyID) // for RateLimit downstream
			next.ServeHTTP(w, r)
		})
	}
}

// RateLimit enforces a sliding 60-second window of requestsPerMin per key,
// counted from api_usage. Compose INSIDE KeyAuth (KeyAuth records the usage
// row and stamps X-API-Key-ID first):
// KeyAuth(conn)(RateLimit(conn, 60)(handler)).
// With N requests already recorded this minute, request N+1 passes while
// N+1 <= requestsPerMin.
func RateLimit(conn *sql.DB, requestsPerMin int) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			keyID := r.Header.Get("X-API-Key-ID")
			if keyID == "" {
				next.ServeHTTP(w, r) // no key context — KeyAuth decides
				return
			}
			var used int
			if err := conn.QueryRowContext(r.Context(),
				"SELECT COUNT(*) FROM api_usage WHERE key_id = ? AND ts > datetime('now', '-60 seconds')",
				keyID).Scan(&used); err != nil {
				http.Error(w, `{"error":"rate limit check failed"}`, http.StatusInternalServerError)
				return
			}
			remaining := requestsPerMin - used
			if remaining < 0 {
				remaining = 0
			}
			w.Header().Set("X-RateLimit-Remaining", strconv.Itoa(remaining))
			if used > requestsPerMin {
				http.Error(w, `{"error":"rate limit exceeded"}`, http.StatusTooManyRequests)
				return
			}
			next.ServeHTTP(w, r)
		})
	}
}
