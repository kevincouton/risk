# risk

> Risk scores open-source dependencies on release trajectory, maintenance
> signals, and documentation quality, producing green/yellow/red/critical
> verdicts at risk.lucanian.app.
>
> **Status: early development.** The scoring engine is implemented; the
> data-ingestion pipeline (deps.dev + GitHub collectors) is planned next.
> Domain: `risk.lucanian.app`

---

## Status

Early development. Bootstrapped from platform-templates.

## Stack

- Backend: Go (SQLite, REST API)
- Frontend: Nuxt 4 (Vite+, Tailwind)
- Database: SQLite
- Deployment: systemd + Caddy

## Quick Start

```bash
# Backend
cd api && go mod tidy && go run cmd/server/main.go

# Frontend
cd web && npm ci && npm run dev
```

## CI

```bash
# Local CI verification
act --container-daemon-socket /run/podman/podman.sock
```

## Local CI with act

You can run GitHub Actions workflows locally using [act](https://github.com/nektos/act):

```bash
act
```

The `.actrc` file sets `--container-architecture linux/amd64` and `--action-offline-mode` for faster local runs.
