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
  # The device carry-forward reads the LIVE container's .HostConfig.Devices.
  # \$FAKE_KVM_DEVICES models a KVM-enabled node; empty (the default) is a fleet
  # with no device, which is every node until an operator turns one on. Set it
  # WITH the trailing space docker's \`{{range}}\` template really emits per
  # device — command substitution strips newlines, not spaces, so a fixture
  # without it would hide the double space the composed run actually carries.
  inspect*HostConfig.Devices*) [ -z "\${FAKE_KVM_DEVICES:-}" ] || echo "\$FAKE_KVM_DEVICES" ;;
  inspect*data/keys*)     echo "/home/worksalot/chuggernaut-worker/keys" ;;
  inspect*docker.sock*)   echo "/var/run/docker.sock" ;;
  # The nix mount carry-forward (design #373 P1) reads every mount of the LIVE
  # container as \`Destination|Source|RW\`. \$FAKE_NIX_MOUNTS models a node with
  # per-task GC roots on; empty (the default) is a node without them, which is
  # every node until an operator turns them on.
  inspect*Destination*)   [ -z "\${FAKE_NIX_MOUNTS:-}" ] || echo "\$FAKE_NIX_MOUNTS" ;;
  # Image-label read-back for the retag-swap guard: echo \$FAKE_LABEL (default
  # abc123, the requested SHA in the success cases) so the assert passes unless a
  # case forces a mismatch (the stale-image-label case).
  inspect*chug.git.sha*)  echo "\${FAKE_LABEL:-abc123}" ;;
  # A prune frees space: move the fake df reading to \$FREE_KB_AFTER_PRUNE so the
  # script's reclaim report is computed from a real before/after difference.
  *prune*) [ -z "\${FREE_KB_AFTER_PRUNE:-}" ] || echo "\$FREE_KB_AFTER_PRUNE" > "$FREE_FILE" ;;
  # A build CONSUMES space: same trick as the prune above, in the other
  # direction, so the script's build-cost report is computed from a real
  # before/after difference. Listed after *prune* — \`builder prune\` also
  # starts with "build" and must keep matching that arm.
  build*) [ -z "\${FREE_KB_AFTER_BUILD:-}" ] || echo "\$FREE_KB_AFTER_BUILD" > "$FREE_FILE" ;;
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

# Fake mkdir: log the argv, then do the real thing (the script's own temp work
# must still succeed). It exists so a case can assert what this script must NOT
# do — create the host cache dir — which is otherwise invisible to a log of
# docker/git calls.
cat > "$BIN/mkdir" <<EOF
#!/bin/sh
echo "mkdir \$*" >> "$LOG"
exec /bin/mkdir "\$@"
EOF

chmod +x "$BIN/git" "$BIN/docker" "$BIN/df" "$BIN/mkdir"

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
grep_log() { grep -qF -- "$1" "$LOG" || fail "expected in log: $1"; }
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
grep_out "disk pre-flight: 57.2GB free on /, need 30GB"
grep_log "docker image prune -f"
grep_log "docker builder prune -f --keep-storage 8GB"
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
set_free_kb 10000000   # ~9.5GB — under the 30GB threshold
if PATH="$BIN:$PATH" \
     WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
     WORKER_GIT_KEY="$KEY" \
     FAKE_FETCH_HEAD=abc123 \
     sh "$SUT" build abc123 prod > "$OUT" 2>&1; then
  fail "build should fail when the docker filesystem lacks room for a generation"
fi
grep_out "need ~30GB"
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

# ── Case 1f2: the build reports what it actually cost, BEFORE the prune ──────
# DISK_FREE_GB_MIN is a constant every past derivation (#248, #347, #351) got
# from an operator sampling `df` over ssh through a live refresh — and #351's
# sample showed it had drifted BELOW the real peak. The refresh now prints its
# own consumption into the deploy leg so the next derivation reads off a number
# instead of a projection. Pre-prune on purpose: after the prune the reading
# describes the cleanup, not the build.
: > "$LOG"
set_free_kb 60000000
PATH="$BIN:$PATH" \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  FAKE_FETCH_HEAD=abc123 \
  FREE_KB_AFTER_BUILD=40000000 \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1 \
  || fail "build should succeed with ample space"
