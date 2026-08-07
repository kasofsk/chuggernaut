#!/bin/sh
# Shell test for build-worker.sh — no Docker, no ssh, no worker node.
#
# It drives build-worker.sh with fake `ssh` and `git` on PATH that just log
# their invocations, then asserts what the node would be given: the ENVIRONMENT
# FILE the supervisor hands the daemon (design #440 D2), the systemd unit or
# launchd agent that supervises it, and the drift guard that refuses a run spec
# dropping a setting the live daemon carries (#390, #440 D7). In particular
# WORKER_CACHE_DIR (the #55 dormant-cache fix), the self-refresh coordinates,
# WORKER_MODES and the node identity — so a hand-run or scripted (re)deploy never
# installs a daemon with caching, refresh or a declared runtime silently dropped.
# Same spirit as worker-refresh.test.sh / restart-verify.test.sh.
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
# install script that writes the environment file and the unit. It also answers
# the probes the script runs over ssh:
#   * the node platform probe (`uname -s` + $HOME + the toolchain) echoes
#     $FAKE_NODE_OS (default Linux) and $FAKE_NODE_HOME, which is what selects
#     systemd or launchd, then the three toolchain answers only a Darwin node
#     needs (its daemon binary is compiled ON the node, because the worker image
#     is Linux): $FAKE_NODE_CARGO, $FAKE_NODE_RUSTC — where `rustc` resolves
#     with cargo's own directory on PATH, which is the question the COMPILE asks
#     — and $FAKE_NODE_CARGO_RUNS, whether that cargo execs at all. Set any of
#     them EMPTY to model a mac the deploy cannot compile on;
#   * the docker-context probe (`docker context inspect`, Darwin only) echoes
#     $FAKE_DOCKER_CONTEXT, the endpoint the node's own docker CLI uses, and the
#     socket check (`[ -S …`) succeeds unless $FAIL_DOCKER_SOCKET is set;
#   * the CONTAINER-PLATFORM probe (`docker version --format`, Darwin only)
#     echoes $FAKE_DOCKER_PLATFORM, default `arm64/linux` — what colima on the
#     air answers, and what the injected channel binary must be built for. Set it
#     empty to model a node whose docker is not running;
#   * the native daemon build (`… build --release --locked …`) is a no-op that
#     is logged, so the assertions can read back exactly what was compiled;
#   * the image-label inspect (`...chug.git.sha...`) echoes $FAKE_LABEL, default
#     the SHA fake-git reports, so the label assert passes unless a case forces a
#     mismatch (the stale-image-label case);
#   * the daemon health probe (`systemctl is-active` / `launchctl print`) echoes
#     HEALTHY unless $FAIL_PROBE is set, so a case can drive it to time out.
#     $FAKE_STALE_LOG models the node this probe must NOT be fooled by: the
#     supervisor reports the daemon up (it is crash-looping under Restart=always
#     / KeepAlive, so it is `active` on most polls) and "worker up" is present
#     but only from an EARLIER generation. The fake answers HEALTHY exactly when
#     the probe would have read that stale line — a Linux read not scoped to the
#     unit's current InvocationID, or a macOS tail of a log the install did not
#     truncate — so an unbounded probe passes this node and a bounded one times
#     out;
#   * the cache-dir provisioning (`mkdir -p '…'`) succeeds unless $FAIL_MKDIR is
#     set, which models a node where neither the login user nor `sudo -n` can
#     create the path;
#   * the CREDENTIAL DIRECTORY probe (`printf 'own=%s mode=%s…`, Linux) answers
#     with $FAKE_KEYS_OWNER (default root) and $FAKE_KEYS_MODE (default 700), or
#     with both empty when $FAKE_KEYS_ABSENT is set — the directory is not there.
#     Its credential half is TRI-STATE for the reason the run-spec read is: in a
#     root-owned 0700 directory the login user cannot look, so "not there" and
#     "not allowed to look" are the same failed `test -r`. `creds=readable` by
#     default, `creds=absent` under $FAIL_CREDS, `creds=unknown` under
#     $FAKE_KEYS_NOSUDO (a node whose login user has no passwordless sudo);
#   * the macOS NATS credential check (`[ -r …worker.creds ]`) succeeds unless
#     $FAIL_CREDS is set — that platform runs the daemon as the login user, so
#     D5's root-owned directory does not port and the home path is still it;
#   * the unit-directory check (`command -v systemctl …`) succeeds unless
#     $FAIL_UNIT_DIR is set, which models NixOS's read-only /etc/systemd/system;
#   * the macOS binary-directory check (`for d in …`) succeeds unless
#     $FAIL_MAC_DIRS is set, which models a stock mac whose /usr/local is
#     root-owned and whose login user has no passwordless sudo;
#   * the LIVE run spec, which the node answers as one of THREE states because
#     "absent" and "unreadable" both look like empty output and collapsing them
#     is how the guard would degrade to a pass: `CHUG_SPEC_ABSENT` by default (no
#     environment file yet), $FAKE_LIVE_ENV_FILE's contents when it is set, and
#     `CHUG_SPEC_UNREADABLE` when $FAKE_ENV_FILE_UNREADABLE is — a file the login
#     user cannot `cat` and `sudo -n cat` cannot either. The pre-conversion
#     fallback (`…Config.Env…`, the live container) echoes $FAKE_LIVE_ENV, empty
#     by default: a node with no worker at all, and nothing that can drift;
#   * the nix preconditions (`[ -d '/nix/store' ] …`) and the toolchain-shape
#     probe (`readlink -f …`) succeed unless $FAIL_NIX_PRECHECK /
#     $FAIL_SDK_PRECHECK is set.
cat > "$BIN/ssh" <<EOF
#!/bin/sh
cat >/dev/null 2>&1 || true
echo "ssh \$*" >> "$LOG"
case "\$*" in
  *CHUG_WORKER_ENV*) ;;
  *uname*)
    printf '%s\n%s\n%s\n%s\n%s\n' "\${FAKE_NODE_OS:-Linux}" "\${FAKE_NODE_HOME:-/home/worksalot}" \
      "\${FAKE_NODE_CARGO-/opt/homebrew/bin/cargo}" \
      "\${FAKE_NODE_RUSTC-/opt/homebrew/bin/rustc}" "\${FAKE_NODE_CARGO_RUNS-ok}"
    ;;
  *"build --release --locked"*) ;;
  *"docker context inspect"*) printf '%s\n' "\${FAKE_DOCKER_CONTEXT-unix:///Users/op/.colima/default/docker.sock}" ;;
  *"docker version --format"*) printf '%s\n' "\${FAKE_DOCKER_PLATFORM-arm64/linux}" ;;
  *chug.git.sha*)  echo "\${FAKE_LABEL:-deadbeefcafe}" ;;
  *is-active*)
    if [ -n "\${FAKE_STALE_LOG:-}" ]; then
      case "\$*" in *_SYSTEMD_INVOCATION_ID*) ;; *) echo HEALTHY ;; esac
    else
      [ -n "\${FAIL_PROBE:-}" ] || echo HEALTHY
    fi
    ;;
  *"launchctl print"*)
    if [ -n "\${FAKE_STALE_LOG:-}" ]; then
      grep -qF ": > '" "$LOG" || echo HEALTHY
    else
      [ -n "\${FAIL_PROBE:-}" ] || echo HEALTHY
    fi
    ;;
  *"mkdir -p '"*)  [ -z "\${FAIL_MKDIR:-}" ] || exit 1 ;;
  *"own=%s mode=%s"*)
    if [ -n "\${FAKE_KEYS_ABSENT:-}" ]; then
      echo "own= mode="
    else
      echo "own=\${FAKE_KEYS_OWNER:-root} mode=\${FAKE_KEYS_MODE:-700}"
    fi
    if [ -n "\${FAKE_KEYS_NOSUDO:-}" ]; then
      echo creds=unknown
    elif [ -n "\${FAIL_CREDS:-}" ]; then
      echo creds=absent
    else
      echo creds=readable
    fi
    ;;
  *worker.creds*)  [ -z "\${FAIL_CREDS:-}" ] || exit 1 ;;
  *"command -v systemctl"*) [ -z "\${FAIL_UNIT_DIR:-}" ] || exit 1 ;;
  *"for d in "*)   [ -z "\${FAIL_MAC_DIRS:-}" ] || exit 1 ;;
  *"cat '"*worker.env"'"*)
    if [ -n "\${FAKE_ENV_FILE_UNREADABLE:-}" ]; then
      echo CHUG_SPEC_UNREADABLE
    elif [ -n "\${FAKE_LIVE_ENV_FILE:-}" ]; then
      printf '%s\n' "\${FAKE_LIVE_ENV_FILE}"
    else
      echo CHUG_SPEC_ABSENT
    fi
    ;;
  *Config.Env*)    [ -z "\${FAKE_LIVE_ENV:-}" ] || printf '%s\n' "\${FAKE_LIVE_ENV}" ;;
  *"[ -d '/nix/store' ]"*) [ -z "\${FAIL_NIX_PRECHECK:-}" ] || exit 1 ;;
  *"readlink -f"*) [ -z "\${FAIL_SDK_PRECHECK:-}" ] || exit 1 ;;
  *"[ -S '"*)      [ -z "\${FAIL_DOCKER_SOCKET:-}" ] || exit 1 ;;
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

# The one line that says "this node's daemon was (re)started", per supervisor.
# Everything that must happen with the LIVE daemon untouched asserts its absence.
STARTED_LINUX="sudo -n systemctl restart chug-worker.service"
STARTED_MACOS="launchctl bootstrap gui/\$(id -u)"
started() { grep -qF -- "$STARTED_LINUX" "$LOG" || grep -qF -- "$STARTED_MACOS" "$LOG"; }
not_started() { started && fail "$1"; :; }

# The three rendered files, read back out of the heredocs the install script
# carries. They are what the node ends up holding, so assertions on the WHOLE
# run spec (rather than one token of it) read them rather than the log.
heredoc() { sed -n "/<<'$1'\$/,/^$1\$/p" "$LOG" | sed '1d;$d'; }
env_file() { heredoc CHUG_WORKER_ENV; }
unit_file() { heredoc CHUG_WORKER_UNIT; }
plist_file() { heredoc CHUG_WORKER_PLIST; }

# ── Case 1: cache on ⇒ the env file carries WORKER_CACHE_DIR (env only) ────────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  sh "$SUT"

# The daemon is installed and supervised natively; nothing composes a `docker
# run` for it any more (design #440 D1/D2).
started || fail "the daemon must be (re)started through its supervisor"
if grep -qF -- "docker run -d --restart=always --name chug-worker" "$LOG"; then
  fail "the daemon must no longer be started with docker run (design #440 D2)"
fi
grep_log "WORKER_CACHE_DIR='/var/cache/chuggernaut/sccache'"
grep_log "WORKER_REFRESH_GIT_URL='ssh://git@front:2222/acme/chug.git'"
grep_log "WORKER_GIT_KEY='/etc/chuggernaut/keys/worker_git'"
grep_log "WORKER_NODE='nuc'"
# A daemon started with no RUST_LOG logs at ERROR only (ticket #270): no "worker
# up", no refresh relay, so the supervisor's log is silent about the one thing an
# operator needs it for. The node-creation path sets the same floor the
# self-refresh swap carries forward.
grep_log "RUST_LOG='info,async_nats=warn'"
# Env only: no bind of the cache into the daemon, which no longer has a view to
# bind anything into.
if grep -qF -- "-v /var/cache/chuggernaut/sccache:" "$LOG"; then
  fail "WORKER_CACHE_DIR must be env only on the daemon, not a bind-mount"
