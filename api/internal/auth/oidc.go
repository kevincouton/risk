// Package auth implements OIDC login (Authentik), HMAC session cookies,
// and auth middleware for instantiated platforms. Everything is inert
// unless AUTH_ENABLED=true (fail-closed, spec §5.2).
package auth

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net/http"
	"time"

	"github.com/coreos/go-oidc/v3/oidc"
	"golang.org/x/oauth2"

	"risk.lucanian.app/api/internal/db"
)

// OIDCConfig configures the relying party. RedirectURL is <APP_URL>/auth/callback.
type OIDCConfig struct {
	IssuerURL    string
	ClientID     string
	ClientSecret string
	RedirectURL  string
}

// User is the authenticated principal (users table row + session payload).
type User struct {
	ID          string   `json:"id"`
	OIDCSub     string   `json:"-"`
	Email       string   `json:"email"`
	DisplayName string   `json:"display_name"`
	Groups      []string `json:"groups"`
	Premium     bool     `json:"premium"`
}

// TokenClaims are the verified OIDC claims we keep.
type TokenClaims struct {
	Sub    string
	Email  string
	Name   string
	Groups []string
}

// Provider handles the OIDC flow and session lifecycle.
type Provider struct {
	cfg      OIDCConfig
	sessions *SessionManager
	db       *sql.DB

	// Seams for tests (spec §5.4 — no network in tests).
	authCodeURL func(state string) string
	exchange    func(ctx context.Context, code string) (*TokenClaims, error)
}

// NewProvider builds a Provider against a live OIDC issuer (discovery).
func NewProvider(ctx context.Context, cfg OIDCConfig, signingKey []byte, conn *sql.DB) (*Provider, error) {
	op, err := oidc.NewProvider(ctx, cfg.IssuerURL)
	if err != nil {
		return nil, err
	}
	oauthCfg := oauth2.Config{
		ClientID:     cfg.ClientID,
		ClientSecret: cfg.ClientSecret,
		RedirectURL:  cfg.RedirectURL,
		Endpoint:     op.Endpoint(),
		Scopes:       []string{oidc.ScopeOpenID, "profile", "email", "groups"},
	}
	verifier := op.Verifier(&oidc.Config{ClientID: cfg.ClientID})
	p := &Provider{cfg: cfg, sessions: NewSessionManager(signingKey, true), db: conn}
	p.authCodeURL = func(state string) string {
		return oauthCfg.AuthCodeURL(state)
	}
	p.exchange = func(ctx context.Context, code string) (*TokenClaims, error) {
		tok, err := oauthCfg.Exchange(ctx, code)
		if err != nil {
			return nil, err
		}
		raw, ok := tok.Extra("id_token").(string)
		if !ok {
			return nil, errors.New("no id_token in token response")
		}
		id, err := verifier.Verify(ctx, raw)
		if err != nil {
			return nil, err
		}
		var claims struct {
			Email  string   `json:"email"`
			Name   string   `json:"name"`
			Groups []string `json:"groups"`
		}
		if err := id.Claims(&claims); err != nil {
			return nil, err
		}
		return &TokenClaims{Sub: id.Subject, Email: claims.Email, Name: claims.Name, Groups: claims.Groups}, nil
	}
	return p, nil
}

const stateCookieName = "oidc_state"

func randomState() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	return base64.RawURLEncoding.EncodeToString(b[:])
}

// HandleLogin starts the OIDC flow: state cookie + redirect to the issuer.
func (p *Provider) HandleLogin(w http.ResponseWriter, r *http.Request) {
	state := randomState()
	http.SetCookie(w, &http.Cookie{
		Name: stateCookieName, Value: state, Path: "/",
		MaxAge: 300, HttpOnly: true, Secure: p.sessions.secure, SameSite: http.SameSiteLaxMode,
	})
	http.Redirect(w, r, p.authCodeURL(state), http.StatusFound)
}

// HandleCallback completes the flow: verify state, exchange code, upsert
// the user, issue a session cookie, redirect to /.
func (p *Provider) HandleCallback(w http.ResponseWriter, r *http.Request) {
	c, err := r.Cookie(stateCookieName)
	if err != nil || c.Value == "" || c.Value != r.URL.Query().Get("state") {
		http.Error(w, "invalid oauth state", http.StatusBadRequest)
		return
	}
	claims, err := p.exchange(r.Context(), r.URL.Query().Get("code"))
	if err != nil {
		http.Error(w, "token exchange failed", http.StatusBadGateway)
		return
	}
	u, err := p.upsertUser(r.Context(), claims)
	if err != nil {
		http.Error(w, "user upsert failed", http.StatusInternalServerError)
		return
	}
	p.sessions.Create(w, u)
	http.Redirect(w, r, "/", http.StatusFound)
}

// HandleLogout clears the session.
func (p *Provider) HandleLogout(w http.ResponseWriter, r *http.Request) {
	p.sessions.Clear(w)
	w.Header().Set("Content-Type", "application/json")
	w.Write([]byte(`{"ok":true}`))
}

// HandleMe returns the session user as JSON, or 401.
func (p *Provider) HandleMe(w http.ResponseWriter, r *http.Request) {
	u := p.CurrentUser(r)
	if u == nil {
		http.Error(w, `{"error":"unauthenticated"}`, http.StatusUnauthorized)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(u)
}

// CurrentUser returns the session user, or nil.
func (p *Provider) CurrentUser(r *http.Request) *User {
	u, err := p.sessions.Read(r)
	if err != nil {
		return nil
	}
	return u
}

// upsertUser inserts or updates the users row for these claims and returns it.
func (p *Provider) upsertUser(ctx context.Context, claims *TokenClaims) (*User, error) {
	groupsJSON, _ := json.Marshal(claims.Groups)
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := p.db.ExecContext(ctx, `
		INSERT INTO users (id, oidc_sub, email, display_name, groups, created_at, last_login_at)
		VALUES (?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(oidc_sub) DO UPDATE SET
			email = excluded.email,
			display_name = excluded.display_name,
			groups = excluded.groups,
			last_login_at = excluded.last_login_at
	`, db.NewID(), claims.Sub, claims.Email, claims.Name, string(groupsJSON), now, now)
	if err != nil {
		return nil, err
	}
	var u User
	var groups string
	var premium int
	err = p.db.QueryRowContext(ctx,
		"SELECT id, email, display_name, groups, premium FROM users WHERE oidc_sub = ?", claims.Sub,
	).Scan(&u.ID, &u.Email, &u.DisplayName, &groups, &premium)
	if err != nil {
		return nil, err
	}
	u.OIDCSub = claims.Sub
	u.Premium = premium != 0
	_ = json.Unmarshal([]byte(groups), &u.Groups)
	return &u, nil
}
