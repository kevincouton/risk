//! Port of go-service/internal/auth/{oidc,session,middleware}.go.
//! Sessions are stateless HMAC-signed cookies; premium/groups are re-read
//! from the DB at request time (delta 7) — the cookie snapshot is a display
//! hint only. OIDC via the openidconnect crate (3.5.0, verified).
//!
//! Contract parity notes (R-1 golden auth_login_302.json pins the wire shape):
//! - The authorize URL carries NO nonce param and its query keys are in
//!   Go's url.Values.Encode() (alphabetical) order — openidconnect's
//!   authorize_url() would append `nonce` and emit its own order, so the
//!   URL is built manually. Go never sent or verified a nonce, so the id
//!   token is verified with a nonce-skipping NonceVerifier (exact Go parity).
//! - The state cookie value is the state token ALONE (base64url, dot-free),
//!   exactly as Go's HandleLogin wrote it — no `{state}.{nonce}` compound.

use std::future::Future;
use std::pin::Pin;

use hmac::{Hmac, Mac};
use rusqlite::params;
use sha2::Sha256;

use crate::db::SharedDb;

type HmacSha256 = Hmac<Sha256>;

pub const SESSION_COOKIE: &str = "session";
pub const STATE_COOKIE: &str = "oidc_state";
/// Go session.go: maxAge = 7 * 24 * time.Hour.
pub const SESSION_MAX_AGE_SECS: i64 = 7 * 24 * 3600; // 604800
/// Go oidc.go HandleLogin: state cookie MaxAge: 300.
pub const STATE_MAX_AGE_SECS: i64 = 300;

#[derive(Debug, Clone, serde::Serialize)]
pub struct User {
    pub id: String,
    #[serde(skip)] // Go: `json:"-"`
    pub oidc_sub: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub groups: Vec<String>,
    pub premium: bool,
}

/// oidc.go TokenClaims: the verified OIDC claims we keep.
pub struct TokenClaims {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub groups: Vec<String>,
}

// ---- sessions (session.go) ----

/// sessionPayload field names are Go's short JSON keys: uid/email/name/groups/premium/exp.
#[derive(serde::Serialize, serde::Deserialize)]
struct SessionPayload {
    uid: String,
    email: String,
    name: String,
    groups: Vec<String>,
    premium: bool,
    exp: i64,
}

/// Go sign: base64.RawURLEncoding(payload) + "." + base64.RawURLEncoding(HMAC-SHA256(payload)).
pub fn sign_session(key: &[u8], payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(payload.as_bytes());
    format!(
        "{}.{}",
        base64::encode(payload.as_bytes(), base64::Alphabet::UrlNoPadding),
        base64::encode(mac.finalize().into_bytes(), base64::Alphabet::UrlNoPadding)
    )
}

/// Go Read (signature part): split on '.', constant-time-compare the base64
/// signature strings (Go used hmac.Equal on the encoded strings). Returns the
/// payload JSON; expiry is checked by the caller (current_user), as in Go.
pub fn verify_session(key: &[u8], signed: &str) -> Option<String> {
    let (b64_payload, sig) = signed.split_once('.')?;
    let raw = base64::decode(b64_payload, base64::Alphabet::UrlNoPadding).ok()?;
    let payload = String::from_utf8(raw).ok()?;
    let want = sign_session(key, &payload);
    let (_, want_sig) = want.split_once('.')?;
    if !constant_time_eq::constant_time_eq(sig.as_bytes(), want_sig.as_bytes()) {
        return None;
    }
    Some(payload)
}

/// Full Set-Cookie header value for a new session.
/// Go cookie, quoted: Name "session", Path=/, MaxAge 604800, HttpOnly,
/// Secure (NewProvider hardcodes secure=true), SameSite=Lax.
pub fn session_cookie_for(key: &[u8], user: &User) -> String {
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + SESSION_MAX_AGE_SECS;
    session_cookie_with_exp(key, user, exp)
}

/// Test seam for expiry tests (Go manipulated SessionManager.maxAge).
#[doc(hidden)]
pub fn session_cookie_with_exp(key: &[u8], user: &User, exp: i64) -> String {
    let payload = SessionPayload {
        uid: user.id.clone(),
        email: user.email.clone().unwrap_or_default(),
        name: user.display_name.clone().unwrap_or_default(),
        groups: user.groups.clone(),
        premium: user.premium,
        exp,
    };
    let json =
        serde_json::to_string(&payload).expect("session payload serialization is infallible");
    format!(
        "{SESSION_COOKIE}={}; Path=/; Max-Age={SESSION_MAX_AGE_SECS}; HttpOnly; Secure; SameSite=Lax",
        sign_session(key, &json)
    )
}

