//! Config-driven GitHub topic-search collector (Wave 2, spec §3 W2-0).
//!
//! For each topic in `GITHUB_TOPICS` (comma-separated), fetches the first
//! page of `GET /search/repositories?q=topic:<t>&sort=stars&order=desc&per_page=100`
//! and maps repositories to entities. Deduped by `full_name` across topics
//! (first topic wins as `category`), capped at MAX_TOTAL entities per run.
//! Empty or unset `GITHUB_TOPICS` collects zero entities — never an error.

use std::cell::RefCell;
use std::collections::HashSet;

use serde_json::Value;

use crate::collectors::{CollectedEntity, Collector, RateLimiter};
use crate::platform::PlatformClient;

/// Per-site cap on entities collected in one run (spec §3 W2-0).
const MAX_TOTAL: usize = 100;

/// GitHub search rate budget: 30 req/min authenticated, 10 anonymous
/// (spec §6: search is the tightest GitHub budget).
const RATE_PER_MIN_AUTHED: u32 = 30;
const RATE_PER_MIN_ANON: u32 = 10;

pub struct GithubTopicCollector {
    client: PlatformClient,
    topics: Vec<String>,
    limiter: RefCell<RateLimiter>,
}

impl GithubTopicCollector {
    /// Build from the environment: `GITHUB_TOPICS` (comma-separated; empty or
    /// unset collects nothing) and `GITHUB_TOKEN` (optional Bearer token,
    /// per the spine's env/secrets rule).
    pub fn from_env() -> Self {
        let token = std::env::var("GITHUB_TOKEN").ok();
        let topics = parse_topics(std::env::var("GITHUB_TOPICS").ok());
        let per_min = if token.is_some() {
            RATE_PER_MIN_AUTHED
        } else {
            RATE_PER_MIN_ANON
        };
        Self {
            client: PlatformClient::new(token),
            topics,
            limiter: RefCell::new(RateLimiter::new(per_min)),
        }
    }

    /// Build against an explicit client (tests: `PlatformClient::with_base_url`).
    /// Rate limiting is disabled.
    pub fn with_client(client: PlatformClient, topics: Vec<String>) -> Self {
        Self {
            client,
            topics,
            limiter: RefCell::new(RateLimiter::new(0)),
        }
    }
}

impl Collector for GithubTopicCollector {
    fn name(&self) -> &'static str {
        "github_topic"
    }

    fn fetch(&self) -> anyhow::Result<Vec<CollectedEntity>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for topic in &self.topics {
            if out.len() >= MAX_TOTAL {
                break;
            }
            self.limiter.borrow_mut().wait();
            let resp = self.client.get_json(&format!(
                "/search/repositories?q=topic:{topic}&sort=stars&order=desc&per_page=100"
            ))?;
            let items = resp
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for repo in &items {
                if out.len() >= MAX_TOTAL {
                    break;
                }
                let Some(entity) = map_repo(repo, topic) else {
                    continue;
                };
                // First topic wins: skip repos already collected.
                if seen.insert(entity.full_name.clone()) {
                    out.push(entity);
                }
            }
        }
        Ok(out)
    }
}

/// Split a comma-separated topic list; trims whitespace, drops empty entries.
/// `None` (unset) yields an empty list.
fn parse_topics(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Map one GitHub search item to an entity; skips malformed entries
/// (missing or invalid `full_name`).
fn map_repo(repo: &Value, topic: &str) -> Option<CollectedEntity> {
    let full_name = repo.get("full_name")?.as_str()?;
    let (owner, name) = full_name.split_once('/')?;
    let metadata = serde_json::json!({
        "language": repo.get("language").cloned().unwrap_or(Value::Null),
        "forks_count": repo.get("forks_count").and_then(Value::as_i64).unwrap_or(0),
        "license": repo
            .get("license")
            .and_then(|l| l.get("spdx_id"))
            .cloned()
            .unwrap_or(Value::Null),
    });
    Some(CollectedEntity {
        platform: "github".to_string(),
        slug: owner.to_string(),
        name: name.to_string(),
        full_name: full_name.to_string(),
        description: repo
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        category: Some(topic.to_string()),
        score_value: repo
            .get("stargazers_count")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        metadata: Some(metadata.to_string()),
        last_pushed_at: repo
            .get("pushed_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        open_issues: repo
            .get("open_issues_count")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_topics;

    #[test]
    fn parse_topics_trims_and_drops_empty_entries() {
        let topics = parse_topics(Some(" rust , ,web-framework,".to_string()));
        assert_eq!(
            topics,
            vec!["rust".to_string(), "web-framework".to_string()]
        );
    }

    #[test]
    fn parse_topics_unset_or_empty_yields_no_topics() {
        assert!(parse_topics(None).is_empty());
        assert!(parse_topics(Some(String::new())).is_empty());
    }
}
