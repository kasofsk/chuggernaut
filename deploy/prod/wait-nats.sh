#!/bin/sh
# Block until NATS answers on localhost:$NATS_WAIT_PORT (default 4222). Used as a
# readiness gate by boot.sh and the run-*.sh wrappers so launchd start ordering
# is not load-bearing.
set -eu
PORT="${NATS_WAIT_PORT:-4222}"
for _ in $(seq 1 120); do
  nc -z localhost "$PORT" 2>/dev/null && exit 0
  sleep 1
done
echo "wait-nats: timed out waiting for nats on localhost:$PORT" >&2
exit 1
