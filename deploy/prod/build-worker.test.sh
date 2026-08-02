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
#     $FAIL_PROBE is set, so a case can drive the probe to time out;
#   * the cache-dir provisioning (`mkdir -p …`) succeeds unless $FAIL_MKDIR is
#     set, which models a node where neither the login user nor `sudo -n` can
#     create the path;
#   * the nix preconditions (`[ -d '/nix/store' ] …`) and the toolchain-shape
#     probe (`[ -L … ]`) succeed unless $FAIL_NIX_PRECHECK / $FAIL_SDK_PRECHECK
#     is set.
cat > "$BIN/ssh" <<EOF
#!/bin/sh
cat >/dev/null 2>&1 || true
echo "ssh \$*" >> "$LOG"
case "\$*" in
  *chug.git.sha*)  echo "\${FAKE_LABEL:-deadbeefcafe}" ;;
  *State.Running*) [ -n "\${FAIL_PROBE:-}" ] || echo HEALTHY ;;
  *mkdir*)         [ -z "\${FAIL_MKDIR:-}" ] || exit 1 ;;
  *"[ -d '/nix/store' ]"*) [ -z "\${FAIL_NIX_PRECHECK:-}" ] || exit 1 ;;
  *"[ -L "*)       [ -z "\${FAIL_SDK_PRECHECK:-}" ] || exit 1 ;;
esac
exit 0
EOF

chmod +x "$BIN/git" "$BIN/ssh"

fail() { echo "FAIL: $1" >&2; exit 1; }
grep_log() { grep -qF -- "$1" "$LOG" || fail "expected in log: $1"; }

# Line number of the first log entry containing $1, for the ORDER assertions
# (a cache dir created after the daemon is already running provisions nothing in
# time — the daemon must find the path there).
line_of() { grep -nF -- "$1" "$LOG" | head -n 1 | cut -d: -f1; }

# The daemon `docker run` as the node's shell will see it, on one line with runs
# of whitespace squeezed: the script composes it across backslash-continued
# lines, so an assertion on the WHOLE run spec (rather than one token of it) has
# to normalise the layout it is written in.
daemon_run() {
  sed -n '/docker run -d --restart=always --name chug-worker/,/chuggernaut\/worker:/p' "$LOG" \
    | tr '\\\n' '  ' | tr -s ' '
}

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

# ── Case 1b: cache on ⇒ the HOST directory is created, before the daemon runs ──
# Nothing else creates it: the daemon's own create_dir_all runs inside the daemon
# container (which does not mount this path), and since #379 the cache is a typed
# mount whose missing source the engine REFUSES — so an unprovisioned node fails
# every launch, permanently. The `sudo -n` fallback covers a first create under a
# root-owned parent; plain `mkdir -p` succeeds unprivileged on every node that
# already has the dir.
grep_log "mkdir -p '/var/cache/chuggernaut/sccache'"
grep_log "sudo -n mkdir -p '/var/cache/chuggernaut/sccache'"
mkdir_line="$(line_of "mkdir -p '/var/cache/chuggernaut/sccache'")"
run_line="$(line_of "docker run -d --restart=always --name chug-worker")"
[ -n "$mkdir_line" ] && [ -n "$run_line" ] || fail "expected both a mkdir and a daemon run in the log"
[ "$mkdir_line" -lt "$run_line" ] || fail "the cache dir must be created BEFORE the daemon is started"
echo "ok: WORKER_CACHE_DIR's host dir is provisioned on the node before the daemon starts"

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
# Caching off ⇒ nothing to provision: a node that asked for no cache must not
# acquire a directory (nor a `sudo` call) from a deploy.
if grep -qF "mkdir" "$LOG"; then
  fail "no cache dir may be created when WORKER_CACHE_DIR is unset"
fi
echo "ok: no WORKER_CACHE_DIR, WORKER_REFRESH_DISK_* or WORKER_SLOTS passed when unset, and no dir created"

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

