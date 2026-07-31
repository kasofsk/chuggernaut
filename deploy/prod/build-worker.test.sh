#!/bin/sh
# Shell test for build-worker.sh — no Docker, no ssh, no worker node.
#
# It drives build-worker.sh with fake `ssh` and `git` on PATH that just log
# their invocations, then asserts the daemon `docker run` (the last ssh command)
# carries the full worker env forward — in particular WORKER_CACHE_DIR (the #55
# dormant-cache fix), the self-refresh coordinates, and the node identity — so a
# hand-run or scripted (re)deploy never starts a daemon with caching or refresh
# silently dropped. Same spirit as worker-refresh.test.sh / restart-verify.test.sh.
#
# Run:  deploy/prod/build-worker.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/build-worker.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
mkdir -p "$BIN"
LOG="$WORK/calls.log"

# Fake git: `rev-parse HEAD` echoes a fixed SHA; `archive` writes a tiny tar to
# stdout so the pipe into the (faked) ssh `docker build` has input; everything
# else is a no-op.
cat > "$BIN/git" <<EOF
#!/bin/sh
case "\$1" in
  rev-parse) echo "deadbeefcafe" ;;
  archive)   printf 'FAKE-TAR' ;;
esac
exit 0
EOF

# Fake ssh: consume any piped stdin (git archive) and log the full argv, so the
# assertions can inspect the remote command strings — including the multi-line
# daemon `docker run`. It also answers the two verification probes the script now
# runs over ssh:
#   * the image-label inspect (`...chug.git.sha...`) echoes $FAKE_LABEL, default
#     the SHA fake-git reports, so the label assert passes unless a case forces a
#     mismatch (the stale-image-label case);
#   * the daemon health probe (`...State.Running...`) echoes HEALTHY unless
#     $FAIL_PROBE is set, so a case can drive the probe to time out.
cat > "$BIN/ssh" <<EOF
#!/bin/sh
cat >/dev/null 2>&1 || true
echo "ssh \$*" >> "$LOG"
case "\$*" in
  *chug.git.sha*)  echo "\${FAKE_LABEL:-deadbeefcafe}" ;;
  *State.Running*) [ -n "\${FAIL_PROBE:-}" ] || echo HEALTHY ;;
esac
exit 0
EOF

chmod +x "$BIN/git" "$BIN/ssh"

fail() { echo "FAIL: $1" >&2; exit 1; }
grep_log() { grep -qF "$1" "$LOG" || fail "expected in log: $1"; }

# ── Case 1: cache on ⇒ the daemon run passes WORKER_CACHE_DIR (env only) ───────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY=/data/keys/worker_git \
  sh "$SUT"

# The daemon (re)start carries the full env forward.
grep_log "docker run -d --restart=always --name chug-worker"
grep_log "WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache"
grep_log "WORKER_REFRESH_GIT_URL=ssh://git@front:2222/acme/chug.git"
grep_log "WORKER_GIT_KEY=/data/keys/worker_git"
grep_log "WORKER_NODE=nuc"
# A daemon started with no RUST_LOG logs at ERROR only (ticket #270): no "worker
# up", no refresh relay, so `docker logs chug-worker` is silent about the one
# thing an operator needs it for. The node-creation path sets the same floor the
# self-refresh swap carries forward.
grep_log "RUST_LOG=info,async_nats=warn"
# Env only: no cache bind-mount into the daemon container itself.
if grep -qF -- "-v /var/cache/chuggernaut/sccache:" "$LOG"; then
  fail "WORKER_CACHE_DIR must be env only on the daemon, not a bind-mount"
fi
echo "ok: daemon run carries WORKER_CACHE_DIR (env only) + refresh coords + node"

# ── Case 2: cache unset ⇒ no WORKER_CACHE_DIR passed (caching stays off) ───────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"

grep_log "docker run -d --restart=always --name chug-worker"
if grep -qF "WORKER_CACHE_DIR" "$LOG"; then
  fail "WORKER_CACHE_DIR must not be passed when unset (caching stays off)"
fi
# Likewise the disk pre-flight knobs: unset ⇒ nothing passed, so the refresh uses
# the conservative default documented in worker-refresh.sh.
if grep -qF "WORKER_REFRESH_DISK_" "$LOG"; then
  fail "WORKER_REFRESH_DISK_* must not be passed when unset (documented default applies)"
