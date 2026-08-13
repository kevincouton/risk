//! Route semantics ported 1:1 from go-service/cmd/server/main.go.
//! Everything here is topcoat-independent: `dispatch` is a plain async fn over
//! primitives, and the bin layer only maps the wire onto it. Status codes,
//! body bytes, SQL, and JSON shapes in this file are FINAL.

use chassis::apikeys;
use chassis::auth::{self, AuthProvider};
use chassis::billing;
use chassis::db::SharedDb;
use rusqlite::params;

pub struct AppState {
    pub cfg: chassis::config::Config,
    pub db: SharedDb,
    pub auth: Option<AuthProvider>,
}

/// Wire response. `body` is bytes so the SPA can serve binary assets.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ApiResponse {
    fn build(status: u16, headers: &[(&str, &str)], body: &str) -> ApiResponse {
        ApiResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }
    /// Go http.Error: text/plain; charset=utf-8 + X-Content-Type-Options: nosniff (+ \n in the body string).
    pub fn http_error(status: u16, body: &str) -> ApiResponse {
        ApiResponse::build(
            status,
            &[
                ("Content-Type", "text/plain; charset=utf-8"),
                ("X-Content-Type-Options", "nosniff"),
            ],
            body,
        )
    }
    /// Handler set Content-Type: application/json explicitly (Go w.Header().Set before Encode/Write).
    pub fn json(status: u16, body: String) -> ApiResponse {
        ApiResponse::build(status, &[("Content-Type", "application/json")], &body)
    }
    /// Go auto-detect for bodies written without a Content-Type (health).
    pub fn sniffed(status: u16, body: &str) -> ApiResponse {
        ApiResponse::build(
            status,
            &[("Content-Type", "text/plain; charset=utf-8")],
            body,
        )
    }
    pub fn header(mut self, k: &str, v: &str) -> ApiResponse {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }
}

fn qget<'a>(query: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    query.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn hget<'a>(headers: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
}

fn parse_cookies(header: &str) -> Vec<(String, String)> {
    header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Go main.go EntityResponse. `omitempty` → serde skip_serializing_if.
#[derive(Debug, serde::Serialize)]
pub struct EntityResponse {
    pub id: String,
    pub platform: String,
    pub full_name: String,
    pub description: String,
    pub category: String,
    pub score_value: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verdict: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub trajectory: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub composite_score: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn map_entity_row(r: &rusqlite::Row) -> rusqlite::Result<EntityResponse> {
    // Go: rows.Scan error (NULL description/category) → `continue` (row skipped).
    Ok(EntityResponse {
        id: r.get(0)?,
        platform: r.get(1)?,
        full_name: r.get(2)?,
        description: r.get(3)?,
        category: r.get(4)?,
        score_value: r.get(5)?,
        verdict: String::new(),
        trajectory: String::new(),
        composite_score: 0,
    })
}

/// Go: empty result list serializes as "entities":null (nil slice), and
/// json.NewEncoder appends a trailing newline.
fn entities_json(entities: &[EntityResponse]) -> serde_json::Value {
    if entities.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::to_value(entities).expect("EntityResponse serialization is infallible")
    }
}

pub fn health() -> ApiResponse {
    // Go: json.NewEncoder without Content-Type → sniffed text/plain, trailing \n.
    ApiResponse::sniffed(200, "{\"status\":\"ok\"}\n")
}

/// Go handleEntities: limit default 50, clamp 1..=200 (invalid → 50);
/// optional category filter; per-entity latest-score subquery (errors ignored).
pub fn entities(db: &SharedDb, query: &[(&str, &str)]) -> ApiResponse {
    let mut limit: i64 = 50;
    if let Some(l) = qget(query, "limit") {
        if let Ok(v) = l.parse::<i64>() {
            if v > 0 && v <= 200 {
                limit = v;
            }
        }
    }
    let category = qget(query, "category").unwrap_or("");
    let conn = db.lock().expect("db mutex poisoned");
    let mut sql = String::from(
        "SELECT id, platform, full_name, description, category, score_value FROM entities",
    );
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if !category.is_empty() {
        sql.push_str(" WHERE category = ?");
        args.push(rusqlite::types::Value::Text(category.to_string()));
    }
    sql.push_str(" ORDER BY score_value DESC LIMIT ?");
    args.push(rusqlite::types::Value::Integer(limit));
    let mut entities: Vec<EntityResponse> = Vec::new();
    {
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => return ApiResponse::http_error(500, &format!("{e}\n")),
        };
        let mapped = match stmt.query_map(rusqlite::params_from_iter(args), map_entity_row) {
            Ok(m) => m,
            Err(e) => return ApiResponse::http_error(500, &format!("{e}\n")),
        };
        for e in mapped.flatten() {
            // Err skipped → Go `continue`
            entities.push(e);
        }
    }
    for e in &mut entities {
        // Go: latest-score subquery; error ignored; NULL composite/verdict/trajectory leave zero values.
        let _ = conn.query_row(
            "SELECT composite_score, verdict, trajectory FROM entity_scores WHERE entity_id = ? ORDER BY scored_at DESC LIMIT 1",
            params![e.id],
            |r| {
                e.composite_score = r.get::<_, Option<i64>>(0)?.unwrap_or(0);
                e.verdict = r.get::<_, Option<String>>(1)?.unwrap_or_default();
                e.trajectory = r.get::<_, Option<String>>(2)?.unwrap_or_default();
                Ok(())
            },
        );
    }
    let body = serde_json::json!({ "entities": entities_json(&entities), "limit": limit, "total": entities.len() });
    ApiResponse::json(200, format!("{body}\n"))
}