fi
echo "ok: the env file carries WORKER_CACHE_DIR (env only) + refresh coords + node"

# ── Case 1a: the daemon BINARY comes out of the image just built (D6) ─────────
# Extracted rather than compiled on the node: the build environment stays
# byte-identical to today's and no host Rust toolchain becomes a machine fact.
# The channel binary and the refresh script ride out of the same image, into the
# paths crates/worker/src/config.rs already defaults to — which is what makes
# those defaults correct on a host with no code change.
grep_log "docker create chuggernaut/worker:prod"
grep_log '"$CID:/usr/local/bin/chuggernaut"'
grep_log '"$CID:/usr/local/lib/chuggernaut/chuggernaut-channel"'
grep_log '"$CID:/usr/local/lib/chuggernaut/worker-refresh.sh"'
grep_log "chug_put 0755 \"\$STAGE/chuggernaut\" '/usr/local/bin/chuggernaut'"
grep_log "chug_put 0755 \"\$STAGE/chuggernaut-channel\" '/usr/local/lib/chuggernaut/chuggernaut-channel'"
grep_log "chug_put 0755 \"\$STAGE/worker-refresh.sh\" '/usr/local/lib/chuggernaut/worker-refresh.sh'"
echo "ok: the daemon binary, the channel binary and the refresh script are extracted from the image"

# ── Case 1b: cache on ⇒ the HOST directory is created, before the daemon runs ──
# A native daemon's own create_dir_all does reach the host, but it runs when the
# daemon starts — and the engine REFUSES a missing bind source since #379, so a
# launch admitted before that create fails. Provisioning first also keeps the
# directory's ownership and mode what the fleet has run on. The `sudo -n`
# fallback covers a first create under a root-owned parent.
grep_log "mkdir -p '/var/cache/chuggernaut/sccache'"
grep_log "sudo -n mkdir -p '/var/cache/chuggernaut/sccache'"
mkdir_line="$(line_of "mkdir -p '/var/cache/chuggernaut/sccache'")"
run_line="$(line_of "$STARTED_LINUX")"
[ -n "$mkdir_line" ] && [ -n "$run_line" ] || fail "expected both a mkdir and a daemon start in the log"
[ "$mkdir_line" -lt "$run_line" ] || fail "the cache dir must be created BEFORE the daemon is started"
echo "ok: WORKER_CACHE_DIR's host dir is provisioned on the node before the daemon starts"

# ── Case 2: cache unset ⇒ no WORKER_CACHE_DIR passed (caching stays off) ───────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"

started || fail "the daemon must be (re)started through its supervisor"
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
# And the runtimes the node offers: unset must stay UNSET, never an explicit
# `container`. The daemon already defaults to container-only, so forwarding that
# default would have every node advertise a value it never declared and make a
# future change of the default a silent no-op.
if grep -qF "WORKER_MODES" "$LOG"; then
  fail "WORKER_MODES must not be passed when unset (daemon default applies)"
fi
# Caching off ⇒ nothing to provision: a node that asked for no cache must not
# acquire a directory (nor a `sudo` call) from a deploy.
if grep -qF -- "mkdir -p '" "$LOG"; then
  fail "no cache dir may be created when WORKER_CACHE_DIR is unset"
fi
echo "ok: no WORKER_CACHE_DIR, WORKER_REFRESH_DISK_* or WORKER_SLOTS passed when unset, and no dir created"

# Also assert the label + health verification happened on the success path.
grep_log "chug.git.sha=deadbeefcafe"                 # SHA baked as an image LABEL
grep_log "docker inspect --format"                   # label read back for the assert
grep -F "ssh" "$LOG" | grep -qF "is-active"          # daemon health probe ran
echo "ok: success path bakes + verifies the image label and probes daemon health"

# ── Case 2a: a container-only node's whole run spec, as a golden ──────────────
# THE equivalence assertion for design #440 slice 4: every node in the fleet runs
# container mode only, and the daemon this installs must behave identically to
# the containerised one it replaces. So the whole environment file is compared,
# not a token of it — a stray or missing setting fails here. Update this string
# deliberately when the run spec changes; hand-composing it var by var is what
# drops settings (#265 reason 3), and a golden is the cheapest guard the shape
# has. The two paths that MOVED are the point: the keys are read off the node
# now, not out of a `:ro` bind at /data/keys.
EXPECTED_ENV="WORKER_NODE='nuc'
NATS_URL='nats://10.0.0.1:4222'
NATS_CREDS='/etc/chuggernaut/keys/worker.creds'
RUST_LOG='info,async_nats=warn'
WORKER_REFRESH_GIT_URL=''
WORKER_GIT_KEY='/etc/chuggernaut/keys/worker_git'"
GOT_ENV="$(env_file)"
[ "$GOT_ENV" = "$EXPECTED_ENV" ] || fail "a container-only node's environment file must be exactly the run spec.
  expected: $EXPECTED_ENV
  got:      $GOT_ENV"
echo "ok: a container-only node's environment file is exactly the composed run spec"

# ── Case 2a1: the unit is the machine fact, the env file is the run spec ──────
# design #440 D2's answer to #372 §8's R3. The unit holds nothing an operator
# tunes per project — a binary path, a restart policy, the environment file to
# read — which is what makes it slice 7's to hand to `nix/chug-node/`. PATH is
# in the unit because it is a machine fact too: the daemon shells out to git,
# ssh and docker, and systemd's default PATH names a NixOS node's copies of
# none of them.
EXPECTED_UNIT="[Unit]
Description=Chuggernaut worker daemon (nuc)
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=/etc/chuggernaut/worker.env
Environment=PATH=/run/current-system/sw/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ExecStart=/usr/local/bin/chuggernaut worker
User=root
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target"
GOT_UNIT="$(unit_file)"
[ "$GOT_UNIT" = "$EXPECTED_UNIT" ] || fail "the rendered systemd unit is not what was expected.
  expected: $EXPECTED_UNIT
  got:      $GOT_UNIT"
grep_log "chug_put 0644 \"\$STAGE/chug-worker.service\" '/etc/systemd/system/chug-worker.service'"
# 0644, not 0640: the environment file carries paths, URLs and settings and no
# secret — it NAMES the credential, it does not hold it — and the drift guard
# reads it back as the LOGIN USER on every later deploy. Root-only here is a
# guard that silently passes (case 7f).
grep_log "chug_put 0644 \"\$STAGE/worker.env\" '/etc/chuggernaut/worker.env'"
grep_log "sudo -n systemctl daemon-reload"
grep_log "sudo -n systemctl enable chug-worker.service"
# The container daemon it replaces is removed in the same breath: two daemons
# announcing one node name is two rows in fleet.status and nothing summing their
# slots (design #440 §1).
grep_log "docker rm -f chug-worker"
echo "ok: a systemd unit + environment file are rendered and installed, and the container is removed"

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

grep_log "WORKER_REFRESH_DISK_FREE_GB_MIN='45'"
grep_log "WORKER_REFRESH_DISK_PATH='/var/lib/docker'"
echo "ok: the env file carries the disk pre-flight knobs when set"

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

grep_log "WORKER_SLOTS='2'"
grep_log "WORKER_NODE='air'"
echo "ok: the env file carries WORKER_SLOTS when set"

# ── Case 2c1: the runtimes the node offers reach the daemon (WORKER_MODES) ─────
# #434 gave the daemon the knob and nothing in the deploy path could set it, so
# host mode could not be turned on at all — a node property whose only declared
# home is this file has to arrive at the daemon to exist. A bare value applies to
# a node with no override of its own, exactly like WORKER_SLOTS. Quoted, because
# the daemon trims each entry (crates/worker/src/config.rs `parse_modes`) and so
# `container, host` is a list an operator will write — unquoted it word-splits
# in the macOS agent's `. <file>` and is a second variable to systemd.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_MODES="container, host" \
  WORKER_SLOTS=1 \
  sh "$SUT"

case "$(env_file)" in
  *"WORKER_MODES='container, host'"*) ;;
  *) fail "WORKER_MODES must reach the environment file as ONE quoted value" ;;
esac
echo "ok: the env file carries WORKER_MODES when set, as one quoted value"

# ── Case 2c2: WORKER_MODES is a per-node property like every other one ─────────
# The whole point of declaring modes per node: a fleet enables host execution on
# the node that can serve it, not on all of them. It rides the same derived
# <VAR>_<node> resolution as WORKER_SLOTS and WORKER_CACHE_DIR — no second path.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_MODES=container \
  WORKER_MODES_air=container,host \
  WORKER_SLOTS_air=1 \
  sh "$SUT"

grep_log "WORKER_MODES='container,host'"
if grep -qF "WORKER_MODES='container'" "$LOG"; then
  fail "a per-node WORKER_MODES_air must win over the bare WORKER_MODES"
fi
echo "ok: WORKER_MODES_<node> wins over the bare WORKER_MODES"

# ── Case 2c2b: host without the capacity it needs REFUSES, before the restart ──
# `host` is additive since #309 P1 (job #479) — a node naming both routes each
# launch by the mode it declares — but it still costs capacity: the daemon
# refuses to start unless WORKER_SLOTS and WORKER_SLOTS_MAX are both 1, node-wide
# (two host tasks cannot both own /workspace, #309 §2 option (iii)). Prod's nodes
# run at 2, so a `host` line
# added to chuggernaut.env without the capacity beside it would replace a working
# daemon with one the supervisor boot-loops out of the fleet.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_MODES=container,host \
  WORKER_SLOTS=2 \
  sh "$SUT" >"$WORK/modes-slots.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "host mode without WORKER_SLOTS=1 must fail the deploy (got rc=0)"
grep -qF "WORKER_SLOTS_air=1" "$WORK/modes-slots.out" || fail "the refusal must name the fix, per node"
# The half this script cannot forward has to be said out loud too, or the next
# attempt destroys the daemon and the replacement still refuses to boot.
grep -qF "WORKER_SLOTS_MAX=1" "$WORK/modes-slots.out" \
  || fail "the refusal must name the WORKER_SLOTS_MAX half no script forwards"
grep -qF "/etc/chuggernaut/worker.env" "$WORK/modes-slots.out" \
  || fail "the refusal must name the file that half has to be added to by hand"
not_started "host mode without the capacity it needs must not reach the daemon restart"
echo "ok: host mode without WORKER_SLOTS=1 refuses before the daemon is replaced"

# ── Case 2c3: a mode the daemon cannot parse is refused BEFORE the restart ────
# An unknown name is a hard config error (crates/worker/src/config.rs), so
# passing it through would replace a working daemon with one that cannot boot,
# looped by the supervisor until the node leaves the fleet — the WORKER_KVM
# hazard, one knob over.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_MODES="container,hostt" \
  sh "$SUT" >"$WORK/modes.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unparseable WORKER_MODES must fail the deploy (got rc=0)"
grep -qF "expected container | host" "$WORK/modes.out" || fail "the refusal must name what is wrong"
not_started "an unparseable WORKER_MODES must not reach the daemon restart (live daemon untouched)"
echo "ok: an unparseable WORKER_MODES refuses before the daemon is replaced"

