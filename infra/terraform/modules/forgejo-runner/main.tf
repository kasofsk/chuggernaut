terraform {
  required_version = ">= 1.4"
}

# ---------------------------------------------------------------------------
# Get a runner registration token from Forgejo
# ---------------------------------------------------------------------------

data "external" "registration_token" {
  program = ["bash", "-c", <<-EOT
    set -euo pipefail
    RESP=$(curl -sf \
      "${var.forgejo_url}/api/v1/admin/runners/registration-token" \
      -H "Authorization: token ${var.admin_token}")
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
# Start N runners as Docker containers
#
# Each runner registers with Forgejo and runs as a daemon. Uses Docker
# socket passthrough so runners can spawn job containers.
# ---------------------------------------------------------------------------

resource "terraform_data" "runners" {
  for_each = { for i in range(var.runner_count) : tostring(i) => i }

  input = {
    name  = "chuggernaut-runner-${each.value}"
    index = each.value
  }

  provisioner "local-exec" {
    command = <<-EOT
      set -euo pipefail

      NAME="chuggernaut-runner-${each.value}"
      REG_TOKEN="${data.external.registration_token.result["token"]}"

      # Remove existing container if present (idempotent restart)
      docker rm -f "$NAME" 2>/dev/null || true

      docker run -d \
        --name "$NAME" \
        --user root \
        -v /var/run/docker.sock:/var/run/docker.sock \
        -v "${var.runner_config_path}:/data/config.yaml:ro" \
        --add-host=host.docker.internal:host-gateway \
        "${var.runner_image}" \
        sh -c "
          cd /data && \
          forgejo-runner register --no-interactive \
            --instance '${var.forgejo_internal_url != "" ? var.forgejo_internal_url : var.forgejo_url}' \
            --token '$REG_TOKEN' \
            --name '$NAME' \
            --labels '${var.runner_labels != "" ? var.runner_labels : "ubuntu-latest:docker://chuggernaut-runner-env:${var.runner_env_tag}"}' \
            -c /data/config.yaml && \
          forgejo-runner daemon -c /data/config.yaml
        "

      echo "Runner $NAME: started"
    EOT
  }

  provisioner "local-exec" {
    when    = destroy
    command = <<-EOT
      docker rm -f "chuggernaut-runner-${self.input.index}" 2>/dev/null || true
    EOT
  }
}
