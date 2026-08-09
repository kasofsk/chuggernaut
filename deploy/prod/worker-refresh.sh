#!/bin/sh
# Worker self-refresh (spec §3.1) — runs ON the worker node, invoked by the
# `chuggernaut worker` daemon when a `refresh` RPC arrives. The dispatcher host
# cannot ssh a tagged worker (Tailscale blocks tagged->tagged), so control is
# inverted: the daemon fetches the build context itself over the existing ssh
# front and rebuilds its three node images, then swaps itself.
#
# Two phases, so the daemon can quiesce launches only for the brief swap window:
#   worker-refresh.sh build <sha> <tag>   # fetch context + build 3 images
#   worker-refresh.sh swap  <tag>         # install the new binary + restart the unit
#
# The daemon is NOT a container (design #440 D1/D2/D6): it is a binary under a
# systemd unit or a launchd agent, over an environment file build-worker.sh
# renders. So the swap installs and restarts, and every mount, device and
# `docker inspect` carry-forward it used to compose is gone — see the swap arm.
#
# All external tools (git, ssh, docker, the supervisor) come from PATH so the
# shell test can fake them (worker-refresh.test.sh). Config is env-driven,
# inherited from the daemon's own environment, which the supervisor loads from
# that environment file — so a value survives a refresh because it is WRITTEN
# DOWN, not because this script copies it forward. The run spec is declared in
# deploy/prod/chuggernaut.env on the dispatcher host and applied by
# build-worker.sh (deploy/prod/README.md §6). Each phase reports the spec it is
# running so a node the Mini cannot ssh still states its own configuration into
# the deploy's task output:
#   WORKER_REFRESH_GIT_URL  ssh://git@<ssh-front>:2222/<owner>/<repo>.git (required)
#   WORKER_GIT_KEY          ssh private key for the node credential
#   WORKER_NODE, WORKER_SLOTS, WORKER_CACHE_DIR   reported, never re-applied
#   WORKER_REFRESH_DISK_FREE_GB_MIN / _DISK_PATH   disk pre-flight (see below)
#   WORKER_REFRESH_DISK_TRIM_TIMEOUT_SECS   how long the post-prune colima trim
#     may run on a mac before it is abandoned (see below); Darwin only
#   WORKER_DAEMON_BIN / WORKER_CHANNEL_BINARY / WORKER_HOST_CHANNEL_BINARY /
#   WORKER_REFRESH_SCRIPT
#     where the swap installs the artifacts it stages — three on a
#     container-only node, four when WORKER_MODES names `host`; the defaults are
#     the paths build-worker.sh installs to and the ones
#     crates/worker/src/config.rs already defaults to. The first three come out
#     of the new worker image (the daemon excepted on Darwin, below). The HOST
#     channel copy is the odd one: the node execs it ITSELF, so it comes out of
#     the image on Linux and out of the node's OWN native build on Darwin, where
#     the image's Linux ELF is not something the mac can start (design #490 D2)
#   WORKER_UNIT / WORKER_AGENT_LABEL   what the swap asks the supervisor to restart
#   WORKER_CARGO / WORKER_BUILD_DIR   DARWIN ONLY: the node's Rust toolchain and
#     the tree + target dir it compiles the daemon in, because the worker image
#     is a Linux container and a mac cannot run what comes out of it (#440 D6,
#     corrected 2026-08-07). build-worker.sh writes both at conversion
#   WORKER_SWAP_CONTAINER_MARKER   the file that says "this daemon is itself a
#     container", i.e. a node nobody has converted yet (default /.dockerenv)
set -eu

PHASE="${1:?usage: worker-refresh.sh build <sha> <tag> | swap <tag>}"

# ── progress markers (ticket #253) ───────────────────────────────────────────
# A refresh rebuilds three images and the agent-rust leg alone can run 10+
# minutes; before these markers the whole window was silent from the deploy
# job's side and the only diagnosis was ssh + `docker logs chug-worker`. Each
# marker names WHERE the refresh is; the daemon reads them off this script's
# stdout (`REFRESH_PHASE_MARKER` in crates/worker/src/daemon.rs), reports the
# current one in `ping`, and the deploy's wait loop relays it — so the deploy
# job's task output carries per-phase progress with elapsed time.
#
# The marker prefix is a CONTRACT with the daemon: keep the two in step, and
# emit a marker before every step that can take minutes. Bare `echo` per line
# keeps the stream line-at-a-time — never batch progress behind a build.
refresh_phase() {
  echo "worker-refresh: phase $1"
}

# ── the runtimes this node offers (design #309 §1) ───────────────────────────
# Read off WORKER_MODES, which build-worker.sh wrote into the environment file
# the supervisor hands this daemon — so this script and the daemon that invoked
# it answer the question from the same declaration. The rule is the daemon's own
# (`serves_container` in crates/worker/src/daemon.rs): a node names containers if
# it says `container`, or if it says nothing at all, which is every node in the
# fleet today. Written once here because a second spelling of it somewhere below
# could disagree with the daemon this script is about to replace.
#
# What follows from it: a host-only node launches no image, so it builds no agent
# image, and it is handed no INJECTED chuggernaut-channel binary — that file is
# read by an agent CONTAINER (`Core::channel_mcp`, whose two callers are both
# agent-shaped) and this node creates none.
#
# HOST capability is the second question and is asked separately, in the daemon's
# other spelling (`serves_host`: names `host`). Design #490 D2 gives a
# host-capable node its own chuggernaut-channel at its own path, executed by THIS
# NODE rather than injected, so a dual-mode mac wants both files and the two
# answers must not be collapsed into one.
NODE_MODES="$(printf '%s' "${WORKER_MODES:-}" | tr -d '[:space:]')"
SERVES_CONTAINER=1
SERVES_HOST=""
case ",$NODE_MODES," in
  *,container,*) ;;
  *,host,*) SERVES_CONTAINER="" ;;
esac
case ",$NODE_MODES," in
  *,host,*) SERVES_HOST=1 ;;
esac

# Whether a refresh here builds a container image AT ALL, which is not the same
# question. A host-only DARWIN node builds none: it compiles its daemon natively
# (#440's 2026-08-07 correction) and the worker image's only other passenger is
# the channel binary it does not take. A host-only LINUX node still builds the
# worker image, because #440 D6 holds there and that image is the only place its
# daemon binary comes from — so docker is still a machine fact on Linux.
refresh_builds_images() {
  [ -n "$SERVES_CONTAINER" ] || [ "$(uname -s)" != Darwin ]
}

# ── the run spec this node is actually running (ticket #390) ─────────────────
# This script inherits its config from the daemon's own environment, which is
# the only mechanism available to it: the dispatcher host cannot ssh a tagged
# worker, so nothing here can read chuggernaut.env on the Mini. Since design
# #440 D6 that environment comes from the node's own environment file rather
# than from a container recreated with a dozen `-e` flags, so a value no longer
# survives by circulating from one generation of the daemon to the next — the
# #265 reason 3 shape. What is unchanged is that the node is the only thing that
# can say what it is running, and this report is where it says it.
#
# So the node reports what it is running, every refresh, on the daemon's stdout
# — which the daemon relays line by line into the deploy's task output. That is
# the drift report for a node the Mini cannot reach: an operator reads the
# node's real spec off the deploy leg and compares it with what
# deploy/prod/chuggernaut.env declares, with no ssh and no UI.
#
# The two that fail SILENTLY get their own line, because their absence looks
# exactly like a working node: caching off is a slow node (#55), and a boot
# capacity back at the daemon's default is an over-committed one.
refresh_run_spec_report() {
  echo "worker-refresh: run spec on ${WORKER_NODE:-?} ($1): WORKER_SLOTS=${WORKER_SLOTS:-<unset>} WORKER_CACHE_DIR=${WORKER_CACHE_DIR:-<unset>} WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-<unset>} WORKER_GIT_KEY=${WORKER_GIT_KEY:-/data/keys/worker_git}"
  if [ -z "${WORKER_CACHE_DIR:-}" ]; then
    echo "worker-refresh: WARNING: WORKER_CACHE_DIR is unset — this node builds with sccache OFF; if that is not deliberate the value was dropped at some daemon (re)creation and only build-worker.sh can put it back"
  fi
  if [ -z "${WORKER_SLOTS:-}" ]; then
    echo "worker-refresh: WARNING: WORKER_SLOTS is unset — this node boots at the daemon's default of 4 until the dispatcher reconciles its recorded intent"
  fi
}

