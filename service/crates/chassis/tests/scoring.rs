use chassis::db;
use chassis::scoring::{self, TrajectorySignals};
use rusqlite::Connection;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap()
}

fn setup_db() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = db::open(&dir.path().join("t.db").to_string_lossy()).unwrap();
    db::migrate(&conn).unwrap();
    (dir, conn)
}

/// The hand-computed fixture: e1 with 3 releases (see plan text for the math).
fn seed_e1(conn: &Connection) {
    let now = OffsetDateTime::now_utc();
    let last_push = rfc3339(now - Duration::hours(241)); // 10 days ago, 1h margin
    conn.execute(
        "INSERT INTO entities (id, platform, slug, name, full_name, score_value, open_issues, last_pushed_at)
         VALUES ('e1', 'github', 'owner', 'repo', 'owner/repo', 5000, 100, ?1)",
        [last_push],
    )
    .unwrap();
    for (id, days_ago) in [("r1", 100i64), ("r2", 160), ("r3", 190)] {
        conn.execute(
            "INSERT INTO releases (id, entity_id, tag_name, published_at) VALUES (?1, 'e1', ?2, ?3)",
            rusqlite::params![id, format!("v{id}"), rfc3339(now - Duration::days(days_ago))],
        )
        .unwrap();
    }
}

