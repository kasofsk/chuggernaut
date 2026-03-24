#!/usr/bin/env bash
# Initialize Forgejo with admin user, reviewer user, test org/repo, and API tokens.
# Run after `docker compose up -d` and waiting for Forgejo to start.

set -euo pipefail

FORGEJO_URL="${CHUGGERNAUT_FORGEJO_URL:-http://localhost:3000}"
ADMIN_USER="chuggernaut-admin"
ADMIN_PASS="chuggernaut-admin"
ADMIN_EMAIL="admin@chuggernaut.local"
REVIEWER_USER="chuggernaut-reviewer"
REVIEWER_PASS="chuggernaut-reviewer"
REVIEWER_EMAIL="reviewer@chuggernaut.local"

echo "Waiting for Forgejo to be ready..."
until curl -sf "${FORGEJO_URL}/api/v1/version" > /dev/null 2>&1; do
    sleep 1
done
echo "Forgejo is ready."

# Create admin user (may already exist)
docker compose exec -T forgejo forgejo admin user create \
    --admin --username "${ADMIN_USER}" --password "${ADMIN_PASS}" \
    --email "${ADMIN_EMAIL}" --must-change-password=false 2>/dev/null || true

# Create reviewer user (may already exist)
docker compose exec -T forgejo forgejo admin user create \
    --username "${REVIEWER_USER}" --password "${REVIEWER_PASS}" \
    --email "${REVIEWER_EMAIL}" --must-change-password=false 2>/dev/null || true

# Create admin API token
ADMIN_TOKEN=$(curl -sf -X POST "${FORGEJO_URL}/api/v1/users/${ADMIN_USER}/tokens" \
    -u "${ADMIN_USER}:${ADMIN_PASS}" \
    -H "Content-Type: application/json" \
    -d '{"name":"chuggernaut-token","scopes":["all"]}' | jq -r '.sha1')

if [ -z "$ADMIN_TOKEN" ] || [ "$ADMIN_TOKEN" = "null" ]; then
    echo "Admin token may already exist. Trying to list tokens..."
    ADMIN_TOKEN=$(curl -sf "${FORGEJO_URL}/api/v1/users/${ADMIN_USER}/tokens" \
        -u "${ADMIN_USER}:${ADMIN_PASS}" | jq -r '.[0].sha1 // empty')
fi

# Create reviewer API token
REVIEWER_TOKEN=$(curl -sf -X POST "${FORGEJO_URL}/api/v1/users/${REVIEWER_USER}/tokens" \
    -u "${REVIEWER_USER}:${REVIEWER_PASS}" \
    -H "Content-Type: application/json" \
    -d '{"name":"reviewer-token","scopes":["all"]}' | jq -r '.sha1')

if [ -z "$REVIEWER_TOKEN" ] || [ "$REVIEWER_TOKEN" = "null" ]; then
    echo "Reviewer token may already exist. Trying to list tokens..."
    REVIEWER_TOKEN=$(curl -sf "${FORGEJO_URL}/api/v1/users/${REVIEWER_USER}/tokens" \
        -u "${REVIEWER_USER}:${REVIEWER_PASS}" | jq -r '.[0].sha1 // empty')
fi

# Get runner registration token
RUNNER_REG_TOKEN=$(curl -sf -X POST "${FORGEJO_URL}/api/v1/user/actions/runners/registration-token" \
    -H "Authorization: token ${ADMIN_TOKEN}" | jq -r '.token // empty')

echo "Admin token:    ${ADMIN_TOKEN}"
echo "Reviewer token: ${REVIEWER_TOKEN}"
echo "Runner token:   ${RUNNER_REG_TOKEN}"

# Create test org and repo
curl -sf -X POST "${FORGEJO_URL}/api/v1/orgs" \
    -H "Authorization: token ${ADMIN_TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"username":"test","visibility":"public"}' > /dev/null 2>&1 || true

curl -sf -X POST "${FORGEJO_URL}/api/v1/orgs/test/repos" \
    -H "Authorization: token ${ADMIN_TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"name":"repo","default_branch":"main","auto_init":true}' > /dev/null 2>&1 || true

# Add reviewer as collaborator on test repo
curl -sf -X PUT "${FORGEJO_URL}/api/v1/repos/test/repo/collaborators/${REVIEWER_USER}" \
    -H "Authorization: token ${ADMIN_TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"permission":"write"}' > /dev/null 2>&1 || true

# Write .env file for docker-compose
cat > .env <<EOF
CHUGGERNAUT_FORGEJO_TOKEN=${ADMIN_TOKEN}
CHUGGERNAUT_REVIEWER_TOKEN=${REVIEWER_TOKEN}
RUNNER_TOKEN=${RUNNER_REG_TOKEN}
EOF

echo ""
echo "Setup complete! Wrote .env file."
echo ""
echo "To use the CLI directly:"
echo "  export CHUGGERNAUT_FORGEJO_URL=${FORGEJO_URL}"
echo "  export CHUGGERNAUT_FORGEJO_TOKEN=${ADMIN_TOKEN}"
echo ""
echo "To start the full stack:"
echo "  docker compose build runner-env   # build worker image (first time only)"
echo "  docker compose up -d              # restart with .env tokens"
echo ""
echo "Then create a job:"
echo "  cargo run -p chuggernaut-cli -- create --repo test/repo --title 'Test job'"
