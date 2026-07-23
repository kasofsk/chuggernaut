#!/bin/sh
# Shell test for worker-refresh.sh — no Docker, no git server, no NATS.
#
# It drives worker-refresh.sh with fake `git` and `docker` on PATH that just log
# their invocations, then asserts the build phase builds the three node images
# and the swap phase schedules a DETACHED sibling that recreates chug-worker on
# the new tag (spec §3.1). This locks the script's contract — the phase split,
# the three images, the self-replace shape — without any real infrastructure.
#
# Run:  deploy/prod/worker-refresh.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/worker-refresh.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
mkdir -p "$BIN"
LOG="$WORK/calls.log"

# Fake git: `archive` writes a tiny tar to stdout so the pipe into `docker
# build` has input; `rev-parse FETCH_HEAD` echoes $FAKE_FETCH_HEAD so a case can
# simulate the remote HEAD matching (or not) the requested SHA; everything else
# is a no-op. Logs the full argv so we can assert the fetch targets HEAD (a ref)
# and never a raw SHA the ssh front would reject.
cat > "$BIN/git" <<EOF
#!/bin/sh
# Skip a leading "-C <dir>".
if [ "\$1" = "-C" ]; then shift 2; fi
echo "git \$*" >> "$LOG"
case "\$1" in
  archive)   printf 'FAKE-TAR' ;;
  rev-parse) echo "\${FAKE_FETCH_HEAD:-abc123}" ;;
esac
exit 0
EOF

# Fake docker: log the full argv (so we can assert on `build -t <image>` and the
# detached swapper's inner command), consume any piped stdin. `inspect` answers
# with the real host bind Source the swap phase recovers instead of re-deriving
# $HOME — keys under the node login user's home, socket at the usual path.
cat > "$BIN/docker" <<EOF
#!/bin/sh
cat >/dev/null 2>&1 || true
echo "docker \$*" >> "$LOG"
case "\$*" in
  inspect*data/keys*)   echo "/home/worksalot/chuggernaut-worker/keys" ;;
  inspect*docker.sock*) echo "/var/run/docker.sock" ;;
esac
# Build-failure injection: FAIL_BUILD names an image (e.g. agent-rust) whose
# \`docker build -t chuggernaut/<img>:...\` should fail, to exercise the atomic
# refresh guarantee — a build that dies part-way must leave the live tags
# untouched (no retag-swap reached).
if [ -n "\${FAIL_BUILD:-}" ]; then
  case "\$*" in
    build*chuggernaut/\$FAIL_BUILD:*) exit 1 ;;
  esac
fi
exit 0
EOF

chmod +x "$BIN/git" "$BIN/docker"

# A real git key file: the build phase now validates its presence before any
# docker mutation, so the success cases must point WORKER_GIT_KEY at a file that
# exists.
KEY="$WORK/key"
: > "$KEY"

fail() { echo "FAIL: $1" >&2; exit 1; }
grep_log() { grep -qF "$1" "$LOG" || fail "expected in log: $1"; }

# ── Case 1: build fetches an advertised ref + builds temp tags + retag-swaps ──
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  FAKE_FETCH_HEAD=abc123 \
  sh "$SUT" build abc123 prod

grep_log "git fetch"
# Fetch HEAD (a ref the ssh front advertises), NOT a raw SHA: the front only
# enables uploadpack.allowFilter, so `want <sha>` would be refused.
grep -F "fetch" "$LOG" | grep -qF "HEAD" || fail "build must fetch the advertised HEAD ref"
if grep -F "fetch" "$LOG" | grep -qF "abc123"; then
  fail "build must not fetch a raw SHA (ssh front rejects want <sha>)"
fi
grep_log "git rev-parse FETCH_HEAD"   # verifies HEAD resolves to the requested SHA
# Atomic refresh: the three images build to TEMP tags first...
grep_log "docker build -q -t chuggernaut/worker:prod-refresh"
grep_log "docker build -q -t chuggernaut/agent:prod-refresh"
grep_log "docker build -q -t chuggernaut/agent-rust:prod-refresh"
# ...then retag-swap onto the live tag only after all three succeed.
grep_log "docker tag chuggernaut/worker:prod-refresh chuggernaut/worker:prod"
grep_log "docker tag chuggernaut/agent:prod-refresh chuggernaut/agent:prod"
grep_log "docker tag chuggernaut/agent-rust:prod-refresh chuggernaut/agent-rust:prod"
# The live tag is NEVER built onto directly — a failed build must never leave a
# half-swapped live image, so builds only ever target the temp tag.
if grep -F "docker build" "$LOG" | grep -qE 'chuggernaut/(worker|agent|agent-rust):prod([[:space:]]|$)'; then
  fail "build must target temp tags, never build directly onto the live :prod tag"
