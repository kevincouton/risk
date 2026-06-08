package scoring

import (
	"math"
	"time"

	"risk.lucanian.app/api/internal/db"
)

type TrajectorySignals struct {
	ReleaseVelocityDays *int    `json:"release_velocity_days"`
	DaysSinceLastPush   int     `json:"days_since_last_push"`
	OpenIssuesRatio     float64 `json:"open_issues_ratio"`
}

func CalculateTrajectorySignals(entityID string) (*TrajectorySignals, error) {
	var lastPushed string
	var scoreValue, openIssues int
	err := db.DB.QueryRow("SELECT last_pushed_at, score_value, open_issues FROM entities WHERE id = ?", entityID).Scan(&lastPushed, &scoreValue, &openIssues)
	if err != nil {
		return nil, err
	}

	var daysSinceLastPush int
	if t, err := time.Parse(time.RFC3339, lastPushed); err == nil {
		daysSinceLastPush = int(time.Since(t).Hours() / 24)
	}

	openIssuesRatio := 0.0
	if scoreValue > 0 {
		openIssuesRatio = float64(openIssues) / float64(scoreValue)
	}

	rows, err := db.DB.Query("SELECT published_at FROM releases WHERE entity_id = ? ORDER BY published_at DESC LIMIT 5", entityID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var dates []time.Time
	for rows.Next() {
		var d string
		if err := rows.Scan(&d); err == nil {
			if t, err := time.Parse(time.RFC3339, d); err == nil {
				dates = append(dates, t)
			}
		}
	}

	var releaseVelocity *int
	if len(dates) >= 2 {
		totalDays := 0
		for i := 1; i < len(dates); i++ {
			totalDays += int(dates[i-1].Sub(dates[i]).Hours() / 24)
		}
		avg := totalDays / (len(dates) - 1)
		releaseVelocity = &avg
	}

	return &TrajectorySignals{
		ReleaseVelocityDays: releaseVelocity,
		DaysSinceLastPush:   daysSinceLastPush,
		OpenIssuesRatio:     math.Round(openIssuesRatio*1000) / 1000,
	}, nil
}

func TrajectoryScore(signals *TrajectorySignals) float64 {
	score := 50.0
	if signals.ReleaseVelocityDays != nil {
		if *signals.ReleaseVelocityDays < 30 {
			score += 20
		} else if *signals.ReleaseVelocityDays < 60 {
			score += 10
		} else if *signals.ReleaseVelocityDays > 120 {
			score -= 15
		}
	}
	if signals.DaysSinceLastPush < 30 {
		score += 15
	} else if signals.DaysSinceLastPush > 90 {
		score -= 20
	}
	if signals.OpenIssuesRatio < 0.05 {
		score += 10
	} else if signals.OpenIssuesRatio > 0.15 {
		score -= 15
	}
	return math.Max(0, math.Min(100, score))
}
