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

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let cfg = Config::load();
    let conn = db::open(&cfg.database_path).context("open db")?;
    db::migrate(&conn).context("migrate")?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let ids = entity_ids(&conn, &args)?;
    tracing::info!("Scoring {} entities...", ids.len());

    for id in &ids {
        // Go passes a nil fetcher: doc signals stay the zero value.
        let result = match scoring::score_entity(&conn, id, None) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Error scoring {id}: {e:#}");
                continue;
            }
        };
        if let Err(e) = scoring::save_score(&conn, &result) {
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
