#!/bin/sh
# launchd wrapper for the dispatcher: source prod config, derive repo-relative
# paths, wait for NATS, then exec. launchd can't source an env file — this can.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod
REPO="$(cd "$HERE/../.." && pwd)"          # workspace root

set -a
. "$HERE/chuggernaut.env"
set +a

# Repo-relative paths are derived here, not stored in the env file.
# With a remote worker fleet (WORKER_DOCKER_HOST set), job containers run on
# the worker, so inject the worker-arch channel binary built by
# build-worker.sh; otherwise the local (colima-arch) one from build.sh.
if [ -n "${WORKER_DOCKER_HOST:-}" ] && [ -f "$HERE/out-nuc/chuggernaut-channel" ]; then
  export CHANNEL_BINARY="$HERE/out-nuc/chuggernaut-channel"
else
  export CHANNEL_BINARY="$HERE/out/chuggernaut-channel"
fi

"$HERE/wait-nats.sh"
exec "$REPO/target/release/chuggernaut" dispatcher
