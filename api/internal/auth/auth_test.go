package auth

import (
	"context"
	"database/sql"
	"errors"
	"net/http"
	"net/http/httptest"
	"net/url"
	"path/filepath"
	"strings"
	"testing"
	"time"

	templatedb "risk.lucanian.app/api/internal/db"
	_ "modernc.org/sqlite"
)

func testProvider(t *testing.T) *Provider {
	t.Helper()
	conn, err := sql.Open("sqlite", filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	if err := templatedb.MigrateWith(conn); err != nil {
		t.Fatal(err)
	}
	p := &Provider{
		cfg:        OIDCConfig{IssuerURL: "https://auth.test/application/o/x", ClientID: "cid", ClientSecret: "sec", RedirectURL: "http://localhost:8080/auth/callback"},
		sessions:   NewSessionManager([]byte("0123456789abcdef0123456789abcdef"), false),
		db:         conn,
		authCodeURL: func(state string) string { return "https://auth.test/authorize?state=" + state },
		exchange: func(ctx context.Context, code string) (*TokenClaims, error) {
			if code != "good" {
				return nil, errTestExchange
			}
			return &TokenClaims{Sub: "sub-1", Email: "a@b.c", Name: "Alice", Groups: []string{"premium", "users"}}, nil
		},
	}
	return p
}

var errTestExchange = errors.New("bad code")

func TestLoginRedirectsToProvider(t *testing.T) {
	p := testProvider(t)
	rec := httptest.NewRecorder()
	p.HandleLogin(rec, httptest.NewRequest("GET", "/auth/login", nil))
	if rec.Code != http.StatusFound {
		t.Fatalf("status = %d, want 302", rec.Code)
	}
	if !strings.HasPrefix(rec.Header().Get("Location"), "https://auth.test/authorize?state=") {
		t.Fatalf("Location = %q", rec.Header().Get("Location"))
	}
	if !strings.Contains(rec.Header().Get("Set-Cookie"), "oidc_state=") {
		t.Fatal("state cookie must be set")
	}
}

func TestCallbackCreatesSessionAndUser(t *testing.T) {
	p := testProvider(t)
	// Drive login first to get a valid state cookie.
	loginRec := httptest.NewRecorder()
	p.HandleLogin(loginRec, httptest.NewRequest("GET", "/auth/login", nil))
	stateCookie := loginRec.Header().Get("Set-Cookie")
	state := strings.Split(strings.Split(stateCookie, "oidc_state=")[1], ";")[0]
	loc, _ := url.Parse(loginRec.Header().Get("Location"))
	wantState := loc.Query().Get("state")
	if state != wantState {
		t.Fatalf("state cookie %q != redirect state %q", state, wantState)
	}

	req := httptest.NewRequest("GET", "/auth/callback?code=good&state="+state, nil)
	req.Header.Set("Cookie", stateCookie)
	rec := httptest.NewRecorder()
	p.HandleCallback(rec, req)
	if rec.Code != http.StatusFound {
		t.Fatalf("status = %d, want 302; body: %s", rec.Code, rec.Body)
	}
	if !strings.Contains(rec.Header().Get("Set-Cookie"), "session=") {
		t.Fatal("session cookie must be set")
	}
	var n int
	if err := p.db.QueryRow("SELECT COUNT(*) FROM users WHERE oidc_sub = 'sub-1'").Scan(&n); err != nil || n != 1 {
		t.Fatalf("user row missing: n=%d err=%v", n, err)
	}
	var groups string
	_ = p.db.QueryRow("SELECT groups FROM users WHERE oidc_sub = 'sub-1'").Scan(&groups)
	if !strings.Contains(groups, "premium") {
		t.Fatalf("groups = %q, want premium group stored", groups)
	}
}

func TestCallbackRejectsBadState(t *testing.T) {
	p := testProvider(t)
	req := httptest.NewRequest("GET", "/auth/callback?code=good&state=forged", nil)
	rec := httptest.NewRecorder()
	p.HandleCallback(rec, req)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", rec.Code)
	}
}

func TestSessionRoundTripAndMe(t *testing.T) {
	p := testProvider(t)
	u := &User{ID: "u1", OIDCSub: "sub-1", Email: "a@b.c", DisplayName: "Alice", Groups: []string{"premium"}}
	rec := httptest.NewRecorder()
	p.sessions.Create(rec, u)
	cookie := rec.Header().Get("Set-Cookie")

	req := httptest.NewRequest("GET", "/auth/me", nil)
	req.Header.Set("Cookie", cookie)
	meRec := httptest.NewRecorder()
	p.HandleMe(meRec, req)
	if meRec.Code != http.StatusOK {
		t.Fatalf("/auth/me status = %d, want 200", meRec.Code)
	}
	if !strings.Contains(meRec.Body.String(), "a@b.c") {
		t.Fatalf("/auth/me body = %s", meRec.Body)
	}
}

func TestMeUnauthorizedWithoutSession(t *testing.T) {
	p := testProvider(t)
	rec := httptest.NewRecorder()
	p.HandleMe(rec, httptest.NewRequest("GET", "/auth/me", nil))
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("status = %d, want 401", rec.Code)
	}
}

func TestRequireAuthAndRequireGroup(t *testing.T) {
	p := testProvider(t)
	ok := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(200) })

	// No session → 401.
	rec := httptest.NewRecorder()
	p.RequireAuth(ok).ServeHTTP(rec, httptest.NewRequest("GET", "/x", nil))
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("RequireAuth without session = %d, want 401", rec.Code)
	}

	// Session without the group → 403; with the group → 200.
	mk := func(groups []string) *http.Request {
		u := &User{ID: "u1", Groups: groups}
		r := httptest.NewRecorder()
		p.sessions.Create(r, u)
		req := httptest.NewRequest("GET", "/x", nil)
		req.Header.Set("Cookie", r.Header().Get("Set-Cookie"))
		return req
	}
	rec2 := httptest.NewRecorder()
	p.RequireGroup("premium")(ok).ServeHTTP(rec2, mk([]string{"users"}))
	if rec2.Code != http.StatusForbidden {
		t.Fatalf("RequireGroup without group = %d, want 403", rec2.Code)
	}
	rec3 := httptest.NewRecorder()
	p.RequireGroup("premium")(ok).ServeHTTP(rec3, mk([]string{"premium"}))
	if rec3.Code != http.StatusOK {
		t.Fatalf("RequireGroup with group = %d, want 200", rec3.Code)
	}
}

func TestSessionExpiry(t *testing.T) {
	sm := NewSessionManager([]byte("0123456789abcdef0123456789abcdef"), false)
	sm.maxAge = -time.Hour // already expired
	rec := httptest.NewRecorder()
	sm.Create(rec, &User{ID: "u1"})
	req := httptest.NewRequest("GET", "/", nil)
	req.Header.Set("Cookie", rec.Header().Get("Set-Cookie"))
	if _, err := sm.Read(req); err == nil {
		t.Fatal("expired session must not validate")
	}
}
