package billing

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	templatedb "risk.lucanian.app/api/internal/db"
	_ "modernc.org/sqlite"
)

const testWebhookSecret = "whsec_testsecret"

func signPayload(secret string, payload []byte, ts int64) string {
	mac := hmac.New(sha256.New, []byte(secret))
	fmt.Fprintf(mac, "%d.", ts)
	mac.Write(payload)
	return hex.EncodeToString(mac.Sum(nil))
}

func webhookRequest(t *testing.T, secret string, payload []byte) *http.Request {
	t.Helper()
	ts := time.Now().Unix()
	req := httptest.NewRequest("POST", "/api/billing/webhook", strings.NewReader(string(payload)))
	req.Header.Set("Stripe-Signature", fmt.Sprintf("t=%d,v1=%s", ts, signPayload(secret, payload, ts)))
	return req
}

func openTestDB(t *testing.T) *sql.DB {
	t.Helper()
	conn, err := sql.Open("sqlite", filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	if err := templatedb.MigrateWith(conn); err != nil {
		t.Fatal(err)
	}
	return conn
}

func seedUser(t *testing.T, conn *sql.DB) {
	t.Helper()
	_, err := conn.Exec(`INSERT INTO users (id, oidc_sub, email, display_name, groups, premium, created_at)
		VALUES ('u1', 'sub-1', 'a@b.c', 'Alice', '[]', 0, datetime('now'))`)
	if err != nil {
		t.Fatal(err)
	}
}

func premiumOf(t *testing.T, conn *sql.DB, userID string) bool {
	t.Helper()
	var p int
	if err := conn.QueryRow("SELECT premium FROM users WHERE id = ?", userID).Scan(&p); err != nil {
		t.Fatal(err)
	}
	return p != 0
}

func completedEvent(userID, customer, subscription string) []byte {
	e := map[string]interface{}{
		"id":   "evt_1",
		"type": "checkout.session.completed",
		"data": map[string]interface{}{"object": map[string]interface{}{
			"client_reference_id": userID,
			"customer":            customer,
			"subscription":        subscription,
		}},
	}
	b, _ := json.Marshal(e)
	return b
}

func deletedEvent(customer, subscription string) []byte {
	e := map[string]interface{}{
		"id":   "evt_2",
		"type": "customer.subscription.deleted",
		"data": map[string]interface{}{"object": map[string]interface{}{
			"id":       subscription,
			"customer": customer,
		}},
	}
	b, _ := json.Marshal(e)
	return b
}

func TestWebhookCheckoutCompletedGrantsPremium(t *testing.T) {
	conn := openTestDB(t)
	seedUser(t, conn)
	h := NewWebhookHandler(conn, testWebhookSecret)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, webhookRequest(t, testWebhookSecret, completedEvent("u1", "cus_1", "sub_stripe_1")))
	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, body %s", rec.Code, rec.Body)
	}
	if !premiumOf(t, conn, "u1") {
		t.Fatal("user must be premium after checkout.session.completed")
	}
	var status string
	if err := conn.QueryRow("SELECT status FROM subscriptions WHERE stripe_subscription_id = 'sub_stripe_1'").Scan(&status); err != nil {
		t.Fatalf("subscription row missing: %v", err)
	}
}

func TestWebhookReplayIsIdempotent(t *testing.T) {
	conn := openTestDB(t)
	seedUser(t, conn)
	h := NewWebhookHandler(conn, testWebhookSecret)
	for i := 0; i < 2; i++ {
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, webhookRequest(t, testWebhookSecret, completedEvent("u1", "cus_1", "sub_stripe_1")))
		if rec.Code != http.StatusOK {
			t.Fatalf("replay %d: status = %d", i, rec.Code)
		}
	}
	var n int
	_ = conn.QueryRow("SELECT COUNT(*) FROM subscriptions WHERE stripe_subscription_id = 'sub_stripe_1'").Scan(&n)
	if n != 1 {
		t.Fatalf("subscription rows = %d, want 1 (idempotent)", n)
	}
}

