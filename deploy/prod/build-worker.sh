#!/bin/sh
# Build + deploy the worker-node pieces ON the worker over plain SSH (no
# Docker endpoint on any network, no tunnel): the worker daemon image (which
# bakes the worker-arch chuggernaut + channel binaries at this git SHA) and
# the agent images job types reference. Build context streams over ssh via
# `git archive`, so the node needs nothing but Docker and an authorized key.
#
# The daemon itself is NOT a container (design #440 D1/D2/D6): the binary is
# extracted from the image just built and supervised natively — a systemd unit
# on Linux, a launchd agent in the login user's GUI domain on macOS — over an
# environment file that carries the whole run spec.
#
# EXTRACTED ON LINUX, COMPILED ON THE NODE ON macOS (#440's 2026-08-07
# correction). The image is a Linux container, so on a Darwin node its binary is
# an ELF file launchd reports as `cannot execute binary file` — measured on
# gumbo-air-0, 2026-08-06. D6's "no host Rust toolchain" promise is Linux-only,
# and a Darwin node declares a cargo instead.
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

# EVERY ssh below reads `< /dev/null` except the three that are genuinely fed a
# build context. `ssh <host> <cmd>` with no redirect hands the remote command
# THIS script's stdin, and update.sh runs this whole script over an ssh session —
# so a call that reads nothing still drains the session's stdin, and a caller
# holding that pipe open blocks the deploy where nothing is waiting on anything.
# The drift check said this of its own two calls; it is true of all of them, and
# the health probe (a loop) is the one that turns it into a hang.
#
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

# ── the node's platform, and the paths that follow from it (design #440 D2) ──
# The daemon is supervised natively, so the supervisor, the unit path and the
# restart command are all decided by what the node IS. Asked in ONE round trip
# before anything is built — a node this script cannot supervise refuses in
# seconds rather than after a ten-minute image build — and $HOME comes back with
# it because the keys directory and the macOS agent both hang off a value that
# cannot be expanded from here.
#
# The node's TOOLCHAIN rides in the same round trip because on Darwin the daemon
# binary is COMPILED HERE (below), and a node that cannot compile one must
# refuse in seconds rather than after three image builds. Three answers, not
# one, because `command -v cargo` is not the question the compile asks:
#
#   * where `cargo` is — `command -v` of WORKER_CARGO or of plain `cargo`, over
#     ssh, which is the shell this deploy actually gets rather than the
#     interactive one an operator sees;
#   * whether `rustc` resolves with cargo's OWN DIRECTORY on PATH — cargo looks
#     its compiler up through PATH (RUSTC, then `build.rustc`, then a plain
#     lookup), and an absolute WORKER_CARGO is declared precisely BECAUSE the
#     bare name is not on that PATH. Without this the deploy passes the guard
#     and dies mid-compile, after three image builds;
#   * whether that cargo RUNS at all — a rustup shim with no default toolchain
#     is on PATH, execs, and fails every build.
#
# All three empty on a Linux node, where nothing reads them.
NODE_PROBE="$(ssh "$WORKER_SSH" "printf '%s\n%s\n' \"\$(uname -s)\" \"\$HOME\"
_c=\$(command -v '${WORKER_CARGO:-cargo}' 2> /dev/null || true)
_r=
_v=
if [ -n \"\$_c\" ]; then
  _r=\$(PATH=\"\$(dirname \"\$_c\"):\$PATH\"; command -v rustc 2> /dev/null || true)
  \"\$_c\" --version > /dev/null 2>&1 && _v=ok
fi
printf '%s\n%s\n%s\n' \"\$_c\" \"\$_r\" \"\$_v\"" < /dev/null)"
NODE_OS="$(printf '%s\n' "$NODE_PROBE" | sed -n 1p)"
NODE_HOME="$(printf '%s\n' "$NODE_PROBE" | sed -n 2p)"
NODE_CARGO="$(printf '%s\n' "$NODE_PROBE" | sed -n 3p)"
NODE_RUSTC="$(printf '%s\n' "$NODE_PROBE" | sed -n 4p)"
NODE_CARGO_RUNS="$(printf '%s\n' "$NODE_PROBE" | sed -n 5p)"
case "$NODE_OS" in
  Linux | Darwin) ;;
  *)
    echo "build-worker: $WORKER_SSH reports 'uname -s' = '$NODE_OS' — the worker daemon is supervised by systemd (Linux) or launchd (macOS) and there is no third supervisor here (design #440 D2); REFUSING (live daemon untouched)" >&2
    exit 1
    ;;
esac
if [ -z "$NODE_HOME" ]; then
  echo "build-worker: $WORKER_SSH reports no \$HOME — the node's keys directory and its log path are both relative to it; REFUSING (live daemon untouched)" >&2
  exit 1
fi
# The daemon's node-local artifacts land at the paths crates/worker/src/config.rs
# ALREADY defaults to (WORKER_CHANNEL_BINARY, WORKER_REFRESH_SCRIPT): they were
# shaped like host paths while being image paths, so materialising the same
# layout on the host makes both defaults correct with no code change (#440 §4).
BIN_DIR=/usr/local/bin
LIB_DIR=/usr/local/lib/chuggernaut
# Where the daemon reads its NATS credential and its git key off the NODE. The
# `:ro` bind of $HOME/chuggernaut-worker/keys is gone with the container, and the
# boundary it was pretending to give with it — the bind SOURCE was a directory in
# the login user's home, and that user is in the `docker` group and is who this
# script ssh's in as. So the default is a ROOT-OWNED 0700 directory beside the
# unit's own environment file, outside any user's home (design #440 D5), and the
# guard below refuses a node whose directory is not one.
if [ "$NODE_OS" = Linux ]; then
  UNIT_DIR="${WORKER_UNIT_DIR:-/etc/systemd/system}"
  UNIT_PATH="$UNIT_DIR/chug-worker.service"
  ENV_FILE="${WORKER_ENV_FILE:-/etc/chuggernaut/worker.env}"
  KEYS_DIR="${WORKER_KEYS_DIR:-/etc/chuggernaut/keys}"
else
  AGENT_LABEL=com.chuggernaut.worker
  UNIT_PATH="$NODE_HOME/Library/LaunchAgents/$AGENT_LABEL.plist"
  ENV_FILE="${WORKER_ENV_FILE:-$NODE_HOME/chuggernaut-worker/worker.env}"
  WORKER_LOG_PATH="$NODE_HOME/Library/Logs/chuggernaut/worker.log"
  # macOS runs the daemon as the LOGIN USER in their GUI domain (#322: the
  # keychain and CoreSimulator are per-user-session services), so there is no
  # user for a root-owned directory to exclude and D5's boundary does not port.
  # The home path is therefore the default here, unchanged — the status quo #440
  # §4 names, and the gap #322 §7 already lists under what it gives up.
  KEYS_DIR="${WORKER_KEYS_DIR:-$NODE_HOME/chuggernaut-worker/keys}"
  # Where the node compiles its own daemon (#440's 2026-08-07 correction). It is
  # KEPT between deploys on purpose: `target/` is what makes the second build
  # minutes instead of a cold workspace compile, and the node's self-refresh
  # builds into the same directory — which is why the path rides in the run spec
  # below rather than being re-derived at each end.
  BUILD_DIR="${WORKER_BUILD_DIR:-$NODE_HOME/chuggernaut-worker/build}"
fi
ENV_DIR="${ENV_FILE%/*}"

# ── the Darwin node's Rust toolchain (design #440 D6, corrected 2026-08-07) ───
# D6 extracts the daemon binary from the worker image because that keeps its
# build environment byte-identical and needs no host Rust toolchain. THAT HOLDS
# ON LINUX AND CANNOT HOLD ON macOS: the image is a Linux container and the host
# is Darwin, so what `docker cp` lifts out of it is an `ELF 64-bit LSB pie
# executable, ARM aarch64` that launchd loops on with `cannot execute binary
# file` (gumbo-air-0, 2026-08-06). Of the three ways to get a Mach-O daemon —
# compile on the node, cross-compile on the builder, ship a prebuilt artifact —
# only the first needs nothing this platform does not have: cross-compiling to
# Darwin needs the macOS SDK and a Darwin linker (i.e. a mac) on the builder,
# and a prebuilt artifact needs somewhere to put it, which is design #313 gap 11
# and does not exist. So a Darwin node DECLARES a toolchain, and the honest cost
# is stated rather than hidden: that node's binary is built by the NODE's cargo,
# not by the pinned `rust:` image, so its build environment is the node's.
#
# Refused here, before anything is built, with the live daemon untouched — the
# same place every other "this node cannot serve the spec" refusal lives.
#
# CARGO_DIR is the whole toolchain, not one binary. Everything that compiles —
# the remote build below, and the DAEMON'S OWN self-refresh, whose PATH is the
# launchd agent's — runs with it prepended, because a nix-darwin profile
# directory or a rustup home is on none of the PATHs either end would otherwise
# have.
if [ "$NODE_OS" = Darwin ]; then
  if [ -z "$NODE_CARGO" ]; then
    echo "build-worker: $WORKER_SSH is a Darwin node and 'command -v ${WORKER_CARGO:-cargo}' found nothing over ssh — the daemon binary in the worker image is a LINUX binary (design #440 D6 is Linux-only, corrected 2026-08-07: on the air it installed as ELF aarch64 and launchd looped 'cannot execute binary file'), so a mac COMPILES its own and this one has no cargo this deploy can reach; REFUSING (live daemon untouched). Install a Rust toolchain on the node, and if it is not on the ssh PATH (a nix-darwin or rustup one usually is not) declare WORKER_CARGO_$NODE=<absolute path to cargo> in deploy/prod/chuggernaut.env ON THE MINI — 'ssh $WORKER_SSH command -v cargo' is the exact question this asked." >&2
    exit 1
  fi
  if [ -z "$NODE_CARGO_RUNS" ]; then
    echo "build-worker: $WORKER_SSH has '$NODE_CARGO' but '$NODE_CARGO --version' failed over ssh — a rustup shim with no default toolchain is on PATH and execs while compiling nothing, so the deploy would die mid-build with the images already replaced; REFUSING (live daemon untouched). Run 'ssh $WORKER_SSH $NODE_CARGO --version' to see what it says, and set the node's default toolchain ('rustup default stable')." >&2
    exit 1
  fi
  if [ -z "$NODE_RUSTC" ]; then
    echo "build-worker: $WORKER_SSH has '$NODE_CARGO' but no 'rustc' beside it or on the ssh PATH — cargo resolves its compiler THROUGH PATH, so declaring an absolute WORKER_CARGO makes 'command -v' pass while the compile still fails; REFUSING (live daemon untouched). Put rustc where cargo is (a rustup or nix-darwin toolchain installs both in one directory) — 'ssh $WORKER_SSH PATH=$(dirname "$NODE_CARGO"):\$PATH command -v rustc' is the exact question this asked." >&2
    exit 1
  fi
  CARGO_DIR="$(dirname "$NODE_CARGO")"