# ── docker disk hygiene (deploy #248) ────────────────────────────────────────
# A refresh needs headroom for a WHOLE new image generation ON TOP of the one
# the node is still running, plus the BuildKit cache it grows while building.
# When that headroom is missing the build dies ~10 minutes in AND strands the
# partial generation, so — because the script used to prune only after a
# SUCCESSFUL refresh — every failure made the next attempt more likely to fail
# (deploy #248: images 37 -> 45.6GB, cache to 30.6GB, recovered by hand with an
# ssh + prune + a colima disk resize). Both halves of that loop are closed
# below: refuse up front when the space cannot be there, and prune on the
# FAILED-build path too, not only the successful one.
#
# The threshold is a CONSTANT derived from a measured refresh, re-derived for
# deploy #347 (the previous 20GB waved through an attempt with ~7GB of real
# headroom, which then died with ENOSPC unpacking the agent-rust seed and
# stranded prod on the old SHA for 40 minutes). The derivation, sampling free
# space every 15s through a real refresh on dev-air:
#   * a refresh consumed ~41GB peak against a 13.3GB agent-rust image, which is
#     ~2x the image (docker holds it TWICE while exporting — the content blob
#     plus the unpacked overlay snapshot — on top of the live generation the
#     node keeps running) plus ~14GB of BuildKit growth, dominated by the #115
#     /build-target cache mount that holds the FULL unpruned target dir;
#   * #347 pruned the baked seed's linked executables, taking agent-rust to
#     ~4.4GB, so the image half of that drops to ~9GB while the BuildKit half
#     does not move (the cache mount still holds the fat pre-prune target);
#   * ~23GB expected peak, rounded up to 30 for the volatility below.
#
# That 30 was then measured TOO LOW: deploy #351 sampled air at ~32GB consumed
# (68.8GB -> 36.8GB minimum), so a node sitting at exactly 30GB free would have
# passed this pre-flight and then run out. #352 removed the cause rather than
# raising the number — the agent-rust warm-target seed and, with it, the
# /build-target cache mount that held the full unpruned debug target and drove
# both halves of that peak. What is left to hold twice is a ~3.7GB (air) /
# ~2.25GB (nuc) agent-rust image instead of a 5.86GB / 4.51GB one, and the only
# surviving target cache mount is the worker image's release build.
#
# 30 is therefore kept as-is: it was tight against the old peak and is slack
# against the new one, and lowering it on a projection is how #347 got burned.
# The next refresh prints its own consumption (`disk: … consumed …GB` below, from
# the same free-space samples this pre-flight reads) — re-derive from THAT
# number, not from this paragraph.
# Free space at refresh time is NOT stable: it swung 47.8GB -> 72GB across two
# runs an hour apart on the same node, because job-container overlays (agents
# running cargo in their own containers) and the BuildKit cache both move it
# under us. This constant is a FLOOR checked once before the build, not a
# guarantee that the space is still there ten minutes later.
#
# Revisit it if the image sizes change again. Env-overridable so a node with a
# different disk shape can tune it: build-worker.sh writes the knob into the
# node's environment file, which the supervisor hands the daemon on every start,
# so an override survives a self-refresh because it is declared — the swap
# carries nothing forward (design #440 D6).
DISK_FREE_GB_MIN="${WORKER_REFRESH_DISK_FREE_GB_MIN:-30}"
# Where the space is measured. The default is the daemon's own root filesystem,
# which on a node whose docker lives under /var is the filesystem backing
# /var/lib/docker — statfs on it reports exactly the pool images and the
# BuildKit cache compete for. A node that splits them points _DISK_PATH at the
# docker filesystem instead.
#
# STILL OPEN ON A CONTAINER-CAPABLE MAC, and deliberately not fixed here (job
# #487). Since #440 made the daemon native, `/` on a mac is the boot volume
# while docker lives in a VM: dev-air measured 7.2GB free on `/` against 76.3GB
# free inside colima, so the pre-flight refused a refresh (deploy #486) over
# space the build was never going to touch — and the comment above's remedy is
# unavailable, because that filesystem is not addressable from the host at all.
# A host-only node does not meet this, because it runs no build to protect; a
# dual-mode or container mac still does. Not fixed in this change for two
# reasons: asking docker for its own free space means running a container on
# every refresh of every node, which is a new failure mode on the fleet's whole
# hot path for a guard that is deliberately fail-open; and the 30GB floor was
# derived against `/` semantics on a Linux node and would have to be re-derived
# with it. The workaround today is to declare
# WORKER_REFRESH_DISK_FREE_GB_MIN_<node>=0 for that mac, which switches the
# guard off for it and for nothing else.
DISK_PATH="${WORKER_REFRESH_DISK_PATH:-/}"
# BuildKit cache cap for the prune pair. 15G was sized for the #115 cache mounts
# when agent-rust compiled the whole workspace into one; #352 deleted that build,
# leaving only the worker image's release target plus the shared registry/git
# caches, so the ceiling comes down to keep those hot and nothing else. A cap
# below what is live evicts the coldest entries — a slower next build, never a
# wrong one.
BUILDER_KEEP_STORAGE="8GB"
# How long the VM trim below may run. `fstrim -av` over a 100GiB volume is not
# instant, and it sits on the refresh's hot path on both the successful and the
# failed leg — so it is bounded the way everything else here is, and the bound is
# printed rather than implied. Overridable per node the way the disk knobs above
# are, through the environment file build-worker.sh renders.
DISK_TRIM_TIMEOUT_SECS="${WORKER_REFRESH_DISK_TRIM_TIMEOUT_SECS:-120}"
# Held to a number here rather than where it is compared: a bound that is not one
# makes `[ -lt ]` fail, and a failure inside the trim is a loop rather than a
# refusal (see refresh_disk_trim_vm).
case "$DISK_TRIM_TIMEOUT_SECS" in
  '' | *[!0-9]*) DISK_TRIM_TIMEOUT_SECS=120 ;;
esac

# Free space on $DISK_PATH in 1K blocks; empty when df cannot report it.
refresh_disk_free_kb() {
  _df_row="$(df -Pk "$DISK_PATH" 2>/dev/null | tail -n 1)"
  # Word-split the POSIX df row deliberately: fs blocks used AVAIL cap mount.
  # shellcheck disable=SC2086
  set -- $_df_row
  [ $# -ge 4 ] || return 0
  case "$4" in
    '' | *[!0-9]*) return 0 ;;
  esac
  echo "$4"
}

# Render 1K blocks as GB with one decimal, for a human reading a deploy leg.
# Integer arithmetic only: the worker image is debian-slim, so this depends on
# nothing beyond the shell itself.
refresh_disk_gb() {
  _tenths=$(( $1 * 10 / 1048576 ))
  echo "$(( _tenths / 10 )).$(( _tenths % 10 ))"
}

# Refuse a build that cannot possibly fit, in SECONDS and with the numbers that
# explain it, instead of burning ten minutes of cargo into a doomed build that
# then strands a half generation. Read-only (no docker call), so it belongs in
# the validate-first block. Fails OPEN when df cannot report: an unreadable
# filesystem shape must not block a refresh that would otherwise work.
refresh_disk_preflight() {
  _free_kb="$(refresh_disk_free_kb)"
  if [ -z "$_free_kb" ]; then
    echo "worker-refresh: disk pre-flight: df cannot read $DISK_PATH — proceeding unchecked"
    return 0
  fi
  DISK_FREE_KB_BEFORE="$_free_kb"
  _free_gb="$(refresh_disk_gb "$_free_kb")"
  echo "worker-refresh: disk pre-flight: ${_free_gb}GB free on $DISK_PATH, need ${DISK_FREE_GB_MIN}GB"
  if [ "$(( _free_kb / 1048576 ))" -lt "$DISK_FREE_GB_MIN" ]; then
    echo "worker-refresh: insufficient docker disk: need ~${DISK_FREE_GB_MIN}GB for a new image generation + cache growth, have ${_free_gb}GB free on $DISK_PATH — prune (docker image prune -f; docker builder prune -f --keep-storage $BUILDER_KEEP_STORAGE) or grow the VM disk; refusing build (live images untouched)" >&2
    return 1
  fi
  return 0
}

# What the build actually cost, sampled the same way the pre-flight samples it
# and printed BEFORE the prune reclaims anything. This is the number
# DISK_FREE_GB_MIN above must be re-derived from: every past derivation
# (#248, #347, #351) needed an operator sampling `df` over ssh through a live
# refresh, and #351's showed the floor had drifted below the real peak.
# Under-reports a transient mid-build peak — it is two samples, not a watcher —
# but it is the only one that costs nothing and appears in every deploy leg.
refresh_disk_report_build_cost() {
  [ -n "${DISK_FREE_KB_BEFORE:-}" ] || return 0
  _after_kb="$(refresh_disk_free_kb)"
  [ -n "$_after_kb" ] || return 0
  _used_kb=$(( DISK_FREE_KB_BEFORE - _after_kb ))
  [ "$_used_kb" -ge 0 ] || _used_kb=0
  echo "worker-refresh: disk: build consumed $(refresh_disk_gb "$_used_kb")GB on $DISK_PATH ($(refresh_disk_gb "$DISK_FREE_KB_BEFORE")GB -> $(refresh_disk_gb "$_after_kb")GB free, pre-prune; floor is ${DISK_FREE_GB_MIN}GB)"
}