grep_out "disk: build consumed 19.0GB on / (57.2GB -> 38.1GB free, pre-prune; floor is 30GB)"
echo "ok: build reports its own disk cost, so the floor is re-derived from a measurement"
set_free_kb 60000000

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
set_free_kb 35000000                    # ~33.4GB free: clears the pre-flight
export FREE_KB_AFTER_PRUNE=44000000     # ~42.0GB after the prune pair
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
grep_log "docker builder prune -f --keep-storage 8GB"
if grep -qE "prune (-a|.* -a)" "$LOG"; then
  fail "the failure-path prune must never use -a (live :prod images must survive)"
fi
# ...and the reclaim is in the output the daemon relays into the failed leg.
grep_out "reclaimed 8.5GB (33.3GB -> 41.9GB free on /)"
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
# Carried forward, never created. This phase runs INSIDE chug-worker, which does
# not mount the cache path, so a `mkdir -p` here would land in the daemon
# container's writable layer and never on the host — the same illusion the
# daemon's own create_dir_all gives (#379). The host dir is provisioned at node
# creation by build-worker.sh; a dir missing at swap time is something to fail
# loudly on, not to re-create silently.
# Catches both shapes: a direct `mkdir` (the fake logs one) and a `mkdir` smuggled
# into the detached swapper's inner command (logged with the `docker run` argv).
if grep -F "/var/cache/chuggernaut/sccache" "$LOG" | grep -qF "mkdir"; then
  fail "the swap must not create the cache dir (it would land in the container, not on the host)"
fi
echo "ok: swap carries WORKER_CACHE_DIR forward as env, and creates no host dir"

# ── Case 3c: swap carries the disk pre-flight knobs forward (deploy #248) ──────
# Same class of silent-drop bug as WORKER_CACHE_DIR: a node tuned to a different
# disk shape must keep that tuning across a self-refresh, or the very next
# refresh reverts it to the built-in default and the operator's override is a
# no-op. Unset ⇒ nothing passed, so a stock node keeps the documented constant.
# The value deliberately differs from the built-in default, so a swap that
# hardcoded the default instead of forwarding the operator's value still fails.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_REFRESH_DISK_FREE_GB_MIN=45 \
  WORKER_REFRESH_DISK_PATH=/var/lib/docker \
  sh "$SUT" swap prod

grep_log "WORKER_REFRESH_DISK_FREE_GB_MIN=45"
grep_log "WORKER_REFRESH_DISK_PATH=/var/lib/docker"

: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod

if grep -qF "WORKER_REFRESH_DISK_" "$LOG"; then
  fail "swap must not pass a WORKER_REFRESH_DISK_ knob when unset (default applies)"
fi
echo "ok: swap carries the disk pre-flight knobs forward, and passes none when unset"

# ── Case 3e: swap carries the node's capacity forward (WORKER_SLOTS) ───────────
# The loudest member of the silent-revert class: the daemon announces its own slot
# count and that announcement wins over the dispatcher's DOCKER_NODES seed (spec
# §3.1), so a swap that dropped WORKER_SLOTS would put a node deliberately capped
# at 2 back on the default 4 — more concurrent job containers than the node was
# sized for, and nothing fleet-side to explain it. Unset ⇒ nothing passed.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=air NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_SLOTS=2 \
  sh "$SUT" swap prod

grep_log "WORKER_SLOTS=2"

: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod

if grep -qF "WORKER_SLOTS" "$LOG"; then
  fail "swap must not pass WORKER_SLOTS when unset (daemon default applies)"
fi
echo "ok: swap carries WORKER_SLOTS forward, and passes none when unset"

