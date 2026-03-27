#!/usr/bin/env bash
# Bootstrap chuggernaut with GitHub as the git provider.
#
# Usage:
#   ./scripts/github-setup.sh --owner myorg --repo myproject
#   ./scripts/github-setup.sh --owner myorg --repo myproject --repo another-repo
#   ./scripts/github-setup.sh --owner myorg --repo myproject --initial-file myorg/myproject:CLAUDE.md=./CLAUDE.md
#
# Prerequisites:
#   - terraform >= 1.4
#   - gh CLI (authenticated: `gh auth login`)
#   - nats CLI (https://github.com/nats-io/natscli)
#   - docker, docker compose (for NATS + dispatcher + runners)
#   - curl, jq
#
# Required environment variables:
#   GITHUB_TOKEN                  - PAT with repo, workflow, admin:org scopes (admin token)
#   GITHUB_WORKER_TOKEN           - PAT for the worker bot account
#   GITHUB_REVIEWER_TOKEN         - PAT for the reviewer bot account
#   CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY - Claude credentials
#
# What it does:
#   1. Starts NATS + dispatcher via docker compose (no Forgejo)
#   2. Runs terraform apply to provision:
#      - NATS KV buckets
#      - GitHub repos with workflows, secrets, variables, branch protection
#      - Self-hosted runners as Docker containers (per repo)
#   3. Writes .env for docker compose (git token for dispatcher)
#   4. Restarts dispatcher to pick up the config
#
# GitHub bot accounts (worker + reviewer) must be created manually:
#   1. Create two GitHub accounts (or use GitHub Apps)
#   2. Generate PATs with repo scope for each
#   3. Set GITHUB_WORKER_TOKEN and GITHUB_REVIEWER_TOKEN

set -euo pipefail
cd "$(dirname "$0")/.."

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------

OWNER=""
REPOS=()
INITIAL_FILE_ENTRIES=()
RUNNER_COUNT=1
WORKER_USERNAME="${GITHUB_WORKER_USERNAME:-chuggernaut-worker}"
REVIEWER_USERNAME="${GITHUB_REVIEWER_USERNAME:-chuggernaut-reviewer}"
HUMAN_USERNAME="${GITHUB_HUMAN_USERNAME:-$(gh api user --jq .login 2>/dev/null || echo "human")}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --owner)
      OWNER="$2"
      shift 2
      ;;
    --repo)
      REPOS+=("$2")
      shift 2
      ;;
    --runners)
      RUNNER_COUNT="$2"
      shift 2
      ;;
    --initial-file)
      local_file="${2#*:}"
      local_file="${local_file#*=}"
      if [ ! -f "$local_file" ]; then
        echo "Error: file not found: $local_file" >&2
        exit 1
      fi
      INITIAL_FILE_ENTRIES+=("$2")
      shift 2
      ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Usage: $0 --owner <org> --repo <name> [--repo <name> ...] [--runners N] [--initial-file org/repo:path=file ...]" >&2
      exit 1
      ;;
  esac
done

if [ -z "$OWNER" ]; then
  echo "ERROR: --owner is required" >&2
  exit 1
fi

