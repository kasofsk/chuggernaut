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


# ── worker refresh fan-out (spec §3.1; tickets #186/#187/#253/#254) ───────────
# Request a self-refresh of every worker node in DOCKER_NODES and WAIT for
# confirmation. Split into functions so the whole flow is unit testable
# (update-refresh.test.sh) against a stubbed chuggernaut binary.
#
# PARALLEL, not serial (ticket #254). Each node rebuilds its own three images
# locally and swaps its own daemon — the builds share nothing — so walking the
# fleet serially made deploy wall-clock the SUM of the node build times
# (observed 2026-07-23: air ~11-15min + nuc ~7min ≈ 20+min) where it should be
# the MAX. Every node's refresh is requested up front and the confirmations are
# collected concurrently, each against its own per-node deadline.
#
# The `admin worker-refresh` CLI always exits 0 — every outcome (not accepted,
# SKIPPED for no git credential, already-in-progress, and even "not confirmed
# within the wait window") returns Ok and prints its story to stdout; ONLY a
# confirmed swap prints "refresh OK:" (admin.rs). So we cannot trust the exit
# code: we pass --wait-secs to actually RUN the confirm loop and treat the
# absence of a "refresh OK:" line as a FAILED deploy step. No more
# `|| echo WARNING` masking a refresh that never landed (#186).
#
# CANCEL ON FIRST FAILURE (#254). The moment one node fails there is nothing to
# win by letting the others build for another ten minutes against a deploy that
# is already failing: the remaining refreshes are cancelled (`admin
# worker-refresh --cancel`, which the daemon honours by signalling the build's
# process group). Deploy-level semantics are UNCHANGED — any unconfirmed or
# cancelled node still fails the deploy, so there is no such thing as a
# half-deploy here.
#
# VERSION SKEW, and why the swap is deliberately NOT two-phase. A failed deploy
# can leave a node already swapped onto the new images while the dispatcher
# stays on the old SHA. That window is neither new nor materially widened by the
# fan-out: on EVERY successful deploy all worker nodes swap here at step 3 while
# the dispatcher only restarts at step 6, so "workers ahead of the dispatcher"
# is a state the platform is designed to run in for minutes at a time, exercised
# by every deploy — the worker RPC is versioned for exactly that window (spec
# §3.1). A two-phase build-all-then-swap-all shape was considered and rejected:
# the live image tags flip at the END of a node's BUILD phase (worker-refresh.sh
# does the retag-swap there), so gating only the daemon swap would gate the
# smaller half of the window while forcing the staged `{tag}-refresh` images to
# survive across two RPCs — which breaks the EXIT-trap cleanup that stops a
# failed refresh from stranding a whole image generation (#248), and a deploy
# dying between the phases would strand one on every node at once. Cancelling
# early is the mitigation that actually narrows the window. A node that already
# swapped before its cancel arrived stays swapped, and its leg says so.
#
# Reads TARGET_SHA, DOCKER_NODES, NATS_URL, KEYS_DIR, CHUG_IMAGE_TAG from the
# env; the chuggernaut binary is $CHUG_BIN (default target/release/chuggernaut).
# Returns 0 iff every worker node confirmed onto $TARGET_SHA.
refresh_workers() {
  [ -n "${DOCKER_NODES:-}" ] || return 0
  CHUG_WR_BIN="${CHUG_BIN:-target/release/chuggernaut}"
  # A real refresh REBUILDS the node's three images before the daemon swap —
  # minutes even warm (the nuc took ~7 on 2026-07-23), not seconds. 90s would
  # fail every honest deploy mid-build; default to 15min, overridable per
  # deploy (WORKER_REFRESH_WAIT_SECS) for fleets with hotter caches. It is a
  # PER-NODE window, not a budget the fleet shares.
  CHUG_WR_WAIT="${WORKER_REFRESH_WAIT_SECS:-900}"
  _nodes="$(refresh_worker_nodes)"
  [ -n "$_nodes" ] || return 0
  CHUG_WR_DIR="$(mktemp -d)"
  for _n in $_nodes; do
    echo "update: requesting self-refresh of worker '$_n' -> $TARGET_SHA (waiting up to ${CHUG_WR_WAIT}s for confirmation)"
    # Stamp the start in the MAIN shell, before the waiter exists, so each leg's
    # secs is that node's own elapsed time rather than the fan-out's.
    date +%s > "$CHUG_WR_DIR/$_n.start"
    refresh_worker_one "$_n" &
    echo "$!" > "$CHUG_WR_DIR/$_n.pid"
  done
  _rc=0
  refresh_workers_collect "$_nodes" || _rc=1
  rm -rf "$CHUG_WR_DIR"
  return "$_rc"
}