/// Go handleEntityDetail: {owner}/{repo} after the prefix; platform query
/// default "default"; metadata scanned as string (NULL → 404); invalid
/// metadata JSON → "raw_metadata":null.
pub fn entity_detail(db: &SharedDb, path: &str, query: &[(&str, &str)]) -> ApiResponse {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 2 {
        return ApiResponse::http_error(400, "invalid path\n");
    }
    let platform = qget(query, "platform").unwrap_or("default");
    let full_name = format!("{}/{}", parts[0], parts[1]);
    let conn = db.lock().expect("db mutex poisoned");
    let row = conn.query_row(
        "SELECT id, platform, full_name, description, category, score_value, metadata FROM entities WHERE platform = ? AND full_name = ?",
        params![platform, full_name],
        |r| Ok((map_entity_row(r)?, r.get::<_, String>(6)?)),
    );
    let (mut e, raw_meta) = match row {
        Ok(v) => v,
        Err(_) => return ApiResponse::http_error(404, "not found\n"),
    };
    let _ = conn.query_row(
        "SELECT composite_score, verdict, trajectory FROM entity_scores WHERE entity_id = ? ORDER BY scored_at DESC LIMIT 1",
        params![e.id],
        |r| {
            e.composite_score = r.get::<_, Option<i64>>(0)?.unwrap_or(0);
            e.verdict = r.get::<_, Option<String>>(1)?.unwrap_or_default();
            e.trajectory = r.get::<_, Option<String>>(2)?.unwrap_or_default();
            Ok(())
        },
    );
    // Go: json.Unmarshal error ignored → meta nil → "raw_metadata":null.
    let meta =
        serde_json::from_str::<serde_json::Value>(&raw_meta).unwrap_or(serde_json::Value::Null);
    let entity = serde_json::to_value(&e).expect("EntityResponse serialization is infallible");
    let body = serde_json::json!({ "entity": entity, "raw_metadata": meta });
    ApiResponse::json(200, format!("{body}\n"))
}

