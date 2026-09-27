#!/usr/bin/env bash
#
# Deploy kestrel-studio to a remote host (default: lavo-test).
#
# The image is built ON THE HOST, but the host downloads nothing. Measured
# there: outbound ~65 KB/s versus inbound ~5 MB/s, a 77x asymmetry. So:
#
#   * both base images (node:24-bookworm-slim, golang:1.24) are already cached
#     on the host — go pull them once with scripts/bootstrap-host.sh if the
#     build reports a missing image;
#   * the C++ toolchain for better-sqlite3 comes from the golang image and the
#     Node runtime from the node image, so no apt install happens;
#   * node_modules — the only bulky input — is shipped in the build context
#     from the dev machine instead of being installed on the host.
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
REMOTE_DIR="${DEPLOY_DIR:-/opt/kestrel-studio}"
HOST_PORT="${HOST_PORT:-8848}"

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="/tmp/kestrel-studio-src.tar.gz"

log() { printf '\033[36m==>\033[0m %s\n' "$1"; }
fail() { printf '\033[31m!!\033[0m %s\n' "$1" >&2; exit 1; }

read_api_key() {
  python3 - <<'PY'
import json
import sqlite3
from pathlib import Path

db_path = Path.home() / ".cc-switch" / "cc-switch.db"
try:
    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    row = db.execute(
        "SELECT settings_config FROM providers "
        "WHERE id = 'bailian-token-plan' AND app_type = 'codex'"
    ).fetchone()
    print(json.loads(row[0])["auth"]["OPENAI_API_KEY"] if row else "", end="")
except Exception:
    print("", end="")
PY
}

log "checking ssh connectivity to ${HOST}"
ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" 'true' \
  || fail "cannot reach ${HOST} over ssh (is the key loaded?)"

REMOTE_IP="$(ssh -G "$HOST" | awk '/^hostname /{print $2}')"

log "packaging source + node_modules (the bulky part, ~300MB uncompressed)"
# An explicit whitelist, deliberately NOT --exclude. macOS bsdtar matches exclude
# patterns at ANY depth, so `--exclude='./storage'` silently dropped
# node_modules/.pnpm/multer@2.4.0/node_modules/multer/storage, which surfaced at
# runtime as `Cannot find module './storage/disk'` — misreported by NestJS as
# "platform-express is not installed". ./data and ./docs did the same damage.
# A whitelist has no pattern matching to go wrong. Add new top-level entries here.
#
# COPYFILE_DISABLE stops macOS tar from emitting AppleDouble `._*` sidecars.
COPYFILE_DISABLE=1 tar -cf - -C "$PROJECT_ROOT" \
  src \
  dist \
  public \
  node_modules \
  package.json \
  pnpm-lock.yaml \
  pnpm-workspace.yaml \
  tsconfig.json \
  nest-cli.json \
  drizzle.config.ts \
  vite.config.mts \
  Dockerfile \
  docker-compose.yml \
  .dockerignore \
  Makefile \
  | gzip -1 > "$ARCHIVE"
log "  archive: $(du -h "$ARCHIVE" | cut -f1)"

log "uploading to ${HOST}:${REMOTE_DIR}"
ssh "$HOST" "mkdir -p '${REMOTE_DIR}'"
scp -q "$ARCHIVE" "${HOST}:${REMOTE_DIR}/src.tar.gz"

log "unpacking"
ssh "$HOST" "tar -xzf '${REMOTE_DIR}/src.tar.gz' -C '${REMOTE_DIR}' && rm -f '${REMOTE_DIR}/src.tar.gz'"

# Secrets live only on the host. Create them on first deploy and never rewrite
# an existing file, so restarts keep the same session secret and stay logged in.
if ssh "$HOST" "test -f '${REMOTE_DIR}/.env'"; then
  log "keeping existing .env on host"
else
  log "creating .env on host"
  API_KEY="$(read_api_key)"
  [ -n "$API_KEY" ] || fail "no Bailian API key found in cc-switch; set BAILIAN_API_KEY and retry"

  # The super admin is whatever the operator exports — the repository ships no
  # credentials, and a fresh host must not end up with a guessable /admin.
  [ -n "${SUPER_ADMIN_USERNAME:-}" ] || fail "SUPER_ADMIN_USERNAME is not set; export it and retry"
  [ -n "${SUPER_ADMIN_PASSWORD:-}" ] || fail "SUPER_ADMIN_PASSWORD is not set; export it and retry"

  SESSION_SECRET="$(openssl rand -hex 32)"
  printf 'BAILIAN_API_KEY=%s\nSESSION_SECRET=%s\nSUPER_ADMIN_USERNAME=%s\nSUPER_ADMIN_PASSWORD=%s\nPORT=8848\n' \
    "$API_KEY" "$SESSION_SECRET" "$SUPER_ADMIN_USERNAME" "$SUPER_ADMIN_PASSWORD" \
    | ssh "$HOST" "umask 077 && cat > '${REMOTE_DIR}/.env'"
fi

log "building image on the host (no downloads; native compile takes a few minutes)"
ssh "$HOST" "cd '${REMOTE_DIR}' && HOST_PORT='${HOST_PORT}' docker compose up -d --build"

log "waiting for the app to answer"
for _ in $(seq 1 60); do
  if ssh "$HOST" "curl -sf -o /dev/null 'http://127.0.0.1:${HOST_PORT}/login'"; then
    log "deployed -> http://${REMOTE_IP}:${HOST_PORT}"
    exit 0
  fi
  sleep 3
done

fail "app did not become healthy; inspect: ssh ${HOST} 'cd ${REMOTE_DIR} && docker compose logs --tail=80'"
