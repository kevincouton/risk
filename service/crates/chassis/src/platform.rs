//! Upstream platform API client (GitHub semantics).
//!
//! go-service/internal/platform/client.go defines only the ReadmeFetcher
//! interface; this module ports that contract and provides the shared
//! paginated fetch used by collectors. Delta 4: every pagination loop is
//! bounded by MAX_PAGES — where a Go loop would run unbounded, the Rust
//! client stops at 100 pages, logs a warning, and returns partial results.
//!
//! Synchronous (reqwest::blocking) per the spine's sync/async rule: this
//! client is used by collectors/scoring, never inside async handlers.

use anyhow::{anyhow, Context, Result};

/// Delta 4: hard ceiling on paginated fetch loops.
pub const MAX_PAGES: u32 = 100;

const DEFAULT_BASE_URL: &str = "https://api.github.com";

pub struct PlatformClient {
    http: reqwest::blocking::Client,
    token: Option<String>,
    base_url: String,
}

impl PlatformClient {
    /// Client for the real GitHub API; `token` (optional) is sent as a
    /// Bearer Authorization header on every request.
    pub fn new(token: Option<String>) -> Self {
        Self::with_base_url(DEFAULT_BASE_URL, token)
    }

    /// Client against an arbitrary base URL (used by httptest-based tests).
    pub fn with_base_url(base_url: &str, token: Option<String>) -> Self {
        // Bound to a `let` so rustfmt's rendering is stable no matter how long
        // risk is after instantiation (R-5: the inline chain-arg
        // form flipped multi/single-line on substitution and broke the
        // instantiated `cargo fmt --check` gate).
        let user_agent = concat!("chassis-platform-client (", "risk", ")");
        let http = reqwest::blocking::Client::builder()
            .user_agent(user_agent)
            .build()
            .expect("reqwest blocking client with rustls and default config cannot fail");
        Self {
            http,
            token,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    fn get(&self, url: &str) -> Result<reqwest::blocking::Response> {
        let mut req = self
            .http
            .get(url)
            .header("Accept", "application/vnd.github+json");
        if let Some(token) = &self.token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("GET {url} returned status {status}"));
        }
        Ok(resp)
    }

    /// GET one JSON document from a path relative to the base URL.
    pub fn get_json(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        self.get(&url)?
            .json::<serde_json::Value>()
            .with_context(|| format!("parse JSON from {url}"))
    }

    /// GET all pages of a JSON-array endpoint, following RFC 5988
    /// `Link: <url>; rel="next"` headers, bounded by MAX_PAGES (delta 4).
    /// When the ceiling is hit, a warning is logged and the partial results
    /// collected so far are returned.
    pub fn get_paginated(&self, path: &str) -> Result<Vec<serde_json::Value>> {
        let mut items = Vec::new();
        let mut next_url = Some(format!("{}{}", self.base_url, path));
        let mut pages: u32 = 0;
        while let Some(url) = next_url.take() {
            let resp = self.get(&url)?;
            let link = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let page: Vec<serde_json::Value> = resp
                .json()
                .with_context(|| format!("parse JSON array from {url}"))?;
            items.extend(page);
            pages += 1;
            match link.as_deref().and_then(parse_next_link) {
                Some(next) if pages < MAX_PAGES => next_url = Some(next.to_string()),
                Some(_) => {
                    eprintln!(
                        "chassis::platform: MAX_PAGES ({MAX_PAGES}) reached for {path}; \
                         returning {} items from {pages} pages",
                        items.len()
                    );
                    break;
                }
                None => break,
            }
        }
        Ok(items)
    }

    /// Fetch and base64-decode a repository README
    /// (GitHub `GET /repos/{owner}/{repo}/readme`, `encoding: "base64"`;
    /// the API wraps the content at 60 columns with newlines).
    pub fn get_readme(&self, owner: &str, name: &str) -> Result<String> {
        let value = self.get_json(&format!("/repos/{owner}/{name}/readme"))?;
        let content = value
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow!("readme response for {owner}/{name} has no content field"))?;
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64::decode(compact.as_bytes(), base64::Alphabet::Standard)
            .context("decode readme base64")?;
        String::from_utf8(bytes).context("readme is not valid UTF-8")
    }
}

impl crate::scoring::ReadmeFetcher for PlatformClient {
    fn get_readme(&self, owner: &str, name: &str) -> Result<String> {
        PlatformClient::get_readme(self, owner, name)
    }
}

/// Extract the `rel="next"` URL from an RFC 5988 Link header value.
fn parse_next_link(header: &str) -> Option<&str> {
    for part in header.split(',') {
        let mut segments = part.split(';');
        let url = segments.next()?.trim();
        let is_next = segments.any(|s| s.trim() == "rel=\"next\"");
        if is_next {
            return Some(url.trim_start_matches('<').trim_end_matches('>'));
        }
    }
    None
}
