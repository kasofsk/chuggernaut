#!/usr/bin/env bash
# Tear down the chuggernaut dev environment.
#
# Stops docker compose services and removes Terraform-managed runners.
# Data volumes are preserved — use `docker compose down -v` to also wipe data.

set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> Stopping Terraform-managed runners..."
# Remove any chuggernaut-runner-* containers
docker ps -a --filter "name=chuggernaut-runner-" --format "{{.Names}}" | while read -r name; do
  echo "    Removing $name"
  docker rm -f "$name" 2>/dev/null || true
done

echo "==> Destroying Terraform state..."
if [ -f infra/terraform/terraform.tfstate ]; then
  (cd infra/terraform && terraform destroy -auto-approve -input=false 2>/dev/null) || true
fi

echo "==> Stopping docker compose services..."
docker compose down

echo ""
echo "Dev environment stopped. Data volumes preserved."
echo "To also wipe all data: docker compose down -v"
