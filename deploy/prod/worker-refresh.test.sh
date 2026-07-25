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

# Fake df: report the free space held in $FREE_FILE (1K blocks), in the POSIX
# `df -Pk` layout the disk pre-flight parses. A case moves the number to put the
# node above or below the pre-flight threshold; the fake docker rewrites it on a
# prune so a reclaim is observable.
FREE_FILE="$WORK/free_kb"
cat > "$BIN/df" <<EOF
#!/bin/sh
echo "df \$*" >> "$LOG"
# FAKE_DF_BROKEN mimics a filesystem df cannot report on (an unknown mount, a
# stripped image): the pre-flight must then fail OPEN, not block the refresh.
[ -z "\${FAKE_DF_BROKEN:-}" ] || { echo "df: no such file or directory" >&2; exit 1; }
echo "Filesystem 1024-blocks Used Available Capacity Mounted on"
echo "/dev/vda1 104857600 0 \$(cat "$FREE_FILE") 50% /"
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
  inspect*data/keys*)     echo "/home/worksalot/chuggernaut-worker/keys" ;;
  inspect*docker.sock*)   echo "/var/run/docker.sock" ;;
  # Image-label read-back for the retag-swap guard: echo \$FAKE_LABEL (default
  # abc123, the requested SHA in the success cases) so the assert passes unless a
  # case forces a mismatch (the stale-image-label case).
  inspect*chug.git.sha*)  echo "\${FAKE_LABEL:-abc123}" ;;
  # A prune frees space: move the fake df reading to \$FREE_KB_AFTER_PRUNE so the
  # script's reclaim report is computed from a real before/after difference.
  *prune*) [ -z "\${FREE_KB_AFTER_PRUNE:-}" ] || echo "\$FREE_KB_AFTER_PRUNE" > "$FREE_FILE" ;;
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
# Cancel injection (ticket #254): FAKE_TERM_ON_BUILD names an image whose build
# SIGTERMs the script mid-flight — exactly what the daemon's \`refresh_cancel\`
# does to this script's whole process group when the deploy cancels the node.
if [ -n "\${FAKE_TERM_ON_BUILD:-}" ]; then
  case "\$*" in
    build*chuggernaut/\$FAKE_TERM_ON_BUILD:*) kill -TERM "\$PPID" ;;
  esac
fi
exit 0
EOF

chmod +x "$BIN/git" "$BIN/docker" "$BIN/df"

# Free space the fake df reports, in 1K blocks. Default ~57GB — comfortably over
# the pre-flight threshold, so the pre-existing cases are unaffected.
set_free_kb() { echo "$1" > "$FREE_FILE"; }
set_free_kb 60000000

# A real git key file: the build phase now validates its presence before any
# docker mutation, so the success cases must point WORKER_GIT_KEY at a file that
# exists.
KEY="$WORK/key"
: > "$KEY"

fail() { echo "FAIL: $1" >&2; exit 1; }
grep_log() { grep -qF "$1" "$LOG" || fail "expected in log: $1"; }
# Combined stdout+stderr of the run under test — the daemon streams exactly this
# into the deploy leg, so the disk story is asserted on it.
OUT="$WORK/out.txt"
grep_out() { grep -qF "$1" "$OUT" || fail "expected in output: $1"; }

# ── Case 1: build fetches an advertised ref + builds temp tags + retag-swaps ──
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  FAKE_FETCH_HEAD=abc123 \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1

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
# The requested SHA is baked as an image LABEL and read back before the swap.
grep_log "label chug.git.sha=abc123"
grep_log "docker inspect --format"
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

# Progress markers (ticket #253): the daemon reads these off stdout and reports
# the current one in `ping`, which is what puts per-phase progress in the deploy
# job's task output. Each long step must be ANNOUNCED BEFORE it runs — a marker
# printed after its build would relay the phase only once it was already over.
for _phase in "fetch-context" "build-image 1/3 worker" "build-image 2/3 agent" \
  "build-image 3/3 agent-rust" "verify-label" "retag-swap"; do
  grep -qF "worker-refresh: phase $_phase" "$OUT" \
    || fail "build must emit the '$_phase' progress marker"
