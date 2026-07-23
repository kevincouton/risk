package billing

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"io"
	"log"
	"net/http"
	"strconv"
	"strings"
	"time"

	"risk.lucanian.app/api/internal/db"
)

// WebhookHandler verifies Stripe signatures and syncs entitlements.
type WebhookHandler struct {
	db     *sql.DB
	secret string
}

func NewWebhookHandler(conn *sql.DB, webhookSecret string) http.Handler {
	return &WebhookHandler{db: conn, secret: webhookSecret}
}

// verifySignature checks the Stripe-Signature header (scheme: t=<ts>,v1=<hmac>
// over "<ts>.<payload>" with the webhook secret; 5-minute tolerance).
func (w *WebhookHandler) verifySignature(header string, payload []byte) bool {
	var ts int64
	var sigs []string
	for _, part := range strings.Split(header, ",") {
		kv := strings.SplitN(part, "=", 2)
		if len(kv) != 2 {
			continue
		}
		switch kv[0] {
		case "t":
			ts, _ = strconv.ParseInt(kv[1], 10, 64)
		case "v1":
			sigs = append(sigs, kv[1])
		}
	}
	if ts == 0 || len(sigs) == 0 {
		return false
	}
	if d := time.Since(time.Unix(ts, 0)); d > 5*time.Minute || d < -5*time.Minute {
		return false
	}
	mac := hmac.New(sha256.New, []byte(w.secret))
	mac.Write([]byte(strconv.FormatInt(ts, 10)))
	mac.Write([]byte("."))
	mac.Write(payload)
	want := mac.Sum(nil)
	for _, s := range sigs {
		got, err := hex.DecodeString(s)
		if err == nil && hmac.Equal(got, want) {
			return true
		}
	}
	return false
}

type stripeEvent struct {
	ID   string `json:"id"`
	Type string `json:"type"`
	Data struct {
		Object json.RawMessage `json:"object"`
	} `json:"data"`
}

func (w *WebhookHandler) ServeHTTP(rw http.ResponseWriter, r *http.Request) {
	payload, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
	if err != nil {
		http.Error(rw, "read error", http.StatusBadRequest)
		return
	}
	if !w.verifySignature(r.Header.Get("Stripe-Signature"), payload) {
		log.Printf("billing: webhook signature verification failed")
		http.Error(rw, "invalid signature", http.StatusBadRequest)
		return
	}
	var ev stripeEvent
	if err := json.Unmarshal(payload, &ev); err != nil {
		http.Error(rw, "bad payload", http.StatusBadRequest)
		return
	}

	switch ev.Type {
	case "checkout.session.completed":
		var obj struct {
			ClientReferenceID string `json:"client_reference_id"`
			Customer          string `json:"customer"`
			Subscription      string `json:"subscription"`
		}
		if err := json.Unmarshal(ev.Data.Object, &obj); err != nil {
			http.Error(rw, "bad event object", http.StatusBadRequest)
			return
		}
		if err := w.onCheckoutCompleted(r.Context(), obj.ClientReferenceID, obj.Customer, obj.Subscription); err != nil {
			log.Printf("billing: checkout.session.completed: %v", err)
			http.Error(rw, "processing error", http.StatusInternalServerError)
			return
		}
	case "customer.subscription.deleted":
		var obj struct {
			ID       string `json:"id"`
			Customer string `json:"customer"`
		}
		if err := json.Unmarshal(ev.Data.Object, &obj); err != nil {
			http.Error(rw, "bad event object", http.StatusBadRequest)
			return
		}
		if err := w.onSubscriptionDeleted(r.Context(), obj.Customer, obj.ID); err != nil {
			log.Printf("billing: customer.subscription.deleted: %v", err)
			http.Error(rw, "processing error", http.StatusInternalServerError)
			return
		}
	default:
		// Unknown event types: 200 ack, no state change (spec §5.2).
	}
	rw.WriteHeader(http.StatusOK)
}

// onCheckoutCompleted upserts the subscription (idempotent on
// stripe_subscription_id) and grants premium.
func (w *WebhookHandler) onCheckoutCompleted(ctx context.Context, userID, customer, subscription string) error {
	_, err := w.db.ExecContext(ctx, `
		INSERT INTO subscriptions (id, user_id, stripe_customer_id, stripe_subscription_id, status)
		VALUES (?, ?, ?, ?, 'active')
		ON CONFLICT(stripe_subscription_id) DO UPDATE SET
			status = 'active',
			stripe_customer_id = excluded.stripe_customer_id
	`, db.NewID(), userID, customer, subscription)
	if err != nil {
		return err
	}
	return SetPremium(ctx, w.db, customer, true)
}

// onSubscriptionDeleted marks the subscription canceled and revokes premium.
func (w *WebhookHandler) onSubscriptionDeleted(ctx context.Context, customer, subscription string) error {
	if _, err := w.db.ExecContext(ctx,
		"UPDATE subscriptions SET status = 'canceled' WHERE stripe_subscription_id = ?", subscription); err != nil {
		return err
	}
	return SetPremium(ctx, w.db, customer, false)
}
