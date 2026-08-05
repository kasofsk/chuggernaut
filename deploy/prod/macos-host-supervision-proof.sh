#!/bin/sh
# Design #440 D3, the macOS half: does a host task survive a restart of the
# launchd agent that launched it?
#
# #440 marks launchd's process-group teardown semantics as SECONDHAND — no file
# in this repo states them — and slice 2 must prove rather than assume. This
# cannot run in CI: the evaluation gate is a Debian container and there is no
# launchd in it. So it is an OPERATOR-VERIFIED proof, in the shape of
# .chug/jobs/android-proof.yaml and .chug/jobs/gcp-proof.yaml: one command, one
# answer. See docs/reference/runbooks/macos-host-supervision-proof.md.
#
#   sh deploy/prod/macos-host-supervision-proof.sh
#
# It stands in for the daemon with a LaunchAgent whose program backgrounds one
# task under `set -m`, which is the shell's equivalent of the `process_group(0)`
# `HostBackend::spawn_task` sets — the whole of Supervision::ProcessGroup. Then
# it does to that agent what `worker-refresh.sh` would do to a native daemon
# (`launchctl kickstart -k`) and reports whether the task lived through it and
# still landed its exit code, the way the task's own wrapper writes it.
#
# The agent's plist is written to a TEMP DIRECTORY and never to
# deploy/prod/launchd/: install-launchd.sh globs that directory, so a template
# left there would install this proof on the Mini (#440 §2).
#
# Exit: 0 = the task survived (D3's macOS mechanism holds). 1 = it did not, or
# the proof could not be set up — either way the verdict line says which.
set -eu

LABEL="com.chuggernaut.host-supervision-proof"
UID_N="$(id -u)"
DOMAIN="gui/$UID_N"

fail() {
	echo "FAIL: $*"
	exit 1
}

[ "$(uname -s)" = "Darwin" ] || fail "this proof is launchd's and only runs on macOS (this is $(uname -s))"
command -v launchctl >/dev/null 2>&1 || fail "no launchctl on PATH"

OUT="$(mktemp -d /tmp/chug-supervision-proof.XXXXXX)"
cleanup() {
	launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
	[ -z "${TASK_PID:-}" ] || kill -9 "$TASK_PID" 2>/dev/null || true
	rm -rf "$OUT"
}
trap cleanup EXIT

# The stand-in daemon. `set -m` puts the backgrounded task in its own process
# group, which is what spawn_task's process_group(0) does. The guard makes the
# instance launchd starts AFTER the kickstart spawn no second task, so the pid
# and the exit code this proof reads are the first task's.
cat >"$OUT/daemon.sh" <<'SH'
#!/bin/sh
set -m
if [ -e "$OUT/task.pid" ]; then
	exec sleep 3600
fi
sh -c 'i=0; while [ $i -lt 60 ]; do i=$((i+1)); sleep 0.5; done; printf 7 > "$OUT/exit_code"' &
task=$!
printf %s "$task" >"$OUT/task.pid.tmp"
mv "$OUT/task.pid.tmp" "$OUT/task.pid"
printf %s "$$" >"$OUT/daemon.pid"
while :; do sleep 1; done
SH
chmod +x "$OUT/daemon.sh"

cat >"$OUT/$LABEL.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>/bin/sh</string><string>$OUT/daemon.sh</string></array>
  <key>EnvironmentVariables</key>
  <dict><key>OUT</key><string>$OUT</string></dict>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>$OUT/daemon.log</string>
  <key>StandardErrorPath</key><string>$OUT/daemon.log</string>
</dict>
</plist>
PLIST
plutil -lint "$OUT/$LABEL.plist" >/dev/null || fail "the generated plist is malformed"

launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$OUT/$LABEL.plist" || fail "launchctl bootstrap refused the agent"

i=0
while [ ! -e "$OUT/task.pid" ] && [ "$i" -lt 200 ]; do
	i=$((i + 1))
	sleep 0.1
done
[ -e "$OUT/task.pid" ] || fail "the stand-in daemon never launched a task (see $OUT/daemon.log)"
TASK_PID="$(cat "$OUT/task.pid")"
DAEMON_PID="$(cat "$OUT/daemon.pid")"

task_pgid="$(ps -o pgid= -p "$TASK_PID" | tr -d ' ')"
daemon_pgid="$(ps -o pgid= -p "$DAEMON_PID" | tr -d ' ')"
echo "task pid=$TASK_PID pgid=$task_pgid ; stand-in daemon pid=$DAEMON_PID pgid=$daemon_pgid"
[ -n "$task_pgid" ] || fail "the task was already gone before the kickstart"
[ "$task_pgid" != "$daemon_pgid" ] ||
	fail "the task shares the daemon's process group, so this run tested nothing — Supervision::ProcessGroup assumes they differ"

echo "kicking: launchctl kickstart -k $DOMAIN/$LABEL"
launchctl kickstart -k "$DOMAIN/$LABEL" || fail "launchctl kickstart refused"
sleep 2

kill -0 "$TASK_PID" 2>/dev/null ||
	fail "the task died with the agent that launched it — #440 D3's macOS mechanism does NOT hold, and #322 §6's per-task launchd job is the fallback"

i=0
while [ ! -e "$OUT/exit_code" ] && [ "$i" -lt 600 ]; do
	i=$((i + 1))
	sleep 0.1
done
[ -e "$OUT/exit_code" ] || fail "the task outlived the kickstart but never landed its exit code"
code="$(cat "$OUT/exit_code")"
[ "$code" = "7" ] || fail "the task landed exit code '$code', not the 7 it wrote"

echo "PASS: the task survived 'launchctl kickstart -k' of the agent that launched it and landed exit code 7"
echo "      => design #440 D3's macOS mechanism (the process group) holds on $(sw_vers -productVersion 2>/dev/null || echo 'this macOS')"
echo "      Record the result in docs/design/440-native-worker-daemon.md before relying on it."