done
echo "ok: build announces each phase before it runs (deploy-log progress markers)"

# ── Case 1a: the success path reports the disk numbers and prunes as before ───
# The pre-flight measurement is echoed (free + needed) so an operator reads the
# disk story off the deploy leg, and the post-refresh prune pair is unchanged:
# dangling images + BuildKit cache above the keep threshold, NEVER `-a`.
grep_out "disk pre-flight: 57.2GB free on /, need 20GB"
grep_log "docker image prune -f"
grep_log "docker builder prune -f --keep-storage 15GB"
grep_out "pruned after a successful refresh"
if grep -qE "prune (-a|.* -a)" "$LOG"; then
  fail "prune must never use -a (it would delete the live :prod images)"
fi
echo "ok: success path reports free/needed and keeps the safe prune pair"

# ── Case 1e: too little free space ⇒ fail FAST with the numbers, before work ──
# The 2026-07-24 (deploy #248) loop: a build with no headroom for a new image
# generation dies ten minutes in and strands the partial generation. Refuse up
# front instead, and say what is needed vs what is free.
: > "$LOG"
set_free_kb 10000000   # ~9.5GB — under the 20GB threshold
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$KEY" \
     FAKE_FETCH_HEAD=abc123 \
     sh "$SUT" build abc123 prod > "$OUT" 2>&1; then
  fail "build should fail when the docker filesystem lacks room for a generation"
fi
grep_out "need ~20GB"
grep_out "have 9.5GB free on /"
# Fail FAST: no fetch, no docker call at all — seconds, not a doomed build.
if grep -qE "git fetch|docker (build|tag|image prune|builder prune)" "$LOG"; then
  fail "the disk pre-flight must refuse before any fetch or docker call"
fi
echo "ok: insufficient space fails fast with the free/needed numbers"

# ── Case 1f: a filesystem df cannot report on fails OPEN (never blocks) ──────
# The pre-flight is a guard, not a gate: an unreadable disk shape must not stop
# a refresh that would otherwise work — it says so and proceeds.
: > "$LOG"
set_free_kb 60000000
PATH="$BIN:$PATH" \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  FAKE_FETCH_HEAD=abc123 \
  FAKE_DF_BROKEN=1 \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1 \
  || fail "a refresh must not be blocked by a filesystem df cannot report on"
grep_out "df cannot read"
grep_log "docker build -q -t chuggernaut/worker:prod-refresh"
echo "ok: disk pre-flight fails open when df cannot report"

# ── Case 1g: a CANCELLED build cleans up after itself (ticket #254) ──────────
# The parallel deploy fan-out cancels the nodes still building as soon as one
# node fails, by signalling this script's process group. POSIX sh does NOT run
# an EXIT trap when it is killed by a signal, so without the TERM handler the
# staged `-refresh` tags and the partial generation behind them would be
# stranded — the disk-pressure loop #248 closed, re-opened by cancellation.
: > "$LOG"
set_free_kb 60000000
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$KEY" \
     FAKE_FETCH_HEAD=abc123 \
     FAKE_TERM_ON_BUILD=agent-rust \
     sh "$SUT" build abc123 prod > "$OUT" 2>&1; then
  fail "a cancelled build must exit non-zero (the deploy is failing)"
fi
grep_out "cancelled — dropping staged tags"
grep_log "docker rmi -f chuggernaut/worker:prod-refresh"
grep_log "docker image prune -f"
grep_out "pruned after a failed build"
# The live images belong to the generation the node is still running: a cancel
# must leave them exactly as they were.
if grep -qF "docker tag chuggernaut/worker:prod-refresh chuggernaut/worker:prod" "$LOG"; then
  fail "a cancelled build must never retag-swap onto the live tag"
