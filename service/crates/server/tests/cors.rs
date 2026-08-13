//! HTTP-level ports of the 5 CORS tests in go-service/cmd/server/main_test.go.

mod common;

use common::{spawn_test_server, stop};

fn get_with_origin(base: &str, path: &str, origin: &str) -> reqwest::blocking::Response {
    reqwest::blocking::Client::new()
        .get(format!("{base}{path}"))
        .header("Origin", origin)
        .send()
        .unwrap()
}

// Go port: TestCORSAuthDisabledServesWildcard
#[test]
fn auth_disabled_serves_wildcard() {
    let (child, base) = spawn_test_server(&[]);
    let resp = get_with_origin(&base, "/api/v1/stats", "http://localhost:3000");
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap(),
        "*"
    );
    assert!(resp
        .headers()
        .get("access-control-allow-credentials")
        .is_none());
    assert_eq!(
        resp.headers().get("access-control-allow-methods").unwrap(),
        "GET, OPTIONS"
    );
    assert_eq!(
        resp.headers().get("access-control-allow-headers").unwrap(),
        "Content-Type"
    );
    stop(child);
}

// Go port: TestCORSAuthEnabledReflectsAllowedOrigin (CORS_ORIGIN empty → APP_URL fallback).
// Note: cors() keys off config.AuthEnabled, NOT a built provider — so a short
// SESSION_SIGNING_KEY (provider skipped) still exercises credentialed mode.
#[test]
fn auth_enabled_reflects_allowed_origin() {
    let (child, base) = spawn_test_server(&[
        ("AUTH_ENABLED", "true"),
        ("APP_URL", "https://app.example.com"),
    ]);
    let resp = get_with_origin(&base, "/api/v1/stats", "https://app.example.com");
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap(),
        "https://app.example.com"
    );
    assert_eq!(
        resp.headers()
            .get("access-control-allow-credentials")
            .unwrap(),
        "true"
    );
    assert_eq!(resp.headers().get("vary").unwrap(), "Origin");
    stop(child);
}

// Go port: TestCORSAuthEnabledPrefersConfiguredOrigin
#[test]
fn auth_enabled_prefers_configured_origin() {
    let (child, base) = spawn_test_server(&[
        ("AUTH_ENABLED", "true"),
        ("APP_URL", "https://app.example.com"),
        ("CORS_ORIGIN", "http://localhost:3000"),
    ]);
    let resp = get_with_origin(&base, "/api/v1/stats", "http://localhost:3000");
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap(),
        "http://localhost:3000"
    );
    let resp = get_with_origin(&base, "/api/v1/stats", "https://app.example.com");
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "APP_URL not allowed when CORS_ORIGIN set"
    );
    stop(child);
}

// Go port: TestCORSAuthEnabledRejectsDisallowedOrigin
#[test]
fn auth_enabled_rejects_disallowed_origin() {
    let (child, base) = spawn_test_server(&[
        ("AUTH_ENABLED", "true"),
        ("APP_URL", "https://app.example.com"),
    ]);
    let resp = get_with_origin(&base, "/api/v1/stats", "https://evil.example.com");
    assert!(resp.headers().get("access-control-allow-origin").is_none());
    assert!(resp
        .headers()
        .get("access-control-allow-credentials")
        .is_none());
    stop(child);
}

// Go port: TestCORSAuthEnabledPreflightAllowsCredentialedPost
#[test]
fn auth_enabled_preflight_allows_credentialed_post() {
    let (child, base) = spawn_test_server(&[
        ("AUTH_ENABLED", "true"),
        ("APP_URL", "https://app.example.com"),
    ]);
    let resp = reqwest::blocking::Client::new()
        .request(reqwest::Method::OPTIONS, format!("{base}/api/v1/stats"))
        .header("Origin", "https://app.example.com")
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200, "preflight status");
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap(),
        "https://app.example.com"
    );
    assert_eq!(
        resp.headers().get("access-control-allow-methods").unwrap(),
        "GET, POST, OPTIONS"
    );
    stop(child);
}
