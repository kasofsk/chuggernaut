#!/bin/sh
# The Android proof ladder — design #367 phase A2. Run as the `work` step of an
# `android-proof` job (.chug/jobs/android-proof.yaml), pinned to the one node
# that has /dev/kvm. It proves the whole Android path end to end instead of
# arguing it: the device and the toolchain mounts, KVM itself, the two
# toolchains, a real Flutter APK build of fixtures/mobile, and an emulator boot
# with a device-backed task against the APK that build produced.
#
# THE DELIVERABLE IS STDOUT. The job carries no commits and writes no output
# archive (#381's bucket exists; a report of five verdicts does not need a file).
# The dispatcher harvests a command work task's container logs into its
# `stdout.log` artifact (crates/platform-ops/src/harvest.rs, spec §3.2) and a
# worker node keeps only the LAST 700 KiB of them (LOGS_CAP,
# crates/worker/src/daemon.rs) — so a cold Gradle build's noise is what falls off
# the front, and the LADDER summary is printed LAST, from the EXIT trap, on every
# path including a failure.
#
# EACH RUNG PROVES SOMETHING DIFFERENT, and a failure names which one broke:
#
#   1. mounts and env      — placement/allow-list, not toolchain: an un-granted
#                            project gets no device and no injected env at all
#   2. KVM acceleration    — the device is present AND usable
#   3. toolchains answer   — flutter and the SDK's own tools run off read-only mounts
#   4. flutter build apk   — the Flutter half, unreachable before job #393
#   5. emulator + a device-backed task — the Android half
#
# THE ENTRYPOINT IS `flutter`, NEVER A BARE `./gradlew` (#367 correction 13, job
# #392): the stock template gitignores `gradlew`, `gradle-wrapper.jar` and
# `local.properties`, and `android/settings.gradle.kts` hard-requires
# `flutter.sdk` out of that file — all of which the Flutter tool writes on first
# build. Rung 5 is device-backed for the same reason it is not
# `connectedAndroidTest`: the stock skeleton ships no `androidTest` sources and no
# `integration_test` dependency, so gradle would prove only that it can run zero
# tests, while installing rung 4's APK and watching its process come up on the
# emulator exercises adb, the device and the built artifact — with no bespoke
# structure added to a tree whose value is being stock (fixtures/mobile/README.md).
#
# EVERY WAIT IS BOUNDED BY WALL CLOCK (STYLE.md Tier 2 rule 3) and every bound is
# named in the failure it produces, so an emulator that never boots fails its
# rung in minutes instead of hanging until `task_timeout`. That means every
# `adb` call too, including the ones inside the polling loops: a half-booted
# device accepts the connection and then never answers, and an unbounded probe
# would park inside `adb shell` where the loop's own bound can never fire. The
# bounds, the device path and the
# fixture directory are env-overridable for exactly one reason — so
# .chug/tasks/android-proof.test.sh can drive the whole ladder against stubs in a
# sandbox, in seconds, inside CI's per-suite cap.
set -eu

KVM_DEVICE="${CHUG_KVM_DEVICE:-/dev/kvm}"
FIXTURE_DIR="${CHUG_ANDROID_FIXTURE_DIR:-$(cd "$(dirname "$0")/../.." && pwd)/fixtures/mobile}"
SYSTEM_IMAGE="${CHUG_ANDROID_SYSTEM_IMAGE:-system-images;android-34;google_apis;x86_64}"
FLUTTER_VERSION_EXPECTED="${CHUG_ANDROID_FLUTTER_VERSION:-3.41.2}"
APP_PACKAGE="xyz.kasofsk.chug_mobile_proof"
AVD_NAME="chug-proof"
APK_PATH="build/app/outputs/flutter-apk/app-debug.apk"

# The mounts the worker adds for an allow-listed launch
# (crates/container/src/docker.rs). Reported, not required: a node is free to
# mount elsewhere, and what makes rung 1 pass is that the env resolves.
SDK_MOUNT_EXPECTED="/opt/android-sdk"
FLUTTER_MOUNT_EXPECTED="/opt/flutter"