fi
echo "ok: a cancelled build drops its staged tags, prunes, and never swaps live"

# ── Case 1b: build refuses when remote HEAD != requested SHA (no wrong build) ──
: > "$LOG"
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$WORK/key" \
     FAKE_FETCH_HEAD=deadbeef \
     sh "$SUT" build abc123 prod >/dev/null 2>&1; then
  fail "build should fail when remote HEAD does not match the requested SHA"
fi
if grep -qF "docker build" "$LOG"; then
  fail "build must not build any image when the SHA verify fails (no wrong-tree image)"
fi
echo "ok: build refuses when remote HEAD != requested SHA (drift stays, no wrong build)"

# ── Case 1c: built image label != requested SHA ⇒ REFUSE the retag-swap ───────
# The three images build to their temp tags, but the worker image's baked
# chug.git.sha label does not match the requested SHA (a stale layer / silently
# failed build). The retag-swap onto the live :prod tag must be refused so a
# node keeps its working images rather than going live on the wrong build.
: > "$LOG"
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$KEY" \
     FAKE_FETCH_HEAD=abc123 \
     FAKE_LABEL=staleSHA000 \
     sh "$SUT" build abc123 prod >/dev/null 2>&1; then
  fail "build should fail when the built image label != requested SHA"
fi
if grep -qF "docker tag" "$LOG"; then
  fail "a mismatched image label must not reach the retag-swap (live :prod images stay intact)"
fi
echo "ok: build refuses the retag-swap when the image label != requested SHA"

# ── Case 2: build without a git URL is rejected before any docker mutation ────
# The refresh must validate its config FIRST: no git URL ⇒ no build, no retag —
# the node's existing images are left exactly as they were (the incident: a
# refresh that mutated before it validated stranded the node with no images).
: > "$LOG"
if PATH="$BIN:$PATH" sh "$SUT" build abc123 prod >/dev/null 2>&1; then
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
     sh "$SUT" build abc123 prod >/dev/null 2>&1; then
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
     sh "$SUT" build abc123 prod >/dev/null 2>&1; then
  fail "build should fail when one image build fails"
fi
if grep -qF "docker tag" "$LOG"; then
  fail "a failed build must not reach the retag-swap (live :prod images stay intact)"
fi
echo "ok: a build that fails mid-way leaves the live :prod images untouched"

# ── Case 2d: a FAILED build prunes what it stranded and reports the reclaim ────
# Pruning only after a SUCCESSFUL refresh is what let each failure make the next
# one more likely to fail (deploy #248): the dead build's generation stayed
# stranded until someone ssh'd in. The failure path now runs the same safe prune
# pair and reports the reclaim, so the retry starts no fuller than this one did.
: > "$LOG"
set_free_kb 25000000                    # ~23.8GB free: clears the pre-flight
export FREE_KB_AFTER_PRUNE=34000000     # ~32.4GB after the prune pair
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$KEY" \
     FAKE_FETCH_HEAD=abc123 \
     FAIL_BUILD=agent-rust \
     sh "$SUT" build abc123 prod > "$OUT" 2>&1; then
  fail "build should fail when one image build fails"
fi
# The temp tags are dropped FIRST (that is what makes them dangling), then the
# sanctioned pair reclaims them — dangling images only, never `-a`.
grep_log "docker rmi -f chuggernaut/worker:prod-refresh"
grep_log "docker image prune -f"
grep_log "docker builder prune -f --keep-storage 15GB"
if grep -qE "prune (-a|.* -a)" "$LOG"; then
  fail "the failure-path prune must never use -a (live :prod images must survive)"
fi
# ...and the reclaim is in the output the daemon relays into the failed leg.
grep_out "reclaimed 8.5GB (23.8GB -> 32.4GB free on /)"
# A phase is ANNOUNCED BEFORE its step runs (ticket #253) — this build DIED in
# agent-rust, so seeing its marker proves the marker preceded the work. A marker
# printed after the step would relay the phase only once it was already over,
# which is exactly the silence this ticket removes.
grep_out "worker-refresh: phase build-image 3/3 agent-rust"
unset FREE_KB_AFTER_PRUNE
echo "ok: a failed build prunes what it stranded and reports the reclaim"