# ── Case 3f: KVM off ⇒ the swap carries neither the settings nor a device ─────
# Every node in the fleet is here until an operator turns KVM on, so the
# replacement daemon's run spec must be untouched by #367 existing.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod

if grep -qE "WORKER_KVM|WORKER_ANDROID_SDK_DIR|WORKER_FLUTTER_DIR" "$LOG"; then
  fail "swap must not pass a KVM setting when none is set (no passthrough)"
fi
# Scoped to the swapper's own command line: the device carry-forward READS
# `.HostConfig.Devices` with a `--format` template that itself contains the flag,
# so a whole-log grep would match the inspect that proves the node has none.
if grep -F "chug-worker-swap" "$LOG" | grep -qF -- "--device"; then
  fail "swap must not invent a device the live daemon is not running with"
fi
echo "ok: swap passes no KVM setting and no device when KVM is off"

# ── Case 3g: KVM on ⇒ settings from the env, the DEVICE from the live container ─
# The asymmetry that makes this dangerous: dropping a setting turns KVM off (a
# quiet regression, the node stays up), but dropping the DEVICE while keeping
# WORKER_KVM takes the NODE DOWN — the replacement daemon refuses to start on a
# device its own view lacks (crates/worker/src/daemon.rs), --restart=always
# restarts it into the same refusal, and the node leaves the fleet. The swap
# re-composes `docker run` from scratch, so this is exactly where that happens.
#
# The live container is faked with a NON-default device, so a swap that
# reconstructed `--device` from WORKER_KVM (which says the default path here)
# rather than from what is actually running fails this case.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS="acme/beacon, acme/api" \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  FAKE_KVM_DEVICES="--device /dev/kvm1:/dev/kvm1:rwm " \
  sh "$SUT" swap prod

grep_log "-e WORKER_KVM='1'"
grep_log "-e WORKER_KVM_PROJECTS='acme/beacon, acme/api'"
grep_log "-e WORKER_ANDROID_SDK_DIR='/etc/chug/android-sdk'"
grep_log "--device /dev/kvm1:/dev/kvm1:rwm"
# The device must land as a `docker run` FLAG — after the image it would be the
# container's command, and the daemon would never start. Asserted as ORDERING
# rather than adjacency: the template's trailing space survives into the composed
# run, so the two are separated by whitespace the swapper's shell collapses.
case "$(grep -F chug-worker-swap "$LOG")" in
  *"--device /dev/kvm1:/dev/kvm1:rwm"*"chuggernaut/worker:prod"*) ;;
  *) fail "the device must precede the image (after it, it would be the container's command)" ;;
esac
echo "ok: swap carries the KVM settings forward and the device from the live container"

# ── Case 3g1: the node's Flutter SDK rides forward too (#393) ─────────────────
# A self-refresh that dropped it would silently un-provision Flutter on a node
# whose builds need it — the quiet-regression half of the asymmetry above. The
# Android SDK must come through unchanged beside it: two independent leaves.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS="acme/beacon" \
  WORKER_ANDROID_SDK_DIR=/var/lib/chuggernaut/toolchain/android-sdk \
  WORKER_FLUTTER_DIR=/var/lib/chuggernaut/toolchain/flutter \
  FAKE_KVM_DEVICES="--device /dev/kvm:/dev/kvm:rwm " \
  sh "$SUT" swap prod

grep_log "-e WORKER_ANDROID_SDK_DIR='/var/lib/chuggernaut/toolchain/android-sdk'"
grep_log "-e WORKER_FLUTTER_DIR='/var/lib/chuggernaut/toolchain/flutter'"
echo "ok: swap carries WORKER_FLUTTER_DIR forward beside the unchanged Android SDK"

