//! Ports of go-service/internal/apikeys/apikeys_test.go (4 tests) plus
//! delta-2 atomicity/fail-closed tests and delta-12 pruning tests.

use chassis::apikeys;
use chassis::db;

fn open_test_db() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let conn = db::open(path.to_str().unwrap()).unwrap();
    db::migrate(&conn).unwrap();
    (dir, conn)
}

// ---- Go port: TestCreateKeyStoresHashNotPlaintext ----
#[test]
fn create_key_stores_hash_not_plaintext() {
    let (_d, conn) = open_test_db();
    let key = apikeys::create_key(&conn, "u1", "ci").unwrap();
    // keys.go: 16 random bytes hex-encoded → "pk_" + 32 lowercase hex chars.
    assert!(key.starts_with("pk_"), "key format = {key}");
    assert_eq!(key.len(), 3 + 32, "key format = {key}");
    assert!(key[3..]
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    let hash: String = conn
        .query_row(
            "SELECT key_hash FROM api_keys WHERE user_id = 'u1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(hash, key, "plaintext must never be stored");
    assert_eq!(
        hash.len(),
        64,
        "stored value must be SHA-256 hex, got {hash}"
    );
}

// ---- Go port: TestKeyAuthValidInvalidRevoked (middleware-level parts move to
// server integration tests; here: authenticate + usage recording) ----
#[test]
fn authenticate_valid_invalid_revoked() {
    let (_d, conn) = open_test_db();
    let key = apikeys::create_key(&conn, "u1", "ci").unwrap();
    let id = apikeys::authenticate(&conn, &key).expect("valid key must authenticate");
    assert_eq!(id.user_id, "u1");
    assert!(
        apikeys::authenticate(&conn, "pk_deadbeef").is_none(),
        "invalid key"
    );
    assert!(apikeys::authenticate(&conn, "").is_none(), "missing key");
    apikeys::revoke_key(&conn, &id.key_id, "u1").unwrap();
    assert!(
        apikeys::authenticate(&conn, &key).is_none(),
        "revoked key must not authenticate"
    );
}

// ---- Go port: TestRateLimit61stRequest429 (chassis level; HTTP 429 asserted
// in the server integration tests) ----
#[test]
fn check_and_record_61st_call_fails() {
    let (_d, conn) = open_test_db();
    let key = apikeys::create_key(&conn, "u1", "ci").unwrap();
    let id = apikeys::authenticate(&conn, &key).unwrap();
    for i in 1..=60 {
        assert!(
            apikeys::check_and_record(&conn, &id.key_id, "/api/v1/entities", 60).unwrap(),
            "call {i} must pass"
        );
    }
    assert!(
        !apikeys::check_and_record(&conn, &id.key_id, "/api/v1/entities", 60).unwrap(),
        "61st call must be rejected"
    );
}

// ---- Go port: TestRateLimitWindowSlides ----
#[test]
fn window_slides() {
    let (_d, conn) = open_test_db();
    let key = apikeys::create_key(&conn, "u1", "ci").unwrap();
    let id = apikeys::authenticate(&conn, &key).unwrap();
    // Seed 60 usage rows 61 seconds in the past — outside the window (same SQL as the Go test).
    let mut stmt = conn
        .prepare("INSERT INTO api_usage (id, key_id, ts, endpoint) VALUES (?, ?, datetime('now', '-61 seconds'), '/api/v1/entities')")
        .unwrap();
    for i in 0..60 {
        stmt.execute(rusqlite::params![format!("old-{i}"), id.key_id])
            .unwrap();
    }
    drop(stmt);
    assert!(
        apikeys::check_and_record(&conn, &id.key_id, "/api/v1/entities", 60).unwrap(),
        "call after window slid must pass"
    );
}

// ---- New (delta 2): the 60th slot can be taken only once.
// Single-connection SQLite serializes writers, so sequential calls with an
// explicit `now` simulate the concurrent case deterministically: the first
// caller commits before the second's count runs inside the same transaction. ----
#[test]
fn atomicity_60th_slot_single_winner() {
    let (_d, conn) = open_test_db();
    let key = apikeys::create_key(&conn, "u1", "ci").unwrap();
    let id = apikeys::authenticate(&conn, &key).unwrap();
    let now = db::now();
    for _ in 0..59 {
        assert!(apikeys::check_and_record_at(&conn, &id.key_id, "/e", 60, &now).unwrap());
    }
    let first = apikeys::check_and_record_at(&conn, &id.key_id, "/e", 60, &now).unwrap();
    let second = apikeys::check_and_record_at(&conn, &id.key_id, "/e", 60, &now).unwrap();
    assert!(first, "60th call passes");
    assert!(!second, "a second caller cannot also take the 60th slot");
    let used: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM api_usage WHERE key_id = ?",
            rusqlite::params![id.key_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        used, 60,
        "rejected calls must NOT insert a usage row (delta 2)"
    );
}

// ---- New (delta 12): pruning deletes only rows older than the retention. ----
#[test]
fn prune_usage_deletes_only_old_rows() {
    let (_d, conn) = open_test_db();
    let key = apikeys::create_key(&conn, "u1", "ci").unwrap();
    let id = apikeys::authenticate(&conn, &key).unwrap();
    conn.execute(
        "INSERT INTO api_usage (id, key_id, ts, endpoint) VALUES ('old', ?, datetime('now', '-100 days'), '/e')",
        rusqlite::params![id.key_id],
    )
    .unwrap();
    assert!(
        apikeys::check_and_record(&conn, &id.key_id, "/e", 60).unwrap(),
        "recent row"
    );
    let pruned = apikeys::prune_usage(&conn, 90).unwrap();
    assert_eq!(pruned, 1, "only the 100-day-old row is pruned");
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM api_usage WHERE key_id = ?",
            rusqlite::params![id.key_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1, "the recent row survives");
}

// ---- New (delta 2): metering insert errors fail closed (Err propagates → 500
// at the HTTP layer), they are never swallowed. ----
#[test]
fn check_and_record_insert_error_fails_closed() {
    let (_d, conn) = open_test_db();
    // No api_keys row for "no-such-key": the usage INSERT violates the FK
    // (db::open sets PRAGMA foreign_keys=ON) and must surface as Err.
    let result = apikeys::check_and_record(&conn, "no-such-key", "/e", 60);
    assert!(
        result.is_err(),
        "insert failure must propagate, got {result:?}"
    );
}