TOOL_TIMEOUT_SECS="${CHUG_ANDROID_TOOL_TIMEOUT_SECS:-300}"
BUILD_TIMEOUT_SECS="${CHUG_ANDROID_BUILD_TIMEOUT_SECS:-3600}"
ADB_TIMEOUT_SECS="${CHUG_ANDROID_ADB_TIMEOUT_SECS:-180}"
BOOT_TIMEOUT_SECS="${CHUG_ANDROID_BOOT_TIMEOUT_SECS:-300}"
APP_TIMEOUT_SECS="${CHUG_ANDROID_APP_TIMEOUT_SECS:-120}"
POLL_INTERVAL_SECS="${CHUG_ANDROID_POLL_INTERVAL_SECS:-2}"
# A single probe's own bound, well under the wait it runs inside: generous enough
# that a busy-but-healthy device answers, short enough that a hung one retries.
PROBE_TIMEOUT_SECS="${CHUG_ANDROID_PROBE_TIMEOUT_SECS:-15}"

LADDER=""
RUNGS_CLEARED=0
WAITED_SECS=0
VERDICT="FAIL — the ladder did not finish"
EMULATOR_PID=""
ADB_SERVER_STARTED=""
WORK_DIR=""
EMULATOR_LOG=""

rung_title() {
	case "$1" in
	1) echo "the mounts and env are present" ;;
	2) echo "acceleration is real" ;;
	3) echo "the toolchains answer" ;;
	4) echo "a Flutter APK build" ;;
	5) echo "an emulator boots and a device-backed task runs" ;;
	esac
}

rung_pass() {
	RUNGS_CLEARED="$1"
	LADDER="$LADDER
  rung $1 $(rung_title "$1"): PASS — $2"
	echo "android-proof: rung $1 PASS — $2"
}

# Ends the run. The ladder itself is printed by the EXIT trap, so a failure and a
# success leave stdout in the same readable shape; the rung that failed counts as
# reached, so the summary never reports it twice.
rung_fail() {
	RUNGS_CLEARED="$1"
	LADDER="$LADDER
  rung $1 $(rung_title "$1"): FAIL — $2"
	VERDICT="FAIL at rung $1 ($(rung_title "$1"))"
	echo "!!! android-proof: RUNG $1 FAILED — $2" >&2
	emulator_log_tail
	exit 1
}

emulator_log_tail() {
	if [ -n "$EMULATOR_LOG" ] && [ -s "$EMULATOR_LOG" ]; then
		echo "android-proof: last 40 lines of the emulator's own log:"
		tail -n 40 "$EMULATOR_LOG"
	fi
}

print_ladder() {
	echo
	echo "android-proof: LADDER (design #367 A2) — device=$KVM_DEVICE fixture=$FIXTURE_DIR"
	printf '%s\n' "$LADDER" | sed '/^[[:space:]]*$/d'
	_n=$((RUNGS_CLEARED + 1))
	while [ "$_n" -le 5 ]; do
		echo "  rung $_n $(rung_title "$_n"): NOT REACHED"
		_n=$((_n + 1))
	done
	echo "android-proof: VERDICT $VERDICT"
}

# POSIX sh allows exactly ONE EXIT trap (the #186 web-publish lesson), so the
# emulator teardown and the summary share this one function.
cleanup() {
	[ -n "$EMULATOR_PID" ] && kill "$EMULATOR_PID" 2>/dev/null
	[ -n "$ADB_SERVER_STARTED" ] && adb kill-server >/dev/null 2>&1
	print_ladder
	[ -n "$WORK_DIR" ] && rm -rf "$WORK_DIR"
	return 0
}
trap cleanup EXIT INT TERM

# Reads `timeout`'s own 124 apart from the command's status, so a bound that is
# hit is reported as a bound rather than as a mystery exit code.
bounded() { # bounded <rung> <secs> <what> <cmd...>
	_rung="$1"
	_secs="$2"
	_what="$3"
	shift 3
	_status=0
	timeout "$_secs" "$@" || _status=$?
	if [ "$_status" -eq 124 ]; then
		rung_fail "$_rung" "$_what hit its ${_secs}s bound"
	fi
	if [ "$_status" -ne 0 ]; then
		rung_fail "$_rung" "$_what exited $_status"
	fi
}

