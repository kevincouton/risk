-- risk Platform Schema
-- Compatible with SQLite (local dev) and PostgreSQL (production)

CREATE TABLE IF NOT EXISTS entities (
    id TEXT PRIMARY KEY,
    platform TEXT NOT NULL DEFAULT 'default',
    slug TEXT NOT NULL,
    name TEXT NOT NULL,
    full_name TEXT NOT NULL,
    description TEXT,
    category TEXT,
    score_value INTEGER DEFAULT 0,
    metadata TEXT,
    last_pushed_at TEXT,
    open_issues INTEGER DEFAULT 0,
    scraped_at TEXT DEFAULT (datetime('now')),
    UNIQUE(platform, slug)
);

CREATE INDEX IF NOT EXISTS idx_entities_full_name ON entities(full_name);
CREATE INDEX IF NOT EXISTS idx_entities_category ON entities(category);
CREATE INDEX IF NOT EXISTS idx_entities_score ON entities(score_value DESC);

CREATE TABLE IF NOT EXISTS entity_scores (
    id TEXT PRIMARY KEY,
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    scored_at TEXT DEFAULT (datetime('now')),
    trajectory_score REAL,
    doc_score INTEGER CHECK (doc_score BETWEEN 0 AND 100),
    popularity_score REAL,
    composite_score INTEGER CHECK (composite_score BETWEEN 0 AND 100),
    verdict TEXT DEFAULT 'unknown',
    trajectory TEXT DEFAULT 'unknown',
    calculation_version INTEGER DEFAULT 1,
    raw_signals TEXT
);

CREATE INDEX IF NOT EXISTS idx_entity_scores_entity_id ON entity_scores(entity_id);
CREATE INDEX IF NOT EXISTS idx_entity_scores_composite ON entity_scores(composite_score DESC);

CREATE TABLE IF NOT EXISTS taxonomy (
    id TEXT PRIMARY KEY,
    domain TEXT NOT NULL,
    subdomain TEXT,
    niche TEXT,
    full_path TEXT NOT NULL,
    description TEXT,
    parent_id TEXT REFERENCES taxonomy(id),
    UNIQUE(full_path)
);

CREATE TABLE IF NOT EXISTS entity_taxonomy (
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    taxonomy_id TEXT NOT NULL REFERENCES taxonomy(id) ON DELETE CASCADE,
    confidence REAL DEFAULT 1.0,
    classified_by TEXT DEFAULT 'manual',
    PRIMARY KEY (entity_id, taxonomy_id)
);

CREATE TABLE IF NOT EXISTS star_history (
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    recorded_at TEXT NOT NULL,
    stars INTEGER NOT NULL,
    stars_gained INTEGER DEFAULT 0,
    PRIMARY KEY (entity_id, recorded_at)
);

CREATE TABLE IF NOT EXISTS releases (
    id TEXT PRIMARY KEY,
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    tag_name TEXT NOT NULL,
    name TEXT,
    body TEXT,
    is_prerelease INTEGER DEFAULT 0,
    published_at TEXT,
    raw_metadata TEXT
);
