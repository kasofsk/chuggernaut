#!/bin/sh
# launchd wrapper for the native api (HTTP↔NATS bridge + web UI): source prod
# config, point the binary at the served-UI host dir, wait for NATS, then exec.
# launchd can't source an env file — this can (mirrors run-dispatcher.sh).
#
# The api is the SAME binary the host already builds every deploy (`chuggernaut
# api`), so running it natively removes the api container and its in-VM cargo
# compile — the VM is left holding only NATS + the ssh front (README §2).
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"      # deploy/prod
REPO="$(cd "$HERE/../.." && pwd)"          # workspace root

set -a
. "$HERE/chuggernaut.env"
set +a

# Serve the SPA from the host UI dir that web-publish rsyncs into (README §7),
# not from a baked-in image copy. NATS_URL/KEYS_DIR/SESSION_TTL come from the
# sourced env; bind loopback only — Tailscale Serve / cloudflared front :8080
# (README §5).
export UI_DIST="${UI_ROOT:-$HOME/chuggernaut-data/ui}"
export BIND_ADDR="127.0.0.1:8080"

"$HERE/wait-nats.sh"
exec "$REPO/target/release/chuggernaut" api
