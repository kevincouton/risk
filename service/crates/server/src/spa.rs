//! SPA static serving from ./web/dist with index.html fallback (spine route
//! table: "serve ./web/dist with fallback to index.html").
//! NOTE: Go used http.FileServer with NO fallback (unknown paths 404'd) — the
//! spine freezes the fallback, so it wins; flag for the R-1/R-5 fixture
//! comparison if a fixture captured Go's 404 for an unknown SPA path.
//! /api/, /auth/, /healthz paths never fall back — absent gated routes must
//! keep their Go 404 (flags-off matrix).

use std::path::{Path, PathBuf};

/// Maps a URL path to (Content-Type, bytes), or None → 404.
pub fn serve(dist: &Path, url_path: &str) -> Option<(String, Vec<u8>)> {
    let rel = url_path.trim_start_matches('/');
    if rel.split('/').any(|seg| seg == "..") {
        return None; // path traversal
    }
    let mut candidate: PathBuf = dist.join(if rel.is_empty() { "index.html" } else { rel });
    if candidate.is_dir() {
        candidate = candidate.join("index.html");
    }
    if !candidate.is_file() {
        // Absent gated routes are not SPA pages: keep their Go 404.
        if url_path.starts_with("/api/") || url_path.starts_with("/auth/") || url_path == "/healthz"
        {
            return None;
        }
        candidate = dist.join("index.html");
    }
    let bytes = std::fs::read(&candidate).ok()?;
    Some((content_type(&candidate), bytes))
}

fn content_type(p: &Path) -> String {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>spa</html>").unwrap();
        std::fs::write(dir.path().join("app.js"), "console.log(1)").unwrap();
        let path = dir.path().to_path_buf(); // bind before moving dir into the tuple
        (dir, path)
    }

    #[test]
    fn serves_existing_file_with_content_type() {
        let (_d, dist) = dist();
        let (ct, body) = serve(&dist, "/app.js").expect("app.js exists");
        assert_eq!(ct, "text/javascript; charset=utf-8");
        assert_eq!(body, b"console.log(1)");
    }

    #[test]
    fn falls_back_to_index_html_for_client_routes() {
        let (_d, dist) = dist();
        let (_ct, body) = serve(&dist, "/some/client/route").expect("fallback");
        assert_eq!(body, b"<html>spa</html>");
        let (_ct, body) = serve(&dist, "/").expect("root serves index.html");
        assert_eq!(body, b"<html>spa</html>");
    }

    #[test]
    fn api_auth_paths_never_fall_back() {
        let (_d, dist) = dist();
        assert!(
            serve(&dist, "/api/billing/webhook").is_none(),
            "absent gated route → 404, not SPA"
        );
        assert!(
            serve(&dist, "/auth/login").is_none(),
            "auth-disabled route → 404, not SPA"
        );
        assert!(serve(&dist, "/healthz").is_none());
        assert!(
            serve(&dist, "/../secret").is_none(),
            "path traversal rejected"
        );
    }
}