/// Go handleSearch: trimmed q required; LIKE %q% on full_name/description;
/// LIMIT 50; latest-score subquery WITHOUT trajectory.
pub fn search(db: &SharedDb, query: &[(&str, &str)]) -> ApiResponse {
    let q = qget(query, "q").unwrap_or("").trim().to_string();
    if q.is_empty() {
        // Byte-exact Go body: `{"error": "missing q parameter"}` + http.Error newline.
        return ApiResponse::http_error(400, "{\"error\": \"missing q parameter\"}\n");
    }
    let like = format!("%{q}%");
    let conn = db.lock().expect("db mutex poisoned");
    let mut entities: Vec<EntityResponse> = Vec::new();
    {
        let mut stmt = match conn.prepare(
            "SELECT id, platform, full_name, description, category, score_value
             FROM entities
             WHERE full_name LIKE ? OR description LIKE ?
             ORDER BY score_value DESC
             LIMIT 50",
        ) {
            Ok(s) => s,
            Err(e) => return ApiResponse::http_error(500, &format!("{e}\n")),
        };
        let mapped = match stmt.query_map(params![like, like], map_entity_row) {
            Ok(m) => m,
            Err(e) => return ApiResponse::http_error(500, &format!("{e}\n")),
        };
        for e in mapped.flatten() {
            entities.push(e);
        }
    }
    for e in &mut entities {
        let _ = conn.query_row(
            "SELECT composite_score, verdict FROM entity_scores WHERE entity_id = ? ORDER BY scored_at DESC LIMIT 1",
            params![e.id],
            |r| {
                e.composite_score = r.get::<_, Option<i64>>(0)?.unwrap_or(0);
                e.verdict = r.get::<_, Option<String>>(1)?.unwrap_or_default();
                Ok(())
            },
        );
    }
    let body = serde_json::json!({ "query": q, "entities": entities_json(&entities), "total": entities.len() });
    ApiResponse::json(200, format!("{body}\n"))
}

/// Go handleStats: counts + verdict histogram (errors ignored → zero values).
pub fn stats(db: &SharedDb) -> ApiResponse {
    let conn = db.lock().expect("db mutex poisoned");
    let total_entities: i64 = conn
        .query_row("SELECT count(*) FROM entities", [], |r| r.get(0))
        .unwrap_or(0);
    let total_scores: i64 = conn
        .query_row("SELECT count(*) FROM entity_scores", [], |r| r.get(0))
        .unwrap_or(0);
    let mut verdicts = std::collections::BTreeMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT verdict, count(*) FROM entity_scores GROUP BY verdict ORDER BY count(*) DESC",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        {
            for row in rows.flatten() {
                verdicts.insert(row.0, row.1);
            }
        }
    }
    let body = serde_json::json!({ "total_entities": total_entities, "total_scores": total_scores, "verdicts": verdicts });
    ApiResponse::json(200, format!("{body}\n"))
}

// ---- API-key gate (apikeys middleware.go, delta 2: key-auth-then-rate-limit,
// identity from authenticate's return only — X-API-Key-ID header is GONE) ----

pub struct GatePass {
    pub identity: apikeys::KeyIdentity,
    pub remaining: u32,
}