# The `worker`-endpoint node names in DOCKER_NODES, one per line. One parser
# feeds both the fan-out and the pending-leg registration in the deploy body, so
# the two can never disagree about which nodes get a leg.
refresh_worker_nodes() {
  for _entry in $(printf '%s' "${DOCKER_NODES:-}" | tr ',' ' '); do
    _wn="$(printf '%s' "$_entry" | cut -d'|' -f1 | tr -d '[:space:]')"
    _wep="$(printf '%s' "$_entry" | cut -d'|' -f2 | tr -d '[:space:]')"
    [ "$_wep" = "worker" ] || continue
    echo "$_wn"
  done
  return 0
}

# One node's request + confirm wait. Runs as a BACKGROUND JOB, i.e. in a
# subshell, so it does NO leg bookkeeping: chug_leg_drop mutates
# CHUG_LEGS_PENDING and a subshell's mutations are lost, after which the EXIT
# trap would re-mark already-emitted worker-refresh legs as skipped (#207
# review). It writes its verdict to files under $CHUG_WR_DIR; the MAIN shell
# reads them and emits the legs.
#
# RELAY, don't buffer (ticket #253): `tee` passes every line through to stdout
# the instant the CLI emits it (the CLI relays the node's per-phase progress and
# 30s elapsed-time heartbeats), and keeps a copy for the confirm/detail greps.
# With the fan-out the nodes' lines interleave — every CLI line names its own
# `node=`, and each node's greps read only its own copy. The CLI always exits 0
# (#186), so the pipeline's status is ignored: only a `refresh OK:` line
# confirms.
refresh_worker_one() {
  _n="$1"
  # The CLI runs in the BACKGROUND of this subshell purely so its own pid is
  # recorded: `$!` in the caller is this waiter subshell, and killing that leaves
  # the CLI (and the `tee` reading it) running as orphans — see
  # refresh_workers_cancel, which needs the pid of the process actually holding
  # the deploy's stdout. `wait` puts the pipeline straight back.
  {
    "$CHUG_WR_BIN" admin worker-refresh \
      --nats-url "${NATS_URL:-nats://localhost:4222}" \
      --keys-dir "$KEYS_DIR" \
      --node "$_n" --sha "$TARGET_SHA" --tag "${CHUG_IMAGE_TAG:-prod}" \
      --wait-secs "$CHUG_WR_WAIT" 2>&1 &
    _cli=$!
    echo "$_cli" > "$CHUG_WR_DIR/$_n.cli"
    wait "$_cli"
  } | tee "$CHUG_WR_DIR/$_n.log" || true
  date +%s > "$CHUG_WR_DIR/$_n.end"
  # `.done` is the marker the collector polls, so it is written LAST — after the
  # transcript and the end stamp it stands for — and ATOMICALLY. A plain
  # redirect creates the file BEFORE the verdict lands in it, and a poll inside
  # that window reads an empty marker, which is not "ok": a node that confirmed
  # would be booked as failed, which under the fan-out also cancels every other
  # node still building. `mv` within the mktemp dir is a rename, so the
  # collector sees the verdict or nothing.
  if grep -q 'refresh OK:' "$CHUG_WR_DIR/$_n.log"; then
    echo ok > "$CHUG_WR_DIR/$_n.done.tmp"
  else
    echo failed > "$CHUG_WR_DIR/$_n.done.tmp"
  fi
  mv "$CHUG_WR_DIR/$_n.done.tmp" "$CHUG_WR_DIR/$_n.done"
}

