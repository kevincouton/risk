-- Contract seed dataset (Wave R, R-1).
-- Applied with Python sqlite3 to a temp DB AFTER the server under test has run
-- its own migrations once (tables must exist). Rows are deterministic; all
-- rate-limit-sensitive timestamps sit outside the 60-second sliding window.
-- Test API keys (plaintext known to the runner):
--   pk_contract_test_key_0001 -> k-test       (fresh, 0 in-window uses)
--   pk_contract_test_key_0002 -> k-exhausted  (61 in-window uses; spoof-guard)

INSERT INTO entities (id, platform, slug, name, full_name, description, category, score_value, metadata, last_pushed_at, open_issues, scraped_at) VALUES
('e-alpha', 'default', 'alpha', 'Alpha', 'acme/alpha', 'Alpha toolkit', 'tools', 90, '{"stars": 100}', '2026-07-01 00:00:00', 3, '2026-07-20 00:00:00'),
('e-beta',  'default', 'beta',  'Beta',  'acme/beta',  'Beta docs site', 'docs', 70, '{"stars": 50}',  NULL, 0, '2026-07-20 00:00:01'),
('e-gamma', 'gitlab',  'gamma', 'Gamma', 'acme/gamma', 'Gamma CLI',      'tools', 80, '{"stars": 75}',  NULL, 1, '2026-07-20 00:00:02');
-- NOTE: description/category must be non-NULL: the Go list/search handlers
-- rows.Scan into plain strings and silently SKIP rows with NULLs.

INSERT INTO entity_scores (id, entity_id, scored_at, trajectory_score, doc_score, popularity_score, composite_score, verdict, trajectory, calculation_version, raw_signals, release_velocity_days) VALUES
('s-alpha', 'e-alpha', '2026-07-21 00:00:00', 0.8, 90, 0.9, 88, 'strong', 'rising', 1, '{}', 14),
('s-beta',  'e-beta',  '2026-07-21 00:00:01', 0.5, 60, 0.6, 65, 'fair',   'stable', 1, '{}', 30);
-- e-gamma intentionally has NO score row: pins serde skip_serializing_if /
-- Go omitempty parity (verdict/trajectory/composite_score keys absent).

INSERT INTO users (id, oidc_sub, email, display_name, groups, premium, created_at, last_login_at) VALUES
('u-test', 'sub-contract-test', 'test@example.com', 'Test User', '["admin"]', 1, '2026-07-01 00:00:00', '2026-07-20 00:00:00');

INSERT INTO api_keys (id, user_id, key_hash, label, created_at, revoked_at) VALUES
('k-test',      'u-test', 'e4238310961ef5b6df751f17b0b4cc92c18ae4a13541fda812bf5a878b008516', 'contract test key', '2026-07-01 00:00:00', NULL),
('k-exhausted', 'u-test', '28679443ac7ac01706d0f63cdba240e051c1005ebfa74ccd13514c3ddebe6b8d', 'exhausted key',     '2026-07-01 00:00:01', NULL);

-- Old k-test usage: exercises api_usage shape without touching the 60s window.
INSERT INTO api_usage (id, key_id, ts, endpoint) VALUES
('au-1', 'k-test', datetime('now', '-2 hours'), '/api/v1/stats'),
('au-2', 'k-test', datetime('now', '-2 hours'), '/api/v1/entities'),
('au-3', 'k-test', datetime('now', '-3 hours'), '/api/v1/search');

-- 61 recent rows for k-exhausted: if a server ever trusted a spoofed
-- X-API-Key-ID pointing at this key, the spoof-guard delta case would 429.
WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 61)
INSERT INTO api_usage (id, key_id, ts, endpoint)
SELECT 'aux-' || n, 'k-exhausted', datetime('now', '-30 seconds'), '/api/v1/stats' FROM c;