# ── Case 2d: KVM unset ⇒ the daemon run is exactly the one it was before ──────
# The three #367 settings and the device must be INERT until an operator turns
# them on: this is the whole fleet's run spec, and every node in it has KVM off.
# Asserted on the WHOLE composed run rather than on absent tokens, so a stray
# flag anywhere in it fails here. Update this string deliberately when the run
# spec changes — hand-composing it var by var is what drops settings (#265
# reason 3), and a golden is the cheapest guard the shape has.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"

EXPECTED_RUN="docker run -d --restart=always --name chug-worker\
 -v /var/run/docker.sock:/var/run/docker.sock\
 -v \$HOME/chuggernaut-worker/keys:/data/keys:ro\
 -e WORKER_NODE=nuc -e NATS_URL=nats://10.0.0.1:4222\
 -e NATS_CREDS=/data/keys/worker.creds\
 -e RUST_LOG=info,async_nats=warn\
 -e WORKER_REFRESH_GIT_URL= -e WORKER_GIT_KEY=/data/keys/worker_git\
 chuggernaut/worker:prod >/dev/null"
GOT_RUN="$(daemon_run)"
case "$GOT_RUN" in
  "$EXPECTED_RUN"*) ;;
  *) fail "with the KVM vars unset the daemon run must be unchanged.
  expected: $EXPECTED_RUN
  got:      $GOT_RUN" ;;
esac
echo "ok: KVM unset leaves the daemon run spec byte-for-byte what it was"

# ── Case 2e: KVM on ⇒ the three settings AND the device reach the daemon ──────
# The device is the load-bearing half: chug-worker is itself a container, so the
# daemon's device check reads its OWN view (crates/worker/src/daemon.rs) and a
# daemon given WORKER_KVM without `--device` refuses to start, is looped by
# --restart=always, and takes the node out of the fleet.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS="acme/beacon,acme/api" \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  sh "$SUT"

grep_log "-e WORKER_KVM='1'"
grep_log "-e WORKER_KVM_PROJECTS='acme/beacon,acme/api'"
grep_log "-e WORKER_ANDROID_SDK_DIR='/etc/chug/android-sdk'"
grep_log "--device '/dev/kvm'"
echo "ok: KVM on passes the three settings and the /dev/kvm device"

# ── Case 2e2: an allow-list written with spaces stays ONE argument ─────────────
# The daemon trims each entry (crates/worker/src/config.rs `parse_kvm_projects`),
# so `acme/beacon, acme/api` is a valid list an operator will write. Unquoted it
# word-splits and the tail lands after the image name — i.e. as the container's
# COMMAND, which starts something other than the daemon.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS="acme/beacon, acme/api" \
  sh "$SUT"

grep_log "-e WORKER_KVM_PROJECTS='acme/beacon, acme/api'"
case "$(daemon_run)" in
  *"chuggernaut/worker:prod >/dev/null"*) ;;
  *) fail "the image must stay the LAST argument of the run (a split allow-list would follow it as a command)" ;;
esac
echo "ok: a spaced allow-list stays one argument and nothing follows the image"

# ── Case 2f: WORKER_KVM may name another device node (#374's parse) ───────────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=/dev/kvm1 \
  sh "$SUT"

grep_log "--device '/dev/kvm1'"
grep_log "-e WORKER_KVM='/dev/kvm1'"
echo "ok: an absolute WORKER_KVM names the device that is passed through"

# ── Case 2g: WORKER_KVM=0 is OFF ⇒ the setting rides, the device must not ──────
# The daemon reads 0/false/off as no passthrough at all, so attaching the device
# for it would hand a node hardware its own config says it is not using.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=0 \
  sh "$SUT"

grep_log "-e WORKER_KVM='0'"
if grep -qF -- "--device" "$LOG"; then
  fail "WORKER_KVM=0 is off — no device may be attached"
fi
echo "ok: WORKER_KVM=0 passes the setting and attaches no device"

