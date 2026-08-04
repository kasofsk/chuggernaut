#!/bin/sh
# Instrumented coverage over the whole Rust workspace. Run as the `work` step of
# a `coverage` job (.chug/jobs/coverage.yaml), on demand: a coverage job carries
# no commits, so HEAD == the main it was released from and the branch is scratch.
#
# TWO THINGS SURVIVE THIS RUN: stdout, and /workspace/chug-output.tar.gz. The
# dispatcher harvests a command work task's container logs into its `stdout.log`
# artifact and that one well-known path into its `output.tar.gz` artifact
# (crates/platform-ops/src/harvest.rs, spec §3.2), both readable from the UI and
# the API. Everything else in this workspace is destroyed with the container when
# harvest calls `dispose`, so anything worth keeping goes in the tarball. Do NOT
# invent a substitute: not a commit of the files to the job branch, not an
# attachment upload. The archive is capped at 16 MiB and an over-cap archive is
# refused whole rather than truncated (docs/design/362-binary-artifacts.md).
#
# Stdout is TAILED, not headed: a worker node caps a `logs` reply at the LAST
# 700 KiB (LOGS_CAP, crates/worker/src/daemon.rs). So the human summary is
# printed LAST and an instrumented build's compile noise is what falls off the
# front.
set -eu
export CARGO_TERM_COLOR=always

# cargo-llvm-cov is deliberately NOT in chuggernaut/agent-rust:prod. The
# platform's images rebuild on every node on every deploy
# (deploy/prod/build-worker.sh, worker-refresh.sh) and agent-rust's leg already
# ran 673s on one node before #352 — docs/design/367-android-emulator-execution.md
# §3.1 is the argument, and occasional-use tooling paid for on every deploy on
# every node is the shape it rejects. A pinned prebuilt costs seconds per run and
# nothing per deploy.
#
# PINNED EXACTLY, never floating: an unpinned fetch makes a coverage number
# depend on the day it was taken, and cargo-llvm-cov's own reported percentages
# move between releases. Bump this line deliberately.
LLVM_COV_VERSION="0.8.7"

# POSIX sh allows exactly ONE EXIT trap (the #186 web-publish lesson), so
# everything this script stages is torn down through this one function.
NATS_PID=""
NATS_DIR=""
cleanup() {
	[ -n "$NATS_PID" ] && kill "$NATS_PID" 2>/dev/null
	[ -n "$NATS_DIR" ] && rm -rf "$NATS_DIR"
	return 0
}
trap cleanup EXIT INT TERM

# `cargo llvm-cov` shells out to llvm-profdata/llvm-cov, which ride the
# `llvm-tools` rustup component and must match the active toolchain — hence the
# component add rather than a second pinned download.
install_llvm_cov() {
	if command -v cargo-llvm-cov >/dev/null 2>&1; then
		echo "coverage: cargo-llvm-cov already on PATH: $(cargo-llvm-cov --version)"
	else
		# Arch-selected from dpkg, never hardcoded: the fleet is mixed
		# (gumbo-nuc-0 is amd64, dev-air's colima is arm64) and a foreign-arch
		# binary would run under qemu instead of failing. musl builds, so the
		# binary carries no glibc expectation of its own.
		arch="$(dpkg --print-architecture 2>/dev/null || true)"
		case "$arch" in
		amd64) target="x86_64-unknown-linux-musl" ;;
		arm64) target="aarch64-unknown-linux-musl" ;;
		*)
			echo "coverage: no pinned cargo-llvm-cov build for architecture '${arch:-unknown}'" >&2
			exit 1
			;;
		esac
		echo "coverage: installing pinned cargo-llvm-cov v$LLVM_COV_VERSION ($target)"
		tmp="$(mktemp -d)"
		# Downloaded to a file and then extracted, never `curl | tar`: a POSIX
		# pipeline reports only the LAST command's status, so a truncated
		# download would reach `tar` and only maybe fail (#186 again).
		curl -fsSL --retry 3 -o "$tmp/cargo-llvm-cov.tar.gz" \
			"https://github.com/taiki-e/cargo-llvm-cov/releases/download/v${LLVM_COV_VERSION}/cargo-llvm-cov-${target}.tar.gz"
		tar -xzf "$tmp/cargo-llvm-cov.tar.gz" -C "$tmp"
		install -m 0755 "$tmp/cargo-llvm-cov" /usr/local/bin/cargo-llvm-cov
		rm -rf "$tmp"
		cargo llvm-cov --version
	fi
	rustup component add llvm-tools-preview
}

