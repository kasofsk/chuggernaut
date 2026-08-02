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

# DOCKER_BUILDKIT=1 requests the in-daemon BuildKit builder so the Dockerfiles'
# RUN --mount=type=cache dependency caches take effect (#115). It is a no-op on
# engines that already default to BuildKit and harmless where BuildKit is
# unavailable (the mounts are simply ignored, build stays cold). No buildx CLI
# plugin is required — the engine's built-in BuildKit is enough for cache mounts.
BK="DOCKER_BUILDKIT=1"

# Health-probe budget (overridable for the shell test). Defaults give ~60s.
PROBE_TIMEOUT_SECS="${PROBE_TIMEOUT_SECS:-60}"
PROBE_INTERVAL_SECS="${PROBE_INTERVAL_SECS:-3}"

# Stage every build context to a FILE, then feed `docker build` from it — never
# `git archive | ssh docker build`. A POSIX pipeline reports only the LAST
# command's status, so a `git archive` that dies mid-stream is masked by a
# `docker build` that "succeeds" on a truncated context (the 2026-07-23 incident:
# a buildx-missing failure printed ERROR yet the pipeline exit stayed 0 and the
# deploy sailed on, leaving a stale daemon). With a staged file, `set -e` aborts
# on the archive step itself, and the build reads a complete, verified context.
CTX="$(mktemp)"
trap 'rm -f "$CTX"' EXIT INT TERM

# Worker daemon image (repo-root context; bakes chuggernaut + channel binary).
# --label chug.git.sha=<sha> stamps the requested SHA INTO the image so we can
# positively prove, below, that the image the daemon will run was built from the
# commit we asked for — an exit code alone is not trustworthy here.
git archive --format=tar HEAD > "$CTX"
[ -s "$CTX" ] || { echo "build-worker: empty build context for worker image — aborting" >&2; exit 1; }
ssh "$WORKER_SSH" "$BK docker build -q -t chuggernaut/worker:$TAG \
    -f deploy/prod/Dockerfile.worker --build-arg CHUG_GIT_SHA=$SHA \
    --label chug.git.sha=$SHA -" < "$CTX"

# Positively assert the built image carries the requested SHA label BEFORE we
# restart the daemon onto it. A stale/failed build (label missing or mismatched)
# must never reach `docker run` — refuse loudly and leave the live daemon as-is.
GOT_LABEL="$(ssh "$WORKER_SSH" \
  "docker inspect --format '{{index .Config.Labels \"chug.git.sha\"}}' chuggernaut/worker:$TAG" \
  2>/dev/null | tr -d '[:space:]' || true)"
if [ "$GOT_LABEL" != "$SHA" ]; then
  echo "build-worker: worker image label '$GOT_LABEL' != requested SHA '$SHA' — REFUSING daemon restart (stale or failed build; live daemon untouched)" >&2
  exit 1
fi
echo "build-worker: verified chuggernaut/worker:$TAG carries chug.git.sha=$SHA"

# Agent images the job types run in, native on the node.
git archive --format=tar HEAD:deploy/dev > "$CTX"
[ -s "$CTX" ] || { echo "build-worker: empty build context for agent image — aborting" >&2; exit 1; }
ssh "$WORKER_SSH" "$BK docker build -q -t chuggernaut/agent:$TAG -f Dockerfile.agent -" < "$CTX"
git archive --format=tar HEAD > "$CTX"
[ -s "$CTX" ] || { echo "build-worker: empty build context for agent-rust image — aborting" >&2; exit 1; }
ssh "$WORKER_SSH" "$BK docker build -q -t chuggernaut/agent-rust:$TAG \
    -f deploy/prod/Dockerfile.agent-rust -" < "$CTX"

# (Re)start the worker daemon on the new image. Safe mid-job: containers
# survive, the dispatcher's poll-based wait re-attaches (spec §3.1).
# NODE/NATS URL expand HERE (from chuggernaut.env); \$HOME expands on the node.
NODE="${CHUG_WORKER_NODE:-nuc}"
NATS="${WORKER_NATS_URL:?set WORKER_NATS_URL (tailnet NATS URL of the dispatcher host)}"
# Pass the self-refresh coordinates (spec §3.1) through so a daemon started via
# this legacy path can also be refreshed later over the worker RPC (no-ssh path).
# Empty when unset — the daemon then just rejects refresh requests.
REFRESH_ENV="-e WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-} -e WORKER_GIT_KEY=${WORKER_GIT_KEY:-/data/keys/worker_git}"
# Node-local build cache (spec §3.1 "Node-local build caching"): pass the HOST
# path as ENV ONLY — no bind-mount into the DAEMON container is needed. The
# daemon adds the cache bind to each *sibling* job container via the docker
# socket using this host path, so the daemon itself never touches the cache
# files. Empty when unset ⇒ caching stays off (the daemon reads None). This is
# the durable fix for #55's dormant cache: baked-in sccache only warms when the
# daemon actually runs with WORKER_CACHE_DIR.
CACHE_ENV=""
if [ -n "${WORKER_CACHE_DIR:-}" ]; then
  CACHE_ENV="-e WORKER_CACHE_DIR=$WORKER_CACHE_DIR"