# ── Case 2h: an unparseable WORKER_KVM is refused BEFORE the daemon restart ───
# The daemon rejects anything that is neither a boolean nor an absolute path
# (crates/worker/src/config.rs) — so passing it through would `docker rm -f` a
# working daemon and replace it with one that cannot boot. Refuse instead; the
# live daemon is left running.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=yes \
  sh "$SUT" >"$WORK/kvm.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unparseable WORKER_KVM must fail the deploy (got rc=0)"
grep -qF "neither 1/0 nor an absolute device path" "$WORK/kvm.out" || fail "the refusal must name what is wrong"
if grep -qF "docker run -d --restart=always --name chug-worker" "$LOG"; then
  fail "an unparseable WORKER_KVM must not reach the daemon restart (live daemon untouched)"
fi
echo "ok: an unparseable WORKER_KVM refuses before the daemon is replaced"

# ── Case 2i: WORKER_KVM is trimmed exactly as the daemon trims it ─────────────
# `parse_kvm_device` trims before matching, so ` 1 ` is a value the daemon
# accepts; without the same trim here the deploy would refuse it as unparseable
# (case 2h) and a node an operator configured correctly could never be built.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=" 1 " \
  sh "$SUT"

grep_log "-e WORKER_KVM='1'"
grep_log "--device '/dev/kvm'"

# Whitespace-only is what the daemon reads as unset, so it must produce the
# untouched run of case 2d rather than an unparseable-value refusal.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM="  " \
  sh "$SUT"

if grep -qE -- "WORKER_KVM|--device" "$LOG"; then
  fail "a whitespace-only WORKER_KVM is unset to the daemon — it must add nothing"
fi
echo "ok: WORKER_KVM is trimmed the way the daemon trims it"

# ── Case 2j: an unprovisionable cache dir refuses BEFORE the daemon restart ───
# A daemon started against a host path that does not exist comes up healthy and
# then fails EVERY launch ("bind source path does not exist"), which reads as a
# broken node rather than as a misconfigured deploy. Refuse instead, while the
# working daemon is still running, and name both attempts in the message.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  FAIL_MKDIR=1 \
  sh "$SUT" >"$WORK/cache.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unprovisionable WORKER_CACHE_DIR must fail the deploy (got rc=0)"
grep -qF "cannot provision WORKER_CACHE_DIR" "$WORK/cache.out" || fail "the refusal must name what is wrong"
if grep -qF "docker run -d --restart=always --name chug-worker" "$LOG"; then
  fail "an unprovisionable WORKER_CACHE_DIR must not reach the daemon restart (live daemon untouched)"
fi
echo "ok: an unprovisionable cache dir refuses before the daemon is replaced"

# ── Case 2k: nix roots on ⇒ the four mounts, the settings, and the roots dir ───
# design #373 P1. Each mount is load-bearing and the daemon refuses to boot
# without the roots dir, the client and the socket in its OWN view, so a run spec
# missing one takes the node out of the fleet (--restart=always loops the
# refusal). The CLIENT rides through the profiles rather than the store: a
# store-path client is pinned to the generation current at the last swap, and
# `--delete-older-than` collects it out from under a long-lived daemon.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_NIX_REALISE_TIMEOUT_SECS=40 \
  sh "$SUT"

grep_log "-e WORKER_NIX_GCROOTS_DIR='/var/lib/chuggernaut/gcroots'"
grep_log "-e WORKER_NIX_REALISE_TIMEOUT_SECS='40'"
grep_log "-v '/nix/store':'/nix/store':ro"
grep_log "-v '/nix/var/nix/profiles':'/nix/var/nix/profiles':ro"
grep_log "-v '/nix/var/nix/daemon-socket':'/nix/var/nix/daemon-socket'"
grep_log "-v '/var/lib/chuggernaut/gcroots':'/var/lib/chuggernaut/gcroots'"
# The socket mount must be WRITABLE — connecting to a unix socket needs write on
# the inode, so a :ro parent would fail every realise.
if grep -qF -- "-v '/nix/var/nix/daemon-socket':'/nix/var/nix/daemon-socket':ro" "$LOG"; then
  fail "the nix daemon socket must be mounted read-write"
