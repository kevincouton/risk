//! risk server — port of go-service/cmd/server/main.go wiring.
//! Shell glue follows SERVER_SHELL.md idiom 1 (server bootstrap/listen:
//! `topcoat::serve(listener, router)`) and idiom 2 (handler/response mapping);
//! the catch-all mount uses the same verified TowerRoute idiom as the static
//! mount, with a tower::service_fn in place of ServeDir (topcoat-router's own
//! documented example). Route table, flag gating, and middleware order live in
//! `server::handlers::dispatch` — everything below this file's mapping is final.

use std::convert::Infallible;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use chassis::auth::AuthProvider;
use chassis::db::SharedDb;
use server::handlers::{self, ApiResponse, AppState};
use topcoat::router::{
    to_bytes, tower::TowerRoute, Body, Compression, Methods, Path, Request, Response, Router,
};

/// Go's webhook body cap (io.LimitReader 1<<20); applied uniformly.
const BODY_LIMIT: usize = 1 << 20;

#[tokio::main]
#[cfg_attr(feature = "hotpath", hotpath::main)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    run(chassis::config::Config::load()).await
}

/// Full startup sequence, extracted so the integration surface (`main`) is a
/// thin wrapper and the individual helpers below are testable.
async fn run(cfg: chassis::config::Config) -> anyhow::Result<()> {
    let port = cfg.api_port;
    let (_state, app) = prepare_app(cfg).await?;
    serve_app(app, port).await
}

/// Build the shared state and router. Separated from `run` so the wire-up
/// (database, auth, app) can be unit-tested without binding a TCP port.
async fn prepare_app(cfg: chassis::config::Config) -> anyhow::Result<(Arc<AppState>, Router)> {
    let db = setup_database(&cfg)?;
    spawn_prune_task(db.clone());
    chassis::analytics::init(&cfg.posthog_api_key);
    let auth = setup_auth(&cfg, db.clone()).await?;

    let state = Arc::new(AppState { cfg, db, auth });
    let app = build_app(state.clone());
    Ok((state, app))
}

/// Bind the TCP listener and run the topcoat server.
async fn serve_app(app: Router, port: u16) -> anyhow::Result<()> {
    tracing::info!("risk server listening on :{port}");
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
    topcoat::serve(listener, app).await?;
    Ok(())
}

/// Open (and create parent directory for) the shared database, run migrations,
/// and prune stale api_usage rows at startup.
fn setup_database(cfg: &chassis::config::Config) -> anyhow::Result<SharedDb> {
    if let Some(parent) = std::path::Path::new(&cfg.database_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = chassis::db::open_shared(&cfg.database_path)?;
    {
        let conn = db.lock().expect("db mutex poisoned");
        chassis::db::migrate(&conn)?;
        // delta 12: prune api_usage at startup (90-day retention).
        let pruned = chassis::apikeys::prune_usage(&conn, 90)?;
        if pruned > 0 {
            tracing::info!(pruned, "api_usage pruned at startup");
        }
    }
    Ok(db)
}

/// Auth is opt-in and fail-closed (Go main.go): any misconfiguration
/// disables auth entirely while read-only endpoints keep serving.
fn auth_should_attempt(cfg: &chassis::config::Config) -> bool {
    if !cfg.auth_enabled {
        return false;
    }
    if cfg.session_signing_key.len() < 32 {
        tracing::warn!("auth: SESSION_SIGNING_KEY must be at least 32 bytes, auth disabled");
        return false;
    }
    true
}

async fn setup_auth(
    cfg: &chassis::config::Config,
    db: SharedDb,
) -> anyhow::Result<Option<AuthProvider>> {
    if !auth_should_attempt(cfg) {
        return Ok(None);
    }
    match AuthProvider::discover(cfg, db).await {
        Ok(p) => Ok(Some(p)),
        Err(e) => {
            tracing::warn!("auth: OIDC discovery failed, auth disabled: {e}");
            Ok(None)
        }
    }
}

/// Spawn the daily api_usage re-prune task (DB work in spawn_blocking per the
/// spine sync/async rule — never hold the lock across .await).
fn spawn_prune_task(db: SharedDb) {
    tokio::spawn(prune_forever(db));
}

async fn prune_forever(db: SharedDb) {
    let mut tick = tokio::time::interval(Duration::from_secs(24 * 3600));
    tick.tick().await; // first tick fires immediately; skip it
    loop {
        tick.tick().await;
        prune_once(db.clone()).await;
    }
}

async fn prune_once(db: SharedDb) {
    let result = tokio::task::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        chassis::apikeys::prune_usage(&conn, 90)
    })
    .await;
    match result {
        Ok(Ok(pruned)) => tracing::info!(pruned, "api_usage daily prune complete"),
        Ok(Err(e)) => tracing::warn!("api_usage daily prune failed: {e}"),
        Err(e) => tracing::warn!("api_usage daily prune task failed: {e}"),
    }
}

