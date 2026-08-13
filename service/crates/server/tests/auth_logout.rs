//! delta 3: logout is POST-only (GET → 405). Also: login 302 shape and
//! /auth/me 401. The OIDC issuer is a localhost httptest stub serving only the
//! discovery document (logout/me/login never reach token exchange).

mod common;

use common::{spawn_test_server, stop};

const SIGNING_KEY: &str = "0123456789abcdef0123456789abcdef"; // 32 bytes

/// Start a stub OIDC issuer; returns its URL (no trailing slash).
fn start_issuer() -> (httptest::Server, String) {
    let server = httptest::Server::run();
    let issuer = server.url_str("/").trim_end_matches('/').to_string();
    let doc = serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    });
    server.expect(
        httptest::Expectation::matching(httptest::matchers::request::method_path(
            "GET",
            "/.well-known/openid-configuration",
        ))
        .respond_with(httptest::responders::status_code(200).body(doc.to_string())),
    );
    // openidconnect 3.5 fetches the JWKS during discovery; no test here
    // verifies tokens, so an empty key set suffices.
    server.expect(
        httptest::Expectation::matching(httptest::matchers::request::method_path("GET", "/jwks"))
            .respond_with(httptest::responders::status_code(200).body("{\"keys\":[]}")),
    );
    (server, issuer)
}

fn spawn_authed(issuer: &str) -> (std::process::Child, String) {
    spawn_test_server(&[
        ("AUTH_ENABLED", "true"),
        ("SESSION_SIGNING_KEY", SIGNING_KEY),
        ("OIDC_ISSUER", issuer),
        ("OIDC_CLIENT_ID", "cid"),
        ("OIDC_CLIENT_SECRET", "sec"),
    ])
}

// delta 3: GET → 405, POST → 200 {"ok":true} + cleared session cookie.
#[test]
fn logout_post_only() {
    let (_iss, issuer) = start_issuer();
    let (child, base) = spawn_authed(&issuer);
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client.get(format!("{base}/auth/logout")).send().unwrap();
    assert_eq!(resp.status(), 405, "GET /auth/logout must be 405 (delta 3)");
    assert_eq!(resp.text().unwrap(), "{\"error\":\"method not allowed\"}\n");
    let resp = client.post(format!("{base}/auth/logout")).send().unwrap();
    assert_eq!(resp.status(), 200);
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(resp.text().unwrap(), "{\"ok\":true}");
    assert!(
        set_cookie.starts_with("session=;"),
        "clears the session cookie: {set_cookie}"
    );
    assert!(
        set_cookie.contains("Max-Age=0"),
        "Go renders MaxAge=-1 as Max-Age=0: {set_cookie}"
    );
    stop(child);
}

#[test]
fn login_redirects_to_issuer_with_state_cookie() {
    let (_iss, issuer) = start_issuer();
    let (child, base) = spawn_authed(&issuer);
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client.get(format!("{base}/auth/login")).send().unwrap();
    assert_eq!(resp.status(), 302);
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with(&format!("{issuer}/authorize")),
        "location = {location}"
    );
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        set_cookie.starts_with("oidc_state="),
        "state cookie set: {set_cookie}"
    );
    assert!(set_cookie.contains("Max-Age=300"), "{set_cookie}");
    stop(child);
}

#[test]
fn me_401_without_session() {
    let (_iss, issuer) = start_issuer();
    let (child, base) = spawn_authed(&issuer);
    let resp = reqwest::blocking::Client::new()
        .get(format!("{base}/auth/me"))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(resp.text().unwrap(), "{\"error\":\"unauthenticated\"}\n");
    stop(child);
}
