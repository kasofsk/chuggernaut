terraform {
  required_version = ">= 1.4"
}

locals {
  api  = "https://api.github.com"
  auth = "Authorization: Bearer ${var.github_token}"

  # Action variables (service URLs)
  variables = {
    CHUGGERNAUT_NATS_URL       = var.nats_url
    CHUGGERNAUT_DISPATCHER_URL = var.dispatcher_url
  }
}

# ---------------------------------------------------------------------------
# Step 1: Create repo (if it doesn't exist)
#
# GitHub doesn't have the org/team creation flow that Forgejo needs — the
# org and bot users must already exist. We just create the repo.
# ---------------------------------------------------------------------------

resource "terraform_data" "repo" {
  input = "${var.owner}/${var.repo_name}"

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      # Check if repo exists
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' \
        "${local.api}/repos/${var.owner}/${var.repo_name}" \
        -H "${local.auth}")

      if [ "$STATUS" = "200" ]; then
        echo "Repo ${var.owner}/${var.repo_name}: already exists"
      elif [ "$STATUS" = "404" ]; then
        # Create under the org
        CREATE_STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
          "${local.api}/orgs/${var.owner}/repos" \
          -H "${local.auth}" \
          -H "Content-Type: application/json" \
          -d '{
            "name": "${var.repo_name}",
            "default_branch": "main",
            "auto_init": true,
            "private": true
          }')
        if [ "$CREATE_STATUS" = "201" ]; then
          echo "Repo ${var.owner}/${var.repo_name}: created"
        else
          echo "Repo ${var.owner}/${var.repo_name}: create failed with status $CREATE_STATUS" >&2
          exit 1
        fi
      else
        echo "Repo ${var.owner}/${var.repo_name}: unexpected status $STATUS" >&2
        exit 1
      fi
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 2: Add collaborators
# ---------------------------------------------------------------------------

resource "terraform_data" "collaborator_worker" {
  depends_on = [terraform_data.repo]
  input      = var.worker_username

  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.owner}/${var.repo_name}/collaborators/${var.worker_username}" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"permission": "push"}'
    EOT
  }
}

resource "terraform_data" "collaborator_reviewer" {
  depends_on = [terraform_data.repo]
  input      = var.reviewer_username

  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.owner}/${var.repo_name}/collaborators/${var.reviewer_username}" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"permission": "push"}'
    EOT
  }
}

resource "terraform_data" "collaborator_human" {
  depends_on = [terraform_data.repo]
  input      = var.human_username

  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.owner}/${var.repo_name}/collaborators/${var.human_username}" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"permission": "admin"}'
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 3: Push workflow files into .github/workflows/
#
# Pushed sequentially in a single resource to avoid git race conditions.
# GitHub uses .github/workflows/ (not .forgejo/workflows/).
# ---------------------------------------------------------------------------

