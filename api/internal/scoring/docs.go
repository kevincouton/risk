package scoring

import (
	"strings"
)

type DocSignals struct {
	HasReadme              bool `json:"has_readme"`
	HasInstallSection      bool `json:"has_install_section"`
	HasUsageSection        bool `json:"has_usage_section"`
	HasContributingSection bool `json:"has_contributing_section"`
	HasLicenseSection      bool `json:"has_license_section"`
	HasChangelogSection    bool `json:"has_changelog_section"`
	HasExamplesDir         bool `json:"has_examples_dir"`
	HasAPIDocs             bool `json:"has_api_docs"`
	QuickstartEstimatedMin int  `json:"quickstart_estimated_min"`
	CodeBlocksCount        int  `json:"code_blocks_count"`
	ReadmeLength           int  `json:"readme_length"`
}

func AnalyzeReadme(readme string) *DocSignals {
	s := &DocSignals{}
	if readme == "" {
		return s
	}
	s.HasReadme = true
	lower := strings.ToLower(readme)
	s.HasInstallSection = strings.Contains(lower, "## install") || strings.Contains(lower, "## getting started")
	s.HasUsageSection = strings.Contains(lower, "## usage") || strings.Contains(lower, "## how to use")
	s.HasContributingSection = strings.Contains(lower, "## contributing")
	s.HasLicenseSection = strings.Contains(lower, "## license") || strings.Contains(lower, "mit")
	s.HasChangelogSection = strings.Contains(lower, "## changelog") || strings.Contains(lower, "## changes")
	s.HasExamplesDir = strings.Contains(lower, "examples") || strings.Contains(lower, "samples")
	s.HasAPIDocs = strings.Contains(lower, "api") || strings.Contains(lower, "reference")
	s.ReadmeLength = len(readme)
	s.CodeBlocksCount = strings.Count(readme, "```")

	if s.HasInstallSection && s.HasUsageSection {
		s.QuickstartEstimatedMin = 5
	} else if s.HasInstallSection || s.HasUsageSection {
		s.QuickstartEstimatedMin = 10
	} else {
		s.QuickstartEstimatedMin = 30
	}

	return s
}

func DocScore(signals *DocSignals) int {
	if !signals.HasReadme {
		return 0
	}
	score := 20
	if signals.HasInstallSection {
		score += 15
	}
	if signals.HasUsageSection {
		score += 15
	}
	if signals.HasContributingSection {
		score += 10
	}
	if signals.HasLicenseSection {
		score += 5
	}
	if signals.HasChangelogSection {
		score += 10
	}
	if signals.HasExamplesDir {
		score += 10
	}
	if signals.HasAPIDocs {
		score += 10
	}
	if signals.QuickstartEstimatedMin <= 5 {
		score += 5
	}
	if signals.CodeBlocksCount >= 3 {
		score += 5
	}
	if signals.ReadmeLength > 500 {
		score += 5
	}
	if score > 100 {
		return 100
	}
	return score
}

func DocVerdict(score int) string {
	switch {
	case score >= 80:
		return "excellent"
	case score >= 60:
		return "adequate"
	case score >= 40:
		return "poor"
	default:
		return "none"
	}
}
