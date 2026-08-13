//! Ports of go-service/internal/auth/auth_test.go (7 tests) plus delta 7
//! (premium read from DB at request time). The delta-3 logout test (GET → 405)
//! is an HTTP-behavior test and lives in crates/server/tests/auth_logout.rs.

use std::future::Future;
use std::pin::Pin;

use chassis::auth::{self, AuthProvider, OidcFlow, TokenClaims, User};
use chassis::db::{self, SharedDb};

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef"; // same as the Go tests

struct TestFlow;

impl OidcFlow for TestFlow {
    fn authorize(&self) -> (String, String, String) {
        // Go test seam: "https://auth.test/authorize?state=" + state
        (
            "https://auth.test/authorize?state=teststate".into(),
            "teststate".into(),
            "testnonce".into(),
        )
    }
    fn exchange<'a>(
        &'a self,
        code: &'a str,
        _nonce: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<TokenClaims>> + Send + 'a>> {
        Box::pin(async move {
            if code != "good" {
                anyhow::bail!("bad code");
            }
            Ok(TokenClaims {
                sub: "sub-1".into(),
                email: Some("a@b.c".into()),
                name: Some("Alice".into()),
                groups: vec!["premium".into(), "users".into()],
            })
        })
    }
}

fn test_provider() -> (tempfile::TempDir, SharedDb, AuthProvider) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    {
        let conn = db::open(path.to_str().unwrap()).unwrap();
        db::migrate(&conn).unwrap();
    }
    let shared = db::open_shared(path.to_str().unwrap()).unwrap();
    let provider = AuthProvider::new_for_test(Box::new(TestFlow), KEY, shared.clone());
    (dir, shared, provider)
}

/// Extract the cookie VALUE from a `name=value; Path=/; ...` header string.
fn cookie_value(set_cookie: &str) -> &str {
    set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
}

// ---- Go port: TestLoginRedirectsToProvider ----
#[test]
fn login_redirects_to_provider() {
    let (_d, _db, p) = test_provider();
    let start = p.login();
    assert!(
        start
            .auth_url
            .starts_with("https://auth.test/authorize?state="),
        "auth_url = {}",
        start.auth_url
    );
    assert!(
        start.state_cookie.starts_with("oidc_state="),
        "state cookie must be set: {}",
        start.state_cookie
    );
    // state in the URL equals the cookie value (Go: state alone; no nonce rides
    // along — the split is a no-op kept for shape compatibility).
    let state_in_url = start.auth_url.rsplit("state=").next().unwrap();
    let state_in_cookie = cookie_value(&start.state_cookie).split('.').next().unwrap();
    assert_eq!(state_in_url, state_in_cookie);
    // Go cookie flags, quoted: Path=/; Max-Age=300; HttpOnly; Secure; SameSite=Lax
    assert!(
        start.state_cookie.contains("Path=/"),
        "{}",
        start.state_cookie
    );
    assert!(
        start.state_cookie.contains("Max-Age=300"),
        "{}",
        start.state_cookie
    );
    assert!(
        start.state_cookie.contains("HttpOnly"),
        "{}",
        start.state_cookie
    );
    assert!(
        start.state_cookie.contains("Secure"),
        "{}",
        start.state_cookie
    );
    assert!(
        start.state_cookie.contains("SameSite=Lax"),
        "{}",
        start.state_cookie
    );
}

