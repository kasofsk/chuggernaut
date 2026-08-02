#!/bin/sh
# Worker self-refresh (spec §3.1) — runs ON the worker node, invoked by the
# `chuggernaut worker` daemon when a `refresh` RPC arrives. The dispatcher host
# cannot ssh a tagged worker (Tailscale blocks tagged->tagged), so control is
# inverted: the daemon fetches the build context itself over the existing ssh
# front and rebuilds its three node images, then swaps itself.
#
# Two phases, so the daemon can quiesce launches only for the brief swap window:
#   worker-refresh.sh build <sha> <tag>   # fetch context + build 3 images
#   worker-refresh.sh swap  <tag>         # replace the daemon container
#
# All external tools (git, ssh, docker) come from PATH so the shell test can
# fake them (worker-refresh.test.sh). Config is env-driven, inherited from the
# daemon's own environment:
#   WORKER_REFRESH_GIT_URL  ssh://git@<ssh-front>:2222/<owner>/<repo>.git (required)
#   WORKER_GIT_KEY          ssh private key for the node credential (default /data/keys/worker_git)
#   WORKER_NODE, NATS_URL, NATS_CREDS   passed through to the replacement daemon
#   WORKER_CACHE_DIR        optional node-local build cache (re-applied on swap;
#     the host dir is provisioned at node creation by build-worker.sh, never here)
#   WORKER_SWAP_IMAGE       docker-cli image for the detached swapper (default docker:cli)
#   WORKER_REFRESH_DISK_FREE_GB_MIN / _DISK_PATH   disk pre-flight (see below)
#   WORKER_KVM / WORKER_KVM_PROJECTS / WORKER_ANDROID_SDK_DIR   KVM passthrough
#     (design #367), re-applied on swap; the DEVICE itself is carried forward
#     from the live container, never from this env (docs/runbooks/worker-kvm.md)
#   WORKER_NIX_GCROOTS_DIR / WORKER_NIX_CLIENT / WORKER_NIX_DAEMON_SOCKET /
#     WORKER_NIX_STORE_DIR /
#     WORKER_NIX_REALISE_TIMEOUT_SECS   per-task nix GC roots (design #373 P1),
#     re-applied on swap; the nix MOUNTS are carried forward from the live
#     container, never re-derived here
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
# different disk shape can tune it: build-worker.sh puts the knob in the
# daemon's environment at node creation and the swap phase below carries it
# forward, so an override survives self-refreshes instead of reverting to this
# default on the next one.
DISK_FREE_GB_MIN="${WORKER_REFRESH_DISK_FREE_GB_MIN:-30}"
# Where the space is measured. The build runs inside the worker container, whose
# root filesystem is an overlay on the filesystem that backs /var/lib/docker —
# statfs on it reports exactly the pool images and the BuildKit cache compete
# for.
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
  # The daemon runs INSIDE chug-worker, so it cannot `docker rm -f chug-worker`
  # itself — that would kill this process mid-swap. Instead launch a DETACHED
  # sibling container (its own lifecycle) that removes the old daemon and starts
  # the new one. Job containers are untouched: `docker rm -f` only hits
  # chug-worker, and the dispatcher's poll-based wait re-attaches (spec §3.1).
  # The replacement carries the SAME env the daemon runs with (inherited here).
  NODE="${WORKER_NODE:?WORKER_NODE must be set}"
  NATS="${NATS_URL:?NATS_URL must be set}"
  CREDS="${NATS_CREDS:-/data/keys/worker.creds}"
  SWAP_IMAGE="${WORKER_SWAP_IMAGE:-docker:cli}"

  # Optional node-local build cache, re-applied so caching survives the swap.
  # ENV ONLY, exactly like build-worker.sh's daemon run: the daemon adds the
  # cache bind to each sibling job container via the docker socket (host path),
  # so the DAEMON container needs no cache mount of its own. Carrying this
  # forward is what stops a refresh from silently dropping caching (#55/#82).
  #
  # Carried forward, never CREATED — deliberately, on two counts. This phase
  # runs INSIDE chug-worker, which mounts only the docker socket and the keys
  # dir, so a `mkdir -p` here would land in the daemon container's writable
  # layer and never on the host: it is the very bug this variable already had
  # (crates/worker/src/daemon.rs's `create_dir_all`), not a fix for it. And a
  # swap runs on a node build-worker.sh already provisioned, so a dir missing at
  # this point means something removed it — which the engine's launch refusal
  # should say out loud (#379), not a defensive create that hides it.
  CACHE_ARGS=""
  if [ -n "${WORKER_CACHE_DIR:-}" ]; then
    CACHE_ARGS="-e WORKER_CACHE_DIR=$WORKER_CACHE_DIR"
  fi

  # Same reasoning for the disk pre-flight knobs (deploy #248). A node whose disk
  # shape needs a threshold other than the built-in default sets them on the
  # daemon; if the swap dropped them, the very next self-refresh would silently
  # revert that node to the default — the #55/#82 failure mode again. Unset adds
  # nothing, so a stock node keeps the documented constant.
  DISK_ARGS=""
  if [ -n "${WORKER_REFRESH_DISK_FREE_GB_MIN:-}" ]; then
    DISK_ARGS="-e WORKER_REFRESH_DISK_FREE_GB_MIN=$WORKER_REFRESH_DISK_FREE_GB_MIN"
  fi
  if [ -n "${WORKER_REFRESH_DISK_PATH:-}" ]; then
    DISK_ARGS="$DISK_ARGS -e WORKER_REFRESH_DISK_PATH=$WORKER_REFRESH_DISK_PATH"
  fi

  # The node's FIRST-BOOT capacity (`WORKER_SLOTS`, spec §3.1). Same silent-revert
  # class as the two above: this is the number the replacement daemon comes back
  # reporting, so a swap that dropped it would restore a node deliberately sized
  # at 2 to the default 4 — more concurrent job containers than the node was
  # sized for. The dispatcher does reconcile its recorded intent back onto the
  # node within a scan tick, but that is a repair after the fact and it only
  # happens where an operator has ever set a number; carrying the boot value
  # forward is what keeps the node correct in the gap, and correct at all on a
  # node whose dispatcher is down. Capacity is CHANGED from the operator UI, not
  # here (docs/runbooks/worker-capacity.md). Unset adds nothing, so a stock node
  # keeps the default.
  SLOTS_ARGS=""
  if [ -n "${WORKER_SLOTS:-}" ]; then
    SLOTS_ARGS="-e WORKER_SLOTS=$WORKER_SLOTS"
  fi

  # KVM passthrough (design #367, daemon side shipped by #374). The three
  # SETTINGS are carried the same pass-from-env way as every knob above: this
  # phase runs INSIDE chug-worker, so whatever `docker run -e WORKER_KVM=…` put
  # on the daemon at node creation is already in this shell's environment, and
  # inspecting for them would only re-read what we can read directly. The DEVICE
  # is different in kind and is recovered from the live container below — see
  # KVM_DEVICE_ARGS. Values are single-quoted for the swapper's shell, matching
  # build-worker.sh, so an allow-list written with spaces cannot word-split.
  #
  # $KVM_ON is whether this node actually HAS a device, resolved exactly as the
  # daemon resolves it (`parse_kvm_device`: trim, then 0/false/off ⇒ no device).
  # A node deliberately set to WORKER_KVM=0 runs with the setting and no device
  # — build-worker.sh composes precisely that — so the device refusal below must
  # key off the values that attach one, not off the var being set at all.
  # Nothing validates the SHAPE here: an unparseable value could never have
  # booted the live daemon, so anything else on this node means KVM is on.
  KVM="${WORKER_KVM:-}"
  KVM="${KVM#"${KVM%%[![:space:]]*}"}"
  KVM="${KVM%"${KVM##*[![:space:]]}"}"
  KVM_ON=""
  case "$KVM" in
    '' | 0 | false | off) ;;
    *) KVM_ON=1 ;;
  esac
  KVM_ARGS=""
  if [ -n "$KVM" ]; then
    KVM_ARGS="-e WORKER_KVM='$KVM'"
  fi
  if [ -n "${WORKER_KVM_PROJECTS:-}" ]; then
    KVM_ARGS="$KVM_ARGS -e WORKER_KVM_PROJECTS='$WORKER_KVM_PROJECTS'"
  fi
  if [ -n "${WORKER_ANDROID_SDK_DIR:-}" ]; then
    KVM_ARGS="$KVM_ARGS -e WORKER_ANDROID_SDK_DIR='$WORKER_ANDROID_SDK_DIR'"
  fi

  # Recover the REAL host bind sources from the running daemon rather than
  # reconstructing them from $HOME. build-worker.sh mounts the keys from the
  # node login user's `\$HOME/chuggernaut-worker/keys` (expanded in the node's
  # ssh shell), but the swap runs `RUN_NEW` inside the detached `docker:cli`
  # swapper where HOME=/root — re-deriving $HOME there would bind an empty
  # `/root/chuggernaut-worker/keys` and strand the new daemon without NATS creds.
  # This phase runs inside chug-worker (docker.sock mounted), so inspect the
  # live container for the literal Source paths and bake them into RUN_NEW.
  KEYS_SRC="$(docker inspect chug-worker \
    --format '{{range .Mounts}}{{if eq .Destination "/data/keys"}}{{.Source}}{{end}}{{end}}')"
  SOCK_SRC="$(docker inspect chug-worker \
    --format '{{range .Mounts}}{{if eq .Destination "/var/run/docker.sock"}}{{.Source}}{{end}}{{end}}')"
  if [ -z "$KEYS_SRC" ]; then
    echo "worker-refresh: no /data/keys mount on chug-worker; refusing swap (would strand creds)" >&2
    exit 1
  fi
  SOCK_SRC="${SOCK_SRC:-/var/run/docker.sock}"

  # The KVM device, recovered the same inspect-the-live-container way as the
  # mounts above, and for a sharper reason. A device is a `docker run` flag, not
  # an env var, so it cannot ride the daemon's environment into this shell; and
  # dropping it while carrying WORKER_KVM forward does not degrade the node, it
  # REMOVES it — the replacement daemon refuses to start on a device its own view
  # lacks (crates/worker/src/daemon.rs `build_backend`), --restart=always
  # restarts it into the same refusal, and the node leaves the fleet on the first
  # self-refresh after an operator enabled KVM. Reading `.HostConfig.Devices` off
  # what is ACTUALLY running means the carry-forward cannot drift from the live
  # daemon the way a second copy of the run spec would.
  KVM_DEVICE_ARGS="$(docker inspect chug-worker \
    --format '{{range .HostConfig.Devices}}--device {{.PathOnHost}}:{{.PathInContainer}}:{{.CgroupPermissions}} {{end}}')"
  if [ -n "$KVM_ON" ] && [ -z "$KVM_DEVICE_ARGS" ]; then
    echo "worker-refresh: WORKER_KVM='$KVM' enables KVM but the live chug-worker has no device to carry forward; refusing swap (the replacement would refuse to start and --restart=always would loop it — node down). Recreate the daemon with deploy/prod/build-worker.sh, which passes --device." >&2
    exit 1
  fi

  # Per-task nix GC roots (design #373 P1). The four SETTINGS ride the daemon's
  # environment into this shell like every other knob; the MOUNTS cannot,
  # and are recovered from the live container for the same reason the KVM device
  # is. Dropping them does not degrade the node: the daemon's boot check refuses
  # to start without its roots dir, its client and the socket in ITS OWN view
  # (crates/worker/src/nix.rs), --restart=always loops the refusal, and the node
  # leaves the fleet — the #377 node-down hazard, one mount over.
  #
  # Carried by DESTINATION rather than reconstructed: anything under /nix, the
  # store prefix, the roots dir, and — when this node attaches a KVM device —
  # the DIRECTORY HOLDING the toolchain path (never that path itself: a bind
  # whose source is the operator's symlink resolves it host-side and the client
  # then sees a non-store path it refuses to realise),
  # with each mount's own read-only bit preserved (the
  # store, the profiles and the toolchain are :ro; the socket dir and the roots
  # dir must stay writable — connecting to a unix socket needs write on the
  # inode). Reading what is ACTUALLY running is what keeps this from drifting
  # the way a second copy of the run spec would, and it carries a node whose
  # profiles or socket live on a different mount point (gumbo-nuc-0's
  # /nix/var/nix/gcroots/profiles resolves through /mnt) with no path assumption
  # here at all.
  NIX_GCROOTS="${WORKER_NIX_GCROOTS_DIR:-}"
  NIX_GCROOTS="${NIX_GCROOTS#"${NIX_GCROOTS%%[![:space:]]*}"}"
  NIX_GCROOTS="${NIX_GCROOTS%"${NIX_GCROOTS##*[![:space:]]}"}"
  NIX_ARGS=""
  NIX_MOUNT_ARGS=""
  if [ -n "$NIX_GCROOTS" ]; then
    NIX_ARGS="-e WORKER_NIX_GCROOTS_DIR='$NIX_GCROOTS'"
    if [ -n "${WORKER_NIX_CLIENT:-}" ]; then
      NIX_ARGS="$NIX_ARGS -e WORKER_NIX_CLIENT='$WORKER_NIX_CLIENT'"
    fi
    if [ -n "${WORKER_NIX_DAEMON_SOCKET:-}" ]; then
      NIX_ARGS="$NIX_ARGS -e WORKER_NIX_DAEMON_SOCKET='$WORKER_NIX_DAEMON_SOCKET'"
    fi
    if [ -n "${WORKER_NIX_REALISE_TIMEOUT_SECS:-}" ]; then
      NIX_ARGS="$NIX_ARGS -e WORKER_NIX_REALISE_TIMEOUT_SECS='$WORKER_NIX_REALISE_TIMEOUT_SECS'"
    fi
    if [ -n "${WORKER_NIX_STORE_DIR:-}" ]; then
      NIX_ARGS="$NIX_ARGS -e WORKER_NIX_STORE_DIR='$WORKER_NIX_STORE_DIR'"
    fi
    NIX_SOCKET_DIR="${WORKER_NIX_DAEMON_SOCKET:-/nix/var/nix/daemon-socket/socket}"
    NIX_SOCKET_DIR="${NIX_SOCKET_DIR%/*}"
    NIX_STORE_DIR="${WORKER_NIX_STORE_DIR:-/nix/store}"
    NIX_TOOLCHAIN_DIR=""
    if [ -n "$KVM_ON" ]; then
      NIX_SDK_DIR="${WORKER_ANDROID_SDK_DIR:-/var/lib/chuggernaut/android-sdk}"
      NIX_SDK_DIR="${NIX_SDK_DIR#"${NIX_SDK_DIR%%[![:space:]]*}"}"
      NIX_SDK_DIR="${NIX_SDK_DIR%"${NIX_SDK_DIR##*[![:space:]]}"}"
      NIX_TOOLCHAIN_DIR="${NIX_SDK_DIR%/*}"
    fi
    HAVE_STORE=""
    HAVE_ROOTS=""
    HAVE_SOCKET=""
    HAVE_TOOLCHAIN=""
    for m in $(docker inspect chug-worker \
      --format '{{range .Mounts}}{{.Destination}}|{{.Source}}|{{.RW}} {{end}}'); do
      dst="${m%%|*}"
      rest="${m#*|}"
      src="${rest%%|*}"
      rw="${rest##*|}"
      case "$dst" in
        /nix | /nix/*) ;;
        "$NIX_STORE_DIR") ;;
        "$NIX_GCROOTS") ;;
        *)
          if [ -z "$NIX_TOOLCHAIN_DIR" ] || [ "$dst" != "$NIX_TOOLCHAIN_DIR" ]; then continue; fi
          ;;
      esac
      if [ "$rw" = "true" ]; then
        NIX_MOUNT_ARGS="$NIX_MOUNT_ARGS -v $src:$dst"
      else
        NIX_MOUNT_ARGS="$NIX_MOUNT_ARGS -v $src:$dst:ro"
      fi
      if [ "$dst" = "$NIX_STORE_DIR" ]; then HAVE_STORE=1; fi
      if [ "$dst" = "$NIX_GCROOTS" ]; then HAVE_ROOTS=1; fi
      if [ "$dst" = "$NIX_SOCKET_DIR" ]; then HAVE_SOCKET=1; fi
      if [ -n "$NIX_TOOLCHAIN_DIR" ] && [ "$dst" = "$NIX_TOOLCHAIN_DIR" ]; then HAVE_TOOLCHAIN=1; fi
    done
    nix_mount_missing() {
      echo "worker-refresh: WORKER_NIX_GCROOTS_DIR='$NIX_GCROOTS' enables per-task nix GC roots but the live chug-worker has no mount at '$1' to carry forward; refusing swap (the replacement would refuse to start and --restart=always would loop it — node down). Recreate the daemon with deploy/prod/build-worker.sh, which passes every nix mount." >&2
      exit 1
    }
    if [ -z "$HAVE_STORE" ]; then nix_mount_missing "$NIX_STORE_DIR"; fi
    if [ -z "$HAVE_ROOTS" ]; then nix_mount_missing "$NIX_GCROOTS"; fi
    if [ -z "$HAVE_SOCKET" ]; then nix_mount_missing "$NIX_SOCKET_DIR"; fi
    if [ -n "$NIX_TOOLCHAIN_DIR" ] && [ -z "$HAVE_TOOLCHAIN" ]; then nix_mount_missing "$NIX_TOOLCHAIN_DIR"; fi
  fi

  # The daemon's own log level (ticket #270). The binary configures tracing from
  # RUST_LOG (`tracing_subscriber::fmt::init`) whose default directive is ERROR,
  # so a daemon started without it emits NOTHING an operator can use: not the
  # refresh phase markers, not the per-line relay of THIS script's output
  # (`worker-refresh: <line>`, daemon.rs), not "worker up". That silence is why
  # the deploy #267 post-mortem had to be reconstructed from docker event ring
  # buffers. `info` is exactly the level those lines live at, and it is not a
  # firehose: the daemon logs nothing per-op (launch/inspect/logs/ping are
  # silent), and `docker build -q` keeps a ten-minute image build to one line per
  # phase. Dependencies stay at warn so an async-nats reconnect storm cannot
  # drown the refresh story. Carried forward like the other knobs so an operator
  # who raised the level on the live daemon keeps it across self-refreshes
  # (#55/#82's silent-revert lesson).
  RUST_LOG_NEW="${RUST_LOG:-info,async_nats=warn}"

  # The refreshed worker image already contains the (new SHA's) worker-refresh.sh
  # and daemon binary, so no script is mounted from the host. All bind sources
  # below are literal host paths (already expanded), so evaluating RUN_NEW inside
  # the swapper reproduces build-worker.sh's exact mounts.
  RUN_NEW="docker run -d --restart=always --name chug-worker \
    -v $SOCK_SRC:/var/run/docker.sock \
    -v $KEYS_SRC:/data/keys:ro \
    -e WORKER_NODE=$NODE -e NATS_URL=$NATS -e NATS_CREDS=$CREDS \
    -e RUST_LOG=$RUST_LOG_NEW \
    -e WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-} \
    -e WORKER_GIT_KEY=${WORKER_GIT_KEY:-/data/keys/worker_git} \
    $CACHE_ARGS $DISK_ARGS $SLOTS_ARGS $KVM_ARGS $KVM_DEVICE_ARGS \
    $NIX_ARGS $NIX_MOUNT_ARGS chuggernaut/worker:$TAG"

  # Keep the swapper's transcript (ticket #270). This sibling container holds the
  # only record of the moment the node is most likely to break — it removes the
  # live daemon and starts its replacement — and it used to run `--rm`, so a
  # `$RUN_NEW` that failed took its own error message with it seconds later,
  # leaving a node with neither a daemon nor a reason. Named and retained, that
  # transcript survives as `docker logs chug-worker-swap` (and, on a journald
  # node, `journalctl CONTAINER_NAME=chug-worker-swap`). Bounded to ONE retained
  # container per node: each swap force-removes the previous one by name first.
  #
  # It cannot ride back into the deploy job's output: the daemon that reports to
  # the dispatcher is the very thing being replaced. That is why the node-side
  # record has to survive at all.
  SWAP_NAME="chug-worker-swap"
  docker rm -f "$SWAP_NAME" >/dev/null 2>&1 || true

  # sleep briefly so this RPC's reply flushes before the old daemon is removed.
  # The inner `docker rm -f chug-worker` keeps its STDERR (only the removed-id
  # echo is dropped): its failure — a docker socket that stopped answering, a
  # container that will not die — is the first thing to know when the replacement
  # never appears. Still non-fatal, exactly as before: the swap proceeds.
  refresh_phase "swap-container"
  docker run -d --name "$SWAP_NAME" \
    -v /var/run/docker.sock:/var/run/docker.sock \
    "$SWAP_IMAGE" \
    sh -c "sleep 2; docker rm -f chug-worker >/dev/null || echo 'worker-refresh: docker rm -f chug-worker failed — continuing to start the replacement' >&2; $RUN_NEW"

  echo "worker-refresh: swap scheduled -> chuggernaut/worker:$TAG on $NODE (RUST_LOG=$RUST_LOG_NEW; swapper transcript: docker logs $SWAP_NAME)"
  ;;

*)
  echo "worker-refresh: unknown phase '$PHASE' (want build|swap)" >&2
  exit 2
  ;;
esac
