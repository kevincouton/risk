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

#[derive(Debug)]
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
async fn resolve_checkout_user(state: &AppState, session_cookie: Option<&str>) -> String {
    if let (Some(p), Some(c)) = (state.auth.as_ref(), session_cookie) {
        if let Some(u) = p.current_user(&state.db, c) {
            return u.id;
        }
    }
    if !state.cfg.dev_user_id.is_empty() {
        return state.cfg.dev_user_id.clone();
    }
    String::new()
}

pub async fn billing_checkout(state: &AppState, session_cookie: Option<&str>) -> ApiResponse {
    let user_id = resolve_checkout_user(state, session_cookie).await;
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

fn keys_list(state: &AppState, user: &auth::User) -> ApiResponse {
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

fn keys_create(state: &AppState, user: &auth::User, body: &[u8]) -> ApiResponse {
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
        "GET" => keys_list(state, &user),
        "POST" => keys_create(state, &user, body),
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

#[cfg_attr(feature = "hotpath", hotpath::measure)]
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

const V1_ENTITIES: &str = "/api/v1/entities";
const V1_ENTITIES_PREFIX: &str = "/api/v1/entities/";
const KEYS_PREFIX: &str = "/api/keys/";

/// Routes the public API v1 paths. These paths are always present; the gate
/// is applied inside each arm so the fail-closed behavior matches Go.
fn route_api_v1(
    state: &AppState,
    _method: &str,
    path: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
) -> Option<ApiResponse> {
    match path {
        V1_ENTITIES => Some(gated(state, headers, path, |s| entities(&s.db, query))),
        p if p.starts_with(V1_ENTITIES_PREFIX) => Some(gated(state, headers, path, |s| {
            entity_detail(&s.db, &p[V1_ENTITIES_PREFIX.len()..], query)
        })),
        "/api/v1/search" => Some(gated(state, headers, path, |s| search(&s.db, query))),
        "/api/v1/stats" => Some(gated(state, headers, path, |s| stats(&s.db))),
        _ => None,
    }
}

/// Auth routes exist only when a provider is configured. Unknown /auth/* paths
/// fall through to the SPA, matching Go's file-server behavior.
async fn route_auth(
    state: &AppState,
    method: &str,
    path: &str,
    session: Option<&str>,
    cookies: &[(String, String)],
    query: &[(&str, &str)],
) -> Option<ApiResponse> {
    if state.auth.is_none() {
        return None;
    }
    match path {
        "/auth/login" => Some(auth_login(state)),
        "/auth/callback" => Some(auth_callback(state, cookies, query).await),
        "/auth/logout" => Some(auth_logout(state, method)),
        "/auth/me" => Some(auth_me(state, session)),
        _ => None,
    }
}

/// Billing routes exist only when billing is enabled. Unknown /api/billing/*
/// paths fall through to the SPA.
async fn route_billing(
    state: &AppState,
    path: &str,
    session: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Option<ApiResponse> {
    if !state.cfg.billing_enabled {
        return None;
    }
    match path {
        "/api/billing/webhook" => Some(billing_webhook(
            state,
            hget(headers, "Stripe-Signature").unwrap_or(""),
            body,
        )),
        "/api/billing/checkout" => Some(billing_checkout(state, session).await),
        _ => None,
    }
}

/// /api/keys routes exist only when both API keys and auth are enabled.
/// Unknown /api/keys/* paths fall through to the SPA.
fn route_keys(
    state: &AppState,
    method: &str,
    path: &str,
    session: Option<&str>,
    body: &[u8],
) -> Option<ApiResponse> {
    if !state.cfg.api_keys_enabled || state.auth.is_none() {
        return None;
    }
    match path {
        "/api/keys" => Some(keys_list_or_create(state, method, session, body)),
        p if p.starts_with(KEYS_PREFIX) => {
            Some(keys_revoke(state, method, &p[KEYS_PREFIX.len()..], session))
        }
        _ => None,
    }
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
    if path == "/healthz" {
        return health();
    }
    if let Some(resp) = route_api_v1(state, method, path, query, headers) {
        return resp;
    }
    if let Some(resp) = route_auth(state, method, path, session, &cookies, query).await {
        return resp;
    }
    if let Some(resp) = route_billing(state, path, session, headers, body).await {
        return resp;
    }
    if let Some(resp) = route_keys(state, method, path, session, body) {
        return resp;
    }
    spa_response(path)
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

    // ---- test helpers for handler coverage ----

    fn test_config() -> chassis::config::Config {
        let mut cfg = chassis::config::Config::load();
        cfg.app_url = "http://localhost:8080".into();
        cfg.cors_origin = "http://localhost:3000".into();
        cfg
    }

    fn state_with_db(db: &chassis::db::SharedDb) -> AppState {
        AppState {
            cfg: test_config(),
            db: db.clone(),
            auth: None,
        }
    }

    fn test_signing_key() -> &'static [u8] {
        b"test-signing-key-32bytes-long!"
    }

    fn state_with_auth(db: &chassis::db::SharedDb) -> AppState {
        let mut cfg = test_config();
        cfg.auth_enabled = true;
        let provider = AuthProvider::new_for_test(
            Box::new(MockFlow::ok("state1")),
            test_signing_key(),
            db.clone(),
        );
        AppState {
            cfg,
            db: db.clone(),
            auth: Some(provider),
        }
    }

    fn state_with_billing(db: &chassis::db::SharedDb) -> AppState {
        let mut cfg = test_config();
        cfg.billing_enabled = true;
        cfg.stripe_webhook_secret = "whsec_test".into();
        AppState {
            cfg,
            db: db.clone(),
            auth: None,
        }
    }

    fn state_with_api_keys(db: &chassis::db::SharedDb) -> AppState {
        let mut cfg = test_config();
        cfg.api_keys_enabled = true;
        let provider = AuthProvider::new_for_test(
            Box::new(MockFlow::ok("state1")),
            test_signing_key(),
            db.clone(),
        );
        AppState {
            cfg,
            db: db.clone(),
            auth: Some(provider),
        }
    }

    fn insert_user(db: &chassis::db::SharedDb, id: &str, sub: &str) -> auth::User {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO users (id, oidc_sub, email, display_name, groups, created_at, last_login_at, premium)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![id, sub, "u@example.com", "User", "[]", "2024-01-01T00:00:00Z", "2024-01-01T00:00:00Z", 0],
        )
        .unwrap();
        auth::User {
            id: id.into(),
            oidc_sub: sub.into(),
            email: Some("u@example.com".into()),
            display_name: Some("User".into()),
            groups: vec![],
            premium: false,
        }
    }

    fn session_cookie_for_test(user: &auth::User) -> String {
        let full = auth::session_cookie_for(test_signing_key(), user);
        // session_cookie_for returns a full Set-Cookie header; handlers expect
        // only the signed cookie value (the part after "session=").
        full.split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_string()
    }

    struct MockFlow {
        state: String,
        claims: std::sync::Mutex<Option<auth::TokenClaims>>,
        fail_exchange: std::sync::Mutex<bool>,
    }

    impl MockFlow {
        fn ok(state: &str) -> Self {
            Self {
                state: state.into(),
                claims: std::sync::Mutex::new(Some(auth::TokenClaims {
                    sub: "sub1".into(),
                    email: Some("u@example.com".into()),
                    name: Some("User".into()),
                    groups: vec![],
                })),
                fail_exchange: std::sync::Mutex::new(false),
            }
        }

        fn failing_exchange(state: &str) -> Self {
            Self {
                state: state.into(),
                claims: std::sync::Mutex::new(Some(auth::TokenClaims {
                    sub: "sub1".into(),
                    email: Some("u@example.com".into()),
                    name: Some("User".into()),
                    groups: vec![],
                })),
                fail_exchange: std::sync::Mutex::new(true),
            }
        }
    }

    impl auth::OidcFlow for MockFlow {
        fn authorize(&self) -> (String, String, String) {
            let url = format!("https://issuer.example.com/auth?state={}", self.state);
            (url, self.state.clone(), String::new())
        }

        fn exchange<'a>(
            &'a self,
            _code: &'a str,
            _nonce: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<auth::TokenClaims>> + Send + 'a>,
        > {
            Box::pin(async move {
                if *self.fail_exchange.lock().unwrap() {
                    anyhow::bail!("exchange failed")
                }
                self.claims
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("no claims"))
            })
        }
    }

    fn stripe_signature(secret: &str, payload: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let ts = time::OffsetDateTime::now_utc().unix_timestamp();
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{ts}.").as_bytes());
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());
        format!("t={ts},v1={sig}")
    }

    // ---- dispatch / route ----

    #[tokio::test]
    async fn dispatch_preflight_short_circuits() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        let r = dispatch(
            &state,
            "OPTIONS",
            "/api/v1/stats",
            &[],
            &[("Origin", "http://localhost:3000")],
            &[],
        )
        .await;
        assert_eq!(r.status, 200);
        assert!(body_string(&r).is_empty());
        assert!(r
            .headers
            .iter()
            .any(|(k, _)| k == "Access-Control-Allow-Origin"));
    }

    #[tokio::test]
    async fn dispatch_healthz_skips_analytics() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        let r = dispatch(&state, "GET", "/healthz", &[], &[], &[]).await;
        assert_eq!(r.status, 200);
        assert_eq!(body_string(&r), "{\"status\":\"ok\"}\n");
    }

    #[tokio::test]
    async fn route_healthz_and_api_v1() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        let r = route(&state, "GET", "/healthz", &[], &[], &[]).await;
        assert_eq!(r.status, 200);
        let r = route(&state, "GET", "/api/v1/stats", &[], &[], &[]).await;
        assert_eq!(r.status, 200);
    }

    #[tokio::test]
    async fn route_auth_disabled_falls_through() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        let r = route(&state, "GET", "/auth/login", &[], &[], &[]).await;
        assert_eq!(r.status, 404);
    }

    #[tokio::test]
    async fn route_auth_enabled() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        let r = route(&state, "GET", "/auth/login", &[], &[], &[]).await;
        assert_eq!(r.status, 302);
    }

    #[tokio::test]
    async fn route_billing_disabled_falls_through() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        let r = route(&state, "POST", "/api/billing/webhook", &[], &[], b"{}").await;
        assert_eq!(r.status, 404);
    }

    #[tokio::test]
    async fn route_billing_enabled_webhook_bad_sig() {
        let (_d, db) = open_test_db();
        let state = state_with_billing(&db);
        let r = route(
            &state,
            "POST",
            "/api/billing/webhook",
            &[],
            &[("Stripe-Signature", "bad")],
            b"{}",
        )
        .await;
        assert_eq!(r.status, 400);
    }

    #[tokio::test]
    async fn route_keys_disabled_falls_through() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        let r = route(&state, "GET", "/api/keys", &[], &[], &[]).await;
        assert_eq!(r.status, 404);
    }

    #[tokio::test]
    async fn route_keys_enabled() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let user = insert_user(&db, "u1", "sub1");
        let cookie = session_cookie_for_test(&user);
        let r = route(
            &state,
            "GET",
            "/api/keys",
            &[],
            &[("Cookie", &format!("session={cookie}"))],
            &[],
        )
        .await;
        assert_eq!(r.status, 200);
    }

    #[test]
    fn route_api_v1_known_paths() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        assert!(route_api_v1(&state, "GET", "/api/v1/unknown", &[], &[]).is_none());
        let r = route_api_v1(&state, "GET", "/api/v1/stats", &[], &[]).unwrap();
        assert_eq!(r.status, 200);
    }

    #[tokio::test]
    async fn route_auth_known_and_unknown() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        assert!(route_auth(&state, "GET", "/auth/unknown", None, &[], &[])
            .await
            .is_none());
        let r = route_auth(&state, "GET", "/auth/login", None, &[], &[])
            .await
            .unwrap();
        assert_eq!(r.status, 302);
    }

    #[tokio::test]
    async fn route_billing_known_and_unknown() {
        let (_d, db) = open_test_db();
        let state = state_with_billing(&db);
        assert!(
            route_billing(&state, "/api/billing/unknown", None, &[], &[])
                .await
                .is_none()
        );
        let r = route_billing(
            &state,
            "/api/billing/webhook",
            None,
            &[("Stripe-Signature", "bad")],
            b"{}",
        )
        .await
        .unwrap();
        assert_eq!(r.status, 400);
    }

    #[test]
    fn route_keys_known_and_unknown() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let user = insert_user(&db, "u1", "sub1");
        let cookie = session_cookie_for_test(&user);
        // Disabled flags → None.
        let disabled = AppState {
            cfg: test_config(),
            db: db.clone(),
            auth: None,
        };
        assert!(route_keys(&disabled, "GET", "/api/keys", Some(&cookie), &[]).is_none());
        // Known path with auth.
        let r = route_keys(&state, "GET", "/api/keys", Some(&cookie), &[]).unwrap();
        assert_eq!(r.status, 200);
    }

    // ---- gate / gated ----

    #[test]
    fn gate_v1_missing_and_invalid_key_401() {
        let (_d, db) = open_test_db();
        let r = gate_v1(&db, None, "/api/v1/stats", 60);
        assert_eq!(r.unwrap_err().status, 401);
        let r = gate_v1(&db, Some("pk_deadbeef"), "/api/v1/stats", 60);
        assert_eq!(r.unwrap_err().status, 401);
    }

    #[test]
    fn gate_v1_valid_key_passes_and_rate_limits() {
        let (_d, db) = open_test_db();
        let conn = db.lock().unwrap();
        let plaintext = apikeys::create_key(&conn, "u1", "ci").unwrap();
        drop(conn);
        let r = gate_v1(&db, Some(&plaintext), "/api/v1/stats", 60).unwrap();
        assert_eq!(r.remaining, 59);
        for _ in 0..59 {
            let _ = gate_v1(&db, Some(&plaintext), "/api/v1/stats", 60).unwrap();
        }
        let r = gate_v1(&db, Some(&plaintext), "/api/v1/stats", 60);
        assert_eq!(r.unwrap_err().status, 429);
    }

    #[test]
    fn gate_v1_db_error_500() {
        let (_d, db) = open_test_db();
        let conn = db.lock().unwrap();
        let plaintext = apikeys::create_key(&conn, "u1", "ci").unwrap();
        conn.execute("DROP TABLE api_usage", []).unwrap();
        drop(conn);
        let r = gate_v1(&db, Some(&plaintext), "/api/v1/stats", 60);
        assert_eq!(r.unwrap_err().status, 500);
    }

    #[test]
    fn gated_skips_gate_when_disabled() {
        let (_d, db) = open_test_db();
        let state = state_with_db(&db);
        let r = gated(&state, &[], "/api/v1/stats", |s| stats(&s.db));
        assert_eq!(r.status, 200);
        assert!(!r.headers.iter().any(|(k, _)| k == "X-RateLimit-Remaining"));
    }

    #[test]
    fn gated_applies_gate_when_enabled() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let conn = db.lock().unwrap();
        let plaintext = apikeys::create_key(&conn, "u1", "ci").unwrap();
        drop(conn);
        let r = gated(&state, &[("X-API-Key", &plaintext)], "/api/v1/stats", |s| {
            stats(&s.db)
        });
        assert_eq!(r.status, 200);
        assert!(r.headers.iter().any(|(k, _)| k == "X-RateLimit-Remaining"));
    }

    #[test]
    fn gated_invalid_key_401() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let r = gated(
            &state,
            &[("X-API-Key", "pk_deadbeef")],
            "/api/v1/stats",
            |s| stats(&s.db),
        );
        assert_eq!(r.status, 401);
    }

    // ---- auth ----

    #[tokio::test]
    async fn auth_callback_valid_redirects_with_session() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        let cookies = vec![(auth::STATE_COOKIE.to_string(), "state1".to_string())];
        let r = auth_callback(&state, &cookies, &[("state", "state1"), ("code", "c")]).await;
        assert_eq!(r.status, 302);
        assert!(r.headers.iter().any(|(k, v)| k == "Location" && v == "/"));
        assert!(r.headers.iter().any(|(k, _)| k == "Set-Cookie"));
    }

    #[tokio::test]
    async fn auth_callback_invalid_state_400() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        let cookies = vec![(auth::STATE_COOKIE.to_string(), "state1".to_string())];
        let r = auth_callback(&state, &cookies, &[("state", "wrong"), ("code", "c")]).await;
        assert_eq!(r.status, 400);
    }

    #[tokio::test]
    async fn auth_callback_exchange_error_502() {
        let (_d, db) = open_test_db();
        let provider = AuthProvider::new_for_test(
            Box::new(MockFlow::failing_exchange("state1")),
            test_signing_key(),
            db.clone(),
        );
        let state = AppState {
            cfg: test_config(),
            db: db.clone(),
            auth: Some(provider),
        };
        let cookies = vec![(auth::STATE_COOKIE.to_string(), "state1".to_string())];
        let r = auth_callback(&state, &cookies, &[("state", "state1"), ("code", "c")]).await;
        assert_eq!(r.status, 502);
    }

    #[test]
    fn auth_me_unauthenticated_401() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        let r = auth_me(&state, None);
        assert_eq!(r.status, 401);
    }

    #[test]
    fn auth_me_invalid_session_401() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        let r = auth_me(&state, Some("bad-cookie"));
        assert_eq!(r.status, 401);
    }

    #[test]
    fn auth_me_valid_session() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        let user = insert_user(&db, "u1", "sub1");
        let cookie = session_cookie_for_test(&user);
        let r = auth_me(&state, Some(&cookie));
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert_eq!(v["id"], "u1");
        assert!(v["groups"].is_null());
    }

    // ---- billing ----

    #[tokio::test]
    async fn billing_checkout_unauthenticated_401() {
        let (_d, db) = open_test_db();
        let state = state_with_billing(&db);
        let r = billing_checkout(&state, None).await;
        assert_eq!(r.status, 401);
    }

    #[tokio::test]
    async fn resolve_checkout_user_prefers_session_then_dev() {
        let (_d, db) = open_test_db();
        let state = state_with_auth(&db);
        let user = insert_user(&db, "u1", "sub1");
        let cookie = session_cookie_for_test(&user);
        let user_id = resolve_checkout_user(&state, Some(&cookie)).await;
        assert_eq!(user_id, "u1");

        let mut state = state_with_billing(&db);
        state.cfg.dev_user_id = "dev1".into();
        let user_id = resolve_checkout_user(&state, None).await;
        assert_eq!(user_id, "dev1");
    }

    #[test]
    fn billing_webhook_invalid_signature_400() {
        let (_d, db) = open_test_db();
        let state = state_with_billing(&db);
        let r = billing_webhook(&state, "t=1,v1=00", b"{}");
        assert_eq!(r.status, 400);
    }

    #[test]
    fn billing_webhook_ignored_event() {
        let (_d, db) = open_test_db();
        let state = state_with_billing(&db);
        let payload = br#"{"type":"customer.subscription.deleted","data":{"object":{"id":"sub_404","customer":"cus_404"}}}"#;
        let sig = stripe_signature("whsec_test", payload);
        let r = billing_webhook(&state, &sig, payload);
        assert_eq!(r.status, 200);
        assert_eq!(body_string(&r), "{\"ignored\":true}");
    }

    #[test]
    fn billing_webhook_empty_ack() {
        let (_d, db) = open_test_db();
        let state = state_with_billing(&db);
        let payload = br#"{"type":"unknown.event","data":{"object":{}}}"#;
        let sig = stripe_signature("whsec_test", payload);
        let r = billing_webhook(&state, &sig, payload);
        assert_eq!(r.status, 200);
        assert!(r.body.is_empty());
    }

    // ---- keys ----

    #[test]
    fn keys_list_or_create_unauthenticated_401() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let r = keys_list_or_create(&state, "GET", None, &[]);
        assert_eq!(r.status, 401);
    }

    #[test]
    fn keys_list_or_create_get_and_post() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let user = insert_user(&db, "u1", "sub1");
        let cookie = session_cookie_for_test(&user);

        let r = keys_list_or_create(&state, "GET", Some(&cookie), &[]);
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert!(v["keys"].is_null());

        let r = keys_list_or_create(&state, "POST", Some(&cookie), br#"{"label":"ci"}"#);
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert!(v["key"].as_str().unwrap().starts_with("pk_"));

        let r = keys_list_or_create(&state, "GET", Some(&cookie), &[]);
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_str(&body_string(&r)).unwrap();
        assert_eq!(v["keys"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn keys_list_or_create_wrong_method_405() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let user = insert_user(&db, "u1", "sub1");
        let cookie = session_cookie_for_test(&user);
        let r = keys_list_or_create(&state, "PATCH", Some(&cookie), &[]);
        assert_eq!(r.status, 405);
    }

    #[test]
    fn keys_revoke_flow() {
        let (_d, db) = open_test_db();
        let state = state_with_api_keys(&db);
        let user = insert_user(&db, "u1", "sub1");
        let cookie = session_cookie_for_test(&user);

        let r = keys_revoke(&state, "GET", "k1", Some(&cookie));
        assert_eq!(r.status, 405);

        let r = keys_revoke(&state, "DELETE", "no-such", Some(&cookie));
        assert_eq!(r.status, 404);

        let conn = db.lock().unwrap();
        let plaintext = apikeys::create_key(&conn, "u1", "ci").unwrap();
        let identity = apikeys::authenticate(&conn, &plaintext).unwrap();
        drop(conn);

        let r = keys_revoke(&state, "DELETE", &identity.key_id, Some(&cookie));
        assert_eq!(r.status, 200);
        assert_eq!(body_string(&r), "{\"ok\":true}");

        let r = keys_revoke(&state, "DELETE", &identity.key_id, Some(&cookie));
        assert_eq!(r.status, 404);
    }
}