if [ ${#REPOS[@]} -eq 0 ]; then
  echo "ERROR: at least one --repo is required" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Validate environment
# ---------------------------------------------------------------------------

GITHUB_TOKEN="${GITHUB_TOKEN:-}"
GITHUB_WORKER_TOKEN="${GITHUB_WORKER_TOKEN:-}"
GITHUB_REVIEWER_TOKEN="${GITHUB_REVIEWER_TOKEN:-}"
CLAUDE_TOKEN="${CHUGGERNAUT_CLAUDE_CODE_OAUTH_TOKEN:-${CLAUDE_CODE_OAUTH_TOKEN:-}}"
API_KEY="${CHUGGERNAUT_ANTHROPIC_API_KEY:-${ANTHROPIC_API_KEY:-}}"

if [ -z "$GITHUB_TOKEN" ]; then
  echo "ERROR: GITHUB_TOKEN is required (PAT with repo, workflow, admin:org scopes)" >&2
  exit 1
fi

if [ -z "$GITHUB_WORKER_TOKEN" ]; then
  echo "ERROR: GITHUB_WORKER_TOKEN is required (PAT for worker bot account)" >&2
  exit 1
fi

if [ -z "$GITHUB_REVIEWER_TOKEN" ]; then
  echo "ERROR: GITHUB_REVIEWER_TOKEN is required (PAT for reviewer bot account)" >&2
  exit 1
fi

if [ -z "$CLAUDE_TOKEN" ] && [ -z "$API_KEY" ]; then
  echo "ERROR: No Claude credentials found." >&2
  echo "Set CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY" >&2
  exit 1
fi

NATS_URL="${CHUGGERNAUT_NATS_URL:-nats://localhost:4222}"

# ---------------------------------------------------------------------------
# Step 1: Start NATS + dispatcher (no Forgejo needed)
# ---------------------------------------------------------------------------

echo "==> Starting NATS..."
docker compose up -d nats

echo "==> Waiting for NATS..."
for i in $(seq 1 30); do
  if nats server check connection -s "$NATS_URL" >/dev/null 2>&1; then
    echo "    NATS ready."
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "    ERROR: NATS not ready after 30s" >&2
    exit 1
  fi
  sleep 1
done

# ---------------------------------------------------------------------------
# Step 2: Build terraform.tfvars and apply
# ---------------------------------------------------------------------------

echo "==> Generating terraform.tfvars..."

# Build managed_repos JSON array
REPOS_JSON="["
for repo in "${REPOS[@]}"; do
  if [ "$REPOS_JSON" != "[" ]; then
    REPOS_JSON+=","
  fi

  # Collect initial_files entries matching this repo
  FILES_HCL="{"
  FILES_FIRST=true
  for entry in "${INITIAL_FILE_ENTRIES[@]+${INITIAL_FILE_ENTRIES[@]}}"; do
    ENTRY_REPO="${entry%%:*}"
    if [ "$ENTRY_REPO" = "${OWNER}/${repo}" ]; then
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

  REPOS_JSON+="{org=\"${OWNER}\",repo=\"${repo}\",initial_files=${FILES_HCL}}"
done
REPOS_JSON+="]"

cat > infra/terraform/terraform.tfvars <<EOF
git_provider            = "github"
github_token            = "${GITHUB_TOKEN}"
github_owner            = "${OWNER}"
github_worker_username  = "${WORKER_USERNAME}"
github_reviewer_username = "${REVIEWER_USERNAME}"
github_worker_token     = "${GITHUB_WORKER_TOKEN}"
github_reviewer_token   = "${GITHUB_REVIEWER_TOKEN}"
human_username          = "${HUMAN_USERNAME}"
nats_url                = "${NATS_URL}"
nats_worker_url         = "nats://host.docker.internal:4222"
dispatcher_url          = "http://host.docker.internal:8080"
runner_count            = ${RUNNER_COUNT}
managed_repos           = ${REPOS_JSON}
claude_oauth_token      = "${CLAUDE_TOKEN}"
anthropic_api_key       = "${API_KEY}"
EOF

echo "==> Running terraform init..."
(cd infra/terraform && terraform init -input=false) >/dev/null 2>&1

echo "==> Running terraform apply..."
(cd infra/terraform && terraform apply -auto-approve -input=false)

# ---------------------------------------------------------------------------
# Step 3: Write .env and start dispatcher
# ---------------------------------------------------------------------------

echo "==> Writing .env for docker compose..."
cat > .env <<EOF
CHUGGERNAUT_GIT_URL=https://github.com
CHUGGERNAUT_GIT_TOKEN=${GITHUB_TOKEN}
EOF

echo "==> Starting dispatcher..."
docker compose up -d dispatcher

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------

echo ""
echo "============================================"
echo "  Chuggernaut (GitHub) is ready!"
echo "============================================"
echo ""
echo "Services:"
echo "  GitHub:     https://github.com/${OWNER}"
echo "  NATS:       ${NATS_URL}"
echo "  Dispatcher: http://localhost:8080"
echo "  Runners:    ${RUNNER_COUNT} per repo"
echo ""
echo "Bot accounts:"
echo "  Worker:   ${WORKER_USERNAME}"
echo "  Reviewer: ${REVIEWER_USERNAME}"
echo ""
echo "Repos:"
for repo in "${REPOS[@]}"; do
  echo "  https://github.com/${OWNER}/${repo}"
done
echo ""
echo "Create a job:"
echo "  cargo run -p chuggernaut-cli -- create --repo ${OWNER}/${REPOS[0]} --title 'My task' --body 'Do the thing'"
echo ""
echo "Tear down:"
echo "  docker compose down && (cd infra/terraform && terraform destroy -auto-approve)"
echo ""
