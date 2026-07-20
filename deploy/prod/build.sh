#!/bin/sh
# Build the prod images and extract the linux chuggernaut-channel binary to
# deploy/prod/out/chuggernaut-channel (the dispatcher's CHANNEL_BINARY).
#
# Reuses the dev Dockerfiles (they compile chuggernaut + chuggernaut-channel
# for the Docker platform — arm64 linux on an M-series Mini) and only differs
# in the image tag. Idempotent; safe to re-run from update.sh.
set -eu

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

echo "built chuggernaut/ssh:$TAG, chuggernaut/agent:$TAG; channel -> $(pwd)/out/chuggernaut-channel"