# ── Case 2c4: the guard refuses everything `parse_modes` refuses ──────────────
# A guard that is only ALMOST the daemon's parser is worse than none: it passes
# the recreate, and the operator learns the value was bad from a node that has
# left the fleet. `container,` splits to a trailing empty entry no name parses,
# and a repeat is its own hard config error — both daemon-fatal, so both must die
# here, with the live daemon untouched.
for bad_modes in "container," "container,container"; do
  : > "$LOG"
  set +e
  PATH="$BIN:$PATH" \
    WORKER_SSH=worksalot@nuc \
    WORKER_NATS_URL=nats://10.0.0.1:4222 \
    CHUG_WORKER_NODE=nuc \
    WORKER_MODES="$bad_modes" \
    sh "$SUT" >"$WORK/modes-bad.out" 2>&1
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "WORKER_MODES='$bad_modes' is a daemon config error and must fail the deploy (got rc=0)"
  not_started "WORKER_MODES='$bad_modes' must not reach the daemon restart (live daemon untouched)"
done
echo "ok: an empty entry and a repeated mode both refuse before the daemon is replaced"

# ── Case 2d: KVM unset ⇒ nothing KVM reaches the node ─────────────────────────
# The three #367 settings must be INERT until an operator turns them on: this is
# the whole fleet's run spec, and every node in it has KVM off. Case 2a's golden
# proves the positive half — the env file is exactly six lines with none of them
# here.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"

if grep -qE "WORKER_KVM|WORKER_ANDROID_SDK_DIR|WORKER_FLUTTER_DIR|WORKER_JDK_DIR" "$LOG"; then
  fail "nothing KVM may reach a node with the #367 vars unset"
fi
echo "ok: KVM unset leaves the run spec free of every #367 setting"

# ── Case 2e: KVM on ⇒ the three settings reach the daemon, and NO device flag ──
# The `--device` flag is gone with the container (design #440 §5): a native
# daemon's "does this node have the device" check reads its own view, which IS
# the node's, so there is no flag that could disagree with it.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS="acme/beacon,acme/api" \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  sh "$SUT"

grep_log "WORKER_KVM='1'"
grep_log "WORKER_KVM_PROJECTS='acme/beacon,acme/api'"
grep_log "WORKER_ANDROID_SDK_DIR='/etc/chug/android-sdk'"
if grep -qF -- "--device" "$LOG"; then
  fail "a native daemon takes no --device flag (design #440 §5)"
fi
echo "ok: KVM on passes the three settings and attaches no device"

# ── Case 2e1: Flutter and the JDK are FURTHER, independent leaves (#393, #397) ─
# Set ⇒ each rides beside WORKER_ANDROID_SDK_DIR, whose value is UNTOUCHED (the
# whole reason these are further settings and not a repointing of the first —
# gumbo-nuc-0 needs no migration). Unset ⇒ not a token in the composed spec, so
# an Android-only node's spec is what it was — case 2d proves that half.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS="acme/beacon" \
  WORKER_ANDROID_SDK_DIR=/var/lib/chuggernaut/toolchain/android-sdk \
  WORKER_FLUTTER_DIR=/var/lib/chuggernaut/toolchain/flutter \
  WORKER_JDK_DIR=/var/lib/chuggernaut/toolchain/jdk \
  sh "$SUT"

grep_log "WORKER_ANDROID_SDK_DIR='/var/lib/chuggernaut/toolchain/android-sdk'"
grep_log "WORKER_FLUTTER_DIR='/var/lib/chuggernaut/toolchain/flutter'"
grep_log "WORKER_JDK_DIR='/var/lib/chuggernaut/toolchain/jdk'"
echo "ok: WORKER_FLUTTER_DIR and WORKER_JDK_DIR ride beside the unchanged Android SDK"

# ── Case 2e2: an allow-list written with spaces stays ONE value ────────────────
# The daemon trims each entry (crates/worker/src/config.rs `parse_kvm_projects`),
# so `acme/beacon, acme/api` is a valid list an operator will write. The
# environment file's quoting is what keeps it one value on both readers.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS="acme/beacon, acme/api" \
  sh "$SUT"

case "$(env_file)" in
  *"WORKER_KVM_PROJECTS='acme/beacon, acme/api'"*) ;;
  *) fail "a spaced allow-list must reach the environment file as one quoted value" ;;
esac
echo "ok: a spaced allow-list stays one quoted value"

# ── Case 2f: WORKER_KVM may name another device node (#374's parse) ───────────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=/dev/kvm1 \
  sh "$SUT"

grep_log "WORKER_KVM='/dev/kvm1'"
echo "ok: an absolute WORKER_KVM rides as the value the daemon parses"

# ── Case 2g: WORKER_KVM=0 is OFF ⇒ the setting rides and nothing else does ─────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KVM=0 \
  sh "$SUT"

grep_log "WORKER_KVM='0'"
if grep -qF -- "--device" "$LOG"; then
  fail "WORKER_KVM=0 is off — nothing may be attached"
fi
echo "ok: WORKER_KVM=0 passes the setting and nothing else"

# ── Case 2h: an unparseable WORKER_KVM is refused BEFORE the daemon restart ───
# The daemon rejects anything that is neither a boolean nor an absolute path
# (crates/worker/src/config.rs) — so passing it through would replace a working
# daemon with one that cannot boot. Refuse instead; the live daemon is left
# running.
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
not_started "an unparseable WORKER_KVM must not reach the daemon restart (live daemon untouched)"
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

grep_log "WORKER_KVM='1'"

# Whitespace-only is what the daemon reads as unset, so it must produce the
# untouched spec of case 2a rather than an unparseable-value refusal.
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
# A native daemon creates the cache dir at boot in the node's own view, so a path
# it cannot create is a config refusal the supervisor then loops; a containerized
# one comes up healthy and fails EVERY launch ("bind source path does not
# exist"). Both read as a broken node rather than as a misconfigured deploy, so
# refuse while the working daemon is still running, naming both attempts.
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
not_started "an unprovisionable WORKER_CACHE_DIR must not reach the daemon restart (live daemon untouched)"
echo "ok: an unprovisionable cache dir refuses before the daemon is replaced"

# ── Case 2k: nix roots on ⇒ the settings and the roots dir, and NO mounts ──────
# design #373 P1. The four bind mounts are gone with the container (design #440
# §5): a native daemon has the node's /nix, profiles and daemon socket by
# construction, at the paths the nix daemon itself resolves them by. The
# PRECONDITION survives, and is now checked against exactly the view the daemon
# will have.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_NIX_REALISE_TIMEOUT_SECS=40 \
  sh "$SUT"

grep_log "WORKER_NIX_GCROOTS_DIR='/var/lib/chuggernaut/gcroots'"
grep_log "WORKER_NIX_REALISE_TIMEOUT_SECS='40'"
grep_log "[ -d '/nix/store' ] && [ -d '/nix/var/nix/profiles' ] && [ -S '/nix/var/nix/daemon-socket/socket' ]"
if grep -qF -- "-v '/nix/" "$LOG"; then
  fail "a native daemon takes no nix bind mounts (design #440 §5)"
fi
# The roots dir is provisioned HERE, before the daemon starts (#380's lesson,
# and #372 §5 A5 declines to own this).
grep_log "mkdir -p '/var/lib/chuggernaut/gcroots'"
grep_log "sudo -n mkdir -p '/var/lib/chuggernaut/gcroots'"
roots_line="$(line_of "mkdir -p '/var/lib/chuggernaut/gcroots'")"
run_line="$(line_of "$STARTED_LINUX")"
[ "$roots_line" -lt "$run_line" ] || fail "the gcroots dir must be created BEFORE the daemon starts"
echo "ok: nix roots on passes the settings, provisions the roots dir first, and mounts nothing"

# ── Case 2l: nix roots unset ⇒ nothing nix reaches the node ────────────────────
# The whole fleet is here until an operator turns roots on: no env, no probe, no
# directory.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"

if grep -qE "WORKER_NIX|/nix/store|/nix/var" "$LOG"; then
  fail "nothing nix may reach a node with WORKER_NIX_GCROOTS_DIR unset"
fi
echo "ok: WORKER_NIX_GCROOTS_DIR unset leaves the run spec nix-free"

# ── Case 2m: an unprovisionable roots dir refuses BEFORE the daemon restart ────
# Same shape as the cache dir (#380): a daemon started without its roots dir
# refuses to boot, is looped by the supervisor, and the node leaves the fleet.
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
not_started "an unprovisionable gcroots dir must not reach the daemon restart"
echo "ok: an unprovisionable gcroots dir refuses before the daemon is replaced"

# ── Case 2n: a node with no nix daemon refuses too, and a relative path does ───
# The realise is only sound on a node that actually has a store, a profiles tree
# and a daemon socket; a node without them would take the settings and then fail
# the daemon's own boot check.
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
not_started "a relative gcroots dir must not reach the daemon restart"
echo "ok: a nix-less node and a relative roots dir both refuse before the daemon is replaced"

# ── Case 2o: roots + KVM on ⇒ the toolchain must RESOLVE into the store ────────
# The mount half of this guard is gone; the daemon's half is not. `store_target`
# (crates/worker/src/nix.rs) canonicalizes the realise target at boot and refuses
# anything landing outside the store, so a plain directory still refuses the
# daemon's start and the supervisor still loops it. Native means MORE paths
# qualify — any number of symlink hops, /etc/static included — so the probe asks
# exactly what the daemon asks, and nothing about a parent directory.
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

grep_log "readlink -f '/etc/chug/android-sdk'"
grep_log "in '/nix/store'/*)"
if grep -qF -- "-v '/etc/chug'" "$LOG"; then
  fail "a native daemon mounts no toolchain parent (design #440 §5)"
fi

# A node with roots on but KVM off realises nothing, so it takes no toolchain
# probe — WORKER_KVM=0 is a deliberate configuration, not a half-enabled one.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  WORKER_KVM=0 \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  sh "$SUT"
if grep -qF -- "readlink -f" "$LOG"; then
  fail "a node that realises nothing must not probe the toolchain path"
fi

# And an SDK path that cannot resolve into the store refuses the deploy rather
# than booting a daemon whose own check will refuse it forever.
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
grep -qF "'/etc/chug/android-sdk' on worksalot@nuc does not resolve to a path under '/nix/store'" \
  "$WORK/nixsdk.out" || fail "the refusal must name the path and what it must resolve into"
grep -qF "systemd.tmpfiles.rules" "$WORK/nixsdk.out" || fail "the refusal must name the remedy"
not_started "an unrealisable toolchain path must not reach the daemon restart"
echo "ok: roots + KVM demand a toolchain that resolves into the store, and refuse one that does not"

# ── Case 2p: a realise bound the launch RPC cannot contain is REFUSED ──────────
# The realise runs inside the `launch` RPC the dispatcher abandons after 60s, so
# the daemon refuses a longer bound at parse time (config.rs
# NIX_REALISE_TIMEOUT_SECS_MAX). Catching it here keeps the failure a failed
# deploy instead of a node the supervisor loops out of the fleet.
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
  not_started "an unusable realise bound ($bad) must not reach the daemon restart"
done
echo "ok: a realise bound outside the launch RPC's budget refuses before the daemon is replaced"

