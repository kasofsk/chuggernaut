#!/usr/bin/env bash
# Bootstrap the full chuggernaut dev environment.
#
# Usage:
#   ./scripts/dev-up.sh                          # default: 1 runner, no repos
#   ./scripts/dev-up.sh --runners 3              # 3 runners
#   ./scripts/dev-up.sh --repo acme/payments     # bootstrap a repo (repeatable)
#   ./scripts/dev-up.sh --runners 2 --repo test/repo --repo acme/payments
#   ./scripts/dev-up.sh --clean --repo test/repo   # wipe everything and start fresh
#   ./scripts/dev-up.sh --repo sb/studybuddy --initial-file sb/studybuddy:CLAUDE.md=../studybuddy/CLAUDE.md
#   ./scripts/dev-up.sh --flavor flutter --repo sb/studybuddy  # build base + flutter runner images
#
# Prerequisites:
#   - docker, docker compose
#   - terraform >= 1.4
#   - nats CLI (https://github.com/nats-io/natscli)
#   - curl, jq
#
# What it does:
#   1. Builds runner-env image (if needed)
#   2. Starts NATS + Forgejo + dispatcher via docker compose
#   3. Waits for Forgejo API readiness
#   4. Creates admin user + token (first-run bootstrap)
#   5. Runs terraform apply to provision:
#      - NATS KV buckets + JetStream streams
#      - Worker + reviewer Forgejo users with scoped tokens
#      - Repos with workflow files, secrets, and variables
#      - N action runners as Docker containers
#   6. Writes .env for docker compose (admin token for dispatcher)
#   7. Restarts dispatcher to pick up the admin token
#
# After this script completes, the system is ready to accept jobs via:
#   cargo run -p chuggernaut-cli -- create --repo <org/repo> --title "..."
# or by seeding a fixture:
#   cargo run -p chuggernaut-cli -- seed --file fixtures/sample.json --repo <org/repo>

set -euo pipefail
cd "$(dirname "$0")/.."

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------

RUNNER_COUNT=1
REPOS=()
FLAVORS=()
# Initial files stored as "org/repo:repo_path=local_file" entries
INITIAL_FILE_ENTRIES=()
CLEAN=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runners)
      RUNNER_COUNT="$2"
      shift 2
      ;;
    --repo)
      REPOS+=("$2")
      shift 2
      ;;
    --flavor)
      # Runner flavor to build (e.g. rust, flutter). Repeatable.
      # Base image is always built. Each flavor needs a Dockerfile.runner-env.<flavor>.
      FLAVORS+=("$2")
      shift 2
      ;;
    --initial-file)
      # Format: org/repo:repo_path=local_file
      # e.g. sb/studybuddy:CLAUDE.md=../studybuddy/CLAUDE.md
      local_file="${2#*:}"
      local_file="${local_file#*=}"
      if [ ! -f "$local_file" ]; then
        echo "Error: file not found: $local_file" >&2
        exit 1
      fi
      INITIAL_FILE_ENTRIES+=("$2")
      shift 2
      ;;
    --clean)
      CLEAN=true
      shift
      ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Usage: $0 [--clean] [--runners N] [--flavor name ...] [--repo org/repo ...] [--initial-file org/repo:path=file ...]" >&2
      exit 1
      ;;
  esac
done

