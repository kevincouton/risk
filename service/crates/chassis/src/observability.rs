//! Sentry error capture (Sentry-compatible, works with GlitchTip too).
//! No-op when SENTRY_DSN is empty — the same env-gating idiom as
//! analytics::init, so dev/CI builds ship no reporter.

/// Initialize the Sentry client and return a guard to hold for the process
/// lifetime (dropping it flushes in-flight events).
///
/// `release` tags every event with a release/version; when `None`, the
/// SENTRY_RELEASE env var is used if set. `traces_sample_rate: 1.0` is the
/// Sentry docs' recommended default for a new setup (errors + tracing).
pub fn init(dsn: &str, release: Option<String>) -> Option<sentry::ClientInitGuard> {
    if dsn.is_empty() {
        return None;
    }
    let release = release
        .or_else(|| std::env::var("SENTRY_RELEASE").ok())
        .filter(|r| !r.is_empty())
        .map(std::borrow::Cow::Owned);
    let dsn: sentry::types::Dsn = match dsn.parse() {
        Ok(dsn) => dsn,
        Err(e) => {
            tracing::warn!("sentry: invalid SENTRY_DSN, error capture disabled: {e}");
            return None;
        }
    };
    tracing::info!("sentry: error capture enabled");
    let mut options = sentry::ClientOptions::default();
    options.dsn = Some(dsn);
    options.release = release;
    // Sentry docs' recommended default for a new setup: errors + tracing.
    options.traces_sampling_strategy = sentry::TracesSamplingStrategy::FixedRate(1.0);
    Some(sentry::init(options))
}
