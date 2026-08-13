//! Port of go-service/internal/analytics/posthog.go.
//! No-op when POSTHOG_API_KEY is empty; capture errors are LOGGED via tracing
//! (delta 11 — Go swallowed them silently; Rust still never fails the request).

use std::sync::RwLock;

const DEFAULT_HOST: &str = "https://app.posthog.com";
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5); // Go: 5s client timeout

struct AnalyticsConfig {
    api_key: String,
    api_host: String,
}

static CONFIG: RwLock<Option<AnalyticsConfig>> = RwLock::new(None);

/// Port of Go `analytics.Init` (plus an api_host seam for tests; Go used a package var).
pub fn init(posthog_api_key: &str) {
    let mut guard = CONFIG.write().expect("analytics config lock");
    *guard = if posthog_api_key.is_empty() {
        None
    } else {
        Some(AnalyticsConfig {
            api_key: posthog_api_key.to_string(),
            api_host: DEFAULT_HOST.to_string(),
        })
    };
}

/// Builds the exact JSON Go POSTs to {apiHost}/capture/.
fn capture_body(
    path: &str,
    method: &str,
    user_agent: &str,
    status: u16,
    api_key: &str,
) -> serde_json::Value {
    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    serde_json::json!({
        "api_key": api_key,
        "event": "api_request",
        "distinct_id": path,
        "properties": { "method": method, "ua": user_agent, "status": status },
        "timestamp": timestamp,
    })
}

/// No-op without a key. Fire-and-forget POST (Go: `go func(){ ... }()`);
/// called only from the async server binary, which guarantees a tokio runtime.
/// Capture errors are LOGGED (delta 11) and never fail the request.
pub fn capture_api_request(path: &str, method: &str, user_agent: &str, status: u16) {
    let (api_key, api_host) = {
        let guard = CONFIG.read().expect("analytics config lock");
        match &*guard {
            Some(c) => (c.api_key.clone(), c.api_host.clone()),
            None => return,
        }
    };
    let body = capture_body(path, method, user_agent, status, &api_key);
    tokio::spawn(async move {
        let result = reqwest::Client::builder()
            .timeout(CAPTURE_TIMEOUT)
            .build()
            .map_err(anyhow::Error::from);
        match result {
            Ok(client) => {
                let resp = client
                    .post(format!("{api_host}/capture/"))
                    .json(&body)
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {}
                    Ok(r) => tracing::warn!("analytics: posthog capture returned {}", r.status()),
                    Err(e) => tracing::warn!("analytics: posthog capture failed: {e}"),
                }
            }
            Err(e) => tracing::warn!("analytics: http client build failed: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_shape_matches_go() {
        let v = capture_body("/api/v1/stats", "GET", "ua-x", 200, "phc_test");
        assert_eq!(v["api_key"], "phc_test");
        assert_eq!(v["event"], "api_request");
        assert_eq!(v["distinct_id"], "/api/v1/stats"); // Go passes path as distinct_id
        assert_eq!(v["properties"]["method"], "GET");
        assert_eq!(v["properties"]["ua"], "ua-x");
        assert_eq!(v["properties"]["status"], 200);
        let ts = v["timestamp"].as_str().expect("timestamp is a string");
        assert!(
            time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).is_ok()
        );
    }

    // CONFIG is a process-global static; serialize the tests that mutate it.
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn no_key_noop_returns_before_spawning() {
        let _g = TEST_LOCK.blocking_lock();
        init(""); // disabled: must return before any tokio::spawn (no runtime in this test)
        capture_api_request("/api/v1/stats", "GET", "ua", 200);
    }

    #[tokio::test]
    async fn capture_fires_post_to_capture_endpoint() {
        let _g = TEST_LOCK.lock().await;
        let server = httptest::Server::run();
        server.expect(
            httptest::Expectation::matching(httptest::matchers::request::method_path(
                "POST",
                "/capture/",
            ))
            .times(1)
            .respond_with(httptest::responders::status_code(200)),
        );
        let host = server.url_str("/").trim_end_matches('/').to_string();
        *CONFIG.write().expect("analytics config lock") = Some(AnalyticsConfig {
            api_key: "phc_test".into(),
            api_host: host,
        });
        capture_api_request("/api/v1/stats", "GET", "ua-x", 200);
        // fire-and-forget task: give it time to land before Server drop verifies the expectation
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