/// Go composition KeyAuth(conn)(RateLimit(conn, 60)(handler)):
/// 401 `{"error":"invalid or missing API key"}`; 500 `{"error":"rate limit check failed"}`
/// (delta 2 fail-closed); 429 `{"error":"rate limit exceeded"}` with
/// X-RateLimit-Remaining: 0 (Go set the header before the 429 branch).
pub fn gate_v1(
    db: &SharedDb,
    x_api_key: Option<&str>,
    path: &str,
    per_min: u32,
) -> Result<GatePass, ApiResponse> {
    let identity = {
        let conn = db.lock().expect("db mutex poisoned");
        apikeys::authenticate(&conn, x_api_key.unwrap_or(""))
    };
    let identity = match identity {
        Some(i) => i,
        None => {
            return Err(ApiResponse::http_error(
                401,
                "{\"error\":\"invalid or missing API key\"}\n",
            ))
        }
    };
    let allowed = {
        let conn = db.lock().expect("db mutex poisoned");
        apikeys::check_and_record(&conn, &identity.key_id, path, per_min)
    };
    match allowed {
        Err(_) => Err(ApiResponse::http_error(
            500,
            "{\"error\":\"rate limit check failed\"}\n",
        )),
        Ok(false) => Err(
            ApiResponse::http_error(429, "{\"error\":\"rate limit exceeded\"}\n")
                .header("X-RateLimit-Remaining", "0"),
        ),
        Ok(true) => {
            // Go: remaining = per_min - used (used counted AFTER the insert), clamped at 0.
            let used: i64 = {
                let conn = db.lock().expect("db mutex poisoned");
                conn.query_row(
                    "SELECT COUNT(*) FROM api_usage WHERE key_id = ? AND ts > datetime('now', '-60 seconds')",
                    params![identity.key_id],
                    |r| r.get(0),
                )
                .unwrap_or(0)
            };
            let remaining = (per_min as i64 - used).max(0) as u32;
            Ok(GatePass {
                identity,
                remaining,
            })
        }
    }
}

fn gated(
    state: &AppState,
    headers: &[(&str, &str)],
    path: &str,
    h: impl FnOnce(&AppState) -> ApiResponse,
) -> ApiResponse {
    if !state.cfg.api_keys_enabled {
        return h(state);
    }
    match gate_v1(&state.db, hget(headers, "X-API-Key"), path, 60) {
        Ok(pass) => h(state).header("X-RateLimit-Remaining", &pass.remaining.to_string()),
        Err(resp) => resp,
    }
}

// ---- auth routes ----

