//! Port of go-service/internal/billing/{stripe,webhook,entitlements}.go.
//! stdlib-style HTTP, NO Stripe SDK. delta 6: deleted-unknown-subscription →
//! 200 {"ignored":true}. delta 14: typed serde form for checkout (no string concat).

use hmac::{Hmac, Mac};
use rusqlite::params;
use sha2::Sha256;

use crate::db::SharedDb;

type HmacSha256 = Hmac<Sha256>;

const STRIPE_API_BASE: &str = "https://api.stripe.com";
const CHECKOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15); // Go: 15s
const WEBHOOK_TOLERANCE_SECS: i64 = 5 * 60; // Go: 5-minute tolerance, both directions

/// webhook.go verifySignature: parse `t=<ts>,v1=<hex>` (comma-separated,
/// multiple v1 allowed), HMAC-SHA256 over "{t}.{payload}", constant-time
/// compare (stdx constant_time_eq crate), ±5-minute timestamp tolerance
/// (strictly greater rejects — exactly ±300s passes).
pub fn verify_webhook_signature(secret: &str, payload: &[u8], stripe_signature: &str) -> bool {
    let mut ts: i64 = 0;
    let mut sigs: Vec<&str> = Vec::new();
    for part in stripe_signature.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => ts = v.parse().unwrap_or(0),
            Some(("v1", v)) => sigs.push(v),
            _ => {}
        }
    }
    if ts == 0 || sigs.is_empty() {
        return false;
    }
    let d = time::OffsetDateTime::now_utc().unix_timestamp() - ts;
    if !(-WEBHOOK_TOLERANCE_SECS..=WEBHOOK_TOLERANCE_SECS).contains(&d) {
        return false;
    }
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(payload);
    let want = mac.finalize().into_bytes();
    sigs.iter().any(|s| {
        hex::decode(s)
            .map(|got| constant_time_eq::constant_time_eq(&got, &want))
            .unwrap_or(false)
    })
}

/// stripe.go CreateCheckoutSession form, delta 14: typed struct →
/// serde_urlencoded. Field set and values frozen by the Go source.
#[derive(serde::Serialize)]
struct CheckoutForm<'a> {
    mode: &'static str,
    #[serde(rename = "line_items[0][price]")]
    price: &'a str,
    #[serde(rename = "line_items[0][quantity]")]
    quantity: u32,
    client_reference_id: &'a str,
    success_url: String,
    cancel_url: String,
}

fn checkout_form<'a>(cfg: &'a crate::config::Config, user_id: &'a str) -> CheckoutForm<'a> {
    CheckoutForm {
        mode: "subscription",
        price: &cfg.stripe_price_id,
        quantity: 1,
        client_reference_id: user_id,
        // Go server wiring: success/cancel = APP_URL + "/premium?status=success|canceled"
        success_url: format!("{}/premium?status=success", cfg.app_url),
        cancel_url: format!("{}/premium?status=canceled", cfg.app_url),
    }
}

/// Creates a Stripe checkout session, returns the hosted URL.
pub async fn create_checkout_session(
    cfg: &crate::config::Config,
    user_id: &str,
) -> anyhow::Result<String> {
    create_checkout_session_with_base(cfg, user_id, STRIPE_API_BASE).await
}

/// Test seam for the API base URL (Go used the package var `stripeAPIBase`).
#[doc(hidden)]
pub async fn create_checkout_session_with_base(
    cfg: &crate::config::Config,
    user_id: &str,
    base: &str,
) -> anyhow::Result<String> {
    let form = checkout_form(cfg, user_id);
    let client = reqwest::Client::builder()
        .timeout(CHECKOUT_TIMEOUT)
        .build()?;
    let resp = client
        .post(format!("{base}/v1/checkout/sessions"))
        .header("Authorization", format!("Bearer {}", cfg.stripe_secret_key))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(serde_urlencoded::to_string(&form)?)
        .send()
        .await?;
    let status = resp.status().as_u16();
    let body = resp.text().await?;
    if status != 200 {
        let truncated: String = body.chars().take(200).collect(); // Go: truncate(body, 200)
        anyhow::bail!("stripe checkout: status {status}: {truncated}");
    }
    #[derive(serde::Deserialize)]
    struct Out {
        url: Option<String>,
    }
    let out: Out = serde_json::from_str(&body)?;
    match out.url {
        Some(u) if !u.is_empty() => Ok(u),
        _ => anyhow::bail!("stripe checkout: empty url in response"),
    }
}

/// What the HTTP layer should answer for a webhook call (Go's http.Error
/// bodies carry a trailing newline; the delta-6 ignored body does not —
/// it is a new wire shape with no Go counterpart).
pub struct WebhookOutcome {
    pub status: u16,
    pub body: String,
}

fn outcome(status: u16, body: &str) -> WebhookOutcome {
    WebhookOutcome {
        status,
        body: body.to_string(),
    }
}

