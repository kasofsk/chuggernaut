#!/bin/sh
# Build + deploy the worker-node pieces ON the worker over plain SSH (no
# Docker endpoint on any network, no tunnel): the worker daemon image (which
# bakes the worker-arch chuggernaut + channel binaries at this git SHA) and
# the agent images job types reference. Build context streams over ssh via
# `git archive`, so the node needs nothing but Docker and an authorized key.
#
# No-ops cleanly when WORKER_SSH is unset (single-node deploys).
# Called from update.sh after the env is loaded; runnable by hand:
#   WORKER_SSH=worksalot@gumbo-nuc-0 deploy/prod/build-worker.sh
set -eu

if [ -z "${WORKER_SSH:-}" ]; then
  echo "build-worker: WORKER_SSH unset — no worker node; skipping"
  exit 0
fi

cd "$(dirname "$0")/../.."             # workspace root
TAG="${CHUG_IMAGE_TAG:-prod}"
SHA="$(git rev-parse HEAD)"

# Worker daemon image (repo-root context; bakes chuggernaut + channel binary).
git archive --format=tar HEAD \
  | ssh "$WORKER_SSH" "docker build -q -t chuggernaut/worker:$TAG \
      -f deploy/prod/Dockerfile.worker --build-arg CHUG_GIT_SHA=$SHA -"

# Agent images the job types run in, native on the node.
git archive --format=tar HEAD:deploy/dev \
  | ssh "$WORKER_SSH" "docker build -q -t chuggernaut/agent:$TAG -f Dockerfile.agent -"
git archive --format=tar HEAD \
  | ssh "$WORKER_SSH" "docker build -q -t chuggernaut/agent-rust:$TAG \
      -f deploy/prod/Dockerfile.agent-rust -"

# (Re)start the worker daemon on the new image. Safe mid-job: containers
# survive, the dispatcher's poll-based wait re-attaches (spec §3.1).
# NODE/NATS URL expand HERE (from chuggernaut.env); \$HOME expands on the node.
NODE="${CHUG_WORKER_NODE:-nuc}"
NATS="${WORKER_NATS_URL:?set WORKER_NATS_URL (tailnet NATS URL of the dispatcher host)}"
REMOTE="docker rm -f chug-worker >/dev/null 2>&1 || true
docker run -d --restart=always --name chug-worker \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v \$HOME/chuggernaut-worker/keys:/data/keys:ro \
  -e WORKER_NODE=$NODE \
  -e NATS_URL=$NATS \
  -e NATS_CREDS=/data/keys/worker.creds \
  chuggernaut/worker:$TAG >/dev/null"
ssh "$WORKER_SSH" "$REMOTE"

echo "build-worker: chuggernaut/{worker,agent,agent-rust}:$TAG deployed on $WORKER_SSH ($SHA)"
