package main

import (
	"context"
	"encoding/json"
	"log"
	"net/http"
	"strconv"
	"strings"
	"time"

	"risk.lucanian.app/api/internal/analytics"
	"risk.lucanian.app/api/internal/apikeys"
	"risk.lucanian.app/api/internal/auth"
	"risk.lucanian.app/api/internal/billing"
	"risk.lucanian.app/api/internal/config"
	"risk.lucanian.app/api/internal/db"
)

type EntityResponse struct {
	ID          string  `json:"id"`
	Platform    string  `json:"platform"`
	FullName    string  `json:"full_name"`
	Description string  `json:"description"`
	Category    string  `json:"category"`
	ScoreValue  int     `json:"score_value"`
	Verdict     string  `json:"verdict,omitempty"`
	Trajectory  string  `json:"trajectory,omitempty"`
	Composite   int     `json:"composite_score,omitempty"`
}

func main() {
	config.MustLoad()
	if err := db.Init(); err != nil {
		log.Fatal(err)
	}
	if err := db.Migrate(); err != nil {
		log.Fatal(err)
	}
	analytics.Init()

	// Auth is opt-in and fail-closed: any misconfiguration disables auth
	// entirely while read-only endpoints keep serving (spec §5.2).
	var authProvider *auth.Provider
	if config.AuthEnabled {
		if len(config.SessionSigningKey) < 32 {
			log.Printf("auth: SESSION_SIGNING_KEY must be at least 32 bytes, auth disabled")
		} else {
			var err error
			authProvider, err = auth.NewProvider(context.Background(), auth.OIDCConfig{
				IssuerURL:    config.OIDCIssuer,
				ClientID:     config.OIDCClientID,
				ClientSecret: config.OIDCClientSecret,
				RedirectURL:  config.AppURL + "/auth/callback",
			}, []byte(config.SessionSigningKey), db.DB)
			if err != nil {
				log.Printf("auth: OIDC discovery failed, auth disabled: %v", err)
				authProvider = nil
			}
		}
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", handleHealth)

	// API-key metering is opt-in and fail-closed: /api/v1/* is gated behind
	// X-API-Key only when API_KEYS_ENABLED=true (spec §5.2).
	if config.APIKeysEnabled {
		wrapV1 := func(h http.HandlerFunc) http.Handler {
			return apikeys.KeyAuth(db.DB)(apikeys.RateLimit(db.DB, 60)(h))
		}
		mux.Handle("/api/v1/entities", wrapV1(handleEntities))
		mux.Handle("/api/v1/entities/", wrapV1(handleEntityDetail))
		mux.Handle("/api/v1/search", wrapV1(handleSearch))
		mux.Handle("/api/v1/stats", wrapV1(handleStats))
	} else {
		mux.HandleFunc("/api/v1/entities", handleEntities)
		mux.HandleFunc("/api/v1/entities/", handleEntityDetail)
		mux.HandleFunc("/api/v1/search", handleSearch)
		mux.HandleFunc("/api/v1/stats", handleStats)
	}

	if authProvider != nil {
		mux.HandleFunc("/auth/login", authProvider.HandleLogin)
		mux.HandleFunc("/auth/callback", authProvider.HandleCallback)
		mux.HandleFunc("/auth/logout", authProvider.HandleLogout)
		mux.HandleFunc("/auth/me", authProvider.HandleMe)
	}

	// Billing is opt-in and fail-closed: no billing routes exist unless
	// BILLING_ENABLED=true (spec §5.2).
	if config.BillingEnabled {
		billing.SetSecretKey(config.StripeSecretKey)
		billing.SetCheckoutURLs(config.AppURL+"/premium?status=success", config.AppURL+"/premium?status=canceled")
		webhook := billing.NewWebhookHandler(db.DB, config.StripeWebhookSecret)
		mux.Handle("/api/billing/webhook", webhook)
		checkout := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			userID := r.URL.Query().Get("user_id")
			if authProvider != nil {
				if u := authProvider.CurrentUser(r); u != nil {
					userID = u.ID
				}
			}
			if userID == "" {
				http.Error(w, `{"error":"authentication required"}`, http.StatusUnauthorized)
				return
			}
			url, err := billing.CreateCheckoutSession(r.Context(), userID, config.StripePriceID)
			if err != nil {
				http.Error(w, `{"error":"checkout failed"}`, http.StatusBadGateway)
				return
			}
			w.Header().Set("Content-Type", "application/json")
			w.Write([]byte(`{"url":"` + url + `"}`))
		})
		if authProvider != nil {
			mux.Handle("/api/billing/checkout", authProvider.RequireAuth(checkout))
		} else {
			mux.Handle("/api/billing/checkout", checkout)
		}
	}

	// Key management endpoints require both API_KEYS_ENABLED and auth.
	if config.APIKeysEnabled && authProvider != nil {
		mux.Handle("/api/keys", authProvider.RequireAuth(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			u := authProvider.CurrentUser(r)
			w.Header().Set("Content-Type", "application/json")
			switch r.Method {
			case http.MethodGet:
				keys, err := apikeys.ListKeys(r.Context(), db.DB, u.ID)
				if err != nil {
					http.Error(w, `{"error":"list failed"}`, http.StatusInternalServerError)
					return
				}
				json.NewEncoder(w).Encode(map[string]interface{}{"keys": keys})
			case http.MethodPost:
				var req struct {
					Label string `json:"label"`
				}
				_ = json.NewDecoder(r.Body).Decode(&req)
				plaintext, err := apikeys.CreateKey(r.Context(), db.DB, u.ID, req.Label)
				if err != nil {
					http.Error(w, `{"error":"create failed"}`, http.StatusInternalServerError)
					return
				}
				json.NewEncoder(w).Encode(map[string]string{"key": plaintext})
			default:
				http.Error(w, `{"error":"method not allowed"}`, http.StatusMethodNotAllowed)
			}
		})))
		mux.Handle("/api/keys/", authProvider.RequireAuth(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.Method != http.MethodDelete {
				http.Error(w, `{"error":"method not allowed"}`, http.StatusMethodNotAllowed)
				return
			}
			u := authProvider.CurrentUser(r)
			keyID := strings.TrimPrefix(r.URL.Path, "/api/keys/")
			if err := apikeys.RevokeKey(r.Context(), db.DB, keyID, u.ID); err != nil {
				http.Error(w, `{"error":"revoke failed"}`, http.StatusNotFound)
				return
			}
			w.Header().Set("Content-Type", "application/json")
			w.Write([]byte(`{"ok":true}`))
		})))
	}

	fs := http.FileServer(http.Dir("./web/dist"))
	mux.Handle("/", fs)

	addr := ":" + config.APIPort
	log.Println("risk server listening on", addr)
	log.Fatal(http.ListenAndServe(addr, cors(analyticsMiddleware(mux))))
}

func analyticsMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		next.ServeHTTP(w, r)
		duration := time.Since(start)

		if r.URL.Path == "/healthz" || !strings.HasPrefix(r.URL.Path, "/api/") {
			return
		}
		analytics.CaptureAPIRequest(r.URL.Path, r.Method, r.UserAgent(), http.StatusOK)
		_ = duration
	})
}

func cors(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Access-Control-Allow-Methods", "GET, OPTIONS")
		w.Header().Set("Access-Control-Allow-Headers", "Content-Type")
		if r.Method == "OPTIONS" {
			w.WriteHeader(http.StatusOK)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func handleHealth(w http.ResponseWriter, r *http.Request) {
	json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func handleEntities(w http.ResponseWriter, r *http.Request) {
	query := r.URL.Query()
	limit := 50
	if l := query.Get("limit"); l != "" {
		if v, err := strconv.Atoi(l); err == nil && v > 0 && v <= 200 {
			limit = v
		}
	}

	var conditions []string
	var args []interface{}

	if category := query.Get("category"); category != "" {
		conditions = append(conditions, "category = ?")
		args = append(args, category)
	}

	whereClause := ""
	if len(conditions) > 0 {
		whereClause = "WHERE " + strings.Join(conditions, " AND ")
	}

	sqlStr := "SELECT id, platform, full_name, description, category, score_value FROM entities " + whereClause + " ORDER BY score_value DESC LIMIT ?"
	args = append(args, limit)

	rows, err := db.DB.Query(sqlStr, args...)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	defer rows.Close()

	var entities []EntityResponse
	for rows.Next() {
		var e EntityResponse
		if err := rows.Scan(&e.ID, &e.Platform, &e.FullName, &e.Description, &e.Category, &e.ScoreValue); err != nil {
			continue
		}
		var composite *int
		var verdict, trajectory *string
		_ = db.DB.QueryRow("SELECT composite_score, verdict, trajectory FROM entity_scores WHERE entity_id = ? ORDER BY scored_at DESC LIMIT 1", e.ID).Scan(&composite, &verdict, &trajectory)
		if composite != nil {
			e.Composite = *composite
		}
		if verdict != nil {
			e.Verdict = *verdict
		}
		if trajectory != nil {
			e.Trajectory = *trajectory
		}
		entities = append(entities, e)
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{
		"entities": entities,
		"limit":    limit,
		"total":    len(entities),
	})
}

func handleEntityDetail(w http.ResponseWriter, r *http.Request) {
	path := strings.TrimPrefix(r.URL.Path, "/api/v1/entities/")
	parts := strings.Split(path, "/")
	if len(parts) != 2 {
		http.Error(w, "invalid path", http.StatusBadRequest)
		return
	}

	platform := r.URL.Query().Get("platform")
	if platform == "" {
		platform = "default"
	}

	var e EntityResponse
	var rawMeta string
	err := db.DB.QueryRow("SELECT id, platform, full_name, description, category, score_value, metadata FROM entities WHERE platform = ? AND full_name = ?", platform, parts[0]+"/"+parts[1]).Scan(
		&e.ID, &e.Platform, &e.FullName, &e.Description, &e.Category, &e.ScoreValue, &rawMeta,
	)
	if err != nil {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}

	var composite *int
	var verdict, trajectory *string
	_ = db.DB.QueryRow("SELECT composite_score, verdict, trajectory FROM entity_scores WHERE entity_id = ? ORDER BY scored_at DESC LIMIT 1", e.ID).Scan(&composite, &verdict, &trajectory)
	if composite != nil {
		e.Composite = *composite
	}
	if verdict != nil {
		e.Verdict = *verdict
	}
	if trajectory != nil {
		e.Trajectory = *trajectory
	}

	var meta map[string]interface{}
	json.Unmarshal([]byte(rawMeta), &meta)

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{
		"entity":       e,
		"raw_metadata": meta,
	})
}

func handleSearch(w http.ResponseWriter, r *http.Request) {
	q := strings.TrimSpace(r.URL.Query().Get("q"))
	if q == "" {
		http.Error(w, `{"error": "missing q parameter"}`, http.StatusBadRequest)
		return
	}

	like := "%" + q + "%"
	rows, err := db.DB.Query(`
		SELECT id, platform, full_name, description, category, score_value
		FROM entities
		WHERE full_name LIKE ? OR description LIKE ?
		ORDER BY score_value DESC
		LIMIT 50
	`, like, like)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	defer rows.Close()

	var entities []EntityResponse
	for rows.Next() {
		var e EntityResponse
		if err := rows.Scan(&e.ID, &e.Platform, &e.FullName, &e.Description, &e.Category, &e.ScoreValue); err != nil {
			continue
		}
		var composite *int
		var verdict *string
		_ = db.DB.QueryRow("SELECT composite_score, verdict FROM entity_scores WHERE entity_id = ? ORDER BY scored_at DESC LIMIT 1", e.ID).Scan(&composite, &verdict)
		if composite != nil {
			e.Composite = *composite
		}
		if verdict != nil {
			e.Verdict = *verdict
		}
		entities = append(entities, e)
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{
		"query":    q,
		"entities": entities,
		"total":    len(entities),
	})
}

func handleStats(w http.ResponseWriter, r *http.Request) {
	var totalEntities, totalScores int
	_ = db.DB.QueryRow("SELECT count(*) FROM entities").Scan(&totalEntities)
	_ = db.DB.QueryRow("SELECT count(*) FROM entity_scores").Scan(&totalScores)

	rows, _ := db.DB.Query("SELECT verdict, count(*) FROM entity_scores GROUP BY verdict ORDER BY count(*) DESC")
	verdicts := map[string]int{}
	if rows != nil {
		defer rows.Close()
		for rows.Next() {
			var v string
			var c int
			if err := rows.Scan(&v, &c); err == nil {
				verdicts[v] = c
			}
		}
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{
		"total_entities": totalEntities,
		"total_scores":   totalScores,
		"verdicts":       verdicts,
	})
}