func TestWebhookSubscriptionDeletedRevokesPremium(t *testing.T) {
	conn := openTestDB(t)
	seedUser(t, conn)
	h := NewWebhookHandler(conn, testWebhookSecret)
	h.ServeHTTP(httptest.NewRecorder(), webhookRequest(t, testWebhookSecret, completedEvent("u1", "cus_1", "sub_stripe_1")))
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, webhookRequest(t, testWebhookSecret, deletedEvent("cus_1", "sub_stripe_1")))
	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d", rec.Code)
	}
	if premiumOf(t, conn, "u1") {
		t.Fatal("premium must be revoked after customer.subscription.deleted")
	}
}

func TestWebhookRejectsBadSignature(t *testing.T) {
	conn := openTestDB(t)
	h := NewWebhookHandler(conn, testWebhookSecret)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, webhookRequest(t, "whsec_wrong", completedEvent("u1", "cus_1", "sub_1")))
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", rec.Code)
	}
}

func TestWebhookUnknownEventAcksWithoutStateChange(t *testing.T) {
	conn := openTestDB(t)
	h := NewWebhookHandler(conn, testWebhookSecret)
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, webhookRequest(t, testWebhookSecret, []byte(`{"id":"evt_9","type":"ping","data":{"object":{}}}`)))
	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200 ack", rec.Code)
	}
	var n int
	_ = conn.QueryRow("SELECT COUNT(*) FROM subscriptions").Scan(&n)
	if n != 0 {
		t.Fatalf("subscriptions = %d, want 0 (no state change)", n)
	}
}

func TestCreateCheckoutSession(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/checkout/sessions" {
			t.Fatalf("path = %s", r.URL.Path)
		}
		if err := r.ParseForm(); err != nil {
			t.Fatal(err)
		}
		if got := r.Form.Get("client_reference_id"); got != "u1" {
			t.Fatalf("client_reference_id = %q", got)
		}
		if got := r.Form.Get("line_items[0][price]"); got != "price_123" {
			t.Fatalf("price = %q", got)
		}
		if r.Header.Get("Authorization") != "Bearer sk_test_x" {
			t.Fatalf("auth header = %q", r.Header.Get("Authorization"))
		}
		w.Write([]byte(`{"url": "https://checkout.stripe.com/pay/cs_1"}`))
	}))
	defer srv.Close()

	oldBase, oldKey := stripeAPIBase, stripeSecretKey
	stripeAPIBase, stripeSecretKey = srv.URL, "sk_test_x"
	defer func() { stripeAPIBase, stripeSecretKey = oldBase, oldKey }()

	url, err := CreateCheckoutSession(context.Background(), "u1", "price_123")
	if err != nil {
		t.Fatalf("CreateCheckoutSession: %v", err)
	}
	if url != "https://checkout.stripe.com/pay/cs_1" {
		t.Fatalf("url = %q", url)
	}
}

func TestSetPremium(t *testing.T) {
	conn := openTestDB(t)
	seedUser(t, conn)
	if err := SetPremium(context.Background(), conn, "cus_1", true); err == nil {
		t.Fatal("SetPremium for unknown customer should error (no subscription link)")
	}
	if _, err := conn.Exec(`INSERT INTO subscriptions (id, user_id, stripe_customer_id, stripe_subscription_id, status)
		VALUES ('s1', 'u1', 'cus_1', 'sub_stripe_1', 'active')`); err != nil {
		t.Fatal(err)
	}
	if err := SetPremium(context.Background(), conn, "cus_1", true); err != nil {
		t.Fatalf("SetPremium: %v", err)
	}
	if !premiumOf(t, conn, "u1") {
		t.Fatal("premium not set")
	}
	if err := SetPremium(context.Background(), conn, "cus_1", false); err != nil {
		t.Fatalf("SetPremium: %v", err)
	}
	if premiumOf(t, conn, "u1") {
		t.Fatal("premium not revoked")
	}
}
