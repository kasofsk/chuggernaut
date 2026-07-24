#!/bin/sh
# Chuggernaut deploy workhorse — build the target commit natively and restart
# the host services, idempotently. Invoked over ssh by a `deploy` job's
# tasks/deploy.sh (which passes the released SHA), and runnable by hand.
#
# It operates on the DEPLOYED checkout ($CHUG_REPO), NOT on wherever this script
# happens to be invoked from — the deploy job's container/checkout is a
# different directory from the one launchd runs the binary out of.
#
# The checkout's `origin` is now the local bare repo (HEAD == main), so with no
# explicit ref we deploy whatever `origin/main` points at.
#
# Usage: update.sh [ref]        (ref defaults to origin/main)
set -eu

# Deploy jobs reach this script over non-interactive ssh, whose PATH lacks
# homebrew (docker/colima) and cargo. Set it explicitly so the script behaves
# identically from any caller: runner, ssh, or an interactive shell.
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin"

# refresh_workers — request a self-refresh of every worker node in DOCKER_NODES
# and WAIT for confirmation (spec §3.1). Extracted as a function so it is unit
# testable (update-refresh.test.sh) with a stubbed chuggernaut binary.
#
# The `admin worker-refresh` CLI always exits 0 — every outcome (not accepted,
# SKIPPED for no git credential, already-in-progress, and even "not confirmed
# within the wait window") returns Ok and prints its story to stdout; ONLY a
# confirmed swap prints "refresh OK:" (admin.rs). So we cannot trust the exit
# code: we pass --wait-secs 90 to actually RUN the confirm loop and treat the
# absence of a "refresh OK:" line as a FAILED deploy step. No more
# `|| echo WARNING` masking a refresh that never landed (#186).
#
# Returns 0 iff every worker node confirmed onto $TARGET_SHA; non-zero otherwise.
# Reads TARGET_SHA, DOCKER_NODES, NATS_URL, KEYS_DIR, CHUG_IMAGE_TAG from the env;
# the chuggernaut binary is $CHUG_BIN (default target/release/chuggernaut).
# --- structured-leg helpers (ticket #187) ------------------------------------
# Pure emit helpers, defined above the CHUG_UPDATE_LIB gate so the test harness
# (and refresh_workers, which emits per-node legs) can use them when sourced.
# The leg STATE and the exit trap live in the execution section further down.
chug_json_str() {
  # Minimal, JSON-safe scalar: drop backslashes/quotes, flatten whitespace, cap.
  printf '%s' "$1" | tr '\n\r\t' '   ' | tr -d '\\"' | cut -c1-200
}

chug_emit_leg() {
  # chug_emit_leg NAME STATUS [SECS] [ERROR] [DETAIL]
  _l="{\"name\":\"$1\",\"status\":\"$2\""
  [ -n "${3:-}" ] && _l="$_l,\"secs\":$3"
  [ -n "${4:-}" ] && _l="$_l,\"error\":\"$(chug_json_str "$4")\""
  [ -n "${5:-}" ] && _l="$_l,\"detail\":\"$(chug_json_str "$5")\""
  echo "@chug:leg $_l}"
}

chug_leg_drop() {
  _new=""
  for _p in $CHUG_LEGS_PENDING; do
    [ "$_p" = "$1" ] || _new="$_new $_p"
  done
  CHUG_LEGS_PENDING="$_new"
}

leg_begin() {
  CHUG_LEG_NAME="$1"
  CHUG_LEG_START="$(date +%s)"
}

leg_ok() {
  [ -n "$CHUG_LEG_NAME" ] || return 0
  chug_emit_leg "$CHUG_LEG_NAME" ok "$(( $(date +%s) - CHUG_LEG_START ))"
  chug_leg_drop "$CHUG_LEG_NAME"
  CHUG_LEG_NAME=""
}


