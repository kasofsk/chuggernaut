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
#   WORKER_DAEMON_BIN / WORKER_CHANNEL_BINARY / WORKER_REFRESH_SCRIPT
#     where the swap installs the three artifacts it extracts from the new worker
#     image; the defaults are the paths build-worker.sh installs to and the ones
#     crates/worker/src/config.rs already defaults to
#   WORKER_UNIT / WORKER_AGENT_LABEL   what the swap asks the supervisor to restart
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
DISK_PATH="${WORKER_REFRESH_DISK_PATH:-/}"
# BuildKit cache cap for the prune pair. 15G was sized for the #115 cache mounts
# when agent-rust compiled the whole workspace into one; #352 deleted that build,
# leaving only the worker image's release target plus the shared registry/git
# caches, so the ceiling comes down to keep those hot and nothing else. A cap
# below what is live evicts the coldest entries — a slower next build, never a
# wrong one.
BUILDER_KEEP_STORAGE="8GB"

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
    return 0
  fi
  _reclaimed_kb=$(( _after_kb - _before_kb ))
  [ "$_reclaimed_kb" -ge 0 ] || _reclaimed_kb=0
  echo "worker-refresh: pruned after $_why: reclaimed $(refresh_disk_gb "$_reclaimed_kb")GB ($(refresh_disk_gb "$_before_kb")GB -> $(refresh_disk_gb "$_after_kb")GB free on $DISK_PATH)"
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
  refresh_disk_preflight || exit 1

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
    docker rmi -f "chuggernaut/worker:$NEW" "chuggernaut/agent:$NEW" "chuggernaut/agent-rust:$NEW" >/dev/null 2>&1 || true
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
  git -C "$TMP" archive --format=tar FETCH_HEAD > "$TMP/worker.tar"
  [ -s "$TMP/worker.tar" ] || { echo "worker-refresh: empty worker context — aborting (live images untouched)" >&2; exit 1; }
  BUILD_STARTED=1
  refresh_phase "build-image 1/3 worker"
  docker build -q -t "chuggernaut/worker:$NEW" \
      -f deploy/prod/Dockerfile.worker --build-arg "CHUG_GIT_SHA=$SHA" \
      --label "chug.git.sha=$SHA" - < "$TMP/worker.tar"

  # Agent images the job types run in.
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

  # Positively assert the freshly built worker image carries the requested SHA
  # label BEFORE the retag-swap flips the live tag onto it. A build whose label
  # is missing or wrong (stale layer, silent buildx failure) must never become
  # the live image — refuse the swap; the trap drops the temp tags and the live
  # images stay exactly as they were.
  refresh_phase "verify-label"
  GOT_LABEL="$(docker inspect --format '{{index .Config.Labels "chug.git.sha"}}' "chuggernaut/worker:$NEW" 2>/dev/null | tr -d '[:space:]' || true)"
  if [ "$GOT_LABEL" != "$SHA" ]; then
    echo "worker-refresh: built worker image label '$GOT_LABEL' != requested $SHA — refusing retag-swap (live images untouched)" >&2
    exit 1
  fi

  # All three built to completion — retag-swap onto the live tag. `docker tag`
  # is local and instant, so the live images flip to the new build only now,
  # and only after we know every image is buildable.
  refresh_phase "retag-swap"
  docker tag "chuggernaut/worker:$NEW"     "chuggernaut/worker:$TAG"
  docker tag "chuggernaut/agent:$NEW"      "chuggernaut/agent:$TAG"
  docker tag "chuggernaut/agent-rust:$NEW" "chuggernaut/agent-rust:$TAG"

  # Bound the node's docker disk after every refresh (the 2026-07-23 air
  # ENOSPC incident): the retag-swap just stranded the previous generation as
  # dangling — prune those (NEVER -a: live tags must survive, #183) and cap
  # the BuildKit cache at $BUILDER_KEEP_STORAGE. Report the build's own disk
  # cost first, while the generation it built is still unpruned.
  refresh_disk_report_build_cost
  refresh_phase "prune"
  refresh_disk_prune "a successful refresh"

  echo "worker-refresh: built chuggernaut/{worker,agent,agent-rust}:$TAG ($SHA)"
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
  for _p in "$DAEMON_BIN" "$CHANNEL_BIN" "$REFRESH_SCRIPT"; do
    _d="$(dirname "$_p")"
    while [ ! -d "$_d" ]; do _d="$(dirname "$_d")"; done
    if [ -w "$_d" ] || sudo -n test -w "$_d" 2> /dev/null; then continue; fi
    echo "worker-refresh: cannot install '$_p' — neither this daemon's user nor 'sudo -n' can write '$_d', the nearest directory that exists; REFUSING swap (live daemon untouched, job containers untouched, images already built). Re-apply the node's spec with deploy/prod/build-worker.sh from the operator's laptop, or grant this user passwordless sudo on the node." >&2
    exit 1
  done

  # ── extract, then install (design #440 D6) ───────────────────────────────────
  # The binary comes OUT OF THE IMAGE the build phase just made, never from a
  # compile on the node: that keeps its build environment byte-identical to the
  # containerized daemon's, needs no Rust toolchain as a node machine fact, and
  # leaves deploy/prod/Dockerfile.worker the single definition of how the binary
  # is produced. This is the same `docker create` + `docker cp` pair
  # build-worker.sh runs over ssh — one extraction, two callers.
  refresh_phase "swap-extract"
  SWAP_STAGE="$(mktemp -d)"
  SWAP_CID=""
  swap_cleanup() {
    [ -z "$SWAP_CID" ] || docker rm -f "$SWAP_CID" > /dev/null 2>&1 || true
    rm -rf "$SWAP_STAGE"
  }
  trap 'RC=$?; swap_cleanup; exit "$RC"' EXIT
  SWAP_CID="$(docker create "chuggernaut/worker:$TAG")"
  docker cp "$SWAP_CID:/usr/local/bin/chuggernaut" "$SWAP_STAGE/chuggernaut"
  docker cp "$SWAP_CID:/usr/local/lib/chuggernaut/chuggernaut-channel" "$SWAP_STAGE/chuggernaut-channel"
  docker cp "$SWAP_CID:/usr/local/lib/chuggernaut/worker-refresh.sh" "$SWAP_STAGE/worker-refresh.sh"
  docker rm "$SWAP_CID" > /dev/null
  SWAP_CID=""
  for f in chuggernaut chuggernaut-channel worker-refresh.sh; do
    if [ ! -s "$SWAP_STAGE/$f" ]; then
      echo "worker-refresh: '$f' came out of chuggernaut/worker:$TAG empty or not at all — refusing to install a daemon that cannot run; REFUSING swap (live daemon untouched, the node stays one generation behind)" >&2
      exit 1
    fi
  done

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
  swap_install "$SWAP_STAGE/chuggernaut-channel" "$CHANNEL_BIN"
  swap_install "$SWAP_STAGE/worker-refresh.sh" "$REFRESH_SCRIPT"
  echo "worker-refresh: installed $DAEMON_BIN, $CHANNEL_BIN and $REFRESH_SCRIPT from chuggernaut/worker:$TAG"

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
