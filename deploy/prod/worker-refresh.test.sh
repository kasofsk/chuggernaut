#!/bin/sh
# Shell test for worker-refresh.sh — no Docker, no git server, no NATS.
#
# It drives worker-refresh.sh with fake `git`, `docker` and supervisor commands
# on PATH that just log their invocations, then asserts the build phase builds
# the three node images and the swap phase EXTRACTS THE BINARY FROM THE IMAGE IT
# JUST BUILT, INSTALLS IT AND ASKS THE SUPERVISOR TO RESTART (design #440 D6) —
# with no detached process and no carry-forward of any kind. On a DARWIN node
# that extraction is wrong by construction (the image is a Linux container), so
# there the build phase compiles a native daemon and the swap installs that;
# both platforms must prove the staged binary RUNS before installing it. This locks the
# script's contract — the phase split, the three images, the install-and-restart
# shape and every refusal in front of it — without any real infrastructure.
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

# Fake docker: log the full argv (so we can assert on `build -t <image>` and on
# the swap's `create`/`cp` extraction), consume any piped stdin.
cat > "$BIN/docker" <<EOF
#!/bin/sh
cat >/dev/null 2>&1 || true
echo "docker \$*" >> "$LOG"
case "\$*" in
  # The swap extracts the daemon out of the image the build just made
  # (design #440 D6): \`docker create\` hands back a container id and each
  # \`docker cp\` materialises the file the script is about to install.
  # \$FAKE_CP_EMPTY models an extraction that produced nothing — a stale or
  # broken image — which must refuse before anything is installed. What it
  # materialises otherwise is a binary that RUNS, because the swap now demands
  # exactly that of the staged daemon before installing it; \$FAKE_BAD_BINARY is
  # the other node, whose extracted binary is for a foreign platform (the air's
  # ELF-under-launchd crash loop, 2026-08-06).
  create*) echo fakecid ;;
  cp*)
    shift
    if [ -n "\${FAKE_CP_EMPTY:-}" ]; then
      : > "\$2"
    elif [ -n "\${FAKE_BAD_BINARY:-}" ]; then
      printf 'NOT-A-BINARY-FOR-THIS-PLATFORM\n' > "\$2"
    else
      printf '#!/bin/sh\nexit 0\n' > "\$2"
    fi
    ;;
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

# Fake systemctl: log the argv so the swap's two calls are assertable — the
# is-active precondition (is this daemon the unit's, or would a restart start a
# SECOND one) and the restart itself. $FAKE_UNIT_INACTIVE makes the node answer
# "not mine". $FAKE_KILL_ON_RESTART makes the restart do to the caller what a
# real `systemctl restart` does — kill the cgroup this shell is in — so a case
# can assert what survives an exit that runs no EXIT trap.
cat > "$BIN/systemctl" <<EOF
#!/bin/sh
echo "systemctl \$*" >> "$LOG"
case "\$*" in
  is-active*) [ -z "\${FAKE_UNIT_INACTIVE:-}" ] || exit 3 ;;
  restart*)   [ -z "\${FAKE_KILL_ON_RESTART:-}" ] || kill -KILL "\$PPID" ;;
esac
exit 0
EOF

# The macOS half, kept in its OWN dir: a Linux case must not see `launchctl`,
# and a "this node has no launchctl" case must be able to run with `uname`
# saying Darwin and nothing to drive.
MACBIN="$WORK/macbin"
NOLCBIN="$WORK/nolcbin"
mkdir -p "$MACBIN" "$NOLCBIN"
for d in "$MACBIN" "$NOLCBIN"; do
  cat > "$d/uname" <<'EOF'
#!/bin/sh
echo Darwin
EOF
  chmod +x "$d/uname"
done
cat > "$MACBIN/launchctl" <<EOF
#!/bin/sh
echo "launchctl \$*" >> "$LOG"
case "\$*" in
  print*) [ -z "\${FAKE_AGENT_MISSING:-}" ] || exit 113 ;;
esac
exit 0
EOF
chmod +x "$MACBIN/launchctl"

