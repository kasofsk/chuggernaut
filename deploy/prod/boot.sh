#!/bin/sh
# Bring the container substrate up in the right order, idempotently:
#   colima (Docker runtime) -> NATS + sshd (compose) -> wait for NATS ready.
# Run at login by com.chuggernaut.boot; the dispatcher/api KeepAlive launchd
# jobs then start (each waits for NATS itself, so exact ordering isn't
# load-bearing). The api runs natively too now (com.chuggernaut.api), so this
# compose stack is just nats + ssh.
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
# Served-UI host dir (the native api serves it via UI_DIST — run-api.sh); ensure
# it exists so the api doesn't warn about a missing dir on a cold boot before the
# first deploy has seeded it.
UI_ROOT="${UI_ROOT:-$HOME/chuggernaut-data/ui}"
export UI_ROOT
mkdir -p "$UI_ROOT"

echo "boot: docker compose up"
docker compose -f "$HERE/compose.yaml" up -d

# 3. Readiness gate.
"$HERE/wait-nats.sh"
echo "boot: stack up"