/// Go http.Redirect for GET: 302 + Location + an HTML body
/// (`<a href="...">Found</a>.\n\n`, Go's htmlReplacer escaping).
fn redirect_with_cookie(location: &str, cookie: &str) -> ApiResponse {
    ApiResponse {
        status: 302,
        headers: vec![
            (
                "Content-Type".to_string(),
                "text/html; charset=utf-8".to_string(),
            ),
            ("Location".to_string(), location.to_string()),
            ("Set-Cookie".to_string(), cookie.to_string()),
        ],
        body: format!("<a href=\"{}\">Found</a>.\n\n", html_escape(location)).into_bytes(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

pub fn auth_login(state: &AppState) -> ApiResponse {
    let p = state
        .auth
        .as_ref()
        .expect("auth routes are only routed with a provider");
    let start = p.login();
    redirect_with_cookie(&start.auth_url, &start.state_cookie)
}

pub async fn auth_callback(
    state: &AppState,
    cookies: &[(String, String)],
    query: &[(&str, &str)],
) -> ApiResponse {
    let p = state
        .auth
        .as_ref()
        .expect("auth routes are only routed with a provider");
    let state_cookie = cookies
        .iter()
        .find(|(k, _)| k == auth::STATE_COOKIE)
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    match p
        .callback(
            state_cookie,
            qget(query, "state").unwrap_or(""),
            qget(query, "code").unwrap_or(""),
        )
        .await
    {
        // Go redirects to "/" (NOT APP_URL) — verified in oidc.go HandleCallback.
        Ok(ok) => redirect_with_cookie("/", &ok.session_cookie),
        Err(auth::CallbackError::InvalidState) => {
            ApiResponse::http_error(400, "invalid oauth state\n")
        }
        Err(auth::CallbackError::Exchange(_)) => {
            ApiResponse::http_error(502, "token exchange failed\n")
        }
        Err(auth::CallbackError::Upsert(_)) => ApiResponse::http_error(500, "user upsert failed\n"),
    }
}

pub fn auth_me(state: &AppState, session_cookie: Option<&str>) -> ApiResponse {
    let p = state
        .auth
        .as_ref()
        .expect("auth routes are only routed with a provider");
    let user = session_cookie.and_then(|c| p.current_user(&state.db, c));
    match user {
        None => ApiResponse::http_error(401, "{\"error\":\"unauthenticated\"}\n"),
        Some(u) => {
            // Go User JSON: oidc_sub skipped; empty strings (not null) for absent
            // email/name; "groups":null when empty (Go nil slice); trailing \n.
            let groups = if u.groups.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::to_value(&u.groups).expect("groups serialization is infallible")
            };
            let body = serde_json::json!({
                "id": u.id,
                "email": u.email.unwrap_or_default(),
                "display_name": u.display_name.unwrap_or_default(),
                "groups": groups,
                "premium": u.premium,
            });
            ApiResponse::json(200, format!("{body}\n"))
        }
    }
}

pub fn auth_logout(state: &AppState, method: &str) -> ApiResponse {
    let _ = state;
    if method != "POST" {
        // delta 3: POST-only; GET → 405.
        return ApiResponse::http_error(405, "{\"error\":\"method not allowed\"}\n");
    }
    // Go: w.Header().Set("Content-Type", "application/json") + w.Write(`{"ok":true}`)
    // — explicit JSON content type, no trailing newline. (The brief's fact list
    // claimed logout was sniffed text/plain; the Go source sets it explicitly,
    // and Go wins for anything observable over HTTP.)
    ApiResponse::json(200, "{\"ok\":true}".to_string())
        .header("Set-Cookie", &auth::clear_session_cookie())
}

// ---- billing routes ----

pub fn billing_webhook(state: &AppState, stripe_signature: &str, payload: &[u8]) -> ApiResponse {
    let out = billing::handle_webhook(
        &state.db,
        &state.cfg.stripe_webhook_secret,
        payload,
        stripe_signature,
    );
    if out.status == 200 && out.body.is_empty() {
        return ApiResponse::build(200, &[], ""); // Go: WriteHeader(200), no body
    }
    if out.status == 200 {
        return ApiResponse::json(200, out.body); // delta 6 {"ignored":true}
    }
    ApiResponse::http_error(out.status, &out.body) // Go http.Error bodies
}

/// Go checkout handler, delta 5: the `?user_id=` query-param fallback is GONE.
/// Resolution order: session user → DEV_USER_ID → 401 `{"error":"authentication required"}`.
/// (Deviation from Go's RequireAuth wrap, intended by delta 5: with a provider
/// configured AND DEV_USER_ID set AND no session, Go 401'd; Rust honors the
/// explicit dev override. The 401 body is byte-identical either way.)
pub async fn billing_checkout(state: &AppState, session_cookie: Option<&str>) -> ApiResponse {
    let mut user_id = String::new();
    if let (Some(p), Some(c)) = (state.auth.as_ref(), session_cookie) {
        if let Some(u) = p.current_user(&state.db, c) {
            user_id = u.id;
        }
    }
    if user_id.is_empty() && !state.cfg.dev_user_id.is_empty() {
        user_id = state.cfg.dev_user_id.clone();
    }
    if user_id.is_empty() {
        return ApiResponse::http_error(401, "{\"error\":\"authentication required\"}\n");
    }
    match billing::create_checkout_session(&state.cfg, &user_id).await {
        // Go: Content-Type application/json + w.Write — no trailing newline.
        Ok(url) => ApiResponse::json(200, serde_json::json!({ "url": url }).to_string()),
        Err(_) => ApiResponse::http_error(502, "{\"error\":\"checkout failed\"}\n"),
    }
}

// ---- /api/keys routes (require api_keys_enabled && auth provider) ----

fn require_user(state: &AppState, session_cookie: Option<&str>) -> Result<auth::User, ApiResponse> {
    let p = state
        .auth
        .as_ref()
        .expect("keys routes are only routed with a provider");
    match session_cookie.and_then(|c| p.current_user(&state.db, c)) {
        Some(u) => Ok(u),
        None => Err(ApiResponse::http_error(
            401,
            "{\"error\":\"authentication required\"}\n",
        )),
    }
}

pub fn keys_list_or_create(
    state: &AppState,
    method: &str,
    session_cookie: Option<&str>,
    body: &[u8],
) -> ApiResponse {
    // Go wraps the whole handler in RequireAuth: 401 precedes the method switch.
    let user = match require_user(state, session_cookie) {
        Ok(u) => u,
        Err(r) => return r,
    };
    match method {
        "GET" => {
            let conn = state.db.lock().expect("db mutex poisoned");
            match apikeys::list_keys(&conn, &user.id) {
                Ok(keys) => {
                    // Go: nil slice → "keys":null; json.NewEncoder trailing \n.
                    // Serialize a struct (declaration field order) rather than
                    // serde_json::Value, whose BTreeMap would sort each key's
                    // fields alphabetically — the keys_list golden pins Go's
                    // struct order id,label,created_at,revoked byte-exactly.
                    #[derive(serde::Serialize)]
                    struct KeysResponse<'a> {
                        keys: Option<&'a [apikeys::KeyInfo]>,
                    }
                    let body = serde_json::to_string(&KeysResponse {
                        keys: if keys.is_empty() { None } else { Some(&keys) },
                    })
                    .expect("KeyInfo serialization is infallible");
                    ApiResponse::json(200, format!("{body}\n"))
                }
                Err(_) => ApiResponse::http_error(500, "{\"error\":\"list failed\"}\n"),
            }
        }
        "POST" => {
            #[derive(serde::Deserialize)]
            struct Req {
                #[serde(default)]
                label: String,
            }
            // Go: json decode error ignored → empty label.
            let req: Req = serde_json::from_slice(body).unwrap_or(Req {
                label: String::new(),
            });
            let conn = state.db.lock().expect("db mutex poisoned");
            match apikeys::create_key(&conn, &user.id, &req.label) {
                Ok(plaintext) => ApiResponse::json(
                    200,
                    format!("{}\n", serde_json::json!({ "key": plaintext })),
                ),
                Err(_) => ApiResponse::http_error(500, "{\"error\":\"create failed\"}\n"),
            }
        }
        _ => ApiResponse::http_error(405, "{\"error\":\"method not allowed\"}\n"),
    }
}

pub fn keys_revoke(
    state: &AppState,
    method: &str,
    key_id: &str,
    session_cookie: Option<&str>,
) -> ApiResponse {
    let user = match require_user(state, session_cookie) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if method != "DELETE" {
        return ApiResponse::http_error(405, "{\"error\":\"method not allowed\"}\n");
    }
    let conn = state.db.lock().expect("db mutex poisoned");
    match apikeys::revoke_key(&conn, key_id, &user.id) {
        // Go: Content-Type application/json + w.Write(`{"ok":true}`) — no newline.
        Ok(()) => ApiResponse::json(200, "{\"ok\":true}".to_string()),
        Err(_) => ApiResponse::http_error(404, "{\"error\":\"revoke failed\"}\n"),
    }
}

// ---- dispatch: CORS outermost (delta 1) → analytics (skip /healthz and
// non-/api/) → routes. Routes exist ONLY when their flag is on (fail-closed,
// Go main.go); absent routes fall through to the SPA, which 404s /api//auth
// paths exactly like Go's file server did. ----

pub async fn dispatch(
    state: &AppState,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
    body: &[u8],
) -> ApiResponse {
    let cors = crate::cors::cors_headers(
        state.cfg.auth_enabled,
        &state.cfg.app_url,
        &state.cfg.cors_origin,
        method,
        hget(headers, "Origin"),
    );
    if cors.preflight {
        return cors.apply(ApiResponse::build(200, &[], ""));
    }
    let resp = route(state, method, path, query, headers, body).await;
    // Go analyticsMiddleware: skip /healthz and non-/api/ paths; status is
    // hardcoded 200 in Go (the measured duration is discarded) — replicated.
    if path != "/healthz" && path.starts_with("/api/") {
        chassis::analytics::capture_api_request(
            path,
            method,
            hget(headers, "User-Agent").unwrap_or(""),
            200,
        );
    }
    cors.apply(resp)
}

async fn route(
    state: &AppState,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
    body: &[u8],
) -> ApiResponse {
    let cookies = parse_cookies(hget(headers, "Cookie").unwrap_or(""));
    let session = cookies
        .iter()
        .find(|(k, _)| k == auth::SESSION_COOKIE)
        .map(|(_, v)| v.as_str());
    const V1_ENTITIES: &str = "/api/v1/entities";
    const V1_ENTITIES_PREFIX: &str = "/api/v1/entities/";
    const KEYS_PREFIX: &str = "/api/keys/";
    match path {
        "/healthz" => health(),
        V1_ENTITIES => gated(state, headers, path, |s| entities(&s.db, query)),
        p if p.starts_with(V1_ENTITIES_PREFIX) => gated(state, headers, path, |s| {
            entity_detail(&s.db, &p[V1_ENTITIES_PREFIX.len()..], query)
        }),
        "/api/v1/search" => gated(state, headers, path, |s| search(&s.db, query)),
        "/api/v1/stats" => gated(state, headers, path, |s| stats(&s.db)),
        "/auth/login" if state.auth.is_some() => auth_login(state),
        "/auth/callback" if state.auth.is_some() => auth_callback(state, &cookies, query).await,
        "/auth/logout" if state.auth.is_some() => auth_logout(state, method),
        "/auth/me" if state.auth.is_some() => auth_me(state, session),
        "/api/billing/webhook" if state.cfg.billing_enabled => {
            billing_webhook(state, hget(headers, "Stripe-Signature").unwrap_or(""), body)
        }
        "/api/billing/checkout" if state.cfg.billing_enabled => {
            billing_checkout(state, session).await
        }
        "/api/keys" if state.cfg.api_keys_enabled && state.auth.is_some() => {
            keys_list_or_create(state, method, session, body)
        }
        p if state.cfg.api_keys_enabled && state.auth.is_some() && p.starts_with(KEYS_PREFIX) => {
            keys_revoke(state, method, &p[KEYS_PREFIX.len()..], session)
        }
        _ => spa_response(path),
    }
}

fn spa_response(path: &str) -> ApiResponse {
    match crate::spa::serve(std::path::Path::new("./web/dist"), path) {
        Some((ct, bytes)) => ApiResponse {
            status: 200,
            headers: vec![("Content-Type".to_string(), ct)],
            body: bytes,
        },
        None => ApiResponse::build(
            404,
            &[
                ("Content-Type", "text/plain; charset=utf-8"),
                ("X-Content-Type-Options", "nosniff"),
            ],
            "404 page not found\n",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> (tempfile::TempDir, chassis::db::SharedDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let conn = chassis::db::open(path.to_str().unwrap()).unwrap();
            chassis::db::migrate(&conn).unwrap();
        }
        (
            dir,
            chassis::db::open_shared(path.to_str().unwrap()).unwrap(),
        )
    }

    fn seed_entity(
        db: &chassis::db::SharedDb,
        id: &str,
        full_name: &str,
        description: Option<&str>,
        score: i64,
    ) {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO entities (id, platform, slug, name, full_name, description, category, score_value)
             VALUES (?, 'default', ?, ?, ?, ?, 'cat', ?)",
            rusqlite::params![id, id, full_name, full_name, description, score],
        )
        .unwrap();
    }

    fn body_string(r: &ApiResponse) -> String {
        String::from_utf8(r.body.clone()).unwrap()
    }

    #[test]
    fn entities_limit_clamp_and_null_parity() {
        let (_d, db) = open_test_db();
        seed_entity(&db, "e1", "o/r1", Some("d"), 10);
        // Default 50; valid value honored.
        let r = entities(&db, &[]);
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert_eq!(v["limit"], 50);
        assert_eq!(v["total"], 1);
        let r = entities(&db, &[("limit", "5")]);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body_string(&r)).unwrap()["limit"],
            5
        );
        // Out-of-range / unparseable → default 50 (Go clamp 1..200).
        let r = entities(&db, &[("limit", "500")]);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body_string(&r)).unwrap()["limit"],
            50
        );
        let r = entities(&db, &[("limit", "abc")]);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body_string(&r)).unwrap()["limit"],
            50
        );
        // Empty result → "entities":null (Go nil slice), trailing newline (Go json.Encoder).
        let r = entities(&db, &[("category", "no-such")]);
        let b = body_string(&r);
        assert!(b.ends_with('\n'), "Go json.Encoder appends newline: {b:?}");
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert!(v["entities"].is_null(), "empty list must be null, got {b}");
        assert_eq!(v["total"], 0);
    }

    #[test]
    fn entities_skip_null_description_rows() {
        let (_d, db) = open_test_db();
        seed_entity(&db, "e1", "o/r1", None, 10); // NULL description → Go rows.Scan error → continue
        seed_entity(&db, "e2", "o/r2", Some("d"), 5);
        let r = entities(&db, &[]);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert_eq!(
            v["total"], 1,
            "NULL-description row must be skipped exactly as Go"
        );
        assert_eq!(v["entities"][0]["full_name"], "o/r2");
        // EntityResponse omitempty: no score row → no verdict/trajectory/composite_score keys.
        let e = &v["entities"][0];
        assert!(
            e.get("verdict").is_none()
                && e.get("trajectory").is_none()
                && e.get("composite_score").is_none(),
            "{e}"
        );
    }

    #[test]
    fn entity_detail_paths_and_null_metadata_404() {
        let (_d, db) = open_test_db();
        seed_entity(&db, "e1", "o/r1", Some("d"), 10); // metadata column is NULL
        let r = entity_detail(&db, "a/b/c", &[]);
        assert_eq!(r.status, 400);
        assert_eq!(body_string(&r), "invalid path\n");
        let r = entity_detail(&db, "no/such", &[]);
        assert_eq!(r.status, 404);
        assert_eq!(body_string(&r), "not found\n");
        // Go scans metadata into a string: NULL metadata fails the scan → 404.
        let r = entity_detail(&db, "o/r1", &[]);
        assert_eq!(r.status, 404, "NULL metadata → 404 exactly as Go");
        // With metadata set, detail succeeds; platform defaults to "default".
        db.lock()
            .unwrap()
            .execute(
                "UPDATE entities SET metadata = '{\"k\":1}' WHERE id = 'e1'",
                [],
            )
            .unwrap();
        let r = entity_detail(&db, "o/r1", &[]);
        assert_eq!(r.status, 200, "{}", body_string(&r));
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert_eq!(v["entity"]["full_name"], "o/r1");
        assert_eq!(v["raw_metadata"]["k"], 1);
        // Wrong platform → 404.
        let r = entity_detail(&db, "o/r1", &[("platform", "other")]);
        assert_eq!(r.status, 404);
    }

    #[test]
    fn search_and_stats_shapes() {
        let (_d, db) = open_test_db();
        seed_entity(&db, "e1", "o/rocket", Some("fast"), 10);
        // Missing q → 400, byte-exact Go body (note the space after the colon).
        let r = search(&db, &[]);
        assert_eq!(r.status, 400);
        assert_eq!(body_string(&r), "{\"error\": \"missing q parameter\"}\n");
        let r = search(&db, &[("q", "rocket")]);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["query"], "rocket");
        let r = search(&db, &[("q", "nope")]);
        assert!(
            serde_json::from_str::<serde_json::Value>(&body_string(&r)).unwrap()["entities"]
                .is_null()
        );
        let r = stats(&db);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert_eq!(v["total_entities"], 1);
        assert_eq!(v["total_scores"], 0);
        assert_eq!(v["verdicts"], serde_json::json!({}));
    }
}
