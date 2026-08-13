//! Ports of go-service/internal/billing/billing_test.go (7 tests) plus the
//! delta-6 test (deleted-unknown-subscription → 200 {"ignored":true}).

use hmac::{Hmac, Mac};
use sha2::Sha256;

use chassis::billing;
use chassis::db::{self, SharedDb};

const TEST_WEBHOOK_SECRET: &str = "whsec_testsecret";

fn sign_payload(secret: &str, payload: &[u8], ts: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{ts}.").as_bytes());
    mac.update(payload);
    hex::encode(mac.finalize().into_bytes())
}

fn signed_header(secret: &str, payload: &[u8]) -> String {
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();
    format!("t={ts},v1={}", sign_payload(secret, payload, ts))
}

fn open_test_db() -> (tempfile::TempDir, SharedDb) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    {
        let conn = db::open(path.to_str().unwrap()).unwrap();
        db::migrate(&conn).unwrap();
    }
    let shared = db::open_shared(path.to_str().unwrap()).unwrap();
    (dir, shared)
}

fn seed_user(db: &SharedDb) {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO users (id, oidc_sub, email, display_name, groups, premium, created_at)
         VALUES ('u1', 'sub-1', 'a@b.c', 'Alice', '[]', 0, datetime('now'))",
        [],
    )
    .unwrap();
}

fn premium_of(db: &SharedDb, user_id: &str) -> bool {
    let conn = db.lock().unwrap();
    let p: i64 = conn
        .query_row(
            "SELECT premium FROM users WHERE id = ?",
            rusqlite::params![user_id],
            |r| r.get(0),
        )
        .unwrap();
    p != 0
}

fn completed_event(user: &str, customer: &str, subscription: &str) -> Vec<u8> {
    serde_json::json!({
        "id": "evt_1",
        "type": "checkout.session.completed",
        "data": {"object": {"client_reference_id": user, "customer": customer, "subscription": subscription}},
    })
    .to_string()
    .into_bytes()
}

fn deleted_event(customer: &str, subscription: &str) -> Vec<u8> {
    serde_json::json!({
        "id": "evt_2",
        "type": "customer.subscription.deleted",
        "data": {"object": {"id": subscription, "customer": customer}},
    })
    .to_string()
    .into_bytes()
}

// ---- Go port: TestWebhookCheckoutCompletedGrantsPremium ----
#[test]
fn webhook_checkout_completed_grants_premium() {
    let (_d, db) = open_test_db();
    seed_user(&db);
    let payload = completed_event("u1", "cus_1", "sub_stripe_1");
    let out = billing::handle_webhook(
        &db,
        TEST_WEBHOOK_SECRET,
        &payload,
        &signed_header(TEST_WEBHOOK_SECRET, &payload),
    );
    assert_eq!(out.status, 200, "body: {}", out.body);
    assert!(
        premium_of(&db, "u1"),
        "user must be premium after checkout.session.completed"
    );
    let conn = db.lock().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM subscriptions WHERE stripe_subscription_id = 'sub_stripe_1'",
            [],
            |r| r.get(0),
        )
        .expect("subscription row must exist");
    assert_eq!(status, "active");
}

// ---- Go port: TestWebhookReplayIsIdempotent ----
#[test]
fn webhook_replay_is_idempotent() {
    let (_d, db) = open_test_db();
    seed_user(&db);
    let payload = completed_event("u1", "cus_1", "sub_stripe_1");
    for i in 0..2 {
        let out = billing::handle_webhook(
            &db,
            TEST_WEBHOOK_SECRET,
            &payload,
            &signed_header(TEST_WEBHOOK_SECRET, &payload),
        );
        assert_eq!(out.status, 200, "replay {i}: status {}", out.status);
    }
    let conn = db.lock().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM subscriptions WHERE stripe_subscription_id = 'sub_stripe_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "replays must be idempotent");
}

// ---- Go port: TestWebhookSubscriptionDeletedRevokesPremium ----
#[test]
fn webhook_subscription_deleted_revokes_premium() {
    let (_d, db) = open_test_db();
    seed_user(&db);
    let p1 = completed_event("u1", "cus_1", "sub_stripe_1");
    billing::handle_webhook(
        &db,
        TEST_WEBHOOK_SECRET,
        &p1,
        &signed_header(TEST_WEBHOOK_SECRET, &p1),
    );
    let p2 = deleted_event("cus_1", "sub_stripe_1");
    let out = billing::handle_webhook(
        &db,
        TEST_WEBHOOK_SECRET,
        &p2,
        &signed_header(TEST_WEBHOOK_SECRET, &p2),
    );
    assert_eq!(out.status, 200);
    assert!(
        !premium_of(&db, "u1"),
        "premium must be revoked after customer.subscription.deleted"
    );
}