refresh_workers() {
  [ -n "${DOCKER_NODES:-}" ] || return 0
  bin="${CHUG_BIN:-target/release/chuggernaut}"
  # A real refresh REBUILDS the node's three images before the daemon swap —
  # minutes even warm (the nuc took ~7 on 2026-07-23), not seconds. 90s would
  # fail every honest deploy mid-build; default to 15min, overridable per
  # deploy (WORKER_REFRESH_WAIT_SECS) for fleets with hotter caches.
  wait_secs="${WORKER_REFRESH_WAIT_SECS:-900}"
  # Main-shell iteration (no pipeline): chug_leg_drop mutates
  # CHUG_LEGS_PENDING, and a `| while` subshell would discard those drops —
  # the EXIT trap would then re-mark already-emitted worker-refresh legs as
  # skipped (#207 review).
  for _entry in $(printf '%s' "$DOCKER_NODES" | tr ',' ' '); do
    wn="$(printf '%s' "$_entry" | cut -d'|' -f1 | tr -d '[:space:]')"
    wep="$(printf '%s' "$_entry" | cut -d'|' -f2 | tr -d '[:space:]')"
    [ "$wep" = "worker" ] || continue
    echo "update: requesting self-refresh of worker '$wn' -> $TARGET_SHA (waiting up to ${wait_secs}s for confirmation)"
    _wr_start="$(date +%s)"
    out="$("$bin" admin worker-refresh \
      --nats-url "${NATS_URL:-nats://localhost:4222}" \
      --keys-dir "$KEYS_DIR" \
      --node "$wn" --sha "$TARGET_SHA" --tag "${CHUG_IMAGE_TAG:-prod}" \
      --wait-secs "$wait_secs" 2>&1)" || true
    printf '%s\n' "$out"   # stream the CLI's story into the deploy task log
    if printf '%s\n' "$out" | grep -q 'refresh OK:'; then
      chug_emit_leg "worker-refresh:$wn" ok "$(( $(date +%s) - _wr_start ))"
      chug_leg_drop "worker-refresh:$wn"
      echo "update: worker '$wn' confirmed on $TARGET_SHA"
    else
      # Harvest the daemon-captured failure tail the CLI prints on a FAILED
      # refresh (deploy #212), so the leg's `detail` carries the real cause
      # (e.g. docker disk pressure), not just "refresh not confirmed".
      _wr_detail="$(printf '%s\n' "$out" | sed -n 's/^worker-refresh-detail: //p' | head -1)"
      chug_emit_leg "worker-refresh:$wn" failed "$(( $(date +%s) - _wr_start ))" \
        "refresh not confirmed" "$_wr_detail"
      chug_leg_drop "worker-refresh:$wn"
      echo "update: worker '$wn' refresh NOT confirmed on $TARGET_SHA — FAILING deploy (not a warning)" >&2
      return 1
    fi
  done
  return 0
}


# When sourced by the test harness (CHUG_UPDATE_LIB set), stop here: the helper
# functions above are all the test wants — none of the deploy side effects below.
[ -n "${CHUG_UPDATE_LIB:-}" ] && return 0

CHUG_REPO="${CHUG_REPO:-$HOME/chuggernaut}"   # the deployed checkout
TARGET_REF="${1:-origin/main}"

# Pre-bootstrap guard: before the Mini has been set up (README §1) there is no
# deployed checkout / config to act on. Skip cleanly (exit 0) rather than fail
# the deploy — the first real deploy takes over once bootstrap is done.
if [ ! -d "$CHUG_REPO/.git" ]; then
  echo "update: $CHUG_REPO not bootstrapped yet — see deploy/prod/README.md §1; skipping"
  exit 0
fi
if [ ! -f "$CHUG_REPO/deploy/prod/chuggernaut.env" ]; then
  echo "update: deploy/prod/chuggernaut.env missing — bootstrap not complete; skipping"
  exit 0
fi

cd "$CHUG_REPO"
git fetch --quiet origin

