package scoring

import (
	"encoding/json"
	"math"

	"risk.lucanian.app/api/internal/db"
	"risk.lucanian.app/api/internal/platform"
)

// ScoreResult holds the complete score for an entity.
type ScoreResult struct {
	EntityID            string             `json:"entity_id"`
	TrajectorySignals   *TrajectorySignals `json:"trajectory_signals"`
	DocSignals          *DocSignals        `json:"doc_signals"`
	TrajectoryScore     float64            `json:"trajectory_score"`
	DocScore            int                `json:"doc_score"`
	CompositeScore      int                `json:"composite_score"`
	Verdict             string             `json:"verdict"`
	Trajectory          string             `json:"trajectory"`
	DocVerdict          string             `json:"doc_verdict"`
	ReleaseVelocityDays *int               `json:"release_velocity_days"`
}

const (
	TrajectoryWeight = 0.45
	DocWeight        = 0.35
	PopularityWeight = 0.20
)

func ScoreEntity(entityID string, fetcher platform.ReadmeFetcher) (*ScoreResult, error) {
	result := &ScoreResult{EntityID: entityID}

	ts, err := CalculateTrajectorySignals(entityID)
	if err != nil {
		return nil, err
	}
	result.TrajectorySignals = ts
	result.TrajectoryScore = TrajectoryScore(ts)
	result.ReleaseVelocityDays = ts.ReleaseVelocityDays

	var owner, name string
	_ = db.DB.QueryRow("SELECT slug, name FROM entities WHERE id = ?", entityID).Scan(&owner, &name)

	var docSignals *DocSignals
	if fetcher != nil && owner != "" && name != "" {
		readme, err := fetcher.GetReadme(owner, name)
		if err == nil {
			docSignals = AnalyzeReadme(readme)
		}
	}
	if docSignals == nil {
		docSignals = &DocSignals{}
	}
	result.DocSignals = docSignals
	result.DocScore = DocScore(docSignals)
	result.DocVerdict = DocVerdict(result.DocScore)

	var scoreValue int
	_ = db.DB.QueryRow("SELECT score_value FROM entities WHERE id = ?", entityID).Scan(&scoreValue)
	popularityScore := math.Min(100, float64(scoreValue)/1000.0*10.0)

	composite := TrajectoryWeight*result.TrajectoryScore +
		DocWeight*float64(result.DocScore) +
		PopularityWeight*popularityScore

	result.CompositeScore = int(math.Round(composite))
	result.Verdict = assignVerdict(result.CompositeScore, ts)
	result.Trajectory = assignTrajectory(ts)

	return result, nil
}

func assignVerdict(score int, signals *TrajectorySignals) string {
	if signals.DaysSinceLastPush > 365 {
		return "red"
	}
	if signals.OpenIssuesRatio > 0.2 {
		return "red"
	}
	switch {
	case score >= 70:
		return "green"
	case score >= 50:
		return "yellow"
	case score >= 30:
		return "red"
	default:
		return "critical"
	}
}

func assignTrajectory(signals *TrajectorySignals) string {
	if signals.ReleaseVelocityDays == nil {
		return "unknown"
	}
	if signals.DaysSinceLastPush < 30 && *signals.ReleaseVelocityDays < 45 {
		return "accelerating"
	}
	if signals.DaysSinceLastPush < 90 && *signals.ReleaseVelocityDays < 90 {
		return "plateauing"
	}
	if signals.DaysSinceLastPush > 180 {
		return "declining"
	}
	return "plateauing"
}

func SaveScore(result *ScoreResult) error {
	signalsJSON, _ := json.Marshal(result.TrajectorySignals)

	_, err := db.DB.Exec(`
		INSERT INTO entity_scores (
			id, entity_id, release_velocity_days, doc_score, composite_score,
			verdict, trajectory, calculation_version, raw_signals
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
	`, db.NewID(), result.EntityID, result.ReleaseVelocityDays, result.DocScore,
		result.CompositeScore, result.Verdict, result.Trajectory,
		1, string(signalsJSON))
	return err
}
