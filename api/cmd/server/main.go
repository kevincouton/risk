package main

import (
	"encoding/json"
	"log"
	"net/http"
	"strconv"
	"strings"
	"time"

	"risk.lucanian.app/api/internal/analytics"
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

	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", handleHealth)
	mux.HandleFunc("/api/v1/entities", handleEntities)
	mux.HandleFunc("/api/v1/entities/", handleEntityDetail)
	mux.HandleFunc("/api/v1/search", handleSearch)
	mux.HandleFunc("/api/v1/stats", handleStats)

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
