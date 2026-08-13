//! Port of go-service/internal/collectors/collectors_test.go (all 6 tests)
//! plus 2 new delta-8b tests (finish-error propagation, rollback accounting).

use std::cell::Cell;

use anyhow::anyhow;
use chassis::collectors::{run_collector, CollectedEntity, Collector, RateLimiter};
use rusqlite::Connection;
use tempfile::TempDir;

fn open_test_db() -> (TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.db");
    let conn = chassis::db::open(path.to_str().expect("utf8 path")).expect("open db");
    chassis::db::migrate(&conn).expect("migrate");
    (dir, conn)
}

fn entity(name: &str) -> CollectedEntity {
    CollectedEntity {
        platform: "test".into(),
        slug: "owner".into(),
        name: name.into(),
        full_name: format!("owner/{name}"),
        description: Some("fake".into()),
        category: Some("general".into()),
        score_value: 0,
        metadata: Some(format!("{{\"x\":\"{name}\"}}")),
        last_pushed_at: None,
        open_issues: 0,
    }
}

fn count_entities(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM entities WHERE platform = 'test'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// Port of Go fakeCollector: two entities, failing fetch on the first call when armed.
struct Fake {
    calls: Cell<u32>,
    fail_first: bool,
}

impl Collector for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn fetch(&self) -> anyhow::Result<Vec<CollectedEntity>> {
        self.calls.set(self.calls.get() + 1);
        if self.fail_first && self.calls.get() == 1 {
            return Err(anyhow!("transient source error"));
        }
        Ok(vec![entity("a"), entity("b")])
    }
}

/// Port of Go TestRunUpsertsAllEntities.
#[test]
fn run_upserts_all_entities() {
    let (_dir, mut conn) = open_test_db();
    let res = run_collector(
        &mut conn,
        &Fake {
            calls: Cell::new(0),
            fail_first: false,
        },
    )
    .expect("run");
    assert_eq!((res.fetched, res.upserted), (2, 2));
    assert_eq!(count_entities(&conn), 2);
    // Second run: idempotent upsert, still 2 rows.
    let res2 = run_collector(
        &mut conn,
        &Fake {
            calls: Cell::new(0),
            fail_first: false,
        },
    )
    .expect("rerun");
    assert_eq!(res2.upserted, 2);
    assert_eq!(count_entities(&conn), 2, "upsert must be idempotent");
}

/// Port of Go TestRunRetriesFailedFetchAndLogsRun.
#[test]
fn run_retries_failed_fetch_and_logs_run() {
    let (_dir, mut conn) = open_test_db();
    let fake = Fake {
        calls: Cell::new(0),
        fail_first: true,
    };
    let res = run_collector(&mut conn, &fake).expect("run");
    assert_eq!(fake.calls.get(), 2, "one retry after the transient failure");
    assert_eq!(res.upserted, 2);
    let (runs, err): (i64, Option<String>) = conn
        .query_row(
            "SELECT COUNT(*), error FROM collector_runs WHERE collector = 'fake'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(runs, 1, "exactly one collector_runs row per run");
    assert!(err.is_none(), "successful run logs NULL error");
}

/// Port of Go errCollector: fetch always fails.
struct AlwaysErr;

impl Collector for AlwaysErr {
    fn name(&self) -> &'static str {
        "err"
    }

    fn fetch(&self) -> anyhow::Result<Vec<CollectedEntity>> {
        Err(anyhow!("permanent failure"))
    }
}

/// Port of Go TestRunExhaustsRetriesAndRecordsError.
#[test]
fn run_exhausts_retries_and_records_error() {
    let (_dir, mut conn) = open_test_db();
    let err = run_collector(&mut conn, &AlwaysErr).expect_err("must fail after 3 attempts");
    assert!(format!("{err:#}").contains("permanent failure"));
    let err_text: String = conn
        .query_row(
            "SELECT error FROM collector_runs WHERE collector = 'err'",
            [],
            |r| r.get(0),
        )
        .expect("run must be logged even on failure");
    assert!(err_text.contains("permanent failure"));
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "no partial commit");
}

/// Two entities; a trigger aborts the second one's upsert to simulate a
/// mid-batch failure (Go used a failing Normalize; the Rust trait folds
/// normalization into fetch, so the failure is forced at the DB layer).
struct TwoEntities;

impl Collector for TwoEntities {
    fn name(&self) -> &'static str {
        "batchfail"
    }

    fn fetch(&self) -> anyhow::Result<Vec<CollectedEntity>> {
        Ok(vec![entity("ok"), entity("bad")])
    }
}

fn install_abort_trigger(conn: &Connection) {
    conn.execute_batch(
        "CREATE TRIGGER abort_bad BEFORE INSERT ON entities
         WHEN NEW.full_name = 'owner/bad'
         BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    )
    .unwrap();
}

/// Port of Go TestRunRollsBackFailedBatch.
#[test]
fn run_rolls_back_failed_batch() {
    let (_dir, mut conn) = open_test_db();
    install_abort_trigger(&conn);
    let err = run_collector(&mut conn, &TwoEntities).expect_err("upsert failure must propagate");
    assert!(format!("{err:#}").contains("boom"));
    assert_eq!(
        count_entities(&conn),
        0,
        "failed batch must not partially commit"
    );
}

/// NEW delta-8b: rolled-back batch accounting. Go counted upserts from
/// earlier committed batches; the Rust runtime upserts in ONE transaction,
/// so a rolled-back run reports 0 upserted in both the error and the runlog row.
#[test]
fn delta8b_rollback_accounting() {
    let (_dir, mut conn) = open_test_db();
    install_abort_trigger(&conn);
    run_collector(&mut conn, &TwoEntities).expect_err("upsert failure");
    let (fetched, upserted, err): (i64, i64, Option<String>) = conn
        .query_row(
            "SELECT fetched, upserted, error FROM collector_runs WHERE collector = 'batchfail'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (fetched, upserted),
        (2, 0),
        "rolled-back batch reports 0 upserted (delta 8b)"
    );
    assert!(err.expect("error recorded").contains("boom"));
}

/// NEW delta-8b: runlog finish errors PROPAGATE (Go's `_, _ = conn.Exec(...)`
/// swallowed them). Forced by dropping collector_runs so the finish insert fails.
#[test]
fn delta8b_finish_error_propagates() {
    let (_dir, mut conn) = open_test_db();
    conn.execute_batch("DROP TABLE collector_runs").unwrap();
    let err = run_collector(
        &mut conn,
        &Fake {
            calls: Cell::new(0),
            fail_first: false,
        },
    )
    .expect_err("runlog finish failure must propagate (delta 8b; Go swallowed it)");
    assert!(
        format!("{err:#}").contains("collector_runs"),
        "got: {err:#}"
    );
    assert_eq!(
        count_entities(&conn),
        2,
        "data committed; only the runlog write failed"
    );
}

/// Port of Go TestNewIDIsUnique (exercises chassis::db::new_id from R-2).
#[test]
fn new_id_is_unique() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1000 {
        assert!(seen.insert(chassis::db::new_id()), "duplicate id");
    }
}

/// Port of Go TestRateLimiterPaces.
#[test]
fn rate_limiter_paces() {
    let mut rl = RateLimiter::new(120); // 2 per second
    let start = std::time::Instant::now();
    rl.wait();
    rl.wait();
    rl.wait();
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(900),
        "3 permits at 120/min took {:?}, want >= ~1s of pacing",
        start.elapsed()
    );
}
