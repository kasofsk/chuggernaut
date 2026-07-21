#!/bin/sh
# Build the prod images and extract the linux chuggernaut-channel binary to
# deploy/prod/out/chuggernaut-channel (the dispatcher's CHANNEL_BINARY).
#
# Reuses the dev Dockerfiles (they compile chuggernaut + chuggernaut-channel
# for the Docker platform — arm64 linux on an M-series Mini) and only differs
# in the image tag. Idempotent; safe to re-run from update.sh.
set -eu

# Preflight: the first build below uses BuildKit (`docker build --output`), which
# needs the buildx CLI plugin. Fail with a pointer instead of a cryptic
# "unknown flag: --output" (see deploy/prod/README.md §0 for the plugin symlink).
if ! docker buildx version >/dev/null 2>&1; then
  echo "build.sh: 'docker buildx' not found — link the buildx CLI plugin (README §0)" >&2
  exit 1
fi

cd "$(dirname "$0")"                 # deploy/prod
DEV="../dev"                          # dev Dockerfiles + sshd_config live here
CTX="../.."                          # build context = workspace root
TAG="${CHUG_IMAGE_TAG:-prod}"
GIT_UID="${GIT_UID:-$(id -u)}"

# Linux channel binary for injection into agent containers.
docker build -f "$DEV/Dockerfile.ssh" --target artifacts \
  --output type=local,dest=out "$CTX"

# SSH front (embeds the linux chuggernaut binary + sshd forced command).
docker build -f "$DEV/Dockerfile.ssh" --build-arg "GIT_UID=$GIT_UID" \
  -t "chuggernaut/ssh:$TAG" "$CTX"

# Agent image the job types run in.
docker build -f "$DEV/Dockerfile.agent" -t "chuggernaut/agent:$TAG" "$DEV"

# Rust agent image for the chuggernaut dogfood project (repo-root context —
# it bakes a cargo prefetch of the workspace deps).
docker build -f Dockerfile.agent-rust -t "chuggernaut/agent-rust:$TAG" "$CTX"

# API service image (HTTP↔NATS bridge + web UI baked in).
docker build -f Dockerfile.api -t "chuggernaut/api:$TAG" "$CTX"

echo "built chuggernaut/{ssh,agent,agent-rust,api}:$TAG; channel -> $(pwd)/out/chuggernaut-channel"
