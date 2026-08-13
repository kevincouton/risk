# Contract suite (Wave R)

Framework-agnostic behavioral contract for the platform chassis server.
Golden fixtures were captured from the Go chassis; the same fixtures verify
the Rust chassis (R-4) and run in CI thereafter. Python 3 stdlib only — no
pip installs, no sqlite3 CLI, no network (a local OIDC discovery stub covers
AUTH_ENABLED groups; Stripe calls fail closed offline, which the fixtures
rely on).

## Layout

- `seed.sql` — deterministic seed: 3 entities / 2 platforms, 2 entity_scores,
  1 user, 2 api_keys (plaintexts `pk_contract_test_key_0001|0002`, SHA-256 at
  rest), api_usage rows (old for k-test, 61 in-window for k-exhausted).
- `cases/*.json` — parity fixtures, 35 (must behave identically on Go and Rust).
- `deltas/*.json` — redesign deltas (spec §5.2), 5, with `go_status`/`rust_status`.
- `capture.py` — runs cases, writes `golden/<case>.json`. Refuses to write on
  any expect mismatch.
- `verify.py` — runs cases and diffs against `golden/`; runs deltas against
  the `--target` side. Exit 1 + diff report on mismatch.
- `schema_dump.py` — migrates fresh / synthetic pre-v2 DBs via the binary's
  own startup and dumps PRAGMA table_info / index_list / sqlite_master.sql
  as canonical JSON.
- `golden/` — captured Go truth (35 case goldens), incl. `schema_fresh.json` /
  `schema_prev2.json`.

## Capture (Go) — already done in R-1; reproduce with:

```bash
/root/platform-templates/bin/instantiate-platform r1contract r1contract.example.com /tmp/r1contract
cd /tmp/r1contract/api && go mod tidy && go build -o /tmp/r1contract/server ./cmd/server
cd /root/platform-templates
python3 contract/capture.py --binary /tmp/r1contract/server --port 18080
```

## Verify (Go, then Rust)

```bash
python3 contract/verify.py --binary /tmp/r1contract/server --port 18081 --target go
python3 contract/verify.py --binary service/target/debug/server --port 18081 --target rust   # R-4 gate
```

Expected on the Go binary: `verify ok (go target): 40 cases green`
(35 parity + 5 deltas).

## Schema dumps

```bash
python3 contract/schema_dump.py --binary <server> --mode fresh --out /tmp/fresh.json
python3 contract/schema_dump.py --binary <server> --mode prev2 --out /tmp/prev2.json
```

Comparison policy (deltas 8a/9): fresh Go vs fresh Rust dumps differ by
exactly `ux_entities_platform_full_name` (Rust does not create the redundant
index on fresh v2 DBs). Pre-v2 dumps differ by the delta-9 table rebuild
(Rust: `UNIQUE(platform, full_name)` table constraint, three recreated
non-unique indexes, no `ux_` index). Everything else must be byte-identical.

## Fixture format

Required keys: `name`, `method`, `path`, `headers` ({}), `body` (null or raw
string), `expect` with `status` and any of `headers` / `json` / `json_subset`
/ `text`. Extensions: `env` (per-case env, `{PORT}`/`{OIDC_PORT}` templated;
cases grouped by env, one server + fresh DB per group), `isolate`,
`slow` (runs last), `repeat` (expectation on the last response),
`stripe_sign` (runner computes Stripe-Signature), `session_cookie` (runner
fabricates a Go-format signed session cookie). Header matchers: exact string,
`{"contains": s}`, `{"contains_all": [...]}`, `{"prefix": s}`,
`{"absent": true}`. Deltas add `go_status`/`rust_status` (+ optional
`go_json`/`rust_json`/`go_text`/`rust_text`).

## Known authoring notes

- `entities`/`search` empty results marshal as `"entities": null` (Go nil
  slice), not `[]`.
- `?limit=999` falls back to the default 50 (Go ignores out-of-range limits).
- `rate_limit_61` is slow and isolated; `x-ratelimit-remaining: 59` on the
  first keyed request is deterministic (usage row inserted before the count).
- `text` expectations (and `go_text`/`rust_text`) compare against the RAW
  response body bytes, even when the body is JSON-parseable — this is what
  pins quirks like the space in `{"error": "missing q parameter"}` and the
  json.Encoder trailing newline. Golden `body` values, by contrast, store the
  parsed JSON value when parseable, so verify.py re-checks each parity case's
  `text` expectation against the live raw body in addition to the golden
  comparison; byte-exact bodies bind both targets.
- Volatile values are normalized before recording/comparing so goldens are
  deterministic: the server port becomes `{PORT}`, the OIDC stub port becomes
  `{OIDC_PORT}`, random OIDC state values become `{STATE}` (in bodies, the
  `location` header, and the `oidc_state` cookie), and API key plaintexts
  minted by POST /api/keys (`pk_` + 32 hex chars) become `{KEY}`.
- The `keys_*` fixtures pin the `/api/keys` management routes (registered only
  when API_KEYS_ENABLED=true AND auth is on). `keys_create`/`keys_delete` are
  `isolate`d because they mutate the seeded api_keys rows; `keys_delete`
  also pins that Go's `w.Write` success body `{"ok":true}` has NO trailing
  newline (unlike the json.Encoder responses).
- Session-cookie *write* path (callback) is not capturable offline — go-oidc
  verifies id_tokens against the issuer JWKS and Python stdlib cannot RSA-sign
  one. Cookie name/flags/lifetime are pinned from `internal/auth/session.go`
  (session; Path=/; Max-Age=604800; HttpOnly; Secure; SameSite=Lax) and the
  read path is pinned by the `auth_me_session` fixture; the live Authentik
  smoke stays owner-pending (spec §10).
- `auth_login_302` pins Go's actual 302 redirect (not topcoat's 307 default);
  R-3 must force 302.
- SPA fallback was ratified as delta 15: unknown paths fall back to
  index.html EXCEPT under `/api/`, `/auth/`, `/healthz`. The
  `delta_spa_fallback` fixture lives in `deltas/` with `go_status: 404`
  (Go's `http.FileServer` has no fallback) and `rust_status: 200` serving the
  index bytes. The `entity_detail_404`, `auth_*_404`, and `billing_*_404`
  parity fixtures pin that Go 404s under the excluded prefixes are preserved.
- `delta_api_key_id_spoof` is a guard case: Go's KeyAuth overwrites the
  spoofed header before RateLimit reads it, so both targets are 200; the case
  pins that identity comes from the validated key (`x-ratelimit-remaining`
  reflects k-test, not the spoofed, exhausted k-exhausted).
