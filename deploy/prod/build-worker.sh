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
# The node this run builds. Read here rather than at the `docker run` below
# because the per-node declaration layer keys off it, and everything the run
# spec is composed from must be resolved before the first thing reads it.
NODE="${CHUG_WORKER_NODE:-nuc}"

# ── the DECLARED run spec (ticket #390) ──────────────────────────────────────
# A fleet has more than one node and their paths differ (a colima node's cache
# lives under the mac home that is shared into the VM; a NixOS node's under
# /var/cache), so a single-valued `WORKER_CACHE_DIR` in chuggernaut.env can only
# ever be true of ONE node — and the other node's spec then lives nowhere but
# inside its running container, which is exactly how the four settings this
# ticket found (#265 reason 3) came to survive by circulation instead of by
# declaration. Any `WORKER_*_<node>` in the environment therefore overrides the
# bare `WORKER_*` here, so one env file declares a FLEET.
#
# Derived from the environment rather than from a list of knob names: a list
# would be a second copy of the run spec, drifting from the composition below
# the first time a knob is added. Node names that are not shell identifiers
# cannot be looked up this way — said once, out loud, instead of silently
# ignoring a declaration the operator wrote.
#
# WORKER_SSH is deliberately NOT resolved per node: it is the switch that says
# "this machine can reach that node at all" (the whole script no-ops without
# it), and prod's deploy depends on that no-op — the Mini cannot ssh a tagged
# worker. A per-node destination that switched the script ON, or retargeted it
# after the images were built, would be a different script.
build_worker_run_spec_per_node() {
  case "$NODE" in
    '' | *[!A-Za-z0-9_]*)
      echo "build-worker: node name '$NODE' is not a shell identifier — per-node declarations (<VAR>_$NODE) cannot be looked up; using the bare values"
      return 0
      ;;
  esac
  for _var in $(env | sed -n "s/^\(WORKER_[A-Za-z0-9_]*\)_$NODE=.*/\1/p" | sort -u); do
    [ "$_var" != "WORKER_SSH" ] || continue
    # `env` prints VALUES too, so a multi-line one can contribute a line that
    # merely LOOKS like a declaration; take only names that are really set,
    # rather than clobbering a good value with an empty one.
    eval "_set=\${${_var}_$NODE+x}"
    [ -n "$_set" ] || continue
    eval "$_var=\$${_var}_$NODE"
    echo "build-worker: run spec: $_var declared per node (${_var}_$NODE)"
  done
}
build_worker_run_spec_per_node

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
# Scratch for the run-spec drift check below (the live daemon's environment).
SPEC="$(mktemp)"
trap 'rm -f "$CTX" "$SPEC"' EXIT INT TERM

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
NATS="${WORKER_NATS_URL:?set WORKER_NATS_URL (tailnet NATS URL of the dispatcher host)}"
# Pass the self-refresh coordinates (spec §3.1) through so a daemon started via
# this legacy path can also be refreshed later over the worker RPC (no-ssh path).
# Empty when unset — the daemon then just rejects refresh requests.
REFRESH_ENV="-e WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-} -e WORKER_GIT_KEY=${WORKER_GIT_KEY:-/data/keys/worker_git}"
# An empty URL is not a neutral default: the node comes up healthy, serves jobs,
# and is SKIPPED by every subsequent deploy — a node that looks like it is
# participating and has quietly stopped updating (#382, one guise over). The
# daemon reports the skip when a deploy asks it to refresh; this says it at the
# moment the node is given the spec, which is the moment it can still be fixed.
if [ -z "${WORKER_REFRESH_GIT_URL:-}" ]; then
  echo "build-worker: WARNING: WORKER_REFRESH_GIT_URL is undeclared — $NODE will be built WITHOUT self-refresh coordinates and every deploy will SKIP it ('refresh SKIPPED — no git credential'). Declare it in deploy/prod/chuggernaut.env on the Mini (README §6)." >&2
