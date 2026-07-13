# risk

> Platform description here.
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
