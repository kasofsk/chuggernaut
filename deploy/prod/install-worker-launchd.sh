#!/bin/sh
# Install THIS mac's worker daemon as a launchd agent in the login user's GUI
# domain (design #440 D2) — the macOS half of slice 7, and the counterpart of
# the systemd unit nix/chug-node/ declares on NixOS.
#
#   install-worker-launchd.sh            install / reload the worker agent
#   install-worker-launchd.sh uninstall  bootout the agent and remove the plist
#
# OPT-IN, three times over, because the Mini is a control-plane host whose own
# colima node sits at 0 slots on purpose and a worker agent there would starve
# it (deploy/prod/README.md §6, design #440 §2):
#
#   1. The template is in ./launchd-worker/, which install-launchd.sh's
#      `launchd/*.plist.template` glob cannot reach (job #467's finding).
#   2. Nothing calls this script — not install-launchd.sh, not boot.sh, not
#      update.sh, not any job type. An operator types its name.
#   3. It REFUSES on a mac that runs the dispatcher or api agent, naming
#      CHUG_WORKER_ON_CONTROL_PLANE=1 as the deliberate override.
#
# It installs the LIFECYCLE only. The run spec is the platform's, in the
# environment file this agent sources — deploy/prod/build-worker.sh renders it
# and #390's drift guard compares it — so a missing one is refused here rather
# than boot-looped under KeepAlive.
#
# It also removes the containerized `chug-worker` if one is still there, for the
# same reason build-worker.sh does: the agent it bootstraps claims the same
# `WORKER_NODE`, and two daemons on one node name is what worker-refresh.sh
# refuses its swap over (#440 §1). A docker it cannot ask is a REFUSAL rather
# than a shrug, like every other precondition here — CHUG_WORKER_SKIP_DOCKER_CHECK=1
# is the operator's way to say this mac has never run one.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
LABEL=com.chuggernaut.worker
TEMPLATE="$HERE/launchd-worker/$LABEL.plist.template"
LA="$HOME/Library/LaunchAgents"
PLIST="$LA/$LABEL.plist"
DOMAIN="gui/$(id -u)"

# The paths and the PATH build-worker.sh gives a macOS node, spelled the same
# way so the two renderings stay one shape (nix/chug-node/chug-worker-unit.test.sh
# pins the Linux pair; deploy/prod/install-worker-launchd.test.sh pins this one).
#
# The login user's `~/.local/bin` is on that PATH because the agent CLI a host
# AGENT task execs is resolved as a bare `claude` on the DAEMON's own PATH
# (design #490 D3) — a host task has no image to carry one — and that directory
# is where the CLI's own installer puts it. Measured on gumbo-air-0 (#490 M3):
# `claude` at /Users/worksalot/.local/bin/claude, resolvable on the login PATH
# and on none of the entries below it, so without this the node discovers no CLI
# at boot and refuses every agent host launch by name. It rides LAST because it
# is user-writable: this PATH is also every host task's, and a directory ahead of
# /usr/bin would silently reselect `git` or `ssh` for work that never asked.
ENV_FILE="${WORKER_ENV_FILE:-$HOME/chuggernaut-worker/worker.env}"
BINARY="${WORKER_BINARY:-/usr/local/bin/chuggernaut}"
LOG="$HOME/Library/Logs/chuggernaut/worker.log"
AGENT_PATH="${WORKER_PATH:-/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.local/bin}"

if [ "$(uname -s)" != Darwin ]; then
  echo "install-worker-launchd: this host is not Darwin — a launchd agent supervises the daemon on macOS only, and on Linux the unit is nix/chug-node/'s or build-worker.sh's (design #440 D2); REFUSING" >&2
  exit 1
fi

if [ "${1:-}" = uninstall ]; then
  launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  echo "removed $LABEL (the environment file, the binary and the keys are left alone)"
  exit 0
fi

# The control-plane refusal. Both halves are asked because they fail
# differently: the plist is durable evidence that this mac was bootstrapped as a
# control plane, and `launchctl print` is what is true right now.
if [ -z "${CHUG_WORKER_ON_CONTROL_PLANE:-}" ]; then
  for peer in com.chuggernaut.dispatcher com.chuggernaut.api; do
    if [ -f "$LA/$peer.plist" ] || launchctl print "$DOMAIN/$peer" > /dev/null 2>&1; then
      echo "install-worker-launchd: this mac runs $peer, so it is a control-plane host — its own node is registered at 0 slots precisely so heavy builds cannot starve the dispatcher (deploy/prod/README.md §6); REFUSING. Run this on a worker mac, or set CHUG_WORKER_ON_CONTROL_PLANE=1 to say you mean it." >&2
      exit 1
    fi
  done
fi

if [ ! -r "$ENV_FILE" ]; then
  echo "install-worker-launchd: no readable environment file at '$ENV_FILE' — it carries the WHOLE run spec (WORKER_NODE, NATS_URL, NATS_CREDS, the slot count), the agent sources it before exec'ing the daemon, and without it the daemon fails to start and KeepAlive loops that failure; REFUSING (nothing installed). Render it with deploy/prod/build-worker.sh, or point WORKER_ENV_FILE at the one this node already has." >&2
  exit 1
fi

