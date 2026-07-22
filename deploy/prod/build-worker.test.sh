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
# daemon `docker run`.
cat > "$BIN/ssh" <<EOF
#!/bin/sh
cat >/dev/null 2>&1 || true
echo "ssh \$*" >> "$LOG"
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
echo "ok: no WORKER_CACHE_DIR passed when unset"

# ── Case 3: no worker node ⇒ clean no-op ──────────────────────────────────────
: > "$LOG"
PATH="$BIN:$PATH" sh "$SUT"
if [ -s "$LOG" ]; then
  fail "build-worker.sh must no-op (no ssh) when WORKER_SSH is unset"
fi
echo "ok: no-op when WORKER_SSH unset"

echo "ALL PASS"