# ── Case 3h: WORKER_KVM set, no device on the live container ⇒ REFUSE the swap ─
# The node-down mode, made impossible: rather than launch a replacement that
# cannot boot, refuse. The node keeps running its current daemon (an old SHA is a
# deploy warning; a node that will not start is an outage).
: > "$LOG"
if PATH="$BIN:$PATH" \
     WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
     WORKER_KVM=1 \
     sh "$SUT" swap prod > "$OUT" 2>&1; then
  fail "a KVM daemon whose device cannot be carried forward must refuse the swap"
fi
grep_out "refusing swap"
if grep -qF "docker run -d --name chug-worker-swap" "$LOG"; then
  fail "the refusal must happen BEFORE the swapper is scheduled (the live daemon keeps running)"
fi
echo "ok: a device that cannot be carried forward refuses the swap instead of downing the node"

# ── Case 3i: WORKER_KVM is OFF ⇒ no device is expected, so the swap PROCEEDS ───
# The other side of 3h, and the one that bites: build-worker.sh composes an
# explicitly-off node as `-e WORKER_KVM='0'` with NO device (build-worker.test.sh
# case 2g), and the daemon reads 0/false/off as no passthrough at all. Refusing
# that node's swap would freeze it on its old SHA forever for a node-down hazard
# that cannot happen — so the refusal must key off the values that attach a
# device, not off the var being set. The trailing/leading spaces ride along
# because the daemon trims before it matches (`parse_kvm_device`) and the swap
# must read the value the same way rather than seeing ` off ` as "on".
for KVM_OFF_VALUE in 0 false " off "; do
  : > "$LOG"
  PATH="$BIN:$PATH" \
    WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
    WORKER_KVM="$KVM_OFF_VALUE" \
    sh "$SUT" swap prod > "$OUT" 2>&1 \
    || fail "WORKER_KVM='$KVM_OFF_VALUE' is off — the swap must proceed, not refuse"
  grep_log "docker run -d --name chug-worker-swap"
  if grep -F "chug-worker-swap" "$LOG" | grep -qF -- "--device"; then
    fail "WORKER_KVM='$KVM_OFF_VALUE' is off — no device may be carried forward"
  fi
done
# The setting still rides, trimmed exactly as the daemon would trim it, so the
# replacement daemon reads the same off as the one it replaces.
grep_log "-e WORKER_KVM='off'"
echo "ok: an explicitly-off WORKER_KVM swaps normally and carries the trimmed setting"

# ── Case 3i: nix roots on ⇒ settings from the env, MOUNTS from the live node ──
# design #373 P1, and the same asymmetry as the KVM device one case up: dropping
# a nix SETTING turns roots off (quiet), dropping a nix MOUNT takes the node DOWN
# — the replacement daemon refuses to start without its roots dir, its client or
# the socket in its own view (crates/worker/src/nix.rs) and --restart=always
# loops the refusal. The fixture uses a NON-default profiles path resolved
# through /mnt (gumbo-nuc-0's real shape), so a swap that reconstructed the
# mounts from a hardcoded path instead of reading the live container still fails.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_NIX_REALISE_TIMEOUT_SECS=40 \
  FAKE_NIX_MOUNTS="/nix/store|/mnt/nix/store|false /nix/var/nix/profiles|/mnt/nix/var/nix/profiles|false /nix/var/nix/daemon-socket|/nix/var/nix/daemon-socket|true /var/lib/chuggernaut/gcroots|/var/lib/chuggernaut/gcroots|true" \
  sh "$SUT" swap prod

grep_log "-e WORKER_NIX_GCROOTS_DIR='/var/lib/chuggernaut/gcroots'"
grep_log "-e WORKER_NIX_REALISE_TIMEOUT_SECS='40'"
grep_log "-v /mnt/nix/store:/nix/store:ro"
grep_log "-v /mnt/nix/var/nix/profiles:/nix/var/nix/profiles:ro"
grep_log "-v /nix/var/nix/daemon-socket:/nix/var/nix/daemon-socket"
grep_log "-v /var/lib/chuggernaut/gcroots:/var/lib/chuggernaut/gcroots"
# Each mount keeps the live container's own read-only bit: a socket mounted :ro
# fails every realise (connecting needs write on the inode), and a store mounted
# writable would hand a task the node's whole store to edit.
if grep -qF -- "-v /nix/var/nix/daemon-socket:/nix/var/nix/daemon-socket:ro" "$LOG"; then
  fail "the daemon socket must stay read-write across a swap"