fi
# The roots dir is provisioned HERE, before the daemon starts: the daemon's own
# view is a container, so nothing it does reaches the host path (#380's lesson,
# and #372 §5 A5 declines to own this).
grep_log "mkdir -p '/var/lib/chuggernaut/gcroots'"
grep_log "sudo -n mkdir -p '/var/lib/chuggernaut/gcroots'"
roots_line="$(line_of "mkdir -p '/var/lib/chuggernaut/gcroots'")"
run_line="$(line_of "docker run -d --restart=always --name chug-worker")"
[ "$roots_line" -lt "$run_line" ] || fail "the gcroots dir must be created BEFORE the daemon starts"
case "$(daemon_run)" in
  *"chuggernaut/worker:prod >/dev/null"*) ;;
  *) fail "the image must stay the LAST argument of the run" ;;
esac
echo "ok: nix roots on passes the four mounts + settings and provisions the roots dir first"

# ── Case 2l: nix roots unset ⇒ nothing nix reaches the node ────────────────────
# The whole fleet is here until an operator turns roots on: no mount, no env, no
# probe, no directory.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"

if grep -qE "WORKER_NIX|/nix/store|/nix/var" "$LOG"; then
  fail "nothing nix may reach a node with WORKER_NIX_GCROOTS_DIR unset"
fi
echo "ok: WORKER_NIX_GCROOTS_DIR unset leaves the daemon run nix-free"

# ── Case 2m: an unprovisionable roots dir refuses BEFORE the daemon restart ────
# Same shape as the cache dir (#380): a daemon started without its roots dir
# refuses to boot, is looped by --restart=always, and the node leaves the fleet.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  FAIL_MKDIR=1 \
  sh "$SUT" >"$WORK/nixroots.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unprovisionable gcroots dir must fail the deploy (got rc=0)"
grep -qF "cannot provision WORKER_NIX_GCROOTS_DIR" "$WORK/nixroots.out" \
  || fail "the refusal must name what is wrong"
if grep -qF "docker run -d --restart=always --name chug-worker" "$LOG"; then
  fail "an unprovisionable gcroots dir must not reach the daemon restart"
fi
echo "ok: an unprovisionable gcroots dir refuses before the daemon is replaced"

# ── Case 2n: a node with no nix daemon refuses too, and a relative path does ───
# The mounts are only sound on a node that actually has a store, a profiles tree
# and a daemon socket; a node without them would take the four mounts and then
# fail the daemon's own boot check.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  FAIL_NIX_PRECHECK=1 \
  sh "$SUT" >"$WORK/nixpre.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a node without a nix daemon must fail the deploy (got rc=0)"
grep -qF "lacks the nix preconditions" "$WORK/nixpre.out" || fail "the refusal must name what is missing"

: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=relative/gcroots \
  sh "$SUT" >"$WORK/nixrel.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a relative gcroots dir must fail the deploy (got rc=0)"
grep -qF "is not an absolute host path" "$WORK/nixrel.out" || fail "the refusal must name what is wrong"
if grep -qF "docker run -d --restart=always --name chug-worker" "$LOG"; then
  fail "a relative gcroots dir must not reach the daemon restart"
fi
echo "ok: a nix-less node and a relative roots dir both refuse before the daemon is replaced"

# ── Case 2o: roots + a KVM device ⇒ the toolchain's PARENT is mounted too ──────
# `nix-store --realise` resolves its argument CLIENT-side, inside chug-worker,
# before the nix daemon hears anything, and the operator's stable path is a
# symlink INTO the store. Binding that path itself destroys the symlink —
# mount(2) resolves the source host-side — leaving the store path's content at a
# non-store path the client refuses. Binding the parent keeps it readable as a
# symlink, resolving through the store mount, and following the node across a
# `nixos-rebuild` rather than pinning this deploy's generation.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS=acme/beacon \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  sh "$SUT"

