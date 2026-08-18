//! `github` collector: seeds risk's entity table with high-signal packages
//! from the three dependency ecosystems in `seeds::SEED_SEARCHES`
//! (npm / pypi / cargo), discovered via GitHub repository search.
//! Supplies stars (score_value), pushed_at and open_issues; the `depsdev`
//! collector overlays advisory/freshness metadata onto the same rows.

use std::collections::HashSet;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chassis::collectors::{CollectedEntity, Collector, RateLimiter};
use chassis::platform::PlatformClient;
use serde_json::Value;

use crate::seeds::SEED_SEARCHES;

/// GitHub search API, authenticated: 30 requests/minute.
const SEARCH_PER_MIN: u32 = 30;

pub struct GithubCollector {
    client: PlatformClient,
    limiter: Mutex<RateLimiter>,
}

impl GithubCollector {
    /// Production constructor: real GitHub API, token from GITHUB_TOKEN
    /// (unset → unauthenticated, lower rate limits).
    pub fn from_env() -> Self {
        Self::with_client(PlatformClient::new(std::env::var("GITHUB_TOKEN").ok()))
    }

    /// Test/explicit constructor (httptest entry point).
    pub fn with_client(client: PlatformClient) -> Self {
        Self {
            client,
            limiter: Mutex::new(RateLimiter::new(SEARCH_PER_MIN)),
        }
    }
}

impl Collector for GithubCollector {
    fn name(&self) -> &'static str {
        "github"
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn fetch(&self) -> Result<Vec<CollectedEntity>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for seed in SEED_SEARCHES {
            for item in search(&self.client, &self.limiter, seed.query, seed.per_page)? {
                let Some(entity) = map_repo(&item, seed.ecosystem) else {
                    continue;
                };
                // Dedupe across ecosystems: first search wins as category.
                if seen.insert(entity.full_name.clone()) {
                    out.push(entity);
                }
            }
        }
        Ok(out)
    }
}