fi
# Disk pre-flight knobs (deploy #248, worker-refresh.sh): the refresh refuses a
# build that cannot fit a new image generation, sized by a conservative constant.
# A node with a different disk shape (a bigger colima volume, docker's data root
# on its own filesystem) tunes it here, at creation — the refresh's swap phase
# carries whatever is set forward, so the override survives self-refreshes.
# Empty when unset ⇒ the documented default applies.
DISK_ENV=""
if [ -n "${WORKER_REFRESH_DISK_FREE_GB_MIN:-}" ]; then
  DISK_ENV="-e WORKER_REFRESH_DISK_FREE_GB_MIN=$WORKER_REFRESH_DISK_FREE_GB_MIN"
fi
if [ -n "${WORKER_REFRESH_DISK_PATH:-}" ]; then
  DISK_ENV="$DISK_ENV -e WORKER_REFRESH_DISK_PATH=$WORKER_REFRESH_DISK_PATH"
fi
# The node's FIRST-BOOT capacity (`WORKER_SLOTS`, spec §3.1 dynamic registration):
# the number it starts at before any operator intent exists, and the last resort
# when the dispatcher is down. It is NOT how a node's concurrency is changed —
# that is a runtime command from the operator UI (`req.worker.{node}.set_slots`),
# which needs no ssh, no rebuild and no restart (docs/runbooks/worker-capacity.md).
# The passthrough stays deliberately: worker-refresh.sh's swap carries it forward,
# so after a swap the node reports this boot value until the dispatcher reconciles
# the recorded intent back onto it (one scan tick). Set it to something the node
# can serve (prod runs air and nuc at 2 each); empty when unset ⇒ the daemon's
# documented default of 4. The ceiling is a separate knob, `WORKER_SLOTS_MAX`,
# which this script does not pass — add it to the `docker run` below by hand on a
# node whose CPU count overstates what it can serve.
SLOTS_ENV=""
if [ -n "${WORKER_SLOTS:-}" ]; then
  SLOTS_ENV="-e WORKER_SLOTS=$WORKER_SLOTS"
