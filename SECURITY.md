# Security Policy

## Reporting a Vulnerability

Please report vulnerabilities privately via **GitHub private vulnerability
reporting** (the "Security" tab → "Report a vulnerability") on
[kevincouton/risk](https://github.com/kevincouton/risk/security/advisories/new).
Do not open a public issue for security problems.

## Scope

Risk is a solo-maintained internal tool (Rust API server + collectors, Nuxt
frontend, SQLite) deployed on a private homelab. In scope:

- The API server (`service/`), dependency collectors (`collectors/`), and web
  frontend (`web/`)
- The deployment scripts and CI workflows in this repository
- Dependency vulnerabilities affecting the above

Out of scope: issues in third-party dependencies themselves (report those
upstream), and findings that require already-authenticated access to the
private deployment.

## Response

This is a solo-maintainer project; expect an acknowledgement within a few
days. Fixes for confirmed issues are prioritized over feature work, but no
formal SLA is offered. There are no supported releases — only the `main`
branch receives fixes.