TARGET_SHA="$(git rev-parse --verify --quiet "${TARGET_REF}^{commit}" || git rev-parse origin/main)"
MARK="$CHUG_REPO/.deployed-sha"

if [ -f "$MARK" ] && [ "$(cat "$MARK")" = "$TARGET_SHA" ]; then
  echo "update: already deployed $TARGET_SHA — nothing to do"
  exit 0
fi
echo "update: deploying $TARGET_SHA"

# gitignored files (deploy/prod/chuggernaut.env, deploy/prod/out/, .deployed-sha)
# survive a forced checkout.
git checkout --quiet --force "$TARGET_SHA"

# Rollback snapshot of the currently-running binary before we overwrite it.
# restart-verify.sh (step 6) restores this if the new build fails its health
# check. It pairs with $MARK/.deployed-sha (still the PREVIOUS sha until step 6
# succeeds), so the snapshot and the sha we roll back to always match.
if [ -f target/release/chuggernaut ]; then
  cp -f target/release/chuggernaut target/release/chuggernaut.prev
fi

# Re-exec the freshly checked-out script so the rest of the deploy runs the code
# being deployed, not the stale copy this shell started with. `git checkout
# --force` above swapped this file (and everything around it) out from under the
# running process — without this the old logic runs against the new tree.
# Bitten three times on 2026-07-21: (1) the PATH-fix deploys #23/#25 ran the
# pre-fix script; (2) deploy #35 ran the pre-UI_ROOT script against the new
# compose.yaml — ${UI_ROOT} unset → `invalid spec: :/srv/web` → deploy failed.
# Each time the retry silently worked, the worst kind of flake. Everything above
# (bootstrap guards, fetch, SHA resolution, already-deployed short-circuit,
# rollback snapshot) belongs to this first pass; everything below runs in the
# re-exec'd second pass. The guard var makes the re-exec happen exactly once, and
# we pass the RESOLVED SHA (not the original ref) so the second pass is
# deterministic.
if [ -z "${CHUG_UPDATE_REEXEC:-}" ]; then
  CHUG_UPDATE_REEXEC=1 exec "$CHUG_REPO/deploy/prod/update.sh" "$TARGET_SHA"
fi

# Bake the deployed SHA into everything built natively below: the dispatcher +
# api binaries read it via option_env!("CHUG_GIT_SHA") (cd.rs::deployed_sha, the
# api health probe), and the web bundle via its vite `define` (web/vite.config.ts,
# read from process.env.CHUG_GIT_SHA). This is what lets the cluster view show a
# short hash for each deployable. Absent locally, each degrades to a dash.
# (Worker images bake it themselves via a build-arg — build-worker.sh.)
export CHUG_GIT_SHA="$TARGET_SHA"

# --- structured deploy legs (ticket #187) ------------------------------------
# Each step below is a "leg". We time it and emit one machine-readable
# `@chug:leg {json}` line to stdout; a single `@chug:report {json}` envelope
# follows at exit. tasks/deploy.sh streams stdout back unchanged, and the
# dispatcher harvests these lines into the deploy task's structured result — so a
# deploy's outcome is a typed record, not one opaque log. Purely additive: the
# wrapped commands are unchanged, and this is confined to a few helper calls.
CHUG_LEG_NAME=""
CHUG_LEG_START=0
# Legs still to run, in order, so a failure can mark the remainder skipped. The
# per-node worker-refresh legs are emitted inline (their count is dynamic).
CHUG_LEGS_PENDING="build-dispatcher build-images web-publish init ssh-front restart-verify sha-advance"
CHUG_ROLLBACK=false
CHUG_HEALTH=""
CHUG_FROM_SHA=""
[ -f "$MARK" ] && CHUG_FROM_SHA="$(cat "$MARK")"