fi
# KVM passthrough for Android emulator work (design #367 §2.3/§3.5, daemon side
# shipped by #374): the three node settings AND the device node itself. The
# device is not optional decoration — `chug-worker` is itself a container, so the
# daemon's "does this node have the device" check reads the DAEMON CONTAINER's
# own view (crates/worker/src/daemon.rs `build_backend`), and a daemon that gets
# WORKER_KVM without `--device` refuses to start, is restarted into the same
# refusal by --restart=always, and the node leaves the fleet.
#
# The value maps to a device path exactly as the daemon parses it
# (crates/worker/src/config.rs `parse_kvm_device`): a boolean turns on the
# default device node, an absolute path names another. A value that is neither is
# refused HERE, before the live daemon is removed, rather than by a replacement
# that then cannot boot. Values are single-quoted for the node's shell so an
# allow-list written with spaces (`acme/beacon, acme/api` — the daemon trims)
# cannot word-split into a stray `docker run` argument.
#
# Trimmed first, because `parse_kvm_device` trims before it matches: a ` 1 ` the
# daemon accepts must not be refused by the deploy, and a whitespace-only value
# must read as unset (the daemon's own reading) rather than as unparseable. The
# trimmed value is what rides in `-e`, so the daemon and worker-refresh.sh's swap
# both see exactly what was decided on here.
#
# All three empty when unset ⇒ no passthrough and no device: exactly the run this
# script produced before Android existed. Enabling KVM on a node and granting it
# to a project stay two separate acts — WORKER_KVM_PROJECTS is fail-closed, and
# an empty one grants nobody. docs/runbooks/worker-kvm.md is the procedure.
KVM_ENV=""
KVM_DEVICE_ARG=""
KVM="${WORKER_KVM:-}"
KVM="${KVM#"${KVM%%[![:space:]]*}"}"
KVM="${KVM%"${KVM##*[![:space:]]}"}"
if [ -n "$KVM" ]; then
  KVM_ENV="-e WORKER_KVM='$KVM'"
  case "$KVM" in
    0 | false | off) ;;
    1 | true | on) KVM_DEVICE_ARG="--device '/dev/kvm'" ;;
    /*) KVM_DEVICE_ARG="--device '$KVM'" ;;
    *)
      echo "build-worker: WORKER_KVM='$KVM' is neither 1/0 nor an absolute device path — the daemon would refuse to start on it (crates/worker/src/config.rs); REFUSING (live daemon untouched)" >&2
      exit 1
      ;;
  esac
fi
if [ -n "${WORKER_KVM_PROJECTS:-}" ]; then
  KVM_ENV="$KVM_ENV -e WORKER_KVM_PROJECTS='$WORKER_KVM_PROJECTS'"
fi
if [ -n "${WORKER_ANDROID_SDK_DIR:-}" ]; then
  KVM_ENV="$KVM_ENV -e WORKER_ANDROID_SDK_DIR='$WORKER_ANDROID_SDK_DIR'"
fi
# Log level for the daemon (ticket #270). The binary filters on RUST_LOG and its
# default directive is ERROR, so a daemon started without it emits nothing — not
# even the "worker up" line the probe below waits for, nor the refresh relay that
# is the node's only account of a self-refresh. `info` is where those lines live
# and costs nothing per-op; deps stay at warn. Overridable per node at creation
# via WORKER_RUST_LOG (a dedicated knob, so an unrelated RUST_LOG in the
# operator's own shell cannot leak into the fleet), and worker-refresh.sh's swap
# carries whatever is set forward across self-refreshes.
LOG_ENV="-e RUST_LOG=${WORKER_RUST_LOG:-info,async_nats=warn}"
REMOTE="docker rm -f chug-worker >/dev/null 2>&1 || true
docker run -d --restart=always --name chug-worker \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v \$HOME/chuggernaut-worker/keys:/data/keys:ro \
  -e WORKER_NODE=$NODE \
  -e NATS_URL=$NATS \
  -e NATS_CREDS=/data/keys/worker.creds \
  $LOG_ENV \
  $REFRESH_ENV \
  $CACHE_ENV \
  $DISK_ENV \
  $SLOTS_ENV \
  $KVM_ENV \
  $KVM_DEVICE_ARG \
  chuggernaut/worker:$TAG >/dev/null"
ssh "$WORKER_SSH" "$REMOTE"

# Positively PROVE the daemon actually came up before we claim "deployed". An
# exit code from `docker run` only says the container was created, not that it
# stayed up. A direct NATS ping from this laptop path is impractical (the
# dispatcher's NATS is not generally reachable here), so the probe demands the
# daemon's OWN proof of NATS liveness: the "worker up" log line, which the
# daemon emits only AFTER its NATS connection and worker-RPC subscription
# succeed (daemon.rs) — the ping RPC is serving once that line exists. This is
# strictly stronger than container-running + any-log-line (#207 review: a
# crash-looping daemon can log plenty without ever reaching NATS). A timeout
# is a LOUD failure with a non-zero exit — never a silent "deployed".
PROBE_REMOTE='r=$(docker inspect -f "{{.State.Running}}" chug-worker 2>/dev/null || echo false); [ "$r" = true ] && docker logs --tail 50 chug-worker 2>&1 | grep -q "worker up" && echo HEALTHY'
probe_deadline=$(( $(date +%s) + PROBE_TIMEOUT_SECS ))
probe_attempt=0
until ssh "$WORKER_SSH" "$PROBE_REMOTE" 2>/dev/null | grep -q HEALTHY; do
  probe_attempt=$((probe_attempt + 1))
  if [ "$(date +%s)" -ge "$probe_deadline" ]; then
    echo "build-worker: chug-worker did NOT report healthy within ${PROBE_TIMEOUT_SECS}s on $WORKER_SSH (State.Running + a "worker up" NATS-subscribed log line) — FAILED; the daemon is not confirmed up" >&2
    exit 1
  fi
  echo "build-worker: waiting for chug-worker to report healthy (attempt $probe_attempt) — retrying in ${PROBE_INTERVAL_SECS}s"
  sleep "$PROBE_INTERVAL_SECS"
done
echo "build-worker: verified chug-worker is running and NATS-subscribed (worker up) on $WORKER_SSH"

# Bound the node's docker disk (the 2026-07-23 air incident: 27G of BuildKit
# cache + dangling image generations filled the colima partition and an image
# build died ENOSPC mid-deploy). Each rebuild strands the previous image
# generation as dangling — prune those (NEVER -a: tagged agent images must
# survive, the #183 lesson) and cap the BuildKit cache at 15G, which keeps the
# hot cargo/sccache cache-mounts (#115) while shedding stale layers.
ssh "$WORKER_SSH" "docker image prune -f >/dev/null; docker builder prune -f --keep-storage 15GB >/dev/null 2>&1 || true"

echo "build-worker: chuggernaut/{worker,agent,agent-rust}:$TAG deployed + VERIFIED on $WORKER_SSH ($SHA) — image label matches and chug-worker is up"
