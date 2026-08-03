#!/bin/sh
# Shell test for android-proof.sh — no KVM, no SDK, no Flutter, no emulator.
#
# It drives the real ladder against stubbed `flutter`, `adb`, `avdmanager` and
# `emulator` on a controlled PATH, with a sandbox SDK, a sandbox Flutter root, a
# regular file standing in for /dev/kvm and every bound turned down to seconds.
# What it pins is the part of the script no run on the node would notice was
# broken until it mattered:
#
#   1. THE ORDER OF THE RUNGS. Each proves something different, and a cheap rung
#      running after an expensive one destroys the signal: an accel-check that
#      happens after a 20-minute Gradle build no longer distinguishes "the device
#      is gone" from "the build broke".
#   2. THE LADDER IS PRINTED LAST, on the failing path as much as the passing
#      one, because a worker keeps only the final 700 KiB of a task's logs
#      (LOGS_CAP, crates/worker/src/daemon.rs) and the ladder IS the deliverable.
#   3. A FAILURE NAMES ITS RUNG EXACTLY ONCE, and the rungs AFTER it — never the
#      failing one itself — read NOT REACHED rather than silently missing.
#   4. EVERY WAIT REALLY IS BOUNDED (STYLE.md Tier 2 rule 3) and the failure
#      names the bound in seconds — an emulator that never reports
#      `sys.boot_completed` must fail its rung, not hang until `task_timeout`.
#      Including the case that bound is really for: a device that accepts the
#      adb connection and then never answers, where an unbounded probe would
#      park inside `adb shell` and the wait's own bound could never fire.
#   5. A BUILD THAT EXITS 0 WITHOUT PRODUCING AN APK STILL FAILS. `flutter build`
#      is the rung's evidence, not its verdict (the coverage.test.sh lesson: a
#      stub that only logs lets the script claim work it never did).
#
# Run:  .chug/tasks/android-proof.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
LADDER_SUT="$HERE/android-proof.sh"

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

BIN="$SANDBOX/bin"
STATE="$SANDBOX/state"
FIXTURE="$SANDBOX/fixture"
SDK="$SANDBOX/sdk"
CMD_LOG="$SANDBOX/cmd.log"
APK="$FIXTURE/build/app/outputs/flutter-apk/app-debug.apk"
mkdir -p "$BIN" "$STATE" "$FIXTURE" "$SANDBOX/home" "$SANDBOX/flutter/bin" \
	"$SDK/system-images/android-34/google_apis/x86_64"
echo "name: chug_mobile_proof" >"$FIXTURE/pubspec.yaml"

BAD=0
verdict() { # verdict <what> <ok|no>
	if [ "$2" = "ok" ]; then
		echo "  ok   $1"
	else
		echo "  BAD  $1"
		BAD=$((BAD + 1))
	fi
}
yes_no() { if [ "$1" = 0 ]; then echo ok; else echo no; fi }
found() { grep -qF -e "$2" "$1" 2>/dev/null && echo ok || echo no; }
missing() { grep -qF -e "$2" "$1" 2>/dev/null && echo no || echo ok; }
line_of() { grep -nF -e "$2" "$1" 2>/dev/null | head -n 1 | cut -d: -f1; }
before() { # before <log> <earlier> <later>
	_a="$(line_of "$1" "$2")"
	_b="$(line_of "$1" "$3")"
	[ -n "$_a" ] && [ -n "$_b" ] && [ "$_a" -lt "$_b" ] && echo ok || echo no
}

# --- stubs -------------------------------------------------------------------
# Every stub appends its argv to one log, so the ORDER of the rungs is readable
# from a single file. Their outcomes are steered by marker files under $STATE,
# which is what lets one set of stubs serve every case. The `flutter` stub really
# does write the APK when told to, because the script asserts the artifact exists
# rather than trusting the exit code.
write_stubs() {
	cat >"$BIN/flutter" <<EOF
#!/bin/sh
echo "flutter \$*" >> "$CMD_LOG"
case "\$1" in
--version) cat "$STATE/version"; exit 0 ;;
build)
  if [ -f "$STATE/emit_apk" ]; then
    mkdir -p "$(dirname "$APK")"
    echo apk > "$APK"
  fi
  exit "\$(cat "$STATE/build_rc")" ;;