# ── returning what the prune freed to the filesystem the guard measures ──────
# ON A CONTAINER-CAPABLE MAC THE PRUNE PAIR AND THE PRE-FLIGHT ADDRESS TWO
# DIFFERENT FILESYSTEMS, and without this they never meet. Since #440 made the
# daemon native, `/` is the boot volume while docker lives in a colima VM: the
# prune above frees blocks INSIDE that VM, and a lima disk image grows on write
# and never shrinks on delete — so none of that space returns to the filesystem
# the pre-flight reads, and #248's prune-on-failure half is a no-op for the
# number that decides whether the next attempt is allowed to start. `fstrim`
# marks the already-free blocks as discardable and the hypervisor punches them
# out of the backing file; it deletes nothing — no image, no container, no cache.
#
# Measured on dev-air, 2026-08-08, after deploy #508 was refused at 3.2GB free on
# `/`: the datadisk was 99G host-allocated against 18G used inside the VM, and
# one `fstrim -av` trimmed 80.1 GiB, taking the file to 19G and `/` from 3.4G to
# 84G free. Discard is plumbed end to end and that was verified rather than
# assumed — the guest advertises it (`lsblk`: vdb DISC-GRAN 512B, DISC-MAX 100G)
# AND macOS Virtualization.framework passes it through to the backing file, which
# is what those 80.1 GiB prove. The guest's advertisement alone is not sufficient.
#
# THIS MAKES THE GUARD'S NUMBER HONEST; IT DOES NOT FIX THE MEASUREMENT. `/` is
# still the wrong filesystem to size a docker build against, and job #487's item
# beside $DISK_PATH above stays open with both its reasons intact. What changes
# is that the number now moves: while the datadisk sat fully allocated, VM writes
# largely reused blocks the host had already given it and `/` barely moved, so
# the host reading tracked the build's real needs poorly in BOTH directions; once
# the file is sparse again, VM growth does consume `/`. That is also the argument
# against simply declaring WORKER_REFRESH_DISK_FREE_GB_MIN_<node>=0 — the
# documented workaround, and what the air is NOT running.
#
# Best-effort and fail-open, like the guard it serves and like the nix reaper's
# charter next door (crates/worker/src/nix.rs): a node with no colima, a colima
# with no fstrim, a hypervisor that ignores discard, and a trim that outruns its
# bound all report and continue. The trim is never the reason a refresh fails.
# LINUX IS UNTOUCHED — there the prune and the pre-flight are the same
# filesystem, so there is nothing to hand back and this makes no call at all.
refresh_disk_trim_vm() {
  [ "$(uname -s)" = Darwin ] || return 0
  if ! command -v colima > /dev/null 2>&1; then
    echo "worker-refresh: no 'colima' on this daemon's PATH ('${PATH}') — skipping the VM trim, so what the prune freed stays inside the VM and $DISK_PATH sees none of it (continuing: a refresh is never failed by the trim)"
    return 0
  fi
  echo "worker-refresh: trimming the colima VM (bounded at ${DISK_TRIM_TIMEOUT_SECS}s): the prune freed blocks INSIDE the VM, and a lima disk image never shrinks on delete — until this runs, none of that reclaim reaches $DISK_PATH, which is what the pre-flight measures"
  _trim_before_kb="$(refresh_disk_free_kb)"
  # Guarded, unlike the two `mktemp -d`s the phases open with, because THIS one
  # can run from the EXIT trap: a `set -e` abort inside a trap that re-enters the
  # prune is a loop, not a failure, so nothing in here may exit non-zero.
  _trim_out="$(mktemp 2> /dev/null || true)"
  if [ -z "$_trim_out" ]; then
    echo "worker-refresh: cannot stage the colima trim's output ('mktemp' failed) — skipping the VM trim (continuing: a refresh is never failed by the trim)"
    return 0
  fi
  _trim_done="$_trim_out.done"
  # The bound is a POLLED FLAG the trim raises for itself, and both halves of that
  # are deliberate. Not `timeout`, which macOS does not ship — homebrew's
  # coreutils installs it as `gtimeout`, so a bound built on it would silently
  # never apply on the one platform this ever runs on. And not `kill -0`, which
  # answers "still running" for a child nobody has reaped yet. The flag is
  # renamed into place so it is never observed half-written.
  {
    if colima ssh -- sudo fstrim -av > "$_trim_out" 2>&1; then
      _trim_child_rc=0
    else
      _trim_child_rc=$?
    fi
    printf '%s\n' "$_trim_child_rc" > "$_trim_done.part"
    mv -f "$_trim_done.part" "$_trim_done"
  } < /dev/null &
  _trim_pid=$!
  _trim_waited=0
  while [ ! -f "$_trim_done" ] && [ "$_trim_waited" -lt "$DISK_TRIM_TIMEOUT_SECS" ]; do
    sleep 1
    _trim_waited=$(( _trim_waited + 1 ))
  done
  _trim_rc="$(cat "$_trim_done" 2> /dev/null || true)"
  case "$_trim_rc" in
    '' | *[!0-9]*) _trim_rc=0 ;;
  esac
  if [ ! -f "$_trim_done" ]; then
    # KILL, not TERM: a background subshell can inherit this script's TERM
    # handler, and running it here would print the build phase's cancellation
    # notice over a refresh nobody cancelled. What is already running inside the
    # VM is welcome to finish — the refresh just stops waiting for it.
    kill -KILL "$_trim_pid" 2> /dev/null || true
    echo "worker-refresh: the colima trim did not finish within ${DISK_TRIM_TIMEOUT_SECS}s and was abandoned — the VM keeps the blocks and $DISK_PATH sees no reclaim from it (continuing: a refresh is never failed by the trim; raise WORKER_REFRESH_DISK_TRIM_TIMEOUT_SECS if this node needs longer)"
  elif [ "$_trim_rc" -ne 0 ]; then
    echo "worker-refresh: the colima trim failed (exit $_trim_rc: $(tail -n 1 "$_trim_out" 2> /dev/null || true)) — what the prune freed stays inside the VM (continuing: a refresh is never failed by the trim)"
  else
    _trim_after_kb="$(refresh_disk_free_kb)"
    if [ -z "$_trim_before_kb" ] || [ -z "$_trim_after_kb" ]; then
      echo "worker-refresh: trimmed the colima VM: the blocks the prune freed are back on $DISK_PATH"
    else
      _trim_returned_kb=$(( _trim_after_kb - _trim_before_kb ))
      [ "$_trim_returned_kb" -ge 0 ] || _trim_returned_kb=0
      echo "worker-refresh: trimmed the colima VM: returned $(refresh_disk_gb "$_trim_returned_kb")GB to $DISK_PATH ($(refresh_disk_gb "$_trim_before_kb")GB -> $(refresh_disk_gb "$_trim_after_kb")GB free), which is the reclaim the pre-flight will actually read"
    fi
  fi
  rm -f "$_trim_out" "$_trim_done" "$_trim_done.part"
  return 0
}

# The sanctioned safe prune pair (#183) plus the reclaim it won, reported so an
# operator reads the disk story off the deploy leg without ssh'ing the node.
# NEVER `-a`: only DANGLING images (the generation a retag-swap orphaned, or the
# temp tags a failed build left behind) and BuildKit cache above the keep
# threshold. Layers in use by the running daemon and by job containers are
# protected by docker itself — keep it that way. Cleanup never fails its caller:
# on the failure path the real cause is already reported, on the success path
# the new images are already live.
refresh_disk_prune() {
  _why="$1"
  _before_kb="$(refresh_disk_free_kb)"
  docker image prune -f >/dev/null 2>&1 || true
  docker builder prune -f --keep-storage "$BUILDER_KEEP_STORAGE" >/dev/null 2>&1 || true
  _after_kb="$(refresh_disk_free_kb)"
  if [ -z "$_before_kb" ] || [ -z "$_after_kb" ]; then
    echo "worker-refresh: pruned after $_why: dangling images + BuildKit cache above $BUILDER_KEEP_STORAGE"
  else
    _reclaimed_kb=$(( _after_kb - _before_kb ))
    [ "$_reclaimed_kb" -ge 0 ] || _reclaimed_kb=0
    echo "worker-refresh: pruned after $_why: reclaimed $(refresh_disk_gb "$_reclaimed_kb")GB ($(refresh_disk_gb "$_before_kb")GB -> $(refresh_disk_gb "$_after_kb")GB free on $DISK_PATH)"
  fi
  # Both callers reach this — the successful refresh and the failed build — and
  # the failed one is the point: it is where the next attempt's headroom is
  # decided, and on a mac the prune above returned none of it.
  refresh_disk_trim_vm
}

