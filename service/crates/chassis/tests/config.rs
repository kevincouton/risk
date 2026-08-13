use chassis::config::{Config, PLATFORM_NAME};
use std::sync::Mutex;

/// Env-mutating tests must not race each other under cargo's parallel harness.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const KEYS: [&str; 18] = [
    "DATABASE_PATH",
    "API_PORT",
    "POSTHOG_API_KEY",
    "GA_ID",
    "ADS_ID",
    "AUTH_ENABLED",
    "OIDC_ISSUER",
    "OIDC_CLIENT_ID",
    "OIDC_CLIENT_SECRET",
    "SESSION_SIGNING_KEY",
    "APP_URL",
    "CORS_ORIGIN",
    "BILLING_ENABLED",
    "STRIPE_SECRET_KEY",
    "STRIPE_WEBHOOK_SECRET",
    "STRIPE_PRICE_ID",
    "API_KEYS_ENABLED",
    "DEV_USER_ID",
];

fn clean_env() {
    for k in KEYS {
        std::env::remove_var(k);
    }
}

#[test]
fn defaults_are_fail_closed() {
    let _g = ENV_LOCK.lock().unwrap();
    clean_env();
    let cfg = Config::load();
    // The template compiles as-is: the platform name is literally
    // "risk" until bin/instantiate-platform substitutes it,
    // matching the Go template's behavior.
    assert_eq!(PLATFORM_NAME, "risk");
    assert_eq!(cfg.platform_name, "risk");
    assert_eq!(cfg.database_path, "./data/risk.db");
    assert_eq!(cfg.api_port, 8080);
    assert!(!cfg.auth_enabled);
    assert!(!cfg.billing_enabled);
    assert!(!cfg.api_keys_enabled);
    assert_eq!(cfg.app_url, "http://localhost:8080");
    assert_eq!(cfg.cors_origin, "");
    assert_eq!(cfg.dev_user_id, "");
    assert_eq!(cfg.posthog_api_key, "");
    assert_eq!(cfg.ga_id, "");
    assert_eq!(cfg.ads_id, "");
    assert_eq!(cfg.oidc_issuer, "");
    assert_eq!(cfg.session_signing_key, "");
    assert_eq!(cfg.stripe_secret_key, "");
    clean_env();
}

#[test]
fn env_overrides_apply() {
    let _g = ENV_LOCK.lock().unwrap();
    clean_env();
    std::env::set_var("API_PORT", "9090");
    std::env::set_var("AUTH_ENABLED", "true");
    std::env::set_var("DATABASE_PATH", "/tmp/override.db");
    std::env::set_var("APP_URL", "https://example.com");
    std::env::set_var("CORS_ORIGIN", "https://ui.example.com");
    std::env::set_var("DEV_USER_ID", "dev-1");
    let cfg = Config::load();
    assert_eq!(cfg.api_port, 9090);
    assert!(cfg.auth_enabled);
    assert_eq!(cfg.database_path, "/tmp/override.db");
    assert_eq!(cfg.app_url, "https://example.com");
    assert_eq!(cfg.cors_origin, "https://ui.example.com");
    assert_eq!(cfg.dev_user_id, "dev-1");
    clean_env();
}

#[test]
fn app_url_default_uses_api_port() {
    let _g = ENV_LOCK.lock().unwrap();
    clean_env();
    std::env::set_var("API_PORT", "9001");
    let cfg = Config::load();
    assert_eq!(cfg.app_url, "http://localhost:9001");
    clean_env();
}

#[test]
fn bool_flags_require_exact_true() {
    let _g = ENV_LOCK.lock().unwrap();
    clean_env();
    for value in ["TRUE", "1", "yes", ""] {
        std::env::set_var("AUTH_ENABLED", value);
        assert!(
            !Config::load().auth_enabled,
            "AUTH_ENABLED={value:?} must be false"
        );
    }
    std::env::set_var("AUTH_ENABLED", "true");
    assert!(Config::load().auth_enabled);
    clean_env();
}
