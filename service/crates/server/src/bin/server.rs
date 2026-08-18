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

#[tokio::main]
#[cfg_attr(feature = "hotpath", hotpath::main)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cfg = chassis::config::Config::load();
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
    chassis::analytics::init(&cfg.posthog_api_key);

    // delta 12: daily re-prune on a tokio interval (DB work in spawn_blocking
    // per the spine sync/async rule — never hold the lock across .await).
    {
        let db = db.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
            tick.tick().await; // first tick fires immediately; skip it
            loop {
                tick.tick().await;
                let db = db.clone();
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
        });
    }

    // Auth is opt-in and fail-closed (Go main.go): any misconfiguration
    // disables auth entirely while read-only endpoints keep serving.
    let auth = if cfg.auth_enabled {
        if cfg.session_signing_key.len() < 32 {
            tracing::warn!("auth: SESSION_SIGNING_KEY must be at least 32 bytes, auth disabled");
            None
        } else {
            match chassis::auth::AuthProvider::discover(&cfg, db.clone()).await {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!("auth: OIDC discovery failed, auth disabled: {e}");
                    None
                }
            }
        }
    } else {
        None
    };

    let state = Arc::new(AppState { cfg, db, auth });
    let port = state.cfg.api_port;

    // SERVER_SHELL.md idiom 1: application construction + listen. One catch-all
    // tower service mounted at "/" and "/{*rest}" (a catch-all segment does not
    // match the bare prefix — both registrations are required).
    let svc = {
        let state = state.clone();
        tower::service_fn(move |req: Request| {
            let state = state.clone();
            async move { Ok::<_, Infallible>(handle(state, req).await) }
        })
    };
    let app = Router::builder()
        // Go parity: Go never compresses and never emits Vary; topcoat's
        // default compression layer adds `Vary: accept-encoding` to every
        // response, which the R-1 goldens pin as absent. Off.
        .compression(Compression::off())
        .route(TowerRoute::new(Methods::Any, Path::new("/"), svc.clone()))
        .route(TowerRoute::new(Methods::Any, Path::new("/{*rest}"), svc))
        .build();
    tracing::info!("risk server listening on :{port}");
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?;
    topcoat::serve(listener, app).await?;
    Ok(())
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
