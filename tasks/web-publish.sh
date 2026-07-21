#!/bin/sh
# Publish the web UI at HEAD to the Mini's served UI directory (UI_ROOT, which
# the api container bind-mounts at /srv/web — deploy/prod/README §7). Run as
# the work step of a `web-publish` job (jobs/web-publish.yaml): the job
# carries no commits, so HEAD == the main it was released from.
#
# The swap replaces directory CONTENTS, never the directory itself — UI_ROOT
# is a live bind mount, and replacing the inode would detach it from the
# running container. Staged extract + rsync --delete keeps the window where
# index.html and its hashed assets disagree to a minimum.
set -eu

# LAN address, not tailnet (same reasoning as tasks/deploy.sh: Tailscale SSH
# owns :22 on the tailnet interface and rejects tagged->tagged).
MINI_HOST="worksalot@192.168.129.128"
UI_ROOT="\$HOME/chuggernaut-data/ui"   # expanded remotely

SHA="$(git rev-parse HEAD)"

# Resolve the deploy key to a file for `ssh -i` (same contract as deploy.sh).
if [ -n "${MINI_DEPLOY_KEY_FILE:-}" ]; then
  KEY_FILE="$MINI_DEPLOY_KEY_FILE"
elif [ -n "${MINI_DEPLOY_KEY:-}" ]; then
  KEY_FILE="$(mktemp)"
  chmod 600 "$KEY_FILE"
  printf '%s\n' "$MINI_DEPLOY_KEY" > "$KEY_FILE"
  trap 'rm -f "$KEY_FILE"' EXIT INT TERM
else
  echo "web-publish: MINI_DEPLOY_KEY (or MINI_DEPLOY_KEY_FILE) not set — cannot ssh" >&2
  exit 1
fi
SSH="ssh -i $KEY_FILE -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"

echo "web-publish: building web/dist at $SHA"
cd web
npm ci --no-audit --no-fund
npm run build
cd ..

echo "web-publish: shipping dist to $MINI_HOST:$UI_ROOT"
# Stage remotely, then swap contents in place (macOS has rsync natively).
tar -C web/dist -cf - . | $SSH "$MINI_HOST" "
  set -eu
  rm -rf $UI_ROOT.new && mkdir -p $UI_ROOT.new $UI_ROOT
  tar -xf - -C $UI_ROOT.new
  rsync -a --delete $UI_ROOT.new/ $UI_ROOT/
  rm -rf $UI_ROOT.new
"

echo "web-publish: done — $SHA is live (refresh the page)"