# Collect the fan-out's verdicts in the MAIN shell, emitting each node's leg the
# moment it lands, and cancel whatever is still in flight as soon as one node
# fails. Returns non-zero if any node did not confirm.
#
# Bounded (STYLE): the poll gives up at the per-node wait window plus a slack
# that only has to cover a CLI which died without writing its verdict — the CLI
# bounds itself with --wait-secs, so this never becomes the deploy's real clock.
refresh_workers_collect() {
  _pending="$1"
  _why=""
  _rc=0
  _deadline=$(( $(date +%s) + CHUG_WR_WAIT + ${WORKER_REFRESH_COLLECT_SLACK_SECS:-120} ))
  while [ -n "$_pending" ]; do
    _still=""
    for _n in $_pending; do
      if [ -f "$CHUG_WR_DIR/$_n.done" ]; then
        if ! refresh_worker_leg "$_n" ""; then
          _rc=1
          [ -n "$_why" ] || _why="worker '$_n' did not confirm"
        fi
      else
        _still="$_still $_n"
      fi
    done
    _pending="$_still"
    [ -z "$_why" ] || break
    [ -n "$_pending" ] || break
    if [ "$(date +%s)" -ge "$_deadline" ]; then
      _why="the refresh collector deadline elapsed with$_pending still in flight"
      _rc=1
      break
    fi
    sleep 1
  done
  if [ -n "$_pending" ]; then
    refresh_workers_cancel "$_pending" "$_why"
    _rc=1
  fi
  return "$_rc"
}

# Cancel the refreshes still in flight, then emit their legs. MAIN SHELL (it
# calls refresh_worker_leg, which mutates CHUG_LEGS_PENDING).
refresh_workers_cancel() {
  _rest="$1"
  _cwhy="$2"
  for _n in $_rest; do
    echo "update: cancelling in-flight refresh of worker '$_n' — $_cwhy" >&2
    "$CHUG_WR_BIN" admin worker-refresh --cancel \
      --nats-url "${NATS_URL:-nats://localhost:4222}" \
      --keys-dir "$KEYS_DIR" \
      --node "$_n" --sha "$TARGET_SHA" 2>&1 | tee "$CHUG_WR_DIR/$_n.cancel" || true
  done
  # Bounded drain: a cancelled daemon reports its terminal outcome within a poll
  # or two and the waiter returns on its own; the bound covers one that does not.
  _cdeadline=$(( $(date +%s) + ${WORKER_REFRESH_CANCEL_WAIT_SECS:-60} ))
  while [ "$(date +%s)" -lt "$_cdeadline" ]; do
    _left=""
    for _n in $_rest; do
      [ -f "$CHUG_WR_DIR/$_n.done" ] || _left="$_left $_n"
    done
    [ -n "$_left" ] || break
    sleep 1
  done
  for _n in $_rest; do
    # A waiter that outlived the drain is bounded by its own --wait-secs, but
    # the deploy is over — don't leave it polling a node for another ten
    # minutes. Kill the CLI ITSELF, not just the background job: `$!` is the
    # waiter SUBSHELL, and its children — the CLI and the `tee` relaying it —
    # survive a kill of their parent, reparented and still holding this shell's
    # stdout. That stdout is the deploy's ssh session (tasks/deploy.sh runs ssh
    # without a tty), so sshd would hold the session open until the orphan's own
    # --wait-secs elapsed: the exact wall-clock this fan-out exists to remove.
    # Killing the CLI is what ends the wait; `tee` then reads EOF and goes.
    # (No process-GROUP kill here: dash refuses job control without a tty, so
    # `set -m` + `kill -- -$pid` would silently no-op under /bin/sh.)
    # Best effort by design: the leg below is emitted either way.
    if [ ! -f "$CHUG_WR_DIR/$_n.done" ]; then
      for _f in cli pid; do
        _p="$(cat "$CHUG_WR_DIR/$_n.$_f" 2>/dev/null || true)"
        [ -z "$_p" ] || kill "$_p" 2>/dev/null || true
      done
    fi
    refresh_worker_leg "$_n" "$_cwhy" || true
  done
}