# Every `adb` read goes through here, because a device that stops answering
# mid-boot hangs `adb shell` forever. A call that hits its bound reads as empty
# output, which is what lets the polling probes below stay one-liners.
adb_bounded() { # adb_bounded <secs> <adb-args...>
	_adb_secs="$1"
	shift
	timeout "$_adb_secs" adb "$@" 2>/dev/null || true
}

# Both of rung 5's waits run through here, so the bound is WALL CLOCK rather than
# a count of sleeps and the failure names it. WAITED_SECS is left for the caller.
wait_bounded() { # wait_bounded <secs> <what> <probe-fn>
	_wait_started="$(date +%s)"
	WAITED_SECS=0
	while ! "$3"; do
		WAITED_SECS=$(($(date +%s) - _wait_started))
		[ "$WAITED_SECS" -lt "$1" ] ||
			rung_fail 5 "$2 within its ${1}s bound"
		sleep "$POLL_INTERVAL_SECS"
	done
	WAITED_SECS=$(($(date +%s) - _wait_started))
}

rung_1_mounts_and_env() {
	[ -e "$KVM_DEVICE" ] ||
		rung_fail 1 "$KVM_DEVICE is absent — this container was granted no device. Check that the job ran on the pinned node and that JOB_PROJECT is in WORKER_KVM_PROJECTS (docs/runbooks/worker-kvm.md)"
	[ -r "$KVM_DEVICE" ] && [ -w "$KVM_DEVICE" ] ||
		rung_fail 1 "$KVM_DEVICE is present but not readable and writable by this container"
	ls -l "$KVM_DEVICE"
	[ -n "${ANDROID_SDK_ROOT:-}" ] && [ -d "$ANDROID_SDK_ROOT" ] ||
		rung_fail 1 "ANDROID_SDK_ROOT ('${ANDROID_SDK_ROOT:-}') does not name a directory — the worker injects it only for an allow-listed launch (inject_toolchain_env, crates/worker/src/daemon.rs)"
	[ -n "${FLUTTER_ROOT:-}" ] && [ -d "$FLUTTER_ROOT" ] ||
		rung_fail 1 "FLUTTER_ROOT ('${FLUTTER_ROOT:-}') does not name a directory — the node sets WORKER_FLUTTER_DIR or it gets no Flutter mount at all"
	[ "${ANDROID_HOME:-}" = "$ANDROID_SDK_ROOT" ] ||
		rung_fail 1 "ANDROID_HOME ('${ANDROID_HOME:-}') and ANDROID_SDK_ROOT ('$ANDROID_SDK_ROOT') disagree"
	[ -n "${ANDROID_USER_HOME:-}" ] ||
		rung_fail 1 "ANDROID_USER_HOME is unset, so the AVD rung 5 creates would land in the read-only SDK"
	[ -n "${HOME:-}" ] && [ -w "$HOME" ] ||
		rung_fail 1 "HOME ('${HOME:-}') is not writable — the emulator writes \$HOME/.android even with ANDROID_USER_HOME set"
	[ -f "$FIXTURE_DIR/pubspec.yaml" ] ||
		rung_fail 1 "$FIXTURE_DIR holds no pubspec.yaml — rung 4 has nothing to build"
	rung_1_report_mount ANDROID_SDK_ROOT "$ANDROID_SDK_ROOT" "$SDK_MOUNT_EXPECTED"
	rung_1_report_mount FLUTTER_ROOT "$FLUTTER_ROOT" "$FLUTTER_MOUNT_EXPECTED"
	echo "android-proof: HOME=$HOME ANDROID_USER_HOME=$ANDROID_USER_HOME"
	rung_pass 1 "the device is read-write and both toolchain mounts resolve"
}

# Reported, not required (see SDK_MOUNT_EXPECTED). A drift between the injected
# env and the backend's mount constant is still worth shouting about.
rung_1_report_mount() { # rung_1_report_mount <name> <actual> <expected>
	if [ "$2" = "$3" ]; then
		echo "android-proof: $1=$2, which is the worker's mount constant"
	else
		echo "!!! android-proof: $1=$2 is NOT the worker's mount constant $3 — this node"
		echo "!!!     mounts the toolchain elsewhere, or crates/container/src/docker.rs moved."
	fi
}