fi

# ── the docker engine the daemon dials (#440's 2026-08-07 correction) ────────
# `WORKER_DOCKER_ENDPOINT` has existed in crates/worker/src/config.rs all along
# and NOTHING has ever rendered it, because its default — /var/run/docker.sock —
# was correct by construction while the daemon was a container with that socket
# bind-mounted in. Natively on macOS it is not: colima listens at
# ~/.colima/default/docker.sock, and the converted air answered every launch
# with `backend unavailable: Socket not found: /var/run/docker.sock`
# (2026-08-06). Same shape as WORKER_MODES: forwarded, per-node overridable,
# UNSET STAYS UNSET so a node that declares nothing produces the run spec it
# produced before this existed.
#
# On Darwin it is DERIVED when undeclared, from the node's own `docker context`
# — the same answer the `docker build` calls below get, so the daemon dials the
# engine that just built its images rather than one an operator had to notice
# and write down. THE DERIVED VALUE IS A SNAPSHOT: it is written into the
# environment file and read at daemon start, so a node whose docker context
# changes underneath it keeps dialing the old socket and fails every launch
# until it is re-converted (or the file is edited and the agent kickstarted).
# That is the same durability every other line in this file has, and it is why
# the derivation announces the value it wrote.
#
# The shape is refused for WORKER_KVM's reason: DockerBackend::new rejects
# anything that is not unix:// or tcp:// / http:// (crates/container/src/docker.rs)
# and that is a start-time error the supervisor would loop. The socket is
# checked for its own reason — a wrong path is not a daemon that refuses to
# boot, it is a node that comes up healthy, announces its slots, and fails every
# launch. Both are asked HERE, before the first image build, because neither
# needs anything built and the alternative is a full conversion spent to learn
# that the node cannot serve the spec.
DOCKER_ENDPOINT="${WORKER_DOCKER_ENDPOINT:-}"
DOCKER_ENDPOINT="${DOCKER_ENDPOINT#"${DOCKER_ENDPOINT%%[![:space:]]*}"}"
DOCKER_ENDPOINT="${DOCKER_ENDPOINT%"${DOCKER_ENDPOINT##*[![:space:]]}"}"
DOCKER_ENDPOINT_DEFAULT="unix:///var/run/docker.sock"
DOCKER_ENDPOINT_DERIVED=""
if [ -z "$DOCKER_ENDPOINT" ] && [ "$NODE_OS" = Darwin ]; then
  DOCKER_ENDPOINT="$(ssh "$WORKER_SSH" "docker context inspect --format '{{.Endpoints.docker.Host}}' 2> /dev/null || true" < /dev/null | tr -d '[:space:]')"
  if [ -n "$DOCKER_ENDPOINT" ]; then
    DOCKER_ENDPOINT_DERIVED=1
    echo "build-worker: WORKER_DOCKER_ENDPOINT derived from $WORKER_SSH's own docker context: $DOCKER_ENDPOINT"
  fi
fi
if [ -n "$DOCKER_ENDPOINT" ]; then
  case "$DOCKER_ENDPOINT" in
    unix://* | tcp://* | http://*) ;;
    *)
      echo "build-worker: WORKER_DOCKER_ENDPOINT='$DOCKER_ENDPOINT' is neither unix:// nor tcp:// / http:// — the daemon refuses it when it opens the backend (crates/container/src/docker.rs) and the supervisor would loop that refusal; REFUSING (live daemon untouched)" >&2
      exit 1
      ;;
  esac
fi
DOCKER_SOCKET="${DOCKER_ENDPOINT:-$DOCKER_ENDPOINT_DEFAULT}"
case "$DOCKER_SOCKET" in
  unix://*)
    DOCKER_SOCKET="${DOCKER_SOCKET#unix://}"
    if ! ssh "$WORKER_SSH" "[ -S '$DOCKER_SOCKET' ]" < /dev/null; then
      echo "build-worker: the daemon on $NODE would dial '$DOCKER_SOCKET' and that is not a socket on $WORKER_SSH — the node would come up, announce its slots and fail EVERY launch with 'backend unavailable: Socket not found: $DOCKER_SOCKET' (gumbo-air-0, 2026-08-06); REFUSING (live daemon untouched). Read the node's real endpoint off it with 'ssh $WORKER_SSH docker context inspect --format \"{{.Endpoints.docker.Host}}\"' (on a mac colima answers ~/.colima/default/docker.sock) and declare WORKER_DOCKER_ENDPOINT_$NODE=unix://<path> in deploy/prod/chuggernaut.env ON THE MINI." >&2
      exit 1
    fi
    ;;
esac