fi
if grep -qF -- "-v /var/lib/chuggernaut/gcroots:/var/lib/chuggernaut/gcroots:ro" "$LOG"; then
  fail "the roots dir must stay writable across a swap"
fi
# The swap must not create the roots dir: it runs INSIDE chug-worker, so a mkdir
# here lands in the daemon container's filesystem and never on the host (#379/#380).
if grep -F "/var/lib/chuggernaut/gcroots" "$LOG" | grep -qF "mkdir"; then
  fail "the swap must not create the gcroots dir (it would land in the container)"
fi
echo "ok: swap carries the nix settings forward and the four mounts from the live container"

# ── Case 3j: a nix mount that cannot be carried forward REFUSES the swap ──────
# The node-down case: WORKER_NIX_GCROOTS_DIR says roots are on, but the live
# container has no roots mount to carry, so the replacement could only come up
# refusing to boot. Refuse the swap instead and leave the old daemon serving.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  FAKE_NIX_MOUNTS="/nix/store|/nix/store|false /nix/var/nix/daemon-socket|/nix/var/nix/daemon-socket|true" \
  sh "$SUT" swap prod >"$WORK/nixswap.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a missing nix mount must refuse the swap (got rc=0)"
grep -qF "/var/lib/chuggernaut/gcroots" "$WORK/nixswap.out" || fail "the refusal must name the missing mount"
if grep -qF "docker run -d --name chug-worker-swap" "$LOG"; then
  fail "a missing nix mount must not schedule the swapper (the live daemon keeps serving)"
fi
echo "ok: a nix mount that cannot be carried forward refuses the swap instead of downing the node"

# ── Case 3l: roots + a KVM device ⇒ the toolchain's PARENT is carried forward ──
# The fifth mount, and its destination is the DIRECTORY HOLDING the stable path
# — binding that path itself would resolve the operator's symlink host-side and
# hand the client a non-store path it refuses. Dropping the mount on a swap does
# not turn roots off: it makes every admitted launch on the node fail, and the
# replacement's own boot check refuses to start at all. Carried by destination
# like the rest, with its read-only bit, and refused when it is not there.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=1 WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  FAKE_KVM_DEVICES="--device /dev/kvm:/dev/kvm:rwm" \
  FAKE_NIX_MOUNTS="/nix/store|/nix/store|false /nix/var/nix/daemon-socket|/nix/var/nix/daemon-socket|true /var/lib/chuggernaut/gcroots|/var/lib/chuggernaut/gcroots|true /etc/chug|/etc/chug|false" \
  sh "$SUT" swap prod

grep_log "-v /etc/chug:/etc/chug:ro"

: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=1 WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  FAKE_KVM_DEVICES="--device /dev/kvm:/dev/kvm:rwm" \
  FAKE_NIX_MOUNTS="/nix/store|/nix/store|false /nix/var/nix/daemon-socket|/nix/var/nix/daemon-socket|true /var/lib/chuggernaut/gcroots|/var/lib/chuggernaut/gcroots|true" \
  sh "$SUT" swap prod >"$WORK/nixsdk.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a missing toolchain mount must refuse the swap (got rc=0)"
grep -qF "no mount at '/etc/chug'" "$WORK/nixsdk.out" || fail "the refusal must name the missing mount"
if grep -qF "docker run -d --name chug-worker-swap" "$LOG"; then
  fail "a missing toolchain mount must not schedule the swapper"
fi