/// webhook.go ServeHTTP as a pure function.
pub fn handle_webhook(db: &SharedDb, secret: &str, payload: &[u8], sig: &str) -> WebhookOutcome {
    if !verify_webhook_signature(secret, payload, sig) {
        tracing::warn!("billing: webhook signature verification failed");
        return outcome(400, "invalid signature\n");
    }
    #[derive(serde::Deserialize)]
    struct Event {
        #[serde(rename = "type")]
        ty: String,
        data: EventData,
    }
    #[derive(serde::Deserialize)]
    struct EventData {
        object: serde_json::Value,
    }
    let ev: Event = match serde_json::from_slice(payload) {
        Ok(e) => e,
        Err(_) => return outcome(400, "bad payload\n"),
    };
    match ev.ty.as_str() {
        "checkout.session.completed" => {
            #[derive(serde::Deserialize)]
            struct Obj {
                client_reference_id: String,
                customer: String,
                subscription: String,
            }
            let obj: Obj = match serde_json::from_value(ev.data.object) {
                Ok(o) => o,
                Err(_) => return outcome(400, "bad event object\n"),
            };
            if let Err(e) = on_checkout_completed(
                db,
                &obj.client_reference_id,
                &obj.customer,
                &obj.subscription,
            ) {
                tracing::error!("billing: checkout.session.completed: {e}");
                return outcome(500, "processing error\n");
            }
        }
        "customer.subscription.deleted" => {
            #[derive(serde::Deserialize)]
            struct Obj {
                id: String,
                customer: String,
            }
            let obj: Obj = match serde_json::from_value(ev.data.object) {
                Ok(o) => o,
                Err(_) => return outcome(400, "bad event object\n"),
            };
            match on_subscription_deleted(db, &obj.customer, &obj.id) {
                Ok(DeleteResult::Updated) => {}
                // delta 6: unknown subscription → 200 {"ignored":true} (stops Stripe retry storms).
                Ok(DeleteResult::UnknownSubscription) => return outcome(200, "{\"ignored\":true}"),
                Err(e) => {
                    tracing::error!("billing: customer.subscription.deleted: {e}");
                    return outcome(500, "processing error\n");
                }
            }
        }
        // Unknown event types: 200 ack, no state change (spec §5.2, as Go).
        _ => {}
    }
    outcome(200, "")
}

/// webhook.go onCheckoutCompleted: upsert keyed on stripe_subscription_id
/// (idempotent replays), then grant premium.
fn on_checkout_completed(
    db: &SharedDb,
    user_id: &str,
    customer: &str,
    subscription: &str,
) -> anyhow::Result<()> {
    let conn = db.lock().expect("db mutex poisoned");
    conn.execute(
        "INSERT INTO subscriptions (id, user_id, stripe_customer_id, stripe_subscription_id, status)
         VALUES (?, ?, ?, ?, 'active')
         ON CONFLICT(stripe_subscription_id) DO UPDATE SET
             status = 'active',
             stripe_customer_id = excluded.stripe_customer_id",
        params![crate::db::new_id(), user_id, customer, subscription],
    )?;
    set_premium(&conn, customer, true)
}

enum DeleteResult {
    Updated,
    UnknownSubscription,
}

/// webhook.go onSubscriptionDeleted + delta 6.
fn on_subscription_deleted(
    db: &SharedDb,
    customer: &str,
    subscription: &str,
) -> anyhow::Result<DeleteResult> {
    let conn = db.lock().expect("db mutex poisoned");
    let n = conn.execute(
        "UPDATE subscriptions SET status = 'canceled' WHERE stripe_subscription_id = ?",
        params![subscription],
    )?;
    if n == 0 {
        return Ok(DeleteResult::UnknownSubscription); // delta 6 — Go returned a 500 here
    }
    set_premium(&conn, customer, false)?;
    Ok(DeleteResult::Updated)
}

/// entitlements.go SetPremium: flip premium on the user linked to the Stripe
/// customer via the subscriptions table; error when no user is linked.
/// (delta 7's request-time DB read lives in chassis::auth::current_user.)
pub fn set_premium(
    conn: &rusqlite::Connection,
    stripe_customer_id: &str,
    premium: bool,
) -> anyhow::Result<()> {
    let n = conn.execute(
        "UPDATE users SET premium = ?
         WHERE id = (SELECT user_id FROM subscriptions WHERE stripe_customer_id = ? LIMIT 1)",
        params![premium as i64, stripe_customer_id],
    )?;
    if n == 0 {
        anyhow::bail!("no user linked to stripe customer {stripe_customer_id}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // delta 14: the form is a typed struct; assert the exact encoded fields
    // (Go's TestCreateCheckoutSession asserted these from the parsed form).
    #[test]
    fn checkout_form_is_typed() {
        let mut cfg = crate::config::Config::load(); // env-independent fields overwritten below
        cfg.app_url = "http://localhost:8080".into();
        cfg.stripe_price_id = "price_123".into();
        let encoded = serde_urlencoded::to_string(checkout_form(&cfg, "u1")).unwrap();
        assert!(encoded.contains("mode=subscription"), "{encoded}");
        assert!(
            encoded.contains("line_items%5B0%5D%5Bprice%5D=price_123"),
            "{encoded}"
        );
        assert!(
            encoded.contains("line_items%5B0%5D%5Bquantity%5D=1"),
            "{encoded}"
        );
        assert!(encoded.contains("client_reference_id=u1"), "{encoded}");
        assert!(
            encoded
                .contains("success_url=http%3A%2F%2Flocalhost%3A8080%2Fpremium%3Fstatus%3Dsuccess"),
            "{encoded}"
        );
        assert!(
            encoded
                .contains("cancel_url=http%3A%2F%2Flocalhost%3A8080%2Fpremium%3Fstatus%3Dcanceled"),
            "{encoded}"
        );
    }

    // verify_webhook_signature boundary: exactly ±300s still passes (Go uses >).
    #[test]
    fn webhook_tolerance_boundary() {
        let payload = b"{}";
        let ts = time::OffsetDateTime::now_utc().unix_timestamp() - 300;
        let mut mac = HmacSha256::new_from_slice(b"s").expect("hmac");
        mac.update(format!("{ts}.").as_bytes());
        mac.update(payload);
        let header = format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()));
        assert!(
            verify_webhook_signature("s", payload, &header),
            "t-300s is inside tolerance"
        );
        assert!(
            !verify_webhook_signature("s", payload, "t=0,v1=00"),
            "missing/garbage header rejected"
        );
    }
}