fi
# Node-local build cache (spec §3.1 "Node-local build caching"): pass the HOST
# path as ENV ONLY — no bind-mount into the DAEMON container is needed. The
# daemon adds the cache bind to each *sibling* job container via the docker
# socket using this host path, so the daemon itself never touches the cache
# files. Empty when unset ⇒ caching stays off (the daemon reads None). This is
# the durable fix for #55's dormant cache: baked-in sccache only warms when the
# daemon actually runs with WORKER_CACHE_DIR.
#
# The HOST directory is provisioned HERE, at node creation, because nothing else
# does: the daemon's own `create_dir_all` (crates/worker/src/daemon.rs) runs
# inside the daemon container, which does not mount this path, so it lands in
# that container's writable layer and never on the host. Until #379 dockerd
# covered for that — the sibling launch's `-v` silently created a missing source
# — but the cache is a typed mount now and the engine REFUSES a missing source,
# so a fresh node with WORKER_CACHE_DIR set would fail every launch, permanently.
#
# Plain `mkdir -p` first (idempotent: an existing dir is a success, which is
# every node built before this, and needs no privilege), `sudo -n` only as the
# fallback for a first create under a root-owned parent like /var/cache. No
# `chmod`: the dir keeps the node's default ownership and mode, which is exactly
# what dockerd's silent create produced and what the fleet has run on — job
# containers write to it as root (neither agent Dockerfile sets `USER`, and the
# launch config sets no user), and widening the mode of a directory that already
# holds a warm cache is not this script's call. A failure REFUSES the deploy
# before the live daemon is touched, rather than starting a daemon whose every
# launch fails.
#
# Ownership of this step is #372's if the chug-node module ever lands: it
# provisions the same path via systemd.tmpfiles (#372 §5, the treatment #373
# Decision 4 gives the nix gcroots dir). This is the bridge until then, and it
# moves out in the same change that lands the module — never alongside it.
CACHE_ENV=""
if [ -n "${WORKER_CACHE_DIR:-}" ]; then
  CACHE_ENV="-e WORKER_CACHE_DIR=$WORKER_CACHE_DIR"
  if ! ssh "$WORKER_SSH" "mkdir -p '$WORKER_CACHE_DIR' 2>/dev/null || sudo -n mkdir -p '$WORKER_CACHE_DIR'"; then
    echo "build-worker: cannot provision WORKER_CACHE_DIR '$WORKER_CACHE_DIR' on $WORKER_SSH (tried mkdir -p, then sudo -n mkdir -p) — the daemon would start and then fail EVERY launch with 'bind source path does not exist'; REFUSING daemon restart (live daemon untouched). Create it by hand on the node, or unset WORKER_CACHE_DIR to run without caching." >&2
    exit 1
  fi
  echo "build-worker: host cache dir $WORKER_CACHE_DIR present on $WORKER_SSH"
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
# shipped by #374): the node settings AND the device node itself. The
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
# All of them empty when unset ⇒ no passthrough and no device: exactly the run this
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
# The node's Flutter SDK (#393): a SECOND, independent toolchain leaf, mounted
# read-only at /opt/flutter for an allow-listed launch with FLUTTER_ROOT pointed
# at it. Optional by construction — unset ⇒ no mount, no env, and the run spec is
# byte-identical to what it was, so an Android-only node needs no migration and
# WORKER_ANDROID_SDK_DIR's meaning is untouched. It is NOT realised and takes no
# GC root: #373 P2's declared project toolchains supersede this stopgap.
if [ -n "${WORKER_FLUTTER_DIR:-}" ]; then
  KVM_ENV="$KVM_ENV -e WORKER_FLUTTER_DIR='$WORKER_FLUTTER_DIR'"
fi
# The node's JDK (#397): a THIRD leaf on the same terms, mounted read-only at
# /opt/jdk with JAVA_HOME pointed at it. It exists because the nix wrappers'
# resolve-my-own-JDK trick stops at gradle, which Flutter invokes and which is
# not a wrapper (design #367 correction 14): the SDK tools ran without a JDK on
# PATH, `flutter build apk` did not. Optional exactly as the others are — unset ⇒
# no mount, no env, no migration — and superseded by #373 P2 with them.
if [ -n "${WORKER_JDK_DIR:-}" ]; then
  KVM_ENV="$KVM_ENV -e WORKER_JDK_DIR='$WORKER_JDK_DIR'"