grep_log "-v '/etc/chug':'/etc/chug':ro"
# And never the leaf: that is the bind whose source the kernel resolves.
if grep -qF -- "-v '/etc/chug/android-sdk':" "$LOG"; then
  fail "binding the stable path itself resolves the symlink away — mount its parent"
fi
# The probe demands the SHAPE the mount depends on, not mere existence: a direct
# absolute symlink into the store, under a real parent directory.
grep_log "[ -L '/etc/chug/android-sdk' ]"
grep_log "readlink '/etc/chug/android-sdk'"
grep_log "in '/nix/store'/*)"

# The default toolchain path is the one the daemon's own config defaults to, so a
# node that never set WORKER_ANDROID_SDK_DIR still gets the realise target it
# will actually be handed.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=1 \
  sh "$SUT"
grep_log "-v '/var/lib/chuggernaut':'/var/lib/chuggernaut':ro"

# A node with roots on but NO device realises nothing, so it takes no toolchain
# mount — WORKER_KVM=0 is a deliberate configuration, not a half-enabled one.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=0 \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  sh "$SUT"
if grep -qF -- "-v '/etc/chug'" "$LOG"; then
  fail "a node with no device realises nothing and must take no toolchain mount"
fi

# And an SDK path whose SHAPE the mount cannot carry — a plain directory, or a
# NixOS environment.etc entry hopping through /etc/static — refuses the deploy
# rather than booting a daemon whose own check will refuse it forever.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=1 \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  FAIL_SDK_PRECHECK=1 \
  sh "$SUT" >"$WORK/nixsdk.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unrealisable toolchain path must fail the deploy (got rc=0)"
grep -qF "'/etc/chug/android-sdk' on worksalot@nuc is not a direct symlink into '/nix/store'" \
  "$WORK/nixsdk.out" || fail "the refusal must name the path and the shape it needs"
grep -qF "systemd.tmpfiles.rules" "$WORK/nixsdk.out" || fail "the refusal must name the remedy"
if grep -qF "docker run -d --restart=always --name chug-worker" "$LOG"; then
  fail "an unrealisable toolchain path must not reach the daemon restart"
fi

# A toolchain path directly under / would need the node's root filesystem bound
# into chug-worker to be readable as a symlink. Refused on sight, no ssh needed.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=1 \
  WORKER_ANDROID_SDK_DIR=/android-sdk \
  sh "$SUT" >"$WORK/nixroot.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a toolchain path under / must fail the deploy (got rc=0)"
grep -qF "sits directly under /" "$WORK/nixroot.out" || fail "the refusal must say why"
echo "ok: roots + a device mount the toolchain's PARENT, and a shape that cannot work refuses"

# ── Case 2p: a realise bound the launch RPC cannot contain is REFUSED ──────────
# The realise runs inside the `launch` RPC the dispatcher abandons after 60s, so
# the daemon refuses a longer bound at parse time (config.rs
# NIX_REALISE_TIMEOUT_SECS_MAX). Catching it here keeps the failure a failed
# deploy instead of a node looping --restart=always out of the fleet.
for bad in 900 0 soon; do
  : > "$LOG"
  set +e
  PATH="$BIN:$PATH" \
    WORKER_SSH=worksalot@nuc \
    WORKER_NATS_URL=nats://10.0.0.1:4222 \
    CHUG_WORKER_NODE=nuc \
    WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
    WORKER_NIX_REALISE_TIMEOUT_SECS="$bad" \
    sh "$SUT" >"$WORK/nixbound.out" 2>&1
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "WORKER_NIX_REALISE_TIMEOUT_SECS=$bad must fail the deploy (got rc=0)"
  grep -qF "WORKER_NIX_REALISE_TIMEOUT_SECS" "$WORK/nixbound.out" \
    || fail "the refusal must name the bound ($bad)"
  if grep -qF "docker run -d --restart=always --name chug-worker" "$LOG"; then
    fail "an unusable realise bound ($bad) must not reach the daemon restart"
  fi
done
echo "ok: a realise bound outside the launch RPC's budget refuses before the daemon is replaced"

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
