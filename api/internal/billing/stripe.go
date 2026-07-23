// Package billing implements Stripe checkout + webhook → entitlement sync,
// stdlib-only (no Stripe SDK). Everything is inert unless BILLING_ENABLED=true.
package billing

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// Package vars are seams for tests.
var (
	stripeAPIBase   = "https://api.stripe.com"
	stripeSecretKey string
	successURL      string
	cancelURL       string
)

// SetSecretKey configures the API key (called from server wiring).
func SetSecretKey(key string) { stripeSecretKey = key }

// SetCheckoutURLs configures the post-checkout redirects (called from server wiring).
func SetCheckoutURLs(success, cancel string) { successURL, cancelURL = success, cancel }

// CreateCheckoutSession creates a Stripe checkout session for priceID and
// returns its hosted URL. userID is carried as client_reference_id so the
// webhook can link the payment back to the user.
func CreateCheckoutSession(ctx context.Context, userID, priceID string) (string, error) {
	form := url.Values{
		"mode":                    {"subscription"},
		"line_items[0][price]":    {priceID},
		"line_items[0][quantity]": {"1"},
		"client_reference_id":     {userID},
		"success_url":             {successURL},
		"cancel_url":              {cancelURL},
	}
	req, err := http.NewRequestWithContext(ctx, "POST",
		stripeAPIBase+"/v1/checkout/sessions", strings.NewReader(form.Encode()))
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+stripeSecretKey)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	client := &http.Client{Timeout: 15 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("stripe checkout: status %d: %s", resp.StatusCode, truncate(string(body), 200))
	}
	var out struct {
		URL string `json:"url"`
	}
	if err := json.Unmarshal(body, &out); err != nil {
		return "", err
	}
	if out.URL == "" {
		return "", fmt.Errorf("stripe checkout: empty url in response")
	}
	return out.URL, nil
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n]
}
