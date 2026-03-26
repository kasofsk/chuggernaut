terraform {
  required_version = ">= 1.4"
}

locals {
  api  = "${var.forgejo_url}/api/v1"
  auth = "Authorization: token ${var.admin_token}"

  # Workflow files to push into the repo (with {{ACTIONS_URL}} replaced)
  workflows = {
    "work.yml"   = replace(var.work_workflow, "{{ACTIONS_URL}}", var.actions_url)
    "review.yml" = replace(var.review_workflow, "{{ACTIONS_URL}}", var.actions_url)
  }

  # Action variables (service URLs)
  variables = {
    CHUGGERNAUT_NATS_URL       = var.nats_url
    CHUGGERNAUT_DISPATCHER_URL = var.dispatcher_url
  }
}

# ---------------------------------------------------------------------------
# Step 1: Create org
# ---------------------------------------------------------------------------

resource "terraform_data" "org" {
  input = var.org_name

  provisioner "local-exec" {
    command = <<-EOT
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
        "${local.api}/orgs" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"username": "${var.org_name}", "visibility": "public"}')
      if [ "$STATUS" = "201" ] || [ "$STATUS" = "409" ] || [ "$STATUS" = "422" ]; then
        echo "Org ${var.org_name}: status $STATUS (ok)"
      else
        echo "Org ${var.org_name}: unexpected status $STATUS" >&2
        exit 1
      fi
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 2: Create repo with auto-init
# ---------------------------------------------------------------------------

resource "terraform_data" "repo" {
  depends_on = [terraform_data.org]
  input      = "${var.org_name}/${var.repo_name}"

  provisioner "local-exec" {
    command = <<-EOT
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
        "${local.api}/orgs/${var.org_name}/repos" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{
          "name": "${var.repo_name}",
          "default_branch": "main",
          "auto_init": true
        }')
      if [ "$STATUS" = "201" ] || [ "$STATUS" = "409" ] || [ "$STATUS" = "422" ]; then
        echo "Repo ${var.org_name}/${var.repo_name}: status $STATUS (ok)"
      else
        echo "Repo ${var.org_name}/${var.repo_name}: unexpected status $STATUS" >&2
        exit 1
      fi
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 3: Add collaborators
# ---------------------------------------------------------------------------

resource "terraform_data" "collaborator_worker" {
  depends_on = [terraform_data.repo]
  input      = var.worker_username

  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/collaborators/${var.worker_username}" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"permission": "write"}'
    EOT
  }
}

resource "terraform_data" "collaborator_reviewer" {
  depends_on = [terraform_data.repo]
  input      = var.reviewer_username

  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/collaborators/${var.reviewer_username}" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"permission": "write"}'
    EOT
  }
}

resource "terraform_data" "collaborator_human" {
  depends_on = [terraform_data.repo]
  input      = var.human_username

  provisioner "local-exec" {
    command = <<-EOT
      # Admin permission lets the human cancel action runs in the Forgejo UI.
      # Branch protection with apply_to_admins=true prevents merge bypass.
      curl -sf -X PUT \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/collaborators/${var.human_username}" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"permission": "admin"}'
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 3b: Create "reviewers" org team and add reviewer + human
#
# Org membership (via team) is required for the approvals_whitelist to work
# in Forgejo branch protection. Repo collaborator status is not sufficient.
# ---------------------------------------------------------------------------

resource "terraform_data" "reviewers_team" {
  depends_on = [terraform_data.repo]

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      # Create team (ignore if exists)
      TEAM_RESP=$(curl -s -X POST \
        "${local.api}/orgs/${var.org_name}/teams" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"name":"reviewers","permission":"write","units":["repo.code","repo.pulls","repo.issues"]}')
      TEAM_ID=$(echo "$TEAM_RESP" | jq -r '.id // empty')

      # If team already exists, look it up
      if [ -z "$TEAM_ID" ]; then
        TEAM_ID=$(curl -sf \
          "${local.api}/orgs/${var.org_name}/teams" \
          -H "${local.auth}" | jq -r '.[] | select(.name=="reviewers") | .id')
      fi

      if [ -z "$TEAM_ID" ]; then
        echo "Failed to create or find reviewers team" >&2
        exit 1
      fi

      # Add members and repo to team
      curl -sf -X PUT "${local.api}/teams/$TEAM_ID/members/${var.reviewer_username}" -H "${local.auth}"
      curl -sf -X PUT "${local.api}/teams/$TEAM_ID/members/${var.human_username}" -H "${local.auth}"
      curl -sf -X PUT "${local.api}/teams/$TEAM_ID/repos/${var.org_name}/${var.repo_name}" -H "${local.auth}"

      echo "Reviewers team: id=$TEAM_ID members=[${var.reviewer_username}, ${var.human_username}]"
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 4: Push workflow files into .forgejo/workflows/
#
# Pushed sequentially in a single resource to avoid git race conditions
# (each file push creates a commit; concurrent commits on the same branch
# cause the second to fail with a conflict).
# ---------------------------------------------------------------------------

resource "terraform_data" "workflows" {
  depends_on = [terraform_data.repo]

  triggers_replace = {
    work_content   = local.workflows["work.yml"]
    review_content = local.workflows["review.yml"]
  }

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      push_file() {
        local FILENAME="$1"
        local CONTENT_B64="$2"
        local FILE_PATH=".forgejo/workflows/$FILENAME"

        # Check if file already exists to get its SHA
        EXISTING=$(curl -s \
          "${local.api}/repos/${var.org_name}/${var.repo_name}/contents/$FILE_PATH" \
          -H "${local.auth}")
        SHA=$(echo "$EXISTING" | jq -r '.sha // empty')

        if [ -n "$SHA" ]; then
          PAYLOAD=$(jq -n --arg msg "chore: update $FILENAME workflow" \
                          --arg content "$CONTENT_B64" \
                          --arg sha "$SHA" \
                          '{message: $msg, content: $content, sha: $sha}')
          curl -sf -X PUT \
            "${local.api}/repos/${var.org_name}/${var.repo_name}/contents/$FILE_PATH" \
            -H "${local.auth}" \
            -H "Content-Type: application/json" \
            -d "$PAYLOAD"
        else
          PAYLOAD=$(jq -n --arg msg "chore: add $FILENAME workflow" \
                          --arg content "$CONTENT_B64" \
                          '{message: $msg, content: $content}')
          curl -sf -X POST \
            "${local.api}/repos/${var.org_name}/${var.repo_name}/contents/$FILE_PATH" \
            -H "${local.auth}" \
            -H "Content-Type: application/json" \
            -d "$PAYLOAD"
        fi

        echo "Workflow $FILENAME: pushed"
      }

      # Push sequentially — each creates a git commit
      push_file "work.yml" "${base64encode(local.workflows["work.yml"])}"
      push_file "review.yml" "${base64encode(local.workflows["review.yml"])}"
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 4b: Push initial bootstrap files (CLAUDE.md, .gitignore, etc.)
#
# Pushed sequentially after workflows to avoid git race conditions.
# Uses the same create-or-update pattern as workflow pushes.
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
          "${local.api}/repos/${var.org_name}/${var.repo_name}/contents/$FILE_PATH" \
          -H "${local.auth}")
        SHA=$(echo "$EXISTING" | jq -r '.sha // empty')

        if [ -n "$SHA" ]; then
          PAYLOAD=$(jq -n --arg msg "chore: update $FILE_PATH" \
                          --arg content "$CONTENT_B64" \
                          --arg sha "$SHA" \
                          '{message: $msg, content: $content, sha: $sha}')
          curl -sf -X PUT \
            "${local.api}/repos/${var.org_name}/${var.repo_name}/contents/$FILE_PATH" \
            -H "${local.auth}" \
            -H "Content-Type: application/json" \
            -d "$PAYLOAD"
        else
          PAYLOAD=$(jq -n --arg msg "chore: add $FILE_PATH" \
                          --arg content "$CONTENT_B64" \
                          '{message: $msg, content: $content}')
          curl -sf -X POST \
            "${local.api}/repos/${var.org_name}/${var.repo_name}/contents/$FILE_PATH" \
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
# Step 5: Set action secrets
# ---------------------------------------------------------------------------

resource "terraform_data" "secret_worker_token" {
  depends_on = [terraform_data.repo]
  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/actions/secrets/CHUGGERNAUT_WORKER_TOKEN" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"data": "${var.worker_token}"}'
    EOT
  }
}

resource "terraform_data" "secret_reviewer_token" {
  depends_on = [terraform_data.repo]
  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/actions/secrets/CHUGGERNAUT_REVIEWER_TOKEN" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"data": "${var.reviewer_token}"}'
    EOT
  }
}

resource "terraform_data" "secret_claude_oauth" {
  count      = var.claude_oauth_token != "" ? 1 : 0
  depends_on = [terraform_data.repo]
  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/actions/secrets/CLAUDE_CODE_OAUTH_TOKEN" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"data": "${var.claude_oauth_token}"}'
    EOT
  }
}

resource "terraform_data" "secret_anthropic_key" {
  count      = var.anthropic_api_key != "" ? 1 : 0
  depends_on = [terraform_data.repo]
  provisioner "local-exec" {
    command = <<-EOT
      curl -sf -X PUT \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/actions/secrets/ANTHROPIC_API_KEY" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"data": "${var.anthropic_api_key}"}'
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 6: Set action variables
# ---------------------------------------------------------------------------

resource "terraform_data" "variables" {
  depends_on = [terraform_data.repo]

  for_each = { for k, v in local.variables : k => v if v != "" }

  triggers_replace = {
    value = each.value
  }

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      # Try create first (POST to /variables/{name}), update on conflict
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/actions/variables/${each.key}" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{"value": "${each.value}"}')

      if [ "$STATUS" = "201" ] || [ "$STATUS" = "204" ]; then
        echo "Variable ${each.key}: created"
      elif [ "$STATUS" = "409" ] || [ "$STATUS" = "422" ]; then
        # Already exists — update it (PUT to /variables/{name})
        curl -sf -X PUT \
          "${local.api}/repos/${var.org_name}/${var.repo_name}/actions/variables/${each.key}" \
          -H "${local.auth}" \
          -H "Content-Type: application/json" \
          -d '{"value": "${each.value}"}'
        echo "Variable ${each.key}: updated"
      else
        echo "Variable ${each.key}: unexpected status $STATUS" >&2
        exit 1
      fi
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 7: Branch protection — restrict who can merge to main
#
# Merge whitelist: admin + reviewer bot only. The reviewer bot merges after
# approving. Human collaborators can approve/reject but cannot merge.
# Workers can push branches and create PRs but cannot merge.
# ---------------------------------------------------------------------------

resource "terraform_data" "branch_protection" {
  depends_on = [terraform_data.repo, terraform_data.reviewers_team]

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      # Delete existing rule if present (idempotent)
      curl -s -o /dev/null -X DELETE \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/branch_protections/main" \
        -H "${local.auth}" || true

      # Create branch protection
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
        "${local.api}/repos/${var.org_name}/${var.repo_name}/branch_protections" \
        -H "${local.auth}" \
        -H "Content-Type: application/json" \
        -d '{
          "branch_name": "main",
          "enable_push": true,
          "enable_merge_whitelist": true,
          "merge_whitelist_usernames": ["${var.admin_username}", "${var.reviewer_username}"],
          "enable_approvals_whitelist": true,
          "approvals_whitelist_teams": ["reviewers"],
          "required_approvals": 1,
          "block_on_rejected_reviews": true,
          "enable_push_whitelist": false,
          "apply_to_admins": true
        }')

      if [ "$STATUS" = "201" ] || [ "$STATUS" = "200" ]; then
        echo "Branch protection on main: merge restricted to [${var.admin_username}, ${var.reviewer_username}]"
      else
        echo "Branch protection on main: unexpected status $STATUS" >&2
        exit 1
      fi
    EOT
  }
}