# ── Case 2q: the node's NATS credential is read off the NODE now ───────────────
# The `:ro` bind of $HOME/chuggernaut-worker/keys is gone with the container, so
# a missing credential is a daemon that cannot reach NATS and is restarted into
# the same failure. Refuse while the live daemon is still running — and BEFORE
# the ten-minute image build, since none of what this asks needs an image.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAIL_CREDS=1 \
  sh "$SUT" >"$WORK/creds.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a missing NATS credential must fail the deploy (got rc=0)"
grep -qF "'/etc/chuggernaut/keys/worker.creds' is not on worksalot@nuc" "$WORK/creds.out" \
  || fail "the refusal must name the path the daemon will read"
grep -qF "sudo install -o root -g root -m 0600 <staged> /etc/chuggernaut/keys/worker.creds" \
  "$WORK/creds.out" || fail "the refusal must name how to install it"
not_started "a missing NATS credential must not reach the daemon restart"
if grep -qF "docker build" "$LOG"; then
  fail "a missing credential must refuse before the images are built"
fi

# And a WORKER_GIT_KEY still naming the container's mount point is refused: it
# only ever existed inside chug-worker, so a native daemon would come up and
# every self-refresh would fail to fetch.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_GIT_KEY=/data/keys/worker_git \
  sh "$SUT" >"$WORK/gitkey.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a container-path WORKER_GIT_KEY must fail the deploy (got rc=0)"
grep -qF "names the container's key mount" "$WORK/gitkey.out" \
  || fail "the refusal must say why the path cannot work"
grep -qF "WORKER_GIT_KEY_nuc=/etc/chuggernaut/keys/worker_git" "$WORK/gitkey.out" \
  || fail "the refusal must name the host path to declare instead"
not_started "a container-path WORKER_GIT_KEY must not reach the daemon restart"

# And on Linux a WORKER_GIT_KEY still naming the LOGIN USER's home is refused for
# the same reason one directory over: §6's migration DELETES that copy, so the
# daemon would be handed a spec naming a key that is gone — and while it is still
# there, the `docker` group can read it, which is the boundary this slice raises.
# macOS is exempt by construction (case 2s): the agent runs as the login user, so
# its keys live under that home on purpose.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_GIT_KEY=/home/worksalot/chuggernaut-worker/keys/worker_git \
  sh "$SUT" >"$WORK/gitkeyhome.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a home-path WORKER_GIT_KEY must fail the deploy on Linux (got rc=0)"
grep -qF "is under the login user's home ('/home/worksalot')" "$WORK/gitkeyhome.out" \
  || fail "the refusal must name the home it found the key under"
grep -qF "sudo install -o root -g root -m 0600 /home/worksalot/chuggernaut-worker/keys/worker_git /etc/chuggernaut/keys/worker_git" \
  "$WORK/gitkeyhome.out" || fail "the refusal must name the command that moves the key"
grep -qF "drop WORKER_GIT_KEY_nuc" "$WORK/gitkeyhome.out" \
  || fail "the refusal must name the per-node declaration to drop"
not_started "a home-path WORKER_GIT_KEY must not reach the daemon restart"
if grep -qF "docker build" "$LOG"; then
  fail "a home-path WORKER_GIT_KEY must refuse before the images are built"
fi

# But a key INSIDE the credential directory is served wherever that directory
# sits: the owner-and-mode guard has already vouched for it, so a node whose
# root-owned 0700 WORKER_KEYS_DIR happens to be under the home is not refused for
# a boundary it already satisfies.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KEYS_DIR_nuc=/home/worksalot/chug-keys \
  sh "$SUT" >"$WORK/gitkeyin.out" 2>&1
grep_log "WORKER_GIT_KEY='/home/worksalot/chug-keys/worker_git'"
grep -qF "REFUSING" "$WORK/gitkeyin.out" \
  && fail "a git key inside the vouched-for credential directory must not refuse"
started || fail "a root-owned 0700 credential directory must be served wherever it sits"
echo "ok: the node's credentials are read off the node, and the container and home paths are refused"

# ── Case 2q1: the credential directory is ROOT-OWNED and 0700 (design #440 D5) ─
# The default moved out of the login user's home, and that IS the slice: the
# login user is in the `docker` group and is who this script ssh's in as, so a
# creds file under that home is readable by anything that user runs — a weaker
# boundary than the read-only bind the native daemon replaces. The credential
# path and the git key both follow it.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT" >"$WORK/keysok.out" 2>&1
grep_log "NATS_CREDS='/etc/chuggernaut/keys/worker.creds'"
grep_log "WORKER_GIT_KEY='/etc/chuggernaut/keys/worker_git'"
if grep -qF "/home/worksalot/chuggernaut-worker/keys" "$LOG"; then
  fail "a Linux node's credentials must not live under the login user's home any more"
fi
grep -qF "credential directory /etc/chuggernaut/keys on worksalot@nuc is root-owned at 0700" \
  "$WORK/keysok.out" || fail "a correct directory must be confirmed out loud"
started || fail "a root-owned 0700 directory holding the creds must proceed"
echo "ok: the credential directory defaults to a root-owned 0700 path outside any home"

# ── Case 2q2: a WRONG OWNER or WRONG MODE refuses, naming both and the remedy ──
# The failure this prevents is a daemon that cannot read its own credential:
# it does not come up degraded, it fails to START, and Restart=always loops that
# on a node the operator has just converted. So the refusal names the directory,
# the expected owner, the expected mode, what was actually found, and the exact
# command that fixes it.
for _case in "worksalot 700 owned by 'worksalot' at mode '700'" \
             "root 755 owned by 'root' at mode '755'" \
             "root 750 owned by 'root' at mode '750'"; do
  set -- $_case
  _owner=$1
  _mode=$2
  shift 2
  : > "$LOG"
  set +e
  PATH="$BIN:$PATH" \
    WORKER_SSH=worksalot@nuc \
    WORKER_NATS_URL=nats://10.0.0.1:4222 \
    CHUG_WORKER_NODE=nuc \
    FAKE_KEYS_OWNER="$_owner" \
    FAKE_KEYS_MODE="$_mode" \
    sh "$SUT" >"$WORK/keysbad.out" 2>&1
  rc=$?
  set -e
  [ "$rc" -ne 0 ] || fail "a $_owner:$_mode credential directory must fail the deploy (got rc=0)"
  grep -qF "'/etc/chuggernaut/keys' on worksalot@nuc is $*" "$WORK/keysbad.out" \
    || fail "the refusal must name the directory and what it FOUND ($_owner $_mode)"
  grep -qF "must be owned by 'root' at mode '700'" "$WORK/keysbad.out" \
    || fail "the refusal must name the expected owner and mode"
  grep -qF "sudo chown root:root /etc/chuggernaut/keys && sudo chmod 0700 /etc/chuggernaut/keys" \
    "$WORK/keysbad.out" || fail "the refusal must name the remedy"
  grep -qF "WORKER_KEYS_DIR_nuc" "$WORK/keysbad.out" \
    || fail "the refusal must name the knob, per node"
  not_started "a wrong-owner/mode credential directory must not reach the daemon restart"
  if grep -qF "docker build" "$LOG"; then
    fail "the credential-directory refusal must come before the images are built"
  fi
done
echo "ok: a wrong-owner or wrong-mode credential directory refuses, naming owner, mode and remedy"

# ── Case 2q3: the directory is not there at all ⇒ a DIFFERENT, named refusal ───
# Distinct from a wrong mode: nothing to chown, so the remedy is the create.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_KEYS_ABSENT=1 \
  sh "$SUT" >"$WORK/keysgone.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an absent credential directory must fail the deploy (got rc=0)"
grep -qF "'/etc/chuggernaut/keys' does not exist on worksalot@nuc" "$WORK/keysgone.out" \
  || fail "the refusal must name the directory it could not find"
grep -qF "sudo install -d -o root -g root -m 0700 /etc/chuggernaut/keys" "$WORK/keysgone.out" \
  || fail "the refusal must name the command that creates it correctly"
not_started "an absent credential directory must not reach the daemon restart"
echo "ok: an absent credential directory refuses with the create command, not the chown one"

# ── Case 2q4: "I cannot look" is not "it is not there" ─────────────────────────
# Inside a root-owned 0700 directory the login user cannot `test -r` the file at
# all, so a missing credential and an unprivileged check produce the SAME failed
# test. Collapsing them tells an operator to re-mint a credential that is already
# installed, which is exactly the cryptic failure this slice must not add.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_KEYS_NOSUDO=1 \
  sh "$SUT" >"$WORK/keysnosudo.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unverifiable credential must fail the deploy (got rc=0)"
grep -qF "cannot tell whether '/etc/chuggernaut/keys/worker.creds' exists" "$WORK/keysnosudo.out" \
  || fail "the refusal must say it could not SEE the credential"
grep -qF "passwordless sudo" "$WORK/keysnosudo.out" || fail "the refusal must name the remedy"
grep -qF "is not on worksalot@nuc" "$WORK/keysnosudo.out" \
  && fail "an unverifiable credential must NOT be reported as a missing one"
not_started "an unverifiable credential must not reach the daemon restart"
echo "ok: an unreadable-by-the-check credential refuses distinctly from a missing one"

# ── Case 2q5: WORKER_KEYS_DIR_<node> moves it, and the guard follows ───────────
# The knob resolves per node like every other WORKER_*, and the directory it
# names is the one checked — so a node that keeps its keys elsewhere is served,
# and is served under the same rule.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KEYS_DIR_nuc=/var/lib/chuggernaut/keys \
  sh "$SUT"
grep_log "NATS_CREDS='/var/lib/chuggernaut/keys/worker.creds'"
grep_log "WORKER_GIT_KEY='/var/lib/chuggernaut/keys/worker_git'"
grep_log "stat -c %U '/var/lib/chuggernaut/keys'"
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_KEYS_DIR_nuc=/var/lib/chuggernaut/keys \
  FAKE_KEYS_OWNER=worksalot \
  sh "$SUT" >"$WORK/keysmoved.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "the guard must follow WORKER_KEYS_DIR (got rc=0)"
grep -qF "'/var/lib/chuggernaut/keys' on worksalot@nuc is owned by 'worksalot'" "$WORK/keysmoved.out" \
  || fail "the refusal must name the declared directory, not the default"
not_started "a wrong-owner declared keys directory must not reach the daemon restart"
echo "ok: WORKER_KEYS_DIR_<node> moves the credential directory and the guard with it"

# ── Case 2q6: a node still running the CONTAINER daemon is not stranded ────────
# The fleet is mixed while the conversion happens, and this run is the one that
# converts. The live container's own environment names /data/keys — a path that
# only ever existed inside it — and reading that back must neither refuse nor
# drop anything: the drift guard reports the two paths that moved and proceeds,
# and the container is removed in the same install. This run is also what
# RESTORES the node's self-refresh: since design #440 slice 6 the swap installs
# a binary and restarts a unit, so an unconverted node's own swap refuses
# (deploy/prod/worker-refresh.test.sh) and conversion is what ends that.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  FAKE_LIVE_ENV="WORKER_NODE=nuc
NATS_CREDS=/data/keys/worker.creds
WORKER_GIT_KEY=/data/keys/worker_git
WORKER_REFRESH_GIT_URL=ssh://git@front:2222/acme/chug.git" \
  sh "$SUT" >"$WORK/convert.out" 2>&1
grep -qF "REFUSING" "$WORK/convert.out" \
  && fail "converting a container node must not refuse on the container's own /data/keys"