fi
# And the capacity knob: unset ⇒ the daemon's own default of 4 applies.
if grep -qF "WORKER_SLOTS" "$LOG"; then
  fail "WORKER_SLOTS must not be passed when unset (daemon default applies)"
fi
echo "ok: no WORKER_CACHE_DIR, WORKER_REFRESH_DISK_* or WORKER_SLOTS passed when unset"

# Also assert the label + health verification happened on the success path.
grep_log "chug.git.sha=deadbeefcafe"                 # SHA baked as an image LABEL
grep_log "docker inspect --format"                   # label read back for the assert
grep -F "ssh" "$LOG" | grep -qF "State.Running"      # daemon health probe ran
echo "ok: success path bakes + verifies the image label and probes daemon health"

# ── Case 2b: disk pre-flight knobs reach the daemon's environment (deploy #248) ─
# worker-refresh.sh's pre-flight threshold is documented as env-overridable, and
# node creation is the only place an operator can set it — a knob the daemon
# never receives is an inert knob. (worker-refresh.test.sh covers the other half:
# the swap phase carrying it forward so a self-refresh does not drop it.) The
# value deliberately differs from worker-refresh.sh's built-in default, so a run
# path that hardcoded the default instead of forwarding the operator's value
# still fails.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_REFRESH_DISK_FREE_GB_MIN=45 \
  WORKER_REFRESH_DISK_PATH=/var/lib/docker \
  sh "$SUT"

grep_log "WORKER_REFRESH_DISK_FREE_GB_MIN=45"
grep_log "WORKER_REFRESH_DISK_PATH=/var/lib/docker"
echo "ok: daemon run carries the disk pre-flight knobs when set"

# ── Case 2c: the node's capacity reaches the daemon (WORKER_SLOTS) ─────────────
# The daemon announces its OWN slot count and that announcement wins over the
# dispatcher's DOCKER_NODES seed (spec §3.1), so node creation is the only place
# a node can be capped below the daemon's default 4 — air at 2. A knob the daemon
# never receives is an inert knob, and the node would silently keep running 4
# concurrent job containers. (worker-refresh.test.sh covers the other half: the
# swap carrying it forward so a self-refresh does not restore the default.)
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_SLOTS=2 \
  sh "$SUT"

grep_log "WORKER_SLOTS=2"
grep_log "WORKER_NODE=air"
echo "ok: daemon run carries WORKER_SLOTS when set"

# ── Case 3: no worker node ⇒ clean no-op ──────────────────────────────────────
: > "$LOG"
PATH="$BIN:$PATH" sh "$SUT"
if [ -s "$LOG" ]; then
  fail "build-worker.sh must no-op (no ssh) when WORKER_SSH is unset"
fi
echo "ok: no-op when WORKER_SSH unset"

# ── Case 4: stale image label ⇒ REFUSE the daemon restart ─────────────────────
# If the built worker image's chug.git.sha label does not match the requested
# SHA (a stale layer / silently failed build), the script must refuse to (re)start
# the daemon onto it and exit non-zero — the live daemon stays put.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_LABEL=staleSHA000 \
  sh "$SUT" >/dev/null 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "stale image label must fail the build (got rc=0)"
if grep -qF "docker run -d --restart=always --name chug-worker" "$LOG"; then
  fail "stale image label must REFUSE the daemon restart (docker run must not have happened)"
fi
echo "ok: stale image label refuses the daemon restart (non-zero, daemon untouched)"

# ── Case 5: daemon never reports healthy ⇒ probe times out, loud failure ──────
# The label matches (build ok) but the daemon does not come up (FAIL_PROBE): the
# health probe must time out and exit non-zero — never a silent 'deployed'.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAIL_PROBE=1 \
  PROBE_TIMEOUT_SECS=0 PROBE_INTERVAL_SECS=0 \
  sh "$SUT" >"$WORK/probe.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unhealthy daemon must fail the deploy (got rc=0)"
grep -qF "did NOT report healthy" "$WORK/probe.out" || fail "probe timeout must be loud"
grep -qF "deployed" "$WORK/probe.out" && fail "must not print a 'deployed' message when the probe times out"
echo "ok: unhealthy daemon times out loudly (non-zero, no 'deployed' claim)"

echo "ALL PASS"
