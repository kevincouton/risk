//! Scoring engine: port of go-service/internal/scoring/{engine,trajectory,docs}.go.
//!
//! Formulas, constants, and verdict/trajectory thresholds are ported exactly.
//! Delta 11: Go swallowed the json.Marshal error in SaveScore; the Rust port
//! propagates serialization errors via anyhow.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const TRAJECTORY_WEIGHT: f64 = 0.45;
pub const DOC_WEIGHT: f64 = 0.35;
pub const POPULARITY_WEIGHT: f64 = 0.20;
pub const CALCULATION_VERSION: i64 = 1;

/// Port of scoring.TrajectorySignals. JSON field names match the Go tags.
#[derive(Debug, Clone, Serialize)]
pub struct TrajectorySignals {
    pub release_velocity_days: Option<i64>,
    pub days_since_last_push: i64,
    pub open_issues_ratio: f64,
}

/// Port of scoring.DocSignals (zero value = Default).
#[derive(Debug, Clone, Default, Serialize)]
pub struct DocSignals {
    pub has_readme: bool,
    pub has_install_section: bool,
    pub has_usage_section: bool,
    pub has_contributing_section: bool,
    pub has_license_section: bool,
    pub has_changelog_section: bool,
    pub has_examples_dir: bool,
    pub has_api_docs: bool,
    pub quickstart_estimated_min: i64,
    pub code_blocks_count: i64,
    pub readme_length: i64,
}

/// Port of scoring.ScoreResult.
#[derive(Debug, Clone)]
pub struct ScoreResult {
    pub entity_id: String,
    pub trajectory_signals: TrajectorySignals,
    pub doc_signals: DocSignals,
    pub trajectory_score: f64,
    pub doc_score: i64,
    pub composite_score: i64,
    pub verdict: String,
    pub trajectory: String,
    pub doc_verdict: String,
    pub release_velocity_days: Option<i64>,
}

/// Port of platform.ReadmeFetcher (client.go): abstracts README retrieval
/// across sources. Implemented by platform::PlatformClient.
pub trait ReadmeFetcher {
    fn get_readme(&self, owner: &str, name: &str) -> Result<String>;
}