# A node with roots on and NO device realises nothing, so no toolchain mount is
# expected and its absence must NOT refuse the swap.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=0 WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  FAKE_NIX_MOUNTS="/nix/store|/nix/store|false /nix/var/nix/daemon-socket|/nix/var/nix/daemon-socket|true /var/lib/chuggernaut/gcroots|/var/lib/chuggernaut/gcroots|true" \
  sh "$SUT" swap prod
grep_log "docker run -d --name chug-worker-swap"
echo "ok: swap carries the toolchain mount forward with the device, and refuses when it is gone"

# ── Case 3k: nix roots off ⇒ the swap is nix-free ─────────────────────────────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  FAKE_NIX_MOUNTS="/nix/store|/nix/store|false" \
  sh "$SUT" swap prod

if grep -F "chug-worker-swap" "$LOG" | grep -qE "WORKER_NIX|/nix/store"; then
  fail "a node with roots off must get no nix env and no nix mount"
fi
echo "ok: swap adds nothing nix when WORKER_NIX_GCROOTS_DIR is unset"

# ── Case 3m: every phase REPORTS the run spec it is running (ticket #390) ─────
# This script's config is INHERITED from the daemon's own environment, which is
# the only mechanism it can have — the swap runs inside chug-worker and the
# dispatcher host cannot ssh a tagged worker, so nothing here can read
# chuggernaut.env. Inheritance is how a value survives; it is not how a value is
# DECLARED, and #390 found four settings living only inside the container. The
# node therefore states its own spec on stdout, which the daemon relays into the
# deploy's task output — that report IS the drift check for a node the Mini
# cannot reach, and it costs no UI and no new mechanism.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=air NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  WORKER_SLOTS=2 \
  WORKER_CACHE_DIR=/Users/op/chuggernaut-worker/sccache \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY=/data/keys/worker_git \
  sh "$SUT" swap prod > "$OUT" 2>&1

grep_out "run spec on air (swap): WORKER_SLOTS=2 WORKER_CACHE_DIR=/Users/op/chuggernaut-worker/sccache WORKER_REFRESH_GIT_URL=ssh://git@front:2222/acme/chug.git WORKER_GIT_KEY=/data/keys/worker_git"
if grep -qF "WARNING" "$OUT"; then
  fail "a fully specified node must report its spec without warning about it"
fi

# The build phase reports it too — it runs FIRST and for minutes, so it is the
# report that reaches the deploy while the daemon relaying it is still alive.
PATH="$BIN:$PATH" \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  WORKER_NODE=air WORKER_SLOTS=2 WORKER_CACHE_DIR=/Users/op/chuggernaut-worker/sccache \
  FAKE_FETCH_HEAD=abc123 \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1
grep_out "run spec on air (build): WORKER_SLOTS=2"
echo "ok: both phases report the run spec the node is actually running"

# ── Case 3n: the two settings that fail SILENTLY say so, by name ──────────────
# Their absence looks exactly like a healthy node: caching off is a slow node
# (#55's dormant cache, which took a dedicated fix to notice), and a dropped
# capacity is an over-committed one. Nothing else reports either.
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc NATS_URL=nats://10.0.0.1:4222 NATS_CREDS=/data/keys/worker.creds \
  sh "$SUT" swap prod > "$OUT" 2>&1

grep_out "WARNING: WORKER_CACHE_DIR is unset"
grep_out "WARNING: WORKER_SLOTS is unset"
grep_out "run spec on nuc (swap): WORKER_SLOTS=<unset> WORKER_CACHE_DIR=<unset>"
# Reporting is all it does: a node missing a value must still swap, or a deploy
# would leave it stranded on the old SHA over a line of config.
grep_log "docker run -d --name chug-worker-swap"
echo "ok: an unset cache dir or capacity is named out loud, and still swaps"

# ── Case 4: unknown phase is a hard error ────────────────────────────────────
if PATH="$BIN:$PATH" sh "$SUT" frobnicate 2>/dev/null; then
  fail "unknown phase should exit non-zero"
fi
echo "ok: unknown phase rejected"

echo "ALL PASS"
