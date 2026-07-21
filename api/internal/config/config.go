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
)

func Load() {
	_ = godotenv.Load("../../.env")

	DatabaseURL = getEnv("DATABASE_URL", "file:/root/risk/risk.db?_pragma=foreign_keys(1)")
	APIPort = getEnv("API_PORT", "8080")
	RedisAddr = getEnv("REDIS_ADDR", "localhost:6379")
	PosthogAPIKey = getEnv("POSTHOG_API_KEY", "")
	GaID = getEnv("GA_ID", "")
	AdsID = getEnv("ADS_ID", "")
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
