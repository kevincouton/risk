//! delta 5: without a session AND without DEV_USER_ID, checkout is 401.
//! (The success path calls api.stripe.com and is covered at chassis level with
//! an httptest stub — create_checkout_session_posts_form_and_returns_url.)

mod common;

use common::{spawn_test_server, stop};

#[test]
fn checkout_401_without_session_or_dev_user() {
    let (child, base) = spawn_test_server(&[
        ("BILLING_ENABLED", "true"),
        ("STRIPE_SECRET_KEY", "sk_test_x"),
        ("STRIPE_WEBHOOK_SECRET", "whsec_x"),
        ("STRIPE_PRICE_ID", "price_123"),
    ]);
    let resp = reqwest::blocking::Client::new()
        .post(format!("{base}/api/billing/checkout"))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_eq!(
        resp.text().unwrap(),
        "{\"error\":\"authentication required\"}\n"
    );
    stop(child);
}
