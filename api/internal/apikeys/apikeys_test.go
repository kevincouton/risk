package apikeys

import (
	"context"
	"database/sql"
	"fmt"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	templatedb "risk.lucanian.app/api/internal/db"
	_ "modernc.org/sqlite"
)

func openTestDB(t *testing.T) *sql.DB {
	t.Helper()
	conn, err := sql.Open("sqlite", filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	if err := templatedb.MigrateWith(conn); err != nil {
		t.Fatal(err)
	}
	return conn
}

var okHandler = http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(200) })

func TestCreateKeyStoresHashNotPlaintext(t *testing.T) {
	conn := openTestDB(t)
	key, err := CreateKey(context.Background(), conn, "u1", "ci")
	if err != nil {
		t.Fatalf("CreateKey: %v", err)
	}
	if len(key) != 3+32 || key[:3] != "pk_" {
		t.Fatalf("key format = %q", key)
	}
	var hash string
	if err := conn.QueryRow("SELECT key_hash FROM api_keys WHERE user_id = 'u1'").Scan(&hash); err != nil {
		t.Fatal(err)
	}
	if hash == key || len(hash) != 64 {
		t.Fatalf("stored value must be the SHA-256 hex hash, got %q", hash)
	}
}

func TestKeyAuthValidInvalidRevoked(t *testing.T) {
	conn := openTestDB(t)
	key, err := CreateKey(context.Background(), conn, "u1", "ci")
	if err != nil {
		t.Fatal(err)
	}
	h := KeyAuth(conn)(okHandler)

	mk := func(k string) *httptest.ResponseRecorder {
		req := httptest.NewRequest("GET", "/api/v1/entities", nil)
		if k != "" {
			req.Header.Set("X-API-Key", k)
		}
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, req)
		return rec
	}
	if rec := mk(key); rec.Code != 200 {
		t.Fatalf("valid key = %d, want 200", rec.Code)
	}
	if rec := mk("pk_deadbeef"); rec.Code != 401 {
		t.Fatalf("invalid key = %d, want 401", rec.Code)
	}
	if rec := mk(""); rec.Code != 401 {
		t.Fatalf("missing key = %d, want 401", rec.Code)
	}

	// Revoke it.
	var keyID string
	_ = conn.QueryRow("SELECT id FROM api_keys WHERE user_id = 'u1'").Scan(&keyID)
	if err := RevokeKey(context.Background(), conn, keyID, "u1"); err != nil {
		t.Fatalf("RevokeKey: %v", err)
	}
	if rec := mk(key); rec.Code != 401 {
		t.Fatalf("revoked key = %d, want 401", rec.Code)
	}

	// Usage was recorded for the successful call.
	var usage int
	_ = conn.QueryRow("SELECT COUNT(*) FROM api_usage WHERE key_id = ?", keyID).Scan(&usage)
	if usage != 1 {
		t.Fatalf("api_usage rows = %d, want 1", usage)
	}
}

func TestRateLimit61stRequest429(t *testing.T) {
	conn := openTestDB(t)
	key, _ := CreateKey(context.Background(), conn, "u1", "ci")
	h := KeyAuth(conn)(RateLimit(conn, 60)(okHandler))

	send := func() *httptest.ResponseRecorder {
		req := httptest.NewRequest("GET", "/api/v1/entities", nil)
		req.Header.Set("X-API-Key", key)
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, req)
		return rec
	}
	var last *httptest.ResponseRecorder
	for i := 0; i < 60; i++ {
		if last = send(); last.Code != 200 {
			t.Fatalf("request %d = %d, want 200", i+1, last.Code)
		}
	}
	if rec := send(); rec.Code != 429 {
		t.Fatalf("61st request = %d, want 429", rec.Code)
	}
	if last.Header().Get("X-RateLimit-Remaining") == "" {
		t.Fatal("X-RateLimit-Remaining header must be set")
	}
}

func TestRateLimitWindowSlides(t *testing.T) {
	conn := openTestDB(t)
	key, _ := CreateKey(context.Background(), conn, "u1", "ci")
	var keyID string
	_ = conn.QueryRow("SELECT id FROM api_keys WHERE user_id = 'u1'").Scan(&keyID)
	// Seed 60 usage rows 61 seconds in the past — outside the window.
	for i := 0; i < 60; i++ {
		if _, err := conn.Exec(`INSERT INTO api_usage (id, key_id, ts, endpoint)
			VALUES (?, ?, datetime('now', '-61 seconds'), '/api/v1/entities')`, fmt.Sprintf("old-%d", i), keyID); err != nil {
			t.Fatal(err)
		}
	}
	h := KeyAuth(conn)(RateLimit(conn, 60)(okHandler))
	req := httptest.NewRequest("GET", "/api/v1/entities", nil)
	req.Header.Set("X-API-Key", key)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != 200 {
		t.Fatalf("request after window slid = %d, want 200", rec.Code)
	}
}
