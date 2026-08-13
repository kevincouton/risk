//! Shared harness: spawn the REAL server binary on an ephemeral port with a
//! tempdir SQLite DB. No network beyond 127.0.0.1.

use std::net::TcpListener;
use std::process::{Child, Command};

/// Returns (child, base_url). Caller MUST `stop(child)`.
///
/// `env` entries override the defaults below — including DATABASE_PATH, which
/// tests pass explicitly when they need to seed the DB via chassis afterwards
/// (the helper's own tempdir DB then simply goes unused).
pub fn spawn_test_server(env: &[(&str, &str)]) -> (Child, String) {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.keep().join("test.db"); // keep(): dir must outlive the child
    let base = format!("http://127.0.0.1:{port}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_server"));
    cmd.env("API_PORT", port.to_string())
        .env("DATABASE_PATH", &db_path)
        .env("APP_URL", &base)
        .env("AUTH_ENABLED", "false")
        .env("BILLING_ENABLED", "false")
        .env("API_KEYS_ENABLED", "false")
        .env("POSTHOG_API_KEY", "");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Ok(resp) = reqwest::blocking::get(format!("{base}/healthz")) {
            if resp.status().is_success() {
                break;
            }
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("server exited before becoming healthy: {status}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "server did not become healthy within 15s"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    (child, base)
}

pub fn stop(mut child: Child) {
    child.kill().unwrap();
    child.wait().unwrap();
}