install) : > "$STATE/installed"; exit 0 ;;
esac
exit 0
EOF

	cat >"$BIN/adb" <<EOF
#!/bin/sh
echo "adb \$*" >> "$CMD_LOG"
case "\$1" in
get-serialno) echo emulator-5554; exit 0 ;;
shell)
  case "\$2 \$3" in
  "getprop sys.boot_completed")
    [ -f "$STATE/adb_hangs" ] && sleep 30
    [ -f "$STATE/booted" ] && echo 1; exit 0 ;;
  "getprop ro.build.version.sdk") echo 34; exit 0 ;;
  esac
  case "\$2" in
  pm) [ -f "$STATE/installed" ] && echo "package:xyz.kasofsk.chug_mobile_proof"; exit 0 ;;
  pidof) [ -f "$STATE/app_running" ] && echo 4242; exit 0 ;;
  esac ;;
esac
exit 0
EOF

	cat >"$BIN/emulator" <<EOF
#!/bin/sh
echo "emulator \$*" >> "$CMD_LOG"
case "\$1" in
-accel-check) exit "\$(cat "$STATE/accel_rc")" ;;
esac
echo "emulator stub: pretending to boot"
sleep 20
EOF

	printf '#!/bin/sh\necho "avdmanager $*" >> "%s"\nexit 0\n' "$CMD_LOG" >"$BIN/avdmanager"
	chmod +x "$BIN"/*
}

# The node's shape by default: device granted, both toolchains mounted, the
# emulator boots, the app comes up. Each case spoils exactly one of those.
reset_case() {
	rm -rf "$STATE" "$FIXTURE/build"
	mkdir -p "$STATE"
	: >"$CMD_LOG"
	printf 'Flutter 3.41.2 - channel stable\n' >"$STATE/version"
	echo 0 >"$STATE/accel_rc"
	echo 0 >"$STATE/build_rc"
	: >"$STATE/emit_apk"
	: >"$STATE/booted"
	: >"$STATE/app_running"
	: >"$SANDBOX/kvm"
	write_stubs
}

run_ladder() { # run_ladder <stdout-file>; the sandbox is the whole world
	set +e
	env -i PATH="$BIN:/usr/bin:/bin" \
		HOME="$SANDBOX/home" \
		ANDROID_SDK_ROOT="$SDK" ANDROID_HOME="$SDK" \
		ANDROID_USER_HOME="$SANDBOX/home/.android" \
		FLUTTER_ROOT="$SANDBOX/flutter" \
		CHUG_KVM_DEVICE="$SANDBOX/kvm" \
		CHUG_ANDROID_FIXTURE_DIR="$FIXTURE" \
		CHUG_ANDROID_TOOL_TIMEOUT_SECS=10 \
		CHUG_ANDROID_BUILD_TIMEOUT_SECS=10 \
		CHUG_ANDROID_ADB_TIMEOUT_SECS=10 \
		CHUG_ANDROID_BOOT_TIMEOUT_SECS=3 \
		CHUG_ANDROID_APP_TIMEOUT_SECS=3 \
		CHUG_ANDROID_POLL_INTERVAL_SECS=1 \
		CHUG_ANDROID_PROBE_TIMEOUT_SECS=1 \
		sh "$LADDER_SUT" >"$1" 2>&1
	STATUS=$?
	set -e
}

# --- case 1: the whole ladder ------------------------------------------------
echo "case 1: a granted container clears all five rungs, cheapest proof first"
reset_case
run_ladder "$SANDBOX/out1.txt"

verdict "exits 0" "$(yes_no "$STATUS")"
verdict "rung 1 passes on the device and the mounts" "$(found "$SANDBOX/out1.txt" "rung 1 PASS")"
verdict "rung 2 passes on acceleration" "$(found "$SANDBOX/out1.txt" "rung 2 PASS")"
verdict "rung 3 passes on the toolchains" "$(found "$SANDBOX/out1.txt" "rung 3 PASS")"
verdict "rung 4 passes on the APK" "$(found "$SANDBOX/out1.txt" "rung 4 PASS")"
verdict "rung 5 passes on the emulator" "$(found "$SANDBOX/out1.txt" "rung 5 PASS")"
verdict "accel-check runs before the build" \
	"$(before "$CMD_LOG" "emulator -accel-check" "flutter build apk --debug")"
verdict "the build runs before the AVD is created" \
	"$(before "$CMD_LOG" "flutter build apk --debug" "avdmanager create avd")"
verdict "the emulator boots before the app is installed" \
	"$(before "$CMD_LOG" "emulator -avd" "flutter install")"
verdict "the emulator is headless" "$(found "$CMD_LOG" "-no-window -no-audio")"
verdict "it waits for the device, then for sys.boot_completed" \
	"$(before "$CMD_LOG" "adb wait-for-device" "getprop sys.boot_completed")"
verdict "it names the expected Flutter version" \
	"$(found "$SANDBOX/out1.txt" "flutter is the expected 3.41.2")"
verdict "the VERDICT is the very last line" \
	"$(tail -n 1 "$SANDBOX/out1.txt" | grep -q 'VERDICT PASS' && echo ok || echo no)"

# --- case 2: no device --------------------------------------------------------
echo "case 2: a container with no /dev/kvm fails rung 1 and spends nothing"
reset_case
rm -f "$SANDBOX/kvm"
run_ladder "$SANDBOX/out2.txt"

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names rung 1" "$(found "$SANDBOX/out2.txt" "RUNG 1 FAILED")"
verdict "points at the allow-list" "$(found "$SANDBOX/out2.txt" "WORKER_KVM_PROJECTS")"
verdict "rung 4 reads NOT REACHED" "$(found "$SANDBOX/out2.txt" "rung 4 a Flutter APK build: NOT REACHED")"
verdict "the failed rung is not ALSO NOT REACHED" \
	"$(missing "$SANDBOX/out2.txt" "rung 1 the mounts and env are present: NOT REACHED")"
verdict "still prints the ladder last" \
	"$(tail -n 1 "$SANDBOX/out2.txt" | grep -q 'VERDICT FAIL at rung 1' && echo ok || echo no)"
verdict "invokes no toolchain at all" "$([ ! -s "$CMD_LOG" ] && echo ok || echo no)"

# --- case 3: the mounts are there, the env is not -----------------------------
echo "case 3: a missing FLUTTER_ROOT is a rung-1 failure, not a build failure"
reset_case
set +e
env -i PATH="$BIN:/usr/bin:/bin" HOME="$SANDBOX/home" \
	ANDROID_SDK_ROOT="$SDK" ANDROID_HOME="$SDK" \
	ANDROID_USER_HOME="$SANDBOX/home/.android" \
	CHUG_KVM_DEVICE="$SANDBOX/kvm" CHUG_ANDROID_FIXTURE_DIR="$FIXTURE" \
	sh "$LADDER_SUT" >"$SANDBOX/out3.txt" 2>&1
STATUS=$?
set -e

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names FLUTTER_ROOT" "$(found "$SANDBOX/out3.txt" "FLUTTER_ROOT")"
verdict "names rung 1" "$(found "$SANDBOX/out3.txt" "RUNG 1 FAILED")"

# --- case 4: the device is present but unusable -------------------------------
echo "case 4: a failing accel-check stops the ladder before any build"
reset_case
echo 1 >"$STATE/accel_rc"
run_ladder "$SANDBOX/out4.txt"

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names rung 2" "$(found "$SANDBOX/out4.txt" "RUNG 2 FAILED")"
verdict "never builds" "$(missing "$CMD_LOG" "flutter build")"
verdict "rung 5 reads NOT REACHED" "$(found "$SANDBOX/out4.txt" "rung 5 an emulator boots and a device-backed task runs: NOT REACHED")"
verdict "the failed rung is not ALSO NOT REACHED" \
	"$(missing "$SANDBOX/out4.txt" "rung 2 acceleration is real: NOT REACHED")"
verdict "the cleared rung is not NOT REACHED either" \
	"$(missing "$SANDBOX/out4.txt" "rung 1 the mounts and env are present: NOT REACHED")"

# --- case 5: the build fails ---------------------------------------------------
echo "case 5: a failing APK build stops the ladder before an emulator is booted"
reset_case
echo 1 >"$STATE/build_rc"
run_ladder "$SANDBOX/out5.txt"

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names rung 4" "$(found "$SANDBOX/out5.txt" "RUNG 4 FAILED")"
verdict "boots no emulator" "$(missing "$CMD_LOG" "emulator -avd")"

# --- case 6: the build lies ----------------------------------------------------
echo "case 6: a build that exits 0 without an APK is still a rung-4 failure"
reset_case
rm -f "$STATE/emit_apk"
run_ladder "$SANDBOX/out6.txt"

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names rung 4" "$(found "$SANDBOX/out6.txt" "RUNG 4 FAILED")"
verdict "names the missing artifact" "$(found "$SANDBOX/out6.txt" "app-debug.apk is absent")"

# --- case 7: the emulator never boots -------------------------------------------
echo "case 7: an emulator that never reports sys.boot_completed fails on its bound"
reset_case
rm -f "$STATE/booted"
run_ladder "$SANDBOX/out7.txt"

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names rung 5" "$(found "$SANDBOX/out7.txt" "RUNG 5 FAILED")"
verdict "names the bound in seconds" "$(found "$SANDBOX/out7.txt" "within its 3s bound")"
verdict "installs nothing" "$(missing "$CMD_LOG" "flutter install")"
verdict "shows the emulator's own log" "$(found "$SANDBOX/out7.txt" "emulator stub: pretending to boot")"

# --- case 8: the app never runs --------------------------------------------------
echo "case 8: an installed app that never shows a process fails on its own bound"
reset_case
rm -f "$STATE/app_running"
run_ladder "$SANDBOX/out8.txt"

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names rung 5" "$(found "$SANDBOX/out8.txt" "RUNG 5 FAILED")"
verdict "got as far as installing" "$(found "$CMD_LOG" "flutter install")"
verdict "names the app bound" "$(found "$SANDBOX/out8.txt" "never showed a process")"

# --- case 9: a Flutter the fixture was not generated against ----------------------
echo "case 9: a Flutter version drift is loud, and still lets rung 4 judge"
reset_case
printf 'Flutter 9.9.9 - channel stable\n' >"$STATE/version"
run_ladder "$SANDBOX/out9.txt"

verdict "exits 0" "$(yes_no "$STATUS")"
verdict "shouts about the version" "$(found "$SANDBOX/out9.txt" "the node's flutter is NOT 3.41.2")"
verdict "still builds" "$(found "$CMD_LOG" "flutter build apk --debug")"

# --- case 10: the device answers adb and then stops ------------------------------
echo "case 10: an adb that hangs mid-boot still fails rung 5 on its wall-clock bound"
reset_case
rm -f "$STATE/booted"
: >"$STATE/adb_hangs"
run_ladder "$SANDBOX/out10.txt"

verdict "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
verdict "names rung 5" "$(found "$SANDBOX/out10.txt" "RUNG 5 FAILED")"
verdict "still names the bound in seconds" "$(found "$SANDBOX/out10.txt" "within its 3s bound")"
verdict "did not park inside adb shell" "$(missing "$SANDBOX/out10.txt" "sys.boot_completed=1")"

echo
if [ "$BAD" -eq 0 ]; then
	echo "android-proof.test.sh: all cases pass"
else
	echo "android-proof.test.sh: $BAD check(s) FAILED"
	exit 1
fi
