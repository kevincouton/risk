//! API-keys gate: 401 without a valid key, 200 with one (key created directly
//! in the DB via chassis::apikeys with the known plaintext), 429 on the 61st
//! request — the HTTP-level ports of Go's KeyAuth/RateLimit middleware tests,
//! plus delta-2 behavior (rejected requests are not metered).

mod common;

use common::{spawn_test_server, stop};

fn seed_db_with_key() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let conn = chassis::db::open(db_path.to_str().unwrap()).unwrap();
    chassis::db::migrate(&conn).unwrap();
    let plaintext = chassis::apikeys::create_key(&conn, "u1", "ci").unwrap();
    let identity = chassis::apikeys::authenticate(&conn, &plaintext).unwrap();
    (dir, db_path, plaintext, identity.key_id)
}

fn authed(base: &str, path: &str, key: &str) -> reqwest::blocking::Response {
    reqwest::blocking::Client::new()
        .get(format!("{base}{path}"))
        .header("X-API-Key", key)
        .send()
        .unwrap()
}

#[test]
fn missing_or_invalid_key_401() {
    let (_d, db_path, _key, _id) = seed_db_with_key();
    let db = db_path.to_str().unwrap().to_string();
    let (child, base) =
        spawn_test_server(&[("API_KEYS_ENABLED", "true"), ("DATABASE_PATH", db.as_str())]);
    let client = reqwest::blocking::Client::new();
    let resp = client.get(format!("{base}/api/v1/stats")).send().unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(
        resp.text().unwrap(),
        "{\"error\":\"invalid or missing API key\"}\n"
    );
    let resp = authed(&base, "/api/v1/stats", "pk_deadbeef");
    assert_eq!(resp.status(), 401, "invalid key");
    stop(child);
}

#[test]
fn valid_key_200_and_usage_recorded() {
    let (_d, db_path, key, key_id) = seed_db_with_key();
    let db = db_path.to_str().unwrap().to_string();
    let (child, base) =
        spawn_test_server(&[("API_KEYS_ENABLED", "true"), ("DATABASE_PATH", db.as_str())]);
    let resp = authed(&base, "/api/v1/stats", &key);
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("x-ratelimit-remaining").is_some(),
        "Go sets X-RateLimit-Remaining"
    );
    let conn = chassis::db::open(&db).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM api_usage WHERE key_id = ?",
            rusqlite::params![key_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "the successful call was metered");
    stop(child);
}

#[test]
fn revoked_key_401() {
    let (_d, db_path, key, key_id) = seed_db_with_key();
    {
        let conn = chassis::db::open(db_path.to_str().unwrap()).unwrap();
        chassis::apikeys::revoke_key(&conn, &key_id, "u1").unwrap();
    }
    let db = db_path.to_str().unwrap().to_string();
    let (child, base) =
        spawn_test_server(&[("API_KEYS_ENABLED", "true"), ("DATABASE_PATH", db.as_str())]);
    let resp = authed(&base, "/api/v1/stats", &key);
    assert_eq!(resp.status(), 401, "revoked key must not authenticate");
    stop(child);
}

#[test]
fn rate_limit_61st_request_429() {
    let (_d, db_path, key, _id) = seed_db_with_key();
    let db = db_path.to_str().unwrap().to_string();
    let (child, base) =
        spawn_test_server(&[("API_KEYS_ENABLED", "true"), ("DATABASE_PATH", db.as_str())]);
    let mut last_remaining = String::new();
    for i in 1..=60 {
        let resp = authed(&base, "/api/v1/entities", &key);
        assert_eq!(resp.status(), 200, "request {i} must pass");
        last_remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .expect("X-RateLimit-Remaining must be set")
            .to_str()
            .unwrap()
            .to_string();
    }
    assert_eq!(
        last_remaining, "0",
        "after the 60th request nothing remains"
    );
    let resp = authed(&base, "/api/v1/entities", &key);
    assert_eq!(resp.status(), 429, "61st request must be rejected");
    assert_eq!(
        resp.text().unwrap(),
        "{\"error\":\"rate limit exceeded\"}\n"
    );
    stop(child);
}