resource "terraform_data" "workflows" {
  depends_on = [terraform_data.repo]

  triggers_replace = {
    work_content   = var.work_workflow
    review_content = var.review_workflow
  }

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      push_file() {
        local FILENAME="$1"
        local CONTENT_B64="$2"
        local FILE_PATH=".github/workflows/$FILENAME"

        # Check if file already exists to get its SHA
        EXISTING=$(curl -s \
          "${local.api}/repos/${var.owner}/${var.repo_name}/contents/$FILE_PATH" \
          -H "${local.auth}")
        SHA=$(echo "$EXISTING" | jq -r '.sha // empty')

        if [ -n "$SHA" ]; then
          PAYLOAD=$(jq -n --arg msg "chore: update $FILENAME workflow" \
                          --arg content "$CONTENT_B64" \
                          --arg sha "$SHA" \
                          '{message: $msg, content: $content, sha: $sha}')
          curl -sf -X PUT \
            "${local.api}/repos/${var.owner}/${var.repo_name}/contents/$FILE_PATH" \
            -H "${local.auth}" \
            -H "Content-Type: application/json" \
            -d "$PAYLOAD"
        else
          PAYLOAD=$(jq -n --arg msg "chore: add $FILENAME workflow" \
                          --arg content "$CONTENT_B64" \
                          '{message: $msg, content: $content}')
          curl -sf -X PUT \
            "${local.api}/repos/${var.owner}/${var.repo_name}/contents/$FILE_PATH" \
            -H "${local.auth}" \
            -H "Content-Type: application/json" \
            -d "$PAYLOAD"
        fi

        echo "Workflow $FILENAME: pushed"
      }

      # Push sequentially — each creates a git commit
      push_file "work.yml" "${base64encode(var.work_workflow)}"
      push_file "review.yml" "${base64encode(var.review_workflow)}"
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 3b: Push initial bootstrap files (CLAUDE.md, .gitignore, etc.)
# ---------------------------------------------------------------------------

resource "terraform_data" "initial_files" {
  count      = length(var.initial_files) > 0 ? 1 : 0
  depends_on = [terraform_data.workflows]

  triggers_replace = {
    files = jsonencode(var.initial_files)
  }

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      push_file() {
        local FILE_PATH="$1"
        local CONTENT_B64="$2"

        EXISTING=$(curl -s \
          "${local.api}/repos/${var.owner}/${var.repo_name}/contents/$FILE_PATH" \
          -H "${local.auth}")
        SHA=$(echo "$EXISTING" | jq -r '.sha // empty')

        if [ -n "$SHA" ]; then
          PAYLOAD=$(jq -n --arg msg "chore: update $FILE_PATH" \
                          --arg content "$CONTENT_B64" \
                          --arg sha "$SHA" \
                          '{message: $msg, content: $content, sha: $sha}')
          curl -sf -X PUT \
            "${local.api}/repos/${var.owner}/${var.repo_name}/contents/$FILE_PATH" \
            -H "${local.auth}" \
            -H "Content-Type: application/json" \
            -d "$PAYLOAD"
        else
          PAYLOAD=$(jq -n --arg msg "chore: add $FILE_PATH" \
                          --arg content "$CONTENT_B64" \
                          '{message: $msg, content: $content}')
          curl -sf -X PUT \
            "${local.api}/repos/${var.owner}/${var.repo_name}/contents/$FILE_PATH" \
            -H "${local.auth}" \
            -H "Content-Type: application/json" \
            -d "$PAYLOAD"
        fi

        echo "Bootstrap file $FILE_PATH: pushed"
      }

      %{for path, content in var.initial_files~}
      push_file "${path}" "${base64encode(content)}"
      %{endfor~}
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 4: Set action secrets
#
# GitHub uses the repo public key to encrypt secrets via libsodium.
# We use the gh CLI for this since it handles encryption transparently.
# ---------------------------------------------------------------------------

resource "terraform_data" "secrets" {
  depends_on = [terraform_data.repo]

  provisioner "local-exec" {
    environment = {
      GH_TOKEN                   = var.github_token
      CHUGGERNAUT_WORKER_TOKEN   = var.worker_token
      CHUGGERNAUT_REVIEWER_TOKEN = var.reviewer_token
      CLAUDE_CODE_OAUTH_TOKEN    = var.claude_oauth_token
      ANTHROPIC_API_KEY          = var.anthropic_api_key
    }

    command = <<-EOT
      set -euo pipefail

      gh secret set CHUGGERNAUT_WORKER_TOKEN \
        --repo "${var.owner}/${var.repo_name}" \
        --body "$CHUGGERNAUT_WORKER_TOKEN"

      gh secret set CHUGGERNAUT_REVIEWER_TOKEN \
        --repo "${var.owner}/${var.repo_name}" \
        --body "$CHUGGERNAUT_REVIEWER_TOKEN"

      if [ -n "$CLAUDE_CODE_OAUTH_TOKEN" ]; then
        gh secret set CLAUDE_CODE_OAUTH_TOKEN \
          --repo "${var.owner}/${var.repo_name}" \
          --body "$CLAUDE_CODE_OAUTH_TOKEN"
      fi

      if [ -n "$ANTHROPIC_API_KEY" ]; then
        gh secret set ANTHROPIC_API_KEY \
          --repo "${var.owner}/${var.repo_name}" \
          --body "$ANTHROPIC_API_KEY"
      fi

      echo "Secrets: set"
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 5: Set action variables
# ---------------------------------------------------------------------------

resource "terraform_data" "variables" {
  depends_on = [terraform_data.repo]

  for_each = { for k, v in local.variables : k => v if v != "" }

  triggers_replace = {
    value = each.value
  }

  provisioner "local-exec" {
    environment = {
      GH_TOKEN = var.github_token
    }

    command = <<-EOT
      set -euo pipefail

      gh variable set "${each.key}" \
        --repo "${var.owner}/${var.repo_name}" \
        --body "${each.value}"

      echo "Variable ${each.key}: set"
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 6: Branch protection on main
#
# GitHub branch protection uses a different schema than Forgejo:
# - Required reviews with dismiss_stale_reviews
# - Restrict who can push (enforce_admins)
# - No explicit merge whitelist — merge is allowed for anyone with push
#   access who satisfies the review requirements
#
# We use the GitHub API directly since the branch protection rules are
# complex and gh CLI doesn't cover all options.
# ---------------------------------------------------------------------------

resource "terraform_data" "branch_protection" {
  depends_on = [
    terraform_data.repo,
    terraform_data.collaborator_worker,
    terraform_data.collaborator_reviewer,
    terraform_data.collaborator_human,
  ]

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X PUT \
        "${local.api}/repos/${var.owner}/${var.repo_name}/branches/main/protection" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{
          "required_status_checks": null,
          "enforce_admins": true,
          "required_pull_request_reviews": {
            "dismiss_stale_reviews": true,
            "require_code_owner_reviews": false,
            "required_approving_review_count": ${var.required_approvals},
            "bypass_pull_request_allowances": {
              "users": ["${var.reviewer_username}"]
            }
          },
          "restrictions": {
            "users": ["${var.reviewer_username}"],
            "teams": []
          }
        }')

      if [ "$STATUS" = "200" ]; then
        echo "Branch protection on main: configured (merge restricted to ${var.reviewer_username})"
      else
        echo "Branch protection on main: unexpected status $STATUS" >&2
        exit 1
      fi
    EOT
  }
}
