terraform {
  required_version = ">= 1.4"
}

# ---------------------------------------------------------------------------
# Get a runner registration token from GitHub
#
# GitHub runner registration tokens are short-lived (1 hour) and scoped
# to a repo. The token is used by the actions/runner config.sh script.
# ---------------------------------------------------------------------------

data "external" "registration_token" {
  program = ["bash", "-c", <<-EOT
    set -euo pipefail
    RESP=$(curl -sf -X POST \
      "https://api.github.com/repos/${var.owner}/${var.repo_name}/actions/runners/registration-token" \
      -H "Authorization: Bearer ${var.github_token}" \
      -H "Accept: application/vnd.github+json")
    TOKEN=$(echo "$RESP" | jq -r '.token')
    if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
      echo "Failed to get runner registration token" >&2
      exit 1
    fi
    jq -n --arg token "$TOKEN" '{"token": $token}'
  EOT
  ]
}

# ---------------------------------------------------------------------------
# Start N self-hosted runners as Docker containers
#
# Each runner:
# 1. Downloads the latest GitHub Actions runner
# 2. Configures it with the registration token
# 3. Runs as a daemon picking up jobs
#
# Uses Docker socket passthrough so runners can spawn job containers.
# ---------------------------------------------------------------------------

resource "terraform_data" "runners" {
  for_each = { for i in range(var.runner_count) : tostring(i) => i }

  input = {
    name  = "chuggernaut-gh-runner-${each.value}"
    index = each.value
  }

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      NAME="chuggernaut-gh-runner-${each.value}"
      REG_TOKEN="${data.external.registration_token.result["token"]}"

      # Remove existing container if present (idempotent restart)
      docker rm -f "$NAME" 2>/dev/null || true

      docker run -d \
        --name "$NAME" \
        --user root \
        -v /var/run/docker.sock:/var/run/docker.sock \
        --add-host=host.docker.internal:host-gateway \
        "${var.runner_image}" \
        bash -c "
          set -e
          cd ${var.runner_work_dir}

          # Download latest GitHub Actions runner
          RUNNER_VERSION=\$(curl -sf https://api.github.com/repos/actions/runner/releases/latest | grep -oP '\"tag_name\": \"v\\K[^\"]+')
          curl -sL https://github.com/actions/runner/releases/download/v\$RUNNER_VERSION/actions-runner-linux-x64-\$RUNNER_VERSION.tar.gz | tar xz

          # Configure (non-interactive, ephemeral so it deregisters on exit)
          ./config.sh --unattended \
            --url 'https://github.com/${var.owner}/${var.repo_name}' \
            --token '$REG_TOKEN' \
            --name '$NAME' \
            --labels '${var.runner_labels}' \
            --ephemeral \
            --replace

          # Run
          ./run.sh
        "

      echo "Runner $NAME: started"
    EOT
  }

  provisioner "local-exec" {
    when    = destroy
    command = <<-EOT
      docker rm -f "chuggernaut-gh-runner-${self.input.index}" 2>/dev/null || true
    EOT
  }
}