# The SDK's tools are not on the image's PATH — nothing about the image is
# Android-aware (#367 correction 11), so the mounts are put on PATH here.
prepare_environment() {
	PATH="$FLUTTER_ROOT/bin:$ANDROID_SDK_ROOT/emulator:$ANDROID_SDK_ROOT/platform-tools:$ANDROID_SDK_ROOT/cmdline-tools/latest/bin:$ANDROID_SDK_ROOT/tools/bin:$PATH"
	export PATH
	ANDROID_AVD_HOME="${ANDROID_AVD_HOME:-$ANDROID_USER_HOME/avd}"
	GRADLE_USER_HOME="${GRADLE_USER_HOME:-$HOME/.gradle}"
	PUB_CACHE="${PUB_CACHE:-$HOME/.pub-cache}"
	export ANDROID_AVD_HOME GRADLE_USER_HOME PUB_CACHE
	mkdir -p "$ANDROID_USER_HOME" "$ANDROID_AVD_HOME" "$GRADLE_USER_HOME" "$PUB_CACHE"
	WORK_DIR="$(mktemp -d)"
	EMULATOR_LOG="$WORK_DIR/emulator.log"
}

rung_2_acceleration() {
	command -v emulator >/dev/null 2>&1 ||
		rung_fail 2 "no \`emulator\` on PATH under $ANDROID_SDK_ROOT"
	bounded 2 "$TOOL_TIMEOUT_SECS" "emulator -accel-check" emulator -accel-check
	rung_pass 2 "emulator -accel-check accepts this container's $KVM_DEVICE"
}

rung_3_toolchains() {
	for _tool in flutter adb avdmanager; do
		command -v "$_tool" >/dev/null 2>&1 ||
			rung_fail 3 "no \`$_tool\` on PATH — looked under $FLUTTER_ROOT/bin and $ANDROID_SDK_ROOT"
		echo "android-proof: $_tool -> $(command -v "$_tool")"
	done
	rung_3_flutter_version
	bounded 3 "$TOOL_TIMEOUT_SECS" "avdmanager list target" avdmanager list target
	_image_dir="$ANDROID_SDK_ROOT/$(printf '%s' "$SYSTEM_IMAGE" | tr ';' '/')"
	[ -d "$_image_dir" ] ||
		rung_fail 3 "the SDK holds no '$SYSTEM_IMAGE' ($_image_dir) — the node provisions the images, so either pick another with CHUG_ANDROID_SYSTEM_IMAGE or add it to the node's configuration.nix"
	ls "$ANDROID_SDK_ROOT/platforms" "$ANDROID_SDK_ROOT/build-tools" 2>/dev/null || true
	rung_pass 3 "flutter and the SDK tools run off the read-only mounts, and '$SYSTEM_IMAGE' is installed"
}

# Run once and kept, because the same output answers two questions. A version
# drift is loud but not fatal: the fixture is regenerated against whatever the
# node pins, so rung 4 is the honest judge of whether it still builds.
rung_3_flutter_version() {
	_out="$WORK_DIR/flutter-version.txt"
	_status=0
	timeout "$TOOL_TIMEOUT_SECS" flutter --version >"$_out" 2>&1 || _status=$?
	cat "$_out"
	[ "$_status" -eq 0 ] ||
		rung_fail 3 "flutter --version exited $_status (124 = its ${TOOL_TIMEOUT_SECS}s bound) — a Flutter that needs to write into its own SDK cannot run off the read-only $FLUTTER_ROOT mount"
	if grep -q "$FLUTTER_VERSION_EXPECTED" "$_out"; then
		echo "android-proof: flutter is the expected $FLUTTER_VERSION_EXPECTED"
	else
		echo "!!! android-proof: the node's flutter is NOT $FLUTTER_VERSION_EXPECTED, which is what"
		echo "!!!     fixtures/mobile was generated against — if rung 4 fails, regenerate the"
		echo "!!!     fixture against the node's version rather than patching it."
	fi
}

