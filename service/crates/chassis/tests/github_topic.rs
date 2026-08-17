//! httptest-fixture tests for chassis::collectors::github_topic (Wave 2, W2-0).
//! No live network: every test runs against an httptest::Server via
//! PlatformClient::with_base_url.

use chassis::collectors::github_topic::GithubTopicCollector;
use chassis::collectors::{run_collector, Collector};
use chassis::platform::PlatformClient;
use httptest::{matchers::*, responders::*, Expectation, Server};
use rusqlite::Connection;
use tempfile::TempDir;

const SEARCH_PATH: &str = "/search/repositories";

fn repo_fixture(full_name: &str, stars: i64, issues: i64) -> serde_json::Value {
    let (owner, name) = full_name.split_once('/').expect("fixture full_name");
    serde_json::json!({
        "full_name": full_name,
        "name": name,
        "owner": {"login": owner},
        "description": format!("desc of {full_name}"),
        "stargazers_count": stars,
        "open_issues_count": issues,
        "pushed_at": "2026-08-01T12:00:00Z",
        "language": "Rust",
        "forks_count": 7,
        "license": {"spdx_id": "MIT"},
    })
}

fn search_response(items: Vec<serde_json::Value>) -> serde_json::Value {
    let total = items.len();
    serde_json::json!({"total_count": total, "incomplete_results": false, "items": items})
}

/// The query is matched byte-for-byte (httptest: a String is an implicit Eq
/// matcher over the raw query string; reqwest sends `topic:<t>` unencoded).
fn expect_search(server: &Server, topic: &str, body: serde_json::Value) {
    server.expect(
        Expectation::matching(all_of![
            request::method_path("GET", SEARCH_PATH),
            request::query(format!(
                "q=topic:{topic}&sort=stars&order=desc&per_page=100"
            )),
        ])
        .times(1)
        .respond_with(json_encoded(body)),
    );
}

fn collector(server: &Server, topics: &[&str]) -> GithubTopicCollector {
    GithubTopicCollector::with_client(
        PlatformClient::with_base_url(&server.url("/").to_string(), None),
        topics.iter().map(|t| t.to_string()).collect(),
    )
}

fn open_test_db() -> (TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.db");
    let conn = chassis::db::open(path.to_str().expect("utf8 path")).expect("open db");
    chassis::db::migrate(&conn).expect("migrate");
    (dir, conn)
}

#[test]
fn maps_search_items_to_collected_entities() {
    let server = Server::run();
    expect_search(
        &server,
        "rust",
        search_response(vec![repo_fixture("octo/hello", 1234, 56)]),
    );
    let c = collector(&server, &["rust"]);
    let entities = c.fetch().expect("fetch");
    assert_eq!(entities.len(), 1);
    let e = &entities[0];
    assert_eq!(e.platform, "github");
    assert_eq!(e.slug, "octo");
    assert_eq!(e.name, "hello");
    assert_eq!(e.full_name, "octo/hello");
    assert_eq!(e.description.as_deref(), Some("desc of octo/hello"));
    assert_eq!(e.category.as_deref(), Some("rust"));
    assert_eq!(e.score_value, 1234);
    assert_eq!(e.open_issues, 56);
    assert_eq!(e.last_pushed_at.as_deref(), Some("2026-08-01T12:00:00Z"));
    let meta: serde_json::Value =
        serde_json::from_str(e.metadata.as_deref().expect("metadata json"))
            .expect("parse metadata");
    assert_eq!(meta["language"], "Rust");
    assert_eq!(meta["forks_count"], 7);
    assert_eq!(meta["license"], "MIT");
}

#[test]
fn dedupes_across_topics_first_topic_wins_category() {
    let server = Server::run();
    expect_search(
        &server,
        "alpha",
        search_response(vec![repo_fixture("a/x", 10, 1)]),
    );
    expect_search(
        &server,
        "beta",
        search_response(vec![repo_fixture("a/x", 10, 1), repo_fixture("b/y", 5, 0)]),
    );
    let c = collector(&server, &["alpha", "beta"]);
    let entities = c.fetch().expect("fetch");
    assert_eq!(entities.len(), 2, "a/x from topic beta must be deduped");
    let x = entities.iter().find(|e| e.full_name == "a/x").expect("a/x");
    assert_eq!(x.category.as_deref(), Some("alpha"), "first topic wins");
    let y = entities.iter().find(|e| e.full_name == "b/y").expect("b/y");
    assert_eq!(y.category.as_deref(), Some("beta"));
}

#[test]
fn empty_topics_fetch_zero_entities_without_http() {
    // No expectations registered: any HTTP request would fail the test.
    let server = Server::run();
    let c = collector(&server, &[]);
    let entities = c.fetch().expect("empty topics is not an error");
    assert!(entities.is_empty());
}

#[test]
fn empty_search_page_yields_zero_entities() {
    let server = Server::run();
    expect_search(&server, "rust", search_response(vec![]));
    let c = collector(&server, &["rust"]);
    let entities = c.fetch().expect("empty page is not an error");
    assert!(entities.is_empty());
}

#[test]
fn run_collector_retries_once_after_500_then_upserts() {
    let server = Server::run();
    // Use a cycling responder so the first request returns 500 and the second
    // (retry) returns the successful search payload.
    server.expect(
        Expectation::matching(all_of![
            request::method_path("GET", SEARCH_PATH),
            request::query("q=topic:rust&sort=stars&order=desc&per_page=100"),
        ])
        .times(2)
        .respond_with(httptest::cycle![
            status_code(500),
            json_encoded(search_response(vec![repo_fixture("octo/hello", 42, 3)]))
        ]),
    );
    let (_dir, mut conn) = open_test_db();
    let c = collector(&server, &["rust"]);
    // run_collector supplies the retry (1s backoff); fetch alone must fail on 500.
    let res = run_collector(&mut conn, &c).expect("run succeeds after one retry");
    assert_eq!((res.fetched, res.upserted), (1, 1));
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE platform = 'github' AND full_name = 'octo/hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}
