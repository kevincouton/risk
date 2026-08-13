//! Flags default-off fail-closed: gated routes are absent (404) when disabled
//! (spec §5.1 flag gating).

mod common;

use common::{spawn_test_server, stop};

#[test]
fn auth_routes_404_when_disabled() {
    let (child, base) = spawn_test_server(&[]);
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for path in ["/auth/login", "/auth/callback", "/auth/me"] {
        let resp = client.get(format!("{base}{path}")).send().unwrap();
        assert_eq!(
            resp.status(),
            404,
            "{path} must be absent when AUTH_ENABLED=false"
        );
    }
    let resp = client.post(format!("{base}/auth/logout")).send().unwrap();
    assert_eq!(
        resp.status(),
        404,
        "/auth/logout absent when AUTH_ENABLED=false"
    );
    stop(child);
}

#[test]
fn billing_routes_404_when_disabled() {
    let (child, base) = spawn_test_server(&[]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base}/api/billing/webhook"))
        .body("{}")
        .send()
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "webhook absent when BILLING_ENABLED=false"
    );
    let resp = client
        .post(format!("{base}/api/billing/checkout"))
        .send()
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "checkout absent when BILLING_ENABLED=false"
    );
    stop(child);
}

#[test]
fn health_stats_search_ok_when_flags_off() {
    let (child, base) = spawn_test_server(&[]);
    let client = reqwest::blocking::Client::new();
    let resp = client.get(format!("{base}/healthz")).send().unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "{\"status\":\"ok\"}\n");
    let resp = client.get(format!("{base}/api/v1/stats")).send().unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().unwrap();
    assert_eq!(v["total_entities"], 0);
    assert_eq!(v["total_scores"], 0);
    assert_eq!(v["verdicts"], serde_json::json!({}));
    // Empty search result → "entities":null (Go nil-slice parity).
    let resp = client
        .get(format!("{base}/api/v1/search?q=x"))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().unwrap();
    assert_eq!(v["query"], "x");
    assert!(v["entities"].is_null());
    assert_eq!(v["total"], 0);
    stop(child);
}

#[test]
fn search_missing_q_400_byte_exact() {
    let (child, base) = spawn_test_server(&[]);
    let resp = reqwest::blocking::Client::new()
        .get(format!("{base}/api/v1/search"))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 400);
    // Byte-exact Go body: note the space after the colon and the trailing newline.
    assert_eq!(
        resp.text().unwrap(),
        "{\"error\": \"missing q parameter\"}\n"
    );
    stop(child);
}