# ── the credential directory, checked before anything is built (design #440 D5) ─
# A daemon that cannot read its own credential does not come up degraded, it
# FAILS TO START, and the supervisor's Restart=always loops that failure on a
# node an operator has just converted. So the whole of it is asked here, in one
# round trip, before the ten-minute image build and with the live daemon still
# running: does the directory exist, is it root's at 0700, and is the credential
# inside it there at all.
#
# WORKER_GIT_KEY defaulted to `/data/keys/worker_git`, which only ever existed
# INSIDE the container. A native daemon resolves it on the node, so the default
# follows the keys directory — and a declaration still naming the mount point is
# refused rather than handed to a daemon that cannot fetch anything.
GIT_KEY="${WORKER_GIT_KEY:-$KEYS_DIR/worker_git}"
case "$GIT_KEY" in
  /data/keys/*)
    echo "build-worker: WORKER_GIT_KEY='$GIT_KEY' names the container's key mount, which a NATIVE daemon does not have (design #440 D2) — the node would come up and every self-refresh would fail to fetch; REFUSING (live daemon untouched). Declare WORKER_GIT_KEY_$NODE=$KEYS_DIR/worker_git in deploy/prod/chuggernaut.env ON THE MINI, or drop it and take that default." >&2
    exit 1
    ;;
esac
# The SAME finding one directory over, and the one this slice creates: a Linux
# node's declaration still naming the LOGIN USER's home. It is not merely the
# weaker boundary D5 refuses for the credential — README §6's migration DELETES
# that copy, so the run spec would name a file that is gone and the node would
# keep serving jobs while every self-refresh silently failed to fetch, which is
# the failure the /data/keys refusal above exists to prevent. A path INSIDE the
# credential directory is exempt however it was reached: the owner-and-mode guard
# below has already vouched for that directory, so a node keeping its keys under
# a root-owned 0700 WORKER_KEYS_DIR is served rather than refused twice.
if [ "$NODE_OS" = Linux ]; then
  case "$GIT_KEY" in
    "$KEYS_DIR"/*) ;;
    "$NODE_HOME"/*)
      echo "build-worker: WORKER_GIT_KEY='$GIT_KEY' is under the login user's home ('$NODE_HOME') on a Linux node, outside the credential directory '$KEYS_DIR' — that user is in the 'docker' group, so the git key is readable by anything they run (design #440 D5), and deploy/prod/README.md §6's migration DELETES that copy, after which the node keeps serving jobs and every self-refresh fails to fetch; REFUSING (live daemon untouched). Move the key with the credential ('sudo install -o root -g root -m 0600 $GIT_KEY $KEYS_DIR/worker_git', and worker_git-cert.pub beside it), then drop WORKER_GIT_KEY_$NODE from deploy/prod/chuggernaut.env ON THE MINI to take the default '$KEYS_DIR/worker_git', or point it there." >&2
      exit 1
      ;;
  esac
fi
if [ "$NODE_OS" = Linux ]; then
  # THE READ IS TRI-STATE for the credential, the way the run-spec drift guard's
  # is: inside a root-owned 0700 directory the login user cannot look at all, so
  # "not there" and "I am not allowed to look" produce the same failed `test -r`.
  # Collapsing them would tell an operator to re-mint a credential that is
  # already installed correctly, which is the cryptic failure this whole block
  # exists to avoid. The owner and mode come back as data rather than as a
  # verdict so the refusal can name what it actually found.
  KEYS_PROBE="$(ssh "$WORKER_SSH" "printf 'own=%s mode=%s\n' \"\$(stat -c %U '$KEYS_DIR' 2>/dev/null)\" \"\$(stat -c %a '$KEYS_DIR' 2>/dev/null)\"
if [ -r '$KEYS_DIR/worker.creds' ] || sudo -n test -r '$KEYS_DIR/worker.creds' 2>/dev/null; then
  echo creds=readable
elif sudo -n true 2>/dev/null; then
  echo creds=absent
else
  echo creds=unknown
fi" < /dev/null)"
  KEYS_OWNER="$(printf '%s\n' "$KEYS_PROBE" | sed -n 's/^own=\([^ ]*\) mode=.*/\1/p')"
  KEYS_MODE="$(printf '%s\n' "$KEYS_PROBE" | sed -n 's/^own=.* mode=\(.*\)$/\1/p')"
  KEYS_CREDS="$(printf '%s\n' "$KEYS_PROBE" | sed -n 's/^creds=\(.*\)$/\1/p')"
  if [ -z "$KEYS_OWNER" ]; then
    echo "build-worker: '$KEYS_DIR' does not exist on $WORKER_SSH — a natively supervised daemon reads its NATS credential and its git key from a ROOT-OWNED 0700 directory outside any user's home (design #440 D5), and it FAILS TO START without them; REFUSING (live daemon untouched). On the node: 'sudo install -d -o root -g root -m 0700 $KEYS_DIR', then install the credential into it (deploy/prod/README.md §6, which is also where an existing node's move out of \$HOME is written down). Or point WORKER_KEYS_DIR_$NODE at the directory that already holds them." >&2
    exit 1
  fi
  if [ "$KEYS_OWNER" != root ] || [ "$KEYS_MODE" != 700 ]; then
    echo "build-worker: '$KEYS_DIR' on $WORKER_SSH is owned by '$KEYS_OWNER' at mode '$KEYS_MODE'; the daemon's credential directory must be owned by 'root' at mode '700' (design #440 D5). The login user this deploy ssh's in as is in the 'docker' group, so a credential that user can read is readable by anything that user runs — a WEAKER boundary than the read-only bind mount the native daemon replaces, and going native must not lower it. REFUSING (live daemon untouched). On the node: 'sudo chown root:root $KEYS_DIR && sudo chmod 0700 $KEYS_DIR' (and 'sudo chown root:root $KEYS_DIR/* && sudo chmod 0600 $KEYS_DIR/*' for what is inside it), or point WORKER_KEYS_DIR_$NODE at a directory that already is one." >&2
    exit 1
  fi
  case "$KEYS_CREDS" in
    readable) ;;
    unknown)
      echo "build-worker: cannot tell whether '$KEYS_DIR/worker.creds' exists on $WORKER_SSH — it is inside a root-owned 0700 directory, so only root can look, and 'sudo -n' is not available to the login user there. A check that cannot see the credential is not a check that passes, and the install needs that same passwordless sudo to write the unit and restart it; REFUSING (live daemon untouched). Grant the login user passwordless sudo on this node, or run this script from an account that has it." >&2
      exit 1
      ;;
    *)
      echo "build-worker: '$KEYS_DIR/worker.creds' is not on $WORKER_SSH — a native daemon reads its NATS credential off the node (there is no /data/keys mount any more), so it would fail to connect and the supervisor would restart it into the same failure; REFUSING (live daemon untouched). Mint it on the Mini with 'chuggernaut admin worker-creds --node $NODE', scp it to a staging path the login user owns, then 'sudo install -o root -g root -m 0600 <staged> $KEYS_DIR/worker.creds' (deploy/prod/README.md §6). Or point WORKER_KEYS_DIR_$NODE at the directory that holds it." >&2
      exit 1
      ;;
  esac
  echo "build-worker: credential directory $KEYS_DIR on $WORKER_SSH is root-owned at 0700 and holds worker.creds (design #440 D5)"
else
  if ! ssh "$WORKER_SSH" "[ -r '$KEYS_DIR/worker.creds' ]" < /dev/null; then
    echo "build-worker: '$KEYS_DIR/worker.creds' is not readable on $WORKER_SSH — a native daemon reads its NATS credential off the node (there is no /data/keys mount any more), so it would fail to connect and the supervisor would restart it into the same failure; REFUSING (live daemon untouched). Mint it with 'chuggernaut admin worker-creds --node $NODE' and install it there (deploy/prod/README.md §6), or point WORKER_KEYS_DIR_$NODE at the directory that holds it." >&2
    exit 1
  fi
  echo "build-worker: NOTE: $NODE runs the daemon as the LOGIN USER in their GUI domain, so design #440 D5's root-owned 0700 boundary does not port to macOS — $KEYS_DIR stays in that user's home and cross-task secret isolation there remains given up (#322 §7)"
fi

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
  < /dev/null 2>/dev/null | tr -d '[:space:]' || true)"
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

# ── the Mach-O daemon a Darwin node cannot get out of a Linux image ──────────
# The same context the worker image was built from, unpacked on the node and
# compiled by the node's own cargo. `--locked` so the dependency graph is the
# tree's Cargo.lock and not whatever resolves today — the closest this gets to
# the image's byte-identical promise, and a stale lock refuses here rather than
# shipping a different graph. CHUG_GIT_SHA is passed because the daemon reads it
# with `option_env!` (crates/worker/src/daemon.rs) and the fleet's version
# column is that value; the image passes the same one as a --build-arg.
#
# The source tree is replaced and the target directory is not: a cold workspace
# compile on a mac is tens of minutes, and this is the only thing that makes a
# re-conversion cheap. It is the same directory the node's own self-refresh
# builds in, which is why WORKER_BUILD_DIR rides in the run spec — and why this
# leaves `native.sha` behind exactly as that refresh does. Two writers share the
# staging directory and both must leave it self-describing: the refresh's swap
# refuses when that marker disagrees with the image's `chug.git.sha`, which is
# the only thing standing between it and installing a binary from another
# generation.
#
# CARGO_DIR goes on PATH because cargo resolves `rustc` through it and this is
# the ssh shell's PATH, not the operator's — the same reason WORKER_CARGO had to
# be declared absolute in the first place.
if [ "$NODE_OS" = Darwin ]; then
  git archive --format=tar HEAD > "$CTX"
  [ -s "$CTX" ] || { echo "build-worker: empty build context for the native daemon build — aborting" >&2; exit 1; }
  echo "build-worker: compiling the daemon natively on $WORKER_SSH ($NODE_CARGO, into $BUILD_DIR) — the worker image is Linux and this node is not (design #440 D6, corrected 2026-08-07)"
  ssh "$WORKER_SSH" "set -e
rm -rf '$BUILD_DIR/src'
mkdir -p '$BUILD_DIR/src'
tar -xf - -C '$BUILD_DIR/src'
cd '$BUILD_DIR/src'
PATH='$CARGO_DIR':\"\$PATH\" CHUG_GIT_SHA='$SHA' CARGO_TARGET_DIR='$BUILD_DIR/target' '$NODE_CARGO' build --release --locked --bin chuggernaut --bin chuggernaut-channel
printf '%s\n' '$SHA' > '$BUILD_DIR/native.sha'" < "$CTX"
fi

# (Re)start the worker daemon on the new binary. Safe mid-job: job containers
# are siblings on the node's docker socket and survive, and the dispatcher's
# poll-based wait re-attaches (spec §3.1). NODE/NATS URL expand HERE (from
# chuggernaut.env); every path below is resolved against the node's own $HOME.
NATS="${WORKER_NATS_URL:?set WORKER_NATS_URL (tailnet NATS URL of the dispatcher host)}"

# ── the environment file the supervisor hands the daemon (design #440 D2) ────
# One line per setting, in place of the `-e` flags the `docker run` composed.
# Values are single-quoted because BOTH readers are shell-like — systemd's
# EnvironmentFile parser and the macOS agent's `. <file>` — so `container, host`
# is one value on each rather than a word-split on one. A value that carries a
# single quote of its own is refused rather than escaped two ways: no run spec
# has ever held one, and a wrong guess here is a daemon that will not boot.
SPEC_ENV=""
spec_line() {
  case "$2" in
    *"'"*)
      echo "build-worker: run spec: $1 contains a single quote, which the environment file cannot carry unambiguously (systemd's EnvironmentFile parser and the macOS agent's shell each read it as a quote); REFUSING (live daemon untouched)" >&2
      exit 1
      ;;
  esac
  SPEC_ENV="$SPEC_ENV$1='$2'
"
}
# The self-refresh coordinates (spec §3.1) ride so the node can also be
# refreshed later over the worker RPC (no-ssh path). Empty URL when unset — the
# daemon then just rejects refresh requests. $GIT_KEY and the credential
# directory holding it were decided and checked above, before the build.
spec_line WORKER_NODE "$NODE"
spec_line NATS_URL "$NATS"
spec_line NATS_CREDS "$KEYS_DIR/worker.creds"
# Log level for the daemon (ticket #270). The binary filters on RUST_LOG and its
# default directive is ERROR, so a daemon started without it emits nothing — not
# even the "worker up" line the probe below waits for, nor the refresh relay that
# is the node's only account of a self-refresh. `info` is where those lines live
# and costs nothing per-op; deps stay at warn. Overridable per node at creation
# via WORKER_RUST_LOG (a dedicated knob, so an unrelated RUST_LOG in the
# operator's own shell cannot leak into the fleet), and worker-refresh.sh's swap
# carries whatever is set forward across self-refreshes.
spec_line RUST_LOG "${WORKER_RUST_LOG:-info,async_nats=warn}"
spec_line WORKER_REFRESH_GIT_URL "${WORKER_REFRESH_GIT_URL:-}"
spec_line WORKER_GIT_KEY "$GIT_KEY"
# An empty URL is not a neutral default: the node comes up healthy, serves jobs,
# and is SKIPPED by every subsequent deploy — a node that looks like it is
# participating and has quietly stopped updating (#382, one guise over). The
# daemon reports the skip when a deploy asks it to refresh; this says it at the
# moment the node is given the spec, which is the moment it can still be fixed.
if [ -z "${WORKER_REFRESH_GIT_URL:-}" ]; then
  echo "build-worker: WARNING: WORKER_REFRESH_GIT_URL is undeclared — $NODE will be built WITHOUT self-refresh coordinates and every deploy will SKIP it ('refresh SKIPPED — no git credential'). Declare it in deploy/prod/chuggernaut.env on the Mini (README §6)." >&2
