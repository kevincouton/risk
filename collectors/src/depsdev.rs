//! `depsdev` collector: re-runs the seed GitHub searches and overlays
//! deps.dev v3 data (default-version freshness, advisory counts) onto the
//! SAME entities the `github` collector seeds.
//!
//! Merge mechanism (pinned): fetch() never touches the DB (chassis contract),
//! so instead of reading existing rows this collector re-derives every field
//! from the fresh search response and emits COMPLETE CollectedEntity values
//! with platform "github" and the same full_name. The chassis upsert
//! (ON CONFLICT(platform, full_name) DO UPDATE) then merges by overwrite:
//! score_value/last_pushed_at/open_issues come from the same search payload,
//! and metadata gains the depsdev_* keys. Run order matters: the ingest unit
//! runs `github` first, `depsdev` second.

use std::collections::HashSet;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use chassis::collectors::{CollectedEntity, Collector, RateLimiter};
use chassis::platform::PlatformClient;
use serde_json::Value;

use crate::seeds::SEED_SEARCHES;

/// GitHub search API, authenticated: 30 requests/minute.
const SEARCH_PER_MIN: u32 = 30;
/// deps.dev has no documented tight quota; stay polite (≤ 1 req/s).
const DEPSDEV_PER_MIN: u32 = 60;
const DEFAULT_BASE_URL: &str = "https://api.deps.dev";

/// Minimal synchronous client for the deps.dev v3 REST API (no auth needed).
/// Verified live 2026-08-14:
/// - GET /v3/systems (list) 404s — do not use.
/// - GET /v3/systems/{SYSTEM}/packages/{name} — SYSTEM is uppercase
///   NPM | PYPI | CARGO; response:
///   {"packageKey":{...}, "versions":[{"versionKey":{"version":"1.0.0"},
///   "publishedAt":"2016-03-22T21:42:18Z", "isDefault":false, ...}]}
/// - GET /v3/systems/{SYSTEM}/packages/{name}/versions/{version} —
///   response carries "advisoryKeys":[{"id":"GHSA-..."}]; advisory count =
///   advisoryKeys length (verified: npm minimist 1.2.5 → 1).
///
/// Unknown packages/versions return HTTP 404 → mapped to Ok(None).
pub struct DepsDevClient {
    http: reqwest::blocking::Client,
    base_url: String,
}

/// The default (latest) version of a package and when it was published.
pub struct PackageInfo {
    pub default_version: Option<String>,
    pub default_published_at: Option<String>,
}

impl DepsDevClient {
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Test/explicit constructor (httptest entry point).
    pub fn with_base_url(base_url: &str) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent("risk-collectors depsdev")
            .build()
            .expect("reqwest blocking client with rustls cannot fail");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// GET one JSON document; 404 → Ok(None), other non-2xx → Err.
    fn get(&self, path: &str) -> Result<Option<Value>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(anyhow!("GET {url} returned status {status}"));
        }
        Ok(Some(
            resp.json::<Value>()
                .with_context(|| format!("parse JSON from {url}"))?,
        ))
    }

    /// GET /v3/systems/{system}/packages/{name}: default version + publishedAt.
    pub fn package(&self, system: &str, name: &str) -> Result<Option<PackageInfo>> {
        let path = format!(
            "/v3/systems/{system}/packages/{}",
            crate::github::urlencode(name)
        );
        let Some(doc) = self.get(&path)? else {
            return Ok(None);
        };
        let default = doc
            .get("versions")
            .and_then(Value::as_array)
            .and_then(|vs| {
                vs.iter()
                    .find(|v| v.get("isDefault").and_then(Value::as_bool).unwrap_or(false))
            });
        Ok(Some(PackageInfo {
            default_version: default
                .and_then(|v| v.pointer("/versionKey/version"))
                .and_then(Value::as_str)
                .map(str::to_string),
            default_published_at: default
                .and_then(|v| v.get("publishedAt"))
                .and_then(Value::as_str)
                .map(str::to_string),
        }))
    }

    /// GET /v3/systems/{system}/packages/{name}/versions/{version}: advisory count.
    pub fn version_advisories(
        &self,
        system: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<usize>> {
        let path = format!(
            "/v3/systems/{system}/packages/{}/versions/{}",
            crate::github::urlencode(name),
            crate::github::urlencode(version)
        );
        let Some(doc) = self.get(&path)? else {
            return Ok(None);
        };
        Ok(Some(
            doc.get("advisoryKeys")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        ))
    }
}

