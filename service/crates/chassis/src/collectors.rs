//! Collector runtime: fetch (with retry) → single-transaction upsert → runlog.
//! Port of go-service/internal/collectors/{collector,run,retry,runlog,ratelimit}.go.
//!
//! Delta 8b: runlog finish errors are propagated (Go swallowed them), and a
//! rolled-back batch reports 0 upserted — the upsert is ONE transaction, so a
//! rollback unwrites everything and RunResult.upserted stays 0.

pub mod github_topic;

use anyhow::{anyhow, Context};
use rusqlite::params;
use std::time::{Duration, Instant};

/// One normalized entity, upserted into the entities table.
/// `slug` holds the owner, `name` the item name, `full_name` is "owner/name"
/// (template convention used by scoring and the API).
#[derive(Debug, Clone)]
pub struct CollectedEntity {
    pub platform: String,
    pub slug: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub score_value: i64,
    pub metadata: Option<String>,
    pub last_pushed_at: Option<String>,
    pub open_issues: i64,
}

/// A Collector pulls entities from one niche source. fetch does network+parse
/// only; it NEVER touches the database.
pub trait Collector {
    fn name(&self) -> &'static str;
    fn fetch(&self) -> anyhow::Result<Vec<CollectedEntity>>;
}

/// Summary of one completed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunResult {
    pub fetched: usize,
    pub upserted: usize,
}

/// Fetch attempts: 1 initial + 2 retries, backing off 1s then 2s
/// (port of retry.go: backoff = 1<<attempt seconds, capped at 30s).
const MAX_ATTEMPTS: u32 = 3;
const MAX_BACKOFF: Duration = Duration::from_secs(30);

fn with_retries<T>(mut f: impl FnMut() -> anyhow::Result<T>) -> anyhow::Result<T> {
    let mut attempt = 0u32;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_secs(1 << (attempt - 1)).min(MAX_BACKOFF));
            }
        }
    }
}

const UPSERT_SQL: &str = "
    INSERT INTO entities (id, platform, slug, name, full_name, description, category,
                          score_value, metadata, last_pushed_at, open_issues)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
    ON CONFLICT(platform, full_name) DO UPDATE SET
        name = excluded.name,
        full_name = excluded.full_name,
        description = excluded.description,
        category = excluded.category,
        metadata = excluded.metadata,
        score_value = excluded.score_value,
        last_pushed_at = excluded.last_pushed_at,
        open_issues = excluded.open_issues,
        scraped_at = datetime('now')
";

/// Upsert the whole fetch in ONE transaction; any error rolls back everything.
fn upsert_all(
    conn: &mut rusqlite::Connection,
    entities: &[CollectedEntity],
) -> anyhow::Result<usize> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(UPSERT_SQL)?;
        for e in entities {
            stmt.execute(params![
                crate::db::new_id(),
                e.platform,
                e.slug,
                e.name,
                e.full_name,
                e.description,
                e.category,
                e.score_value,
                e.metadata,
                e.last_pushed_at,
                e.open_issues,
            ])?;
        }
    }
    tx.commit()?;
    Ok(entities.len())
}

/// Insert the collector_runs finish row. Delta 8b: errors PROPAGATE.
fn finish_run(
    conn: &rusqlite::Connection,
    run_id: &str,
    collector: &str,
    started: &str,
    res: &RunResult,
    error: Option<&str>,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO collector_runs (id, collector, started_at, finished_at, fetched, upserted, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![run_id, collector, started, crate::db::now(), res.fetched as i64, res.upserted as i64, error],
    )?;
    Ok(())
}

/// Run one collection: fetch with retry → single-transaction upsert → runlog.
/// On fetch or upsert failure the run is logged with the error and 0 upserted,
/// and the error is returned. A runlog-write failure is itself returned (8b).
pub fn run_collector(
    conn: &mut rusqlite::Connection,
    c: &dyn Collector,
) -> anyhow::Result<RunResult> {
    let run_id = crate::db::new_id();
    let started = crate::db::now();
    let mut res = RunResult {
        fetched: 0,
        upserted: 0,
    };

    let entities = match with_retries(|| c.fetch()) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("collector {} fetch: {e:#}", c.name());
            finish_run(conn, &run_id, c.name(), &started, &res, Some(&msg))
                .context("write collector_runs finish row")?;
            return Err(anyhow!(msg));
        }
    };
    res.fetched = entities.len();

    match upsert_all(conn, &entities) {
        Ok(n) => res.upserted = n,
        Err(e) => {
            let msg = format!("collector {} upsert: {e:#}", c.name());
            finish_run(conn, &run_id, c.name(), &started, &res, Some(&msg))
                .context("write collector_runs finish row")?;
            return Err(anyhow!(msg));
        }
    }

    finish_run(conn, &run_id, c.name(), &started, &res, None)
        .context("write collector_runs finish row")?;
    tracing::info!(
        collector = c.name(),
        fetched = res.fetched,
        upserted = res.upserted,
        "collector run complete"
    );
    Ok(res)
}

/// Run every registered collector in order; the first failure aborts the run
/// (ingest exits non-zero, matching Go's log.Fatalf behavior).
pub fn run_all(
    conn: &mut rusqlite::Connection,
    collectors: Vec<Box<dyn Collector>>,
) -> anyhow::Result<()> {
    for c in &collectors {
        run_collector(conn, c.as_ref())?;
    }
    Ok(())
}

/// RateLimiter paces a collector's own outbound source requests to at most
/// `per_min` per minute; 0 disables pacing. The first wait never blocks.
/// (Port of ratelimit.go. run_collector does not use this — clone collectors
/// call it inside fetch to be polite to their source.)
pub struct RateLimiter {
    interval: Option<Duration>,
    last: Option<Instant>,
}

impl RateLimiter {
    pub fn new(per_min: u32) -> Self {
        let interval = (per_min > 0).then(|| Duration::from_secs(60) / per_min);
        Self {
            interval,
            last: None,
        }
    }

    pub fn wait(&mut self) {
        let Some(interval) = self.interval else {
            return;
        };
        if let Some(last) = self.last {
            let next = last + interval;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            }
        }
        self.last = Some(Instant::now());
    }
}
