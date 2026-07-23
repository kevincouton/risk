#!/bin/bash
set -euo pipefail

# Deploy an instantiated platform: build API + web, ship to the VPS,
# restart the systemd unit, verify /healthz, roll back on failure.
# Rollback restores both the previous binary and the previous web dist
# (snapshotted before shipping), restarts, then re-verifies /healthz and
# reports ROLLBACK OK / ROLLBACK FAILED; the script exits non-zero either way.
# Usage: deploy.sh <platform-name>
# Env:   DEPLOY_HOST DEPLOY_USER DEPLOY_PATH DEPLOY_DOMAIN (required)
#        DEPLOY_KEY_FILE (ssh key path; default ./deploy_key)
#        DEPLOY_BINARY   (remote binary path; default /usr/local/bin/<name>-server)
# Run from the clone root (api/ and web/ subdirs).

PLATFORM="${1:?usage: deploy.sh <platform-name>}"
: "${DEPLOY_HOST:?set DEPLOY_HOST}" "${DEPLOY_USER:?set DEPLOY_USER}"
: "${DEPLOY_PATH:?set DEPLOY_PATH}" "${DEPLOY_DOMAIN:?set DEPLOY_DOMAIN}"

KEY="${DEPLOY_KEY_FILE:-deploy_key}"
BINARY="${DEPLOY_BINARY:-/usr/local/bin/${PLATFORM}-server}"
DIST="${DEPLOY_PATH}/web/dist"
SSH=(ssh -i "$KEY" -o StrictHostKeyChecking=no)
RSYNC=(rsync -az -e "ssh -i $KEY -o StrictHostKeyChecking=no")
REMOTE="${DEPLOY_USER}@${DEPLOY_HOST}"

healthz() {
  for _ in $(seq 1 10); do
    if curl -sf "https://${DEPLOY_DOMAIN}/healthz" > /dev/null 2>&1; then
      return 0
    fi
    sleep 3
  done
  return 1
}

rollback() {
  echo "Rolling back to previous release" >&2
  "${SSH[@]}" "$REMOTE" "
    restored=0
    if [ -f '${BINARY}.prev' ]; then mv '${BINARY}.prev' '$BINARY'; restored=1; fi
    if [ -d '${DIST}.prev' ]; then rm -rf '$DIST'; mv '${DIST}.prev' '$DIST'; restored=1; fi
    if [ \"\$restored\" -eq 1 ]; then
      systemctl restart '${PLATFORM}'
    else
      echo 'no previous release to restore' >&2
    fi
  " || true
  if healthz; then
    echo "ROLLBACK OK — service healthy on previous release" >&2
  else
    echo "ROLLBACK FAILED — still unhealthy after rollback; manual intervention needed" >&2
  fi
  exit 1
}

echo "=== Building $PLATFORM ==="
(cd api && go build -ldflags="-s -w" -o "../${PLATFORM}-server.new" cmd/server/main.go)
(cd web && npm ci && npx nuxt generate)

echo "=== Shipping to $REMOTE ==="
"${SSH[@]}" "$REMOTE" "if [ -f '$BINARY' ]; then cp '$BINARY' '${BINARY}.prev'; fi"
"${RSYNC[@]}" "${PLATFORM}-server.new" "$REMOTE:${BINARY}.new"
"${SSH[@]}" "$REMOTE" "if [ -d '$DIST' ]; then rm -rf '${DIST}.prev'; cp -r '$DIST' '${DIST}.prev'; fi"
"${RSYNC[@]}" --delete "web/.output/public/" "$REMOTE:${DIST}/"

echo "=== Activating $PLATFORM ==="
if ! "${SSH[@]}" "$REMOTE" "mv '${BINARY}.new' '$BINARY' && chmod +x '$BINARY' && systemctl restart '${PLATFORM}'"; then
  echo "Activation FAILED" >&2
  rollback
fi

echo "=== Health check ==="
if ! healthz; then
  echo "Health check FAILED" >&2
  rollback
fi

echo "=== Deployed $PLATFORM successfully ==="