# On any early exit (a `set -e` abort), the in-progress leg failed and the rest
# never ran — emit that story so a failed deploy is a structured record, not a
# truncated log. Always closes with the `@chug:report` envelope.
chug_on_exit() {
  _rc=$?
  if [ "$_rc" != 0 ]; then
    # The in-flight leg (if any) failed…
    if [ -n "$CHUG_LEG_NAME" ]; then
      chug_emit_leg "$CHUG_LEG_NAME" failed "$(( $(date +%s) - CHUG_LEG_START ))" "step exited $_rc"
      chug_leg_drop "$CHUG_LEG_NAME"
      CHUG_LEG_NAME=""
    fi
    # …and everything still pending never ran. Unconditional on failure:
    # refresh_workers fails BETWEEN legs (it emits its own per-node legs, no
    # leg_begin in flight), and the old CHUG_LEG_NAME guard skipped this loop
    # entirely, silently omitting the unreached legs (#207 review, cycle 2).
    for _p in $CHUG_LEGS_PENDING; do chug_emit_leg "$_p" skipped; done
    CHUG_LEGS_PENDING=""
  fi
  _rep="{\"rollback\":$CHUG_ROLLBACK"
  [ -n "$CHUG_FROM_SHA" ] && _rep="$_rep,\"from_sha\":\"$CHUG_FROM_SHA\""
  [ -n "${TARGET_SHA:-}" ] && _rep="$_rep,\"to_sha\":\"$TARGET_SHA\""
  [ -n "$CHUG_HEALTH" ] && _rep="$_rep,\"health\":\"$CHUG_HEALTH\""
  echo "@chug:report $_rep}"
}
trap chug_on_exit EXIT

# 1. Native build of the host binaries (dispatcher + api — the api is the same
#    `chuggernaut` binary, now run natively under launchd instead of in a
#    container, so this build is the only place its code is compiled).
leg_begin build-dispatcher
cargo build --release
leg_ok

# 2. SSH-front image + linux channel binary (build.sh). No api/agent images
#    build here anymore: the api runs natively (step 6b) and job containers run
#    only on worker nodes, which build their own agent images (step 3).
leg_begin build-images
CHUG_IMAGE_TAG="${CHUG_IMAGE_TAG:-prod}" deploy/prod/build.sh
leg_ok

# Load prod config for the steps below.
set -a
. deploy/prod/chuggernaut.env
set +a

# Now that DOCKER_NODES is known, register the dynamic per-node refresh legs
# in the pending list: if an earlier leg dies, the exit trap reports these as
# skipped instead of silently omitting them (#207 review). refresh_workers
# drops each as it emits.
for _entry in $(printf '%s' "${DOCKER_NODES:-}" | tr ',' ' '); do
  _wn="$(printf '%s' "$_entry" | cut -d'|' -f1 | tr -d '[:space:]')"
  _wep="$(printf '%s' "$_entry" | cut -d'|' -f2 | tr -d '[:space:]')"
  [ "$_wep" = "worker" ] || continue
  CHUG_LEGS_PENDING="$CHUG_LEGS_PENDING worker-refresh:$_wn"
done

# 2b. Build the web SPA on the host and seed the served UI dir. The native api
#     serves UI_DIST from UI_ROOT (run-api.sh); web-publish jobs rsync new
#     content into the same dir for instant swaps (README §7), so a full deploy
#     must land the same content. node is a Mini prerequisite (README §0).
#     Contents are replaced in place (never the directory — the api reads it
#     live and web-publish rsyncs into it).
UI_ROOT="${UI_ROOT:-$HOME/chuggernaut-data/ui}"
export UI_ROOT
mkdir -p "$UI_ROOT"
leg_begin web-publish
( cd web && npm ci && npm run build )
rsync -a --delete web/dist/ "$UI_ROOT/"
leg_ok