/// Go Clear: empty value, MaxAge -1 (rendered as Max-Age=0 by Go).
pub fn clear_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax")
}

// ---- provider (oidc.go + middleware.go) ----

/// Test seam port of Go Provider's `authCodeURL`/`exchange` fields.
/// The production implementation wraps the openidconnect client.
pub trait OidcFlow: Send + Sync {
    /// Returns (authorization URL, state, nonce). The nonce is a legacy of
    /// the openidconnect API; the production flow neither sends nor verifies
    /// one (Go parity) and `login` ignores it.
    fn authorize(&self) -> (String, String, String);
    fn exchange<'a>(
        &'a self,
        code: &'a str,
        nonce: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<TokenClaims>> + Send + 'a>>;
}

#[derive(Debug)]
pub enum CallbackError {
    InvalidState,
    Exchange(anyhow::Error),
    Upsert(anyhow::Error),
}

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallbackError::InvalidState => write!(f, "invalid oauth state"),
            CallbackError::Exchange(e) => write!(f, "token exchange failed: {e}"),
            CallbackError::Upsert(e) => write!(f, "user upsert failed: {e}"),
        }
    }
}
impl std::error::Error for CallbackError {}

pub struct LoginStart {
    pub auth_url: String,
    pub state_cookie: String,
}

#[derive(Debug)] // Debug required for unwrap_err() in tests
pub struct CallbackSuccess {
    pub session_cookie: String,
}

pub struct AuthProvider {
    flow: Box<dyn OidcFlow>,
    signing_key: Vec<u8>,
    db: SharedDb,
}

impl AuthProvider {
    /// oidc.go NewProvider: discovery against the live issuer.
    /// RedirectURL is {APP_URL}/auth/callback; scopes openid+profile+email+groups.
    pub async fn discover(
        cfg: &crate::config::Config,
        db: SharedDb,
    ) -> anyhow::Result<AuthProvider> {
        let flow = oidc::RealFlow::discover(cfg).await?;
        Ok(AuthProvider {
            flow: Box::new(flow),
            signing_key: cfg.session_signing_key.as_bytes().to_vec(),
            db,
        })
    }

    /// Test constructor (Go tests built Provider with struct literals).
    #[doc(hidden)]
    pub fn new_for_test(flow: Box<dyn OidcFlow>, signing_key: &[u8], db: SharedDb) -> AuthProvider {
        AuthProvider {
            flow,
            signing_key: signing_key.to_vec(),
            db,
        }
    }

    /// HandleLogin: state cookie + issuer URL (the 302 itself is the server's job).
    /// The cookie value is the state token alone (base64url, dot-free), byte-
    /// compatible with Go's HandleLogin and the R-1 golden normalization.
    pub fn login(&self) -> LoginStart {
        let (auth_url, state, _nonce) = self.flow.authorize();
        let state_cookie =
            format!("{STATE_COOKIE}={state}; Path=/; Max-Age={STATE_MAX_AGE_SECS}; HttpOnly; Secure; SameSite=Lax");
        LoginStart {
            auth_url,
            state_cookie,
        }
    }

    /// HandleCallback: verify state, exchange code, upsert user, issue session.
    /// (The 302 to "/" is the server's job — Go redirects to "/", NOT APP_URL.)
    pub async fn callback(
        &self,
        state_cookie_value: &str,
        query_state: &str,
        code: &str,
    ) -> Result<CallbackSuccess, CallbackError> {
        // Go: r.Cookie error OR empty OR != query state → 400 invalid oauth state.
        if state_cookie_value.is_empty() || state_cookie_value != query_state {
            return Err(CallbackError::InvalidState);
        }
        let claims = self
            .flow
            .exchange(code, "")
            .await
            .map_err(CallbackError::Exchange)?;
        let user = self.upsert_user(&claims).map_err(CallbackError::Upsert)?;
        Ok(CallbackSuccess {
            session_cookie: session_cookie_for(&self.signing_key, &user),
        })
    }