/// Build the catch-all router. One tower service mounted at "/" and
/// "/{*rest}" (a catch-all segment does not match the bare prefix — both
/// registrations are required).
fn build_app(state: Arc<AppState>) -> Router {
    let svc = {
        let state = state.clone();
        tower::service_fn(move |req: Request| {
            let state = state.clone();
            async move { Ok::<_, Infallible>(handle(state, req).await) }
        })
    };
    Router::builder()
        // Go parity: Go never compresses and never emits Vary; topcoat's
        // default compression layer adds `Vary: accept-encoding` to every
        // response, which the R-1 goldens pin as absent. Off.
        .compression(Compression::off())
        .route(TowerRoute::new(Methods::Any, Path::new("/"), svc.clone()))
        .route(TowerRoute::new(Methods::Any, Path::new("/{*rest}"), svc))
        .build()
}

// SERVER_SHELL.md idiom 2: Request/Response mapping only.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn handle(state: Arc<AppState>, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query: Vec<(String, String)> = req
        .uri()
        .query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    let query_refs: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let body = match to_bytes(req.into_body(), BODY_LIMIT).await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            tracing::warn!("request body read failed: {e}");
            let r = ApiResponse::http_error(400, "read error\n"); // Go webhook's read-error shape
            return map_response(r);
        }
    };
    let resp: ApiResponse =
        handlers::dispatch(&state, &method, &path, &query_refs, &header_refs, &body).await;
    map_response(resp)
}

fn map_response(resp: ApiResponse) -> Response {
    let mut builder = Response::builder().status(resp.status);
    for (k, v) in &resp.headers {
        builder = builder.header(k, v);
    }
    builder
        .body(Body::from(resp.body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg(db_path: &std::path::Path) -> chassis::config::Config {
        chassis::config::Config {
            platform_name: "risk".to_string(),
            database_path: db_path.to_str().unwrap().to_string(),
            api_port: 0,
            posthog_api_key: String::new(),
            ga_id: String::new(),
            ads_id: String::new(),
            auth_enabled: false,
            oidc_issuer: String::new(),
            oidc_client_id: String::new(),
            oidc_client_secret: String::new(),
            session_signing_key: String::new(),
            app_url: String::new(),
            cors_origin: String::new(),
            billing_enabled: false,
            stripe_secret_key: String::new(),
            stripe_webhook_secret: String::new(),
            stripe_price_id: String::new(),
            api_keys_enabled: false,
            dev_user_id: String::new(),
        }
    }

    #[test]
    fn setup_database_creates_and_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(&dir.path().join("test.db"));
        let db = setup_database(&cfg).unwrap();
        let conn = db.lock().expect("db mutex poisoned");
        // Verify the schema was applied by running a simple query.
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count > 0);
    }

    #[test]
    fn auth_should_attempt_disabled() {
        let cfg = chassis::config::Config {
            auth_enabled: false,
            session_signing_key: "x".repeat(32),
            ..test_cfg(std::path::Path::new("/dev/null"))
        };
        assert!(!auth_should_attempt(&cfg));
    }

    #[test]
    fn auth_should_attempt_short_key() {
        let cfg = chassis::config::Config {
            auth_enabled: true,
            session_signing_key: "short".to_string(),
            ..test_cfg(std::path::Path::new("/dev/null"))
        };
        assert!(!auth_should_attempt(&cfg));
    }

    #[test]
    fn auth_should_attempt_ok() {
        let cfg = chassis::config::Config {
            auth_enabled: true,
            session_signing_key: "x".repeat(32),
            ..test_cfg(std::path::Path::new("/dev/null"))
        };
        assert!(auth_should_attempt(&cfg));
    }

    #[tokio::test]
    async fn setup_auth_disabled_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(&dir.path().join("test.db"));
        let db = setup_database(&cfg).unwrap();
        let auth = setup_auth(&cfg, db).await.unwrap();
        assert!(auth.is_none());
    }

    #[tokio::test]
    async fn handle_healthz_ok() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(&dir.path().join("test.db"));
        let db = setup_database(&cfg).unwrap();
        let state = Arc::new(AppState {
            cfg,
            db,
            auth: None,
        });
        let req = Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = handle(state, req).await;
        assert_eq!(resp.status(), 200);
        let bytes = to_bytes(resp.into_body(), BODY_LIMIT).await.unwrap();
        assert_eq!(bytes.as_ref(), b"{\"status\":\"ok\"}\n");
    }

    #[test]
    fn map_response_preserves_headers_and_status() {
        let resp = ApiResponse {
            status: 418,
            headers: vec![("x-custom".to_string(), "yes".to_string())],
            body: b"tea".to_vec(),
        };
        let mapped = map_response(resp);
        assert_eq!(mapped.status(), 418);
        assert_eq!(mapped.headers().get("x-custom").unwrap(), "yes");
    }

    #[tokio::test]
    async fn prune_once_runs_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(&dir.path().join("test.db"));
        let db = setup_database(&cfg).unwrap();
        prune_once(db).await;
    }

    #[tokio::test]
    async fn prepare_app_builds_state_and_router() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(&dir.path().join("test.db"));
        let (state, _app) = prepare_app(cfg).await.unwrap();
        assert!(state.db.lock().is_ok());
        assert!(state.auth.is_none());
        // Router is opaque, but the AppState wiring is the critical part.
    }
}
