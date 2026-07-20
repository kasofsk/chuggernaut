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
export CHANNEL_BINARY="$HERE/out/chuggernaut-channel"

"$HERE/wait-nats.sh"
exec "$REPO/target/release/chuggernaut" dispatcher
