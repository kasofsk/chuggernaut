#!/bin/sh
# deploy/prod/install-worker-launchd.sh — the macOS half of design #440 slice 7.
# Drives the real script against stubbed `uname`, `launchctl`, `plutil` and
# `docker` in a throwaway $HOME, and pins the two things that matter about it: the agent it
# renders is the one `build-worker.sh` renders (one shape, not two), and it
# cannot arrive on the Mini — not by a glob, not by another script calling it,
# and not by an operator running it on the control-plane host by hand.
#
# Run: sh deploy/prod/install-worker-launchd.test.sh
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SUT="$HERE/install-worker-launchd.sh"
BUILDER="$HERE/build-worker.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM
BIN="$WORK/bin"
HOME_DIR="$WORK/home"
LOG="$WORK/calls.log"
mkdir -p "$BIN" "$HOME_DIR/Library/LaunchAgents" "$HOME_DIR/chuggernaut-worker" "$WORK/usr"

fail() { echo "FAIL: $1" >&2; exit 1; }

cat > "$BIN/uname" <<EOF
#!/bin/sh
[ "\${1:-}" = -s ] || exit 1
echo "\${FAKE_OS:-Darwin}"
EOF

cat > "$BIN/launchctl" <<EOF
#!/bin/sh
echo "launchctl \$*" >> "$LOG"
case "\${1:-}" in
  print) case "\$2" in *"\${FAKE_LIVE_AGENT:-__none__}") exit 0 ;; *) exit 1 ;; esac ;;
esac
exit 0
EOF

cat > "$BIN/plutil" <<EOF
#!/bin/sh
echo "plutil \$*" >> "$LOG"
exit 0
EOF

# Real docker's shape, not a convenient one: `rm --force` exits 0 whether or not
# the container was there, so `inspect` is the only subcommand that answers "is
# there one" and `info` the only one that answers "can I ask at all".
cat > "$BIN/docker" <<EOF
#!/bin/sh
echo "docker \$*" >> "$LOG"
case "\${1:-}" in
  info) [ -z "\${FAKE_NO_DOCKERD:-}" ] ;;
  inspect) [ -n "\${FAKE_CONTAINER:-}" ] ;;
  *) exit 0 ;;
esac
EOF

chmod +x "$BIN/uname" "$BIN/launchctl" "$BIN/plutil" "$BIN/docker"

# A docker that is absent and a dockerd that will not answer are different
# refusals, and the only honest way to stage the first is a PATH with no docker
# on it at all — a stub cannot be missing.
NODOCKER="$WORK/nodocker"
mkdir -p "$NODOCKER"
for tool in sh dirname id sed mkdir rm cat; do
	real="$(command -v "$tool")" || fail "this host has no $tool, so the suite cannot stage a docker-less PATH"
	ln -s "$real" "$NODOCKER/$tool"
done
ln -s "$BIN/uname" "$BIN/launchctl" "$BIN/plutil" "$NODOCKER/"

ENV_FILE="$HOME_DIR/chuggernaut-worker/worker.env"
BINARY="$WORK/usr/chuggernaut"
PLIST="$HOME_DIR/Library/LaunchAgents/com.chuggernaut.worker.plist"
printf "WORKER_NODE='air'\n" > "$ENV_FILE"
printf '#!/bin/sh\n' > "$BINARY"
chmod +x "$BINARY"

run() {
	: > "$LOG"
	rc=0
	env PATH="${RUN_PATH:-$BIN:$PATH}" HOME="$HOME_DIR" WORKER_BINARY="$BINARY" "$@" \
		sh "$SUT" > "$WORK/out" 2>&1 || rc=$?
	return 0
}
installed() { grep -q "launchctl bootstrap" "$LOG"; }
refused() { [ "$rc" -ne 0 ] || fail "$1 (rc=0)"; }

# ── Case 1: the agent is the one build-worker.sh renders ──────────────────────
# The Mini installs the dispatcher and api from templates; a mac worker node
# `WORKER_SSH` never reaches installs its own the same way, and both must land
# the same agent. Read out of the script rather than restated, so a change there
# fails this rather than drifting.
rm -f "$PLIST"
run
[ "$rc" -eq 0 ] || fail "the happy path must install: $(cat "$WORK/out")"
[ -f "$PLIST" ] || fail "no plist was written"

