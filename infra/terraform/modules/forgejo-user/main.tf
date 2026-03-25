terraform {
  required_version = ">= 1.4"
}

# ---------------------------------------------------------------------------
# Step 1: Create user via admin API (idempotent — ignores "already exists")
# ---------------------------------------------------------------------------

resource "terraform_data" "user" {
  input = var.username

  provisioner "local-exec" {
    command = <<-EOT
      STATUS=$(curl -s -o /dev/null -w '%%{http_code}' -X POST \
        "${var.forgejo_url}/api/v1/admin/users" \
        -H "Authorization: token ${var.admin_token}" \
        -H "Content-Type: application/json" \
        -d '{
          "username": "${var.username}",
          "password": "${var.password}",
          "email": "${var.email}",
          "must_change_password": false,
          "login_name": "${var.username}",
          "source_id": 0
        }')
      if [ "$STATUS" = "201" ] || [ "$STATUS" = "422" ]; then
        echo "User ${var.username}: status $STATUS (ok)"
      else
        echo "User ${var.username}: unexpected status $STATUS" >&2
        exit 1
      fi
    EOT
  }
}

# ---------------------------------------------------------------------------
# Step 2: Mint a scoped API token (cached — only recreated if invalid)
#
# Tokens are cached in .tokens/{username}.token so they survive re-applies.
# On each apply the script checks if the cached token still authenticates;
# only if it fails does it delete+recreate.
# ---------------------------------------------------------------------------

data "external" "token" {
  depends_on = [terraform_data.user]

  program = ["bash", "-c", <<-EOT
    set -euo pipefail

    FORGEJO_URL="${var.forgejo_url}"
    USERNAME="${var.username}"
    PASSWORD="${var.password}"
    TOKEN_NAME="${var.token_name}"
    SCOPES_JSON='${jsonencode(var.token_scopes)}'
    CACHE_DIR="${path.root}/.tokens"
    CACHE_FILE="$CACHE_DIR/$USERNAME.token"

    mkdir -p "$CACHE_DIR"

    # Check if cached token is still valid
    if [ -f "$CACHE_FILE" ]; then
      CACHED=$(cat "$CACHE_FILE")
      if [ -n "$CACHED" ]; then
        STATUS=$(curl -s -o /dev/null -w '%%{http_code}' \
          "$FORGEJO_URL/api/v1/repos/search?limit=1" \
          -H "Authorization: token $CACHED")
        if [ "$STATUS" = "200" ]; then
          jq -n --arg token "$CACHED" '{"token": $token}'
          exit 0
        fi
      fi
    fi

    # Cached token missing or invalid — recreate
    curl -sf -X DELETE \
      -u "$USERNAME:$PASSWORD" \
      "$FORGEJO_URL/api/v1/users/$USERNAME/tokens/$TOKEN_NAME" \
      2>/dev/null || true

    RESP=$(curl -sf -X POST \
      -u "$USERNAME:$PASSWORD" \
      "$FORGEJO_URL/api/v1/users/$USERNAME/tokens" \
      -H "Content-Type: application/json" \
      -d "{\"name\": \"$TOKEN_NAME\", \"scopes\": $SCOPES_JSON}")

    TOKEN=$(echo "$RESP" | jq -r '.sha1')
    if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
      echo "Failed to extract token from response: $RESP" >&2
      exit 1
    fi

    # Cache for next apply
    echo "$TOKEN" > "$CACHE_FILE"

    jq -n --arg token "$TOKEN" '{"token": $token}'
  EOT
  ]
}