fi
# Per-task nix GC roots (design #373 P1, daemon side in crates/worker/src/nix.rs).
# WORKER_NIX_GCROOTS_DIR is the switch: unset ⇒ nothing below happens and the run
# spec is exactly what it was. Set ⇒ the daemon realises the node's declared
# toolchain before an allow-listed launch and holds an indirect GC root over it
# for the task's lifetime, so a weekly `nix-gc` cannot collect a store path out
# from under a running task (the mounts #374 shipped hold NO root today).
#
# Four mounts, each load-bearing, plus a fifth when a device is attached:
#   * the store read-only (WORKER_NIX_STORE_DIR, nix's own default) — the closure
#     the client and the task both read, and the prefix the daemon's boot check
#     requires a realise target to resolve into.
#   * the profiles dir read-only — where the CLIENT comes from. Not the store
#     path: chug-worker is long-lived and survives many `nixos-rebuild`s, and
#     docker resolves a bind source host-side at create, so a client resolved at
#     create pins the generation current at the last swap — which
#     `--delete-older-than` collects. `/nix/var/nix/gcroots/profiles` is itself a
#     root, so a client resolved THROUGH the profiles at each use follows the
#     node's current generation and is never collectable.
#   * the daemon socket dir READ-WRITE — connecting to a unix socket needs write
#     on the inode, so a read-only parent will not do. The socket is 0666 on a
#     stock node, so no uid mapping is needed (the /dev/kvm situation in #367).
#   * the roots dir read-write, at the SAME path inside the container as on the
#     host: the nix daemon registers an indirect root by the path it is handed,
#     and resolves that path in its own (host) namespace.
#
# The roots dir is provisioned here for the same reason WORKER_CACHE_DIR is
# (#380): the daemon's own view is a container, so nothing it does reaches the
# host path. #372 §5 A5 declines to own this — the chug-node modules contribute
# nothing for GC roots — so this script is the provisioner, and a failure REFUSES
# the deploy with the live daemon untouched rather than starting a daemon that
# refuses to boot and is looped by --restart=always.
#
# TRUST COST, stated where an operator will see it (design #373 3b): giving the
# worker container the nix daemon socket means anything that realises here runs
# in the process that also holds docker.sock, the NATS creds and the git key.
# P1 realises only what the NODE already declares (WORKER_ANDROID_SDK_DIR) — no
# project-supplied flake is evaluated, so P1 does not yet incur the unsandboxed
# project-code half of that cost. It does incur the rest: unbounded build CPU and
# disk are reachable through the socket, which is why the realise is bounded and
# the node is single-tenant (#373 Decision 1).
NIX_ENV=""
NIX_MOUNT_ARGS=""
GCROOTS="${WORKER_NIX_GCROOTS_DIR:-}"
GCROOTS="${GCROOTS#"${GCROOTS%%[![:space:]]*}"}"
GCROOTS="${GCROOTS%"${GCROOTS##*[![:space:]]}"}"
if [ -n "$GCROOTS" ]; then
  case "$GCROOTS" in
    /*) ;;
    *)
      echo "build-worker: WORKER_NIX_GCROOTS_DIR='$GCROOTS' is not an absolute host path — the daemon would refuse to start on it (crates/worker/src/config.rs); REFUSING (live daemon untouched)" >&2
      exit 1
      ;;
  esac
  NIX_PROFILES_DIR="${WORKER_NIX_PROFILES_DIR:-/nix/var/nix/profiles}"
  NIX_SOCKET="${WORKER_NIX_DAEMON_SOCKET:-/nix/var/nix/daemon-socket/socket}"
  NIX_SOCKET_DIR="${NIX_SOCKET%/*}"
  NIX_STORE_DIR="${WORKER_NIX_STORE_DIR:-/nix/store}"
  if ! ssh "$WORKER_SSH" "mkdir -p '$GCROOTS' 2>/dev/null || sudo -n mkdir -p '$GCROOTS'"; then
    echo "build-worker: cannot provision WORKER_NIX_GCROOTS_DIR '$GCROOTS' on $WORKER_SSH (tried mkdir -p, then sudo -n mkdir -p) — the daemon refuses to start without it and --restart=always would loop it, taking the node out of the fleet; REFUSING daemon restart (live daemon untouched). Create it by hand on the node, or unset WORKER_NIX_GCROOTS_DIR to run without per-task GC roots." >&2
    exit 1
  fi
  if ! ssh "$WORKER_SSH" "[ -d '$NIX_STORE_DIR' ] && [ -d '$NIX_PROFILES_DIR' ] && [ -S '$NIX_SOCKET' ]"; then
    echo "build-worker: $WORKER_SSH lacks the nix preconditions for WORKER_NIX_GCROOTS_DIR (want '$NIX_STORE_DIR', '$NIX_PROFILES_DIR' and the daemon socket '$NIX_SOCKET') — REFUSING daemon restart (live daemon untouched). This node has no nix daemon; unset WORKER_NIX_GCROOTS_DIR." >&2
    exit 1
  fi
  NIX_ENV="-e WORKER_NIX_GCROOTS_DIR='$GCROOTS'"
  if [ "$NIX_STORE_DIR" != "/nix/store" ]; then
    NIX_ENV="$NIX_ENV -e WORKER_NIX_STORE_DIR='$NIX_STORE_DIR'"
  fi
  if [ -n "${WORKER_NIX_CLIENT:-}" ]; then
    NIX_ENV="$NIX_ENV -e WORKER_NIX_CLIENT='$WORKER_NIX_CLIENT'"
  fi
  if [ "$NIX_SOCKET" != "/nix/var/nix/daemon-socket/socket" ]; then
    NIX_ENV="$NIX_ENV -e WORKER_NIX_DAEMON_SOCKET='$NIX_SOCKET'"
  fi
  if [ -n "${WORKER_NIX_REALISE_TIMEOUT_SECS:-}" ]; then
    # Refused HERE for the same reason WORKER_KVM's shape is: a bound the daemon
    # rejects at parse time would be found by a replacement that then cannot
    # boot. The ceiling is NIX_REALISE_TIMEOUT_SECS_MAX in
    # crates/worker/src/config.rs (the `launch` RPC's 60s budget less the
    # container create) — that file is the source of truth; this mirrors it so
    # the deploy fails fast instead of the node leaving the fleet.
    case "$WORKER_NIX_REALISE_TIMEOUT_SECS" in
      '' | *[!0-9]*)
        echo "build-worker: WORKER_NIX_REALISE_TIMEOUT_SECS='$WORKER_NIX_REALISE_TIMEOUT_SECS' is not a number of seconds — the daemon would refuse to start on it (crates/worker/src/config.rs); REFUSING (live daemon untouched)" >&2
        exit 1
        ;;
    esac
    if [ "$WORKER_NIX_REALISE_TIMEOUT_SECS" -lt 1 ] || [ "$WORKER_NIX_REALISE_TIMEOUT_SECS" -gt 45 ]; then
      echo "build-worker: WORKER_NIX_REALISE_TIMEOUT_SECS='$WORKER_NIX_REALISE_TIMEOUT_SECS' is outside 1..45 — the realise runs inside the launch RPC the dispatcher abandons after 60s, so the daemon refuses a longer bound at parse time (crates/worker/src/config.rs NIX_REALISE_TIMEOUT_SECS_MAX); REFUSING (live daemon untouched)" >&2
      exit 1
    fi
    NIX_ENV="$NIX_ENV -e WORKER_NIX_REALISE_TIMEOUT_SECS='$WORKER_NIX_REALISE_TIMEOUT_SECS'"
  fi
  NIX_MOUNT_ARGS="-v '$NIX_STORE_DIR':'$NIX_STORE_DIR':ro -v '$NIX_PROFILES_DIR':'$NIX_PROFILES_DIR':ro -v '$NIX_SOCKET_DIR':'$NIX_SOCKET_DIR' -v '$GCROOTS':'$GCROOTS'"
  # And a FIFTH mount when a device is actually attached: the DIRECTORY HOLDING
  # the toolchain path, never that path itself. `nix-store --realise` resolves
  # its argument CLIENT-side — inside chug-worker, before it says anything to the
  # nix daemon — and the operator's stable path is a symlink into the store, so
  # the client has to be able to READ that symlink. A bind whose source IS the
  # stable path destroys it: mount(2) resolves the source host-side, and the
  # container gets the store path's CONTENT at a non-store PATH, which the client
  # refuses ("not in the Nix store"). Binding the parent keeps the symlink a
  # symlink and lets it resolve through the store mount above — and, because a
  # bound directory is shared rather than copied, it follows the node across a
  # `nixos-rebuild` instead of pinning the generation current at this deploy.
  #
  # Hence the shape requirement checked below: a DIRECT, absolute symlink into
  # the store under a real parent directory (a `systemd.tmpfiles` `L+` line).
  # NixOS's `environment.etc` routes each entry through `/etc/static`, a second
  # hop that no mount here reproduces — refused with the remedy named, rather
  # than deployed into a daemon whose every admitted launch fails. Gated on
  # $KVM_DEVICE_ARG because attaching the device is exactly the condition that
  # makes a launch admitted, and therefore realised
  # (crates/worker/src/daemon.rs `realise_for_launch`); the daemon's own boot
  # check re-derives the same property from inside the container
  # (crates/worker/src/nix.rs `store_target`).
  if [ -n "$KVM_DEVICE_ARG" ]; then
    SDK_DIR="${WORKER_ANDROID_SDK_DIR:-/var/lib/chuggernaut/android-sdk}"
    SDK_DIR="${SDK_DIR#"${SDK_DIR%%[![:space:]]*}"}"
    SDK_DIR="${SDK_DIR%"${SDK_DIR##*[![:space:]]}"}"
    SDK_PARENT="${SDK_DIR%/*}"
    if [ -z "$SDK_PARENT" ]; then
      echo "build-worker: WORKER_ANDROID_SDK_DIR='$SDK_DIR' sits directly under / — chug-worker would have to bind the node's root filesystem to reach it; REFUSING (live daemon untouched). Put the toolchain path in a directory of its own." >&2
      exit 1
    fi
    if ! ssh "$WORKER_SSH" "[ -d '$SDK_PARENT' ] && [ -L '$SDK_DIR' ] && case \"\$(readlink '$SDK_DIR')\" in '$NIX_STORE_DIR'/*) [ -e '$SDK_DIR' ] ;; *) false ;; esac"; then
      echo "build-worker: per-task nix GC roots are on and WORKER_KVM attaches a device, but '$SDK_DIR' on $WORKER_SSH is not a direct symlink into '$NIX_STORE_DIR' under a real parent directory — chug-worker mounts '$SDK_PARENT' read-only and resolves that symlink itself, so anything else (a plain directory, or a NixOS environment.etc entry, which hops through /etc/static) cannot be realised and the daemon refuses to start; REFUSING daemon restart (live daemon untouched). Declare it as one hop — systemd.tmpfiles.rules = [ \"L+ $SDK_DIR - - - - \${pkgs.androidsdk}\" ] — or unset WORKER_NIX_GCROOTS_DIR." >&2
      exit 1
    fi
    NIX_MOUNT_ARGS="$NIX_MOUNT_ARGS -v '$SDK_PARENT':'$SDK_PARENT':ro"
  fi
  echo "build-worker: per-task nix GC roots on — roots dir $GCROOTS present on $WORKER_SSH"
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
  $NIX_ENV \
  $NIX_MOUNT_ARGS \
  chuggernaut/worker:$TAG >/dev/null"

# ── run-spec drift (ticket #390) ─────────────────────────────────────────────
# The live container is not a declaration. Everything above composes the run
# spec from the environment, and `docker rm -f chug-worker` then makes that
# composition the node's whole truth — so any setting the LIVE daemon carries
# that this composition does not is DROPPED here, silently, and the node comes
# back degraded in a way nothing reports: caching off (#55), the boot capacity
# back at the daemon's default, or a node that keeps serving jobs and quietly
# stops updating. That is #265 reason 3, and this is where it is catchable.
#
# The comparison is against $REMOTE, not against the shell's variables, because
# only $REMOTE knows what will actually be passed: a knob this script does not
# forward (WORKER_SLOTS_MAX is the documented one) is dropped no matter what
# chuggernaut.env says about it, and a check that read the env would call that
# clean. Presence decides the refusal; the VALUE comparison is informational
# only, so a quoted value this cannot parse costs a noisy line and never a
# wrong verdict.
#
# A drop REFUSES, with the live daemon still running, in the same spirit as the
# label and KVM-device guards above — removing a setting on purpose is a real
# thing to want, so WORKER_SPEC_DROP_OK=1 is the way to say so out loud.
build_worker_run_spec_drift() {
  # stdin from /dev/null: this ssh reads nothing, and update.sh runs this whole
  # script over an ssh session whose stdin it must not swallow.
  ssh "$WORKER_SSH" \
    "docker inspect chug-worker --format '{{range .Config.Env}}{{println .}}{{end}}'" \
    > "$SPEC" 2>/dev/null < /dev/null || true
  if [ ! -s "$SPEC" ]; then
    echo "build-worker: no live chug-worker on $WORKER_SSH to compare against — this run declares $NODE's whole run spec"
    return 0
  fi
  _dropped=""
  while IFS= read -r _line; do
    case "$_line" in
      WORKER_*=*) ;;
      *) continue ;;
    esac
    _key="${_line%%=*}"
    _live="${_line#*=}"
    case "$_key" in
      *[!A-Za-z0-9_]*) continue ;;
    esac
    _passed=""
    case "$REMOTE" in
      *"-e $_key="*) _passed=1 ;;
    esac
    if [ -z "$_passed" ]; then
      _dropped="$_dropped $_key"
      eval "_declared=\${$_key:-}"
      if [ -n "$_declared" ]; then
        echo "build-worker: run-spec DRIFT on $NODE: the live daemon runs $_key=$_live and it IS declared, but build-worker.sh does not forward $_key — recreating the daemon DROPS it (env.example says so for WORKER_SLOTS_MAX)" >&2
      else
        echo "build-worker: run-spec DRIFT on $NODE: the live daemon runs $_key=$_live and nothing declares it — recreating the daemon DROPS it" >&2
      fi
      continue
    fi
    case "$_key" in
      WORKER_NODE) _declared="$NODE" ;;
      WORKER_GIT_KEY) _declared="${WORKER_GIT_KEY:-/data/keys/worker_git}" ;;
      *) eval "_declared=\${$_key:-}" ;;
    esac
    [ "$_declared" != "$_live" ] || continue
    echo "build-worker: run-spec change on $NODE: $_key: live '$_live' -> declared '$_declared'"
  done < "$SPEC"
  [ -n "$_dropped" ] || return 0
  if [ -n "${WORKER_SPEC_DROP_OK:-}" ]; then
    echo "build-worker: WORKER_SPEC_DROP_OK is set — dropping$_dropped from $NODE's run spec deliberately"
    return 0
  fi
  echo "build-worker: REFUSING daemon restart (live daemon untouched): the run spec composed here drops$_dropped, which the live daemon on $NODE is running. Declare them in deploy/prod/chuggernaut.env ON THE MINI — per node as <VAR>_$NODE (deploy/prod/README.md §6) — or set WORKER_SPEC_DROP_OK=1 to remove them on purpose." >&2
  return 1
}
build_worker_run_spec_drift || exit 1

echo "build-worker: run spec for $NODE: WORKER_SLOTS=${WORKER_SLOTS:-<unset: daemon default 4>} WORKER_CACHE_DIR=${WORKER_CACHE_DIR:-<unset: caching OFF>} WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-<unset: cannot self-refresh>} WORKER_GIT_KEY=${WORKER_GIT_KEY:-/data/keys/worker_git}"
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