# The macOS half of the toolchain, and it is only in $MACBIN: a Linux case that
# saw a `cargo` would pass a script that had started compiling on a node whose
# daemon must come out of the image (design #440 D6, which holds there).
#
# Fake cargo materialises what a real one would leave in CARGO_TARGET_DIR — two
# binaries that RUN, because the swap refuses to install one that does not.
# Fake tar unpacks nothing (fake `git archive` emits no real tar) and instead
# lays down the one file the swap copies out of the tree, so the extraction is
# still assertable end to end.
cat > "$MACBIN/cargo" <<EOF
#!/bin/sh
case "\$1" in --version) echo "cargo 1.95.0 (fake)"; exit \${FAKE_DEAD_CARGO:-0} ;; esac
echo "cargo \$* [CHUG_GIT_SHA=\${CHUG_GIT_SHA:-} CARGO_TARGET_DIR=\${CARGO_TARGET_DIR:-} PWD=\$PWD PATH=\$PATH]" >> "$LOG"
[ -z "\${FAIL_CARGO:-}" ] || exit 101
mkdir -p "\$CARGO_TARGET_DIR/release"
for b in chuggernaut chuggernaut-channel; do
  if [ -n "\${FAKE_BAD_BINARY:-}" ]; then
    printf 'NOT-A-BINARY-FOR-THIS-PLATFORM\n' > "\$CARGO_TARGET_DIR/release/\$b"
  else
    printf '#!/bin/sh\nexit 0\n' > "\$CARGO_TARGET_DIR/release/\$b"
  fi
  chmod +x "\$CARGO_TARGET_DIR/release/\$b"
done
exit 0
EOF
cat > "$MACBIN/tar" <<EOF
#!/bin/sh
echo "tar \$*" >> "$LOG"
_dir=
while [ \$# -gt 0 ]; do
  [ "\$1" != "-C" ] || { _dir="\$2"; shift; }
  shift
done
[ -n "\$_dir" ] || exit 0
mkdir -p "\$_dir/deploy/prod"
printf '#!/bin/sh\n# fetched worker-refresh.sh\n' > "\$_dir/deploy/prod/worker-refresh.sh"
exit 0
EOF
# `rustc` BESIDE cargo, because that is the whole point: cargo resolves its
# compiler through PATH, so the build phase asks for it under the PATH the
# compile will actually run with rather than trusting WORKER_CARGO alone.
cat > "$MACBIN/rustc" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$MACBIN/cargo" "$MACBIN/tar" "$MACBIN/rustc"

# The same toolchain MINUS rustc — a nix-darwin or rustup cargo declared by
# absolute path while its own directory is on no PATH either end has.
NORUSTCBIN="$WORK/norustcbin"
mkdir -p "$NORUSTCBIN"
cp "$MACBIN/uname" "$MACBIN/cargo" "$MACBIN/tar" "$NORUSTCBIN/"

# The guard asks a REAL `command -v rustc`, and this suite's own host may have a
# toolchain (CI runs it in the agent-rust image) — so a case modelling a node
# without one has to strip every PATH entry that holds a rustc, or it would find
# the harness's and the case would assert nothing.
strip_rustc_path() {
  _out=""
  _rest="$1"
  while [ -n "$_rest" ]; do
    case "$_rest" in
      *:*) _d="${_rest%%:*}"; _rest="${_rest#*:}" ;;
      *)   _d="$_rest"; _rest="" ;;
    esac
    [ -n "$_d" ] || continue
    [ ! -x "$_d/rustc" ] || continue
    _out="${_out:+$_out:}$_d"
  done
  printf '%s\n' "$_out"
}
NO_RUSTC_PATH="$(strip_rustc_path "$PATH")"

# Where a converted mac keeps the tree it compiles and the target dir it keeps
# between refreshes (WORKER_BUILD_DIR, written into the run spec by
# build-worker.sh). Under $WORK so the test writes nothing outside its temp dir.
MAC_BUILD_DIR="$WORK/nativebuild"
mac_build_reset() { rm -rf "$MAC_BUILD_DIR"; mkdir -p "$MAC_BUILD_DIR"; }
# A staging directory as the build phase leaves it: the two binaries, the tree's
# copy of this script, and the SHA the swap checks against the image's label.
mac_build_stage() {
  mac_build_reset
  mkdir -p "$MAC_BUILD_DIR/target/release" "$MAC_BUILD_DIR/src/deploy/prod"
  for b in chuggernaut chuggernaut-channel; do
    printf '#!/bin/sh\nexit 0\n' > "$MAC_BUILD_DIR/target/release/$b"
    chmod +x "$MAC_BUILD_DIR/target/release/$b"
  done
  printf '#!/bin/sh\n# staged worker-refresh.sh\n' > "$MAC_BUILD_DIR/src/deploy/prod/worker-refresh.sh"
  printf '%s\n' "${1:-abc123}" > "$MAC_BUILD_DIR/native.sha"
}

