package analytics

import (
	"bytes"
	"encoding/json"
	"net/http"
	"time"

	"risk.lucanian.app/api/internal/config"
)

var (
	enabled bool
	apiKey  string
	apiHost = "https://app.posthog.com"
	httpClient = &http.Client{Timeout: 5 * time.Second}
)

func Init() {
	apiKey = config.PosthogAPIKey
	enabled = apiKey != ""
}

func Capture(event string, distinctID string, properties map[string]interface{}) {
	if !enabled {
		return
	}
	go func() {
		body, _ := json.Marshal(map[string]interface{}{
			"api_key":     apiKey,
			"event":       event,
			"distinct_id": distinctID,
			"properties":  properties,
			"timestamp":   time.Now().UTC().Format(time.RFC3339),
		})
		httpClient.Post(apiHost+"/capture/", "application/json", bytes.NewReader(body))
	}()
}

func CaptureAPIRequest(path, method, ua string, status int) {
	Capture("api_request", path, map[string]interface{}{
		"method": method,
		"ua":     ua,
		"status": status,
	})
}

func CaptureAgentRequest(path, ua string) {
	Capture("agent_request", path, map[string]interface{}{
		"ua": ua,
	})
}
