use chassis::db;
use rusqlite::Connection;

fn temp_db() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db").to_string_lossy().to_string();
    let conn = db::open(&path).unwrap();
    (dir, conn)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .any(|name| name.unwrap() == column);
    exists
}

fn entities_ddl(conn: &Connection) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE name = 'entities'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn index_count(conn: &Connection, name: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn fresh_migrate_creates_v2_schema_without_redundant_index() {
    let (_dir, conn) = temp_db();
    db::migrate(&conn).unwrap();

    let ddl = entities_ddl(&conn);
    assert!(
        ddl.contains("UNIQUE(platform, full_name)"),
        "entities DDL: {ddl}"
    );
    assert!(column_exists(&conn, "entities", "last_pushed_at"));
    assert!(column_exists(&conn, "entities", "open_issues"));
    assert!(column_exists(
        &conn,
        "entity_scores",
        "release_velocity_days"
    ));
    // Delta 8a: a fresh v2 DB must NOT get the redundant unique index.
    assert_eq!(index_count(&conn, "ux_entities_platform_full_name"), 0);
    // open() pragmas per spine.
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
    let busy: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .unwrap();
    assert_eq!(busy, 5000);
}

#[test]
fn migrate_is_idempotent() {
    let (_dir, conn) = temp_db();
    db::migrate(&conn).unwrap();
    db::migrate(&conn).unwrap();
    let ddl = entities_ddl(&conn);
    assert!(
        ddl.contains("UNIQUE(platform, full_name)"),
        "entities DDL: {ddl}"
    );
    assert_eq!(index_count(&conn, "ux_entities_platform_full_name"), 0);
}

#[test]
fn pre_v2_db_is_rebuilt_preserving_data() {
    let (_dir, conn) = temp_db();
    // Synthetic pre-v2 database: old UNIQUE(platform, slug), missing the three
    // columns the guarded ALTERs add.
    conn.execute_batch(
        "CREATE TABLE entities (
            id TEXT PRIMARY KEY,
            platform TEXT NOT NULL DEFAULT 'default',
            slug TEXT NOT NULL,
            name TEXT NOT NULL,
            full_name TEXT NOT NULL,
            description TEXT,
            category TEXT,
            score_value INTEGER DEFAULT 0,
            metadata TEXT,
            scraped_at TEXT DEFAULT (datetime('now')),
            UNIQUE(platform, slug)
        );
        CREATE TABLE entity_scores (
            id TEXT PRIMARY KEY,
            entity_id TEXT NOT NULL,
            scored_at TEXT DEFAULT (datetime('now')),
            composite_score INTEGER,
            verdict TEXT,
            trajectory TEXT,
            calculation_version INTEGER DEFAULT 1,
            raw_signals TEXT
        );
        INSERT INTO entities (id, platform, slug, name, full_name, score_value) VALUES
            ('e1', 'github', 'one', 'one', 'owner/one', 100),
            ('e2', 'github', 'two', 'two', 'owner/two', 200);",
    )
    .unwrap();

    db::migrate(&conn).unwrap();

    // Delta 9: the table was rebuilt with the v2 uniqueness constraint.
    let ddl = entities_ddl(&conn);
    assert!(
        ddl.contains("UNIQUE(platform, full_name)"),
        "entities DDL: {ddl}"
    );
    assert!(
        !ddl.contains("UNIQUE(platform, slug)"),
        "entities DDL: {ddl}"
    );
    // The guarded ALTERs added the missing columns before the rebuild.
    assert!(column_exists(&conn, "entities", "last_pushed_at"));
    assert!(column_exists(&conn, "entities", "open_issues"));
    assert!(column_exists(
        &conn,
        "entity_scores",
        "release_velocity_days"
    ));
    // Data preserved.
    let rows: Vec<(String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT full_name, score_value FROM entities ORDER BY full_name")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            ("owner/one".to_string(), 100),
            ("owner/two".to_string(), 200),
        ]
    );
    // Rebuild path must not create the redundant index either (delta 8a).
    assert_eq!(index_count(&conn, "ux_entities_platform_full_name"), 0);
    // The non-unique indexes were recreated by the rebuild.
    assert_eq!(index_count(&conn, "idx_entities_full_name"), 1);
    assert_eq!(index_count(&conn, "idx_entities_category"), 1);
    assert_eq!(index_count(&conn, "idx_entities_score"), 1);
    // Rebuild is guarded: a second migrate is a no-op success.
    db::migrate(&conn).unwrap();
    let ddl = entities_ddl(&conn);
    assert!(
        ddl.contains("UNIQUE(platform, full_name)"),
        "entities DDL: {ddl}"
    );
}

#[test]
fn new_id_is_uuid_v4_and_unique() {
    // Port of go-service collectors_test.go::TestNewIDIsUnique, plus v4 shape.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1000 {
        let id = db::new_id();
        assert_eq!(id.len(), 36, "id: {id}");
        assert_eq!(id.as_bytes()[8], b'-');
        assert_eq!(id.as_bytes()[13], b'-');
        assert_eq!(id.as_bytes()[18], b'-');
        assert_eq!(id.as_bytes()[23], b'-');
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "id: {id}"
        );
        assert_eq!(id.as_bytes()[14], b'4', "version nibble, id: {id}");
        assert!(
            matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "variant bits, id: {id}"
        );
        assert!(seen.insert(id.clone()), "duplicate id {id}");
    }
}

#[test]
fn now_is_sqlite_datetime_format() {
    let ts = db::now();
    assert_eq!(ts.len(), 19, "ts: {ts}");
    assert_eq!(ts.as_bytes()[4], b'-');
    assert_eq!(ts.as_bytes()[7], b'-');
    assert_eq!(ts.as_bytes()[10], b' ');
    assert_eq!(ts.as_bytes()[13], b':');
    assert_eq!(ts.as_bytes()[16], b':');
    assert!(ts
        .chars()
        .all(|c| c.is_ascii_digit() || c == '-' || c == ' ' || c == ':'));
}

#[test]
fn open_shared_yields_shared_connection() {
    let (_dir, _conn) = temp_db();
    let dir2 = tempfile::tempdir().unwrap();
    let path = dir2.path().join("shared.db").to_string_lossy().to_string();
    let shared = db::open_shared(&path).unwrap();
    let conn = shared.lock().unwrap();
    db::migrate(&conn).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'entities'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}