# Tier-2 (NATS) tests reach a server two ways (crates/test-utils/src/nats.rs):
# `NatsTestServer::shared` honours CHUG_TEST_NATS_URL and otherwise needs a Docker
# daemon, and `NatsTestServer::spawn` gives one caller a private server — since
# job #408 a local `nats-server -js` process, falling back to a container. The
# agent-rust image bakes a `nats-server` binary, so running one here serves the
# shared route by URL and the private route by PATH — no Docker socket required.
start_nats() {
	if [ -n "${CHUG_TEST_NATS_URL:-}" ]; then
		echo "coverage: tier-2 (NATS) uses the provided CHUG_TEST_NATS_URL=$CHUG_TEST_NATS_URL"
		return 0
	fi
	if ! command -v nats-server >/dev/null 2>&1; then
		echo "!!! coverage: no nats-server binary — every tier-2 test SELF-SKIPS and the"
		echo "!!!     percentages below are a LOWER BOUND on what the suite covers."
		return 0
	fi
	NATS_DIR="$(mktemp -d)"
	nats-server -js -sd "$NATS_DIR" -a 127.0.0.1 -p 4222 >"$NATS_DIR/log" 2>&1 &
	NATS_PID=$!
	waited=0
	while [ "$waited" -lt 100 ]; do
		if grep -q "Server is ready" "$NATS_DIR/log" 2>/dev/null; then
			CHUG_TEST_NATS_URL="nats://127.0.0.1:4222"
			export CHUG_TEST_NATS_URL
			echo "coverage: tier-2 (NATS) ENABLED — local nats-server at $CHUG_TEST_NATS_URL"
			return 0
		fi
		waited=$((waited + 1))
		sleep 0.2
	done
	echo "!!! coverage: nats-server never reported ready within 20s — tier-2 SELF-SKIPS"
	echo "!!!     and the percentages below are a LOWER BOUND."
	kill "$NATS_PID" 2>/dev/null || true
	NATS_PID=""
	return 0
}

# A foreign-arch sccache under qemu can hang on start and park cargo forever
# (.chug/tasks/ci.sh carries the incident and its probe). An instrumented
# workspace build is the worst place to discover that, so probe once and drop the
# wrapper if it does not answer.
if [ -n "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null 2>&1 \
	&& ! timeout 15 sccache --start-server >/dev/null 2>&1; then
	echo "!!! coverage: sccache did not answer within 15s — compiling without RUSTC_WRAPPER"
	unset RUSTC_WRAPPER
fi

install_llvm_cov
start_nats

# Say what was measured, so nobody reads a partial number as a total.
echo "coverage: SCOPE — cargo llvm-cov --workspace --all-features, tier 1 + tier 2."
echo "coverage:   Since job #408 the PRIVATE-server suites (NatsTestServer::spawn) serve"
echo "coverage:   themselves from a nats-server on PATH, so when one is there (see above)"
echo "coverage:   they are measured too. What still self-skips is what needs a Docker"
echo "coverage:   BACKEND: 7 of docker_backend.rs's 8 tests, 13 of nats_backend.rs's 20,"
echo "coverage:   and 1 of fleet_e2e.rs's 2, all at their docker_available() guard."
echo "coverage:   Doctests are not instrumented. Every percentage below is therefore a"
echo "coverage:   LOWER BOUND on covered behavior, not a total."

# One instrumented run, three views of the same profile data (`--no-report` then
# `report`), so the lcov file, the HTML tree and the printed summary can never
# disagree with each other. --no-fail-fast for the reason ci.sh gives: a failing
# suite must not hide every suite after it — and here it must not cost the number
# either, so the exit status is stashed and re-raised after the reports.
test_status=0
cargo llvm-cov --workspace --all-features --no-report --no-fail-fast || test_status=$?

cargo llvm-cov report --lcov --output-path coverage.lcov
cargo llvm-cov report --html --output-dir coverage-html
tar czf /workspace/chug-output.tar.gz coverage.lcov coverage-html
echo "coverage: wrote coverage.lcov and coverage-html/ into this task's output.tar.gz"
echo "coverage:   artifact ($(wc -c </workspace/chug-output.tar.gz) bytes), alongside the"
echo "coverage:   summary below. Download it from the task's artifact list."

# LAST, deliberately: see the LOGS_CAP note in the header.
cargo llvm-cov report

if [ "$test_status" -ne 0 ]; then
	echo "!!! coverage: the instrumented test run FAILED (exit $test_status) — the summary"
	echo "!!!     above is measured from a run whose tests did not all pass. Fix them"
	echo "!!!     (that is .chug/tasks/ci.sh's job) before trusting these numbers."
	exit "$test_status"
fi
