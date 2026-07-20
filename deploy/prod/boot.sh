#!/bin/sh
# Bring the container substrate up in the right order, idempotently:
#   colima (Docker runtime) -> NATS + sshd (compose) -> wait for NATS ready.
# Run at login by com.chuggernaut.boot; the dispatcher/api KeepAlive jobs then
# start (each waits for NATS itself, so exact ordering isn't load-bearing).
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod

set -a
. "$HERE/chuggernaut.env"
set +a
export GIT_UID="${GIT_UID:-$(id -u)}"

# 1. Docker runtime (Colima). `colima start` is a no-op if already running.
if ! colima status >/dev/null 2>&1; then
  echo "boot: starting colima"
  colima start
fi

# 2. NATS (operator mode, JetStream) + the SSH front.
echo "boot: docker compose up"
docker compose -f "$HERE/compose.yaml" up -d

# 3. Readiness gate.
"$HERE/wait-nats.sh"
echo "boot: stack up"
