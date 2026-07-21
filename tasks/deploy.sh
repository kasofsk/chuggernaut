#!/bin/sh
# Deploy the current tree to prod. Run as the `work` step of a `deploy` job
# (jobs/deploy.yaml): a deploy job carries no commits, so HEAD is exactly the
# `main` the job was released from — that SHA is what we ship.
#
# Self-restart, by design: the ssh'd update.sh below `kickstart`s the dispatcher
# that supervises THIS job's container. That restart drops the dispatcher's
# in-memory state, but §3.6 reconciliation re-attaches to the still-running work
# container on the next tick and processes its exit normally. So the deploy job
# survives the deploy of its own supervisor — do NOT try to "fix" this by
# skipping the restart or decoupling the job; the reconcile loop is the design.
#
# Secret injection (spec §8.2): the dispatcher decrypts declared secrets and
# injects them as env vars — i.e. MINI_DEPLOY_KEY holds the OpenSSH *private key
# value*, not a path. ssh -i wants a file, so we write the value to a private
# (0600) tempfile and point -i at that. If a future injection form hands us a
# file path in MINI_DEPLOY_KEY_FILE instead, honour it directly.
set -eu

MINI_HOST="worksalot@100.116.243.42"      # the Mini's tailnet IP
REMOTE_UPDATE="~/chuggernaut/deploy/prod/update.sh"

# A deploy job has no commits of its own: HEAD == the released main.
SHA="$(git rev-parse HEAD)"

# Resolve the deploy key to a file for `ssh -i`.
if [ -n "${MINI_DEPLOY_KEY_FILE:-}" ]; then
  KEY_FILE="$MINI_DEPLOY_KEY_FILE"
elif [ -n "${MINI_DEPLOY_KEY:-}" ]; then
  KEY_FILE="$(mktemp)"
  chmod 600 "$KEY_FILE"
  # trailing newline matters for OpenSSH key parsing
  printf '%s\n' "$MINI_DEPLOY_KEY" > "$KEY_FILE"
  trap 'rm -f "$KEY_FILE"' EXIT INT TERM
else
  echo "deploy: MINI_DEPLOY_KEY (or MINI_DEPLOY_KEY_FILE) not set — cannot ssh" >&2
  exit 1
fi

echo "deploy: shipping $SHA to $MINI_HOST"
ssh -i "$KEY_FILE" \
  -o IdentitiesOnly=yes \
  -o StrictHostKeyChecking=accept-new \
  "$MINI_HOST" "$REMOTE_UPDATE $SHA"
