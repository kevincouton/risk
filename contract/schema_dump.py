#!/usr/bin/env python3
"""Schema dump tool — Wave R, R-1. Python 3 stdlib only.

Usage: schema_dump.py --binary BIN --mode fresh|prev2 --out FILE

fresh: let the binary migrate an empty DB.
prev2: create a synthetic pre-v2 DB (old UNIQUE(platform, slug), without the
       three v2 columns), then let the binary migrate it.

The dump is canonical JSON (sorted keys, 2-space indent) so two dumps diff
cleanly. Comparison policy lives in the consumer (R-2 delta 8a/9, R-6 golden
schema check, spec §6) — this tool only produces faithful dumps.
"""
import argparse
import json
import os
import shutil
import sqlite3
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import capture

# Synthetic pre-v2 schema: entities carries the old table-level
# UNIQUE(platform, slug) and lacks last_pushed_at / open_issues;
# entity_scores lacks release_velocity_days. All other tables match the
# current schema.sql (they have no guarded ALTERs).
PRE_V2_SCHEMA = """
CREATE TABLE entities (
    id TEXT PRIMARY KEY,
    platform TEXT NOT NULL DEFAULT 'default',
    slug TEXT NOT NULL,
    name TEXT NOT NULL,
    full_name TEXT NOT NULL,
    description TEXT,
    category TEXT,
    score_value INTEGER DEFAULT 0,
    metadata TEXT,
    scraped_at TEXT DEFAULT (datetime('now')),
    UNIQUE(platform, slug)
);
CREATE INDEX idx_entities_full_name ON entities(full_name);
CREATE INDEX idx_entities_category ON entities(category);
CREATE INDEX idx_entities_score ON entities(score_value DESC);
CREATE TABLE entity_scores (
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
CREATE INDEX idx_entity_scores_entity_id ON entity_scores(entity_id);
CREATE INDEX idx_entity_scores_composite ON entity_scores(composite_score DESC);
CREATE TABLE taxonomy (
    id TEXT PRIMARY KEY,
    domain TEXT NOT NULL,
    subdomain TEXT,
    niche TEXT,
    full_path TEXT NOT NULL,
    description TEXT,
    parent_id TEXT REFERENCES taxonomy(id),
    UNIQUE(full_path)
);
CREATE TABLE entity_taxonomy (
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    taxonomy_id TEXT NOT NULL REFERENCES taxonomy(id) ON DELETE CASCADE,
    confidence REAL DEFAULT 1.0,
    classified_by TEXT DEFAULT 'manual',
    PRIMARY KEY (entity_id, taxonomy_id)
);
CREATE TABLE star_history (
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    recorded_at TEXT NOT NULL,
    stars INTEGER NOT NULL,
    stars_gained INTEGER DEFAULT 0,
    PRIMARY KEY (entity_id, recorded_at)
);
CREATE TABLE releases (
    id TEXT PRIMARY KEY,
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    tag_name TEXT NOT NULL,
    name TEXT,
    body TEXT,
    is_prerelease INTEGER DEFAULT 0,
    published_at TEXT,
    raw_metadata TEXT
);
CREATE TABLE collector_runs (
    id TEXT PRIMARY KEY,
    collector TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    fetched INTEGER DEFAULT 0,
    upserted INTEGER DEFAULT 0,
    error TEXT
);
CREATE INDEX idx_collector_runs_collector ON collector_runs(collector, started_at DESC);
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    oidc_sub TEXT UNIQUE NOT NULL,
    email TEXT,
    display_name TEXT,
    groups TEXT DEFAULT '[]',
    premium INTEGER DEFAULT 0,
    created_at TEXT DEFAULT (datetime('now')),
    last_login_at TEXT
);
CREATE TABLE subscriptions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    stripe_customer_id TEXT NOT NULL,
    stripe_subscription_id TEXT UNIQUE,
    status TEXT DEFAULT 'active',
    current_period_end TEXT
);
CREATE INDEX idx_subscriptions_customer ON subscriptions(stripe_customer_id);
CREATE TABLE api_keys (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    key_hash TEXT UNIQUE NOT NULL,
    label TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    revoked_at TEXT
);
CREATE TABLE api_usage (
    id TEXT PRIMARY KEY,
    key_id TEXT NOT NULL REFERENCES api_keys(id),
    ts TEXT NOT NULL DEFAULT (datetime('now')),
    endpoint TEXT
);
CREATE INDEX idx_api_usage_key_ts ON api_usage(key_id, ts);
"""


def dump_schema(db_path):
    conn = sqlite3.connect(db_path)
    try:
        tables = [
            r[0]
            for r in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' "
                "AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        out = {"tables": {}}
        for t in tables:
            cols = [
                {"cid": r[0], "name": r[1], "type": r[2], "notnull": r[3],
                 "dflt_value": r[4], "pk": r[5]}
                for r in conn.execute(f"PRAGMA table_info({t})")
            ]
            idxs = [
                {"seq": r[0], "name": r[1], "unique": r[2], "origin": r[3], "partial": r[4]}
                for r in conn.execute(f"PRAGMA index_list({t})")
            ]
            table_sql = conn.execute(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?", (t,)
            ).fetchone()[0]
            index_sql = {
                r[0]: r[1]
                for r in conn.execute(
                    "SELECT name, sql FROM sqlite_master WHERE type='index' "
                    "AND tbl_name=? AND sql IS NOT NULL ORDER BY name",
                    (t,),
                )
            }
            out["tables"][t] = {
                "table_info": cols,
                "index_list": idxs,
                "sql": table_sql,
                "index_sql": index_sql,
            }
        return out
    finally:
        conn.close()


def main():
    ap = argparse.ArgumentParser(description="Migrate a DB with a server binary and dump its schema.")
    ap.add_argument("--binary", required=True)
    ap.add_argument("--port", type=int, default=18082)
    ap.add_argument("--mode", choices=["fresh", "prev2"], required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    workdir = tempfile.mkdtemp(prefix="schema-dump-")
    db_path = os.path.join(workdir, "schema.db")
    try:
        if args.mode == "prev2":
            conn = sqlite3.connect(db_path)
            try:
                conn.executescript(PRE_V2_SCHEMA)
                conn.commit()
            finally:
                conn.close()
        proc = capture.start_server(args.binary, args.port, {}, workdir, db_name="schema.db")
        if not capture.wait_healthz(args.port):
            capture.stop_server(proc)
            print("error: server failed to start; migrations did not complete", file=sys.stderr)
            return 1
        capture.stop_server(proc)
        time.sleep(0.3)
        dump = dump_schema(db_path)
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    with open(args.out, "w") as f:
        json.dump(dump, f, indent=2, sort_keys=True)
        f.write("\n")
    print(f"schema dump ({args.mode}): {len(dump['tables'])} tables -> {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