# Emit one node's `worker-refresh:{node}` leg from the files its waiter left
# behind, and drop it from the pending list. MAIN SHELL ONLY: chug_leg_drop
# mutates CHUG_LEGS_PENDING (#207 review). A non-empty $2 is why this node was
# cancelled — it names the failure that aborted the fan-out. Returns non-zero
# when the node did not confirm.
refresh_worker_leg() {
  _n="$1"
  _cancel_why="$2"
  _start="$(cat "$CHUG_WR_DIR/$_n.start" 2>/dev/null || date +%s)"
  _end="$(cat "$CHUG_WR_DIR/$_n.end" 2>/dev/null || date +%s)"
  _secs=$(( _end - _start ))
  [ "$_secs" -ge 0 ] || _secs=0
  if [ "$(cat "$CHUG_WR_DIR/$_n.done" 2>/dev/null || echo failed)" = "ok" ]; then
    chug_emit_leg "worker-refresh:$_n" ok "$_secs"
    chug_leg_drop "worker-refresh:$_n"
    echo "update: worker '$_n' confirmed on $TARGET_SHA (${_secs}s)"
    return 0
  fi
  # Harvest the daemon-captured failure tail the CLI prints on a FAILED refresh
  # (deploy #212) — or, when the leg simply never confirmed, the last progress
  # it relayed (#253) — so the leg's `detail` carries the real cause (docker
  # disk pressure, or "stuck at build-image 3/3 agent-rust"), not just "refresh
  # not confirmed".
  _detail="$(sed -n 's/^worker-refresh-detail: //p' "$CHUG_WR_DIR/$_n.log" 2>/dev/null | head -1)"
  # A cancelled node is still a FAILED leg: LegStatus is ok|failed|skipped and
  # an unknown status makes the harvest DROP the leg (types::deploy), so the
  # cancellation is carried by the error/detail text, not by a fourth status.
  if [ -n "$_cancel_why" ]; then
    # What the node said when the cancel reached it — including the case that
    # matters for the skew story: "already past the swap", i.e. this node stays
    # on the NEW images while the deploy fails (spec §3.1).
    _said="$(grep -m1 '^refresh cancel' "$CHUG_WR_DIR/$_n.cancel" 2>/dev/null || true)"
    chug_emit_leg "worker-refresh:$_n" failed "$_secs" "refresh cancelled" \
      "cancelled because $_cancel_why${_said:+; $_said}${_detail:+; $_detail}"
    chug_leg_drop "worker-refresh:$_n"
    echo "update: worker '$_n' refresh CANCELLED ($_cancel_why) — FAILING deploy" >&2
    return 1
  fi
  chug_emit_leg "worker-refresh:$_n" failed "$_secs" "refresh not confirmed" "$_detail"
  chug_leg_drop "worker-refresh:$_n"
  echo "update: worker '$_n' refresh NOT confirmed on $TARGET_SHA — FAILING deploy (not a warning)" >&2
  return 1
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
for _wn in $(refresh_worker_nodes); do
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
#    refresh over the worker RPC. Every worker node in DOCKER_NODES is requested
#    AT ONCE (#254) and confirmed concurrently, so this step costs the slowest
#    node's build, not the sum of them.
CHUG_IMAGE_TAG="${CHUG_IMAGE_TAG:-prod}" deploy/prod/build-worker.sh
# NO-SSH self-refresh path: request + CONFIRM a refresh of each worker node. A
# node that does not confirm on $TARGET_SHA fails the deploy (refresh_workers
# returns non-zero, aborting under `set -e`) — the deploy no longer claims
# success while a worker silently stayed on the old SHA (#186) — and the first
# such node cancels the refreshes still building on the others (#254). Each node
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
