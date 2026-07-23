// Package apikeys provides metered API access: hashed key storage,
// X-API-Key middleware, and a sliding-window rate limiter.
// Everything is inert unless API_KEYS_ENABLED=true.
package apikeys

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"fmt"
	"net/http"

	"risk.lucanian.app/api/internal/db"
)

// KeyInfo is the safe-to-display view of an API key (never the hash).
type KeyInfo struct {
	ID        string `json:"id"`
	Label     string `json:"label"`
	CreatedAt string `json:"created_at"`
	Revoked   bool   `json:"revoked"`
}

func hashKey(plaintext string) string {
	sum := sha256.Sum256([]byte(plaintext))
	return hex.EncodeToString(sum[:])
}

// CreateKey generates a new key, stores only its SHA-256 hash, and returns
// the plaintext exactly once.
func CreateKey(ctx context.Context, conn *sql.DB, userID, label string) (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	plaintext := "pk_" + hex.EncodeToString(b[:])
	_, err := conn.ExecContext(ctx,
		"INSERT INTO api_keys (id, user_id, key_hash, label) VALUES (?, ?, ?, ?)",
		db.NewID(), userID, hashKey(plaintext), label)
	if err != nil {
		return "", err
	}
	return plaintext, nil
}

// RevokeKey marks a key revoked. userID scopes the operation to the owner.
func RevokeKey(ctx context.Context, conn *sql.DB, keyID, userID string) error {
	res, err := conn.ExecContext(ctx,
		"UPDATE api_keys SET revoked_at = datetime('now') WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
		keyID, userID)
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return fmt.Errorf("key %s not found or already revoked", keyID)
	}
	return nil
}

// ListKeys returns all keys (including revoked) for a user.
func ListKeys(ctx context.Context, conn *sql.DB, userID string) ([]KeyInfo, error) {
	rows, err := conn.QueryContext(ctx,
		"SELECT id, label, created_at, revoked_at IS NOT NULL FROM api_keys WHERE user_id = ? ORDER BY created_at DESC",
		userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []KeyInfo
	for rows.Next() {
		var k KeyInfo
		if err := rows.Scan(&k.ID, &k.Label, &k.CreatedAt, &k.Revoked); err != nil {
			return nil, err
		}
		out = append(out, k)
	}
	return out, rows.Err()
}

// lookupKey resolves a plaintext key to its row id, or "" if invalid/revoked.
func lookupKey(r *http.Request, conn *sql.DB) string {
	presented := r.Header.Get("X-API-Key")
	if presented == "" {
		return ""
	}
	var id string
	err := conn.QueryRowContext(r.Context(),
		"SELECT id FROM api_keys WHERE key_hash = ? AND revoked_at IS NULL",
		hashKey(presented)).Scan(&id)
	if err != nil {
		return ""
	}
	return id
}