case "$PHASE" in
build)
  SHA="${2:?build needs a SHA}"
  TAG="${3:?build needs a tag}"
  # Validate-first (spec §3.1): a refresh must be ATOMIC — every prerequisite is
  # checked here, BEFORE any docker mutation, so a misconfigured node fails with
  # its working images intact. The 2026-07-23 incident stranded the nuc worker
  # with NO agent images because a refresh mutated before it validated its own
  # config; nothing below this block may touch a live image until the new one is
  # built to completion.
  refresh_run_spec_report build
  GIT_URL="${WORKER_REFRESH_GIT_URL:?set WORKER_REFRESH_GIT_URL (ssh://git@<ssh-front>:2222/<owner>/<repo>.git)}"
  KEY="${WORKER_GIT_KEY:-/data/keys/worker_git}"
  if [ ! -f "$KEY" ]; then
    echo "worker-refresh: git key $KEY missing — refusing build (live images untouched)" >&2
    exit 2
  fi
  export GIT_SSH_COMMAND="ssh -i $KEY -o StrictHostKeyChecking=accept-new -o IdentitiesOnly=yes"

  # Disk pre-flight (deploy #248): read-only, so it lives in the validate-first
  # block — a node without headroom fails here, before the fetch and before any
  # docker call, with the free/needed numbers in the leg detail.
  #
  # It exists to protect a docker build, so a node that runs none is not asked:
  # the floor is a whole image generation plus BuildKit growth, and refusing a
  # refresh over headroom nothing is going to consume is how a host-only mac
  # gets locked out of every deploy (job #486, the air at 7.1GB free on / with
  # 99G of colima datadisk it does not need).
  if refresh_builds_images; then
    refresh_disk_preflight || exit 1
  else
    echo "worker-refresh: disk pre-flight skipped on ${WORKER_NODE:-?} — WORKER_MODES names no container runtime and this node builds no image, so there is no image generation to find room for (design #309 §1)"
  fi

  # ── a Darwin node's daemon is COMPILED, not extracted (#440 D6, corrected
  # 2026-08-07) ───────────────────────────────────────────────────────────────
  # D6 has the swap lift the binary out of the worker image, which is a LINUX
  # container: on a mac that binary is an ELF file the supervisor loops on with
  # `cannot execute binary file` (gumbo-air-0, 2026-08-06). So this node builds
  # its own, in the build phase where minutes are affordable, and the swap
  # installs what was built. WORKER_CARGO is written into the environment file
  # by build-worker.sh, because the daemon's PATH is the agent's and a
  # nix-darwin or rustup cargo is not on it.
  #
  # Refused in the validate-first block for that block's reason: a node that
  # cannot compile a daemon must fail with its images and its live daemon
  # intact, rather than half-way through, and a converted mac that was never
  # given a toolchain is exactly the state the air was left in.
  NATIVE_DAEMON=""
  if [ "$(uname -s)" = Darwin ]; then
    NATIVE_DAEMON=1
    CARGO="${WORKER_CARGO:-cargo}"
    if ! command -v "$CARGO" > /dev/null 2>&1; then
      echo "worker-refresh: this is a Darwin node and '$CARGO' is not on this daemon's PATH ('${PATH}') — the daemon binary in chuggernaut/worker:$TAG is a LINUX binary (design #440 D6 holds on Linux only, corrected 2026-08-07), so a mac COMPILES its own and this one cannot; REFUSING build (live images and live daemon untouched). Re-apply the node's spec with deploy/prod/build-worker.sh from the operator's laptop: it resolves the node's cargo and writes WORKER_CARGO into the environment file." >&2
      exit 2
    fi
    # The toolchain is a DIRECTORY, not one binary: cargo resolves `rustc`
    # through PATH, so an absolute WORKER_CARGO passes `command -v` while the
    # compile still fails. Both questions are asked here, in the validate-first
    # block written to catch them, rather than half-way through the build.
    CARGO_DIR="$(dirname "$(command -v "$CARGO")")"
    if ! "$CARGO" --version > /dev/null 2>&1; then
      echo "worker-refresh: this is a Darwin node and '$CARGO' does not run ('$CARGO --version' failed) — a rustup shim with no default toolchain execs and compiles nothing; REFUSING build (live images and live daemon untouched). Set the node's default toolchain, or re-apply its spec with deploy/prod/build-worker.sh, which asks this same question over ssh before it converts." >&2
      exit 2
    fi
    if ! PATH="$CARGO_DIR:$PATH" command -v rustc > /dev/null 2>&1; then
      echo "worker-refresh: this is a Darwin node with '$CARGO' but no 'rustc' beside it or on this daemon's PATH ('${PATH}') — cargo resolves its compiler THROUGH PATH, so the build would fail after the images are replaced; REFUSING build (live images and live daemon untouched). Put rustc where cargo is and re-apply the node's spec with deploy/prod/build-worker.sh, which prepends that directory to the launchd agent's PATH." >&2
      exit 2
    fi
    BUILD_DIR="${WORKER_BUILD_DIR:-$HOME/chuggernaut-worker/build}"
  fi

  # Build every image under a TEMP tag first; only once all three build to
  # completion do we retag-swap them onto the live $TAG (below). A build that
  # dies part-way therefore leaves each live tag exactly as it was — a node that
  # had working images keeps them. The temp tags are dropped on exit; after a
  # successful retag they share an image id with the live tag, so `rmi` only
  # untags the temp name and the live image survives.
  NEW="$TAG-refresh"
  TMP="$(mktemp -d)"
  # Set once the first `docker build` is reached: only from there on can a
  # failure have stranded a partial generation worth pruning.
  BUILD_STARTED=0

  # Drop the temp tags, then — on the FAILURE path (deploy #248) — prune what a
  # dead build stranded, so a failed attempt leaves the docker filesystem no
  # fuller than it started and cannot poison the retry. Order matters: the `rmi`
  # is what turns the temp tags into the dangling images the prune reclaims.
  refresh_cleanup() {
    _rc="$1"
    rm -rf "$TMP"
    if refresh_builds_images; then
      docker rmi -f "chuggernaut/worker:$NEW" "chuggernaut/agent:$NEW" "chuggernaut/agent-rust:$NEW" >/dev/null 2>&1 || true
    fi
    if [ "$_rc" -ne 0 ] && [ "$BUILD_STARTED" -eq 1 ]; then
      refresh_disk_prune "a failed build"
    fi
  }
  trap 'RC=$?; refresh_cleanup "$RC"; exit "$RC"' EXIT
  # A cancelled deploy signals this script's whole process group (ticket #254:
  # the daemon's `refresh_cancel`), so the build dies mid-flight. POSIX sh does
  # NOT run the EXIT trap when it is killed by a signal — without this handler
  # the staged `-refresh` tags and the partial generation behind them would be
  # stranded on the node, which is exactly the disk-pressure loop #248 closed.
  # Exiting from the handler runs the EXIT trap, so a cancel cleans up on the
  # same path a failed build does. 143 = 128 + SIGTERM, the conventional code.
  trap 'echo "worker-refresh: cancelled — dropping staged tags (live images untouched)" >&2; exit 143' TERM INT
  # Fetch the repo's advertised HEAD (its default branch tip) over the ssh
  # front, then verify it resolves to the requested SHA before building.
  #
  # We fetch a REF, not a raw commit: agents clone by ref (container::bootstrap_cmd
  # runs `git clone --single-branch --branch "$JOB_BRANCH"`), and the ssh front's
  # bare repos only enable `uploadpack.allowFilter` (crates/vcs `ensure_upload_filter`)
  # — they do NOT set `allowAnySHA1InWant`, so a `want <raw sha>` fetch is refused
  # ("Server does not allow request for unadvertised object"). A ref fetch needs
  # no such server capability, so this rides the proven agent-clone path.
  #
  # HEAD is the requested SHA in the deploy path: prod requests refresh to the
  # commit it just checked out, which is the tip of `main` and — because the
  # platform hosts its own repo (§3.1) — exactly the ssh front's HEAD. If HEAD
  # has moved off the requested SHA we refuse rather than build the wrong tree:
  # the daemon aborts, the old images stay, and the deploy surfaces a WARNING
  # with the drift (spec §3.1) instead of silently shipping the wrong SHA.
  refresh_phase "fetch-context"
  git -C "$TMP" init -q
  git -C "$TMP" fetch -q --depth 1 "$GIT_URL" HEAD
  GOT="$(git -C "$TMP" rev-parse FETCH_HEAD)"
  if [ "$GOT" != "$SHA" ]; then
    echo "worker-refresh: remote HEAD $GOT != requested $SHA — refusing build (drift stays, deploy warns)" >&2
    exit 1
  fi

  # Build the three images to their TEMP tags. Any failure here aborts under
  # `set -e` with the live tags untouched (the temp leftovers are pruned by the
  # trap on exit).
  #
  # DOCKER_BUILDKIT=1 turns on the in-daemon BuildKit builder so the
  # Dockerfiles' RUN --mount=type=cache dependency caches take effect (#115),
  # keeping the daemon self-refresh build (every deploy) warm across SHA bumps.
  # No-op where BuildKit is already default; harmless (mounts ignored, cold
  # build) where unavailable. This mirrors build-worker.sh's laptop path so both
  # benefit identically.
  export DOCKER_BUILDKIT=1


  # Stage each build context to a FILE, then feed `docker build` from it — never
  # `git archive | docker build`. A POSIX pipeline reports only the LAST
  # command's status, so a `git archive` that dies mid-stream would be masked by
  # a build that "succeeds" on a truncated context. With a staged file, `set -e`
  # aborts on the archive step itself and the build reads a complete context.
  #
  # --label chug.git.sha=<sha> stamps the requested SHA INTO the worker image so
  # we can positively PROVE, before the retag-swap, that the image about to go
  # live was built from the commit we asked for — an exit code alone is not
  # trustworthy (the buildx-missing class of failure).
  # How many images this generation has, said out loud in the markers because a
  # host-only Linux node builds the worker image and nothing else — "1/3" there
  # would report two builds that are never coming.
  REFRESH_IMAGE_COUNT=3
  [ -n "$SERVES_CONTAINER" ] || REFRESH_IMAGE_COUNT=1
  if refresh_builds_images; then
    git -C "$TMP" archive --format=tar FETCH_HEAD > "$TMP/worker.tar"
    [ -s "$TMP/worker.tar" ] || { echo "worker-refresh: empty worker context — aborting (live images untouched)" >&2; exit 1; }
    BUILD_STARTED=1
    refresh_phase "build-image 1/$REFRESH_IMAGE_COUNT worker"
    docker build -q -t "chuggernaut/worker:$NEW" \
      -f deploy/prod/Dockerfile.worker --build-arg "CHUG_GIT_SHA=$SHA" \
      --label "chug.git.sha=$SHA" - < "$TMP/worker.tar"
  else
    echo "worker-refresh: skipping chuggernaut/worker:$NEW — this node names no container runtime and compiles its own daemon, so the image has no passenger left here (design #309 §1, #440 D6)"
  fi

  # Agent images the job types run in.
  if [ -n "$SERVES_CONTAINER" ]; then
    git -C "$TMP" archive --format=tar FETCH_HEAD:deploy/dev > "$TMP/agent.tar"
    [ -s "$TMP/agent.tar" ] || { echo "worker-refresh: empty agent context — aborting (live images untouched)" >&2; exit 1; }
    refresh_phase "build-image 2/3 agent"
    docker build -q -t "chuggernaut/agent:$NEW" -f Dockerfile.agent - < "$TMP/agent.tar"
    git -C "$TMP" archive --format=tar FETCH_HEAD > "$TMP/agent-rust.tar"
    [ -s "$TMP/agent-rust.tar" ] || { echo "worker-refresh: empty agent-rust context — aborting (live images untouched)" >&2; exit 1; }
    # Was the long pole while this image baked the #123 warm-target seed (air 673s,
    # nuc 332s, nearly all of it the workspace compile). #352 deleted the seed, and
    # with it the `COPY . /workspace` that made every SHA bump invalidate the
    # layers below — so on a node that has built this image before, a refresh is
    # now layer-cache hits down to the binary fetches.
    refresh_phase "build-image 3/3 agent-rust"
    docker build -q -t "chuggernaut/agent-rust:$NEW" \
      -f deploy/prod/Dockerfile.agent-rust - < "$TMP/agent-rust.tar"
  else
    echo "worker-refresh: skipping chuggernaut/{agent,agent-rust}:$NEW — a host job type declares no image (crates/types/src/job_type.rs), so nothing launched on this node ever runs one"
  fi

  # Positively assert the freshly built worker image carries the requested SHA
  # label BEFORE the retag-swap flips the live tag onto it. A build whose label
  # is missing or wrong (stale layer, silent buildx failure) must never become
  # the live image — refuse the swap; the trap drops the temp tags and the live
  # images stay exactly as they were.
  if refresh_builds_images; then
    refresh_phase "verify-label"
    GOT_LABEL="$(docker inspect --format '{{index .Config.Labels "chug.git.sha"}}' "chuggernaut/worker:$NEW" 2>/dev/null | tr -d '[:space:]' || true)"
    if [ "$GOT_LABEL" != "$SHA" ]; then
      echo "worker-refresh: built worker image label '$GOT_LABEL' != requested $SHA — refusing retag-swap (live images untouched)" >&2
      exit 1
    fi
  fi

  # On Darwin, the fourth artifact: the Mach-O daemon this node will actually
  # run, compiled from the same context the worker image was built from. Placed
  # BEFORE the retag-swap so a node whose compile fails keeps the whole previous
  # generation, images included — the swap phase has nothing to install and
  # never runs. `--locked` so the dependency graph is the tree's, and
  # CHUG_GIT_SHA because the daemon reads it with `option_env!` and the fleet's
  # version column is that value. The source tree is replaced and `target/` is
  # kept: it is the only thing that makes this minutes instead of a cold
  # workspace compile. The exec check is the point of the exercise — a binary
  # that will not run here must never reach the install.
  #
  # BOTH binaries, on every mac, and the channel half is job #487's condition
  # reversed. #487 dropped it from a host-only node against a premise that was
  # true then — "on this node there is no container to inject one into either, so
  # building it spends the node's own cargo on a file nothing will read" — and
  # design #490 D2 kills it: the reader is the NODE, which execs this Mach-O
  # itself for a host agent task. A container-serving mac has always built it and
  # still discards it (its injected copy rides out of the image, #480), so with
  # `serves_container` and `serves_host` between them covering every legal
  # WORKER_MODES the list is unconditional again.
  NATIVE_BINS="--bin chuggernaut --bin chuggernaut-channel"
  if [ -n "$NATIVE_DAEMON" ]; then
    refresh_phase "build-daemon"
    git -C "$TMP" archive --format=tar FETCH_HEAD > "$TMP/native.tar"
    [ -s "$TMP/native.tar" ] || { echo "worker-refresh: empty context for the native daemon build — aborting (live images untouched)" >&2; exit 1; }
    rm -rf "$BUILD_DIR/src"
    mkdir -p "$BUILD_DIR/src"
    tar -xf "$TMP/native.tar" -C "$BUILD_DIR/src"
    (
      cd "$BUILD_DIR/src"
      # shellcheck disable=SC2086
      PATH="$CARGO_DIR:$PATH" CHUG_GIT_SHA="$SHA" CARGO_TARGET_DIR="$BUILD_DIR/target" \
        "$CARGO" build --release --locked $NATIVE_BINS
    )
    if ! "$BUILD_DIR/target/release/chuggernaut" --version > /dev/null 2>&1; then
      echo "worker-refresh: the daemon binary just compiled at $BUILD_DIR/target/release/chuggernaut does not run on this node — refusing to stage a binary the supervisor would loop on; aborting (live images and live daemon untouched)" >&2
      exit 1
    fi
    printf '%s\n' "$SHA" > "$BUILD_DIR/native.sha"
    echo "worker-refresh: compiled the native daemon for $SHA with $CARGO into $BUILD_DIR/target/release (design #440 D6, corrected 2026-08-07: the worker image is Linux and this node is not)"
  fi

  # All three built to completion — retag-swap onto the live tag. `docker tag`
  # is local and instant, so the live images flip to the new build only now,
  # and only after we know every image is buildable.
  if refresh_builds_images; then
    refresh_phase "retag-swap"
    docker tag "chuggernaut/worker:$NEW"     "chuggernaut/worker:$TAG"
  fi
  if [ -n "$SERVES_CONTAINER" ]; then
    docker tag "chuggernaut/agent:$NEW"      "chuggernaut/agent:$TAG"
    docker tag "chuggernaut/agent-rust:$NEW" "chuggernaut/agent-rust:$TAG"
  fi

  # Bound the node's docker disk after every refresh (the 2026-07-23 air
  # ENOSPC incident): the retag-swap just stranded the previous generation as
  # dangling — prune those (NEVER -a: live tags must survive, #183) and cap
  # the BuildKit cache at $BUILDER_KEEP_STORAGE. Report the build's own disk
  # cost first, while the generation it built is still unpruned.
  if refresh_builds_images; then
    refresh_disk_report_build_cost
    refresh_phase "prune"
    refresh_disk_prune "a successful refresh"
  fi

  if [ -n "$SERVES_CONTAINER" ]; then
    echo "worker-refresh: built chuggernaut/{worker,agent,agent-rust}:$TAG ($SHA)"
  elif refresh_builds_images; then
    echo "worker-refresh: built chuggernaut/worker:$TAG ($SHA) — this node names no container runtime, so the agent images were skipped and only the daemon's own image was made"
  else
    echo "worker-refresh: built no image for $SHA — this node names no container runtime and compiles its own daemon, which is staged and ready for the swap"
  fi
  ;;

