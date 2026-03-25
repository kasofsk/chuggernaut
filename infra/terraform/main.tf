terraform {
  required_version = ">= 1.4"
}

# ---------------------------------------------------------------------------
# NATS: KV buckets + JetStream streams
# ---------------------------------------------------------------------------

module "nats" {
  source   = "./modules/nats"
  nats_url = var.nats_url
}

# ---------------------------------------------------------------------------
# Forgejo users with scoped tokens
# ---------------------------------------------------------------------------

module "worker_user" {
  source      = "./modules/forgejo-user"
  forgejo_url = var.forgejo_url
  admin_token = var.forgejo_admin_token
  username    = "chuggernaut-worker"
  password    = var.worker_password
  email       = "worker@chuggernaut.local"
  token_name  = "chuggernaut-worker"

  # Minimum permissions for work actions:
  # - clone repo, push branches, create/find PRs
  # - NO merge (that's the reviewer's job)
  token_scopes = [
    "read:repository",
    "write:repository",
    "read:issue",
    "write:issue",
  ]
}

module "reviewer_user" {
  source      = "./modules/forgejo-user"
  forgejo_url = var.forgejo_url
  admin_token = var.forgejo_admin_token
  username    = "chuggernaut-reviewer"
  password    = var.reviewer_password
  email       = "reviewer@chuggernaut.local"
  token_name  = "chuggernaut-reviewer"

  # Minimum permissions for review actions:
  # - clone repo, read PRs, submit reviews, merge (bot reviewer)
  # - Separate user ensures reviewer cannot approve their own PRs
  token_scopes = [
    "read:repository",
    "write:repository",
    "read:issue",
    "write:issue",
  ]
}

module "human_user" {
  source      = "./modules/forgejo-user"
  forgejo_url = var.forgejo_url
  admin_token = var.forgejo_admin_token
  username    = var.human_username
  password    = var.human_password
  email       = "${var.human_username}@chuggernaut.local"
  token_name  = var.human_username

  # Human collaborator: can approve/reject PRs but cannot merge.
  # Branch protection restricts merge to admin + reviewer bot only.
  token_scopes = [
    "read:repository",
    "write:repository",
    "read:issue",
    "write:issue",
  ]
}

# ---------------------------------------------------------------------------
# Actions repo: chuggernaut/actions — hosts composite actions (chug)
# ---------------------------------------------------------------------------

resource "terraform_data" "actions_org" {
  input = "chuggernaut"
  provisioner "local-exec" {
    command = <<-EOT
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
        "${var.forgejo_url}/api/v1/orgs" \
        -H "Authorization: token ${var.forgejo_admin_token}" \
        -H "Content-Type: application/json" \
        -d '{"username": "chuggernaut", "visibility": "public"}')
      if [ "$STATUS" = "201" ] || [ "$STATUS" = "409" ] || [ "$STATUS" = "422" ]; then
        echo "Org chuggernaut: status $STATUS (ok)"
      else
        echo "Org chuggernaut: unexpected status $STATUS" >&2; exit 1
      fi
    EOT
  }
}

resource "terraform_data" "actions_repo" {
  depends_on = [terraform_data.actions_org]
  input      = "chuggernaut/actions"
  provisioner "local-exec" {
    command = <<-EOT
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
        "${var.forgejo_url}/api/v1/orgs/chuggernaut/repos" \
        -H "Authorization: token ${var.forgejo_admin_token}" \
        -H "Content-Type: application/json" \
        -d '{"name": "actions", "default_branch": "main", "auto_init": true}')
      if [ "$STATUS" = "201" ] || [ "$STATUS" = "409" ] || [ "$STATUS" = "422" ]; then
        echo "Repo chuggernaut/actions: status $STATUS (ok)"
      else
        echo "Repo chuggernaut/actions: unexpected status $STATUS" >&2; exit 1
      fi
    EOT
  }
}

resource "terraform_data" "actions_files" {
  depends_on = [terraform_data.actions_repo]

  triggers_replace = {
    chug_content = file("${path.root}/../../action/chug/action.yml")
  }

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail
      API="${var.forgejo_url}/api/v1"
      AUTH="Authorization: token ${var.forgejo_admin_token}"
      REPO="chuggernaut/actions"

      push_file() {
        local FILE_PATH="$1"
        local CONTENT_B64="$2"

        EXISTING=$(curl -s "$API/repos/$REPO/contents/$FILE_PATH" -H "$AUTH")
        SHA=$(echo "$EXISTING" | jq -r '.sha // empty')

        if [ -n "$SHA" ]; then
          PAYLOAD=$(jq -n --arg msg "chore: update $FILE_PATH" \
                          --arg content "$CONTENT_B64" \
                          --arg sha "$SHA" \
                          '{message: $msg, content: $content, sha: $sha}')
          curl -sf -X PUT "$API/repos/$REPO/contents/$FILE_PATH" \
            -H "$AUTH" -H "Content-Type: application/json" -d "$PAYLOAD"
        else
          PAYLOAD=$(jq -n --arg msg "chore: add $FILE_PATH" \
                          --arg content "$CONTENT_B64" \
                          '{message: $msg, content: $content}')
          curl -sf -X POST "$API/repos/$REPO/contents/$FILE_PATH" \
            -H "$AUTH" -H "Content-Type: application/json" -d "$PAYLOAD"
        fi
        echo "$FILE_PATH: pushed"
      }

      push_file "chug/action.yml" "${base64encode(file("${path.root}/../../action/chug/action.yml"))}"
    EOT
  }
}

# ---------------------------------------------------------------------------
# Managed repos: org, repo, workflows, secrets, variables
# ---------------------------------------------------------------------------

module "repos" {
  source   = "./modules/forgejo-repo"
  for_each = { for r in var.managed_repos : "${r.org}/${r.repo}" => r }

  # Explicit dependency: all users must exist before repo provisioning
  depends_on = [module.worker_user, module.reviewer_user, module.human_user]

  forgejo_url       = var.forgejo_url
  admin_token       = var.forgejo_admin_token
  org_name          = each.value.org
  repo_name         = each.value.repo
  admin_username    = var.forgejo_admin_username
  worker_username   = module.worker_user.username
  reviewer_username = module.reviewer_user.username
  human_username    = module.human_user.username
  worker_token      = module.worker_user.token
  reviewer_token    = module.reviewer_user.token
  nats_url          = var.nats_worker_url
  dispatcher_url    = var.dispatcher_url
  claude_oauth_token = var.claude_oauth_token
  anthropic_api_key  = var.anthropic_api_key
  actions_url        = var.forgejo_actions_url
  work_workflow      = file("${path.root}/../../action/work.yml")
  review_workflow    = file("${path.root}/../../action/review.yml")
}

# ---------------------------------------------------------------------------
# Forgejo Action runners
# ---------------------------------------------------------------------------

module "runners" {
  source               = "./modules/forgejo-runner"
  forgejo_url          = var.forgejo_url
  forgejo_internal_url = "http://host.docker.internal:3000"
  admin_token          = var.forgejo_admin_token
  runner_count         = var.runner_count
  runner_config_path   = abspath("${path.root}/../runner/config.yaml")
}
