//! Byte-for-byte port of the cors() middleware in go-service/cmd/server/main.go
//! (delta 1). Logic kept pure so the unit tests mirror cmd/server/main_test.go
//! 1:1; the five Go tests are additionally ported as HTTP-level integration
//! tests in crates/server/tests/cors.rs.

pub struct CorsDecision {
    pub headers: Vec<(String, String)>,
    pub preflight: bool,
}

impl CorsDecision {
    pub fn apply(self, mut resp: crate::handlers::ApiResponse) -> crate::handlers::ApiResponse {
        for (k, v) in self.headers {
            resp.headers.push((k, v));
        }
        resp
    }
}

/// Exact port of Go cors():
///   auth on  → reflect Origin only when it equals CORS_ORIGIN (fallback
///              APP_URL), + Allow-Credentials: true + Vary: Origin; methods
///              "GET, POST, OPTIONS", headers "Content-Type, X-API-Key".
///   auth off → wildcard dev mode, byte-identical to Go.
///   OPTIONS  → 200 preflight, no body, handler not called.
pub fn cors_headers(
    auth_enabled: bool,
    app_url: &str,
    cors_origin: &str,
    method: &str,
    origin: Option<&str>,
) -> CorsDecision {
    let mut headers = Vec::new();
    if auth_enabled {
        let allowed = if cors_origin.is_empty() {
            app_url
        } else {
            cors_origin
        };
        if let Some(o) = origin {
            if !o.is_empty() && o == allowed {
                headers.push(("Access-Control-Allow-Origin".to_string(), o.to_string()));
                headers.push((
                    "Access-Control-Allow-Credentials".to_string(),
                    "true".to_string(),
                ));
                headers.push(("Vary".to_string(), "Origin".to_string()));
            }
        }
        headers.push((
            "Access-Control-Allow-Methods".to_string(),
            "GET, POST, OPTIONS".to_string(),
        ));
        headers.push((
            "Access-Control-Allow-Headers".to_string(),
            "Content-Type, X-API-Key".to_string(),
        ));
    } else {
        headers.push(("Access-Control-Allow-Origin".to_string(), "*".to_string()));
        headers.push((
            "Access-Control-Allow-Methods".to_string(),
            "GET, OPTIONS".to_string(),
        ));
        headers.push((
            "Access-Control-Allow-Headers".to_string(),
            "Content-Type".to_string(),
        ));
    }
    CorsDecision {
        headers,
        preflight: method == "OPTIONS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header<'a>(d: &'a CorsDecision, name: &str) -> Option<&'a str> {
        d.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    // Go port: TestCORSAuthDisabledServesWildcard
    #[test]
    fn auth_disabled_serves_wildcard() {
        let d = cors_headers(
            false,
            "http://localhost:8080",
            "",
            "GET",
            Some("http://localhost:3000"),
        );
        assert_eq!(header(&d, "Access-Control-Allow-Origin"), Some("*"));
        assert_eq!(header(&d, "Access-Control-Allow-Credentials"), None);
        assert!(!d.preflight);
    }

    // Go port: TestCORSAuthEnabledReflectsAllowedOrigin (CORS_ORIGIN empty → APP_URL fallback)
    #[test]
    fn auth_enabled_reflects_allowed_origin() {
        let d = cors_headers(
            true,
            "https://app.example.com",
            "",
            "GET",
            Some("https://app.example.com"),
        );
        assert_eq!(
            header(&d, "Access-Control-Allow-Origin"),
            Some("https://app.example.com")
        );
        assert_eq!(header(&d, "Access-Control-Allow-Credentials"), Some("true"));
        assert_eq!(header(&d, "Vary"), Some("Origin"));
    }

    // Go port: TestCORSAuthEnabledPrefersConfiguredOrigin
    #[test]
    fn auth_enabled_prefers_configured_origin() {
        let d = cors_headers(
            true,
            "https://app.example.com",
            "http://localhost:3000",
            "GET",
            Some("http://localhost:3000"),
        );
        assert_eq!(
            header(&d, "Access-Control-Allow-Origin"),
            Some("http://localhost:3000")
        );
        let d = cors_headers(
            true,
            "https://app.example.com",
            "http://localhost:3000",
            "GET",
            Some("https://app.example.com"),
        );
        assert_eq!(
            header(&d, "Access-Control-Allow-Origin"),
            None,
            "APP_URL is NOT allowed when CORS_ORIGIN is set"
        );
    }

    // Go port: TestCORSAuthEnabledRejectsDisallowedOrigin
    #[test]
    fn auth_enabled_rejects_disallowed_origin() {
        let d = cors_headers(
            true,
            "https://app.example.com",
            "",
            "GET",
            Some("https://evil.example.com"),
        );
        assert_eq!(header(&d, "Access-Control-Allow-Origin"), None);
        assert_eq!(header(&d, "Access-Control-Allow-Credentials"), None);
    }

    // Go port: TestCORSAuthEnabledPreflightAllowsCredentialedPost
    #[test]
    fn auth_enabled_preflight_allows_credentialed_post() {
        let d = cors_headers(
            true,
            "https://app.example.com",
            "",
            "OPTIONS",
            Some("https://app.example.com"),
        );
        assert!(d.preflight, "OPTIONS short-circuits with 200");
        assert_eq!(
            header(&d, "Access-Control-Allow-Origin"),
            Some("https://app.example.com")
        );
        assert_eq!(
            header(&d, "Access-Control-Allow-Methods"),
            Some("GET, POST, OPTIONS"),
            "POST allowed for logout/checkout"
        );
        assert_eq!(
            header(&d, "Access-Control-Allow-Headers"),
            Some("Content-Type, X-API-Key")
        );
    }
}
