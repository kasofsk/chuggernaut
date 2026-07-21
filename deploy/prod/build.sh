#!/bin/sh
# Build the Mini's container substrate: the SSH front image and the linux
# chuggernaut-channel binary extracted to deploy/prod/out/chuggernaut-channel
# (the dispatcher's CHANNEL_BINARY). That's ALL the Mini builds now — the api
# runs natively (README §2), and job containers run only on worker nodes, which
# build their own agent images (build-worker.sh). No cargo compile runs in the
# VM here.
#
# Reuses the dev Dockerfile.ssh (it compiles chuggernaut + chuggernaut-channel
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

echo "built chuggernaut/ssh:$TAG; channel -> $(pwd)/out/chuggernaut-channel"
