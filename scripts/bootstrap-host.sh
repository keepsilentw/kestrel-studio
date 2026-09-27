#!/usr/bin/env bash
#
# One-time preparation of a fresh deploy host: cache the two base images the
# build consumes. After this runs, `make deploy` needs no network on the host.
#
# dockerproxy.net is used because the host cannot reach registry-1.docker.io,
# and of the mirrors tried the others either timed out or crawled (see
# .claude/skills/deploy/SKILL.md for the measurements).
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
MIRROR="${DOCKER_MIRROR:-dockerproxy.net}"

log() { printf '\033[36m==>\033[0m %s\n' "$1"; }

log "caching node base image on ${HOST} (Node runtime)"
ssh -o BatchMode=yes "$HOST" "docker pull '${MIRROR}/library/node:24-bookworm-slim'"

log "caching golang base image on ${HOST} (supplies gcc/g++/make/python3)"
ssh -o BatchMode=yes "$HOST" \
  "docker pull '${MIRROR}/library/golang:1.24' && docker tag '${MIRROR}/library/golang:1.24' golang:1.24"

log "images now present on ${HOST}:"
ssh -o BatchMode=yes "$HOST" "docker images --format '{{.Repository}}:{{.Tag}}' | grep -E 'node|golang' || true"
