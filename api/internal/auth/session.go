package auth

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"
)

const sessionCookieName = "session"

// SessionManager issues and validates HMAC-signed session cookies.
// Sessions are stateless: the cookie carries the user payload + expiry.
type SessionManager struct {
	key    []byte
	secure bool
	maxAge time.Duration
}

func NewSessionManager(signingKey []byte, secure bool) *SessionManager {
	return &SessionManager{key: signingKey, secure: secure, maxAge: 7 * 24 * time.Hour}
}

type sessionPayload struct {
	UserID      string   `json:"uid"`
	Email       string   `json:"email"`
	DisplayName string   `json:"name"`
	Groups      []string `json:"groups"`
	Premium     bool     `json:"premium"`
	Expiry      int64    `json:"exp"`
}

func (m *SessionManager) sign(payload []byte) string {
	mac := hmac.New(sha256.New, m.key)
	mac.Write(payload)
	return base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
}

// Create writes the session cookie for u.
func (m *SessionManager) Create(w http.ResponseWriter, u *User) {
	p := sessionPayload{
		UserID: u.ID, Email: u.Email, DisplayName: u.DisplayName,
		Groups: u.Groups, Premium: u.Premium,
		Expiry: time.Now().Add(m.maxAge).Unix(),
	}
	raw, _ := json.Marshal(p)
	value := base64.RawURLEncoding.EncodeToString(raw) + "." + m.sign(raw)
	http.SetCookie(w, &http.Cookie{
		Name:     sessionCookieName,
		Value:    value,
		Path:     "/",
		MaxAge:   int(m.maxAge.Seconds()),
		HttpOnly: true,
		Secure:   m.secure,
		SameSite: http.SameSiteLaxMode,
	})
}

// Read validates the session cookie and returns the session user, or an error.
func (m *SessionManager) Read(r *http.Request) (*User, error) {
	c, err := r.Cookie(sessionCookieName)
	if err != nil {
		return nil, err
	}
	parts := strings.Split(c.Value, ".")
	if len(parts) != 2 {
		return nil, errors.New("malformed session cookie")
	}
	raw, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return nil, err
	}
	if !hmac.Equal([]byte(parts[1]), []byte(m.sign(raw))) {
		return nil, errors.New("bad session signature")
	}
	var p sessionPayload
	if err := json.Unmarshal(raw, &p); err != nil {
		return nil, err
	}
	if time.Now().Unix() > p.Expiry {
		return nil, fmt.Errorf("session expired")
	}
	return &User{ID: p.UserID, Email: p.Email, DisplayName: p.DisplayName, Groups: p.Groups, Premium: p.Premium}, nil
}

// Clear expires the session cookie.
func (m *SessionManager) Clear(w http.ResponseWriter) {
	http.SetCookie(w, &http.Cookie{
		Name: sessionCookieName, Value: "", Path: "/", MaxAge: -1,
		HttpOnly: true, Secure: m.secure, SameSite: http.SameSiteLaxMode,
	})
}
