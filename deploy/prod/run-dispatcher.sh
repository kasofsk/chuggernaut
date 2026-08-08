#!/bin/sh
# launchd wrapper for the dispatcher: source prod config, derive repo-relative
# paths, wait for NATS, then exec. launchd can't source an env file — this can.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod
REPO="$(cd "$HERE/../.." && pwd)"          # workspace root

set -a
. "$HERE/chuggernaut.env"
set +a

# Log level (#270, #493). The binary filters on RUST_LOG and its default
# directive is ERROR, so a dispatcher started without one discards every warn!
# and info! it emits. Declared HERE and not in the plist: launchd's
# EnvironmentVariables would shadow an operator's chuggernaut.env value, and
# this line yields to it.
: "${RUST_LOG:=info,async_nats=warn}"
export RUST_LOG

# Repo-relative paths are derived here, not stored in the env file. Worker
# nodes never receive these bytes (they inject their own node-local copy —
# spec §3.1); this file only feeds local docker-node launches and enables the
# channel MCP config.
export CHANNEL_BINARY="$HERE/out/chuggernaut-channel"

"$HERE/wait-nats.sh"
exec "$REPO/target/release/chuggernaut" dispatcher