# Fake install/mv: log the argv, then do the real thing. They exist so the
# install-by-RENAME discipline is assertable — writing over the running daemon
# binary in place is ETXTBSY, and truncating this very script under the shell
# reading it feeds that shell the tail of a different file.
REAL_INSTALL="$(command -v install)"
REAL_MV="$(command -v mv)"
cat > "$BIN/install" <<EOF
#!/bin/sh
echo "install \$*" >> "$LOG"
exec "$REAL_INSTALL" "\$@"
EOF
cat > "$BIN/mv" <<EOF
#!/bin/sh
echo "mv \$*" >> "$LOG"
exec "$REAL_MV" "\$@"
EOF

chmod +x "$BIN/git" "$BIN/docker" "$BIN/df" "$BIN/mkdir" "$BIN/systemctl" \
  "$BIN/install" "$BIN/mv"

# A path that does not exist, so the swap's "this daemon is itself a container"
# marker reads FALSE. It has to be injectable: this test runs inside a container
# on CI, where the real /.dockerenv is present and every native case would
# refuse (design #440 slice 6).
NOT_CONTAINER="$WORK/not-a-container"

# Where a native swap installs the three artifacts it extracts. Under $WORK
# rather than /usr/local so the test writes nothing outside its own temp dir.
NATIVE="$WORK/native"
mkdir -p "$NATIVE/bin" "$NATIVE/lib"
DAEMON_BIN="$NATIVE/bin/chuggernaut"
CHANNEL_BIN="$NATIVE/lib/chuggernaut-channel"
NODE_SCRIPT="$NATIVE/lib/worker-refresh.sh"
native_swap_reset() { rm -f "$DAEMON_BIN" "$CHANNEL_BIN" "$NODE_SCRIPT"; }

# Where the swap's `mktemp -d` staging dir lands, so a case can assert the swap
# removed it. Its own dir because "is it gone" is only decidable if nothing else
# is in there.
STAGE_TMP="$WORK/stage-tmp"
mkdir -p "$STAGE_TMP"
stage_tmp_empty() { [ -z "$(ls -A "$STAGE_TMP")" ]; }

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

# Runs the case that asserts a directory permission, which root bypasses — and
# the CI gate's container is root. So when we are root the run under test drops
# to `nobody` and the harness it reads is opened up to it; a non-root host needs
# neither and $DROP stays empty.
DROP=""
if [ "$(id -u)" -eq 0 ]; then
  command -v setpriv > /dev/null 2>&1 ||
    fail "this suite needs 'setpriv' (util-linux) to run a case as an unprivileged user"
  DROP="setpriv --reuid=65534 --regid=65534 --clear-groups"
  chmod 0755 "$WORK" "$BIN" "$MACBIN" "$NOLCBIN"
  : > "$LOG"
  chmod 0666 "$LOG"
fi

grep_log() { grep -qF -- "$1" "$LOG" || fail "expected in log: $1"; }
# Line number of the first log entry containing $1, for the ORDER assertions —
# a daemon compiled after the images are already live is a generation the node
# cannot install.
line_of() { grep -nF -- "$1" "$LOG" | head -n 1 | cut -d: -f1; }
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

# ── Case 2c: a Darwin node COMPILES its daemon in the build phase ──────────────
# Design #440 D6 has the swap lift the binary out of the worker image, and that
# image is a LINUX container: on a mac it installs as an ELF file launchd loops
# on with `cannot execute binary file` (the air, 2026-08-06). So the mac builds
# a Mach-O daemon here, where minutes are affordable, and BEFORE the retag-swap
# — a compile that fails leaves the whole previous generation, images included.
: > "$LOG"
mac_build_reset
PATH="$MACBIN:$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  WORKER_CARGO="$MACBIN/cargo" \
  WORKER_BUILD_DIR="$MAC_BUILD_DIR" \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1

grep_log "cargo build --release --locked --bin chuggernaut --bin chuggernaut-channel [CHUG_GIT_SHA=abc123 CARGO_TARGET_DIR=$MAC_BUILD_DIR/target PWD=$MAC_BUILD_DIR/src PATH=$MACBIN:"
[ -x "$MAC_BUILD_DIR/target/release/chuggernaut" ] || fail "the build phase must leave a daemon binary staged"
[ "$(cat "$MAC_BUILD_DIR/native.sha")" = abc123 ] || fail "the staged build must record the SHA it was built for"
grep_out "phase build-daemon"
cargo_line="$(line_of "cargo build --release")"
tag_line="$(line_of "docker tag chuggernaut/worker:prod-refresh")"
[ -n "$cargo_line" ] && [ -n "$tag_line" ] || fail "expected both the compile and the retag in the log"
[ "$cargo_line" -lt "$tag_line" ] || fail "the daemon must be compiled BEFORE the images are retag-swapped"
echo "ok: a Darwin node compiles its own daemon before the retag-swap"