// ---- Go port: TestCallbackCreatesSessionAndUser ----
#[tokio::test]
async fn callback_creates_session_and_user() {
    let (_d, db, p) = test_provider();
    let start = p.login();
    let ok = p
        .callback(cookie_value(&start.state_cookie), "teststate", "good")
        .await
        .expect("callback must succeed");
    assert!(
        ok.session_cookie.starts_with("session="),
        "session cookie must be set: {}",
        ok.session_cookie
    );
    assert!(
        ok.session_cookie.contains("Max-Age=604800"),
        "{}",
        ok.session_cookie
    );
    let conn = db.lock().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM users WHERE oidc_sub = 'sub-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "user row must exist");
    let groups: String = conn
        .query_row(
            "SELECT groups FROM users WHERE oidc_sub = 'sub-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(groups.contains("premium"), "groups = {groups}");
}

// ---- Go port: TestCallbackRejectsBadState ----
#[tokio::test]
async fn callback_rejects_bad_state() {
    let (_d, _db, p) = test_provider();
    let start = p.login();
    let err = p
        .callback(cookie_value(&start.state_cookie), "forged", "good")
        .await
        .unwrap_err();
    assert!(
        matches!(err, auth::CallbackError::InvalidState),
        "want InvalidState, got {err:?}"
    );
    // Missing cookie entirely is also InvalidState (Go: r.Cookie error → 400).
    let err = p.callback("", "teststate", "good").await.unwrap_err();
    assert!(matches!(err, auth::CallbackError::InvalidState));
}

/// Seed a users row directly (delta 7: current_user reads the DB, so the
/// session cookie alone is no longer sufficient).
fn seed_user(db: &SharedDb, id: &str, groups_json: &str, premium: i64) {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO users (id, oidc_sub, email, display_name, groups, premium, created_at)
         VALUES (?, ?, 'a@b.c', 'Alice', ?, ?, datetime('now'))",
        rusqlite::params![id, format!("sub-{id}"), groups_json, premium],
    )
    .unwrap();
}

fn user(id: &str) -> User {
    User {
        id: id.into(),
        oidc_sub: String::new(),
        email: Some("a@b.c".into()),
        display_name: Some("Alice".into()),
        groups: vec![],
        premium: false,
    }
}

// ---- Go port: TestSessionRoundTripAndMe (HandleMe JSON is asserted at the
// server layer; here: sign/verify + current_user) ----
#[test]
fn session_round_trip_and_current_user() {
    let (_d, db, p) = test_provider();
    seed_user(&db, "u1", "[\"premium\"]", 0);
    let cookie = auth::session_cookie_for(KEY, &user("u1"));
    let value = cookie_value(&cookie);
    let u = p.current_user(&db, value).expect("session must validate");
    assert_eq!(u.id, "u1");
    assert_eq!(u.email.as_deref(), Some("a@b.c"));
    assert_eq!(
        u.groups,
        vec!["premium".to_string()],
        "groups read from DB (delta 7)"
    );
}

// ---- Go port: TestMeUnauthorizedWithoutSession ----
#[test]
fn current_user_none_without_session() {
    let (_d, db, p) = test_provider();
    assert!(p.current_user(&db, "garbage").is_none());
    assert!(p.current_user(&db, "").is_none());
}

// ---- Go port: TestRequireAuthAndRequireGroup ----
#[test]
fn require_group_semantics() {
    let (_d, db, p) = test_provider();
    seed_user(&db, "u1", "[\"users\"]", 0);
    let cookie = auth::session_cookie_for(KEY, &user("u1"));
    let value = cookie_value(&cookie).to_string();
    assert!(
        p.require_group(&db, &value, "premium").is_none(),
        "user without the group → None (server maps to 403)"
    );
    // Grant the group in the DB; the SAME cookie must now pass (delta 7).
    db.lock()
        .unwrap()
        .execute(
            "UPDATE users SET groups = '[\"premium\"]' WHERE id = 'u1'",
            [],
        )
        .unwrap();
    assert!(
        p.require_group(&db, &value, "premium").is_some(),
        "user with the group → Some"
    );
}

// ---- Go port: TestSessionExpiry ----
#[test]
fn session_expiry() {
    let (_d, db, p) = test_provider();
    seed_user(&db, "u1", "[]", 0);
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() - 3600; // already expired (Go: maxAge = -1h)
    let cookie = auth::session_cookie_with_exp(KEY, &user("u1"), exp);
    assert!(
        p.current_user(&db, cookie_value(&cookie)).is_none(),
        "expired session must not validate"
    );
}

// ---- New (delta 7): premium changed in the DB after login is visible on the
// next current_user call with the unchanged cookie. ----
#[test]
fn premium_refresh_from_db() {
    let (_d, db, p) = test_provider();
    seed_user(&db, "u1", "[]", 0);
    let cookie = auth::session_cookie_for(KEY, &user("u1")); // cookie premium snapshot: false
    let value = cookie_value(&cookie).to_string();
    assert!(!p.current_user(&db, &value).unwrap().premium);
    db.lock()
        .unwrap()
        .execute("UPDATE users SET premium = 1 WHERE id = 'u1'", [])
        .unwrap();
    assert!(
        p.current_user(&db, &value).unwrap().premium,
        "DB premium wins over the cookie snapshot (delta 7)"
    );
}
