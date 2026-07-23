package config

import (
	"os"

	"github.com/joho/godotenv"
)

var (
	DatabaseURL   string
	APIPort       string
	RedisAddr     string
	PosthogAPIKey string
	GaID          string
	AdsID         string

	AuthEnabled       bool
	OIDCIssuer        string
	OIDCClientID      string
	OIDCClientSecret  string
	SessionSigningKey string
	AppURL            string

	BillingEnabled      bool
	StripeSecretKey     string
	StripeWebhookSecret string
	StripePriceID       string

	APIKeysEnabled bool
)

func Load() {
	_ = godotenv.Load("../../.env")

	DatabaseURL = getEnv("DATABASE_URL", "file:/root/risk/risk.db?_pragma=foreign_keys(1)")
	APIPort = getEnv("API_PORT", "8080")
	RedisAddr = getEnv("REDIS_ADDR", "localhost:6379")
	PosthogAPIKey = getEnv("POSTHOG_API_KEY", "")
	GaID = getEnv("GA_ID", "")
	AdsID = getEnv("ADS_ID", "")

	AuthEnabled = getEnv("AUTH_ENABLED", "false") == "true"
	OIDCIssuer = getEnv("OIDC_ISSUER", "")
	OIDCClientID = getEnv("OIDC_CLIENT_ID", "")
	OIDCClientSecret = getEnv("OIDC_CLIENT_SECRET", "")
	SessionSigningKey = getEnv("SESSION_SIGNING_KEY", "")
	AppURL = getEnv("APP_URL", "http://localhost:"+APIPort)

	BillingEnabled = getEnv("BILLING_ENABLED", "false") == "true"
	StripeSecretKey = getEnv("STRIPE_SECRET_KEY", "")
	StripeWebhookSecret = getEnv("STRIPE_WEBHOOK_SECRET", "")
	StripePriceID = getEnv("STRIPE_PRICE_ID", "")

	APIKeysEnabled = getEnv("API_KEYS_ENABLED", "false") == "true"
}

func getEnv(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func MustLoad() {
	Load()
}
