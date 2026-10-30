#!/usr/bin/env bash
#
# Deploy kestrel-studio to a remote host (default: lavo-test).
#
# The image is built ON THE HOST, but the host downloads nothing. Measured
# there: outbound ~65 KB/s versus inbound ~5 MB/s, a 77x asymmetry. So:
#
#   * the runtime base is `scratch`, which Docker never pulls: there is no base
#     image to cache on the host at all;
#   * the server binary is cross-compiled on the dev machine
#     (`make rust-build-linux`) as a static musl ELF, so there is no libc to
#     match on the host and nothing to compile there;
#   * the client assets in `public/` are built on the dev machine too.
#
# What used to be the bulky input — node_modules, ~300 MB uncompressed — is gone
# with the Node pipeline, so the archive is a few MB now.
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
REMOTE_DIR="${DEPLOY_DIR:-/opt/kestrel-studio}"
HOST_PORT="${HOST_PORT:-8848}"

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="/tmp/kestrel-studio-src.tar.gz"
# Build artefacts stay outside the working tree on purpose: nothing here may sit
# next to source files, where a stray `git add -A` would pick it up. Override
# with KESTREL_LINUX_BINARY if the build was pointed somewhere else.
BINARY="${KESTREL_LINUX_BINARY:-/tmp/kestrel-studio-linux/kestrel-studio}"
# The docker context is assembled here, so the Dockerfile's `COPY .build/...`
# has something to read without the artefact ever living in the repository.
STAGING="/tmp/kestrel-studio-deploy"

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

# Preflight, before anything is uploaded: a stale binary used to be discoverable
# only after the container came up and served yesterday's code.
[ -f "$BINARY" ] || fail "no $BINARY — run \`make rust-build-linux\` first"
STALE="$(find "$PROJECT_ROOT/rust/src" -newer "$BINARY" -name '*.rs' 2>/dev/null | head -1)"
[ -z "$STALE" ] || fail "$STALE is newer than $BINARY — run \`make rust-build-linux\` again"
log "  binary: $(du -h "$BINARY" | cut -f1)"

REMOTE_IP="$(ssh -G "$HOST" | awk '/^hostname /{print $2}')"

log "packaging the cross-built binary + client assets (a few MB)"
# An explicit whitelist, deliberately NOT --exclude. macOS bsdtar matches exclude
# patterns at ANY depth, so `--exclude='./storage'` silently dropped
# node_modules/.pnpm/multer@2.4.0/node_modules/multer/storage, which surfaced at
# runtime as `Cannot find module './storage/disk'` — misreported by NestJS as
# "platform-express is not installed". ./data and ./docs did the same damage.
# A whitelist has no pattern matching to go wrong. Add new top-level entries here,
# and never add data/ or storage/: those are host volumes, not build inputs.
#
# COPYFILE_DISABLE stops macOS tar from emitting AppleDouble `._*` sidecars.
rm -rf "$STAGING"
mkdir -p "$STAGING/.build/linux-x86_64"
cp "$BINARY" "$STAGING/.build/linux-x86_64/kestrel-studio"
cp "$PROJECT_ROOT/Dockerfile" "$PROJECT_ROOT/docker-compose.yml" \
   "$PROJECT_ROOT/.dockerignore" "$PROJECT_ROOT/Makefile" "$STAGING/"
cp -R "$PROJECT_ROOT/public" "$STAGING/public"
COPYFILE_DISABLE=1 tar -cf - -C "$STAGING" \
  public \
  .build/linux-x86_64/kestrel-studio \
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

log "building image on the host (FROM scratch: no pull, no compile)"
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