/// One GitHub repository search:
/// GET /search/repositories?q=<query>&sort=stars&order=desc&per_page=<n>.
/// Shared with the depsdev collector so both iterate the identical seed set.
pub(crate) fn search(
    client: &PlatformClient,
    limiter: &Mutex<RateLimiter>,
    query: &str,
    per_page: u32,
) -> Result<Vec<Value>> {
    limiter.lock().expect("rate limiter poisoned").wait();
    let path = format!(
        "/search/repositories?q={}&sort=stars&order=desc&per_page={per_page}",
        urlencode(query)
    );
    let doc = client
        .get_json(&path)
        .with_context(|| format!("github search {query:?}"))?;
    Ok(doc
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Map one /search/repositories item to a CollectedEntity. Returns None when
/// full_name or owner.login is missing (defensive: GitHub always sends them).
pub(crate) fn map_repo(item: &Value, ecosystem: &str) -> Option<CollectedEntity> {
    let full_name = item.get("full_name")?.as_str()?.to_string();
    let owner = item.pointer("/owner/login")?.as_str()?.to_string();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| full_name.rsplit('/').next())
        .unwrap_or(&full_name)
        .to_string();
    let metadata = serde_json::json!({
        "ecosystem": ecosystem,
        // Package-name heuristic: the deps.dev lookup key is the repo name
        // (right of "/"); repos whose registry name differs simply 404 in
        // deps.dev and are recorded as depsdev_found=false.
        "package": name,
        "language": item.get("language").cloned().unwrap_or(Value::Null),
        "forks_count": item.get("forks_count").and_then(Value::as_i64).unwrap_or(0),
        "license": item.pointer("/license/spdx_id").cloned().unwrap_or(Value::Null),
        "topics": item.get("topics").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
    });
    Some(CollectedEntity {
        platform: "github".to_string(),
        slug: owner,
        name,
        full_name,
        description: item
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        category: Some(ecosystem.to_string()),
        score_value: item
            .get("stargazers_count")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        metadata: Some(metadata.to_string()),
        last_pushed_at: item
            .get("pushed_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        open_issues: item
            .get("open_issues_count")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass through).
/// Only the fixed SEED_SEARCHES queries and deps.dev package/version path
/// segments pass through here.
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use httptest::{all_of, matchers::*, responders::*, Expectation, Server};
    use serde_json::{json, Value};

    use super::*;
    use chassis::platform::PlatformClient;

    fn repo(full_name: &str, stars: i64) -> Value {
        let (owner, name) = full_name.split_once('/').unwrap();
        json!({
            "full_name": full_name,
            "name": name,
            "owner": {"login": owner},
            "description": "a package",
            "stargazers_count": stars,
            "open_issues_count": 7,
            "pushed_at": "2026-01-02T03:04:05Z",
            "language": "JavaScript",
            "license": {"spdx_id": "MIT"},
            "forks_count": 12,
            "topics": ["npm-package"]
        })
    }

    fn search_doc(items: Vec<Value>) -> Value {
        json!({"total_count": items.len(), "incomplete_results": false, "items": items})
    }

    /// Expect one search request per seed query (SEED_SEARCHES order). The
    /// collector percent-encodes `:` in the `q` parameter, so the raw query
    /// string carries `topic%3A...`.
    fn expect_searches(server: &Server, npm: Value, pypi: Value, cargo: Value) {
        for (query, per_page, doc) in [
            ("topic%3Anpm-package", 34, npm),
            ("topic%3Apypi", 33, pypi),
            ("topic%3Acrates-io", 34, cargo),
        ] {
            server.expect(
                Expectation::matching(all_of![
                    request::method("GET"),
                    request::path("/search/repositories"),
                    request::query(format!(
                        "q={query}&sort=stars&order=desc&per_page={per_page}"
                    )),
                ])
                .times(1)
                .respond_with(json_encoded(doc)),
            );
        }
    }

    fn collector(server: &Server) -> GithubCollector {
        GithubCollector::with_client(PlatformClient::with_base_url(
            &server.url("/").to_string(),
            None,
        ))
    }

    #[test]
    fn fetch_maps_and_dedupes_across_ecosystems() {
        let server = Server::run();
        expect_searches(
            &server,
            search_doc(vec![
                repo("substack/left-pad", 4200),
                repo("substack/minimist", 6200),
            ]),
            search_doc(vec![repo("substack/minimist", 6200)]), // cross-ecosystem duplicate
            search_doc(vec![]),
        );
        let entities = collector(&server).fetch().expect("fetch");
        assert_eq!(entities.len(), 2);
        let e = &entities[0];
        assert_eq!(e.platform, "github");
        assert_eq!(e.slug, "substack");
        assert_eq!(e.name, "left-pad");
        assert_eq!(e.full_name, "substack/left-pad");
        assert_eq!(e.category.as_deref(), Some("npm"));
        assert_eq!(e.score_value, 4200);
        assert_eq!(e.open_issues, 7);
        assert_eq!(e.last_pushed_at.as_deref(), Some("2026-01-02T03:04:05Z"));
        let meta: Value = serde_json::from_str(e.metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["ecosystem"], "npm");
        assert_eq!(meta["package"], "left-pad");
        assert_eq!(meta["license"], "MIT");
        assert_eq!(meta["forks_count"], 12);
        // The pypi duplicate was dropped: minimist kept its npm category.
        assert_eq!(entities[1].full_name, "substack/minimist");
        assert_eq!(entities[1].category.as_deref(), Some("npm"));
    }

    #[test]
    fn fetch_empty_pages_yields_empty_vec() {
        let server = Server::run();
        expect_searches(
            &server,
            search_doc(vec![]),
            search_doc(vec![]),
            search_doc(vec![]),
        );
        let entities = collector(&server).fetch().expect("fetch");
        assert!(entities.is_empty());
    }

    #[test]
    fn urlencode_encodes_colon() {
        assert_eq!(urlencode("topic:npm-package"), "topic%3Anpm-package");
    }
}