# 3. Worker nodes: refresh to the deployed SHA (rebuild the three node images +
#    swap the daemon — job containers survive, spec §3.1).
#
#    Two paths, and prod uses (b): (a) legacy over plain SSH (build-worker.sh),
#    used only when WORKER_SSH is set (a laptop that can reach the node); no-ops
#    otherwise. (b) NO-SSH self-refresh: the dispatcher host cannot ssh a tagged
#    worker (Tailscale blocks tagged->tagged), so we invert control and REQUEST
#    refresh over the worker RPC. Each worker node in DOCKER_NODES gets a request;
#    a node that fails is a WARNING with its drift surfaced (its ping version
#    also feeds the fleet snapshot), never a deploy failure.
CHUG_IMAGE_TAG="${CHUG_IMAGE_TAG:-prod}" deploy/prod/build-worker.sh
# NO-SSH self-refresh path: request + CONFIRM a refresh of each worker node. A
# node that does not confirm on $TARGET_SHA fails the deploy (refresh_workers
# returns non-zero, aborting under `set -e`) — the deploy no longer claims
# success while a worker silently stayed on the old SHA (#186). Each node also
# emits a `worker-refresh:{node}` leg (#187) from inside refresh_workers. The
# fleet snapshot's per-node version (#109) remains the independent cross-check.
if [ -z "${WORKER_SSH:-}" ]; then
  CHUG_BIN="target/release/chuggernaut" refresh_workers
fi

# 4. Idempotent init — creates only missing keys (e.g. a newly-added age key).
leg_begin init
target/release/chuggernaut init --keys-dir "$KEYS_DIR" --repos-root "$REPOS_ROOT"
leg_ok

# 5. Rebuild + restart the ssh front (the only container whose code ships here;
#    nats runs unchanged, brought up by boot.sh).
leg_begin ssh-front
GIT_UID="$(id -u)" docker compose -f deploy/prod/compose.yaml up -d --build ssh
leg_ok

# 6. Restart the host services (dispatcher + api — one binary, two launchd
#    services; restart is safe, §3.6 reconciles in-memory state from KV) onto
#    the new build, PROVE the dispatcher answered on NATS, and roll back to the
#    previous binary if it didn't. restart-verify.sh does the health check +
#    rollback ON the Mini and ignores SIGHUP, so a bad build never leaves
#    launchd crash-looping unwatched even if this ssh session drops when the
#    deploy container is reaped (§3.6 then marks the deploy task). Its transcript
#    streams back through the ssh session into the deploy task's log. A non-zero
#    exit here fails the deploy job — but prod is already back on the old binary.
PREV_SHA="unknown"
[ -f "$MARK" ] && PREV_SHA="$(cat "$MARK")"
# restart-verify runs inside `if`, so a failure does NOT `set -e`-abort here —
# emit the restart-verify/sha-advance legs explicitly in each branch (ticket
# #187) rather than relying on the EXIT trap.
_rv_start="$(date +%s)"
if deploy/prod/restart-verify.sh "$TARGET_SHA" "$PREV_SHA"; then
  chug_emit_leg "restart-verify" ok "$(( $(date +%s) - _rv_start ))"
  chug_leg_drop "restart-verify"
  CHUG_HEALTH="ok"
  # Only record success once the dispatcher has genuinely come up on the new SHA.
  _sa_start="$(date +%s)"
  echo "$TARGET_SHA" > "$MARK"
  chug_emit_leg "sha-advance" ok "$(( $(date +%s) - _sa_start ))"
  chug_leg_drop "sha-advance"
  echo "update: deployed $TARGET_SHA OK"
else
  rc=$?
  # restart-verify.sh already printed the story (rolled back, or shouting for
  # help). Leave .deployed-sha untouched: prod is NOT on $TARGET_SHA.
  chug_emit_leg "restart-verify" failed "$(( $(date +%s) - _rv_start ))" "health check failed (exit $rc), rolled back"
  chug_leg_drop "restart-verify"
  chug_emit_leg "sha-advance" skipped
  chug_leg_drop "sha-advance"
  CHUG_ROLLBACK=true
  CHUG_HEALTH="failed"
  echo "update: deploy of $TARGET_SHA FAILED health check (exit $rc)" >&2
  exit "$rc"
fi