grep -qF "run-spec drift checked against the live chug-worker CONTAINER" "$WORK/convert.out" \
  || fail "the guard must say it read the container side"
grep -qF "WORKER_GIT_KEY: live '/data/keys/worker_git' -> declared '/etc/chuggernaut/keys/worker_git'" \
  "$WORK/convert.out" || fail "the key path that MOVED must be reported before it is applied"
grep_log "docker rm -f chug-worker"
started || fail "an unconverted container node must still be convertible"
echo "ok: a node still running the container daemon converts, and its /data/keys values do not refuse"

# ── Case 2r: a node with no writable unit directory refuses ────────────────────
# On NixOS /etc/systemd/system is a read-only symlink into the store, which is
# precisely the state design #440 slice 7 exists for. Refuse with the live daemon
# still running rather than half-way through the install.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAIL_UNIT_DIR=1 \
  sh "$SUT" >"$WORK/unitdir.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unwritable unit directory must fail the deploy (got rc=0)"
grep -qF "no usable systemd unit directory at '/etc/systemd/system'" "$WORK/unitdir.out" \
  || fail "the refusal must name the directory"
grep -qF "WORKER_UNIT_DIR_nuc" "$WORK/unitdir.out" || fail "the refusal must name the knob, per node"
not_started "an unwritable unit directory must not reach the daemon restart"

# And the knob is honoured, per node, like every other WORKER_* declaration.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_UNIT_DIR_nuc=/run/systemd/system \
  sh "$SUT"
grep_log "chug_put 0644 \"\$STAGE/chug-worker.service\" '/run/systemd/system/chug-worker.service'"
echo "ok: an unwritable unit directory refuses, and WORKER_UNIT_DIR_<node> is honoured"

# ── Case 2s: macOS gets a launchd agent, not a unit (design #440 D2) ───────────
# The shape deploy/prod/install-launchd.sh already installs for the dispatcher
# and api — a GUI-domain agent, because CoreSimulator and the keychain are
# per-user-session services (#322). launchd has no EnvironmentFile, so the agent
# SOURCES the same environment file the systemd unit reads: one declaration, one
# thing for the drift guard to compare.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_SLOTS=2 \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  sh "$SUT" >"$WORK/mac.out" 2>&1

GOT_PLIST="$(plist_file)"
[ -n "$GOT_PLIST" ] || fail "a macOS node must be given a plist"
if [ -n "$(unit_file)" ]; then
  fail "a macOS node must NOT be given a systemd unit"
fi
case "$GOT_PLIST" in
  *"<key>Label</key><string>com.chuggernaut.worker</string>"*) ;;
  *) fail "the agent must carry its label" ;;
esac
case "$GOT_PLIST" in
  *"set -a; . '/Users/op/chuggernaut-worker/worker.env'; set +a; exec '/usr/local/bin/chuggernaut' worker"*) ;;
  *) fail "the agent must source the same environment file the unit reads" ;;
esac
case "$GOT_PLIST" in
  *"<key>KeepAlive</key><true/>"*) ;;
  *) fail "the agent must be kept alive, as --restart=always was" ;;
esac
grep_log "launchctl bootstrap gui/\$(id -u) '/Users/op/Library/LaunchAgents/com.chuggernaut.worker.plist'"
grep_log "launchctl bootout gui/\$(id -u)/com.chuggernaut.worker"
grep_log "plutil -lint"
# The keys and the run spec follow the node's own $HOME, and nothing systemd is
# asked of a mac. Design #440 D5's root-owned 0700 directory deliberately does
# NOT port here: the agent runs as the login user in their GUI domain (#322), so
# there is no user a root-owned directory would exclude, and the boundary would
# be theatre. The script says so rather than pretending, and #322 §7 already
# lists cross-task secret isolation on macOS under what it gives up.
grep_log "NATS_CREDS='/Users/op/chuggernaut-worker/keys/worker.creds'"
grep_log "WORKER_GIT_KEY='/Users/op/chuggernaut-worker/keys/worker_git'"
grep_log "WORKER_SLOTS='2'"
grep -qF "design #440 D5's root-owned 0700 boundary does not port to macOS" "$WORK/mac.out" \
  || fail "a macOS node must be told the boundary does not port, not left to assume it does"
if grep -qF "stat -c" "$LOG"; then
  fail "a macOS node must not be asked GNU stat's owner/mode probe"
fi
if grep -qF "systemctl" "$LOG"; then
  fail "a macOS node must never be asked for systemctl"
fi
echo "ok: a macOS node gets a launchd agent sourcing the same environment file"

# ── Case 2s1: a mac that cannot be written at /usr/local refuses, before the ──
# install. Only the agent and the environment file are in the login user's tree;
# the binaries go to /usr/local on BOTH platforms, and on a stock Apple-Silicon
# mac (which the plist's own /opt/homebrew PATH assumes) that is root-owned and
# often absent. deploy/prod/install-launchd.sh — the precedent design #440 D2
# names — writes only under $HOME, so this is the first thing on this platform to
# need it. Without the check the operator gets a bare `sudo: a password is
# required` from inside the install; with it, the same named remedy the Linux
# unit-directory refusal gives.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAIL_MAC_DIRS=1 \
  sh "$SUT" >"$WORK/macdirs.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unwritable /usr/local must fail the deploy on macOS (got rc=0)"
grep -qF "cannot install the daemon binaries into '/usr/local/bin' and '/usr/local/lib/chuggernaut'" \
  "$WORK/macdirs.out" || fail "the refusal must name both directories"
grep -qF "sudo mkdir -p" "$WORK/macdirs.out" || fail "the refusal must name the remedy"
not_started "an unwritable /usr/local must not reach the agent bootstrap"
if grep -qF "docker create chuggernaut/worker" "$LOG"; then
  fail "the refusal must come before the binaries are extracted"
fi
# And a mac that CAN be written is never asked for systemd's check, nor Linux for
# the mac one: each platform is asked only what it can answer.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"
if grep -qF "for d in '/usr/local/bin'" "$LOG"; then
  fail "a Linux node must not be asked the macOS binary-directory check"
fi
echo "ok: an unwritable /usr/local refuses on macOS, before anything is extracted"

# ── Case 2s2: a Darwin node COMPILES its daemon; it cannot extract one ─────────
# design #440 D6 extracts the binary from the worker image, and that image is a
# LINUX container: on the air (2026-08-06) it installed as `ELF 64-bit LSB pie
# executable, ARM aarch64` and launchd looped `cannot execute binary file`. So a
# mac builds its DAEMON from the same context, with the node's own cargo, and
# that one artifact comes out of that tree instead of out of a `docker cp`.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  sh "$SUT" > "$WORK/macbuild.out" 2>&1

grep_log "CHUG_GIT_SHA='deadbeefcafe' CARGO_TARGET_DIR='/Users/op/chuggernaut-worker/build/target' '/opt/homebrew/bin/cargo' build --release --locked --bin chuggernaut --bin chuggernaut-channel"
grep_log "install -m 0755 '/Users/op/chuggernaut-worker/build/target/release/chuggernaut' \"\$STAGE/chuggernaut\""
grep_log "install -m 0755 '/Users/op/chuggernaut-worker/build/src/deploy/prod/worker-refresh.sh' \"\$STAGE/worker-refresh.sh\""
# The DAEMON must not come out of that image — an ELF under launchd is exactly
# what crash-looped. The channel binary is the opposite case and has its own
# case below; what is asserted here is that the daemon is not one of the files
# lifted out of the container.
if grep -qF -- '"$CID:/usr/local/bin/chuggernaut"' "$LOG"; then
  fail "a Darwin node must not extract its daemon binary from the Linux worker image"
fi
# The target directory is kept and the source tree is replaced: a cold workspace
# compile on a mac is tens of minutes, and the self-refresh builds in the same
# place — which is why both coordinates ride in the run spec.
grep_log "rm -rf '/Users/op/chuggernaut-worker/build/src'"
if grep -qF -- "rm -rf '/Users/op/chuggernaut-worker/build/target'" "$LOG"; then
  fail "the node's cargo target directory must survive between deploys"
fi
grep_log "WORKER_CARGO='/opt/homebrew/bin/cargo'"
grep_log "WORKER_BUILD_DIR='/Users/op/chuggernaut-worker/build'"
echo "ok: a Darwin node compiles its own daemon and installs that, not the image's Linux binary"

# ── Case 2s2b: …and its CHANNEL binary comes out of the image, not the tree ────
# The two binaries have OPPOSITE platform requirements and #440's 2026-08-07
# correction generalised over both. The daemon runs ON the mac; the channel
# binary never does — the daemon injects it into every agent container
# (crates/worker/src/daemon.rs, `FileSource::LocalArtifact`), which is Linux. The
# mac's own build is a Mach-O, so Claude Code's chuggernaut-channel MCP server
# stayed `pending` for every task on the air and the evaluators had no
# `submit_eval` at all (#477, #478). The image is built by the node's own docker,
# so its copy is already linux/<the node's container arch>.
grep_log "docker create chuggernaut/worker:prod"
grep_log '"$CID:/usr/local/lib/chuggernaut/chuggernaut-channel"'
if grep -qF -- "install -m 0755 '/Users/op/chuggernaut-worker/build/target/release/chuggernaut-channel'" "$LOG"; then
  fail "a Darwin node must not inject the Mach-O channel binary its own cargo built"
fi
# The refresh script is /bin/sh and has no platform at all, so it stays on the
# source-file path — the same bytes the image would have handed back.
if grep -qF -- '"$CID:/usr/local/lib/chuggernaut/worker-refresh.sh"' "$LOG"; then
  fail "the refresh script is platform-agnostic and stays on the source-file path"
fi
# Extracted BEFORE the install, like everything else the node is handed.
chan_cp_line="$(line_of '"$CID:/usr/local/lib/chuggernaut/chuggernaut-channel"')"
chan_put_line="$(line_of "chug_put 0755 \"\$STAGE/chuggernaut-channel\" '/usr/local/lib/chuggernaut/chuggernaut-channel'")"
[ -n "$chan_cp_line" ] && [ -n "$chan_put_line" ] || fail "expected both the channel extraction and its install in the log"
[ "$chan_cp_line" -lt "$chan_put_line" ] || fail "the channel binary must be extracted before it is installed"
echo "ok: a Darwin node injects the image's LINUX channel binary, not its own Mach-O one"

# ── Case 2s2a: the conversion NOTE says where THIS node's binary comes from ────
# It is the only place the operator is told what their node's self-refresh does,
# and it is printed at the moment the node is converted. Saying "out of the
# worker image" to a mac's operator is the belief this correction exists to fix.
grep -qF "compiles its own daemon binary with /opt/homebrew/bin/cargo in /Users/op/chuggernaut-worker/build" "$WORK/macbuild.out" \
  || fail "the NOTE must tell a mac's operator their node COMPILES its next daemon, with which cargo and where"
if grep -qF "self-refresh installs the daemon binary out of the worker image" "$WORK/macbuild.out"; then
  fail "the NOTE must not tell a mac's operator their self-refresh extracts from the Linux worker image"
fi
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT" > "$WORK/linuxnote.out" 2>&1
grep -qF "installs the daemon binary out of the worker image (design #440 D6)" "$WORK/linuxnote.out" \
  || fail "a Linux node's NOTE is unchanged — D6 holds there"
echo "ok: the conversion NOTE names the binary's provenance per platform"

