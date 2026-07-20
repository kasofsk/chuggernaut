#!/bin/sh
# Chuggernaut CD workhorse — build the target commit natively and restart the
# host services, idempotently. Called by .github/workflows/deploy.yml on the
# Mini's self-hosted runner (after CI is green on main), and runnable by hand.
#
# It operates on the DEPLOYED checkout ($CHUG_REPO), NOT on wherever this script
# happens to be invoked from — the GitHub runner's ephemeral workspace is a
# different directory from the checkout launchd runs the binary out of.
#
# Usage: update.sh [ref]        (ref defaults to $GITHUB_SHA, else origin/main)
set -eu

CHUG_REPO="${CHUG_REPO:-$HOME/chuggernaut}"   # the deployed checkout
TARGET_REF="${1:-${GITHUB_SHA:-origin/main}}"

# Pre-bootstrap guard: before the Mini has been set up (README §1) there is no
# deployed checkout / config to act on. Skip cleanly (exit 0) rather than fail
# the Actions job — the first real deploy takes over once bootstrap is done.
if [ ! -d "$CHUG_REPO/.git" ]; then
  echo "update: $CHUG_REPO not bootstrapped yet — see deploy/prod/README.md §1; skipping"
  exit 0
fi
if [ ! -f "$CHUG_REPO/deploy/prod/chuggernaut.env" ]; then
  echo "update: deploy/prod/chuggernaut.env missing — bootstrap not complete; skipping"
  exit 0
fi

cd "$CHUG_REPO"
git fetch --quiet origin

TARGET_SHA="$(git rev-parse --verify --quiet "${TARGET_REF}^{commit}" || git rev-parse origin/main)"
MARK="$CHUG_REPO/.deployed-sha"

if [ -f "$MARK" ] && [ "$(cat "$MARK")" = "$TARGET_SHA" ]; then
  echo "update: already deployed $TARGET_SHA — nothing to do"
  exit 0
fi
echo "update: deploying $TARGET_SHA"

# gitignored files (deploy/prod/chuggernaut.env, deploy/prod/out/, .deployed-sha)
# survive a forced checkout.
git checkout --quiet --force "$TARGET_SHA"

# Rollback snapshot of the currently-running binary before we overwrite it.
if [ -f target/release/chuggernaut ]; then
  cp -f target/release/chuggernaut target/release/chuggernaut.prev
fi

# 1. Native build.
cargo build --release

# 2. Web SPA (served by the api from web/dist via UI_DIST).
( cd web && npm ci && npm run build )

# 3. Images + linux channel binary.
CHUG_IMAGE_TAG="${CHUG_IMAGE_TAG:-prod}" deploy/prod/build.sh

# Load prod config for the steps below.
set -a
. deploy/prod/chuggernaut.env
set +a

# 4. Idempotent init — creates only missing keys (e.g. a newly-added age key).
target/release/chuggernaut init --keys-dir "$KEYS_DIR" --repos-root "$REPOS_ROOT"

# 5. Rebuild/refresh the ssh front if its image changed.
GIT_UID="$(id -u)" docker compose -f deploy/prod/compose.yaml up -d --build ssh

# 6. Restart the host services (dispatcher restart is safe — §3.6 reconciles).
UID_N="$(id -u)"
launchctl kickstart -k "gui/$UID_N/com.chuggernaut.dispatcher"
launchctl kickstart -k "gui/$UID_N/com.chuggernaut.api"

# 7. Health check — non-zero exit here fails the Actions job.
HEALTH_URL="http://${BIND_ADDR:-127.0.0.1:8080}/"
ok=""
for _ in $(seq 1 30); do
  if curl -fsS "$HEALTH_URL" >/dev/null 2>&1; then ok=1; break; fi
  sleep 2
done
if [ -z "$ok" ]; then
  echo "update: health check FAILED at $HEALTH_URL" >&2
  exit 1
fi

echo "$TARGET_SHA" > "$MARK"
echo "update: deployed $TARGET_SHA OK"