    /// oidc.go upsertUser: insert-or-update keyed on oidc_sub, return the row.
    /// Go timestamps here are time.RFC3339 (NOT SQLite datetime format) — kept.
    fn upsert_user(&self, claims: &TokenClaims) -> anyhow::Result<User> {
        let groups_json = serde_json::to_string(&claims.groups)?;
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?;
        let conn = self.db.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO users (id, oidc_sub, email, display_name, groups, created_at, last_login_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(oidc_sub) DO UPDATE SET
                 email = excluded.email,
                 display_name = excluded.display_name,
                 groups = excluded.groups,
                 last_login_at = excluded.last_login_at",
            params![crate::db::new_id(), claims.sub, claims.email, claims.name, groups_json, now, now],
        )?;
        let user = conn.query_row(
            "SELECT id, oidc_sub, email, display_name, groups, premium FROM users WHERE oidc_sub = ?",
            params![claims.sub],
            |r| {
                let groups_json: String = r.get(4)?;
                let premium: i64 = r.get(5)?;
                Ok(User {
                    id: r.get(0)?,
                    oidc_sub: r.get(1)?,
                    email: r.get(2)?,
                    display_name: r.get(3)?,
                    groups: serde_json::from_str(&groups_json).unwrap_or_default(),
                    premium: premium != 0,
                })
            },
        )?;
        Ok(user)
    }

    /// CurrentUser, delta 7: verify + expiry-check the cookie, then read
    /// premium/groups fresh from the DB — the cookie snapshot is ignored.
    pub fn current_user(&self, db: &SharedDb, session_cookie: &str) -> Option<User> {
        let payload = verify_session(&self.signing_key, session_cookie)?;
        let p: SessionPayload = serde_json::from_str(&payload).ok()?;
        if time::OffsetDateTime::now_utc().unix_timestamp() > p.exp {
            return None; // Go: "session expired"
        }
        let conn = db.lock().ok()?;
        conn.query_row(
            "SELECT id, oidc_sub, email, display_name, groups, premium FROM users WHERE id = ?",
            params![p.uid],
            |r| {
                let groups_json: String = r.get(4)?;
                let premium: i64 = r.get(5)?;
                Ok(User {
                    id: r.get(0)?,
                    oidc_sub: r.get(1)?,
                    email: r.get(2)?,
                    display_name: r.get(3)?,
                    groups: serde_json::from_str(&groups_json).unwrap_or_default(),
                    premium: premium != 0,
                })
            },
        )
        .ok()
    }

    /// middleware.go RequireGroup, delta 7: Some(user) when the session user
    /// has the group (or group == "premium" && user.premium), else None.
    /// The 401-vs-403 split happens in the server: current_user None → 401;
    /// Some(user) but require_group None → 403.
    pub fn require_group(&self, db: &SharedDb, session_cookie: &str, group: &str) -> Option<User> {
        let u = self.current_user(db, session_cookie)?;
        if u.groups.iter().any(|g| g == group) {
            return Some(u);
        }
        if group == "premium" && u.premium {
            return Some(u);
        }
        None
    }
}

// ---- production OIDC flow (openidconnect 3.5.0; API verified against source) ----

mod oidc {
    use super::{OidcFlow, TokenClaims};
    use std::future::Future;
    use std::pin::Pin;

    use openidconnect::core::{
        CoreAuthDisplay, CoreAuthPrompt, CoreErrorResponseType, CoreGenderClaim, CoreJsonWebKey,
        CoreJsonWebKeyType, CoreJsonWebKeyUse, CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreRevocableToken,
        CoreRevocationErrorResponse, CoreTokenIntrospectionResponse, CoreTokenType,
    };
    use openidconnect::reqwest::async_http_client;
    use openidconnect::{
        AdditionalClaims, AuthorizationCode, Client, ClientId, ClientSecret, EmptyExtraTokenFields,
        IdTokenFields, IssuerUrl, Nonce, RedirectUrl, StandardErrorResponse, StandardTokenResponse,
    };
    use rand::RngCore as _;

    /// Extra claim we keep beyond the standard set: `groups` (Authentik).
    /// email/name are STANDARD claims — read via claims.email()/claims.name().
    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    pub struct PlatformClaims {
        #[serde(default)]
        pub groups: Option<Vec<String>>,
    }
    impl AdditionalClaims for PlatformClaims {}

    type PlatformIdTokenFields = IdTokenFields<
        PlatformClaims,
        EmptyExtraTokenFields,
        CoreGenderClaim,
        CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
        CoreJsonWebKeyType,
    >;

    // Same generic expansion as openidconnect::core::CoreClient, with
    // PlatformClaims in place of EmptyAdditionalClaims.
    type PlatformClient = Client<
        PlatformClaims,
        CoreAuthDisplay,
        CoreGenderClaim,
        CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
        CoreJsonWebKeyType,
        CoreJsonWebKeyUse,
        CoreJsonWebKey,
        CoreAuthPrompt,
        StandardErrorResponse<CoreErrorResponseType>,
        StandardTokenResponse<PlatformIdTokenFields, CoreTokenType>,
        CoreTokenType,
        CoreTokenIntrospectionResponse,
        CoreRevocableToken,
        CoreRevocationErrorResponse,
    >;

