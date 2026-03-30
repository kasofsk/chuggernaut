#!/bin/sh
# Entrypoint for Forgejo action runners in docker-compose.
# Waits for Forgejo, fetches a registration token, registers, and starts the daemon.
# Re-registration on restart is safe — Forgejo deduplicates by runner name.
set -eu

FORGEJO_URL="${FORGEJO_URL:-http://host.docker.internal:3000}"
FORGEJO_ADMIN_USER="${FORGEJO_ADMIN_USER:-chuggernaut-admin}"
FORGEJO_ADMIN_PASS="${FORGEJO_ADMIN_PASS:-chuggernaut-admin}"
RUNNER_LABELS="${RUNNER_LABELS:-ubuntu-latest:docker://chuggernaut-runner-env:latest}"
RUNNER_NAME="chuggernaut-runner-$(hostname)"

CONFIG=/data/config.yaml

echo "==> Waiting for Forgejo at ${FORGEJO_URL}..."
until wget -qO /dev/null "${FORGEJO_URL}/api/v1/version" 2>/dev/null; do
  sleep 2
done
echo "    Forgejo is ready."

echo "==> Fetching registration token..."
REG_TOKEN=""
for i in $(seq 1 30); do
  REG_TOKEN=$(wget -qO - \
    --header "Authorization: Basic $(echo -n "${FORGEJO_ADMIN_USER}:${FORGEJO_ADMIN_PASS}" | base64)" \
    "${FORGEJO_URL}/api/v1/admin/runners/registration-token" 2>/dev/null \
    | sed -n 's/.*"token":"\([^"]*\)".*/\1/p') || true
  if [ -n "$REG_TOKEN" ]; then
    break
  fi
  echo "    Retry ${i}/30..."
  sleep 2
done

if [ -z "$REG_TOKEN" ]; then
  echo "ERROR: Failed to get registration token after 30 attempts" >&2
  exit 1
fi

echo "==> Registering runner ${RUNNER_NAME}..."
cd /data
forgejo-runner register --no-interactive \
  --instance "${FORGEJO_URL}" \
  --token "${REG_TOKEN}" \
  --name "${RUNNER_NAME}" \
  --labels "${RUNNER_LABELS}" \
  -c "${CONFIG}"

echo "==> Starting daemon..."
exec forgejo-runner daemon -c "${CONFIG}"