if [ ! -x "$BINARY" ]; then
  echo "install-worker-launchd: no executable daemon at '$BINARY' — the deploy puts it there (design #440 D6: compiled on a mac, extracted from the worker image on Linux), it is not built here; REFUSING (nothing installed). Run deploy/prod/build-worker.sh against this node, or point WORKER_BINARY at the binary it installed." >&2
  exit 1
fi

# Executable is not the same question as runnable, and on THIS platform the
# difference is the whole finding: the worker image is a Linux container, so a
# binary extracted from it is `ELF … ARM aarch64` — 0755, present, and answered
# by launchd with `cannot execute binary file` on repeat under KeepAlive (the
# air, 2026-08-06; #440's 2026-08-07 correction). Ask the binary itself, before
# an agent is written that would loop on it.
if ! "$BINARY" --version > /dev/null 2>&1; then
  echo "install-worker-launchd: the daemon at '$BINARY' does not run on this mac ('$BINARY --version' failed) — a binary extracted from the LINUX worker image installs perfectly here and then loops under KeepAlive with 'cannot execute binary file' (design #440 D6 holds on Linux only); REFUSING (nothing installed). Re-run deploy/prod/build-worker.sh against this node: on Darwin it compiles the daemon with the node's own cargo (WORKER_CARGO)." >&2
  exit 1
fi

# The node this converts may still be supervising the CONTAINERIZED daemon under
# `--restart=always`, and nothing above excludes it: a hand-placed binary and
# environment file are exactly what an operator puts on a mac the deploy never
# reaches. That question is asked HERE, before anything is written, because the
# answer can be "I cannot tell" — a stopped `--restart=always` container is
# invisible to a docker that is absent or whose daemon is down, and it comes back
# the moment dockerd does. build-worker.sh can afford `|| true` at its own
# removal because it has already driven docker on that node in the same run;
# nothing here has.
if [ -z "${CHUG_WORKER_SKIP_DOCKER_CHECK:-}" ] && ! docker info > /dev/null 2>&1; then
  if command -v docker > /dev/null 2>&1; then
    WHY="docker is installed here but its daemon did not answer — colima may be down"
  else
    WHY="there is no docker on this PATH to ask"
  fi
  echo "install-worker-launchd: cannot tell whether this node still runs the containerized chug-worker — $WHY. A stopped '--restart=always' container comes back when dockerd does, and two daemons on one WORKER_NODE is the state worker-refresh.sh refuses its swap over (#440 §1); REFUSING (nothing installed). Start docker and re-run, or run 'docker rm -f chug-worker' yourself, or set CHUG_WORKER_SKIP_DOCKER_CHECK=1 to say this mac has never run one." >&2
  exit 1
fi

# The toolchain directory rides ahead of the rest, out of the run spec this
# agent is about to source. A converted mac's SELF-refresh compiles under
# exactly this PATH (#440's 2026-08-07 correction), and cargo resolves `rustc`
# through it — so an agent without the directory leaves the node one that
# refuses every refresh, whatever WORKER_CARGO says.
# Read with the shell's own parameter expansion rather than sed or awk: a run
# spec is one `NAME='value'` per line by construction (build-worker.sh's
# `spec_line`, which refuses a value carrying a quote of its own), so no parser
# is warranted for it.
SPEC_CARGO=""
while IFS= read -r _line; do
  case "$_line" in
    WORKER_CARGO=*)
      SPEC_CARGO="${_line#WORKER_CARGO=}"
      SPEC_CARGO="${SPEC_CARGO#\'}"
      SPEC_CARGO="${SPEC_CARGO%\'}"
      ;;
  esac
done < "$ENV_FILE"
if [ -n "$SPEC_CARGO" ]; then
  SPEC_CARGO_DIR="$(dirname "$SPEC_CARGO")"
  case ":$AGENT_PATH:" in
    *":$SPEC_CARGO_DIR:"*) ;;
    *) AGENT_PATH="$SPEC_CARGO_DIR:$AGENT_PATH" ;;
  esac
fi

mkdir -p "$LA" "$(dirname "$LOG")"
sed -e "s|@ENV_FILE@|$ENV_FILE|g" \
  -e "s|@BINARY@|$BINARY|g" \
  -e "s|@HOME@|$HOME|g" \
  -e "s|@PATH@|$AGENT_PATH|g" \
  -e "s|@LOG@|$LOG|g" \
  "$TEMPLATE" > "$PLIST"
plutil -lint "$PLIST" > /dev/null

launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true

# Removed in the same breath as the bootstrap, which is what `build-worker.sh`
# does at its own, and announced rather than silent because here it is the
# operator's node and not the deploy's. `docker inspect` is what answers
# "is there one", not `rm`'s exit status: under `--force` the CLI reports a
# missing container and still exits 0, so removing unconditionally would announce
# a removal on every node that never had one.
if [ -z "${CHUG_WORKER_SKIP_DOCKER_CHECK:-}" ] && docker inspect chug-worker > /dev/null 2>&1; then
  docker rm -f chug-worker > /dev/null
  echo "removed the containerized chug-worker — the native agent replaces it, and two daemons on one WORKER_NODE is the state #440 §1 names"
fi

launchctl bootstrap "$DOMAIN" "$PLIST"
echo "installed $LABEL — 'launchctl print $DOMAIN/$LABEL' for status, log in $LOG"
