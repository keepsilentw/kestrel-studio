#!/usr/bin/env bash
#
# Puts a previously deployed image back on the host.
#
# `deploy.sh` tags every build with the commit it was made from, so this is the
# way back that does not recompile anything: no zig, no upload, no image build.
# The tags start at the first deploy after 2026-11-20, so an older build has no
# tag and still needs a fresh `make deploy`.
#
# The tag is retagged onto `kestrel-studio:latest` and the container is recreated
# from it. `.env` (session secret, provider key, CLIENT_BUNDLE) is not touched.
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
REMOTE_DIR="${DEPLOY_DIR:-/opt/kestrel-studio}"
HOST_PORT="${HOST_PORT:-8848}"
TAG="${1:-${IMAGE:-}}"

log() { printf '\033[36m==>\033[0m %s\n' "$1"; }
fail() { printf '\033[31m!!\033[0m %s\n' "$1" >&2; exit 1; }

ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" 'true' \
  || fail "cannot reach ${HOST} over ssh (is the key loaded?)"

if [ -z "$TAG" ]; then
  printf 'usage: make rollback IMAGE=<tag>\n\nimages on %s:\n' "$HOST"
  ssh "$HOST" "docker images kestrel-studio --format '  {{.Tag}}\t{{.CreatedSince}}\t{{.Size}}'" || true
  exit 2
fi

ssh "$HOST" "docker image inspect 'kestrel-studio:${TAG}' >/dev/null 2>&1" \
  || fail "no image kestrel-studio:${TAG} on ${HOST}; run without IMAGE to list the tags"

log "pointing kestrel-studio:latest at kestrel-studio:${TAG}"
ssh "$HOST" "docker tag 'kestrel-studio:${TAG}' kestrel-studio:latest"

# --force-recreate: compose compares the container's image id, and retagging does
# not change the id of `latest`, so without this the old container would survive
# a plain `up -d` and the rollback would look like a no-op.
log "recreating the app container (no build, a few seconds of downtime)"
ssh "$HOST" "cd '${REMOTE_DIR}' && HOST_PORT='${HOST_PORT}' docker compose up -d --no-build --force-recreate"

log "waiting for the app to answer"
for _ in $(seq 1 60); do
  if ssh "$HOST" "curl -sf -o /dev/null 'http://127.0.0.1:${HOST_PORT}/login'"; then
    log "rolled back to ${TAG}"
    exit 0
  fi
  sleep 3
done

fail "app did not become healthy after the rollback; inspect: ssh ${HOST} 'cd ${REMOTE_DIR} && docker compose logs --tail=80'"
