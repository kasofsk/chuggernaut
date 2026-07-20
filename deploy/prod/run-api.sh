#!/bin/sh
# launchd wrapper for the api + UI: source prod config, point UI_DIST at the
# built SPA, wait for NATS, then exec.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod
REPO="$(cd "$HERE/../.." && pwd)"          # workspace root

set -a
. "$HERE/chuggernaut.env"
set +a

export UI_DIST="$REPO/web/dist"

"$HERE/wait-nats.sh"
exec "$REPO/target/release/chuggernaut" api