# Default to rust flavor if none specified (backwards compat)
if [ ${#FLAVORS[@]} -eq 0 ]; then
  FLAVORS=("rust")
fi

FORGEJO_URL="${CHUGGERNAUT_FORGEJO_URL:-http://localhost:3000}"
NATS_URL="${CHUGGERNAUT_NATS_URL:-nats://localhost:4222}"
ADMIN_USER="chuggernaut-admin"
ADMIN_PASS="chuggernaut-admin"
ADMIN_EMAIL="admin@chuggernaut.local"

# Resolve Claude credentials (check prefixed and unprefixed env vars)
CLAUDE_TOKEN="${CHUGGERNAUT_CLAUDE_CODE_OAUTH_TOKEN:-${CLAUDE_CODE_OAUTH_TOKEN:-}}"
API_KEY="${CHUGGERNAUT_ANTHROPIC_API_KEY:-${ANTHROPIC_API_KEY:-}}"

if [ -z "$CLAUDE_TOKEN" ] && [ -z "$API_KEY" ]; then
  echo "ERROR: No Claude credentials found." >&2
  echo "Set CHUGGERNAUT_CLAUDE_CODE_OAUTH_TOKEN or CHUGGERNAUT_ANTHROPIC_API_KEY" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Step 0: Clean (if requested)
# ---------------------------------------------------------------------------

if [ "$CLEAN" = true ]; then
  echo "==> Cleaning up previous environment..."

  # Remove Terraform-managed runner containers and orphaned action containers
  docker ps -a --filter "name=chuggernaut-runner-" --format "{{.Names}}" | while read -r name; do
    docker rm -f "$name" 2>/dev/null || true
  done
  docker ps -a --filter "name=FORGEJO-ACTIONS" --format "{{.Names}}" | while read -r name; do
    docker rm -f "$name" 2>/dev/null || true
  done

  # Destroy Terraform state
  if [ -f infra/terraform/terraform.tfstate ]; then
    (cd infra/terraform && terraform destroy -auto-approve -input=false 2>/dev/null) || true
    rm -f infra/terraform/terraform.tfstate infra/terraform/terraform.tfstate.backup
  fi

  # Stop all compose services and wipe volumes
  docker compose down -v 2>/dev/null || true

  # Clean Terraform working files
  rm -f infra/terraform/terraform.tfvars
  rm -rf infra/terraform/.terraform
  rm -rf infra/terraform/.tokens
  rm -f .env

  echo "    Clean complete."
fi

# ---------------------------------------------------------------------------
# Step 1: Build runner-env images (base + requested flavors)
# ---------------------------------------------------------------------------

echo "==> Building runner-env images..."
echo "    Flavors: ${FLAVORS[*]}"

# Always build base image first (all flavors extend it)
if ! docker image inspect chuggernaut-runner-env:latest >/dev/null 2>&1; then
  docker compose --profile build build runner-env
else
  echo "    Base image already built (use --clean to rebuild)"
fi

# Build each requested flavor
for flavor in "${FLAVORS[@]}"; do
  DOCKERFILE="Dockerfile.runner-env.${flavor}"
  if [ ! -f "$DOCKERFILE" ]; then
    echo "    ERROR: $DOCKERFILE not found" >&2
    exit 1
  fi
  if ! docker image inspect "chuggernaut-runner-env:${flavor}" >/dev/null 2>&1; then
    echo "    Building ${flavor} flavor..."
    docker build -t "chuggernaut-runner-env:${flavor}" -f "$DOCKERFILE" .
  else
    echo "    ${flavor} flavor already built (use --clean to rebuild)"
  fi
done

# ---------------------------------------------------------------------------
# Step 2: Start core services
# ---------------------------------------------------------------------------

echo "==> Starting NATS + Forgejo..."
docker compose up -d nats forgejo

echo "==> Waiting for Forgejo API..."
for i in $(seq 1 60); do
  if curl -sf "${FORGEJO_URL}/api/v1/version" >/dev/null 2>&1; then
    echo "    Forgejo ready."
    break
  fi
  if [ "$i" -eq 60 ]; then
    echo "    ERROR: Forgejo not ready after 60s" >&2
    exit 1
  fi
  sleep 1
done

# ---------------------------------------------------------------------------
# Step 3: Create admin user + token (first-run bootstrap)
# ---------------------------------------------------------------------------

echo "==> Creating admin user..."
docker compose exec -T forgejo su-exec git forgejo admin user create \
  --admin \
  --username "${ADMIN_USER}" \
  --password "${ADMIN_PASS}" \
  --email "${ADMIN_EMAIL}" \
  --must-change-password=false 2>/dev/null || echo "    (admin user may already exist)"

echo "==> Creating admin API token..."
# Delete stale token if it exists
curl -sf -X DELETE \
  -u "${ADMIN_USER}:${ADMIN_PASS}" \
  "${FORGEJO_URL}/api/v1/users/${ADMIN_USER}/tokens/chuggernaut-admin" 2>/dev/null || true

ADMIN_TOKEN=$(curl -sf -X POST \
  -u "${ADMIN_USER}:${ADMIN_PASS}" \
  "${FORGEJO_URL}/api/v1/users/${ADMIN_USER}/tokens" \
  -H "Content-Type: application/json" \
  -d '{"name":"chuggernaut-admin","scopes":["all"]}' | jq -r '.sha1')

if [ -z "$ADMIN_TOKEN" ] || [ "$ADMIN_TOKEN" = "null" ]; then
  echo "    ERROR: Failed to create admin token" >&2
  exit 1
fi
echo "    Admin token: ${ADMIN_TOKEN:0:8}..."

# ---------------------------------------------------------------------------
# Step 4: Build terraform.tfvars and apply
# ---------------------------------------------------------------------------

echo "==> Generating terraform.tfvars..."

# Build managed_repos JSON array (with optional initial_files)
REPOS_JSON="["
for repo in "${REPOS[@]+"${REPOS[@]}"}"; do
  ORG="${repo%%/*}"
  REPO_NAME="${repo##*/}"
  if [ "$REPOS_JSON" != "[" ]; then
    REPOS_JSON+=","
  fi

  # Collect initial_files entries matching this repo
  FILES_HCL="{"
  FILES_FIRST=true
  for entry in "${INITIAL_FILE_ENTRIES[@]+"${INITIAL_FILE_ENTRIES[@]}"}"; do
    ENTRY_REPO="${entry%%:*}"
    if [ "$ENTRY_REPO" = "$repo" ]; then
      SPEC="${entry#*:}"
      FPATH="${SPEC%%=*}"
      FLOCAL="${SPEC#*=}"
      CONTENT=$(cat "$FLOCAL")
      ESCAPED=$(python3 -c "import json,sys; print(json.dumps(sys.stdin.read()))" <<< "$CONTENT")
      if [ "$FILES_FIRST" = true ]; then FILES_FIRST=false; else FILES_HCL+=","; fi
      FILES_HCL+="\"${FPATH}\"=${ESCAPED}"
    fi
  done
  FILES_HCL+="}"

  REPOS_JSON+="{org=\"${ORG}\",repo=\"${REPO_NAME}\",initial_files=${FILES_HCL}}"
done
REPOS_JSON+="]"

# Build runner labels from flavors
# Always include the default ubuntu-latest label pointing to the base image.
# Each flavor adds its own label → image mapping.
RUNNER_LABELS="ubuntu-latest:docker://chuggernaut-runner-env:latest"
for flavor in "${FLAVORS[@]}"; do
  RUNNER_LABELS+=",${flavor}:docker://chuggernaut-runner-env:${flavor}"
done

cat > infra/terraform/terraform.tfvars <<EOF
forgejo_url         = "${FORGEJO_URL}"
forgejo_admin_token = "${ADMIN_TOKEN}"
nats_url            = "${NATS_URL}"
nats_worker_url     = "nats://host.docker.internal:4222"
dispatcher_url      = "http://host.docker.internal:8080"
worker_password     = "chuggernaut-worker-pass"
reviewer_password   = "chuggernaut-reviewer-pass"
runner_count        = ${RUNNER_COUNT}
runner_labels       = "${RUNNER_LABELS}"
managed_repos       = ${REPOS_JSON}
claude_oauth_token  = "${CLAUDE_TOKEN}"
anthropic_api_key   = "${API_KEY}"
EOF

echo "==> Running terraform init..."
(cd infra/terraform && terraform init -input=false) >/dev/null 2>&1

echo "==> Running terraform apply..."
(cd infra/terraform && terraform apply -auto-approve -input=false)

# Extract tokens from terraform output
WORKER_TOKEN=$(cd infra/terraform && terraform output -raw worker_token 2>/dev/null)
REVIEWER_TOKEN=$(cd infra/terraform && terraform output -raw reviewer_token 2>/dev/null)

# ---------------------------------------------------------------------------
# Step 5: Write .env and restart dispatcher
# ---------------------------------------------------------------------------

# Build runner label map JSON from flavors
LABEL_MAP_JSON="{"
FIRST=true
for flavor in "${FLAVORS[@]}"; do
  if [ "$FIRST" = true ]; then FIRST=false; else LABEL_MAP_JSON+=","; fi
  LABEL_MAP_JSON+="\"${flavor}\":\"${flavor}\""
done
LABEL_MAP_JSON+="}"

echo "==> Writing .env for docker compose..."
cat > .env <<EOF
CHUGGERNAUT_FORGEJO_TOKEN=${ADMIN_TOKEN}
CHUGGERNAUT_RUNNER_LABEL_MAP=${LABEL_MAP_JSON}
EOF

echo "==> Starting dispatcher..."
docker compose up -d dispatcher

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------

echo ""
echo "============================================"
echo "  Chuggernaut dev environment is ready!"
echo "============================================"
echo ""
echo "Services:"
echo "  Forgejo:    ${FORGEJO_URL}"
echo "  NATS:       ${NATS_URL}"
echo "  Dispatcher: http://localhost:8080"
echo "  Runners:    ${RUNNER_COUNT} (flavors: ${FLAVORS[*]})"
echo ""
echo "Users:"
echo "  Admin:    ${ADMIN_USER} (dispatcher token)"
echo "  Worker:   chuggernaut-worker (work action token)"
echo "  Reviewer: chuggernaut-reviewer (review action token)"
echo ""
if [ ${#REPOS[@]} -gt 0 ]; then
  echo "Repos:"
  for repo in "${REPOS[@]}"; do
    echo "  ${FORGEJO_URL}/${repo}"
  done
  echo ""
fi
echo "Create a job:"
echo "  cargo run -p chuggernaut-cli -- create --repo <org/repo> --title 'My task' --body 'Do the thing'"
echo ""
echo "Seed from a fixture:"
echo "  cargo run -p chuggernaut-cli -- seed --file fixtures/sample.json --repo <org/repo>"
echo ""
echo "Tear down:"
echo "  ./scripts/dev-down.sh"