// ---- Go port: TestWebhookRejectsBadSignature ----
#[test]
fn webhook_rejects_bad_signature() {
    let (_d, db) = open_test_db();
    let payload = completed_event("u1", "cus_1", "sub_1");
    let out = billing::handle_webhook(
        &db,
        TEST_WEBHOOK_SECRET,
        &payload,
        &signed_header("whsec_wrong", &payload),
    );
    assert_eq!(out.status, 400, "want 400 for bad signature");
}

// ---- Go port: TestWebhookUnknownEventAcksWithoutStateChange ----
#[test]
fn webhook_unknown_event_acks_without_state_change() {
    let (_d, db) = open_test_db();
    let payload = br#"{"id":"evt_9","type":"ping","data":{"object":{}}}"#.to_vec();
    let out = billing::handle_webhook(
        &db,
        TEST_WEBHOOK_SECRET,
        &payload,
        &signed_header(TEST_WEBHOOK_SECRET, &payload),
    );
    assert_eq!(out.status, 200, "unknown event types get a 200 ack");
    let conn = db.lock().unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM subscriptions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "no state change for unknown events");
}

// ---- New (delta 6): deleted-unknown-subscription → 200 {"ignored":true}. ----
#[test]
fn webhook_deleted_unknown_subscription_is_ignored() {
    let (_d, db) = open_test_db();
    let payload = deleted_event("cus_never_seen", "sub_never_seen");
    let out = billing::handle_webhook(
        &db,
        TEST_WEBHOOK_SECRET,
        &payload,
        &signed_header(TEST_WEBHOOK_SECRET, &payload),
    );
    assert_eq!(
        out.status, 200,
        "unknown-subscription delete must 200 (stops Stripe retry storms)"
    );
    assert_eq!(out.body, "{\"ignored\":true}");
}

// ---- Go port: TestCreateCheckoutSession (httptest stub; form-field contents
// are asserted byte-exact in the in-module unit test checkout_form_is_typed). ----
#[tokio::test]
async fn create_checkout_session_posts_form_and_returns_url() {
    let server = httptest::Server::run();
    server.expect(
        httptest::Expectation::matching(httptest::matchers::request::method_path(
            "POST",
            "/v1/checkout/sessions",
        ))
        .times(1)
        .respond_with(
            httptest::responders::status_code(200)
                .body(r#"{"url": "https://checkout.stripe.com/pay/cs_1"}"#),
        ),
    );
    let base = server.url_str("/").trim_end_matches('/').to_string();
    let url = billing::create_checkout_session_with_base(&test_cfg(), "u1", &base)
        .await
        .unwrap();
    assert_eq!(url, "https://checkout.stripe.com/pay/cs_1");
}

// ---- Go port: TestSetPremium ----
#[test]
fn set_premium() {
    let (_d, db) = open_test_db();
    seed_user(&db);
    {
        let conn = db.lock().unwrap();
        assert!(
            billing::set_premium(&conn, "cus_1", true).is_err(),
            "unknown customer must error (no subscription link)"
        );
        conn.execute(
            "INSERT INTO subscriptions (id, user_id, stripe_customer_id, stripe_subscription_id, status)
             VALUES ('s1', 'u1', 'cus_1', 'sub_stripe_1', 'active')",
            [],
        )
        .unwrap();
        billing::set_premium(&conn, "cus_1", true).unwrap();
    }
    assert!(premium_of(&db, "u1"), "premium not set");
    {
        let conn = db.lock().unwrap();
        billing::set_premium(&conn, "cus_1", false).unwrap();
    }
    assert!(!premium_of(&db, "u1"), "premium not revoked");
}

fn test_cfg() -> chassis::config::Config {
    chassis::config::Config {
        platform_name: "test".into(),
        database_path: String::new(),
        api_port: 8080,
        posthog_api_key: String::new(),
        ga_id: String::new(),
        ads_id: String::new(),
        auth_enabled: false,
        oidc_issuer: String::new(),
        oidc_client_id: String::new(),
        oidc_client_secret: String::new(),
        session_signing_key: String::new(),
        app_url: "http://localhost:8080".into(),
        cors_origin: String::new(),
        billing_enabled: true,
        stripe_secret_key: "sk_test_x".into(),
        stripe_webhook_secret: TEST_WEBHOOK_SECRET.into(),
        stripe_price_id: "price_123".into(),
        api_keys_enabled: false,
        dev_user_id: String::new(),
    }
}
