#!/usr/bin/env bash
#
# One-time preparation of a fresh deploy host: cache the two base images the
# build consumes. After this runs, `make deploy` needs no network on the host.
#
# The Rust pipeline needs no image at all: the runtime base is `scratch` (Docker
# never pulls it) and the server binary is cross-compiled on the dev machine.
# So on a fresh host this script only checks that Docker exists.
#
# Set NEST_BASE_IMAGES=1 to cache the old Node pipeline's two base images —
# that is the rollback path until the WASM client lands, and the images are only
# pulled on demand because dockerproxy.net is the only mirror that ever worked
# from this host (the others timed out or crawled; see .claude/skills/deploy/SKILL.md).
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
MIRROR="${DOCKER_MIRROR:-dockerproxy.net}"

log() { printf '\033[36m==>\033[0m %s\n' "$1"; }

log "checking docker on ${HOST}"
ssh -o BatchMode=yes "$HOST" 'docker --version' \
  || { echo "!! ${HOST} has no working docker — install it before deploying" >&2; exit 1; }
log "no base image to cache: the runtime base is scratch"

if [ "${NEST_BASE_IMAGES:-0}" = "1" ]; then
  log "caching the transitional Nest pipeline base images"
  ssh -o BatchMode=yes "$HOST" "docker pull '${MIRROR}/library/node:24-bookworm-slim'"
  ssh -o BatchMode=yes "$HOST" \
    "docker pull '${MIRROR}/library/golang:1.24' && docker tag '${MIRROR}/library/golang:1.24' golang:1.24"
  log "images now present on ${HOST}:"
  ssh -o BatchMode=yes "$HOST" "docker images --format '{{.Repository}}:{{.Tag}}' | grep -E 'node|golang' || true"
fi
