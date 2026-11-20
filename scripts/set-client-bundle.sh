#!/usr/bin/env bash
#
# Chooses which browser bundle the pages load — durably.
#
# `CLIENT_BUNDLE` is deployment config, so compose reads it from the host's
# `.env`; `deploy.sh` never rewrites that file, which is exactly why a value
# written there survives every deploy. Passing the variable on a
# `docker compose up -d` command line does not: the next deploy silently drops it
# and the pages fall back to `nest` (docs/deployment.md §2.2).
#
# Usage: scripts/set-client-bundle.sh wasm|nest
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
REMOTE_DIR="${DEPLOY_DIR:-/opt/kestrel-studio}"
HOST_PORT="${HOST_PORT:-8848}"
TIER="${1:-}"

log() { printf '\033[36m==>\033[0m %s\n' "$1"; }
fail() { printf '\033[31m!!\033[0m %s\n' "$1" >&2; exit 1; }

case "$TIER" in
  wasm|nest) ;;
  *) echo "usage: make tier-wasm | make tier-nest" >&2; exit 2 ;;
esac

ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" 'true' \
  || fail "cannot reach ${HOST} over ssh (is the key loaded?)"

ssh "$HOST" "test -f '${REMOTE_DIR}/.env'" \
  || fail "no ${REMOTE_DIR}/.env on ${HOST} yet — run make deploy first"

log "writing CLIENT_BUNDLE=${TIER} into ${REMOTE_DIR}/.env"
# Rewrite only that key: the session secret and the provider key must survive
# untouched, and 0600 has to stay 0600. `grep -v` returning 1 (key never there)
# is the normal first run, not an error.
ssh "$HOST" "cd '${REMOTE_DIR}' && { grep -v '^CLIENT_BUNDLE=' .env || true; } > .env.tmp \
  && printf 'CLIENT_BUNDLE=%s\n' '${TIER}' >> .env.tmp \
  && chmod 600 .env.tmp && mv .env.tmp .env" \
  || fail "could not rewrite ${REMOTE_DIR}/.env (permissions?)"

log "recreating the app container (no build, a few seconds of downtime)"
ssh "$HOST" "cd '${REMOTE_DIR}' && HOST_PORT='${HOST_PORT}' docker compose up -d --no-build --force-recreate"

log "waiting for the app to answer"
for _ in $(seq 1 60); do
  if ssh "$HOST" "curl -sf -o /dev/null 'http://127.0.0.1:${HOST_PORT}/login'"; then
    break
  fi
  sleep 3
done
ssh "$HOST" "curl -sf -o /dev/null 'http://127.0.0.1:${HOST_PORT}/login'" \
  || fail "app did not become healthy; inspect: ssh ${HOST} 'cd ${REMOTE_DIR} && docker compose logs --tail=80'"

# Read the tier back from the one place the server states it (main.rs logs
# `client=` at startup) instead of trusting that the file edit took effect.
# Both `client=wasm` and `client="wasm"` are accepted: the quoting is tracing's.
ACTUAL="$(ssh "$HOST" "docker logs --tail=40 kestrel-studio 2>&1" \
  | grep -oE 'client="?[a-z]+"?' | tail -1 | tr -d '"' || true)"
[ -n "$ACTUAL" ] || fail "container is up but the startup log has no client= line"
[ "$ACTUAL" = "client=${TIER}" ] || fail "startup log says ${ACTUAL}, expected client=${TIER}"
log "tier is now ${TIER} (${ACTUAL})"
