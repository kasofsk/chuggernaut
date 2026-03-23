#!/usr/bin/env bash
# Initialize Forgejo with a test org, repo, and API token.
# Run after `docker compose up -d` and waiting for Forgejo to start.

set -euo pipefail

FORGEJO_URL="${FORGE2_FORGEJO_URL:-http://localhost:3000}"
ADMIN_USER="forge2-admin"
ADMIN_PASS="forge2-admin"
ADMIN_EMAIL="admin@forge2.local"

echo "Waiting for Forgejo to be ready..."
until curl -sf "${FORGEJO_URL}/api/v1/version" > /dev/null 2>&1; do
    sleep 1
done
echo "Forgejo is ready."

# Create admin user (may already exist)
docker compose exec -T forgejo forgejo admin user create \
    --admin --username "${ADMIN_USER}" --password "${ADMIN_PASS}" \
    --email "${ADMIN_EMAIL}" --must-change-password=false 2>/dev/null || true

# Create API token
TOKEN=$(curl -sf -X POST "${FORGEJO_URL}/api/v1/users/${ADMIN_USER}/tokens" \
    -u "${ADMIN_USER}:${ADMIN_PASS}" \
    -H "Content-Type: application/json" \
    -d '{"name":"forge2-token","scopes":["all"]}' | jq -r '.sha1')

if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    echo "Token may already exist. Trying to list tokens..."
    TOKEN=$(curl -sf "${FORGEJO_URL}/api/v1/users/${ADMIN_USER}/tokens" \
        -u "${ADMIN_USER}:${ADMIN_PASS}" | jq -r '.[0].sha1 // empty')
fi

echo "API Token: ${TOKEN}"

# Create test org and repo
curl -sf -X POST "${FORGEJO_URL}/api/v1/orgs" \
    -H "Authorization: token ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"username":"test","visibility":"public"}' > /dev/null 2>&1 || true

curl -sf -X POST "${FORGEJO_URL}/api/v1/orgs/test/repos" \
    -H "Authorization: token ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"name":"repo","default_branch":"main","auto_init":true}' > /dev/null 2>&1 || true

echo ""
echo "Setup complete!"
echo ""
echo "Export these:"
echo "  export FORGE2_FORGEJO_URL=${FORGEJO_URL}"
echo "  export FORGE2_FORGEJO_TOKEN=${TOKEN}"
echo ""
echo "Then run:"
echo "  cargo run -p forge2-dispatcher"
echo "  cargo run -p forge2-cli -- sim --forgejo-url ${FORGEJO_URL} --forgejo-token ${TOKEN}"
echo "  cargo run -p forge2-cli -- create --repo test/repo --title 'Test job'"