# ── Case 2c0: the toolchain is a DIRECTORY, and the two halves it needs ────────
# Cargo resolves `rustc` through PATH, and this daemon's PATH is the launchd
# agent's — where a nix-darwin profile directory is not. So a cargo declared by
# absolute path passes `command -v` and then fails MID-BUILD, with the images
# already replaced, which is the one place this file refuses to fail. Both
# questions therefore belong in the validate-first block.
: > "$LOG"
mac_build_reset
set +e
PATH="$NORUSTCBIN:$BIN:$NO_RUSTC_PATH" \
  WORKER_NODE=air \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  WORKER_CARGO="$NORUSTCBIN/cargo" \
  WORKER_BUILD_DIR="$MAC_BUILD_DIR" \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a Darwin node whose cargo cannot find rustc must refuse the build"
grep_out "cargo resolves its compiler THROUGH PATH"
if grep -qE "docker (build|tag)" "$LOG"; then
  fail "the rustc refusal must come before any docker mutation, not mid-compile"
fi

# A cargo that is on PATH and does not RUN — a rustup shim with no default
# toolchain — is the same shape one step earlier.
: > "$LOG"
mac_build_reset
set +e
PATH="$MACBIN:$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  WORKER_CARGO="$MACBIN/cargo" \
  WORKER_BUILD_DIR="$MAC_BUILD_DIR" \
  FAKE_DEAD_CARGO=1 \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a Darwin node whose cargo does not run must refuse the build"
grep_out "rustup shim with no default toolchain"
if grep -qE "docker (build|tag)" "$LOG"; then
  fail "a cargo that does not exec must refuse before any docker mutation"
fi
echo "ok: the compile's PATH carries the toolchain directory, and both halves refuse before the images move"

# ── Case 2c1: a compile that fails leaves the live generation alone ────────────
: > "$LOG"
mac_build_reset
set +e
PATH="$MACBIN:$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  WORKER_CARGO="$MACBIN/cargo" \
  WORKER_BUILD_DIR="$MAC_BUILD_DIR" \
  FAIL_CARGO=1 \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a failed native compile must fail the build phase"
if grep -qF "docker tag" "$LOG"; then
  fail "a failed compile must not reach the retag-swap (live images stay intact)"
fi
echo "ok: a failed native compile leaves the live images untouched"

# ── Case 2c2: a converted mac with no reachable cargo refuses, by name ─────────
# The state the air was left in: converted, running, and with no toolchain in the
# run spec. A refusal here is a failed deploy leg on a node that keeps serving;
# installing the image's binary instead is the node out of the fleet.
: > "$LOG"
mac_build_reset
set +e
PATH="$NOLCBIN:$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  WORKER_CARGO="$WORK/absent-cargo" \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a Darwin node with no cargo must refuse the build"
grep_out "build-worker.sh"
if grep -qE "docker (build|tag)" "$LOG"; then
  fail "the toolchain refusal must come before any docker mutation (images untouched)"
fi
# And a Linux node is never asked for one: its daemon comes out of the image.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY="$KEY" \
  sh "$SUT" build abc123 prod > "$OUT" 2>&1
if grep -qF "cargo build" "$LOG"; then
  fail "a Linux node must not compile the daemon (design #440 D6 holds there)"
fi
echo "ok: a mac with no cargo refuses the build by name; a Linux node is never asked for one"

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

# ── Case 3: the swap installs the new binary and asks the supervisor to restart ─
# Design #440 D6, and the whole of slice 6: a daemon that is a binary under a
# unit is replaced by writing the binary and restarting the unit. The three
# artifacts come OUT OF THE IMAGE the build phase just made (`docker create` +
# `docker cp`), which is the same extraction build-worker.sh runs over ssh — one
# definition of how the binary is produced, and no Rust toolchain as a node fact.
: > "$LOG"
native_swap_reset
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  sh "$SUT" swap prod > "$OUT" 2>&1

grep_log "docker create chuggernaut/worker:prod"
grep_log "docker cp fakecid:/usr/local/bin/chuggernaut"
grep_log "docker cp fakecid:/usr/local/lib/chuggernaut/chuggernaut-channel"
grep_log "docker cp fakecid:/usr/local/lib/chuggernaut/worker-refresh.sh"
grep_log "docker rm fakecid"
for f in "$DAEMON_BIN" "$CHANNEL_BIN" "$NODE_SCRIPT"; do
  [ -x "$f" ] || fail "the swap must install $f, executable"
  [ -e "$f.chug-new" ] && fail "the swap must not leave $f.chug-new behind"
