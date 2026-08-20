//! score: recompute entity scores. Synchronous twin of go-service/cmd/score.
//! Usage: score --all  |  score owner/name
//!
//! Go parity notes (cmd/score/main.go:12-67):
//! - Per-entity errors are LOGGED and the loop CONTINUES (R-2 review
//!   follow-up 2; rescore_all in the chassis is intentionally fail-fast, so
//!   the log-and-continue loop lives here, exactly as in Go).
//! - No arguments (or an unknown owner/name) is fatal, exit 1 (Go log.Fatal).
//! - Scoring uses a nil ReadmeFetcher, so doc signals stay zeroed, as Go.

use anyhow::{Context, Result};
use chassis::{config::Config, db, scoring};
use rusqlite::OptionalExtension;

fn entity_ids(conn: &rusqlite::Connection, args: &[String]) -> Result<Vec<String>> {
    match args.first().map(String::as_str) {
        Some("--all") => {
            // Go ignores per-row Scan errors; rusqlite's typed collect
            // surfaces them — unreachable in practice (id is TEXT NOT NULL).
            let mut stmt = conn.prepare("SELECT id FROM entities")?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ids)
        }
        Some(full_name) => {
            let id: Option<String> = conn
                .query_row(
                    "SELECT id FROM entities WHERE full_name = ?1",
                    [full_name],
                    |row| row.get(0),
                )
                .optional()?;
            match id {
                Some(id) => Ok(vec![id]),
                // Go: log.Fatalf("Entity not found: %s", arg) → exit 1.
                None => anyhow::bail!("Entity not found: {full_name}"),
            }
        }
        // Go: log.Fatal("Usage: ...") → exit 1.
        None => anyhow::bail!("Usage: score --all  OR  score owner/name"),
    }
}

/// Recompute scores for the entity IDs resolved from `args`.
fn run(conn: &rusqlite::Connection, args: &[String]) -> Result<()> {
    let ids = entity_ids(conn, args)?;
    tracing::info!("Scoring {} entities...", ids.len());

    for id in &ids {
        // Go passes a nil fetcher: doc signals stay the zero value.
        let result = match scoring::score_entity(conn, id, None) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Error scoring {id}: {e:#}");
                continue;
            }
        };
        if let Err(e) = scoring::save_score(conn, &result) {
            tracing::error!("Error saving score for {id}: {e:#}");
            continue;
        }
        // Go re-reads full_name for the log line, ignoring the scan error.
        let full_name: String = conn
            .query_row(
                "SELECT full_name FROM entities WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default();
        tracing::info!(
            "  ✓ {full_name} | Score: {}/100 | Verdict: {} | Trajectory: {}",
            result.composite_score,
            result.verdict,
            result.trajectory
        );
    }

    tracing::info!("Scoring complete");
    Ok(())
}

#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let cfg = Config::load();
    let conn = db::open(&cfg.database_path).context("open db")?;
    db::migrate(&conn).context("migrate")?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&conn, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    fn insert_entity(conn: &rusqlite::Connection, full_name: &str) -> String {
        let id = chassis::db::new_id();
        conn.execute(
            "INSERT INTO entities (id, platform, slug, name, full_name, score_value, last_pushed_at, open_issues)
             VALUES (?1, 'default', ?2, ?3, ?4, 100, datetime('now'), 5)",
            rusqlite::params![&id, full_name, full_name, full_name],
        )
        .unwrap();
        id
    }

    #[test]
    fn entity_ids_all_empty() {
        let conn = open_temp_db();
        let ids = entity_ids(&conn, &["--all".to_string()]).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn entity_ids_by_full_name_found() {
        let conn = open_temp_db();
        insert_entity(&conn, "owner/repo");
        let ids = entity_ids(&conn, &["owner/repo".to_string()]).unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn entity_ids_by_full_name_missing() {
        let conn = open_temp_db();
        let err = entity_ids(&conn, &["owner/repo".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("Entity not found"));
    }

    #[test]
    fn entity_ids_no_args() {
        let conn = open_temp_db();
        let err = entity_ids(&conn, &[]).unwrap_err().to_string();
        assert!(err.contains("Usage: score --all"));
    }

    #[test]
    fn run_all_empty() {
        let conn = open_temp_db();
        run(&conn, &["--all".to_string()]).unwrap();
    }

    #[test]
    fn run_specific_entity() {
        let conn = open_temp_db();
        insert_entity(&conn, "owner/repo");
        run(&conn, &["owner/repo".to_string()]).unwrap();

        // Verify a score row was written.
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM entity_scores WHERE entity_id = (SELECT id FROM entities WHERE full_name = ?1)",
                ["owner/repo"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
