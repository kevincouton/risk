//! Environment-driven configuration.
//!
//! Port of go-service/internal/config/config.go with delta 10: the database
//! path is no longer a hardcoded clone path; DATABASE_PATH defaults to
//! ./data/<platform>.db.
//!
//! This file is a template: `risk` is a valid Rust string
//! literal, so the crate compiles as-is with the platform name literally
//! "risk" until bin/instantiate-platform substitutes it — the
//! same behavior as the Go template.

use std::env;

/// Platform name, substituted per clone by bin/instantiate-platform.
pub const PLATFORM_NAME: &str = "risk";

pub struct Config {
    pub platform_name: String,
    pub database_path: String,
    pub api_port: u16,
    pub posthog_api_key: String,
    pub ga_id: String,
    pub ads_id: String,
    pub auth_enabled: bool,
    pub oidc_issuer: String,
    pub oidc_client_id: String,
    pub oidc_client_secret: String,
    pub session_signing_key: String,
    pub app_url: String,
    pub cors_origin: String,
    pub billing_enabled: bool,
    pub stripe_secret_key: String,
    pub stripe_webhook_secret: String,
    pub stripe_price_id: String,
    pub api_keys_enabled: bool,
    pub dev_user_id: String,
}

impl Config {
    /// Load configuration from the environment, loading an optional `.env`
    /// file first (a missing `.env` is not an error, matching godotenv).
    pub fn load() -> Config {
        let _ = dotenvy::dotenv();

        let api_port = env_or("API_PORT", "8080").parse::<u16>().unwrap_or(8080);
        Config {
            platform_name: PLATFORM_NAME.to_string(),
            database_path: env_or("DATABASE_PATH", &format!("./data/{PLATFORM_NAME}.db")),
            api_port,
            posthog_api_key: env_or("POSTHOG_API_KEY", ""),
            ga_id: env_or("GA_ID", ""),
            ads_id: env_or("ADS_ID", ""),
            auth_enabled: env_flag("AUTH_ENABLED"),
            oidc_issuer: env_or("OIDC_ISSUER", ""),
            oidc_client_id: env_or("OIDC_CLIENT_ID", ""),
            oidc_client_secret: env_or("OIDC_CLIENT_SECRET", ""),
            session_signing_key: env_or("SESSION_SIGNING_KEY", ""),
            app_url: env_or("APP_URL", &format!("http://localhost:{api_port}")),
            cors_origin: env_or("CORS_ORIGIN", ""),
            billing_enabled: env_flag("BILLING_ENABLED"),
            stripe_secret_key: env_or("STRIPE_SECRET_KEY", ""),
            stripe_webhook_secret: env_or("STRIPE_WEBHOOK_SECRET", ""),
            stripe_price_id: env_or("STRIPE_PRICE_ID", ""),
            api_keys_enabled: env_flag("API_KEYS_ENABLED"),
            dev_user_id: env_or("DEV_USER_ID", ""),
        }
    }
}

/// Go getEnv semantics: an unset OR empty variable falls back.
fn env_or(key: &str, fallback: &str) -> String {
    match env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => fallback.to_string(),
    }
}

/// Boolean flags are fail-closed: only the exact string "true" enables them.
fn env_flag(key: &str) -> bool {
    env::var(key).map(|v| v == "true").unwrap_or(false)
}
