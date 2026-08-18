# Risk — Agent Guide

## Project
Risk scores open-source dependencies on release trajectory, maintenance signals, and documentation quality, producing green/yellow/red/critical verdicts.

## Tech Stack
- Rust (API server, `service/` workspace; chassis synced from platform-templates)
- Rust (`collectors/`, clone-owned)
- Nuxt 4 + Vue + Tailwind CSS (frontend)
- SQLite (database)
- Playwright (e2e tests)
- Vite+ (linting/formatting)

## Build
```bash
cd service && cargo build --release
cd collectors && cargo build --release
cd web && npm ci && npm run build
```

## Test
```bash
cd service && cargo test --workspace
python3 contract/verify.py --binary service/target/release/server --port 18115 --target rust
cd web && npm run check && npm run test
cd web && npx playwright install chromium && npm run test:e2e
```

## Run
```bash
API_PORT=31009 DATABASE_PATH=/var/lib/risk/risk.db ./service/target/release/server
```

## Quality & Profiling

- Local CRAP report: `./scripts/crap-report.sh` (reports CRAP ≤ 15 without failing)
- CI gating: the `crap-check` job in `.github/workflows/ci.yml` fails on CRAP > 15 and blocks `deploy`
- Optional hotpath profiling:
  - Service: `cd service && cargo build --release --features hotpath`
  - Collectors: `cd collectors && cargo build --bin ingest --features hotpath`

## gstack Skills
This repo uses gstack skills in `.claude/skills/gstack/`.
Invoke with `/office-hours`, `/plan-ceo-review`, `/review`, `/qa`, etc.

## Local CI with act

You can run GitHub Actions workflows locally using [act](https://github.com/nektos/act):

```bash
act
```

The `.actrc` file sets `--container-architecture linux/amd64` and `--action-offline-mode` for faster local runs.