done
grep_log "systemctl restart --no-block chug-worker.service"
echo "ok: the swap extracts the daemon from the image, installs it and restarts the unit"

# ── Case 3a: there is NO detached process and NO container touched ────────────
# What this deletes is the reason the old swap existed: a container cannot
# replace itself, so the swap detached a `docker:cli` sibling that did
# `docker rm -f chug-worker` + `docker run`. A supervisor restarting its own unit
# has no such problem (#372 §8 R1 dissolves), so a `docker run` here would mean
# the old shape survived — and a `docker rm -f` would mean the swap can still
# take a container down, which is exactly what job containers are protected from.
if grep -qF "docker run" "$LOG"; then
  fail "the native swap must start no container at all (the detached swapper is gone)"
fi
if grep -qF "chug-worker-swap" "$LOG"; then
  fail "the detached swapper is deleted — nothing may name it"
fi
if grep -qF "rm -f chug-worker" "$LOG"; then
  fail "the native swap must never remove the daemon container (job containers ride on that)"
fi
echo "ok: the native swap runs no detached process and removes no container"

# ── Case 3b: NOTHING is carried forward, however much is set ──────────────────
# Every `*_ARGS` block the old swap composed — the cache dir, the disk knobs,
# WORKER_SLOTS, WORKER_MODES, the KVM settings, the nix settings, RUST_LOG —
# existed because inheritance was how a value survived a container recreate.
# With the run spec in an environment file the supervisor loads on every start,
# a value survives because it is WRITTEN DOWN (design #440 D6/D7), and a swap
# that still passed one would be re-declaring the node from a copy that can
# drift. So: set them all, and assert none of them reaches any command.
: > "$LOG"
native_swap_reset
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  WORKER_REFRESH_DISK_FREE_GB_MIN=45 \
  WORKER_REFRESH_DISK_PATH=/var/lib/docker \
  WORKER_SLOTS=2 \
  WORKER_MODES="container, host" \
  WORKER_KVM=1 WORKER_KVM_PROJECTS="acme/beacon" \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  WORKER_FLUTTER_DIR=/etc/chug/flutter WORKER_JDK_DIR=/etc/chug/jdk \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  RUST_LOG=debug \
  sh "$SUT" swap prod > "$OUT" 2>&1

if grep -qE "(-e |--device|-v )" "$LOG"; then
  fail "the swap must compose no docker run spec at all (mounts, devices and -e are gone)"
fi
if grep -qE "WORKER_MODES|WORKER_KVM|WORKER_NIX|WORKER_SLOTS=|RUST_LOG" "$LOG"; then
  fail "the swap must carry no environment forward — the environment file declares it"
fi
# The live daemon is never inspected either: there is no second copy of the run
# spec to recover, so there is nothing to ask it.
if grep -qF "docker inspect" "$LOG"; then
  fail "the swap must not inspect the live daemon (nothing is recovered from it any more)"
fi
grep_log "systemctl restart --no-block chug-worker.service"
echo "ok: the swap carries nothing forward and inspects nothing"

# ── Case 3c: installed by RENAME, never over the running file ─────────────────
# Load-bearing twice: `install` straight onto $DAEMON_BIN is ETXTBSY while the
# daemon is executing it, and straight onto the refresh script truncates the file
# the running shell is reading by byte offset. A rename swaps the directory entry
# and leaves both open inodes alone.
grep_log "mv -f $DAEMON_BIN.chug-new $DAEMON_BIN"
grep_log "mv -f $NODE_SCRIPT.chug-new $NODE_SCRIPT"
if grep -F "install " "$LOG" | grep -qvE '\.chug-new$'; then
  fail "install must target <path>.chug-new, never the live path itself (ETXTBSY / truncation)"
fi
echo "ok: the three artifacts are installed beside their targets and renamed over them"

# ── Case 3d: an UN-CONVERTED node refuses, and says how to convert it ─────────
# The fleet is mixed until an operator converts each node (design #440's slice
# ordering), so a daemon that is still a container is an expected state, not a
# broken one. It cannot be swapped natively — there is no unit here, and a binary
# installed into a container's writable layer vanishes with it — so it refuses
# with the live daemon untouched and names build-worker.sh. Nothing is extracted
# and nothing is installed: the refusal is the FIRST thing the phase does.
: > "$LOG"
native_swap_reset
MARKER="$WORK/dockerenv"
: > "$MARKER"
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$MARKER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a node still running the containerized daemon must refuse the swap"
grep_out "REFUSING swap"
grep_out "deploy/prod/build-worker.sh"
[ -e "$DAEMON_BIN" ] && fail "a refused swap must install nothing"
if grep -qE "docker (create|cp|run|rm)" "$LOG"; then
  fail "the un-converted refusal must come before any docker mutation"
