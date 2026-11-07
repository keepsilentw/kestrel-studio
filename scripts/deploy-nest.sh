#!/usr/bin/env bash
#
# Ship the NestJS backend to a remote host (default: lavo-test) and build its
# image there. It does NOT start the service — that is `make nest-up`.
#
# The split is the whole point of this script. Both backends are long-lived
# (docs/roadmap.md §4.5) and they share one SQLite file plus one storage
# directory; that combination has never run at the same time, so "the image is on
# the host" and "the second backend is serving" stay two separate, deliberate
# steps. `make deploy` (Rust) is untouched by any of this.
#
# Like the Rust pipeline, the host downloads nothing: node_modules is shipped from
# the dev machine, and the only thing compiled on the host is better-sqlite3's
# native binding, against the Node headers that came inside the copied Node image
# (Dockerfile.nest). CentOS 7 cannot run Node at all, hence the container.
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
REMOTE_DIR="${DEPLOY_DIR:-/opt/kestrel-studio}"

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="/tmp/kestrel-studio-nest-src.tar.gz"
# Assembled outside the working tree, exactly like the Rust context, so a stray
# `git add -A` can never pick up a 280 MB node_modules.
STAGING="/tmp/kestrel-studio-nest-deploy"
DIST="$PROJECT_ROOT/nest/server/dist/main.js"

log() { printf '\033[36m==>\033[0m %s\n' "$1"; }
fail() { printf '\033[31m!!\033[0m %s\n' "$1" >&2; exit 1; }

log "checking ssh connectivity to ${HOST}"
ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" 'true' \
  || fail "cannot reach ${HOST} over ssh (is the key loaded?)"

# Preflight, before anything is uploaded: the image runs whatever dist/ holds, and
# a stale dist would silently ship yesterday's code.
[ -f "$DIST" ] || fail "no $DIST — run \`make build-nest\` first"
STALE="$(find "$PROJECT_ROOT/nest/server/src" -newer "$DIST" -name '*.ts' 2>/dev/null | head -1)"
[ -z "$STALE" ] || fail "$STALE is newer than dist — run \`make build-nest\` again"

log "packaging dist + node_modules + public (the bulky part, ~280MB uncompressed)"
# An explicit whitelist, never --exclude: macOS bsdtar matches exclude patterns at
# ANY depth, and a bare `storage` once deleted node_modules/.pnpm/multer@…/storage
# (deployment.md records the incident). Also never add data/ or storage/: those are
# host volumes, not build inputs. node_modules is uploaded whole each time — the
# archive is ~90 MB and the host's inbound bandwidth is ~5 MB/s, so caching it
# would buy minutes while adding a "which node_modules is this image from" question.
#
# COPYFILE_DISABLE stops macOS tar from emitting AppleDouble `._*` sidecars.
rm -rf "$STAGING"
mkdir -p "$STAGING/nest/server"
cp -R "$PROJECT_ROOT/node_modules" "$STAGING/node_modules"
cp -R "$PROJECT_ROOT/nest/server/node_modules" "$STAGING/nest/server/node_modules"
cp -R "$PROJECT_ROOT/nest/server/dist" "$STAGING/nest/server/dist"
cp -R "$PROJECT_ROOT/public" "$STAGING/public"
# docker-compose.yml travels with this context on purpose: without it the host is
# left holding the compose file from the last Rust deploy, which has no `nest`
# service, and the build below fails with "no such service: nest" (hit on the
# first run of this script, 2026-11-07).
cp "$PROJECT_ROOT/Dockerfile.nest" "$PROJECT_ROOT/docker-compose.yml" \
   "$PROJECT_ROOT/.dockerignore" "$STAGING/"
COPYFILE_DISABLE=1 tar -cf - -C "$STAGING" \
  node_modules \
  nest/server/node_modules \
  nest/server/dist \
  public \
  Dockerfile.nest \
  docker-compose.yml \
  .dockerignore \
  | gzip -1 > "$ARCHIVE"
log "  archive: $(du -h "$ARCHIVE" | cut -f1)"

log "uploading to ${HOST}:${REMOTE_DIR}"
ssh "$HOST" "mkdir -p '${REMOTE_DIR}'"
scp -q "$ARCHIVE" "${HOST}:${REMOTE_DIR}/nest-src.tar.gz"

log "unpacking"
ssh "$HOST" "tar -xzf '${REMOTE_DIR}/nest-src.tar.gz' -C '${REMOTE_DIR}' && rm -f '${REMOTE_DIR}/nest-src.tar.gz'"

log "building the image on the host (base layers are already cached; better-sqlite3 compiles here)"
# No `NEST_HOST_PORT=...` prefix here: compose resolves its own `${NEST_HOST_PORT:-8849}`
# default, and a quoted value handed over ssh arrives as literal text, which compose then
# tried to parse as an IP (2026-11-07, first run). The port is irrelevant to a build anyway.
ssh "$HOST" "cd '${REMOTE_DIR}' && docker compose --profile nest build nest"

log "image ready; nothing was started"
cat <<EOF

  To bring the second backend up (it shares data/kestrel-studio.db and storage/
  with the Rust container — see docs/roadmap.md §4.5 before doing it):

      make nest-up          # docker compose --profile nest up -d nest
      make nest-down        # stop it again, Rust keeps serving

  To look at it without the live database, run it against a copy first:

      cp ${REMOTE_DIR}/data/kestrel-studio.db /tmp/nest-smoke/data/kestrel-studio.db
      docker run --rm --env-file ${REMOTE_DIR}/.env \\
        -e PORT=8849 -e ACCESS_LOG=1 \\
        -v /tmp/nest-smoke/data:/app/data -v /tmp/nest-smoke/storage:/app/storage \\
        -p 127.0.0.1:8849:8849 kestrel-studio-nest:latest
EOF