    pub struct RealFlow {
        client: PlatformClient,
        authorization_endpoint: String,
        client_id: String,
        redirect_uri: String,
    }

    impl RealFlow {
        pub async fn discover(cfg: &crate::config::Config) -> anyhow::Result<RealFlow> {
            let metadata = CoreProviderMetadata::discover_async(
                IssuerUrl::new(cfg.oidc_issuer.clone())?,
                async_http_client,
            )
            .await?;
            let authorization_endpoint = metadata.authorization_endpoint().url().to_string();
            let redirect_uri = format!("{}/auth/callback", cfg.app_url);
            let client = PlatformClient::from_provider_metadata(
                metadata,
                ClientId::new(cfg.oidc_client_id.clone()),
                Some(ClientSecret::new(cfg.oidc_client_secret.clone())),
            )
            .set_redirect_uri(RedirectUrl::new(redirect_uri.clone())?);
            Ok(RealFlow {
                client,
                authorization_endpoint,
                client_id: cfg.oidc_client_id.clone(),
                redirect_uri,
            })
        }
    }

    /// Go-compatible query escaping (url.QueryEscape): alphanumerics and
    /// -_.~ pass through, space → '+', everything else %XX uppercase.
    fn query_escape(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    impl OidcFlow for RealFlow {
        fn authorize(&self) -> (String, String, String) {
            // Go oidc.go randomState: 16 random bytes → base64.RawURLEncoding (22 chars, dot-free).
            let mut b = [0u8; 16];
            rand::rng().fill_bytes(&mut b);
            let state = base64::encode(b, base64::Alphabet::UrlNoPadding);
            // Go oauth2.AuthCodeURL: endpoint + "?" + url.Values.Encode() — keys
            // sorted alphabetically, NO nonce param (go-oidc flow). openidconnect's
            // authorize_url() would append `nonce` and its own key order, which
            // breaks the R-1 golden auth_login_302.json — build the URL manually.
            let url = format!(
                "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
                self.authorization_endpoint,
                query_escape(&self.client_id),
                query_escape(&self.redirect_uri),
                query_escape("openid profile email groups"),
                query_escape(&state),
            );
            (url, state, String::new())
        }

        fn exchange<'a>(
            &'a self,
            code: &'a str,
            _nonce: &'a str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<TokenClaims>> + Send + 'a>> {
            Box::pin(async move {
                use openidconnect::TokenResponse as _; // id_token() lives on the trait
                let token = self
                    .client
                    .exchange_code(AuthorizationCode::new(code.to_string()))
                    .request_async(async_http_client)
                    .await?;
                let id_token = token
                    .id_token()
                    .ok_or_else(|| anyhow::anyhow!("no id_token in token response"))?;
                // Go verified the id token with go-oidc WITHOUT a nonce check
                // (no nonce was ever sent); NonceVerifier closure = skip, exact parity.
                let claims = id_token.claims(
                    &self.client.id_token_verifier(),
                    |_nonce: Option<&Nonce>| Ok(()),
                )?;
                Ok(TokenClaims {
                    sub: claims.subject().to_string(),
                    email: claims.email().map(|e| e.as_str().to_owned()),
                    name: claims
                        .name()
                        .and_then(|n| n.get(None))
                        .map(|n| n.as_str().to_owned()),
                    groups: claims
                        .additional_claims()
                        .groups
                        .clone()
                        .unwrap_or_default(),
                })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn sign_verify_round_trip() {
        let signed = sign_session(KEY, r#"{"uid":"u1","exp":9999999999}"#);
        let payload = verify_session(KEY, &signed).expect("valid signature verifies");
        assert_eq!(payload, r#"{"uid":"u1","exp":9999999999}"#);
        // Go value shape: <b64url payload>.<b64url hmac>, no padding, exactly one dot.
        assert_eq!(signed.matches('.').count(), 1);
        assert!(!signed.contains('='), "raw URL encoding has no padding");
    }

    #[test]
    fn verify_rejects_tampering() {
        let signed = sign_session(KEY, r#"{"uid":"u1","exp":9999999999}"#);
        assert!(
            verify_session(b"wrong-key-wrong-key-wrong-key-32", &signed).is_none(),
            "wrong key"
        );
        let (b64, _sig) = signed.split_once('.').unwrap();
        assert!(
            verify_session(KEY, &format!("{b64}.AAAA")).is_none(),
            "tampered signature"
        );
        assert!(
            verify_session(KEY, "no-dot-at-all").is_none(),
            "malformed cookie"
        );
    }
}