fi
if grep -qF "systemctl" "$LOG"; then
  fail "a container node has no unit to restart"
fi
echo "ok: an un-converted node refuses the swap and names the conversion"

# ── Case 3e: a unit that is not active refuses (never TWO daemons) ────────────
# `systemctl restart` on a unit this process does not belong to would start a
# second daemon beside the running one — one machine as two fleet rows, with
# nothing summing their slots (design #440 §1, #372 §8 R2). Refuse instead, with
# the live daemon serving and nothing installed.
: > "$LOG"
native_swap_reset
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  FAKE_UNIT_INACTIVE=1 \
  sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an inactive unit must refuse the swap (a restart would double the daemon)"
grep_out "REFUSING swap"
[ -e "$DAEMON_BIN" ] && fail "a refused swap must install nothing"
if grep -qF "systemctl restart" "$LOG"; then
  fail "the refusal must come before the restart"
fi
echo "ok: a daemon that is not the unit's refuses rather than starting a second one"

# ── Case 3f: an extraction that produced nothing refuses before installing ────
# A stale or broken image would otherwise put a zero-byte file where the daemon
# binary goes, and the supervisor would restart the node into an exec failure it
# loops on — the node-down hazard every refusal in this file exists for.
: > "$LOG"
native_swap_reset
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  FAKE_CP_EMPTY=1 \
  sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an empty extraction must refuse the swap"
grep_out "REFUSING swap"
[ -e "$DAEMON_BIN" ] && fail "an empty extraction must install nothing"
if grep -qF "systemctl restart" "$LOG"; then
  fail "the refusal must come before the restart"
fi
echo "ok: an empty extraction refuses instead of installing a daemon that cannot run"

# ── Case 3g: macOS asks launchd, and installs the daemon IT COMPILED ──────────
# The other supervisor (design #440 D2): a GUI-domain agent, restarted with the
# `launchctl kickstart -k` the macOS D3 proof exercised firsthand
# (docs/reference/runbooks/macos-host-supervision-proof.md). And the other
# PROVENANCE (#440 D6, corrected 2026-08-07): the worker image is a Linux
# container, so on a mac `docker cp` yields an ELF file launchd loops on with
# `cannot execute binary file` — the air, 2026-08-06. The build phase compiled a
# Mach-O daemon instead and this installs that.
: > "$LOG"
native_swap_reset
mac_build_stage abc123
PATH="$MACBIN:$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  WORKER_BUILD_DIR="$MAC_BUILD_DIR" \
  sh "$SUT" swap prod > "$OUT" 2>&1

grep_log "launchctl print gui/$(id -u)/com.chuggernaut.worker"
grep_log "launchctl kickstart -k gui/$(id -u)/com.chuggernaut.worker"
[ -x "$DAEMON_BIN" ] || fail "the macOS swap installs the same three artifacts"
grep -qF "# staged worker-refresh.sh" "$NODE_SCRIPT" \
  || fail "the macOS swap must install the node's OWN build, not the image's copy"
if grep -qF "docker create chuggernaut/worker" "$LOG"; then
  fail "a Darwin node must not extract its daemon from the Linux worker image"
fi
if grep -qF "systemctl" "$LOG"; then
  fail "a Darwin node must not be asked to drive systemd"
fi
echo "ok: a macOS node installs the daemon it compiled and kickstarts its launchd agent"

# ── Case 3g1: a staging directory from ANOTHER generation refuses ──────────────
# Existence is not evidence: a native build left over from an earlier refresh
# looks exactly like this one's, and installing it takes the node BACKWARDS with
# nothing to say so. The image's own chug.git.sha label is the second opinion.
: > "$LOG"
native_swap_reset
mac_build_stage deadbeef
set +e
PATH="$MACBIN:$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  WORKER_BUILD_DIR="$MAC_BUILD_DIR" \
  sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a stale native staging directory must refuse the swap"
grep_out "REFUSING swap"
[ -e "$DAEMON_BIN" ] && fail "a refused swap must install nothing"
if grep -qF "launchctl kickstart" "$LOG"; then
  fail "the refusal must come before the restart"
fi
echo "ok: a native build staged for another SHA refuses instead of taking the node backwards"

