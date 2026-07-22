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

# 1. Native build of the host binaries (dispatcher + api — the api is the same
#    `chuggernaut` binary, now run natively under launchd instead of in a
#    container, so this build is the only place its code is compiled).
cargo build --release

# 2. SSH-front image + linux channel binary (build.sh). No api/agent images
#    build here anymore: the api runs natively (step 6b) and job containers run
#    only on worker nodes, which build their own agent images (step 3).
CHUG_IMAGE_TAG="${CHUG_IMAGE_TAG:-prod}" deploy/prod/build.sh

# Load prod config for the steps below.
set -a
. deploy/prod/chuggernaut.env
set +a

# 2b. Build the web SPA on the host and seed the served UI dir. The native api
#     serves UI_DIST from UI_ROOT (run-api.sh); web-publish jobs rsync new
#     content into the same dir for instant swaps (README §7), so a full deploy
#     must land the same content. node is a Mini prerequisite (README §0).
#     Contents are replaced in place (never the directory — the api reads it
#     live and web-publish rsyncs into it).
UI_ROOT="${UI_ROOT:-$HOME/chuggernaut-data/ui}"
export UI_ROOT
mkdir -p "$UI_ROOT"
( cd web && npm ci && npm run build )
rsync -a --delete web/dist/ "$UI_ROOT/"

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
if [ -z "${WORKER_SSH:-}" ] && [ -n "${DOCKER_NODES:-}" ]; then
  echo "$DOCKER_NODES" | tr ',' '\n' | while IFS='|' read -r wn wep _wslots; do
    wn="$(echo "$wn" | tr -d '[:space:]')"
    wep="$(echo "$wep" | tr -d '[:space:]')"
    [ "$wep" = "worker" ] || continue
    echo "update: requesting self-refresh of worker '$wn' -> $TARGET_SHA"
    # The command's stdout streams into the deploy task log, so its outcome is
    # ALWAYS visible: `refresh requested` / `refresh already in progress` / or,
    # critically, `node <name>: refresh SKIPPED — no git credential` when the
    # node lacks WORKER_REFRESH_GIT_URL / its key (spec §3.1). That loud skip is
    # what stops a credential-less node from making a deploy look like a success
    # that refreshed nothing (#114); the fleet snapshot's per-node version (#109)
    # is the cross-check. A non-zero exit (unreachable NATS etc.) is the fallback.
    target/release/chuggernaut admin worker-refresh \
      --nats-url "${NATS_URL:-nats://localhost:4222}" \
      --keys-dir "$KEYS_DIR" \
      --node "$wn" --sha "$TARGET_SHA" --tag "${CHUG_IMAGE_TAG:-prod}" \
      || echo "update: WARNING worker '$wn' refresh request errored (non-fatal)"
  done
fi

# 4. Idempotent init — creates only missing keys (e.g. a newly-added age key).
target/release/chuggernaut init --keys-dir "$KEYS_DIR" --repos-root "$REPOS_ROOT"

# 5. Rebuild + restart the ssh front (the only container whose code ships here;
#    nats runs unchanged, brought up by boot.sh).
GIT_UID="$(id -u)" docker compose -f deploy/prod/compose.yaml up -d --build ssh

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
if deploy/prod/restart-verify.sh "$TARGET_SHA" "$PREV_SHA"; then
  # Only record success once the dispatcher has genuinely come up on the new SHA.
  echo "$TARGET_SHA" > "$MARK"
  echo "update: deployed $TARGET_SHA OK"
else
  rc=$?
  # restart-verify.sh already printed the story (rolled back, or shouting for
  # help). Leave .deployed-sha untouched: prod is NOT on $TARGET_SHA.
  echo "update: deploy of $TARGET_SHA FAILED health check (exit $rc)" >&2
  exit "$rc"
fi