fi
echo "ok: build verifies SHA, builds temp tags, then atomically retag-swaps to :prod"

# ── Case 1b: build refuses when remote HEAD != requested SHA (no wrong build) ──
: > "$LOG"
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$WORK/key" \
     FAKE_FETCH_HEAD=deadbeef \
     sh "$SUT" build abc123 prod 2>/dev/null; then
  fail "build should fail when remote HEAD does not match the requested SHA"
fi
if grep -qF "docker build" "$LOG"; then
  fail "build must not build any image when the SHA verify fails (no wrong-tree image)"
fi
echo "ok: build refuses when remote HEAD != requested SHA (drift stays, no wrong build)"

# ── Case 2: build without a git URL is rejected before any docker mutation ────
# The refresh must validate its config FIRST: no git URL ⇒ no build, no retag —
# the node's existing images are left exactly as they were (the incident: a
# refresh that mutated before it validated stranded the node with no images).
: > "$LOG"
if PATH="$BIN:$PATH" sh "$SUT" build abc123 prod 2>/dev/null; then
  fail "build should fail when WORKER_REFRESH_GIT_URL is unset"
fi
if grep -qE "docker (build|tag)" "$LOG"; then
  fail "missing git URL must be rejected before any docker mutation (images untouched)"
fi
echo "ok: build without WORKER_REFRESH_GIT_URL is rejected before any docker mutation"

# ── Case 2b: build with a missing git key is rejected before any mutation ─────
: > "$LOG"
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$WORK/absent-key" \
     sh "$SUT" build abc123 prod 2>/dev/null; then
  fail "build should fail when the git key file is missing"
fi
if grep -qE "docker (build|tag)" "$LOG"; then
  fail "missing git key must be rejected before any docker mutation (images untouched)"
fi
echo "ok: build with a missing git key is rejected before any docker mutation"

# ── Case 2c: a build that fails mid-way never retag-swaps the live tag ─────────
# FAIL_BUILD makes the agent-rust build fail after worker+agent already built to
# their temp tags. Because the retag-swap onto :prod runs only after ALL three
# temp builds succeed, a mid-way failure must leave every live :prod tag intact.
: > "$LOG"
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$KEY" \
     FAKE_FETCH_HEAD=abc123 \
     FAIL_BUILD=agent-rust \
     sh "$SUT" build abc123 prod 2>/dev/null; then
  fail "build should fail when one image build fails"
fi
if grep -qF "docker tag" "$LOG"; then
  fail "a failed build must not reach the retag-swap (live :prod images stay intact)"
fi
echo "ok: a build that fails mid-way leaves the live :prod images untouched"

# ── Case 3: swap schedules a detached sibling that recreates chug-worker ──────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod

# The swapper is detached (`run -d --rm`) so `docker rm -f chug-worker` can't
# kill it, and it recreates the daemon on the new tag.
grep_log "docker run -d --rm"
grep_log "docker rm -f chug-worker"
grep_log "chuggernaut/worker:prod"
grep_log "WORKER_NODE=nuc"

# The replacement must mount the REAL host keys source recovered from the live
# container — NOT a $HOME-derived path. Re-deriving $HOME inside the swapper
# (HOME=/root) would bind an empty dir and strand the daemon without NATS creds.
grep_log "/home/worksalot/chuggernaut-worker/keys:/data/keys:ro"
if grep -qF '$HOME' "$LOG"; then
  fail "swap must not mount a \$HOME-derived keys path (strands creds in the swapper)"
fi
echo "ok: swap schedules a detached self-replace mounting the real keys source"

# ── Case 3b: swap carries WORKER_CACHE_DIR forward (env only, no daemon mount) ─
# The refreshed daemon must inherit the node-local build cache config (#55/#82):
# a refresh that dropped WORKER_CACHE_DIR would silently un-warm the cache. It is
# passed as ENV only — the daemon binds the cache into sibling job containers via
# the docker socket, so the daemon container needs no cache mount of its own.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  sh "$SUT" swap prod

grep_log "WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache"
# ENV only: the replacement daemon must NOT bind the cache dir into itself.
if grep -qF -- "-v /var/cache/chuggernaut/sccache:/var/cache/chuggernaut/sccache" "$LOG"; then
  fail "swap must pass WORKER_CACHE_DIR as env only, not a daemon bind-mount"
fi
echo "ok: swap carries WORKER_CACHE_DIR forward as env (no daemon mount)"

# ── Case 4: unknown phase is a hard error ────────────────────────────────────
if PATH="$BIN:$PATH" sh "$SUT" frobnicate 2>/dev/null; then
  fail "unknown phase should exit non-zero"
fi
echo "ok: unknown phase rejected"

echo "ALL PASS"