fi
# Node-local build cache (spec §3.1 "Node-local build caching"): a HOST path the
# daemon adds as a bind to each *sibling* job container via the docker socket, so
# the daemon itself never touches the cache files. Empty when unset ⇒ caching
# stays off (the daemon reads None). This is the durable fix for #55's dormant
# cache: baked-in sccache only warms when the daemon actually runs with
# WORKER_CACHE_DIR.
#
# The HOST directory is still provisioned HERE, at node creation, and the reason
# is now PERMISSION rather than reach: a native daemon's own `create_dir_all`
# (crates/worker/src/daemon.rs, in `local_backend` before it serves anything)
# does land on the host, but only where it can create — a path under a
# root-owned parent is a config refusal at start that the supervisor then loops
# on. Provisioning first also keeps the directory's ownership and mode what the
# fleet has run on.
#
# Plain `mkdir -p` first (idempotent: an existing dir is a success, which is
# every node built before this, and needs no privilege), `sudo -n` only as the
# fallback for a first create under a root-owned parent like /var/cache. No
# `chmod`: the dir keeps the node's default ownership and mode, which is exactly
# what dockerd's silent create produced and what the fleet has run on — job
# containers write to it as root (neither agent Dockerfile sets `USER`, and the
# launch config sets no user), and widening the mode of a directory that already
# holds a warm cache is not this script's call. A failure REFUSES the deploy
# before the live daemon is touched, rather than starting a daemon that refuses
# its own config (native) or that comes up and fails every launch (container).
#
# Ownership of this step is #372's if the chug-node module ever lands: it
# provisions the same path via systemd.tmpfiles (#372 §5, the treatment #373
# Decision 4 gives the nix gcroots dir). This is the bridge until then, and it
# moves out in the same change that lands the module — never alongside it.
if [ -n "${WORKER_CACHE_DIR:-}" ]; then
  spec_line WORKER_CACHE_DIR "$WORKER_CACHE_DIR"
  if ! ssh "$WORKER_SSH" "mkdir -p '$WORKER_CACHE_DIR' 2>/dev/null || sudo -n mkdir -p '$WORKER_CACHE_DIR'" < /dev/null; then
    echo "build-worker: cannot provision WORKER_CACHE_DIR '$WORKER_CACHE_DIR' on $WORKER_SSH (tried mkdir -p, then sudo -n mkdir -p) — a native daemon cannot create it either and would REFUSE TO START, and a containerized one starts and then fails EVERY launch with 'bind source path does not exist'; REFUSING daemon restart (live daemon untouched). Create it by hand on the node, or unset WORKER_CACHE_DIR to run without caching." >&2
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
if [ -n "${WORKER_REFRESH_DISK_FREE_GB_MIN:-}" ]; then
  spec_line WORKER_REFRESH_DISK_FREE_GB_MIN "$WORKER_REFRESH_DISK_FREE_GB_MIN"
fi
if [ -n "${WORKER_REFRESH_DISK_PATH:-}" ]; then
  spec_line WORKER_REFRESH_DISK_PATH "$WORKER_REFRESH_DISK_PATH"
fi
# The node's FIRST-BOOT capacity (`WORKER_SLOTS`, spec §3.1 dynamic registration):
# the number it starts at before any operator intent exists, and the last resort
# when the dispatcher is down. It is NOT how a node's concurrency is changed —
# that is a runtime command from the operator UI (`req.worker.{node}.set_slots`),
# which needs no ssh, no rebuild and no restart (docs/reference/runbooks/worker-capacity.md).
# The passthrough stays deliberately: worker-refresh.sh's swap carries it forward,
# so after a swap the node reports this boot value until the dispatcher reconciles
# the recorded intent back onto it (one scan tick). Set it to something the node
# can serve (prod runs air and nuc at 2 each); empty when unset ⇒ the daemon's
# documented default of 4. The ceiling is a separate knob, `WORKER_SLOTS_MAX`,
# which this script does not pass — add it to the node's environment file by hand
# on a node whose CPU count overstates what it can serve.
if [ -n "${WORKER_SLOTS:-}" ]; then
  spec_line WORKER_SLOTS "$WORKER_SLOTS"
fi
# ── the docker engine the daemon dials: only the LINE is composed here ──────
# The value was resolved and both its refusals answered far above, beside the
# toolchain one (nothing this script builds is needed to answer either). Only
# the composition is here, so the run spec's ordering is unchanged, and unset
# still stays unset.
#
# A DERIVED value equal to the daemon's own default is dropped rather than
# written: deriving must not make a node that declared nothing carry a line it
# never chose. An explicitly declared one always rides, so an operator who wants
# the default said out loud gets it and the drift guard sees it.
if [ -n "$DOCKER_ENDPOINT" ] &&
  ! { [ -n "$DOCKER_ENDPOINT_DERIVED" ] && [ "$DOCKER_ENDPOINT" = "$DOCKER_ENDPOINT_DEFAULT" ]; }; then
  spec_line WORKER_DOCKER_ENDPOINT "$DOCKER_ENDPOINT"
fi
# The Darwin node's own build coordinates, in the run spec because the node's
# SELF-refresh needs exactly what this run needed: which cargo, and where the
# tree and its target directory live. A Linux node gets neither line and its
# swap keeps extracting from the image (#440 D6, which holds there).
if [ "$NODE_OS" = Darwin ]; then
  spec_line WORKER_CARGO "$NODE_CARGO"
  spec_line WORKER_BUILD_DIR "$BUILD_DIR"
fi
# The runtimes the node OFFERS (`WORKER_MODES`, design #309 P0 / #322 W1, daemon
# side shipped by #434). A node property exactly like WORKER_CACHE_DIR: the node
# DECLARES it and nothing verifies it — no probe for a toolchain, an interpreter,
# a nix daemon or anything else, here or in the daemon — and nothing on the wire
# reads it yet (#309 P2 owns NodeCapabilities). Declaring `host` therefore makes
# host jobs runnable on exactly no node: `runtime.mode: host` validates since
# #309 P1 (job #478), but nothing places by it, so the declaration selects no
# node.
#
# What it DOES do is the reason the guard below exists. P0 has no per-request
# selector (crates/worker/src/daemon.rs `backend_kind`), so a node naming `host`
# at all runs EVERY launch as a host process and ignores the declared image, and
# the daemon refuses to start unless WORKER_SLOTS and WORKER_SLOTS_MAX are both 1
# — #309 §2's /workspace collision, taken as option (iii). Only the first of
# those is forwardable from here (WORKER_SLOTS_MAX is the one knob no script
# passes, env.example says so), so this refuses the half it can see and names the
# half it cannot, rather than replacing a working daemon with one the supervisor
# boot-loops on `Restart=always` (systemd) or `KeepAlive` (launchd).
#
# Unset stays UNSET rather than becoming the daemon's `container` default: a node
# that declared nothing must produce the run it produced before this knob existed,
# and an explicit default would make every node advertise a value it never chose
# and turn a future change of that default into a silent no-op. Trimmed and
# whitespace-only-reads-as-unset because that is the daemon's own reading
# (crates/worker/src/config.rs `parse_modes`), and the environment file's own
# quoting is what keeps the `container, host` spelling the daemon accepts one
# value on both readers.
#
# Anything `parse_modes` rejects is refused HERE, for WORKER_KVM's reason: each
# of its rejections is a hard config error, so a replacement daemon would refuse
# to start, --restart=always would loop the refusal, and the node would leave the
# fleet. crates/worker/src/config.rs is the source of truth, and this mirrors all
# three of its rejections — an unknown name, an empty entry (`container,` splits
# to a trailing "" that no name parses) and a repeat — so the deploy fails fast
# with the live daemon untouched. The scan runs over a comma-TERMINATED copy so
# the final entry is examined like every other one; a bare split would consume
# `container,` as one entry and never see the empty one behind it.
MODES="${WORKER_MODES:-}"
MODES="${MODES#"${MODES%%[![:space:]]*}"}"
MODES="${MODES%"${MODES##*[![:space:]]}"}"
if [ -n "$MODES" ]; then
  MODES_HOST=""
  MODES_SEEN=""
  _rest="$MODES,"
  while [ -n "$_rest" ]; do
    _mode="${_rest%%,*}"
    _rest="${_rest#*,}"
    _mode="${_mode#"${_mode%%[![:space:]]*}"}"
    _mode="${_mode%"${_mode##*[![:space:]]}"}"
    case "$_mode" in
      host) MODES_HOST=1 ;;
      container) ;;
      *)
        echo "build-worker: WORKER_MODES='$MODES' names '$_mode', which is not a runtime (expected container | host) — the daemon would refuse to start on it (crates/worker/src/config.rs); REFUSING (live daemon untouched)" >&2
        exit 1
        ;;
    esac
    case "$MODES_SEEN," in
      *",$_mode,"*)
        echo "build-worker: WORKER_MODES='$MODES' lists '$_mode' more than once, which the daemon refuses as a hard config error (crates/worker/src/config.rs \`parse_modes\`); REFUSING (live daemon untouched)" >&2
        exit 1
        ;;
    esac
    MODES_SEEN="$MODES_SEEN,$_mode"
  done
  if [ -n "$MODES_HOST" ] && [ "${WORKER_SLOTS:-}" != "1" ]; then
    echo "build-worker: WORKER_MODES='$MODES' names host, which the daemon serves only at WORKER_SLOTS=1 (one host task per node — design #309 §2 option (iii)), but WORKER_SLOTS is '${WORKER_SLOTS:-<unset: daemon default 4>}' on $NODE; REFUSING daemon restart (live daemon untouched). Declare WORKER_SLOTS_$NODE=1 too — and note the daemon also demands WORKER_SLOTS_MAX=1, which NO script forwards (env.example), so a host node needs that line added to $ENV_FILE by hand." >&2
    exit 1
  fi
  spec_line WORKER_MODES "$MODES"