swap)
  TAG="${2:?swap needs a tag}"
  # ── install the binary, ask the supervisor to restart (design #440 D6) ───────
  # The daemon is a BINARY UNDER A SUPERVISOR, not a container (#440 D1/D2), and
  # that collapses this phase to two acts: put the new binary on the node, then
  # ask the supervisor to restart the unit. What used to be here — a detached
  # `docker:cli` sibling that removed `chug-worker` and re-composed `docker run`
  # from a dozen carried-forward `-e` flags, the keys and socket mounts recovered
  # by `docker inspect`, the KVM device, the nix mounts — existed for ONE reason:
  # a container cannot replace itself, so the replacement had to be re-composed
  # by something else and every value it needed had to be handed over. None of
  # that is true of a unit:
  #
  #   * the DETACHED SWAPPER, because a supervisor restarting its own unit has no
  #     self-replacement problem — the supervisor is the starter (#372 §8 R1);
  #   * the KEYS and SOCKET mount recovery, because a native daemon reads the
  #     node's filesystem and opens the node's docker socket directly (#440 D5);
  #   * the KVM DEVICE and the NIX MOUNTS, because a process on the node already
  #     has the node's devices and the node's /nix (#440 §5);
  #   * every `*_ARGS` ENVIRONMENT carry-forward (cache dir, disk knobs, slots,
  #     modes, KVM settings, nix settings, RUST_LOG), because the run spec is a
  #     file on the node that the supervisor loads on every start — a value
  #     survives because it is WRITTEN DOWN, which deletes the #55/#82
  #     silent-revert class at its root instead of defending against it eleven
  #     times (#440 D6/D7);
  #   * the RETAINED `chug-worker-swap` TRANSCRIPT (#270), because the record of
  #     a replacement that would not start is now the supervisor's own —
  #     `journalctl -u chug-worker` on Linux, the agent's log path on macOS —
  #     which is the thing the sibling container was standing in for.
  #
  # Job containers are untouched, as before and for a stronger reason: nothing
  # here removes a container at all, and the dispatcher's poll-based wait
  # re-attaches over the new daemon (spec §3.1). A HOST task is untouched too,
  # for the first time: `systemctl restart` kills the unit's cgroup, and #440 D3
  # puts each host task in its own transient scope outside it.
  #
  # Reported here because this is the last thing the node says before it goes
  # away: what the daemon was running, read off the environment the supervisor
  # gave it.
  refresh_run_spec_report swap

  # Where the three artifacts go. The defaults are what build-worker.sh installs
  # (deploy/prod/build-worker.sh `REMOTE_INSTALL`) and — for the channel binary
  # and this script — what crates/worker/src/config.rs already defaults to, so a
  # stock node needs none of these set.
  DAEMON_BIN="${WORKER_DAEMON_BIN:-/usr/local/bin/chuggernaut}"
  CHANNEL_BIN="${WORKER_CHANNEL_BINARY:-/usr/local/lib/chuggernaut/chuggernaut-channel}"
  # The HOST copy's path, which is build-worker.sh's `HOST_CHANNEL_BIN` and must
  # stay in step with it: no run spec carries this, so the two defaults ARE the
  # agreement (design #490 D2 leaves the daemon's own config variable to slice 5,
  # which is the first thing that reads the file — slice 4 probed the agent CLI
  # and left this one alone).
  HOST_CHANNEL_BIN="${WORKER_HOST_CHANNEL_BINARY:-/usr/local/lib/chuggernaut/chuggernaut-channel-host}"
  REFRESH_SCRIPT="${WORKER_REFRESH_SCRIPT:-/usr/local/lib/chuggernaut/worker-refresh.sh}"
  SWAP_UNIT="${WORKER_UNIT:-chug-worker.service}"
  SWAP_AGENT_LABEL="${WORKER_AGENT_LABEL:-com.chuggernaut.worker}"

  # ── validate-first, exactly as the build phase does ──────────────────────────
  # Everything that can refuse refuses HERE, before a single byte is installed,
  # so a node this script cannot restart keeps serving on the daemon it has. The
  # build phase already retag-swapped the images, so a refused swap is a node one
  # generation behind with a failed deploy leg naming why — which is the trade
  # #440 D4 makes everywhere else in this file.

  # A node NOBODY HAS CONVERTED YET. Until an operator runs build-worker.sh
  # against it the daemon is still `chug-worker`, a container, and this phase
  # cannot supervise what has no supervisor: installing a binary would write into
  # the container's own writable layer and vanish with it. The fleet is mixed by
  # design (#440's slice ordering), so this is the expected state of a node, not
  # an error in it — but the refusal is the honest answer rather than half a
  # swap, and it is the exact counterpart of the note build-worker.sh prints when
  # it converts a node. The marker is docker's own; it is overridable because
  # the shell test runs INSIDE a container and must be able to drive both sides.
  if [ -f "${WORKER_SWAP_CONTAINER_MARKER:-/.dockerenv}" ]; then
    echo "worker-refresh: this daemon is running INSIDE a container, so it is a node design #440 has not converted yet — the swap installs a binary and restarts a supervisor unit (#440 D6) and there is no unit here; REFUSING swap (live daemon untouched, job containers untouched, images already built). Convert the node from the operator's laptop with 'WORKER_SSH=<user>@<node> deploy/prod/build-worker.sh' (deploy/prod/README.md §6); until then this node is deployed over ssh, not by self-refresh." >&2
    exit 1
  fi

  # Which supervisor, decided by what the node IS — the same question
  # build-worker.sh asks over ssh, asked here of the node we are standing on.
  case "$(uname -s)" in
    Linux) SUPERVISOR=systemd ;;
    Darwin) SUPERVISOR=launchd ;;
    *)
      echo "worker-refresh: 'uname -s' reports '$(uname -s)' — the worker daemon is supervised by systemd (Linux) or launchd (macOS) and there is no third supervisor (design #440 D2); REFUSING swap (live daemon untouched)" >&2
      exit 1
      ;;
  esac

  # The supervisor has to be reachable AND the daemon has to be its child. Both,
  # because they fail differently: no `systemctl` on PATH is a node that cannot
  # be restarted at all, while a unit that is not running is a daemon someone
  # started by hand — and asking the supervisor to start it would leave TWO
  # daemons on one node, which is the fleet-record split #440 §1 refuses and
  # #372 §8 R2 names.
  case "$SUPERVISOR" in
    systemd)
      if ! command -v systemctl > /dev/null 2>&1; then
        echo "worker-refresh: no 'systemctl' on this daemon's PATH ('${PATH}') — the swap asks the supervisor to restart $SWAP_UNIT (design #440 D6) and cannot; REFUSING swap (live daemon untouched). The unit sets its own PATH (deploy/prod/build-worker.sh); re-apply the node's spec with build-worker.sh from the operator's laptop." >&2
        exit 1
      fi
      if ! systemctl is-active --quiet "$SWAP_UNIT"; then
        echo "worker-refresh: '$SWAP_UNIT' is not active, so this daemon is not the one systemd supervises — restarting the unit would start a SECOND daemon beside this one, which splits the node into two fleet rows (design #440 §1); REFUSING swap (live daemon untouched). Re-apply the node's spec with deploy/prod/build-worker.sh, or point WORKER_UNIT at the unit that owns this process." >&2
        exit 1
      fi
      ;;
    launchd)
      if ! command -v launchctl > /dev/null 2>&1; then
        echo "worker-refresh: no 'launchctl' on this daemon's PATH ('${PATH}') — the swap asks the supervisor to restart $SWAP_AGENT_LABEL (design #440 D6) and cannot; REFUSING swap (live daemon untouched)." >&2
        exit 1
      fi
      if ! launchctl print "gui/$(id -u)/$SWAP_AGENT_LABEL" > /dev/null 2>&1; then
        echo "worker-refresh: no launchd agent '$SWAP_AGENT_LABEL' in gui/$(id -u), so this daemon is not the one launchd supervises — kickstarting it would start a SECOND daemon beside this one (design #440 §1); REFUSING swap (live daemon untouched). Re-apply the node's spec with deploy/prod/build-worker.sh, or point WORKER_AGENT_LABEL at the agent that owns this process." >&2
        exit 1
      fi
      ;;
  esac

  # The three install paths have to be WRITABLE, and that is a real question on
  # both platforms: /usr/local is root's on Linux and on macOS, where the daemon
  # is a GUI-domain agent running as the login user (deploy/prod/build-worker.sh
  # asks the same thing over ssh before it converts a mac). Every write below is
  # "unprivileged first, `sudo -n` as the fallback" — the shape build-worker.sh
  # uses — so this asks exactly that of the nearest EXISTING ancestor, since the
  # install creates what is missing. Asked without creating anything, so a
  # refusal changes no node; without it the operator gets a bare `sudo: a
  # password is required` from half-way through an install instead.
  #
  # Each channel path is on that list only when this node writes that one: the
  # injected copy when it serves containers, the host copy when it serves host
  # launches (design #490 D2), both on a dual-mode node. Demanding a writable
  # path for a file this node will never write would refuse it over a permission
  # it never exercises.
  set -- "$DAEMON_BIN" "$REFRESH_SCRIPT"
  [ -z "$SERVES_CONTAINER" ] || set -- "$@" "$CHANNEL_BIN"
  [ -z "$SERVES_HOST" ] || set -- "$@" "$HOST_CHANNEL_BIN"
  for _p in "$@"; do
    _d="$(dirname "$_p")"
    while [ ! -d "$_d" ]; do _d="$(dirname "$_d")"; done
    if [ -w "$_d" ] || sudo -n test -w "$_d" 2> /dev/null; then continue; fi
    echo "worker-refresh: cannot install '$_p' — neither this daemon's user nor 'sudo -n' can write '$_d', the nearest directory that exists; REFUSING swap (live daemon untouched, job containers untouched, images already built). Re-apply the node's spec with deploy/prod/build-worker.sh from the operator's laptop, or grant this user passwordless sudo on the node." >&2
    exit 1
  done

  # ── extract (or stage), then install (design #440 D6) ────────────────────────
  # Which source each artifact comes from is decided by WHO EXECUTES IT, not by
  # which machine staged it. THE DAEMON runs on this node, under this
  # supervisor. THE CHANNEL BINARY never runs on this node at all: the daemon
  # reads it off the disk and injects it into every agent CONTAINER
  # (crates/worker/src/daemon.rs, `FileSource::LocalArtifact`), and a container
  # is Linux on both platforms. This script is /bin/sh either way.
  #
  # ON A LINUX NODE the two executors coincide, so all three come OUT OF THE
  # IMAGE the build phase just made and never from a compile on the node: that
  # keeps the build environment byte-identical to the containerized daemon's,
  # needs no Rust toolchain as a node machine fact, and leaves
  # deploy/prod/Dockerfile.worker the single definition of how the binary is
  # produced. This is the same `docker create` + `docker cp` pair
  # build-worker.sh runs over ssh — one extraction, two callers.
  #
  # ON DARWIN THEY DIVERGE, and #440's 2026-08-07 correction generalised over
  # both when it holds for only one. The DAEMON cannot come out of that image:
  # it is a Linux container, so what `docker cp` lifts out is an ELF launchd
  # loops on, and the build phase compiled a Mach-O beside the tree instead. The
  # CHANNEL BINARY is the opposite — a Mach-O is what no agent container can
  # exec, silently: Claude Code reports the chuggernaut-channel MCP server as
  # `pending` forever and the agent loses `update_status` and `submit_eval`
  # (#477, #478). So it rides out of the image here too, which the node's own
  # docker built and is therefore already linux/<this node's container arch>.
  #
  # The staged SHA is checked against the image's own label rather than trusted:
  # a staging directory from an earlier refresh is indistinguishable from this
  # one's by existence alone, and installing it would silently take the node
  # BACKWARDS. That check now also ties the two halves together — the daemon off
  # the tree and the channel binary out of the image are the same generation.
  refresh_phase "swap-extract"
  SWAP_STAGE="$(mktemp -d)"
  SWAP_CID=""
  swap_cleanup() {
    [ -z "$SWAP_CID" ] || docker rm -f "$SWAP_CID" > /dev/null 2>&1 || true
    rm -rf "$SWAP_STAGE"
  }
  trap 'RC=$?; swap_cleanup; exit "$RC"' EXIT
  SWAP_CHANNEL_FROM=""
  SWAP_HOST_CHANNEL_FROM=""
  if [ "$SUPERVISOR" = launchd ]; then
    BUILD_DIR="${WORKER_BUILD_DIR:-$HOME/chuggernaut-worker/build}"
    if [ -n "$SERVES_CONTAINER" ]; then
      SWAP_IMAGE_SHA="$(docker inspect --format '{{index .Config.Labels "chug.git.sha"}}' "chuggernaut/worker:$TAG" 2>/dev/null | tr -d '[:space:]' || true)"
      SWAP_STAGED_SHA="$(tr -d '[:space:]' < "$BUILD_DIR/native.sha" 2>/dev/null || true)"
      if [ -z "$SWAP_STAGED_SHA" ] || [ "$SWAP_STAGED_SHA" != "$SWAP_IMAGE_SHA" ]; then
        echo "worker-refresh: the native daemon staged at $BUILD_DIR is '${SWAP_STAGED_SHA:-<absent>}' and chuggernaut/worker:$TAG is '${SWAP_IMAGE_SHA:-<unlabelled>}' — this mac compiles its own daemon in the build phase (design #440 D6 is Linux-only, corrected 2026-08-07) and installing a staging directory from another generation would take the node backwards; REFUSING swap (live daemon untouched, the node stays one generation behind). Re-apply the node's spec with deploy/prod/build-worker.sh from the operator's laptop." >&2
        exit 1
      fi
    else
      # A host-only mac builds no image, so the label that generation was checked
      # against does not exist here and the check DEGRADES rather than being
      # dropped quietly: the staging directory must be there and must name a SHA,
      # and which one it names is reported. The swap phase is handed a tag and
      # never a SHA, so there is nothing else on this node to compare it with.
      SWAP_STAGED_SHA="$(tr -d '[:space:]' < "$BUILD_DIR/native.sha" 2>/dev/null || true)"
      if [ -z "$SWAP_STAGED_SHA" ]; then
        echo "worker-refresh: no native daemon staged at $BUILD_DIR (no readable native.sha) — this mac compiles its own daemon in the build phase and there is nothing here to install; REFUSING swap (live daemon untouched, the node stays one generation behind). Re-apply the node's spec with deploy/prod/build-worker.sh from the operator's laptop." >&2
        exit 1
      fi
      echo "worker-refresh: installing the native daemon staged at $BUILD_DIR for $SWAP_STAGED_SHA — this node names no container runtime, so there is no worker image label to cross-check that generation against (design #309 §1)"
    fi
    SWAP_FROM="the native build staged at $BUILD_DIR for $SWAP_STAGED_SHA"
    cp "$BUILD_DIR/target/release/chuggernaut" "$SWAP_STAGE/chuggernaut"
    cp "$BUILD_DIR/src/deploy/prod/worker-refresh.sh" "$SWAP_STAGE/worker-refresh.sh"
    # The HOST copy is the one artifact a mac takes out of its OWN build — the
    # exact inverse of the extraction below it, for the exact inverse reason
    # (design #490 D2): the node execs this one, so it must be the Mach-O.
    if [ -n "$SERVES_HOST" ]; then
      cp "$BUILD_DIR/target/release/chuggernaut-channel" "$SWAP_STAGE/chuggernaut-channel-host"
      SWAP_HOST_CHANNEL_FROM="$SWAP_FROM"
    fi
    if [ -n "$SERVES_CONTAINER" ]; then
      SWAP_CID="$(docker create "chuggernaut/worker:$TAG")"
      docker cp "$SWAP_CID:/usr/local/lib/chuggernaut/chuggernaut-channel" "$SWAP_STAGE/chuggernaut-channel"
      docker rm "$SWAP_CID" > /dev/null
      SWAP_CID=""
      SWAP_CHANNEL_FROM="chuggernaut/worker:$TAG"
    fi
  else
    SWAP_CID="$(docker create "chuggernaut/worker:$TAG")"
    docker cp "$SWAP_CID:/usr/local/bin/chuggernaut" "$SWAP_STAGE/chuggernaut"
    if [ -n "$SERVES_CONTAINER" ]; then
      docker cp "$SWAP_CID:/usr/local/lib/chuggernaut/chuggernaut-channel" "$SWAP_STAGE/chuggernaut-channel"
      SWAP_CHANNEL_FROM="chuggernaut/worker:$TAG"
    fi
    # On LINUX the two executors coincide — the node's kernel IS the container's
    # — so both copies are the same bytes out of the same image. Staged twice
    # under the two names because they are two artifacts with two guards, not one
    # file with two links.
    if [ -n "$SERVES_HOST" ]; then
      docker cp "$SWAP_CID:/usr/local/lib/chuggernaut/chuggernaut-channel" "$SWAP_STAGE/chuggernaut-channel-host"
      SWAP_HOST_CHANNEL_FROM="chuggernaut/worker:$TAG"
    fi
    docker cp "$SWAP_CID:/usr/local/lib/chuggernaut/worker-refresh.sh" "$SWAP_STAGE/worker-refresh.sh"
    docker rm "$SWAP_CID" > /dev/null
    SWAP_CID=""
    SWAP_FROM="chuggernaut/worker:$TAG"
  fi
  SWAP_FILES="chuggernaut worker-refresh.sh"
  [ -z "$SERVES_CONTAINER" ] || SWAP_FILES="$SWAP_FILES chuggernaut-channel"
  [ -z "$SERVES_HOST" ] || SWAP_FILES="$SWAP_FILES chuggernaut-channel-host"
  # shellcheck disable=SC2086
  for f in $SWAP_FILES; do
    if [ ! -s "$SWAP_STAGE/$f" ]; then
      echo "worker-refresh: '$f' came out of $SWAP_FROM empty or not at all — refusing to install a daemon that cannot run; REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
      exit 1
    fi
  done
  # And it must RUN HERE, on both platforms, before anything is renamed into
  # place. That is the generalisation of the finding rather than a mac special
  # case: a binary from a foreign platform installs perfectly and then loops
  # under the supervisor, which is a node out of the fleet with no scripted way
  # back (design #440 D1).
  chmod +x "$SWAP_STAGE/chuggernaut"
  if ! "$SWAP_STAGE/chuggernaut" --version > /dev/null 2>&1; then
    echo "worker-refresh: the daemon binary from $SWAP_FROM does not run on this node (chuggernaut --version failed: a foreign architecture, a missing dynamic loader, or a broken build) — installing it would leave the supervisor restarting a binary that cannot exec; REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
    exit 1
  fi

  # And the CHANNEL BINARY is asked the question ITS executor asks, which is a
  # different one: it is injected into agent containers, so on a mac the right
  # answer is that it does NOT run here. `chuggernaut-channel` takes no
  # `--version` either — it is an MCP server that reads its job context out of
  # the environment — so on Linux what is asked is whether the kernel would load
  # it at all (126/127), and on Darwin its object header is read instead: ELF
  # magic, and the e_machine of the architecture THIS NODE'S DOCKER runs, derived
  # from that docker rather than assumed. This refusal has no visible symptom to
  # fall back on: an injected binary that cannot exec leaves the MCP server
  # `pending` and the agent without `submit_eval`, which surfaces jobs later as
  # "the evaluator produced no output" (#477, #478).
  # Not asked at all on a host-only node, which staged no such file: the question
  # is "can the container this file is injected into exec it", and there is no
  # such container.
  #
  # Defined out here because BOTH channel guards read an object header and there
  # is one way to do it: `od` is POSIX and on both platforms, and e_machine is
  # two little-endian bytes at file offset 18.
  swap_magic() { od -A n -v -t x1 -j "$2" -N "$3" "$1" 2> /dev/null | tr -d '[:space:]'; }
  [ -z "$SERVES_CONTAINER" ] || chmod +x "$SWAP_STAGE/chuggernaut-channel"
  if [ -n "$SERVES_CONTAINER" ] && [ "$SUPERVISOR" = launchd ]; then
    SWAP_DOCKER_PLATFORM="$(docker version --format '{{.Server.Arch}}/{{.Server.Os}}' 2> /dev/null | tr -d '[:space:]' || true)"
    case "$SWAP_DOCKER_PLATFORM" in
      arm64/linux | aarch64/linux) SWAP_ELF_MACHINE=b700 ;;
      amd64/linux | x86_64/linux) SWAP_ELF_MACHINE=3e00 ;;
      *)
        echo "worker-refresh: cannot read this node's container platform ('docker version --format {{.Server.Arch}}/{{.Server.Os}}' answered '${SWAP_DOCKER_PLATFORM:-<nothing>}') — the swap needs it to tell a usable chuggernaut-channel binary from one no agent container can exec, and guessing is how a Mach-O reached the air in the first place (#477, #478); REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
        exit 1
        ;;
    esac
    SWAP_CHAN_MAGIC="$(swap_magic "$SWAP_STAGE/chuggernaut-channel" 0 4)"
    SWAP_CHAN_MACHINE="$(swap_magic "$SWAP_STAGE/chuggernaut-channel" 18 2)"
    if [ "$SWAP_CHAN_MAGIC" != 7f454c46 ] || [ "$SWAP_CHAN_MACHINE" != "$SWAP_ELF_MACHINE" ]; then
      echo "worker-refresh: the chuggernaut-channel binary from $SWAP_CHANNEL_FROM is not a Linux ELF for $SWAP_DOCKER_PLATFORM (magic ${SWAP_CHAN_MAGIC:-none}, e_machine ${SWAP_CHAN_MACHINE:-none}; wanted 7f454c46 / $SWAP_ELF_MACHINE) — this mac never execs that file, it injects it into every agent container, where a binary that cannot exec leaves the chuggernaut-channel MCP server pending forever and the agent without update_status or submit_eval (#477, #478); REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
      exit 1
    fi
  elif [ -n "$SERVES_CONTAINER" ]; then
    if "$SWAP_STAGE/chuggernaut-channel" < /dev/null > /dev/null 2>&1; then SWAP_CHAN_RC=0; else SWAP_CHAN_RC=$?; fi
    if [ "$SWAP_CHAN_RC" = 126 ] || [ "$SWAP_CHAN_RC" = 127 ]; then
      echo "worker-refresh: the chuggernaut-channel binary from $SWAP_CHANNEL_FROM cannot be executed on this node (exit $SWAP_CHAN_RC: a foreign architecture, a missing dynamic loader, or a broken build) — the daemon injects that file into every agent container, where it would leave the chuggernaut-channel MCP server pending forever and the agent without update_status or submit_eval (#477, #478); REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
      exit 1
    fi
  fi

  # And the HOST channel binary is asked the question ITS executor asks, which is
  # the third one on this node: the agent CLI spawns it as a stdio MCP server in
  # this node's own process tree for a host agent task (design #490 D2), so THIS
  # NODE execs it and the node is what runs it here — on both platforms, unlike
  # the injected copy above, whose real question a mac cannot ask at all. On a mac
  # the object header is read first, and it is the mirror image of that guard
  # rather than a second dialect of it: an ELF in the host slot is the OTHER half
  # of the pair (#480), and exec alone would refuse it with "a foreign
  # architecture, a missing dynamic loader, or a broken build" — sending the
  # operator after a toolchain problem that is not there.
  if [ -n "$SERVES_HOST" ]; then
    chmod +x "$SWAP_STAGE/chuggernaut-channel-host"
    if [ "$SUPERVISOR" = launchd ] \
      && [ "$(swap_magic "$SWAP_STAGE/chuggernaut-channel-host" 0 4)" = 7f454c46 ]; then
      echo "worker-refresh: the host chuggernaut-channel binary from $SWAP_HOST_CHANNEL_FROM is a Linux ELF, and this mac execs it ITSELF — the agent CLI spawns it as a stdio MCP server in this node's own process tree (design #490 D2), so it must be the Mach-O this node's cargo compiled. An ELF here is the INJECTED copy in the host slot, which is the other half of the same pair (#480) and belongs at $CHANNEL_BIN; REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
      exit 1
    fi
    if "$SWAP_STAGE/chuggernaut-channel-host" < /dev/null > /dev/null 2>&1; then SWAP_HOST_CHAN_RC=0; else SWAP_HOST_CHAN_RC=$?; fi
    if [ "$SWAP_HOST_CHAN_RC" = 126 ] || [ "$SWAP_HOST_CHAN_RC" = 127 ]; then
      echo "worker-refresh: the host chuggernaut-channel binary from $SWAP_HOST_CHANNEL_FROM cannot be executed on this node (exit $SWAP_HOST_CHAN_RC: a foreign architecture, a missing dynamic loader, or a broken build) — THIS NODE execs that file for a host agent task, so a copy that will not load leaves the task with no update_status and no submit_result at all (design #490 D2); REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
      exit 1
    fi
  fi

  # Installed by RENAME, and that is load-bearing twice over: writing over
  # $DAEMON_BIN in place is ETXTBSY while the daemon is executing it, and
  # truncating THIS SCRIPT under the shell that is reading it feeds the shell the
  # tail of a different file. A rename swaps the directory entry and leaves both
  # open inodes alone. The temp name sits beside the target so the rename stays
  # within one filesystem. Each of the three writes escalates to `sudo -n` the
  # way build-worker.sh's `chug_dir`/`chug_put` do, because /usr/local is root's
  # and the daemon need not be; the pre-flight above has already established that
  # one of the two arms can write here.
  swap_install() {
    _dir="$(dirname "$2")"
    [ -d "$_dir" ] || mkdir -p "$_dir" 2> /dev/null || sudo -n mkdir -p "$_dir"
    install -m 0755 "$1" "$2.chug-new" 2> /dev/null || sudo -n install -m 0755 "$1" "$2.chug-new"
    mv -f "$2.chug-new" "$2" 2> /dev/null || sudo -n mv -f "$2.chug-new" "$2"
  }
  refresh_phase "swap-install"
  swap_install "$SWAP_STAGE/chuggernaut" "$DAEMON_BIN"
  [ -z "$SERVES_CONTAINER" ] || swap_install "$SWAP_STAGE/chuggernaut-channel" "$CHANNEL_BIN"
  [ -z "$SERVES_HOST" ] || swap_install "$SWAP_STAGE/chuggernaut-channel-host" "$HOST_CHANNEL_BIN"
  swap_install "$SWAP_STAGE/worker-refresh.sh" "$REFRESH_SCRIPT"
  if [ -n "$SERVES_CONTAINER" ]; then
    echo "worker-refresh: installed $DAEMON_BIN and $REFRESH_SCRIPT from $SWAP_FROM, and $CHANNEL_BIN from $SWAP_CHANNEL_FROM (the agent containers exec that one, not this node)"
  else
    echo "worker-refresh: installed $DAEMON_BIN and $REFRESH_SCRIPT from $SWAP_FROM; no $CHANNEL_BIN, because this node names no container runtime and that file is only ever injected into an agent container"
  fi
  if [ -n "$SERVES_HOST" ]; then
    echo "worker-refresh: installed $HOST_CHANNEL_BIN from $SWAP_HOST_CHANNEL_FROM — this node serves host launches and execs that one ITSELF, which is why it is a second file rather than the injected copy (design #490 D2)"
  fi

  # The staging dir is dead the moment the three renames land, and it must go
  # HERE rather than in the trap: the restart below kills this shell's cgroup,
  # and POSIX sh runs no EXIT trap when it is killed by a signal (the same reason
  # the build phase carries its own TERM handler). Left behind it is tens of MB
  # of binaries per refresh on a node whose docker-disk headroom is the subject
  # of the longest note in this file. The trap stays as the failure-path backstop.
  swap_cleanup
  # ── ask the supervisor to restart ────────────────────────────────────────────
  # Said BEFORE the restart is requested, because after it there is no guarantee
  # this process gets another scheduling quantum: the daemon is what relays these
  # lines to the dispatcher, and the restart is what ends it.
  #
  # `--no-block` queues the job and returns instead of waiting on a restart that
  # includes killing the caller (this shell is in the unit's cgroup). launchd's
  # `kickstart -k` is the same act on the other platform and is what the macOS
  # D3 proof exercised. Neither is a self-replacement problem the way `docker rm
  # -f` of one's own container was: the supervisor performs both halves.
  refresh_phase "swap-restart"
  case "$SUPERVISOR" in
    systemd)
      echo "worker-refresh: swap -> chuggernaut/worker:$TAG on ${WORKER_NODE:-?}: asking systemd to restart $SWAP_UNIT; this daemon exits with it and Restart=always brings up the new binary (what follows is in 'journalctl -u ${SWAP_UNIT%.service}')"
      systemctl restart --no-block "$SWAP_UNIT"
      ;;
    launchd)
      echo "worker-refresh: swap -> chuggernaut/worker:$TAG on ${WORKER_NODE:-?}: asking launchd to restart $SWAP_AGENT_LABEL; this daemon exits with it and KeepAlive brings up the new binary (what follows is in the agent's StandardOutPath)"
      launchctl kickstart -k "gui/$(id -u)/$SWAP_AGENT_LABEL"
      ;;
  esac
  ;;

*)
  echo "worker-refresh: unknown phase '$PHASE' (want build|swap)" >&2
  exit 2
  ;;
esac