# ── Case 3: swap schedules a detached sibling that recreates chug-worker ──────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod

# The swapper is detached (`run -d`) so `docker rm -f chug-worker` can't kill it,
# and it recreates the daemon on the new tag.
grep_log "docker run -d --name chug-worker-swap"
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

# ── Case 3a: the swapper's transcript survives the swap (ticket #270) ──────────
# The sibling that removes the live daemon and starts its replacement holds the
# only account of the riskiest moment of a refresh. Under `--rm` a failed
# `$RUN_NEW` was deleted seconds later, leaving a node with no daemon and no
# reason (deploy #267). It must be NAMED and RETAINED — one per node, the previous
# one force-removed first — and its inner `docker rm -f` must keep stderr.
if grep -qF -- "--rm" "$LOG"; then
  fail "the swapper must not run --rm (its transcript is the only record of a failed swap)"
fi
# Bounded retention: the prior swapper is removed by name before this one starts.
grep_log "docker rm -f chug-worker-swap"
# stdout of the inner rm is dropped (the id echo), stderr is NOT.
if grep -F "sh -c" "$LOG" | grep -qF "rm -f chug-worker >/dev/null 2>&1"; then
  fail "the swapper's docker rm -f must keep its stderr (2>&1 >/dev/null discards the cause)"
fi
# The deploy log says where the transcript is, so an operator does not have to
# know the container's name by heart.
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod > "$OUT" 2>&1
grep_out "docker logs chug-worker-swap"
echo "ok: swap retains its named swapper transcript and keeps the inner rm's stderr"

# ── Case 3d: the replacement daemon gets a usable RUST_LOG (ticket #270) ───────
# Without RUST_LOG the daemon's tracing default is ERROR, so every phase marker
# and every relayed refresh line is filtered out and `docker logs chug-worker`
# says nothing about a refresh — the silence the #267 post-mortem hit. Default to
# info with noisy deps damped; an override on the live daemon is carried forward
# (the #55/#82 silent-revert class of bug).
grep_log "RUST_LOG=info,async_nats=warn"
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  RUST_LOG=debug \
  sh "$SUT" swap prod
grep_log "RUST_LOG=debug"
if grep -qF "RUST_LOG=info" "$LOG"; then
  fail "swap must carry the daemon's own RUST_LOG forward, not overwrite it"
fi
echo "ok: swap gives the replacement daemon RUST_LOG=info by default, honouring an override"

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

# ── Case 3c: swap carries the disk pre-flight knobs forward (deploy #248) ──────
# Same class of silent-drop bug as WORKER_CACHE_DIR: a node tuned to a different
# disk shape must keep that tuning across a self-refresh, or the very next
# refresh reverts it to the built-in default and the operator's override is a
# no-op. Unset ⇒ nothing passed, so a stock node keeps the documented constant.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_REFRESH_DISK_FREE_GB_MIN=30 \
  WORKER_REFRESH_DISK_PATH=/var/lib/docker \
  sh "$SUT" swap prod

grep_log "WORKER_REFRESH_DISK_FREE_GB_MIN=30"
grep_log "WORKER_REFRESH_DISK_PATH=/var/lib/docker"

: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod

if grep -qF "WORKER_REFRESH_DISK_" "$LOG"; then
  fail "swap must not pass a WORKER_REFRESH_DISK_ knob when unset (default applies)"
fi
echo "ok: swap carries the disk pre-flight knobs forward, and passes none when unset"

# ── Case 4: unknown phase is a hard error ────────────────────────────────────
if PATH="$BIN:$PATH" sh "$SUT" frobnicate 2>/dev/null; then
  fail "unknown phase should exit non-zero"
fi
echo "ok: unknown phase rejected"

echo "ALL PASS"
