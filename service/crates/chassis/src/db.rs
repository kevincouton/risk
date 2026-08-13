//! SQLite connection, schema, and migrations.
//!
//! Port of go-service/internal/db/db.go onto rusqlite (bundled), with:
//! - delta 8a: the redundant ux_entities_platform_full_name index is NOT
//!   created on fresh v2 databases (nor after a rebuild);
//! - delta 9: pre-v2 databases are migrated by a guarded table rebuild moving
//!   uniqueness from (platform, slug) to (platform, full_name).

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

pub type SharedDb = std::sync::Arc<std::sync::Mutex<Connection>>;

const SCHEMA: &str = include_str!("schema.sql");

/// Open a database with the chassis pragmas (foreign_keys ON, busy_timeout
/// 5000ms — same value the Go server uses).
pub fn open(path: &str) -> Result<Connection> {
    let conn = Connection::open(path).context("open db")?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
    Ok(conn)
}

pub fn open_shared(path: &str) -> Result<SharedDb> {
    Ok(std::sync::Arc::new(std::sync::Mutex::new(open(path)?)))
}

/// Apply schema.sql (idempotent CREATE TABLE IF NOT EXISTS), then the guarded
/// column ALTERs for pre-v2 databases, then the delta-9 rebuild / delta-8a
/// index guard. Safe to run repeatedly.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("apply schema")?;

    // Guarded ALTERs for pre-v2 databases (SQLite has no ADD COLUMN IF NOT EXISTS).
    for (table, column, ddl) in [
        (
            "entities",
            "last_pushed_at",
            "ALTER TABLE entities ADD COLUMN last_pushed_at TEXT",
        ),
        (
            "entities",
            "open_issues",
            "ALTER TABLE entities ADD COLUMN open_issues INTEGER DEFAULT 0",
        ),
        (
            "entity_scores",
            "release_velocity_days",
            "ALTER TABLE entity_scores ADD COLUMN release_velocity_days INTEGER",
        ),
    ] {
        if !column_exists(conn, table, column)? {
            conn.execute(ddl, [])
                .with_context(|| format!("add column {table}.{column}"))?;
        }
    }

    let rebuilt = rebuild_entities_if_needed(conn)?;
    ensure_full_name_unique(conn, rebuilt)?;
    Ok(())
}

/// Delta 9 (exact spine algorithm): if the entities table still carries the
/// pre-v2 UNIQUE(platform, slug) table-level constraint, rebuild it with the
/// v2 DDL inside one transaction and recreate the three non-unique indexes.
/// Returns true when a rebuild happened.
fn rebuild_entities_if_needed(conn: &Connection) -> Result<bool> {
    let sql = entities_sql(conn)?;
    if !(sql.contains("UNIQUE(platform, slug)") || sql.contains("UNIQUE(platform,slug)")) {
        return Ok(false);
    }
    let tx = conn
        .unchecked_transaction()
        .context("begin entities rebuild")?;
    tx.execute_batch(
        "CREATE TABLE entities_new (
            id TEXT PRIMARY KEY,
            platform TEXT NOT NULL DEFAULT 'default',
            slug TEXT NOT NULL,
            name TEXT NOT NULL,
            full_name TEXT NOT NULL,
            description TEXT,
            category TEXT,
            score_value INTEGER DEFAULT 0,
            metadata TEXT,
            last_pushed_at TEXT,
            open_issues INTEGER DEFAULT 0,
            scraped_at TEXT DEFAULT (datetime('now')),
            UNIQUE(platform, full_name)
        );
        INSERT INTO entities_new
            SELECT id, platform, slug, name, full_name, description, category,
                   score_value, metadata, last_pushed_at, open_issues, scraped_at
            FROM entities;
        DROP TABLE entities;
        ALTER TABLE entities_new RENAME TO entities;
        CREATE INDEX idx_entities_full_name ON entities(full_name);
        CREATE INDEX idx_entities_category ON entities(category);
        CREATE INDEX idx_entities_score ON entities(score_value DESC);",
    )
    .context("rebuild entities table")?;
    tx.commit().context("commit entities rebuild")?;
    Ok(true)
}

/// Delta 8a: only create ux_entities_platform_full_name when the table was
/// NOT rebuilt and lacks the table-level constraint. Fresh v2 databases (and
/// rebuilt ones) have UNIQUE(platform, full_name) in the DDL and must not get
/// the redundant index.
fn ensure_full_name_unique(conn: &Connection, rebuilt: bool) -> Result<()> {
    if rebuilt {
        return Ok(());
    }
    let sql = entities_sql(conn)?;
    if sql.contains("UNIQUE(platform, full_name)") || sql.contains("UNIQUE(platform,full_name)") {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS ux_entities_platform_full_name ON entities(platform, full_name)",
    )
    .context("ensure entities unique index")?;
    Ok(())
}

fn entities_sql(conn: &Connection) -> Result<String> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'entities'",
            [],
            |r| r.get(0),
        )
        .optional()
        .context("read entities DDL")?;
    Ok(sql.unwrap_or_default())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    // PRAGMA does not accept bound parameters; table names here are constants.
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("table_info {table}"))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("table_info {table}"))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Random UUID v4 (port of Go NewID, crypto/rand → rand crate).
pub fn new_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Current UTC time in SQLite datetime('now') format: YYYY-MM-DD HH:MM:SS.
pub fn now() -> String {
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    time::OffsetDateTime::now_utc()
        .format(&format)
        .expect("static format description is valid")
}