#[test]
fn rescore_all_computes_expected_scores() {
    let (_dir, conn) = setup_db();
    seed_e1(&conn);

    let scored = scoring::rescore_all(&conn).unwrap();
    assert_eq!(scored, 1);

    let row = conn
        .query_row(
            "SELECT trajectory_score, doc_score, composite_score, verdict, trajectory,
                    release_velocity_days, calculation_version
             FROM entity_scores WHERE entity_id = 'e1'",
            [],
            |r| {
                Ok((
                    r.get::<_, f64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            85.0,
            0,
            48,
            "red".to_string(),
            "plateauing".to_string(),
            Some(45),
            1
        )
    );
}

#[test]
fn raw_signals_round_trips_as_json() {
    // Delta 11: Go did `signalsJSON, _ := json.Marshal(...)` and swallowed the
    // error; the Rust port propagates with `?`. TrajectorySignals is plain
    // numbers/options, so serde_json::to_string has no reachable failure path
    // — there is no way to force a serialization error in practice. Instead,
    // assert the stored raw_signals round-trips to the computed signals.
    let (_dir, conn) = setup_db();
    seed_e1(&conn);
    scoring::rescore_all(&conn).unwrap();

    let raw: String = conn
        .query_row(
            "SELECT raw_signals FROM entity_scores WHERE entity_id = 'e1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["release_velocity_days"], 45);
    assert_eq!(v["days_since_last_push"], 10);
    assert_eq!(v["open_issues_ratio"], 0.02);
}

#[test]
fn score_entity_errors_for_missing_entity() {
    let (_dir, conn) = setup_db();
    assert!(scoring::score_entity(&conn, "no-such-entity", None).is_err());
}

#[test]
fn trajectory_unknown_without_releases() {
    let (_dir, conn) = setup_db();
    let last_push = rfc3339(OffsetDateTime::now_utc() - Duration::hours(24));
    conn.execute(
        "INSERT INTO entities (id, platform, slug, name, full_name, score_value, open_issues, last_pushed_at)
         VALUES ('e2', 'github', 'o', 'r', 'o/r', 100, 0, ?1)",
        [last_push],
    )
    .unwrap();

    let r = scoring::score_entity(&conn, "e2", None).unwrap();
    assert_eq!(r.release_velocity_days, None);
    assert_eq!(r.trajectory, "unknown");
    // Trajectory: 50 + 15 (push 1 < 30) + 10 (ratio 0 < 0.05) = 75.
    // The verdict is computed from the COMPOSITE score (matching Go):
    // composite = round(0.45*75 + 0.35*0 + 0.20*1.0) = round(33.95) = 34,
    // which lands in the 30..50 band -> "red".
    assert_eq!(r.trajectory_score, 75.0);
    assert_eq!(r.composite_score, 34);
    assert_eq!(r.verdict, "red");
    assert_eq!(r.doc_verdict, "none");
}

#[test]
fn verdict_red_when_stale_despite_high_score() {
    let (_dir, conn) = setup_db();
    let last_push = rfc3339(OffsetDateTime::now_utc() - Duration::days(400));
    conn.execute(
        "INSERT INTO entities (id, platform, slug, name, full_name, score_value, open_issues, last_pushed_at)
         VALUES ('e3', 'github', 'o', 'r', 'o/r3', 100, 0, ?1)",
        [last_push],
    )
    .unwrap();

    let r = scoring::score_entity(&conn, "e3", None).unwrap();
    assert_eq!(r.verdict, "red"); // days_since_last_push = 400 > 365
}

#[test]
fn verdict_red_when_open_issues_ratio_high() {
    let (_dir, conn) = setup_db();
    let last_push = rfc3339(OffsetDateTime::now_utc() - Duration::hours(24));
    conn.execute(
        "INSERT INTO entities (id, platform, slug, name, full_name, score_value, open_issues, last_pushed_at)
         VALUES ('e4', 'github', 'o', 'r', 'o/r4', 100, 25, ?1)",
        [last_push],
    )
    .unwrap();

    let r = scoring::score_entity(&conn, "e4", None).unwrap();
    assert_eq!(r.trajectory_signals.open_issues_ratio, 0.25);
    assert_eq!(r.verdict, "red"); // ratio 0.25 > 0.2 fires before the score bands
}

#[test]
fn doc_score_full_readme() {
    let readme = format!(
        "# Project\n\nAn api client library with examples and samples.\n\n\
         ## Install\n\n```sh\ncargo add project\n```\n\n\
         ## Usage\n\n```rust\nlet x = 1;\n```\n\n\
         ## Contributing\n\nMIT licensed.\n\n\
         ## Changelog\n\n{}",
        "padding ".repeat(60)
    );
    assert!(readme.len() > 500);

    let s = scoring::analyze_readme(&readme);
    assert!(s.has_readme);
    assert!(s.has_install_section);
    assert!(s.has_usage_section);
    assert!(s.has_contributing_section);
    assert!(s.has_license_section);
    assert!(s.has_changelog_section);
    assert!(s.has_examples_dir);
    assert!(s.has_api_docs);
    assert_eq!(s.quickstart_estimated_min, 5);
    assert_eq!(s.code_blocks_count, 4);
    // 20+15+15+10+5+10+10+10+5+5+5 = 110, capped at 100.
    assert_eq!(scoring::doc_score(&s), 100);
    assert_eq!(scoring::doc_verdict(100), "excellent");
}

#[test]
fn doc_score_empty_readme() {
    let s = scoring::analyze_readme("");
    assert!(!s.has_readme);
    assert_eq!(s.quickstart_estimated_min, 0);
    assert_eq!(scoring::doc_score(&s), 0);
    assert_eq!(scoring::doc_verdict(0), "none");
}

#[test]
fn trajectory_score_formula_boundaries() {
    let hot = TrajectorySignals {
        release_velocity_days: Some(20),
        days_since_last_push: 5,
        open_issues_ratio: 0.01,
    };
    assert_eq!(scoring::trajectory_score(&hot), 95.0); // 50+20+15+10

    let cold = TrajectorySignals {
        release_velocity_days: Some(150),
        days_since_last_push: 100,
        open_issues_ratio: 0.2,
    };
    assert_eq!(scoring::trajectory_score(&cold), 0.0); // 50-15-20-15

    let neutral = TrajectorySignals {
        release_velocity_days: None,
        days_since_last_push: 45,
        open_issues_ratio: 0.10,
    };
    assert_eq!(scoring::trajectory_score(&neutral), 50.0); // no band fires
}
