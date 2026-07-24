#!/bin/sh
# Restart the host services onto the freshly built binary and PROVE the
# dispatcher actually came back up — rolling back to the previous binary if it
# didn't. Called by update.sh at the end of a deploy (steps 6-7), and runnable
# by hand.
#
# Usage: restart-verify.sh <target-sha> <prev-sha>
#
# Why a DISPATCHER-specific probe (not a curl to the api): the api is its own
# launchd service and can answer HTTP while the dispatcher crash-loops next to
# it — the 2026-07-22 fleet-startup outage was exactly this (the api returned
# `dispatcher unavailable: no responders` while launchd retried a doomed
# dispatcher every 10s for ~40 min with nothing watching). So we make a real
# NATS request that only a live dispatcher answers — `req.jobs.list` — via
# nats-box on the NATS network (the same pattern deploy/backup.sh uses). A
# crash-looping dispatcher has no responder and the request fails.
#
# Why it ignores SIGHUP: this runs on the Mini over ssh from the deploy job's
# work container, and it restarts the dispatcher SUPERVISING that very
# container. On some paths §3.6 reconciliation can reap the container mid-run,
# dropping the ssh session — `trap '' HUP` lets the health check + rollback run
# to completion on the Mini regardless, so a bad build never leaves launchd
# crash-looping unwatched. Reconciliation marks the deploy task on the
# dispatcher's next start; prod is kept up here.
set -eu
trap '' HUP

HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod

# Load prod config (NATS_NETWORK, KEYS_DIR) unless the caller already exported it
# (update.sh does) or a test opts out. The gitignored env file only exists on a
# bootstrapped Mini, so the guard also keeps hand/test runs off it.
if [ -z "${CHUG_HEALTH_NO_ENV:-}" ] && [ -f "$HERE/chuggernaut.env" ]; then
  set -a
  . "$HERE/chuggernaut.env"
  set +a
fi

TARGET_SHA="${1:?usage: restart-verify.sh <target-sha> <prev-sha>}"
PREV_SHA="${2:-unknown}"

CHUG_REPO="${CHUG_REPO:-$HOME/chuggernaut}"
BIN="$CHUG_REPO/target/release/chuggernaut"
PREV_BIN="$BIN.prev"
UID_NUM="$(id -u)"

# Poll budget. Overridable for tests; the defaults give ~60s of retry.
HEALTH_TIMEOUT_SECS="${HEALTH_TIMEOUT_SECS:-60}"
HEALTH_INTERVAL_SECS="${HEALTH_INTERVAL_SECS:-3}"

# Preflight: is the docker engine up? The dispatcher liveness probe runs inside
# `docker run ... nats-box`, so a DOWN engine makes every probe fail and would
# roll back a perfectly good binary — mistaking docker-down for dispatcher-down
# (the failure this ticket fixes). docker-down gets its own exit code and does
# NOT roll back. Overridable via DOCKER_PREFLIGHT_CMD for the shell test.
docker_up() {
  if [ -n "${DOCKER_PREFLIGHT_CMD:-}" ]; then
    sh -c "$DOCKER_PREFLIGHT_CMD" >/dev/null 2>&1
    return $?
  fi
  docker info >/dev/null 2>&1
}

# The dispatcher and api are the SAME binary, run as two launchd services.
restart_services() {
  launchctl kickstart -k "gui/$UID_NUM/com.chuggernaut.dispatcher"
  launchctl kickstart -k "gui/$UID_NUM/com.chuggernaut.api"
}

# One dispatcher liveness probe: 0 = the dispatcher answered on NATS. Overridable
# via HEALTHCHECK_CMD (tests inject a fake that inspects the installed binary).
# A live dispatcher answers req.jobs.list; a crash-looping one has no responder
# and `nats request` exits non-zero.
probe_once() {
  if [ -n "${HEALTHCHECK_CMD:-}" ]; then
    sh -c "$HEALTHCHECK_CMD" && return 0 || return 1
  fi
  docker run --rm --network "${NATS_NETWORK:?set NATS_NETWORK}" \
    -v "${KEYS_DIR:?set KEYS_DIR}:/keys:ro" \
    natsio/nats-box:latest \
    nats request "${HEALTHCHECK_SUBJECT:-req.jobs.list.kasofsk.chuggernaut}" '{}' \
    -s "nats://nats:4222" --creds /keys/dispatcher.creds --timeout 4s \
    >/dev/null 2>&1 && return 0 || return 1
}

# Poll probe_once for up to HEALTH_TIMEOUT_SECS, printing a transcript line per
# attempt so a failed deploy reads as a story in the task log (the command-task
# output is now visible in the UI log viewer).
await_health() {
  what="$1"
  deadline=$(( $(date +%s) + HEALTH_TIMEOUT_SECS ))
  attempt=0
  while :; do
    attempt=$((attempt + 1))
    if probe_once; then
      echo "health: dispatcher answered on NATS ($what) — healthy after $attempt attempt(s)"
      return 0
    fi
    if [ "$(date +%s)" -ge "$deadline" ]; then
      echo "health: dispatcher did NOT answer within ${HEALTH_TIMEOUT_SECS}s ($what, $attempt attempt(s))"
      return 1
    fi
    echo "health: no dispatcher yet ($what, attempt $attempt) — retrying in ${HEALTH_INTERVAL_SECS}s"
    sleep "$HEALTH_INTERVAL_SECS"
  done
}

# Preflight the docker engine BEFORE touching services. Only the real nats-box
# probe needs docker, so skip this when the caller injects HEALTHCHECK_CMD (the
# shell test's fake probe inspects a file, not NATS). A down engine is exit 4 —
# a DISTINCT code that update.sh must not confuse with a health-check failure —
# and we do NOT roll back: the running binary is fine, docker is what's down.
if [ -z "${HEALTHCHECK_CMD:-}" ] && ! docker_up; then
  echo "health: docker engine is DOWN (docker info failed) — the NATS liveness probe" \
       "cannot run. This is NOT a dispatcher failure; NOT restarting and NOT rolling" \
       "back. Bring docker/colima up and re-run the deploy." >&2
  exit 4
fi

echo "health: restarting dispatcher + api onto $TARGET_SHA"
restart_services
if await_health "new build $TARGET_SHA"; then
  echo "health: deploy $TARGET_SHA is healthy"
  exit 0
fi

# --- new build failed its health check → roll back to the previous binary -----
echo "health: new build $TARGET_SHA FAILED health check — rolling back to $PREV_SHA" >&2
if [ ! -f "$PREV_BIN" ]; then
  echo "ROLLBACK IMPOSSIBLE: no previous binary at $PREV_BIN to restore. The" \
       "dispatcher is crash-looping on $TARGET_SHA and launchd is retrying it" \
       "UNWATCHED. Manual intervention required NOW." >&2
  exit 3
fi
cp -f "$PREV_BIN" "$BIN"
echo "health: restored previous binary ($PREV_SHA) over $TARGET_SHA; restarting" >&2
restart_services
if await_health "rollback to $PREV_SHA"; then
  # Prod is back up on the old binary — but the DEPLOY still failed, loudly, so
  # the deploy job goes red and the bad build is investigated.
  echo "new build failed health check, rolled back to $PREV_SHA, now healthy" >&2
  exit 1
fi
echo "CATASTROPHE: new build $TARGET_SHA failed health check AND the rollback to" \
     "$PREV_SHA is ALSO unhealthy. launchd is retrying a broken dispatcher" \
     "UNWATCHED — prod is DOWN. Manual intervention required NOW." >&2
exit 2