# ── Case 2s3: a mac with no reachable cargo REFUSES, before anything is built ──
# The strategy needs something this node lacks, so it says which thing and how to
# declare it — rather than installing a binary that cannot exec and timing out on
# the health probe sixty seconds later with the container daemon already gone.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAKE_NODE_CARGO= \
  sh "$SUT" > "$WORK/nocargo.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a Darwin node with no cargo must fail the deploy (got rc=0)"
grep -qF "WORKER_CARGO_air=" "$WORK/nocargo.out" || fail "the refusal must name the per-node declaration"
grep -qF "cannot execute binary file" "$WORK/nocargo.out" || fail "the refusal must name what the image's binary does on a mac"
not_started "a mac with no toolchain must not reach the agent bootstrap"
if grep -qF "docker build" "$LOG"; then
  fail "the toolchain refusal must come BEFORE the image builds"
fi
# And a Linux node is never asked for one: its binary comes out of the image.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_NODE_CARGO= \
  sh "$SUT"
started || fail "a Linux node needs no cargo (design #440 D6 holds there)"
if grep -qF "build --release --locked" "$LOG"; then
  fail "a Linux node must not compile the daemon on the node"
fi
echo "ok: a mac with no reachable cargo refuses by name; a Linux node is never asked for one"

# ── Case 2s3a: the toolchain is a DIRECTORY, and both other halves refuse ──────
# `command -v cargo` is not the question the compile asks. Cargo resolves `rustc`
# THROUGH PATH — and an absolute WORKER_CARGO is declared precisely because the
# bare name is not on the ssh shell's PATH — so a node with cargo and no rustc
# passes a `command -v` guard and dies mid-compile, after three image builds.
# A rustup shim with no default toolchain is the same shape one step earlier.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAKE_NODE_RUSTC= \
  sh "$SUT" > "$WORK/norustc.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a Darwin node whose cargo cannot find rustc must fail the deploy (got rc=0)"
grep -qF "cargo resolves its compiler THROUGH PATH" "$WORK/norustc.out" \
  || fail "the refusal must say why an absolute WORKER_CARGO is not enough"
not_started "a mac with no rustc must not reach the agent bootstrap"
if grep -qF "docker build" "$LOG"; then
  fail "the rustc refusal must come BEFORE the image builds, not mid-compile"
fi

: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAKE_NODE_CARGO_RUNS= \
  sh "$SUT" > "$WORK/deadcargo.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a Darwin node whose cargo does not run must fail the deploy (got rc=0)"
grep -qF "rustup shim with no default toolchain" "$WORK/deadcargo.out" \
  || fail "the refusal must name the state it is describing"
not_started "a mac whose cargo does not exec must not reach the agent bootstrap"
echo "ok: a mac with cargo but no rustc, and one whose cargo does not run, each refuse before the builds"

# ── Case 2s3b: the toolchain DIRECTORY is carried, to both compilers ───────────
# The remote compile and the daemon's OWN self-refresh each run with a PATH that
# is not the operator's interactive one: the first is the ssh shell's, the second
# is the launchd agent's. A nix-darwin profile directory is on neither, so
# WORKER_CARGO alone buys nothing once cargo goes looking for rustc.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAKE_NODE_CARGO=/etc/profiles/per-user/op/bin/cargo \
  FAKE_NODE_RUSTC=/etc/profiles/per-user/op/bin/rustc \
  sh "$SUT" > /dev/null 2>&1
grep_log "PATH='/etc/profiles/per-user/op/bin':\"\$PATH\""
GOT_PLIST="$(plist_file)"
case "$GOT_PLIST" in
  *"<key>PATH</key><string>/etc/profiles/per-user/op/bin:/opt/homebrew/bin:"*) ;;
  *) fail "the launchd agent's PATH must carry the toolchain directory — it is the PATH the daemon's own refresh compiles under" ;;
esac
# And the marker both writers of the staging directory maintain: the swap refuses
# when it disagrees with the image's label, which only works if a conversion
# leaves it describing what it just built.
grep_log "'deadbeefcafe' > '/Users/op/chuggernaut-worker/build/native.sha'"
# A directory already on the agent's PATH is not prepended twice.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  sh "$SUT" > /dev/null 2>&1
GOT_PLIST="$(plist_file)"
case "$GOT_PLIST" in
  *"<key>PATH</key><string>/opt/homebrew/bin:/opt/homebrew/sbin:"*) ;;
  *) fail "a toolchain already on the agent's PATH must not be prepended again" ;;
esac
echo "ok: the toolchain directory reaches the remote compile and the agent's PATH, and native.sha is written"

# ── Case 2s4: WORKER_DOCKER_ENDPOINT reaches the run spec (the second gap) ─────
# The setting has existed in crates/worker/src/config.rs all along and NOTHING
# rendered it, because its default was correct while the daemon was a container
# with /var/run/docker.sock bind-mounted in. Natively on a mac colima listens
# somewhere else, and the converted air answered every launch with `backend
# unavailable: Socket not found: /var/run/docker.sock`. Derived from the node's
# own docker context so a mac does not have to notice and declare it.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_SLOTS=2 \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  sh "$SUT" > "$WORK/endpoint.out" 2>&1
grep_log "WORKER_DOCKER_ENDPOINT='unix:///Users/op/.colima/default/docker.sock'"
grep -qF "derived from op@air" "$WORK/endpoint.out" \
  || fail "a derived endpoint must be announced — it is a SNAPSHOT of the node's context"
# The whole macOS run spec as a golden, for case 2a's reason: this is the file
# the agent sources, and the two lines a conversion adds are the ones the air
# needed by hand.
EXPECTED_MAC_ENV="WORKER_NODE='air'
NATS_URL='nats://10.0.0.1:4222'
NATS_CREDS='/Users/op/chuggernaut-worker/keys/worker.creds'
RUST_LOG='info,async_nats=warn'
WORKER_REFRESH_GIT_URL=''
WORKER_GIT_KEY='/Users/op/chuggernaut-worker/keys/worker_git'
WORKER_SLOTS='2'
WORKER_DOCKER_ENDPOINT='unix:///Users/op/.colima/default/docker.sock'
WORKER_CARGO='/opt/homebrew/bin/cargo'
WORKER_BUILD_DIR='/Users/op/chuggernaut-worker/build'"
GOT_MAC_ENV="$(env_file)"
[ "$GOT_MAC_ENV" = "$EXPECTED_MAC_ENV" ] || fail "a converted mac's environment file must be exactly the composed run spec.
  expected: $EXPECTED_MAC_ENV
  got:      $GOT_MAC_ENV"
# A LINUX node is byte-identical to what it was: nothing derives, nothing is
# asked, and no line is added (case 2a's golden is the other half of this).
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"
if grep -qF "docker context inspect" "$LOG"; then
  fail "a Linux node must not be asked for its docker context"
fi
if grep -qF "WORKER_DOCKER_ENDPOINT" "$LOG"; then
  fail "WORKER_DOCKER_ENDPOINT must not be passed when unset (daemon default applies)"
fi
echo "ok: a mac's docker endpoint is derived into the run spec; a Linux node's spec is unchanged"

# ── Case 2s4a: a DECLARED endpoint rides on either platform, per node ──────────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_DOCKER_ENDPOINT_nuc=unix:///run/podman/podman.sock \
  sh "$SUT"
grep_log "WORKER_DOCKER_ENDPOINT='unix:///run/podman/podman.sock'"
echo "ok: a declared endpoint reaches the daemon, and is per-node like every other knob"

# ── Case 2s4b: a DERIVED endpoint equal to the daemon's default is not written ─
# Deriving must not make a node that declared nothing carry a line it never
# chose — a mac whose docker really is at /var/run/docker.sock gets the run spec
# it would have got before any of this existed.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAKE_DOCKER_CONTEXT=unix:///var/run/docker.sock \
  sh "$SUT" > /dev/null 2>&1
if grep -qF "WORKER_DOCKER_ENDPOINT" "$LOG"; then
  fail "a derived endpoint equal to the daemon's own default must stay unset"
fi
echo "ok: a derived endpoint equal to the default is dropped, not written"

# ── Case 2s4c: an endpoint the node cannot answer at REFUSES, before the swap ──
# Two different failures, and neither is a daemon that comes up degraded: an
# unsupported scheme is a start-time refusal the supervisor would loop, and a
# socket that is not there is a node that announces its slots and fails EVERY
# launch — which is what the air did after its binary was fixed.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_DOCKER_ENDPOINT=ssh://worksalot@nuc \
  sh "$SUT" > "$WORK/ep-scheme.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an endpoint scheme the daemon rejects must fail the deploy (got rc=0)"
grep -qF "neither unix:// nor tcp://" "$WORK/ep-scheme.out" || fail "the refusal must name the shapes"
not_started "an unparseable endpoint must not reach the daemon restart"

: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAIL_DOCKER_SOCKET=1 \
  sh "$SUT" > "$WORK/ep-sock.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an endpoint whose socket is absent must fail the deploy (got rc=0)"
grep -qF "backend unavailable: Socket not found" "$WORK/ep-sock.out" \
  || fail "the refusal must name the error the node would otherwise report per launch"
not_started "a missing docker socket must not reach the daemon restart"
echo "ok: an unparseable endpoint and an absent socket both refuse, live daemon untouched"

# ── Case 2s5: the staged binary must RUN on the node before it is installed ────
# The generalisation of the whole finding (#309 P0 finding 6): a binary whose
# provenance is a foreign platform installs perfectly and then loops under the
# supervisor. `--version` is the cheapest question that separates the two, it is
# asked on BOTH platforms — the image is bookworm and a NixOS node has no /lib64
# loader — and it is asked BEFORE the first chug_put, so a refusal leaves the
# node exactly as it was.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT"
grep_log "\"\$STAGE/chuggernaut\" --version"
probe_line="$(line_of "\"\$STAGE/chuggernaut\" --version")"
put_line="$(line_of "chug_put 0755 \"\$STAGE/chuggernaut\" '/usr/local/bin/chuggernaut'")"
[ -n "$probe_line" ] && [ -n "$put_line" ] || fail "expected both the exec check and the install in the log"
[ "$probe_line" -le "$put_line" ] || fail "the staged binary must be run BEFORE it is installed"
echo "ok: the staged daemon binary is required to exec on the node before it is installed"

# ── Case 2s5a: the CHANNEL binary is asked the question its own executor asks ───
# On Linux the node IS the container's platform, so the question is the same one:
# does this exec here. `chuggernaut-channel` has no `--version` (it is an MCP
# server that reads its job context out of the environment), so what is asked is
# whether the kernel would load it at all — 126/127 — and it is asked before the
# first chug_put.
grep_log "\"\$STAGE/chuggernaut-channel\" < /dev/null"
grep_log "CHAN_RC\" = 126"
chan_probe_line="$(line_of "\"\$STAGE/chuggernaut-channel\" < /dev/null")"
chan_put_line="$(line_of "chug_put 0755 \"\$STAGE/chuggernaut-channel\" '/usr/local/lib/chuggernaut/chuggernaut-channel'")"
[ -n "$chan_probe_line" ] && [ -n "$chan_put_line" ] || fail "expected both the channel exec check and its install in the log"
[ "$chan_probe_line" -lt "$chan_put_line" ] || fail "the staged channel binary must be checked BEFORE it is installed"
# And the Linux STAGING is untouched by all of this: all three artifacts still
# come out of the one image, which is the platform-coincidence D6 rests on.
grep_log "docker create chuggernaut/worker:prod"
grep_log '"$CID:/usr/local/bin/chuggernaut"'
grep_log '"$CID:/usr/local/lib/chuggernaut/chuggernaut-channel"'
grep_log '"$CID:/usr/local/lib/chuggernaut/worker-refresh.sh"'
# A Linux node is never asked for its container platform: it is the node's own.
if grep -qF -- "docker version --format" "$LOG"; then
  fail "a Linux node's container platform is its own — no probe belongs on that path"