fi
# KVM passthrough for Android emulator work (design #367 §2.3/§3.5, daemon side
# shipped by #374). The `--device` flag is GONE with the container (design #440
# §5): the daemon's "does this node have the device" check reads its OWN view
# (crates/worker/src/daemon.rs `build_backend`), and a native daemon's own view
# IS the node's — so a node that has /dev/kvm passes, and one that does not is
# told so by the daemon rather than by a flag that could disagree with it.
#
# The value maps to a device path exactly as the daemon parses it
# (crates/worker/src/config.rs `parse_kvm_device`): a boolean turns on the
# default device node, an absolute path names another. A value that is neither is
# refused HERE, before the live daemon is replaced, rather than by a replacement
# that then cannot boot.
#
# Trimmed first, because `parse_kvm_device` trims before it matches: a ` 1 ` the
# daemon accepts must not be refused by the deploy, and a whitespace-only value
# must read as unset (the daemon's own reading) rather than as unparseable. The
# trimmed value is what lands in the environment file, so the daemon and
# worker-refresh.sh's swap both see exactly what was decided on here.
#
# All of them empty when unset ⇒ no passthrough: exactly the spec this script
# produced before Android existed. Enabling KVM on a node and granting it to a
# project stay two separate acts — WORKER_KVM_PROJECTS is fail-closed, and an
# empty one grants nobody. docs/reference/runbooks/worker-kvm.md is the procedure.
KVM_ON=""
KVM="${WORKER_KVM:-}"
KVM="${KVM#"${KVM%%[![:space:]]*}"}"
KVM="${KVM%"${KVM##*[![:space:]]}"}"
if [ -n "$KVM" ]; then
  case "$KVM" in
    0 | false | off) ;;
    1 | true | on | /*) KVM_ON=1 ;;
    *)
      echo "build-worker: WORKER_KVM='$KVM' is neither 1/0 nor an absolute device path — the daemon would refuse to start on it (crates/worker/src/config.rs); REFUSING (live daemon untouched)" >&2
      exit 1
      ;;
  esac
  spec_line WORKER_KVM "$KVM"
fi
if [ -n "${WORKER_KVM_PROJECTS:-}" ]; then
  spec_line WORKER_KVM_PROJECTS "$WORKER_KVM_PROJECTS"
fi
if [ -n "${WORKER_ANDROID_SDK_DIR:-}" ]; then
  spec_line WORKER_ANDROID_SDK_DIR "$WORKER_ANDROID_SDK_DIR"
fi
# The node's Flutter SDK (#393): a SECOND, independent toolchain leaf, mounted
# read-only at /opt/flutter for an allow-listed launch with FLUTTER_ROOT pointed
# at it. Optional by construction — unset ⇒ no mount, no env, and the run spec is
# byte-identical to what it was, so an Android-only node needs no migration and
# WORKER_ANDROID_SDK_DIR's meaning is untouched. It is NOT realised and takes no
# GC root: #373 P2's declared project toolchains supersede this stopgap.
if [ -n "${WORKER_FLUTTER_DIR:-}" ]; then
  spec_line WORKER_FLUTTER_DIR "$WORKER_FLUTTER_DIR"
fi
# The node's JDK (#397): a THIRD leaf on the same terms, mounted read-only at
# /opt/jdk with JAVA_HOME pointed at it. It exists because the nix wrappers'
# resolve-my-own-JDK trick stops at gradle, which Flutter invokes and which is
# not a wrapper (design #367 correction 14): the SDK tools ran without a JDK on
# PATH, `flutter build apk` did not. Optional exactly as the others are — unset ⇒
# no mount, no env, no migration — and superseded by #373 P2 with them.
if [ -n "${WORKER_JDK_DIR:-}" ]; then
  spec_line WORKER_JDK_DIR "$WORKER_JDK_DIR"
fi
# Per-task nix GC roots (design #373 P1, daemon side in crates/worker/src/nix.rs).
# WORKER_NIX_GCROOTS_DIR is the switch: unset ⇒ nothing below happens and the run
# spec is exactly what it was. Set ⇒ the daemon realises the node's declared
# toolchain before an allow-listed launch and holds an indirect GC root over it
# for the task's lifetime, so a weekly `nix-gc` cannot collect a store path out
# from under a running task (the mounts #374 shipped hold NO root today).
#
# The four read-only/read-write MOUNTS this used to compose are gone with the
# container (design #440 §5): a native daemon has the node's /nix, the node's
# profiles and the node's daemon socket by construction, at the same paths the
# nix daemon itself resolves them by. What survives is the PRECONDITION — a node
# without a store, a profiles tree and a daemon socket cannot serve this at all —
# and it is now checked against exactly the view the daemon will have.
#
# The roots dir is still provisioned here (#380): #372 §5 A5 declines to own it —
# the chug-node modules contribute nothing for GC roots — so this script is the
# provisioner, and a failure REFUSES the deploy with the live daemon untouched
# rather than starting a daemon that refuses to boot and is looped by the
# supervisor's own restart policy.
#
# TRUST COST, stated where an operator will see it (design #373 3b): letting the
# worker reach the nix daemon socket means anything that realises here runs in
# the process that also holds docker.sock, the NATS creds and the git key — and
# that process is now a NODE process, which is design #440 D8's whole point.
# P1 realises only what the NODE already declares (WORKER_ANDROID_SDK_DIR) — no
# project-supplied flake is evaluated, so P1 does not yet incur the unsandboxed
# project-code half of that cost. It does incur the rest: unbounded build CPU and
# disk are reachable through the socket, which is why the realise is bounded and
# the node is single-tenant (#373 Decision 1).
#
# WORKER_NIX_PROJECTS IS WHERE THAT COST BECOMES REAL (design #373 P2). Every
# owner/project listed there may have its OWN flake realised on this node, and
# flake evaluation is client-side and unsandboxed: it runs in the daemon,
# beside docker.sock, the NATS creds and the git key. GRANTING IT GRANTS
# EVALUATION, not merely a package. It is tolerable only because #373 Decision 1
# makes such a node single-tenant — the boundary crossed is platform-vs-project,
# never project-vs-project — so list one project's repos and nothing else, and
# read the flakes you list. Unset (the default) grants nobody and this node
# refuses every launch that declares runtime.env, loudly and by name.
#
# A DECLARED TOOLCHAIN MUST ALREADY BE IN THE STORE. The realise runs inside the
# launch RPC and is capped at 45s (#373 C6), while a cold Flutter/Android closure
# is priced at tens of minutes — so a job type whose environment is not already
# substituted here does not run slowly, it does not run at all. Warm it out of
# band, on the project's own clock: a scheduled job declaring the same
# runtime.env is warmed by this same pre-launch realise (#373 Decision 5), and a
# binary cache in the node's nix.conf is the operator's half.
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
  NIX_STORE_DIR="${WORKER_NIX_STORE_DIR:-/nix/store}"
  if ! ssh "$WORKER_SSH" "mkdir -p '$GCROOTS' 2>/dev/null || sudo -n mkdir -p '$GCROOTS'" < /dev/null; then
    echo "build-worker: cannot provision WORKER_NIX_GCROOTS_DIR '$GCROOTS' on $WORKER_SSH (tried mkdir -p, then sudo -n mkdir -p) — the daemon refuses to start without it and the supervisor would loop that refusal, taking the node out of the fleet; REFUSING daemon restart (live daemon untouched). Create it by hand on the node, or unset WORKER_NIX_GCROOTS_DIR to run without per-task GC roots." >&2
    exit 1
  fi
  if ! ssh "$WORKER_SSH" "[ -d '$NIX_STORE_DIR' ] && [ -d '$NIX_PROFILES_DIR' ] && [ -S '$NIX_SOCKET' ]" < /dev/null; then
    echo "build-worker: $WORKER_SSH lacks the nix preconditions for WORKER_NIX_GCROOTS_DIR (want '$NIX_STORE_DIR', '$NIX_PROFILES_DIR' and the daemon socket '$NIX_SOCKET') — REFUSING daemon restart (live daemon untouched). This node has no nix daemon; unset WORKER_NIX_GCROOTS_DIR." >&2
    exit 1
  fi
  spec_line WORKER_NIX_GCROOTS_DIR "$GCROOTS"
  if [ "$NIX_STORE_DIR" != "/nix/store" ]; then
    spec_line WORKER_NIX_STORE_DIR "$NIX_STORE_DIR"
  fi
  if [ -n "${WORKER_NIX_CLIENT:-}" ]; then
    spec_line WORKER_NIX_CLIENT "$WORKER_NIX_CLIENT"
  fi
  if [ "$NIX_SOCKET" != "/nix/var/nix/daemon-socket/socket" ]; then
    spec_line WORKER_NIX_DAEMON_SOCKET "$NIX_SOCKET"
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
    spec_line WORKER_NIX_REALISE_TIMEOUT_SECS "$WORKER_NIX_REALISE_TIMEOUT_SECS"
  fi
  if [ -n "${WORKER_NIX_PROJECTS:-}" ]; then
    NIX_FLAKE_CLIENT="${WORKER_NIX_FLAKE_CLIENT:-$NIX_PROFILES_DIR/system/sw/bin/nix}"
    if ! ssh "$WORKER_SSH" "[ -x '$NIX_FLAKE_CLIENT' ]" < /dev/null; then
      echo "build-worker: WORKER_NIX_PROJECTS grants '$WORKER_NIX_PROJECTS' project-declared toolchains, but '$NIX_FLAKE_CLIENT' is not executable on $WORKER_SSH — a flake ref is built with \`nix build\`, not \`nix-store --realise\`, so the daemon refuses to start and the supervisor would loop that refusal; REFUSING daemon restart (live daemon untouched). Point WORKER_NIX_FLAKE_CLIENT at the node's nix binary through its profiles, or unset WORKER_NIX_PROJECTS." >&2
      exit 1
    fi
    spec_line WORKER_NIX_PROJECTS "$WORKER_NIX_PROJECTS"
    if [ -n "${WORKER_NIX_FLAKE_CLIENT:-}" ]; then
      spec_line WORKER_NIX_FLAKE_CLIENT "$WORKER_NIX_FLAKE_CLIENT"
    fi
    echo "build-worker: WORKER_NIX_PROJECTS='$WORKER_NIX_PROJECTS' — these projects' OWN flakes will be evaluated in the node's daemon process, beside docker.sock and the node's credentials (design #373 3b); the node must stay single-tenant and their toolchains must already be in '$NIX_STORE_DIR' (the realise is capped at 45s)"
  fi
  # The toolchain path's SHAPE, ported rather than deleted. Design #440 §5 reads
  # this guard as a mount constraint and it is only half one: the "direct symlink
  # under a real parent" half existed because a bind resolves its source
  # host-side, and that half goes with the mount. What survives is that
  # `store_target` (crates/worker/src/nix.rs) CANONICALIZES the realise target at
  # boot and refuses anything landing outside the store — so a plain directory
  # still refuses the daemon's start, natively, and the supervisor still loops
  # that refusal. Native means MORE paths qualify (any number of symlink hops,
  # /etc/static included), so this asks exactly what the daemon asks: does it
  # resolve into the store. Gated on KVM being on because attaching the device is
  # the condition that makes a launch admitted, and therefore realised
  # (crates/worker/src/daemon.rs `realise_for_launch`).
  if [ -n "$KVM_ON" ]; then
    SDK_DIR="${WORKER_ANDROID_SDK_DIR:-/var/lib/chuggernaut/android-sdk}"
    SDK_DIR="${SDK_DIR#"${SDK_DIR%%[![:space:]]*}"}"
    SDK_DIR="${SDK_DIR%"${SDK_DIR##*[![:space:]]}"}"
    if ! ssh "$WORKER_SSH" "case \"\$(readlink -f '$SDK_DIR' 2>/dev/null)\" in '$NIX_STORE_DIR'/*) [ -e '$SDK_DIR' ] ;; *) false ;; esac" < /dev/null; then
      echo "build-worker: per-task nix GC roots are on and WORKER_KVM is on, but '$SDK_DIR' on $WORKER_SSH does not resolve to a path under '$NIX_STORE_DIR' — the daemon canonicalizes its realise target at boot (crates/worker/src/nix.rs \`store_target\`) and refuses to start when it lands outside the store; REFUSING daemon restart (live daemon untouched). Declare it as a symlink into the store — systemd.tmpfiles.rules = [ \"L+ $SDK_DIR - - - - \${pkgs.androidsdk}\" ] — or unset WORKER_NIX_GCROOTS_DIR." >&2
      exit 1
    fi
  fi
  echo "build-worker: per-task nix GC roots on — roots dir $GCROOTS present on $WORKER_SSH"
elif [ -n "${WORKER_NIX_PROJECTS:-}" ]; then
  # A grant the daemon can never act on is worse than no grant: the operator
  # believes those projects' toolchains are served, and every launch declaring
  # runtime.env is refused at the node instead.
  echo "build-worker: WORKER_NIX_PROJECTS='$WORKER_NIX_PROJECTS' grants project-declared toolchains, but WORKER_NIX_GCROOTS_DIR is unset — a realised environment with no GC root is collectable mid-task, so the daemon realises nothing and refuses every launch declaring runtime.env; REFUSING (live daemon untouched). Set WORKER_NIX_GCROOTS_DIR, or unset WORKER_NIX_PROJECTS." >&2
  exit 1
fi
# ── what gets installed, and who supervises it (design #440 D2, D6) ──────────
# On LINUX the daemon binary is EXTRACTED from the image just built rather than
# compiled on the node: that keeps its build environment byte-identical to
# today's, needs no Rust toolchain as a node machine fact, and leaves the pinned
# Dockerfile the single definition of how the binary is produced (#440 D6). The
# channel binary and the refresh script ride out of the same image, into the
# paths crates/worker/src/config.rs already defaults to.
#
# On DARWIN all three come out of the tree compiled above, for the reason stated
# where that build is: an image built for Linux holds a binary a mac cannot exec
# (#440's 2026-08-07 correction). The refresh script is the source file rather
# than the image's copy of it — the same bytes, since the image gets it by COPY.
#
# EITHER WAY THE STAGED BINARY MUST RUN ON THIS NODE, and that is asked before a
# single byte is installed. It is the generalisation of the finding, not a mac
# special case: a binary whose provenance is a foreign platform installs
# perfectly and then loops under the supervisor, and `--version` is the cheapest
# question that distinguishes the two. It is a real question on Linux too — the
# image is bookworm and a NixOS node has no /lib64 dynamic loader — which is a
# prediction this check turns into a named refusal rather than a crash loop.
#
# Every write is "unprivileged first, `sudo -n` as the fallback" — the shape the
# cache-dir provisioning above already uses. The binaries go to /usr/local on
# BOTH platforms, which is root's on both; what differs is the supervision and
# the run spec, which are root's on Linux and the login user's own tree on macOS.
#
# The environment file is 0644, not 0640. It carries paths, URLs and settings and
# no secret — it NAMES the credential file, it does not hold it — and the drift
# guard below must be able to read it back as the login user on every subsequent
# deploy. A mode that made it root-only would turn #390's guard into a guard that
# silently passes, which is the failure it exists to prevent.
if [ "$NODE_OS" = Linux ]; then
  REMOTE_STAGE="CID=\$(docker create chuggernaut/worker:$TAG)
docker cp \"\$CID:/usr/local/bin/chuggernaut\" \"\$STAGE/chuggernaut\"
docker cp \"\$CID:/usr/local/lib/chuggernaut/chuggernaut-channel\" \"\$STAGE/chuggernaut-channel\"
docker cp \"\$CID:/usr/local/lib/chuggernaut/worker-refresh.sh\" \"\$STAGE/worker-refresh.sh\"
docker rm \"\$CID\" >/dev/null"
else
  REMOTE_STAGE="install -m 0755 '$BUILD_DIR/target/release/chuggernaut' \"\$STAGE/chuggernaut\"
install -m 0755 '$BUILD_DIR/target/release/chuggernaut-channel' \"\$STAGE/chuggernaut-channel\"
install -m 0755 '$BUILD_DIR/src/deploy/prod/worker-refresh.sh' \"\$STAGE/worker-refresh.sh\""
fi
REMOTE_INSTALL="set -e
chug_dir() { mkdir -p \"\$1\" 2>/dev/null || sudo -n mkdir -p \"\$1\"; }
chug_put() { install -m \"\$1\" \"\$2\" \"\$3\" 2>/dev/null || sudo -n install -m \"\$1\" \"\$2\" \"\$3\"; }
STAGE=\$(mktemp -d)
trap 'rm -rf \"\$STAGE\"' EXIT INT TERM
$REMOTE_STAGE
if ! \"\$STAGE/chuggernaut\" --version > /dev/null 2>&1; then
  echo 'build-worker: the staged daemon binary does not run on this node (chuggernaut --version failed: a foreign architecture, a missing dynamic loader, or a broken build) — installing it would leave the supervisor restarting a binary that cannot exec, which is the crash loop the air took on 2026-08-06; REFUSING (live daemon untouched, nothing installed)' >&2
  exit 1
fi
chug_dir '$BIN_DIR'
chug_dir '$LIB_DIR'
chug_dir '$ENV_DIR'
chug_put 0755 \"\$STAGE/chuggernaut\" '$BIN_DIR/chuggernaut'
chug_put 0755 \"\$STAGE/chuggernaut-channel\" '$LIB_DIR/chuggernaut-channel'
chug_put 0755 \"\$STAGE/worker-refresh.sh\" '$LIB_DIR/worker-refresh.sh'
cat > \"\$STAGE/worker.env\" <<'CHUG_WORKER_ENV'
${SPEC_ENV}CHUG_WORKER_ENV
chug_put 0644 \"\$STAGE/worker.env\" '$ENV_FILE'"

if [ "$NODE_OS" = Linux ]; then
  # The unit is a MACHINE FACT and the environment file is the RUN SPEC — the
  # split #440 D2 answers #372 §8's R3 with. This script writes both today;
  # slice 7 hands the unit half to `nix/chug-node/`, which is why the unit
  # carries nothing an operator would ever tune per project.
  #
  # PATH is set here rather than in the run spec for the same reason: the daemon
  # shells out to git, ssh and docker, whose homes differ between a NixOS node
  # (/run/current-system/sw/bin) and a Debian one, and systemd's default PATH
  # names neither. It is also the value slice 1's launch floor carries into a
  # host task (crates/container/src/host.rs).
  NODE_PATH="${WORKER_PATH:-/run/current-system/sw/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}"
  UNIT_TEXT="[Unit]
Description=Chuggernaut worker daemon ($NODE)
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=$ENV_FILE
Environment=PATH=$NODE_PATH
ExecStart=$BIN_DIR/chuggernaut worker
User=root
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target"
  # `systemctl restart` kills the unit's cgroup and nothing else, which is
  # exactly what design #440 D3's per-task scope is for: a host task launched
  # into its own scope is not in this cgroup and survives. Job containers are
  # siblings on the docker socket and were never in it.
  REMOTE="$REMOTE_INSTALL
cat > \"\$STAGE/chug-worker.service\" <<'CHUG_WORKER_UNIT'
$UNIT_TEXT
CHUG_WORKER_UNIT
chug_dir '$UNIT_DIR'
chug_put 0644 \"\$STAGE/chug-worker.service\" '$UNIT_PATH'
sudo -n systemctl daemon-reload
sudo -n systemctl enable chug-worker.service >/dev/null
docker rm -f chug-worker >/dev/null 2>&1 || true
sudo -n systemctl restart chug-worker.service"
else
  # A GUI-domain agent, not a LaunchDaemon: CoreSimulator and the keychain are
  # per-user-session services (#322), so a system daemon would be in the wrong
  # session. It deliberately does NOT live in deploy/prod/launchd/ — that
  # directory is globbed by install-launchd.sh, which would then install a
  # worker agent on the Mini, whose colima node sits at 0 slots on purpose
  # (#440 §2).
  #
  # launchd has no EnvironmentFile, so the agent SOURCES the same file the
  # systemd unit reads. One declaration, one thing for the drift guard to read,
  # and the reason every value in it is quoted.
  #
  # PATH carries the node's TOOLCHAIN DIRECTORY ahead of the usual places,
  # because this agent's PATH is the one the daemon's own self-refresh compiles
  # under (#440's 2026-08-07 correction) and a nix-darwin profile directory or a
  # rustup home is on none of the defaults. Without it a converted mac refuses
  # every refresh — or worse, passes `command -v` on an absolute WORKER_CARGO
  # and then fails to find `rustc` mid-build.
  #
  # The log is truncated between `bootout` and `bootstrap` because launchd opens
  # StandardOutPath append-only: without it the health probe's tail spans every
  # generation the node has ever run, and a previous one's "worker up" would
  # pass a daemon that never came up. Safe there and only there — the old agent
  # is gone and the new one has not been asked to start.
  AGENT_PATH="${WORKER_PATH:-/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin}"
  case ":$AGENT_PATH:" in
    *":$CARGO_DIR:"*) ;;
    *) AGENT_PATH="$CARGO_DIR:$AGENT_PATH" ;;
  esac
  PLIST_TEXT="<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key><string>$AGENT_LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>set -a; . '$ENV_FILE'; set +a; exec '$BIN_DIR/chuggernaut' worker</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key><string>$NODE_HOME</string>
    <key>PATH</key><string>$AGENT_PATH</string>
  </dict>
  <key>StandardOutPath</key><string>$WORKER_LOG_PATH</string>
  <key>StandardErrorPath</key><string>$WORKER_LOG_PATH</string>
</dict>
</plist>"
  REMOTE="$REMOTE_INSTALL
cat > \"\$STAGE/$AGENT_LABEL.plist\" <<'CHUG_WORKER_PLIST'
$PLIST_TEXT
CHUG_WORKER_PLIST
plutil -lint \"\$STAGE/$AGENT_LABEL.plist\" >/dev/null
mkdir -p '$NODE_HOME/Library/LaunchAgents' '${WORKER_LOG_PATH%/*}'
install -m 0644 \"\$STAGE/$AGENT_LABEL.plist\" '$UNIT_PATH'
launchctl bootout gui/\$(id -u)/$AGENT_LABEL 2>/dev/null || true
: > '$WORKER_LOG_PATH'
docker rm -f chug-worker >/dev/null 2>&1 || true
launchctl bootstrap gui/\$(id -u) '$UNIT_PATH'"
fi

# A unit path that cannot be written is a node with no daemon at all, and on
# NixOS /etc/systemd/system is a read-only symlink into the store — which is
# precisely the state design #440 slice 7 exists for. Refuse here, with the live
# daemon still running, rather than half-way through the install.
if [ "$NODE_OS" = Linux ] && ! ssh "$WORKER_SSH" "command -v systemctl >/dev/null && [ -d '$UNIT_DIR' ] && { [ -w '$UNIT_DIR' ] || sudo -n test -w '$UNIT_DIR'; }" < /dev/null; then
  echo "build-worker: $WORKER_SSH has no usable systemd unit directory at '$UNIT_DIR' (want \`systemctl\` on PATH and a directory writable by the login user or by \`sudo -n\`) — on NixOS that path is a read-only symlink into the store, where the unit is the node configuration's to declare (design #440 slice 7); REFUSING daemon restart (live daemon untouched). Point WORKER_UNIT_DIR_$NODE at a writable unit path, or declare the unit on the node itself." >&2
  exit 1
fi

# The same courtesy on macOS, for the paths that are NOT in the login user's
# tree. deploy/prod/install-launchd.sh — the precedent design #440 D2 names —
# writes only under $HOME, so the binaries are the first thing on this platform
# to need /usr/local, and on an Apple-Silicon mac (which the plist's own
# /opt/homebrew PATH assumes) /usr/local is root-owned and often absent
# altogether. Without this the operator gets a bare `sudo: a password is
# required` from inside the install, where the Linux side gets a named remedy.
# Asked of the nearest EXISTING ancestor because the install creates what is
# missing, and asked without creating anything, so a refusal changes no node.
if [ "$NODE_OS" = Darwin ] && ! ssh "$WORKER_SSH" "for d in '$BIN_DIR' '$LIB_DIR'; do
  p=\$d
  while [ ! -d \"\$p\" ]; do p=\$(dirname \"\$p\"); done
  [ -w \"\$p\" ] || sudo -n test -w \"\$p\" || exit 1
done" < /dev/null; then
  echo "build-worker: $WORKER_SSH cannot install the daemon binaries into '$BIN_DIR' and '$LIB_DIR' (neither the login user nor \`sudo -n\` can write the nearest existing parent) — on a stock mac /usr/local is root-owned and \`sudo\` wants a password, which a non-interactive deploy cannot give; REFUSING daemon restart (live daemon untouched). Create them once on the node and hand them to the login user — \`sudo mkdir -p $BIN_DIR $LIB_DIR && sudo chown \$(id -un) $BIN_DIR $LIB_DIR\` — or grant that user passwordless sudo." >&2
  exit 1
fi

# ── run-spec drift (ticket #390, design #440 D7) ─────────────────────────────
# What the node is RUNNING is not a declaration. Everything above composes the
# run spec from the environment and then overwrites the node's environment file
# with it — so any setting the LIVE daemon carries that this composition does not
# is DROPPED here, silently, and the node comes back degraded in a way nothing
# reports: caching off (#55), the boot capacity back at the daemon's default, or
# a node that keeps serving jobs and quietly stops updating. That is #265 reason
# 3, and this is where it is catchable.
#
# The guard keeps its meaning and gains reach (#440 D7): the live side is the
# node's own environment file, which an operator can read without `docker
# inspect`. A node that has never been converted has no such file, so the
# container's environment is read instead — the CONVERSION is exactly the
# recreate this guard exists to police, and a guard that went blind at it would
# be worse than no guard.
#
# WHICH IS WHY THE READ IS TRI-STATE, not "did anything come back". "The file is
# absent" and "the file is there and I could not read it" produce the same empty
# output, and collapsing them is how a guard degrades to a pass: an unreadable
# file falls through to a `docker inspect` that a converted node also answers
# emptily, and the run then prints "no live worker" — the fresh-node line — on a
# node whose whole run spec is about to be overwritten unchecked. So the node
# ANSWERS which case it is, and "cannot read" REFUSES with the live daemon
# untouched. A guard that cannot see the declaration is not a guard that passes.
#
# The comparison is against $SPEC_ENV, not against the shell's variables, because
# only $SPEC_ENV knows what will actually be written: a knob this script does not
# forward (WORKER_SLOTS_MAX is the documented one) is dropped no matter what
# chuggernaut.env says about it, and a check that read the env would call that
# clean. Presence decides the refusal; the VALUE comparison is informational
# only, so a quoted value this cannot parse costs a noisy line and never a
# wrong verdict.
#
# A drop REFUSES, with the live daemon still running, in the same spirit as the
# label and capacity guards above — removing a setting on purpose is a real
# thing to want, so WORKER_SPEC_DROP_OK=1 is the way to say so out loud.
build_worker_run_spec_drift() {
  # stdin from /dev/null: these ssh calls read nothing, and update.sh runs this
  # whole script over an ssh session whose stdin it must not swallow.
  ssh "$WORKER_SSH" "if [ -e '$ENV_FILE' ]; then
  cat '$ENV_FILE' 2>/dev/null || sudo -n cat '$ENV_FILE' 2>/dev/null || echo CHUG_SPEC_UNREADABLE
else
  echo CHUG_SPEC_ABSENT
fi" > "$SPEC" 2>/dev/null < /dev/null || true
  if grep -qxF CHUG_SPEC_UNREADABLE "$SPEC"; then
    echo "build-worker: '$ENV_FILE' exists on $WORKER_SSH but neither the login user nor \`sudo -n\` can read it — the run-spec drift guard (#390, design #440 D7) compares the live daemon's environment against the one composed here, and it cannot see the live side; a guard that cannot read the declaration is not a guard that passes. REFUSING daemon restart (live daemon untouched). Make it readable (\`sudo chmod 0644 $ENV_FILE\` — it carries no secret, only the PATH of the credential), or grant the login user passwordless sudo on this node." >&2
    return 1
  fi
  _live_from="the environment file $ENV_FILE"
  if grep -qxF CHUG_SPEC_ABSENT "$SPEC" || [ ! -s "$SPEC" ]; then
    ssh "$WORKER_SSH" \
      "docker inspect chug-worker --format '{{range .Config.Env}}{{println .}}{{end}}'" \
      > "$SPEC" 2>/dev/null < /dev/null || true
    _live_from="the live chug-worker CONTAINER (this run converts $NODE to a native daemon)"
  fi
  if [ ! -s "$SPEC" ]; then
    echo "build-worker: no live worker on $WORKER_SSH to compare against — this run declares $NODE's whole run spec"
    return 0
  fi
  echo "build-worker: run-spec drift checked against $_live_from"
  _dropped=""
  while IFS= read -r _line; do
    case "$_line" in
      WORKER_*=*) ;;
      *) continue ;;
    esac
    _key="${_line%%=*}"
    _live="${_line#*=}"
    # The environment file quotes every value; the container's environment did
    # not. Unquoting keeps a converted node's report free of a spurious
    # "live '2' -> declared '2'" on every run.
    case "$_live" in
      "'"*"'") _live="${_live#\'}" ; _live="${_live%\'}" ;;
    esac
    case "$_key" in
      *[!A-Za-z0-9_]*) continue ;;
    esac
    _passed=""
    case "
$SPEC_ENV" in
      *"
$_key="*) _passed=1 ;;
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
    # The keys this script RESOLVES rather than reads straight out of the
    # environment. Without them a re-deploy reports a change on every run —
    # "live 'x' -> declared ''" — for a value that is not changing at all.
    case "$_key" in
      WORKER_NODE) _declared="$NODE" ;;
      WORKER_GIT_KEY) _declared="$GIT_KEY" ;;
      WORKER_DOCKER_ENDPOINT) _declared="$DOCKER_ENDPOINT" ;;
      WORKER_CARGO) _declared="$NODE_CARGO" ;;
      WORKER_BUILD_DIR) _declared="${BUILD_DIR:-}" ;;
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

echo "build-worker: run spec for $NODE: WORKER_SLOTS=${WORKER_SLOTS:-<unset: daemon default 4>} WORKER_CACHE_DIR=${WORKER_CACHE_DIR:-<unset: caching OFF>} WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-<unset: cannot self-refresh>} WORKER_GIT_KEY=$GIT_KEY WORKER_DOCKER_ENDPOINT=${DOCKER_ENDPOINT:-<unset: daemon default $DOCKER_ENDPOINT_DEFAULT>}"
echo "build-worker: supervising $NODE natively on $NODE_OS — $UNIT_PATH over $ENV_FILE (design #440 D2)"
ssh "$WORKER_SSH" "$REMOTE" < /dev/null

# Positively PROVE the daemon actually came up before we claim "deployed". The
# supervisor's own exit code only says it accepted the job, not that the process
# stayed up. A direct NATS ping from this laptop path is impractical (the
# dispatcher's NATS is not generally reachable here), so the probe demands the
# daemon's OWN proof of NATS liveness: the "worker up" log line, which the
# daemon emits only AFTER its NATS connection and worker-RPC subscription
# succeed (daemon.rs) — the ping RPC is serving once that line exists. This is
# strictly stronger than running + any-log-line (#207 review: a crash-looping
# daemon can log plenty without ever reaching NATS). A timeout is a LOUD failure
# with a non-zero exit — never a silent "deployed".
#
# The log is the supervisor's now: journald on Linux (read as root when the
# login user is not in the journal group), the agent's StandardOutPath on macOS.
#
# BOTH READS ARE BOUNDED TO *THIS* START, and that bound is the whole of #207's
# guarantee under a supervisor. `docker logs` on a container the run had just
# created could only ever show that container's output; a unit's journal and an
# agent's append-only log both span every generation the node has ever run. A
# crash-looping daemon under `Restart=always`/`KeepAlive` is `active (running)`
# on most polls, so an unbounded tail would find the PREVIOUS generation's
# "worker up" on a quiet node and report HEALTHY over a daemon that never reached
# NATS — the silent "deployed" this block exists to make impossible.
#
# Linux binds by InvocationID, which systemd mints fresh per start: exact, and
# immune to clock skew in a way `--since` is not. A systemd too old to report one
# yields no verdict rather than a false pass, so the probe times out loudly.
# macOS has no such handle, so the install TRUNCATES the agent's log between
# `bootout` and `bootstrap` (above) and the tail can only see the new agent.
if [ "$NODE_OS" = Linux ]; then
  PROBE_REMOTE='systemctl is-active --quiet chug-worker.service || exit 0
INV=$(systemctl show -p InvocationID --value chug-worker.service 2>/dev/null)
[ -n "$INV" ] || exit 0
{ journalctl _SYSTEMD_INVOCATION_ID="$INV" -n 50 --no-pager 2>/dev/null || sudo -n journalctl _SYSTEMD_INVOCATION_ID="$INV" -n 50 --no-pager; } 2>&1 | grep -q "worker up" && echo HEALTHY'
else
  PROBE_REMOTE="launchctl print gui/\$(id -u)/$AGENT_LABEL >/dev/null 2>&1 && tail -n 50 '$WORKER_LOG_PATH' 2>&1 | grep -q 'worker up' && echo HEALTHY"
fi
probe_deadline=$(( $(date +%s) + PROBE_TIMEOUT_SECS ))
probe_attempt=0
until ssh "$WORKER_SSH" "$PROBE_REMOTE" < /dev/null 2>/dev/null | grep -q HEALTHY; do
  probe_attempt=$((probe_attempt + 1))
  if [ "$(date +%s)" -ge "$probe_deadline" ]; then
    echo "build-worker: chug-worker did NOT report healthy within ${PROBE_TIMEOUT_SECS}s on $WORKER_SSH (supervised + a "worker up" NATS-subscribed log line) — FAILED; the daemon is not confirmed up" >&2
    exit 1
  fi
  echo "build-worker: waiting for chug-worker to report healthy (attempt $probe_attempt) — retrying in ${PROBE_INTERVAL_SECS}s"
  sleep "$PROBE_INTERVAL_SECS"
done
echo "build-worker: verified chug-worker is running and NATS-subscribed (worker up) on $WORKER_SSH"

# What this node's own self-refresh will do, said HERE — at the moment it is
# converted — because the conversion is what switches it. Since design #440
# slice 6 the swap installs a binary and asks the supervisor to restart; the
# node updates itself again, and the record of a replacement that will not start
# is the supervisor's log rather than a retained sibling container. The converse
# is the note this line used to carry: a node NOBODY has converted refuses its
# own swap, so it is deployed with this script until it is.
#
# WHERE that binary comes from is the claim #440 D6 got wrong for one platform,
# so it is said per-platform: this is the only place the operator is told, and
# telling a mac's operator it extracts from the image is the exact wrong model
# of what their node does on the next deploy.
if [ "$NODE_OS" = Darwin ]; then
  REFRESH_WHENCE="compiles its own daemon binary with $NODE_CARGO in $BUILD_DIR and installs that (the worker image is Linux; design #440 D6 is Linux-only, corrected 2026-08-07)"
else
  REFRESH_WHENCE="installs the daemon binary out of the worker image (design #440 D6)"
fi
echo "build-worker: NOTE: $NODE is supervised NATIVELY now, so its self-refresh $REFRESH_WHENCE and restarts $UNIT_PATH — read what follows a swap in the supervisor's own log, not in a swapper container. A node this script has NOT converted refuses its swap and is deployed over ssh with this script." >&2

# Bound the node's docker disk (the 2026-07-23 air incident: 27G of BuildKit
# cache + dangling image generations filled the colima partition and an image
# build died ENOSPC mid-deploy). Each rebuild strands the previous image
# generation as dangling — prune those (NEVER -a: tagged agent images must
# survive, the #183 lesson) and cap the BuildKit cache at 15G, which keeps the
# hot cargo/sccache cache-mounts (#115) while shedding stale layers.
ssh "$WORKER_SSH" "docker image prune -f >/dev/null; docker builder prune -f --keep-storage 15GB >/dev/null 2>&1 || true" < /dev/null

echo "build-worker: chuggernaut/{worker,agent,agent-rust}:$TAG deployed + VERIFIED on $WORKER_SSH ($SHA) — image label matches and chug-worker is up"