impl Default for DepsDevClient {
    fn default() -> Self {
        Self::new()
    }
}

pub struct DepsDevCollector {
    github: PlatformClient,
    depsdev: DepsDevClient,
    gh_limiter: Mutex<RateLimiter>,
    dd_limiter: Mutex<RateLimiter>,
}

impl DepsDevCollector {
    /// Production constructor: real GitHub API (token from GITHUB_TOKEN) and
    /// the live deps.dev v3 API.
    pub fn from_env() -> Self {
        Self::with_clients(
            PlatformClient::new(std::env::var("GITHUB_TOKEN").ok()),
            DepsDevClient::new(),
        )
    }

    /// Test/explicit constructor (httptest entry points).
    pub fn with_clients(github: PlatformClient, depsdev: DepsDevClient) -> Self {
        Self {
            github,
            depsdev,
            gh_limiter: Mutex::new(RateLimiter::new(SEARCH_PER_MIN)),
            dd_limiter: Mutex::new(RateLimiter::new(DEPSDEV_PER_MIN)),
        }
    }

    /// Overlay deps.dev data onto one mapped entity's metadata. Per-package
    /// lookup failures (non-404) are warn-logged and recorded as
    /// depsdev_found=false — one flaky package must not fail the batch.
    fn enrich(&self, entity: &mut CollectedEntity, system: &str, now_unix: i64) {
        let package = entity.name.clone();
        let mut meta: Value = entity
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        self.dd_limiter
            .lock()
            .expect("rate limiter poisoned")
            .wait();
        match self.depsdev.package(system, &package) {
            Ok(Some(info)) => {
                meta["depsdev_found"] = Value::Bool(true);
                let mut advisory_count = 0i64;
                if let (Some(version), Some(published_at)) =
                    (&info.default_version, &info.default_published_at)
                {
                    meta["depsdev_default_version"] = Value::String(version.clone());
                    meta["depsdev_default_published_at"] = Value::String(published_at.clone());
                    if let Some(days) = freshness_days(published_at, now_unix) {
                        meta["depsdev_version_freshness_days"] = serde_json::json!(days);
                    }
                    self.dd_limiter
                        .lock()
                        .expect("rate limiter poisoned")
                        .wait();
                    match self.depsdev.version_advisories(system, &package, version) {
                        Ok(Some(n)) => advisory_count = n as i64,
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(package = %package, error = %e, "deps.dev version lookup failed")
                        }
                    }
                }
                meta["depsdev_advisory_count"] = serde_json::json!(advisory_count);
            }
            Ok(None) => {
                meta["depsdev_found"] = Value::Bool(false);
                meta["depsdev_advisory_count"] = serde_json::json!(0);
            }
            Err(e) => {
                tracing::warn!(package = %package, error = %e, "deps.dev package lookup failed");
                meta["depsdev_found"] = Value::Bool(false);
                meta["depsdev_advisory_count"] = serde_json::json!(0);
            }
        }
        entity.metadata = Some(meta.to_string());
    }
}

impl Collector for DepsDevCollector {
    fn name(&self) -> &'static str {
        "depsdev"
    }

    fn fetch(&self) -> Result<Vec<CollectedEntity>> {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_secs() as i64;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for seed in SEED_SEARCHES {
            let items =
                crate::github::search(&self.github, &self.gh_limiter, seed.query, seed.per_page)?;
            for item in &items {
                let Some(mut entity) = crate::github::map_repo(item, seed.ecosystem) else {
                    continue;
                };
                if !seen.insert(entity.full_name.clone()) {
                    continue;
                }
                self.enrich(&mut entity, seed.depsdev_system, now_unix);
                out.push(entity);
            }
        }
        Ok(out)
    }
}

/// Days between an RFC 3339 `published_at` (deps.dev shape:
/// "YYYY-MM-DDTHH:MM:SSZ") and `now_unix` (seconds since the epoch).
/// Date-part precision is enough for a freshness signal. None on
/// unparseable input; never negative.
pub fn freshness_days(published_at: &str, now_unix: i64) -> Option<i64> {
    let date = published_at.get(..10)?;
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let published = days_from_civil(y, m, d);
    Some((now_unix.div_euclid(86_400) - published).max(0))
}