fi
echo "ok: the Linux path is unchanged and its channel binary must exec on the node"

# ── Case 2s6: on DARWIN the channel binary is judged as a CONTAINER binary ─────
# Asking it to run on the mac would be exactly backwards — the right answer there
# is that it does not. So the guard reads its object header: ELF magic, and the
# e_machine of the architecture THIS NODE'S DOCKER runs, derived rather than
# assumed. The guard's own two functions are lifted out of the remote script and
# run here against fixtures, so what is tested is the logic the node executes.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  sh "$SUT" > "$WORK/macguard.out" 2>&1

# A 20-byte ELF header is all the guard reads: magic, then e_machine as two
# little-endian bytes at offset 18 (aarch64 = 0xb7, x86-64 = 0x3e). The Mach-O
# fixture is what a mac's own `cargo build` leaves at that path.
elf_header() {
  printf '\177ELF\002\001\001\000'
  printf '\000\000\000\000\000\000\000\000'
  printf '\003\000'
  printf '%b' "$1"
}
elf_header '\267\000' > "$WORK/chan-linux-arm64"
elf_header '\076\000' > "$WORK/chan-linux-amd64"
printf '\317\372\355\376\014\000\000\001\002\000\000\000\020\000\000\000\330\005\000\000' \
  > "$WORK/chan-macho-arm64"

guard_src="$({ grep -m 1 '^chug_magic() {' "$LOG"; grep -m 1 '^chug_channel_ok() {' "$LOG"; } || true)"
case "$guard_src" in
  *chug_magic*chug_channel_ok*) ;;
  *) fail "the Darwin install script must carry the channel guard's own functions" ;;
esac
eval "$guard_src"
guard_refuses() {
  if chug_channel_ok "$1" "$2"; then fail "$3"; fi
}
chug_channel_ok "$WORK/chan-linux-arm64" b700 \
  || fail "the guard must ACCEPT a linux/arm64 ELF — that is what the image carries"
guard_refuses "$WORK/chan-macho-arm64" b700 \
  "the guard must REFUSE a Mach-O: no agent container can exec it"
guard_refuses "$WORK/chan-linux-amd64" b700 \
  "the guard must REFUSE an ELF for another architecture"
# Derived from the node's docker, not assumed: a linux/amd64 colima wants the
# other e_machine and the arm64 ELF is then the wrong one.
chug_channel_ok "$WORK/chan-linux-amd64" 3e00 \
  || fail "the guard must ACCEPT a linux/amd64 ELF on a node whose docker runs amd64"
guard_refuses "$WORK/chan-linux-arm64" 3e00 \
  "the guard must REFUSE an arm64 ELF on a node whose docker runs amd64"
echo "ok: the Darwin channel guard accepts the container's ELF and refuses a Mach-O"

# ── Case 2s6a: the refusal NAMES the node's container platform, and is early ───
# An injected binary that cannot exec produces no error the operator ever sees:
# the MCP server stays `pending`, the agent loses submit_eval, and it surfaces
# four job escalations later as "the evaluator produced no output". So the
# refusal has to carry the whole chain, and the platform it measured against.
grep_log "chug_channel_ok \"\$STAGE/chuggernaut-channel\" b700"
grep_log "this node's docker runs arm64/linux containers"
grep_log "a Mach-O built for this mac"
grep_log "REFUSING (live daemon untouched, nothing installed)"
guard_line="$(line_of "chug_channel_ok \"\$STAGE/chuggernaut-channel\" b700")"
mac_put_line="$(line_of "chug_put 0755 \"\$STAGE/chuggernaut\" '/usr/local/bin/chuggernaut'")"
[ -n "$guard_line" ] && [ -n "$mac_put_line" ] || fail "expected both the channel guard and the install in the log"
[ "$guard_line" -lt "$mac_put_line" ] || fail "the channel guard must refuse BEFORE anything is installed"
# The platform the run measured against is reported, not left implicit.
grep -qF "air runs arm64/linux containers" "$WORK/macguard.out" \
  || fail "the operator must be told which container platform the channel binary was judged against"
echo "ok: the Darwin refusal names the container platform and comes before the install"

# ── Case 2s6b: a node whose docker cannot answer REFUSES; it does not guess ────
# Assuming arm64 because the mac is one is how a linux/amd64 colima would ship
# the same silent failure with a green deploy.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAKE_DOCKER_PLATFORM= \
  sh "$SUT" > "$WORK/noplat.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a Darwin node whose docker platform is unreadable must fail the deploy (got rc=0)"
grep -qF "cannot read the container platform" "$WORK/noplat.out" \
  || fail "the refusal must say what it could not read"
not_started "a node with no derivable container platform must not reach the agent bootstrap"
if grep -qF "chug_put 0755" "$LOG"; then
  fail "the platform refusal must come before anything is installed"
fi
echo "ok: an underivable container platform refuses instead of guessing"

# ── Case 2t: a platform this script cannot supervise refuses, before building ──
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@bsd \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=bsd \
  FAKE_NODE_OS=OpenBSD \
  sh "$SUT" >"$WORK/os.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unsupervisable platform must fail the deploy (got rc=0)"
grep -qF "there is no third supervisor here" "$WORK/os.out" || fail "the refusal must say why"
if grep -qF "docker build" "$LOG"; then
  fail "the platform refusal must come BEFORE the image builds"
fi
echo "ok: an unsupervisable platform refuses before anything is built"

# ── Case 3: no worker node ⇒ clean no-op ──────────────────────────────────────
: > "$LOG"
PATH="$BIN:$PATH" sh "$SUT"
if [ -s "$LOG" ]; then
  fail "build-worker.sh must no-op (no ssh) when WORKER_SSH is unset"
fi
echo "ok: no-op when WORKER_SSH unset"

# ── Case 4: stale image label ⇒ REFUSE the daemon restart ─────────────────────
# If the built worker image's chug.git.sha label does not match the requested
# SHA (a stale layer / silently failed build), the script must refuse to install
# the binary out of it and exit non-zero — the live daemon stays put.
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
not_started "stale image label must REFUSE the daemon restart"
if grep -qF "docker create chuggernaut/worker" "$LOG"; then
  fail "stale image label must not have its binary extracted"
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

# ── Case 5a: the probe is bounded to THIS start, on Linux ─────────────────────
# `docker logs` on a container the run had just created could only ever show that
# container's output. A unit's journal spans every generation the node has ever
# run, so an unbounded `journalctl -u chug-worker.service -n 50` would find a
# PREVIOUS generation's "worker up" on a quiet node — and under Restart=always a
# crash-looping daemon is `active (running)` on most polls, so the probe would
# report HEALTHY over a daemon that never reached NATS. That is exactly the
# silent 'deployed' #207 built this block to prevent. Bound by InvocationID,
# which systemd mints fresh per start.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_STALE_LOG=1 \
  PROBE_TIMEOUT_SECS=0 PROBE_INTERVAL_SECS=0 \
  sh "$SUT" >"$WORK/stale-linux.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a stale 'worker up' from an earlier invocation must NOT pass the probe (got rc=0)"
grep -qF "did NOT report healthy" "$WORK/stale-linux.out" \
  || fail "a daemon that never came up must time out loudly"
grep -qF "verified chug-worker is running" "$WORK/stale-linux.out" \
  && fail "must not claim the daemon is up on a previous generation's log line"
grep_log "_SYSTEMD_INVOCATION_ID"
echo "ok: the Linux probe reads only this start's journal, not the unit's history"

# ── Case 5b: the same bound on macOS, by truncating the agent's log ────────────
# launchd opens StandardOutPath append-only, so the file spans every generation
# too and there is no InvocationID to scope by. The install truncates it between
# `bootout` and `bootstrap` — the one window where the old agent is gone and the
# new one has not started — so the tail can only see the new agent.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=op@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  FAKE_NODE_OS=Darwin \
  FAKE_NODE_HOME=/Users/op \
  FAKE_STALE_LOG=1 \
  PROBE_TIMEOUT_SECS=0 PROBE_INTERVAL_SECS=0 \
  sh "$SUT" >"$WORK/stale-macos.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a stale 'worker up' in the agent's log must NOT pass the probe (got rc=0)"
grep -qF "did NOT report healthy" "$WORK/stale-macos.out" \
  || fail "a daemon that never came up must time out loudly on macOS too"
trunc_line="$(line_of ": > '/Users/op/Library/Logs/chuggernaut/worker.log'")"
bootout_line="$(line_of "launchctl bootout gui/\$(id -u)/com.chuggernaut.worker")"
boot_line="$(line_of "$STARTED_MACOS")"
[ -n "$trunc_line" ] || fail "the agent's log must be truncated before the new agent starts"
[ "$bootout_line" -lt "$trunc_line" ] || fail "truncate AFTER bootout — the old agent still holds the fd"
[ "$trunc_line" -lt "$boot_line" ] || fail "truncate BEFORE bootstrap, or the new agent's own output is lost"
echo "ok: the macOS probe reads only the new agent's log, truncated between bootout and bootstrap"

# ── Case 6: a per-node declaration wins over the bare one (ticket #390) ───────
# The fleet's nodes do not share paths — a colima node's cache lives under the
# mac home shared into the VM, a NixOS node's under /var/cache — so a
# single-valued chuggernaut.env can be true of only one of them, and the other
# node's spec then lives nowhere but on the node. That is the shape #265 reason 3
# predicted and #390 measured. <VAR>_<node> is how one env file declares a fleet;
# the bare value stays the default for the rest.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_SLOTS=4 \
  WORKER_SLOTS_air=2 \
  WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  WORKER_CACHE_DIR_air=/Users/op/chuggernaut-worker/sccache \
  sh "$SUT"

grep_log "WORKER_SLOTS='2'"
grep_log "WORKER_CACHE_DIR='/Users/op/chuggernaut-worker/sccache'"
if grep -qF "WORKER_SLOTS='4'" "$LOG"; then
  fail "a per-node WORKER_SLOTS_air must win over the bare WORKER_SLOTS"
fi
if grep -qF "WORKER_CACHE_DIR='/var/cache/chuggernaut/sccache'" "$LOG"; then
  fail "a per-node WORKER_CACHE_DIR_air must win over the bare WORKER_CACHE_DIR"
fi
# The per-node value is the one PROVISIONED too — a host dir created at the
# fleet-wide path would leave the node's every launch failing on a missing bind
# source (#379/#380) while the deploy claimed success.
grep_log "mkdir -p '/Users/op/chuggernaut-worker/sccache'"
echo "ok: <VAR>_<node> wins over the bare <VAR>, for the run spec and the provisioning"

# ── Case 6b: another node's declaration must not leak onto this node ──────────
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_SLOTS_air=2 \
  WORKER_CACHE_DIR_air=/Users/op/chuggernaut-worker/sccache \
  sh "$SUT"