rung_4_build() {
	cd "$FIXTURE_DIR"
	echo "android-proof: building $FIXTURE_DIR — cold, this fetches the Gradle distribution, the AGP deps and the pub cache"
	bounded 4 "$BUILD_TIMEOUT_SECS" "flutter build apk --debug" flutter build apk --debug
	[ -f "$APK_PATH" ] ||
		rung_fail 4 "flutter build apk --debug exited 0 but $FIXTURE_DIR/$APK_PATH is absent"
	rung_pass 4 "built $APK_PATH ($(wc -c <"$APK_PATH") bytes), so the tool wrote the gitignored wrapper and local.properties too"
}

rung_5_emulator() {
	_status=0
	printf 'no\n' | timeout "$TOOL_TIMEOUT_SECS" \
		avdmanager create avd -n "$AVD_NAME" -k "$SYSTEM_IMAGE" --force || _status=$?
	[ "$_status" -eq 0 ] ||
		rung_fail 5 "avdmanager create avd -n $AVD_NAME -k '$SYSTEM_IMAGE' exited $_status (124 = its ${TOOL_TIMEOUT_SECS}s bound)"
	adb_bounded "$TOOL_TIMEOUT_SECS" start-server >/dev/null
	ADB_SERVER_STARTED=1
	echo "android-proof: booting $AVD_NAME headless, logging to the container's temp dir"
	emulator -avd "$AVD_NAME" -no-window -no-audio -no-boot-anim -no-snapshot \
		-gpu swiftshader_indirect -netdelay none -netspeed full >"$EMULATOR_LOG" 2>&1 &
	EMULATOR_PID=$!
	bounded 5 "$ADB_TIMEOUT_SECS" "adb wait-for-device" adb wait-for-device
	wait_bounded "$BOOT_TIMEOUT_SECS" \
		"the emulator never reported sys.boot_completed" rung_5_probe_booted
	echo "android-proof: sys.boot_completed=1 after ${WAITED_SECS}s"
	_serial="$(adb_bounded "$TOOL_TIMEOUT_SECS" get-serialno)"
	[ -n "$_serial" ] ||
		rung_fail 5 "adb attached a device and then named no serial for it"
	echo "android-proof: device $_serial is up; API $(adb_bounded "$TOOL_TIMEOUT_SECS" shell getprop ro.build.version.sdk | tr -dc '0-9')"
	rung_5_device_backed_task "$_serial"
	emulator_log_tail
	rung_pass 5 "$AVD_NAME booted under KVM and ran $APP_PACKAGE from rung 4's APK"
}

rung_5_probe_booted() {
	kill -0 "$EMULATOR_PID" 2>/dev/null ||
		rung_fail 5 "the emulator process exited before reporting sys.boot_completed"
	[ "$(adb_bounded "$PROBE_TIMEOUT_SECS" shell getprop sys.boot_completed | tr -dc '0-9')" = "1" ]
}

# Install rung 4's APK and watch the app's own process come up. It is the whole
# chain — adb, the device, the artifact — without adding an `integration_test`
# dependency to a fixture whose value is being stock.
rung_5_device_backed_task() { # rung_5_device_backed_task <serial>
	bounded 5 "$TOOL_TIMEOUT_SECS" "flutter install --debug -d $1" flutter install --debug -d "$1"
	adb_bounded "$TOOL_TIMEOUT_SECS" shell pm list packages "$APP_PACKAGE" |
		grep -q "package:$APP_PACKAGE" ||
		rung_fail 5 "$APP_PACKAGE is not installed on $1 after flutter install exited 0"
	bounded 5 "$TOOL_TIMEOUT_SECS" "am start $APP_PACKAGE/.MainActivity" \
		adb shell am start -n "$APP_PACKAGE/.MainActivity"
	wait_bounded "$APP_TIMEOUT_SECS" \
		"$APP_PACKAGE never showed a process on $1" rung_5_probe_app_running
	echo "android-proof: $APP_PACKAGE is running on $1 after ${WAITED_SECS}s"
}

rung_5_probe_app_running() {
	[ -n "$(adb_bounded "$PROBE_TIMEOUT_SECS" shell pidof "$APP_PACKAGE" | tr -dc '0-9')" ]
}

rung_1_mounts_and_env
prepare_environment
rung_2_acceleration
rung_3_toolchains
rung_4_build
rung_5_emulator
VERDICT="PASS — every rung cleared; Android execution works on this node"
