#!/usr/bin/env bash
#
# Installs the daily hot backup on the host.
#
# This is host state, not something a container carries: a rebuilt or replaced
# host loses it silently, and the failure only shows up on the day a restore is
# needed. The same entry in prose is docs/deployment.md §2.5; this script is the
# reproducible form (run `make backup-cron` after rebuilding a host).
#
# It only writes `/etc/cron.d/kestrel-backup` and checks that a cron daemon is
# running — it does not back up anything itself. Use `make backup-now` for that.
set -euo pipefail

HOST="${DEPLOY_HOST:-lavo-test}"
REMOTE_DIR="${DEPLOY_DIR:-/opt/kestrel-studio}"
BACKUP_DIR="${BACKUP_DIR:-/opt/kestrel-backups}"

log() { printf '\033[36m==>\033[0m %s\n' "$1"; }
fail() { printf '\033[31m!!\033[0m %s\n' "$1" >&2; exit 1; }

ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" 'true' \
  || fail "cannot reach ${HOST} over ssh (is the key loaded?)"

ssh "$HOST" "test -d '${REMOTE_DIR}'" \
  || fail "no ${REMOTE_DIR} on ${HOST} — deploy once before installing the backup job"
ssh "$HOST" "test -x /usr/bin/make" \
  || fail "/usr/bin/make is missing on ${HOST}; the cron line calls it by absolute path"

log "creating ${BACKUP_DIR}"
ssh "$HOST" "install -d -m 0755 '${BACKUP_DIR}'" \
  || fail "could not create ${BACKUP_DIR} (permissions?)"

# Same schedule and log file as docs/deployment.md §2.5. `make backup` runs on the
# host (it is `docker compose exec` + a tarball), which is why the path is baked in.
log "installing /etc/cron.d/kestrel-backup"
if ! {
  printf 'SHELL=/bin/bash\n'
  printf 'PATH=/usr/bin:/bin\n'
  printf '30 3 * * * root cd %s && /usr/bin/make backup >> %s/backup.log 2>&1\n' \
    "$REMOTE_DIR" "$BACKUP_DIR"
} | ssh "$HOST" "umask 022 && cat > /etc/cron.d/kestrel-backup && chmod 0644 /etc/cron.d/kestrel-backup"; then
  fail "could not write /etc/cron.d/kestrel-backup — this needs root on ${HOST}"
fi

log "installed:"
ssh "$HOST" "cat /etc/cron.d/kestrel-backup"

ACTIVE="$(ssh "$HOST" "systemctl is-active crond 2>/dev/null || systemctl is-active cron 2>/dev/null" || true)"
[ "$ACTIVE" = "active" ] || fail "cron daemon is not active on ${HOST} (got: ${ACTIVE:-nothing})"
log "cron daemon: active"
log "next run: 03:30 host local time; log: ${BACKUP_DIR}/backup.log"
