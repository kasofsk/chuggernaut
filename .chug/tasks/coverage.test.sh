#!/bin/sh
# Shell test for coverage.sh — no network, no cargo, no NATS, no coverage run.
#
# It puts a stub `cargo`, `curl`, `rustup`, `dpkg`, `install` and `nats-server`
# on a controlled PATH and drives coverage.sh against them, asserting the four
# properties that make its output trustworthy and that no test-free edit would
# otherwise catch:
#
#   1. The human summary is the LAST thing printed. A worker node keeps only the
#      final 700 KiB of a task's logs (LOGS_CAP, crates/worker/src/daemon.rs), so
#      an ordering regression would silently drop the whole deliverable behind an
#      instrumented build's compile noise.
#   2. The cargo-llvm-cov download is PINNED and arch-selected, and an
#      architecture with no pinned build fails before anything is compiled.
#   3. A run that cannot execute tier-2 says so as a LOWER BOUND, so nobody reads
#      a partial number as a total.
#   4. A failing instrumented test run still produces the report — and still
#      exits non-zero.
#
# Run:  .chug/tasks/coverage.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/coverage.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
RUN="$WORK/run"
mkdir -p "$BIN" "$RUN"
CARGO_LOG="$WORK/cargo.log"
CURL_LOG="$WORK/curl.log"

FAILURES=0
check() { # check <description> <condition-description> — reads the last verdict
	if [ "$2" = "ok" ]; then
		echo "  ok   $1"
	else
		echo "  FAIL $1"
		FAILURES=$((FAILURES + 1))
	fi
}
saw() { # saw <file> <pattern> -> "ok" | "no"; -e so a pattern may start with a dash
	if grep -qF -e "$2" "$1" 2>/dev/null; then echo ok; else echo no; fi
}
absent() {
	if grep -qF -e "$2" "$1" 2>/dev/null; then echo no; else echo ok; fi
}