# ── Case 3g2: a staged binary that cannot RUN here refuses, on either platform ─
# The generalisation of the finding (#309 P0 finding 6): provenance from a
# foreign platform installs perfectly and then loops under the supervisor. Driven
# on the Linux path deliberately — the image is bookworm and a node whose loader
# it does not match is the same failure with a different cause.
: > "$LOG"
native_swap_reset
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  FAKE_BAD_BINARY=1 \
  sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a staged binary that cannot exec must refuse the swap"
grep_out "does not run on this node"
[ -e "$DAEMON_BIN" ] && fail "a refused swap must install nothing"
if grep -qF "systemctl restart" "$LOG"; then
  fail "the refusal must come before the restart"
fi
echo "ok: a staged daemon that cannot exec here refuses before it is installed"

# ── Case 3h: no supervisor to drive ⇒ refuse, install nothing ─────────────────
# A node whose daemon cannot reach its own supervisor cannot be restarted, so
# installing the binary would leave a node running an old daemon over a new one
# on disk with nothing to say so. Exercised on the launchd side because "no
# launchctl" is decidable on any Linux test host.
: > "$LOG"
native_swap_reset
set +e
PATH="$NOLCBIN:$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a node with no launchctl must refuse the swap"
grep_out "REFUSING swap"
[ -e "$DAEMON_BIN" ] && fail "a refused swap must install nothing"
echo "ok: a node that cannot reach its supervisor refuses before installing anything"

# ── Case 3i: an install path this node cannot write refuses before extracting ──
# /usr/local is root's on BOTH platforms, and on macOS the daemon is a GUI-domain
# agent running as the login user — so "the daemon cannot write where it must
# install" is a real node, not a hypothetical. Without the pre-flight it surfaces
# as a bare EACCES from half-way through the install, on a node whose images the
# build phase has already retag-swapped. Run as an unprivileged user because root
# bypasses every mode bit.
: > "$LOG"
chmod 0666 "$LOG"
UNWRITABLE="$WORK/root-only"
mkdir -p "$UNWRITABLE"
chmod 0555 "$UNWRITABLE"
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$UNWRITABLE/chuggernaut" \
  WORKER_CHANNEL_BINARY="$UNWRITABLE/chuggernaut-channel" \
  WORKER_REFRESH_SCRIPT="$UNWRITABLE/worker-refresh.sh" \
  $DROP sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an install path the daemon cannot write must refuse the swap"
grep_out "REFUSING swap"
grep_out "$UNWRITABLE"
[ -z "$(ls -A "$UNWRITABLE")" ] || fail "a refused swap must install nothing"
if grep -qE "docker (create|cp|rm)" "$LOG"; then
  fail "the unwritable-path refusal must come before any extraction"
fi
if grep -qF "systemctl restart" "$LOG"; then
  fail "the refusal must come before the restart"
fi
echo "ok: an install path the daemon cannot write refuses before anything is extracted"

# ── Case 3j: an install that needs privilege escalates, as build-worker.sh does ─
# The binaries go to /usr/local, which is root's, and the daemon need not be — so
# each write is "unprivileged first, `sudo -n` as the fallback", the same shape
# `chug_dir`/`chug_put` use over ssh. Modelled with an `install` that fails
# unless it came through `sudo`, which is what EACCES looks like from here.
: > "$LOG"
native_swap_reset
SUDOBIN="$WORK/sudobin"
mkdir -p "$SUDOBIN"
cat > "$SUDOBIN/install" <<EOF
#!/bin/sh
echo "install \$*" >> "$LOG"
[ -n "\${CHUG_VIA_SUDO:-}" ] || exit 1
exec "$REAL_INSTALL" "\$@"
EOF
cat > "$SUDOBIN/sudo" <<EOF
#!/bin/sh
echo "sudo \$*" >> "$LOG"
if [ "\$1" = "-n" ]; then shift; fi
export CHUG_VIA_SUDO=1
exec "\$@"
EOF
chmod +x "$SUDOBIN/install" "$SUDOBIN/sudo"
set +e
PATH="$SUDOBIN:$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  sh "$SUT" swap prod > "$OUT" 2>&1
rc=$?
set -e
[ "$rc" -eq 0 ] || fail "a denied plain write must escalate to 'sudo -n', not fail the swap"
grep_log "sudo -n install -m 0755"
for f in "$DAEMON_BIN" "$CHANNEL_BIN" "$NODE_SCRIPT"; do
  [ -x "$f" ] || fail "the swap must install $f through sudo when the plain write is denied"
done
grep_log "systemctl restart --no-block chug-worker.service"
echo "ok: an install the daemon's own user cannot do escalates to 'sudo -n' rather than dying"