/// Days since 1970-01-01 for a proleptic Gregorian date
/// (Howard Hinnant's days-from-civil algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
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

    fn expect_github_searches(gh: &Server, npm: Value) {
        for (query, per_page, doc) in [
            ("topic%3Anpm-package", 34, npm),
            ("topic%3Apypi", 33, search_doc(vec![])),
            ("topic%3Acrates-io", 34, search_doc(vec![])),
        ] {
            gh.expect(
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

    fn collector(gh: &Server, dd: &Server) -> DepsDevCollector {
        DepsDevCollector::with_clients(
            PlatformClient::with_base_url(&gh.url("/").to_string(), None),
            DepsDevClient::with_base_url(&dd.url("/").to_string()),
        )
    }

    #[test]
    fn fetch_enriches_metadata_and_preserves_github_fields() {
        let gh = Server::run();
        let dd = Server::run();
        expect_github_searches(&gh, search_doc(vec![repo("substack/minimist", 6200)]));
        dd.expect(
            Expectation::matching(all_of![
                request::method("GET"),
                request::path("/v3/systems/NPM/packages/minimist"),
            ])
            .times(1)
            .respond_with(json_encoded(json!({
                "packageKey": {"system": "NPM", "name": "minimist"},
                "versions": [
                    {"versionKey": {"system": "NPM", "name": "minimist", "version": "1.2.5"},
                     "publishedAt": "2020-03-12T22:16:19Z",
                     "isDefault": false, "isDeprecated": false},
                    {"versionKey": {"system": "NPM", "name": "minimist", "version": "1.2.8"},
                     "publishedAt": "2022-02-09T21:04:13Z",
                     "isDefault": true, "isDeprecated": false}
                ]
            }))),
        );
        dd.expect(
            Expectation::matching(all_of![
                request::method("GET"),
                request::path("/v3/systems/NPM/packages/minimist/versions/1.2.8"),
            ])
            .times(1)
            .respond_with(json_encoded(json!({
                "versionKey": {"system": "NPM", "name": "minimist", "version": "1.2.8"},
                "publishedAt": "2022-02-09T21:04:13Z",
                "isDefault": true,
                "advisoryKeys": [{"id": "GHSA-xvch-5gv4-984h"}]
            }))),
        );

        let entities = collector(&gh, &dd).fetch().expect("fetch");
        assert_eq!(entities.len(), 1);
        let e = &entities[0];
        // GitHub-seeded fields preserved (full re-derivation, upsert overwrite).
        assert_eq!(e.platform, "github");
        assert_eq!(e.full_name, "substack/minimist");
        assert_eq!(e.score_value, 6200);
        assert_eq!(e.open_issues, 7);
        assert_eq!(e.category.as_deref(), Some("npm"));
        let meta: Value = serde_json::from_str(e.metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["depsdev_found"], true);
        assert_eq!(meta["depsdev_default_version"], "1.2.8");
        assert_eq!(meta["depsdev_default_published_at"], "2022-02-09T21:04:13Z");
        assert_eq!(meta["depsdev_advisory_count"], 1);
        assert!(meta["depsdev_version_freshness_days"].as_i64().unwrap() > 1000);
        // The github collector's own metadata keys survive the merge.
        assert_eq!(meta["package"], "minimist");
        assert_eq!(meta["ecosystem"], "npm");
    }

    #[test]
    fn fetch_unknown_package_marked_not_found() {
        let gh = Server::run();
        let dd = Server::run();
        expect_github_searches(&gh, search_doc(vec![repo("ghost/nopejs", 10)]));
        dd.expect(
            Expectation::matching(all_of![
                request::method("GET"),
                request::path("/v3/systems/NPM/packages/nopejs"),
            ])
            .times(1)
            .respond_with(status_code(404)),
        );
        let entities = collector(&gh, &dd).fetch().expect("fetch");
        assert_eq!(entities.len(), 1);
        let meta: Value = serde_json::from_str(entities[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["depsdev_found"], false);
        assert_eq!(meta["depsdev_advisory_count"], 0);
    }

    #[test]
    fn freshness_days_known_date() {
        // 2020-03-22T00:00:00Z = 1584835200; published 2020-03-12 → 10 days.
        assert_eq!(
            freshness_days("2020-03-12T22:16:19Z", 1_584_835_200),
            Some(10)
        );
    }

    #[test]
    fn freshness_days_garbage_is_none() {
        assert_eq!(freshness_days("not-a-date", 1_584_835_200), None);
        assert_eq!(freshness_days("", 1_584_835_200), None);
    }
}