MAC_PATH=/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
sed -n '/^  PLIST_TEXT="<?xml/,/^<\/plist>"$/p' "$BUILDER" |
	sed -e '1s|^  PLIST_TEXT="||' -e '$s|"$||' -e 's|\\"|"|g' \
		-e 's|\$AGENT_LABEL|com.chuggernaut.worker|g' \
		-e "s|\${WORKER_PATH:-[^}]*}|$MAC_PATH|g" \
		-e "s|\$ENV_FILE|$ENV_FILE|g" \
		-e "s|\$BIN_DIR/chuggernaut|$BINARY|g" \
		-e "s|\$WORKER_LOG_PATH|$HOME_DIR/Library/Logs/chuggernaut/worker.log|g" \
		-e "s|\$NODE_HOME|$HOME_DIR|g" > "$WORK/from-builder"
sed '/^<!--/,/-->$/d' "$PLIST" > "$WORK/from-template"
[ -s "$WORK/from-builder" ] || fail "no plist could be read out of $BUILDER — the extraction, not the agent, is what broke"
if ! diff -u "$WORK/from-builder" "$WORK/from-template" > "$WORK/plist.diff"; then
	fail "the agent this installer renders is not the agent build-worker.sh renders:
$(cat "$WORK/plist.diff")"
fi
grep -q "plutil -lint" "$LOG" || fail "the rendered plist must be linted before it is bootstrapped"
[ "$(grep -n 'launchctl bootout' "$LOG" | cut -d: -f1 | head -n 1)" \
	-lt "$(grep -n 'launchctl bootstrap' "$LOG" | cut -d: -f1 | head -n 1)" ] ||
	fail "the old agent must be booted out before the new one is bootstrapped"
echo "ok: the installer and build-worker.sh render one launchd agent"

# ── Case 2: it cannot be installed on the Mini ────────────────────────────────
# Job #467's finding: install-launchd.sh globs its template directory, so a
# worker template beside the dispatcher's would be installed on the control
# plane. Three independent locks, asserted one at a time.
for tmpl in "$HERE/launchd/"*.plist.template; do
	case "$(basename "$tmpl")" in
	*worker*) fail "the worker template is in the directory install-launchd.sh globs — it would be installed on the Mini" ;;
	esac
done
[ -f "$HERE/launchd-worker/com.chuggernaut.worker.plist.template" ] ||
	fail "the worker template must live outside deploy/prod/launchd/"

# Suites are excluded because naming it is their job; everything else that runs
# on the Mini or on a node is not allowed to name it at all, which is the
# strongest rule that has no false negatives.
CALLERS="$(cd "$REPO" && git ls-files '*.sh' '*.yaml' '.githooks/*' |
	grep -v '\.test\.sh$' | grep -v 'install-worker-launchd' |
	xargs grep -l 'install-worker-launchd\.sh' 2> /dev/null || true)"
[ -z "$CALLERS" ] || fail "nothing may invoke the installer — it is opt-in and an operator types its name. Found: $CALLERS"

rm -f "$PLIST"
touch "$HOME_DIR/Library/LaunchAgents/com.chuggernaut.dispatcher.plist"
run
refused "a control-plane mac must refuse the worker agent"
grep -q "CHUG_WORKER_ON_CONTROL_PLANE=1" "$WORK/out" || fail "the refusal must name its own override"
! installed || fail "a refused install must bootstrap nothing"
[ ! -f "$PLIST" ] || fail "a refused install must write no plist"

rm -f "$HOME_DIR/Library/LaunchAgents/com.chuggernaut.dispatcher.plist"
run FAKE_LIVE_AGENT=com.chuggernaut.api
refused "a mac whose api agent is LIVE must refuse even with no plist on disk"

run CHUG_WORKER_ON_CONTROL_PLANE=1 FAKE_LIVE_AGENT=com.chuggernaut.api
[ "$rc" -eq 0 ] || fail "the override must let a deliberate operator through: $(cat "$WORK/out")"
echo "ok: the glob cannot reach it, nothing calls it, and a control-plane mac refuses it"

# ── Case 3: it installs a lifecycle and never a run spec ───────────────────────
# design #440 D2's split. The environment file is the platform's, so a missing
# one is refused here rather than boot-looped under KeepAlive on the node.
rm -f "$PLIST"
mv "$ENV_FILE" "$WORK/env.stashed"
run
refused "an agent without its environment file must be refused"
grep -q "$ENV_FILE" "$WORK/out" || fail "the refusal must name the file it wanted"
grep -q "build-worker.sh" "$WORK/out" || fail "the refusal must name what renders it"
[ ! -f "$PLIST" ] || fail "a refused install must write no plist"
mv "$WORK/env.stashed" "$ENV_FILE"