# ── Case 3k: the staging dir is gone before the restart, not by the EXIT trap ──
# `systemctl restart` kills the cgroup this shell is in, and POSIX sh runs no
# EXIT trap when it is killed by a signal — so a staging dir left to the trap is
# tens of MB of binaries stranded on every successful refresh, on a node whose
# docker-disk headroom is the thing this script exists to defend. The fake
# systemctl does to the caller what the real one does.
: > "$LOG"
native_swap_reset
rm -rf "$STAGE_TMP"
mkdir -p "$STAGE_TMP"
set +e
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  TMPDIR="$STAGE_TMP" \
  FAKE_KILL_ON_RESTART=1 \
  sh "$SUT" swap prod > "$OUT" 2>&1
set -e
[ -x "$DAEMON_BIN" ] || fail "the swap must still have installed the daemon"
grep_log "systemctl restart --no-block chug-worker.service"
stage_tmp_empty || fail "the swap must remove its staging dir before asking for the restart"
echo "ok: the staging dir is reclaimed before the restart, which no EXIT trap survives"

# ── Case 3m: every phase REPORTS the run spec it is running (ticket #390) ─────
# This script's config is INHERITED from the daemon's own environment, which
# since design #440 D6 is the node's environment file loaded by the supervisor —
# so it is a DECLARATION now, not a value circulating from one container
# generation to the next. The report stays because the node is still the only
# thing that can say what it is running, and the dispatcher host cannot ssh a
# tagged worker: the daemon relays these lines into the deploy's task output.
: > "$LOG"
native_swap_reset
PATH="$BIN:$PATH" \
  WORKER_NODE=air \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  WORKER_SLOTS=2 \
  WORKER_CACHE_DIR=/Users/op/chuggernaut-worker/sccache \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  WORKER_GIT_KEY=/etc/chuggernaut/keys/worker_git \
  sh "$SUT" swap prod > "$OUT" 2>&1

grep_out "run spec on air (swap): WORKER_SLOTS=2 WORKER_CACHE_DIR=/Users/op/chuggernaut-worker/sccache WORKER_REFRESH_GIT_URL=ssh://git@front:2222/acme/chug.git WORKER_GIT_KEY=/etc/chuggernaut/keys/worker_git"
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
: > "$LOG"
native_swap_reset
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  sh "$SUT" swap prod > "$OUT" 2>&1

grep_out "WARNING: WORKER_CACHE_DIR is unset"
grep_out "WARNING: WORKER_SLOTS is unset"
grep_out "run spec on nuc (swap): WORKER_SLOTS=<unset> WORKER_CACHE_DIR=<unset>"
# Reporting is all it does: a node missing a value must still swap, or a deploy
# would leave it stranded on the old SHA over a line of config.
grep_log "systemctl restart --no-block chug-worker.service"
echo "ok: an unset cache dir or capacity is named out loud, and still swaps"

# ── Case 3o: the swap ANNOUNCES its phases, like every other long step ────────
# The marker prefix is a contract with the daemon (`REFRESH_PHASE_MARKER` in
# crates/worker/src/daemon.rs), which relays the current phase into the deploy's
# task output. The swap is the moment a node is most likely to break, and the
# restart is the last thing that happens before the daemon reporting it dies —
# so each step is announced BEFORE it runs.
: > "$LOG"
native_swap_reset
PATH="$BIN:$PATH" \
  WORKER_NODE=nuc \
  WORKER_SWAP_CONTAINER_MARKER="$NOT_CONTAINER" \
  WORKER_DAEMON_BIN="$DAEMON_BIN" \
  WORKER_CHANNEL_BINARY="$CHANNEL_BIN" \
  WORKER_REFRESH_SCRIPT="$NODE_SCRIPT" \
  sh "$SUT" swap prod > "$OUT" 2>&1
grep_out "worker-refresh: phase swap-extract"
grep_out "worker-refresh: phase swap-install"
grep_out "worker-refresh: phase swap-restart"
# The line that tells an operator where the rest of the story is must be printed
# BEFORE the restart request: after it, this process has no guaranteed quantum.
case "$(tr '\n' '|' < "$OUT")" in
  *"asking systemd to restart chug-worker.service"*) ;;
  *) fail "the swap must say what it is about to ask the supervisor for" ;;
esac
grep_out "journalctl -u chug-worker"
echo "ok: the swap announces extract, install and restart before each runs"

# ── Case 4: unknown phase is a hard error ────────────────────────────────────
if PATH="$BIN:$PATH" sh "$SUT" frobnicate 2>/dev/null; then
  fail "unknown phase should exit non-zero"
fi
echo "ok: unknown phase rejected"

echo "ALL PASS"
