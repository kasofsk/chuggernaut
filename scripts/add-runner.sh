#!/usr/bin/env bash
# Add another Forgejo Action runner and bump dispatcher capacity.
#
# Usage:
#   ./scripts/add-runner.sh              # adds runner, bumps capacity by 1
#   ./scripts/add-runner.sh --capacity 3 # adds runner, sets capacity to 3
#
# The runner registers with the same labels as existing runners.
# The dispatcher's max_concurrent_actions is updated via the HTTP API (no restart).

set -euo pipefail
cd "$(dirname "$0")/.."

CAPACITY=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --capacity) CAPACITY="$2"; shift 2 ;;
    *) echo "Usage: $0 [--capacity N]" >&2; exit 1 ;;
  esac
done

FORGEJO_URL="${CHUGGERNAUT_FORGEJO_URL:-http://localhost:3000}"
DISPATCHER_URL="${CHUGGERNAUT_DISPATCHER_URL:-http://localhost:8080}"

# Find the next runner index
LAST=$(docker ps -a --filter "name=chuggernaut-runner-" --format "{{.Names}}" | grep -oE '[0-9]+$' | sort -n | tail -1)
NEXT=$(( ${LAST:-0} + 1 ))
NAME="chuggernaut-runner-${NEXT}"

echo "==> Getting runner registration token..."
ADMIN_USER="chuggernaut-admin"
ADMIN_PASS="chuggernaut-admin"
REG_TOKEN=$(curl -sf "${FORGEJO_URL}/api/v1/admin/runners/registration-token" \
  -u "${ADMIN_USER}:${ADMIN_PASS}" | python3 -c "import json,sys; print(json.load(sys.stdin)['token'])")

echo "==> Getting labels from existing runner..."
LABELS=$(docker inspect chuggernaut-runner-0 --format '{{.Config.Cmd}}' 2>/dev/null \
  | grep -oE "'[^']*'" | tr -d "'" | grep -E ':docker://' || echo "")

if [ -z "$LABELS" ]; then
  echo "    Could not detect labels from runner-0, using default"
  LABELS="ubuntu-latest:docker://chuggernaut-runner-env:latest"
fi

echo "==> Starting runner ${NAME} with labels: ${LABELS}"
docker run -d \
  --name "$NAME" \
  --user root \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v "$(pwd)/infra/runner/config.yaml:/data/config.yaml:ro" \
  --add-host=host.docker.internal:host-gateway \
  data.forgejo.org/forgejo/runner:11 \
  sh -c "
    cd /data && \
    forgejo-runner register --no-interactive \
      --instance 'http://host.docker.internal:3000' \
      --token '${REG_TOKEN}' \
      --name '${NAME}' \
      --labels '${LABELS}' \
      -c /data/config.yaml && \
    forgejo-runner daemon -c /data/config.yaml
  "

echo "==> Runner ${NAME} started"

# Bump dispatcher capacity
CURRENT=$(curl -sf "${DISPATCHER_URL}/config/max_concurrent_actions" | python3 -c "import json,sys; print(json.load(sys.stdin)['max_concurrent_actions'])")
if [ -n "$CAPACITY" ]; then
  NEW_CAP="$CAPACITY"
else
  NEW_CAP=$((CURRENT + 1))
fi

echo "==> Updating dispatcher capacity: ${CURRENT} -> ${NEW_CAP}"
curl -sf -X PUT "${DISPATCHER_URL}/config/max_concurrent_actions" \
  -H "Content-Type: application/json" \
  -d "{\"value\": ${NEW_CAP}}"

echo ""
echo "Done. Runner ${NAME} registered, dispatcher capacity now ${NEW_CAP}."