/// Port of CalculateTrajectorySignals.
pub fn calculate_trajectory_signals(
    conn: &Connection,
    entity_id: &str,
) -> Result<TrajectorySignals> {
    let (last_pushed, score_value, open_issues): (String, i64, i64) = conn
        .query_row(
            "SELECT last_pushed_at, score_value, open_issues FROM entities WHERE id = ?1",
            params![entity_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .with_context(|| format!("load entity {entity_id}"))?;

    // Go: parse error leaves daysSinceLastPush at the zero value.
    let days_since_last_push = match OffsetDateTime::parse(&last_pushed, &Rfc3339) {
        Ok(t) => (OffsetDateTime::now_utc() - t).whole_hours() / 24,
        Err(_) => 0,
    };

    let mut open_issues_ratio = 0.0;
    if score_value > 0 {
        open_issues_ratio = open_issues as f64 / score_value as f64;
    }

    let mut stmt = conn.prepare(
        "SELECT published_at FROM releases WHERE entity_id = ?1 ORDER BY published_at DESC LIMIT 5",
    )?;
    let dates: Vec<OffsetDateTime> = stmt
        .query_map(params![entity_id], |row| row.get::<_, String>(0))?
        .filter_map(|d| d.ok())
        .filter_map(|d| OffsetDateTime::parse(&d, &Rfc3339).ok())
        .collect();

    let release_velocity_days = if dates.len() >= 2 {
        let mut total_days: i64 = 0;
        for w in dates.windows(2) {
            // Dates are DESC: w[0] is the newer release (Go dates[i-1].Sub(dates[i])).
            total_days += (w[0] - w[1]).whole_hours() / 24;
        }
        Some(total_days / (dates.len() as i64 - 1))
    } else {
        None
    };

    Ok(TrajectorySignals {
        release_velocity_days,
        days_since_last_push,
        open_issues_ratio: (open_issues_ratio * 1000.0).round() / 1000.0,
    })
}

/// Port of TrajectoryScore (exact thresholds).
pub fn trajectory_score(signals: &TrajectorySignals) -> f64 {
    let mut score: f64 = 50.0;
    if let Some(velocity) = signals.release_velocity_days {
        if velocity < 30 {
            score += 20.0;
        } else if velocity < 60 {
            score += 10.0;
        } else if velocity > 120 {
            score -= 15.0;
        }
    }
    if signals.days_since_last_push < 30 {
        score += 15.0;
    } else if signals.days_since_last_push > 90 {
        score -= 20.0;
    }
    if signals.open_issues_ratio < 0.05 {
        score += 10.0;
    } else if signals.open_issues_ratio > 0.15 {
        score -= 15.0;
    }
    score.clamp(0.0, 100.0)
}

/// Port of AnalyzeReadme.
pub fn analyze_readme(readme: &str) -> DocSignals {
    let mut s = DocSignals::default();
    if readme.is_empty() {
        return s;
    }
    s.has_readme = true;
    let lower = readme.to_lowercase();
    s.has_install_section = lower.contains("## install") || lower.contains("## getting started");
    s.has_usage_section = lower.contains("## usage") || lower.contains("## how to use");
    s.has_contributing_section = lower.contains("## contributing");
    s.has_license_section = lower.contains("## license") || lower.contains("mit");
    s.has_changelog_section = lower.contains("## changelog") || lower.contains("## changes");
    s.has_examples_dir = lower.contains("examples") || lower.contains("samples");
    s.has_api_docs = lower.contains("api") || lower.contains("reference");
    s.readme_length = readme.len() as i64; // bytes, like Go len()
    s.code_blocks_count = readme.matches("```").count() as i64;
    s.quickstart_estimated_min = if s.has_install_section && s.has_usage_section {
        5
    } else if s.has_install_section || s.has_usage_section {
        10
    } else {
        30
    };
    s
}

/// Port of DocScore (exact weights and 100 cap).
pub fn doc_score(signals: &DocSignals) -> i64 {
    if !signals.has_readme {
        return 0;
    }
    let mut score = 20;
    if signals.has_install_section {
        score += 15;
    }
    if signals.has_usage_section {
        score += 15;
    }
    if signals.has_contributing_section {
        score += 10;
    }
    if signals.has_license_section {
        score += 5;
    }
    if signals.has_changelog_section {
        score += 10;
    }
    if signals.has_examples_dir {
        score += 10;
    }
    if signals.has_api_docs {
        score += 10;
    }
    if signals.quickstart_estimated_min <= 5 {
        score += 5;
    }
    if signals.code_blocks_count >= 3 {
        score += 5;
    }
    if signals.readme_length > 500 {
        score += 5;
    }
    if score > 100 {
        100
    } else {
        score
    }
}

/// Port of DocVerdict.
pub fn doc_verdict(score: i64) -> &'static str {
    if score >= 80 {
        "excellent"
    } else if score >= 60 {
        "adequate"
    } else if score >= 40 {
        "poor"
    } else {
        "none"
    }
}

/// Port of ScoreEntity. Unlike Go (global db.DB), the connection is a parameter.
pub fn score_entity(
    conn: &Connection,
    entity_id: &str,
    fetcher: Option<&dyn ReadmeFetcher>,
) -> Result<ScoreResult> {
    let ts = calculate_trajectory_signals(conn, entity_id)?;
    let traj_score = trajectory_score(&ts);

    // Go ignores the scan error: missing row leaves owner/name empty.
    let (owner, name): (String, String) = conn
        .query_row(
            "SELECT slug, name FROM entities WHERE id = ?1",
            params![entity_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or_default();

    // Go: docSignals stays the zero value unless the fetch succeeds.
    let mut doc_signals = DocSignals::default();
    if let Some(f) = fetcher {
        if !owner.is_empty() && !name.is_empty() {
            if let Ok(readme) = f.get_readme(&owner, &name) {
                doc_signals = analyze_readme(&readme);
            }
        }
    }
    let doc_score = doc_score(&doc_signals);

    let score_value: i64 = conn
        .query_row(
            "SELECT score_value FROM entities WHERE id = ?1",
            params![entity_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let popularity_score = (score_value as f64 / 1000.0 * 10.0).min(100.0);

    let composite = TRAJECTORY_WEIGHT * traj_score
        + DOC_WEIGHT * doc_score as f64
        + POPULARITY_WEIGHT * popularity_score;
    let composite_score = composite.round() as i64;

    Ok(ScoreResult {
        entity_id: entity_id.to_string(),
        trajectory_signals: ts.clone(),
        trajectory_score: traj_score,
        doc_score,
        composite_score,
        verdict: assign_verdict(composite_score, &ts).to_string(),
        trajectory: assign_trajectory(&ts).to_string(),
        doc_verdict: doc_verdict(doc_score).to_string(),
        doc_signals,
        release_velocity_days: ts.release_velocity_days,
    })
}

/// Port of assignVerdict (exact ordering: stale/issues checks before score bands).
fn assign_verdict(score: i64, signals: &TrajectorySignals) -> &'static str {
    if signals.days_since_last_push > 365 {
        return "red";
    }
    if signals.open_issues_ratio > 0.2 {
        return "red";
    }
    if score >= 70 {
        "green"
    } else if score >= 50 {
        "yellow"
    } else if score >= 30 {
        "red"
    } else {
        "critical"
    }
}

/// Port of assignTrajectory (exact thresholds).
fn assign_trajectory(signals: &TrajectorySignals) -> &'static str {
    let Some(velocity) = signals.release_velocity_days else {
        return "unknown";
    };
    if signals.days_since_last_push < 30 && velocity < 45 {
        return "accelerating";
    }
    if signals.days_since_last_push < 90 && velocity < 90 {
        return "plateauing";
    }
    if signals.days_since_last_push > 180 {
        return "declining";
    }
    "plateauing"
}

/// Port of SaveScore. Delta 11: the serialization error Go discarded with
/// `signalsJSON, _ := json.Marshal(...)` is propagated here.
///
/// Note: Go's INSERT leaves trajectory_score NULL; the brief's own
/// rescore_all test asserts the stored trajectory_score, so the Rust port
/// also persists it. The R-1 goldens never expose this column over HTTP, so
/// the extra write cannot break contract parity.
pub fn save_score(conn: &Connection, result: &ScoreResult) -> Result<()> {
    let signals_json = serde_json::to_string(&result.trajectory_signals)
        .context("serialize trajectory signals")?;
    conn.execute(
        "INSERT INTO entity_scores (
            id, entity_id, release_velocity_days, trajectory_score, doc_score,
            composite_score, verdict, trajectory, calculation_version, raw_signals
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            crate::db::new_id(),
            result.entity_id,
            result.release_velocity_days,
            result.trajectory_score,
            result.doc_score,
            result.composite_score,
            result.verdict,
            result.trajectory,
            CALCULATION_VERSION,
            signals_json
        ],
    )?;
    Ok(())
}

/// Rescore every entity, inserting one entity_scores row per entity with
/// `calculation_version` = 1 (matching Go). Returns the number of rows
/// inserted. Errors propagate fail-fast (the Go cmd/score binary logged and
/// continued per entity; that loop lives in the score binary, task R-4).
pub fn rescore_all(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT id FROM entities")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut scored = 0usize;
    for id in ids {
        let result = score_entity(conn, &id, None)?;
        save_score(conn, &result)?;
        scored += 1;
    }
    Ok(scored)
}