if grep -qE "WORKER_SLOTS|WORKER_CACHE_DIR" "$LOG"; then
  fail "the air's declarations must not reach the nuc"
fi
echo "ok: a per-node declaration applies to that node only"

# ── Case 6c: WORKER_SSH is NOT resolved per node (prod's deploy depends on it) ─
# `WORKER_SSH` is the switch that says this machine can reach the node at all,
# and update.sh calls this script on every prod deploy expecting the no-op —
# the Mini cannot ssh a tagged worker. A per-node destination that turned the
# script ON would make every deploy try, and fail, to reach a node it cannot.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_SSH_air=worksalot@air \
  sh "$SUT" > "$WORK/nossh.out" 2>&1
if [ -s "$LOG" ]; then
  fail "WORKER_SSH_<node> must not switch the script on (prod's deploy relies on the no-op)"
fi
echo "ok: WORKER_SSH stays the switch — a per-node destination cannot turn the script on"

# ── Case 7: a live setting this run would DROP refuses the restart ────────────
# Overwriting the node's environment file makes the composed spec the node's
# whole truth, so anything the live daemon carries and this composition does not
# is dropped silently — caching off (#55), the boot capacity back at 4, or a node
# that keeps serving jobs and quietly stops updating. Refuse while the live
# daemon is still running, exactly as the label and capacity guards do.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_LIVE_ENV_FILE="WORKER_NODE='nuc'
WORKER_SLOTS='2'
WORKER_CACHE_DIR='/var/cache/chuggernaut/sccache'
WORKER_REFRESH_GIT_URL='ssh://git@front:2222/acme/chug.git'
PATH=/usr/bin" \
  sh "$SUT" >"$WORK/drift.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "a run spec that drops a live setting must fail the deploy (got rc=0)"
grep -qF "WORKER_SLOTS" "$WORK/drift.out" || fail "the refusal must name every dropped setting"
grep -qF "WORKER_CACHE_DIR" "$WORK/drift.out" || fail "the refusal must name every dropped setting"
grep -qF "REFUSING daemon restart" "$WORK/drift.out" || fail "a drop must refuse, not warn"
# The live side is the node's own environment file now (design #440 D7) — legible
# without `docker inspect`, and the guard says which side it read.
grep -qF "drift checked against the environment file /etc/chuggernaut/worker.env" "$WORK/drift.out" \
  || fail "the guard must name the declaration it compared against"
# The quoting is unwrapped, so a converted node does not report a change on every
# run for a value that did not change.
grep -qF "live ''2''" "$WORK/drift.out" && fail "the guard must unquote the environment file's values"
# WORKER_REFRESH_GIT_URL is always passed (empty when unset), so it is not
# dropped — but an empty one is its own loud line, not silence.
grep -qF "WORKER_REFRESH_GIT_URL is undeclared" "$WORK/drift.out" \
  || fail "an undeclared refresh URL must be loud on its own"
not_started "a dropped setting must not reach the daemon restart (live daemon untouched)"
echo "ok: a live setting nothing declares refuses the restart and names itself"

# ── Case 7a: the guard sees a node that has NOT been converted yet ─────────────
# The conversion is exactly the recreate this guard exists to police: a node
# whose only declaration is the live container's environment must still be
# compared, or the one deploy that replaces the container is the one deploy that
# drops everything silently.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_LIVE_ENV="WORKER_NODE=nuc
WORKER_SLOTS=2
WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache" \
  sh "$SUT" >"$WORK/convert.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "the conversion must be gated by the drift guard too (got rc=0)"
grep -qF "the live chug-worker CONTAINER (this run converts nuc to a native daemon)" "$WORK/convert.out" \
  || fail "the guard must say it fell back to the container's environment"
grep -qF "WORKER_SLOTS" "$WORK/convert.out" || fail "the conversion must name every dropped setting"
not_started "a dropping conversion must not reach the daemon restart"
echo "ok: the drift guard reads the live container when the node has no environment file yet"

# ── Case 7f: an UNREADABLE environment file REFUSES — it never reads as absent ─
# The failure this closes: "the file is not there" and "the file is there and I
# cannot read it" produce identical empty output. Collapse them and a converted
# node whose environment file the login user cannot `cat` falls through to a
# `docker inspect` that the same node also answers emptily, and the run prints
# the FRESH-NODE line while overwriting the node's whole run spec unchecked —
# #390's failure mode restored, invisibly, on the one path design #440 D7 exists
# to cover. So the node says which case it is, and "cannot read" refuses.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  FAKE_ENV_FILE_UNREADABLE=1 \
  sh "$SUT" >"$WORK/blind.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unreadable environment file must refuse the deploy (got rc=0)"
grep -qF "REFUSING daemon restart" "$WORK/blind.out" \
  || fail "a guard that cannot read the declaration must refuse, not warn"
grep -qF "/etc/chuggernaut/worker.env" "$WORK/blind.out" || fail "the refusal must name the file"
grep -qF "no live worker" "$WORK/blind.out" \
  && fail "an unreadable file must NEVER read as a fresh node"
not_started "an unreadable environment file must not reach the daemon restart"
echo "ok: an unreadable environment file refuses rather than degrading the guard to a pass"

# ── Case 7g: and the genuinely fresh node still says so, and proceeds ──────────
# The contrast that makes 7f a real distinction rather than a blanket refusal:
# no environment file AND no container is a node with nothing to drift, which
# declares its whole run spec and starts.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  sh "$SUT" >"$WORK/fresh.out" 2>&1
grep -qF "no live worker on worksalot@nuc to compare against" "$WORK/fresh.out" \
  || fail "a node with neither an environment file nor a container is a fresh node"
started || fail "a fresh node must proceed to the daemon start"
echo "ok: a node with no environment file and no container declares its whole run spec"

# ── Case 7b: the drop is deliberate ⇒ WORKER_SPEC_DROP_OK=1 proceeds, loudly ──
# Removing a setting on purpose is a real thing to want; it just has to be said
# out loud rather than happen by omission.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_SPEC_DROP_OK=1 \
  FAKE_LIVE_ENV_FILE="WORKER_NODE='nuc'
WORKER_CACHE_DIR='/var/cache/chuggernaut/sccache'" \
  sh "$SUT" >"$WORK/dropok.out" 2>&1

grep -qF "dropping WORKER_CACHE_DIR" "$WORK/dropok.out" \
  || fail "a deliberate drop must still say what it dropped"
started || fail "WORKER_SPEC_DROP_OK=1 must proceed to the daemon restart"
echo "ok: WORKER_SPEC_DROP_OK=1 proceeds and still names what it dropped"

# ── Case 7c: a fully declared spec is clean; a CHANGED value is informational ─
# The declaration is authoritative at node creation, so an edit to
# chuggernaut.env is meant to change the node — it reports, it does not refuse.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_SLOTS=4 \
  WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  FAKE_LIVE_ENV_FILE="WORKER_NODE='nuc'
WORKER_SLOTS='2'
WORKER_CACHE_DIR='/var/cache/chuggernaut/sccache'
WORKER_REFRESH_GIT_URL='ssh://git@front:2222/acme/chug.git'
WORKER_GIT_KEY='/etc/chuggernaut/keys/worker_git'" \
  sh "$SUT" >"$WORK/clean.out" 2>&1

grep -qF "REFUSING" "$WORK/clean.out" && fail "a fully declared spec must not refuse"
grep -qF "WORKER_SLOTS: live '2' -> declared '4'" "$WORK/clean.out" \
  || fail "a changed value must be reported before it is applied"
grep -qF "WORKER_CACHE_DIR: live" "$WORK/clean.out" \
  && fail "an unchanged value must not be reported as a change (the quoting is unwrapped)"
started || fail "a declared spec must proceed to the daemon restart"
echo "ok: a declared spec proceeds, reporting what the deploy changes"

# ── Case 7d: a knob this script does not FORWARD is a drop, declared or not ───
# WORKER_SLOTS_MAX is the documented one (env.example): nothing forwards it, so
# every daemon recreation drops it whatever chuggernaut.env says. Comparing
# against the composed environment file rather than against the environment is
# what keeps this from reading as clean.
: > "$LOG"
set +e
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@nuc \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=nuc \
  WORKER_SLOTS_MAX=2 \
  FAKE_LIVE_ENV_FILE="WORKER_NODE='nuc'
WORKER_SLOTS_MAX='2'" \
  sh "$SUT" >"$WORK/unforwarded.out" 2>&1
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "an unforwarded live setting must fail the deploy (got rc=0)"
grep -qF "build-worker.sh does not forward WORKER_SLOTS_MAX" "$WORK/unforwarded.out" \
  || fail "the refusal must say the value is declared but never forwarded"
echo "ok: a declared-but-unforwarded setting is reported as the drop it is"

# ── Case 7e: WORKER_MODES round-trips through the environment file ────────────
# The regression test for the bug #439 closed, re-asserted over the new
# declaration (design #440 D7): a node whose live daemon carries WORKER_MODES
# must have it forwarded, not dropped — and a CHANGED one reports, exactly as
# WORKER_SLOTS does. Round-trip means the value read back off the node's own
# environment file is the value that was written there.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_MODES=container,host \
  WORKER_SLOTS=1 \
  WORKER_REFRESH_GIT_URL="ssh://git@front:2222/acme/chug.git" \
  FAKE_LIVE_ENV_FILE="WORKER_NODE='air'
WORKER_MODES='container'
WORKER_SLOTS='1'
WORKER_REFRESH_GIT_URL='ssh://git@front:2222/acme/chug.git'
WORKER_GIT_KEY='/etc/chuggernaut/keys/worker_git'" \
  sh "$SUT" >"$WORK/modes-drift.out" 2>&1

grep -qF "does not forward WORKER_MODES" "$WORK/modes-drift.out" \
  && fail "WORKER_MODES is forwarded — the drift guard must not call it a drop"
grep -qF "REFUSING" "$WORK/modes-drift.out" && fail "a declared, forwarded WORKER_MODES must not refuse"
grep -qF "WORKER_MODES: live 'container' -> declared 'container,host'" "$WORK/modes-drift.out" \
  || fail "a changed WORKER_MODES must be reported before it is applied"
grep_log "WORKER_MODES='container,host'"
started || fail "a forwarded WORKER_MODES must proceed to the daemon restart"

# And the unchanged case: the value the node already carries is written back
# byte-identically, so the next run sees no change and no drop.
: > "$LOG"
PATH="$BIN:$PATH" \
  WORKER_SSH=worksalot@air \
  WORKER_NATS_URL=nats://10.0.0.1:4222 \
  CHUG_WORKER_NODE=air \
  WORKER_MODES="container, host" \
  WORKER_SLOTS=1 \
  FAKE_LIVE_ENV_FILE="WORKER_NODE='air'
WORKER_MODES='container, host'" \
  sh "$SUT" >"$WORK/modes-roundtrip.out" 2>&1
grep -qF "WORKER_MODES" "$WORK/modes-roundtrip.out" \
  && fail "an unchanged WORKER_MODES must round-trip silently — no drop, no change line"
case "$(env_file)" in
  *"WORKER_MODES='container, host'"*) ;;
  *) fail "the round-tripped WORKER_MODES must be written back byte-identically" ;;
esac
echo "ok: WORKER_MODES round-trips through the environment file, and a change reports"

echo "ALL PASS"
