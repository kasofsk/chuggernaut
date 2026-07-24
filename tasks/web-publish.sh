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

# Tailnet address on the dedicated :2200 sshd (LaunchDaemon
# com.chuggernaut.sshd2200): worker containers behind colima NAT cannot reach
# the LAN, and Tailscale SSH owns tailnet :22 (rejects tagged->tagged).
MINI_HOST="worksalot@100.116.243.42"
MINI_PORT=2200
UI_ROOT="\$HOME/chuggernaut-data/ui"   # expanded remotely

SHA="$(git rev-parse HEAD)"

# Single cleanup for every temp file this script stages (the tarball below and,
# when we materialize one, the deploy key). POSIX sh allows only one EXIT trap,
# so both paths funnel through these vars — a second `trap` would clobber the
# first and leak whichever file the other path staged.
TARBALL=""
KEY_TMP=""
trap 'rm -f "$TARBALL" "$KEY_TMP" 2>/dev/null || true' EXIT INT TERM

# Resolve the deploy key to a file for `ssh -i` (same contract as deploy.sh).
if [ -n "${MINI_DEPLOY_KEY_FILE:-}" ]; then
  KEY_FILE="$MINI_DEPLOY_KEY_FILE"
elif [ -n "${MINI_DEPLOY_KEY:-}" ]; then
  KEY_FILE="$(mktemp)"
  KEY_TMP="$KEY_FILE"
  chmod 600 "$KEY_FILE"
  printf '%s\n' "$MINI_DEPLOY_KEY" > "$KEY_FILE"
else
  echo "web-publish: MINI_DEPLOY_KEY (or MINI_DEPLOY_KEY_FILE) not set — cannot ssh" >&2
  exit 1
fi
SSH="ssh -i $KEY_FILE -p $MINI_PORT -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"

echo "web-publish: building web/dist at $SHA"
cd web
npm ci --no-audit --no-fund
# Bake this commit into the bundle (vite `define` reads CHUG_GIT_SHA) so the
# published web SHA survives the self-publish flow (#63) and the cluster view
# shows which commit the live UI is on — the same SHA a full deploy bakes.
CHUG_GIT_SHA="$SHA" npm run build
cd ..

echo "web-publish: staging dist tarball"
# Stage the archive to a FILE first, never straight into the ssh pipe. A POSIX
# pipeline reports only the LAST command's exit status, so `tar | ssh "rsync
# --delete"` can mask a tar that dies mid-stream while the remote rsync succeeds
# — with --delete that WIPES the served UI under a "done" message (#186). By
# staging locally and POSITIVELY asserting the archive is non-empty AND carries
# index.html before it can reach the remote --delete, an empty/partial tar can
# never propagate: the wipe is structurally impossible, not merely unlikely.
TARBALL="$(mktemp)"
tar -C web/dist -cf "$TARBALL" .
if [ ! -s "$TARBALL" ]; then
  echo "web-publish: staged tarball is EMPTY — refusing to ship (a --delete sync of an empty tree would WIPE the served UI)" >&2
  exit 1
fi
if ! tar -tf "$TARBALL" | grep -Eq '(^|/)index\.html$'; then
  echo "web-publish: staged tarball has no index.html — refusing to ship (would leave the UI broken/wiped)" >&2
  exit 1
fi

echo "web-publish: shipping dist to $MINI_HOST:$UI_ROOT"
# Stream the VERIFIED tarball from the file; swap contents in place remotely
# (macOS has rsync natively). stdin comes from a file, so there is no upstream
# pipeline whose failure could be masked.
$SSH "$MINI_HOST" "
  set -eu
  rm -rf $UI_ROOT.new && mkdir -p $UI_ROOT.new $UI_ROOT
  tar -xf - -C $UI_ROOT.new
  rsync -a --delete $UI_ROOT.new/ $UI_ROOT/
  rm -rf $UI_ROOT.new
" < "$TARBALL"

echo "web-publish: done — $SHA is live (refresh the page)"
