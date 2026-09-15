//! risk server — port of go-service/cmd/server/main.go wiring.
//! Shell glue follows SERVER_SHELL.md idiom 1 (server bootstrap/listen:
//! `topcoat::serve(listener, router)`) and idiom 2 (handler/response mapping);
//! the catch-all mount uses the same verified TowerRoute idiom as the static
//! mount, with a tower::service_fn in place of ServeDir (topcoat-router's own
//! documented example). Route table, flag gating, and middleware order live in
//! `server::handlers::dispatch` — everything below this file's mapping is final.

use std::convert::Infallible;
use std::sync::Arc;

use server::handlers::{self, ApiResponse, AppState};
use topcoat::router::{
    to_bytes, tower::TowerRoute, Body, Compression, Methods, Path, Request, Response, Router,
};

/// Go's webhook body cap (io.LimitReader 1<<20); applied uniformly.
const BODY_LIMIT: usize = 1 << 20;

fn init_db(cfg: &chassis::config::Config) -> anyhow::Result<chassis::db::SharedDb> {
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

/// delta 12: one prune tick; extracted so it can be unit-tested.
async fn prune_once(db: chassis::db::SharedDb) {
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

/// delta 12: daily re-prune on a tokio interval (DB work in spawn_blocking
/// per the spine sync/async rule — never hold the lock across .await).
fn start_prune_task(db: chassis::db::SharedDb) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
        tick.tick().await; // first tick fires immediately; skip it
        loop {
            tick.tick().await;
            prune_once(db.clone()).await;
        }
    });
}

/// Auth is opt-in and fail-closed (Go main.go): any misconfiguration
/// disables auth entirely while read-only endpoints keep serving.
async fn init_auth(
    cfg: &chassis::config::Config,
    db: chassis::db::SharedDb,
) -> Option<chassis::auth::AuthProvider> {
    if !cfg.auth_enabled {
        return None;
    }
    if cfg.session_signing_key.len() < 32 {
        tracing::warn!("auth: SESSION_SIGNING_KEY must be at least 32 bytes, auth disabled");
        return None;
    }
    match chassis::auth::AuthProvider::discover(cfg, db).await {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("auth: OIDC discovery failed, auth disabled: {e}");
            None
        }
    }
}

// SERVER_SHELL.md idiom 1: application construction. One catch-all
// tower service mounted at "/" and "/{*rest}" (a catch-all segment does not
// match the bare prefix — both registrations are required).
fn build_router(state: Arc<AppState>) -> Router {
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

async fn run_server(cfg: &chassis::config::Config, app: Router) -> anyhow::Result<()> {
    let port = cfg.api_port;
    tracing::info!("risk server listening on :{port}");
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?;
    topcoat::serve(listener, app).await?;
    Ok(())
}

#[tokio::main]
#[cfg_attr(feature = "hotpath", hotpath::main)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cfg = chassis::config::Config::load();
    let db = init_db(&cfg)?;
    chassis::analytics::init(&cfg.posthog_api_key);
    start_prune_task(db.clone());
    let auth = init_auth(&cfg, db.clone()).await;
    let state = Arc::new(AppState { cfg, db, auth });
    let app = build_router(state.clone());
    run_server(&state.cfg, app).await
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
            platform_name: "test".into(),
            database_path: db_path.to_string_lossy().into(),
            api_port: 0,
            posthog_api_key: "".into(),
            ga_id: "".into(),
            ads_id: "".into(),
            auth_enabled: false,
            oidc_issuer: "".into(),
            oidc_client_id: "".into(),
            oidc_client_secret: "".into(),
            session_signing_key: "".into(),
            app_url: "http://localhost:8080".into(),
            cors_origin: "".into(),
            billing_enabled: false,
            stripe_secret_key: "".into(),
            stripe_webhook_secret: "".into(),
            stripe_price_id: "".into(),
            api_keys_enabled: false,
            dev_user_id: "".into(),
        }
    }

    #[test]
    fn init_db_creates_migrates_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sub/db.sqlite");
        let cfg = test_cfg(&db_path);
        let db = init_db(&cfg).unwrap();
        assert!(db_path.exists());
        let conn = db.lock().unwrap();
        // Migration leaves the entities table.
        conn.execute("SELECT 1 FROM entities LIMIT 1", []).unwrap();
    }

    #[tokio::test]
    async fn init_auth_disabled_returns_none() {
        let cfg = chassis::config::Config {
            auth_enabled: false,
            ..test_cfg(std::path::Path::new("./test.db"))
        };
        let (_d, db) = {
            let dir = tempfile::tempdir().unwrap();
            let db =
                chassis::db::open_shared(dir.path().join("db.sqlite").to_str().unwrap()).unwrap();
            (dir, db)
        };
        assert!(init_auth(&cfg, db).await.is_none());
    }

    #[test]
    fn build_router_constructs_app() {
        let cfg = test_cfg(std::path::Path::new("./test.db"));
        let state = Arc::new(AppState {
            cfg,
            db: chassis::db::open_shared(":memory:").unwrap(),
            auth: None,
        });
        let _app = build_router(state);
    }

    #[tokio::test]
    async fn prune_once_runs_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let db = chassis::db::open_shared(dir.path().join("db.sqlite").to_str().unwrap()).unwrap();
        {
            let conn = db.lock().unwrap();
            chassis::db::migrate(&conn).unwrap();
        }
        prune_once(db).await;
    }
}