# --- stubs -------------------------------------------------------------------
# `cargo` logs its argv and exits 0, except the instrumented run (--no-report),
# whose exit code each case chooses. `curl` logs the URL it was asked for and
# writes a tarball that really does contain a `cargo-llvm-cov` file, so the
# extract-and-install path executes for real. `install` diverts the binary into
# the stub PATH instead of /usr/local/bin.
make_stubs() { # make_stubs <arch-dpkg-reports> <exit-code-of-the-instrumented-run>
	rm -f "$BIN"/* "$CARGO_LOG" "$CURL_LOG"
	: >"$CARGO_LOG"
	: >"$CURL_LOG"

	cat >"$BIN/cargo" <<EOF
#!/bin/sh
echo "cargo \$*" >> "$CARGO_LOG"
case "\$*" in
*--no-report*) exit $2 ;;
esac
exit 0
EOF

	cat >"$BIN/curl" <<EOF
#!/bin/sh
echo "curl \$*" >> "$CURL_LOG"
out=""
while [ \$# -gt 0 ]; do
  case "\$1" in -o) out="\$2"; shift ;; esac
  shift
done
[ -n "\$out" ] || exit 0
d="\$(mktemp -d)"
echo stub > "\$d/cargo-llvm-cov"
tar -czf "\$out" -C "\$d" cargo-llvm-cov
rm -rf "\$d"
EOF

	cat >"$BIN/dpkg" <<EOF
#!/bin/sh
echo "$1"
EOF

	cat >"$BIN/install" <<EOF
#!/bin/sh
# install -m MODE SRC DST — keep SRC, ignore the real destination.
eval "src=\\\${\$((\$# - 1))}"
cp "\$src" "$BIN/cargo-llvm-cov.installed"
EOF

	printf '#!/bin/sh\nexit 0\n' >"$BIN/rustup"
	chmod +x "$BIN"/*
}

add_nats_stub() {
	cat >"$BIN/nats-server" <<'EOF'
#!/bin/sh
echo "Server is ready"
sleep 30
EOF
	chmod +x "$BIN/nats-server"
}

run_sut() { # run_sut <stdout-file>; never inherits the host toolchain
	set +e
	(
		cd "$RUN" || exit 1
		PATH="$BIN:/usr/bin:/bin" RUSTC_WRAPPER="" CHUG_TEST_NATS_URL="" \
			sh "$SUT" >"$1" 2>&1
	)
	STATUS=$?
	set -e
}

# --- case 1: the happy path --------------------------------------------------
echo "case 1: a complete run pins its download, enables tier-2 and ends with the summary"
make_stubs amd64 0
add_nats_stub
run_sut "$WORK/out1.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "fetches the pinned version" "$(saw "$CURL_LOG" "cargo-llvm-cov/releases/download/v0.8.7/")"
check "selects the amd64 musl build" "$(saw "$CURL_LOG" "cargo-llvm-cov-x86_64-unknown-linux-musl.tar.gz")"
check "installs the extracted binary" "$([ -f "$BIN/cargo-llvm-cov.installed" ] && echo ok || echo no)"
check "announces tier-2 as enabled" "$(saw "$WORK/out1.txt" "tier-2 (NATS) ENABLED")"
check "runs the workspace instrumented" "$(saw "$CARGO_LOG" "llvm-cov --workspace --all-features --no-report --no-fail-fast")"
check "writes the lcov report" "$(saw "$CARGO_LOG" "llvm-cov report --lcov --output-path coverage.lcov")"
check "writes the html report" "$(saw "$CARGO_LOG" "llvm-cov report --html --output-dir coverage-html")"
check "says the files are discarded" "$(saw "$WORK/out1.txt" "DISCARDED when the container is removed")"
check "states the scope as a lower bound" "$(saw "$WORK/out1.txt" "LOWER BOUND")"
LAST_CARGO="$(tail -n 1 "$CARGO_LOG")"
check "the summary is the LAST cargo call" \
	"$([ "$LAST_CARGO" = "cargo llvm-cov report" ] && echo ok || echo no)"

# --- case 2: an architecture with no pinned build ----------------------------
echo "case 2: an unsupported architecture fails before anything is built"
make_stubs riscv64 0
add_nats_stub
run_sut "$WORK/out2.txt"

check "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
check "names the architecture" "$(saw "$WORK/out2.txt" "riscv64")"
check "never invokes cargo" "$([ ! -s "$CARGO_LOG" ] && echo ok || echo no)"

# --- case 3: no NATS available ------------------------------------------------
echo "case 3: without a nats-server the tier-2 gap is stated, not hidden"
make_stubs amd64 0
run_sut "$WORK/out3.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "warns that tier-2 self-skips" "$(saw "$WORK/out3.txt" "tier-2 test SELF-SKIPS")"
check "never claims tier-2 ran" "$(absent "$WORK/out3.txt" "tier-2 (NATS) ENABLED")"
check "still prints the summary" "$(saw "$CARGO_LOG" "llvm-cov report")"

# --- case 4: the instrumented run fails ---------------------------------------
echo "case 4: a failing test run still reports, and still fails the task"
make_stubs amd64 101
add_nats_stub
run_sut "$WORK/out4.txt"

check "propagates the test exit code" "$([ "$STATUS" -eq 101 ] && echo ok || echo no)"
check "still writes the lcov report" "$(saw "$CARGO_LOG" "--output-path coverage.lcov")"
check "shouts that the number is from a failing run" "$(saw "$WORK/out4.txt" "instrumented test run FAILED")"

# --- case 5: the tool is already present --------------------------------------
echo "case 5: an image that already carries cargo-llvm-cov downloads nothing"
make_stubs amd64 0
add_nats_stub
printf '#!/bin/sh\necho "cargo-llvm-cov 0.0.0-stub"\n' >"$BIN/cargo-llvm-cov"
chmod +x "$BIN/cargo-llvm-cov"
run_sut "$WORK/out5.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "downloads nothing" "$([ ! -s "$CURL_LOG" ] && echo ok || echo no)"
check "says it found one already" "$(saw "$WORK/out5.txt" "already on PATH")"

echo
if [ "$FAILURES" -eq 0 ]; then
	echo "coverage.test.sh: all cases pass"
else
	echo "coverage.test.sh: $FAILURES check(s) FAILED"
	exit 1
fi