chmod -x "$BINARY"
run
refused "an agent without its daemon binary must be refused"
chmod +x "$BINARY"
echo "ok: a missing run spec or binary refuses before anything is installed"

# ── Case 4: not a mac, and the uninstall ──────────────────────────────────────
run FAKE_OS=Linux
refused "a Linux host must be refused a launchd agent"
grep -q "#440 D2" "$WORK/out" || fail "the refusal must say which supervisor Linux gets"

run
[ "$rc" -eq 0 ] || fail "re-install must work: $(cat "$WORK/out")"
: > "$LOG"
rc=0
env PATH="$BIN:$PATH" HOME="$HOME_DIR" sh "$SUT" uninstall > "$WORK/out" 2>&1 || rc=$?
[ "$rc" -eq 0 ] || fail "uninstall must succeed: $(cat "$WORK/out")"
grep -q "launchctl bootout gui/.*/com.chuggernaut.worker" "$LOG" || fail "uninstall must bootout the agent"
[ ! -f "$PLIST" ] || fail "uninstall must remove the plist"
[ -f "$ENV_FILE" ] || fail "uninstall must leave the run spec alone — it is not this script's"
echo "ok: a non-mac refuses, and uninstall removes the agent and nothing else"

# ── Case 5: it never leaves two daemons on one node name ──────────────────────
# The preconditions above do not exclude a mac still running the CONTAINERIZED
# daemon under `--restart=always` — a hand-placed binary and environment file are
# the whole use case. build-worker.sh closes that at its own bootstrap; this must
# close it at this one, or the node ends in the state worker-refresh.sh refuses
# its swap over (#440 §1).
grep -qF 'docker rm -f chug-worker' "$BUILDER" ||
	fail "build-worker.sh no longer removes the container at its bootstrap — the two installers must close this the same way or say why not"

run FAKE_CONTAINER=1
[ "$rc" -eq 0 ] || fail "a node with a live container must still install: $(cat "$WORK/out")"
grep -q "docker rm -f chug-worker" "$LOG" || fail "the containerized daemon must be removed — the agent claims the same WORKER_NODE"
[ "$(grep -n 'docker rm -f' "$LOG" | cut -d: -f1 | head -n 1)" \
	-lt "$(grep -n 'launchctl bootstrap' "$LOG" | cut -d: -f1 | head -n 1)" ] ||
	fail "the container must be gone BEFORE the agent starts, not after"
grep -q "removed the containerized" "$WORK/out" || fail "removing another supervisor's daemon must be announced, not silent"

run
[ "$rc" -eq 0 ] || fail "a node with no container must install unchanged: $(cat "$WORK/out")"
! grep -q "docker rm" "$LOG" ||
	fail "there was nothing to remove, so nothing may be removed — 'docker rm -f' exits 0 either way, so its status is not an existence check"
! grep -q "removed the containerized" "$WORK/out" ||
	fail "nothing was removed, so nothing may be announced"
echo "ok: the container is gone before the agent starts, and only then is it announced"

# ── Case 6: a docker it cannot ask is a refusal, not a shrug ──────────────────
# The two ways the question goes unanswered — no daemon, and no docker at all —
# are the same non-zero exit and are NOT "nothing to remove": a stopped
# `--restart=always` container is invisible to both and comes back when dockerd
# does. Refusing is the same stance as every other precondition here, so it must
# happen before anything is written.
rm -f "$PLIST"
run FAKE_NO_DOCKERD=1
refused "a node whose dockerd does not answer must be refused"
grep -q "colima may be down" "$WORK/out" || fail "the refusal must say which of the two ways it failed to ask"
grep -q "CHUG_WORKER_SKIP_DOCKER_CHECK=1" "$WORK/out" || fail "the refusal must name its own override"
! installed || fail "a refused install must bootstrap nothing"
[ ! -f "$PLIST" ] || fail "a refused install must write no plist"

RUN_PATH="$NODOCKER"
run
refused "a node with no docker on its PATH must be refused too"
grep -q "no docker on this PATH" "$WORK/out" || fail "the refusal must say which of the two ways it failed to ask"
[ ! -f "$PLIST" ] || fail "a refused install must write no plist"

run CHUG_WORKER_SKIP_DOCKER_CHECK=1
[ "$rc" -eq 0 ] || fail "the override must let a mac that never ran a container through: $(cat "$WORK/out")"
installed || fail "the override must install"
unset RUN_PATH
echo "ok: an unanswerable docker refuses before anything is written, and says which override means it"

echo "PASS: deploy/prod/install-worker-launchd.sh"
