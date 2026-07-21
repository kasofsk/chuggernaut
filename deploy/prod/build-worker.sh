#!/bin/sh
# Build the agent images + channel binary for a REMOTE worker node, via its
# (SSH-tunneled) Docker endpoint. The worker may be a different architecture
# than the Mini — images build natively on its daemon, and the channel binary
# artifacts stage writes a worker-arch binary back to deploy/prod/out-nuc/
# (the dispatcher injects those bytes into every container it launches there;
# run-dispatcher.sh prefers out-nuc when it exists).
#
# No-ops cleanly when WORKER_DOCKER_HOST is unset (single-node deploys).
# Called from update.sh after the env is loaded; runnable by hand:
#   WORKER_DOCKER_HOST=tcp://127.0.0.1:23751 deploy/prod/build-worker.sh
set -eu

if [ -z "${WORKER_DOCKER_HOST:-}" ]; then
  echo "build-worker: WORKER_DOCKER_HOST unset — no worker node; skipping"
  exit 0
fi

cd "$(dirname "$0")"                 # deploy/prod
DEV="../dev"
CTX="../.."                          # build context = workspace root
TAG="${CHUG_IMAGE_TAG:-prod}"

export DOCKER_HOST="$WORKER_DOCKER_HOST"

# Worker-arch channel binary, written back through the tunnel to the Mini.
docker build -f "$DEV/Dockerfile.ssh" --target artifacts \
  --output type=local,dest=out-nuc "$CTX"

# Agent images the job types run in, native on the worker's daemon.
docker build -f "$DEV/Dockerfile.agent" -t "chuggernaut/agent:$TAG" "$DEV"
docker build -f Dockerfile.agent-rust -t "chuggernaut/agent-rust:$TAG" "$CTX"

echo "build-worker: chuggernaut/{agent,agent-rust}:$TAG on $WORKER_DOCKER_HOST; channel -> $(pwd)/out-nuc/chuggernaut-channel"
