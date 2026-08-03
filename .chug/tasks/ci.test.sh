#!/bin/sh
# Shell test for ci.sh's tier-2 (NATS) probe — no cargo, no npm, no Docker, no
# real NATS. It drives the real ci.sh inside a throwaway repo whose `cargo`,
# `npm`, `docker` and `nats-server` are stubs, and pins the ONE property the
# #375 defect broke:
#
#   THE ANNOUNCEMENT MUST MATCH THE MECHANISM, in both what it claims and how
#   much. `ci: tier-2 (NATS) ENABLED` may be printed only when something that
#   actually makes tier-2 executable was established — a URL exported to the test
#   run, or a usable Docker daemon the harness can start its own containers with;
#   `SKIPPED` only when nothing was. And a URL-only mechanism may not claim the
#   PRIVATE-server files: NatsTestServer::spawn/spawn_with_config never read
#   CHUG_TEST_NATS_URL, so without a daemon those must be named as self-skipping,
#   not counted as executed. Both halves are checked in EVERY case
#   (announcement_matches_mechanism), because the defect was not a wrong string
#   but two independent notions of "ready" drifting apart.
#
# The defect's own configuration — a baked `nats-server`, no Docker daemon, no
# CHUG_TEST_NATS_URL and nothing opted in — is case 1, and since #382 flipped
# CHUG_CI_LOCAL_NATS to opt-OUT it must genuinely RUN the tier.
# Deliberately Docker-free: a Docker-less path proven with a real daemon would
# prove nothing.
#
# Run:  .chug/tasks/ci.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/ci.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
REPO="$WORK/repo"
CARGO_LOG="$WORK/cargo.log"
mkdir -p "$BIN" "$REPO"

FAILURES=0
check() { # check <description> "ok"|"no"
	if [ "$2" = "ok" ]; then
		echo "  ok   $1"
	else
		echo "  FAIL $1"
		FAILURES=$((FAILURES + 1))
	fi
}
saw() {
	if grep -qF -e "$2" "$1" 2>/dev/null; then echo ok; else echo no; fi
}
absent() {
	if grep -qF -e "$2" "$1" 2>/dev/null; then echo no; else echo ok; fi
}

# --- the repo under test ------------------------------------------------------
# Enough shape for ci.sh to run end to end: two tier-2 test files for it to count
# and classify — one SHARED-server (a URL reaches it) and one PRIVATE-server (only
# Docker does) — a `web/` for the web stage, and pass-through stubs for the three
# pure-shell gates it delegates to (they have their own tests).
PRIVATE_FILE="crates/worker/tests/nats_backend.rs"
mkdir -p "$REPO/.chug/tasks" "$REPO/crates/store/tests" "$REPO/crates/worker/tests" "$REPO/web"
printf 'fn t() { require_nats!(); }\n' >"$REPO/crates/store/tests/nats_store.rs"
printf 'fn t() { NatsTestServer::spawn().await; }\n' >"$REPO/$PRIVATE_FILE"
for g in check-modules check-duplication check-comments; do
	printf '#!/bin/sh\nexit 0\n' >"$REPO/.chug/tasks/$g.sh"
	chmod +x "$REPO/.chug/tasks/$g.sh"
done

# --- the shell-suite stage's fixtures (#385) ----------------------------------
# A real git index, because the stage discovers through `git ls-files` — an
# unindexed tree would make every case below prove nothing. The suites are
# throwaway stand-ins: running the repo's real suites from inside one of them is the
# recursion the stage guards against, and case 9 pins the guard instead.
#
# The stand-in ASSERTS ITS OWN ENVIRONMENT: it fails unless the gate handed it
# CHUG_CI_SHELL_SUITES=0, so every case that expects a green stage is also
# checking that the recursion guard reaches the suite.
#
# SUITE_SLOW is named to sort FIRST out of `git ls-files`, so the budget case can
# assert that the suites after it never ran — a bound checked only once
# everything has already run is not a bound.
SUITE_OK=".chug/tasks/sample.test.sh"
SUITE_TWO="deploy/prod/second.test.sh"
SUITE_BAD="deploy/broken.test.sh"
SUITE_SLOW=".chug/tasks/aa-slow.test.sh"
mkdir -p "$REPO/deploy/prod"
cat >"$REPO/$SUITE_OK" <<'EOF'
#!/bin/sh
if [ "${CHUG_CI_SHELL_SUITES:-unset}" != "0" ]; then
	echo "sample.test.sh: the gate did not hand down the recursion guard"
	exit 1
fi
echo "sample.test.sh: all cases pass"
EOF
git -C "$REPO" init -q
git -C "$REPO" add -A

# Track (or untrack) a fixture suite: the stage sees only what the index does.
suite_add() { # suite_add <repo-relative-path> <body>
	printf '%s' "$2" >"$REPO/$1"
	git -C "$REPO" add -- "$1"
}
suite_drop() { # suite_drop <repo-relative-path>
	rm -f "$REPO/$1"
	git -C "$REPO" rm -q --cached -- "$1" 2>/dev/null || true
}

# --- stubs --------------------------------------------------------------------
# `cargo` records the CHUG_TEST_NATS_URL it was handed — that env var IS the
# mechanism, so the log is the evidence the announcement is checked against — and
# replays a two-binary test output so the tier tally has something to classify.
write_cargo_stub() {
	cat >"$BIN/cargo" <<EOF
#!/bin/sh
echo "cargo \$* [CHUG_TEST_NATS_URL=\${CHUG_TEST_NATS_URL:-}] [RUST_MIN_STACK=\${RUST_MIN_STACK:-}]" >> "$CARGO_LOG"
case "\$1" in
test)
  echo "     Running tests/nats_store.rs (target/debug/deps/nats_store-1)"
  echo "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured"
  echo "     Running unittests src/lib.rs (target/debug/deps/store-2)"
  echo "test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured"
  ;;
esac
exit 0
EOF
	printf '#!/bin/sh\nexit 0\n' >"$BIN/npm"
}

# `docker`: "ok" answers every call the communal-container path makes (both the
# sibling-bridge and the host-port addressing branches, so the test does not
# care whether it runs inside a container); "down" is a host where the binary
# exists but no daemon answers.
write_docker_stub() { # write_docker_stub ok|down
	if [ "$1" = "down" ]; then
		printf '#!/bin/sh\nexit 1\n' >"$BIN/docker"
		return 0
	fi
	cat >"$BIN/docker" <<'EOF'
#!/bin/sh
case "$1" in
info) exit 0 ;;
run) echo "deadbeefcafe" ;;
inspect) echo "172.17.0.5" ;;
port) echo "127.0.0.1:34567" ;;
logs) echo "[1] 2026/08/02 00:00:00.000000 [INF] Server is ready" ;;
esac
exit 0
EOF
}

# `nats-server`: "ready" prints the readiness line and stays alive; "dead" is a
# server that cannot start (a busy port), which must NOT be reported as a tier
# that will run; "none" removes the binary entirely.
write_nats_stub() { # write_nats_stub ready|dead|none
	rm -f "$BIN/nats-server"
	case "$1" in
	ready) printf '#!/bin/sh\necho "[1] [INF] Server is ready"\nsleep 30\n' >"$BIN/nats-server" ;;
	dead) printf '#!/bin/sh\necho "nats-server: listen tcp 127.0.0.1:4222: address already in use" >&2\nexit 1\n' >"$BIN/nats-server" ;;
	none) return 0 ;;
	esac
}

setup() { # setup <docker-mode> <nats-mode>
	rm -f "$BIN"/* "$CARGO_LOG"
	: >"$CARGO_LOG"
	write_cargo_stub
	write_docker_stub "$1"
	write_nats_stub "$2"
	chmod +x "$BIN"/*
}

# Per-case overrides for the shell-suite stage. Empty means "leave the variable
# unset", so the default path is ci.sh's own and not a value this test chose;
# they are also cleared out of the INHERITED environment, since the gate runs
# this very file with CHUG_CI_SHELL_SUITES=0.
SUITES_ENABLED=""
SUITE_CAP=""
SUITE_BUDGET=""

# A third argument sets CHUG_CI_LOCAL_NATS; omitting it leaves the variable
# UNSET, which is the case that exercises ci.sh's own default rather than a
# value this test chose.
run_sut() { # run_sut <stdout-file> [CHUG_TEST_NATS_URL] [CHUG_CI_LOCAL_NATS]
	set +e
	(
		cd "$REPO" || exit 1
		export PATH="$BIN:/usr/bin:/bin" RUSTC_WRAPPER="" BASE_BRANCH="" RUST_MIN_STACK=""
		export CHUG_TEST_NATS_URL="${2:-}"
		if [ "$#" -ge 3 ]; then
			export CHUG_CI_LOCAL_NATS="$3"
		else
			unset CHUG_CI_LOCAL_NATS
		fi
		unset CHUG_CI_SHELL_SUITES CHUG_CI_SUITE_TIMEOUT_SECS CHUG_CI_SUITES_BUDGET_SECS
		if [ -n "$SUITES_ENABLED" ]; then
			export CHUG_CI_SHELL_SUITES="$SUITES_ENABLED"
		fi
		if [ -n "$SUITE_CAP" ]; then
			export CHUG_CI_SUITE_TIMEOUT_SECS="$SUITE_CAP"
		fi
		if [ -n "$SUITE_BUDGET" ]; then
			export CHUG_CI_SUITES_BUDGET_SECS="$SUITE_BUDGET"
		fi
		sh "$SUT" >"$1" 2>&1
	)
	STATUS=$?
	set -e
}

# THE INVARIANT, both halves. (a) ENABLED iff a mechanism was actually
# established — `docker ok` counts because the harness starts its own
# testcontainers there even when the communal container fails. (b) the ENABLED
# line claims only what that mechanism reaches: with no daemon the private-server
# file must be named as unreached, and with one it must not be (it runs, so there
# is nothing to except). (b) is what keeps the COUNT from drifting away from the
# mechanism the way the flag once drifted from the probe.
announcement_matches_mechanism() { # <stdout-file> <docker-mode>
	if grep -qF "tier-2 (NATS) ENABLED" "$1" 2>/dev/null; then
		if [ "$2" != "ok" ] && ! grep -qF "CHUG_TEST_NATS_URL=nats://" "$CARGO_LOG"; then
			echo no
		elif [ "$2" != "ok" ] && ! grep -qF "$PRIVATE_FILE" "$1"; then
			echo no
		elif [ "$2" = "ok" ] && grep -qF "$PRIVATE_FILE" "$1"; then
			echo no
		else
			echo ok
		fi
	elif grep -qF "CHUG_TEST_NATS_URL=nats://" "$CARGO_LOG" 2>/dev/null; then
		echo no
	else
		echo ok
	fi
}

# --- case 1: the #375 defect — a baked binary and no Docker -------------------
echo "case 1: nats-server present, no Docker, nothing set — the gate STARTS one and the tier really runs"
setup down ready
run_sut "$WORK/out1.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announcement matches the mechanism" "$(announcement_matches_mechanism "$WORK/out1.txt" down)"
check "announces tier-2 as enabled" "$(saw "$WORK/out1.txt" "tier-2 (NATS) ENABLED")"
check "starts a local nats-server" "$(saw "$WORK/out1.txt" "local nats-server at nats://127.0.0.1:4222")"
check "exports the URL to the test run" "$(saw "$CARGO_LOG" "CHUG_TEST_NATS_URL=nats://127.0.0.1:4222")"
check "raises the test-thread stack" "$(saw "$CARGO_LOG" "RUST_MIN_STACK=16777216")"
check "tallies the tier-2 file as executed" "$(saw "$WORK/out1.txt" "tier-2 (NATS): 3 passed across 1 file(s)")"
check "never claims a skip" "$(absent "$WORK/out1.txt" "tier-2 (NATS) SKIPPED")"
check "claims only the file a URL reaches" "$(saw "$WORK/out1.txt" "1 of 2 integration file(s) execute in full")"
check "names the private-server file as self-skipping" "$(saw "$WORK/out1.txt" "$PRIVATE_FILE")"
check "warns the tally over-counts the self-skips" "$(saw "$WORK/out1.txt" "SELF-SKIPPED (no Docker)")"

# --- case 1b: the same host, opted out -----------------------------------------
# The #375 defect's exact shape, now reachable only on purpose: a `nats-server`
# on PATH that the gate does not use. It must then announce a SKIP and say what
# turned it off — never claim the tier. Also pins the flip itself: case 1 and
# this case differ only in CHUG_CI_LOCAL_NATS, so an accidental revert to
# opt-in makes case 1 look like this one.
echo "case 1b: nats-server present but CHUG_CI_LOCAL_NATS=0 — announces the skip, not the tier"
setup down ready
run_sut "$WORK/out1b.txt" "" 0

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announcement matches the mechanism" "$(announcement_matches_mechanism "$WORK/out1b.txt" down)"
check "never claims the tier ran" "$(absent "$WORK/out1b.txt" "tier-2 (NATS) ENABLED")"
check "announces the skip" "$(saw "$WORK/out1b.txt" "tier-2 (NATS) SKIPPED")"
check "names the opt-out" "$(saw "$WORK/out1b.txt" "CHUG_CI_LOCAL_NATS=0")"
check "exports no URL" "$(absent "$CARGO_LOG" "CHUG_TEST_NATS_URL=nats://")"
check "leaves the stack alone" "$(saw "$CARGO_LOG" "RUST_MIN_STACK=]")"

# --- case 2: the binary is there but the server cannot start ------------------
echo "case 2: a nats-server that dies is NOT a tier that runs"
setup down dead
run_sut "$WORK/out2.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announcement matches the mechanism" "$(announcement_matches_mechanism "$WORK/out2.txt" down)"
check "announces the skip" "$(saw "$WORK/out2.txt" "tier-2 (NATS) SKIPPED")"
check "never claims the tier ran" "$(absent "$WORK/out2.txt" "tier-2 (NATS) ENABLED")"
check "quotes the server's own words" "$(saw "$WORK/out2.txt" "address already in use")"
check "exports no URL" "$(absent "$CARGO_LOG" "CHUG_TEST_NATS_URL=nats://")"
check "reports the skip in the tally" "$(saw "$WORK/out2.txt" "tier-2 (NATS): SKIPPED (0 of 2 file(s) executed)")"

# --- case 3: nothing at all ---------------------------------------------------
echo "case 3: no nats-server and no Docker — the untested files are named"
setup down none
run_sut "$WORK/out3.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announcement matches the mechanism" "$(announcement_matches_mechanism "$WORK/out3.txt" down)"
check "announces the skip" "$(saw "$WORK/out3.txt" "tier-2 (NATS) SKIPPED")"
check "lists the self-skipping file" "$(saw "$WORK/out3.txt" "crates/store/tests/nats_store.rs")"
check "never claims the tier ran" "$(absent "$WORK/out3.txt" "tier-2 (NATS) ENABLED")"

# --- case 4: a Docker daemon ---------------------------------------------------
echo "case 4: with a daemon the communal container still owns the tier"
setup ok ready
run_sut "$WORK/out4.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announcement matches the mechanism" "$(announcement_matches_mechanism "$WORK/out4.txt" ok)"
check "announces tier-2 as enabled" "$(saw "$WORK/out4.txt" "tier-2 (NATS) ENABLED")"
check "uses the communal container" "$(saw "$WORK/out4.txt" "communal gate NATS at nats://")"
check "does not also start a local server" "$(absent "$WORK/out4.txt" "local nats-server at")"
check "claims the whole tier, private-server files included" "$(saw "$WORK/out4.txt" "2 integration file(s) execute against")"
check "excepts nothing from the tally" "$(absent "$WORK/out4.txt" "SELF-SKIPPED (no Docker)")"

# --- case 5: a caller-provided server -----------------------------------------
# The URL is deliberately one nothing is listening on, and the assertion below is
# deliberately ENABLED: the gate cannot probe TCP from POSIX sh, so it trusts the
# caller and SAYS it is trusting them. Read this as pinning the contract, not as a
# reachability guarantee — the harness is what probes, and it prints a loud
# UNREACHABLE fallback of its own (crates/test-utils/src/nats.rs).
echo "case 5: CHUG_TEST_NATS_URL from the caller is used as-is"
setup down none
run_sut "$WORK/out5.txt" "nats://10.0.0.9:4222"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announcement matches the mechanism" "$(announcement_matches_mechanism "$WORK/out5.txt" down)"
check "announces tier-2 as enabled" "$(saw "$WORK/out5.txt" "tier-2 (NATS) ENABLED")"
check "keeps the caller's URL" "$(saw "$CARGO_LOG" "CHUG_TEST_NATS_URL=nats://10.0.0.9:4222")"
check "starts nothing of its own" "$(absent "$WORK/out5.txt" "local nats-server at")"
check "says the URL is trusted, not probed" "$(saw "$WORK/out5.txt" "NOT probed here")"
check "still excepts the private-server file" "$(saw "$WORK/out5.txt" "1 of 2 integration file(s) execute in full")"

# --- case 6: the shell-suite stage runs the tracked suites --------------------
# Green stage, and the stand-in's own guard assertion means a pass here also
# proves the gate handed it CHUG_CI_SHELL_SUITES=0.
echo "case 6: the shell-suite stage runs every tracked *.test.sh"
setup down none
run_sut "$WORK/out6.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announces the discovered count" "$(saw "$WORK/out6.txt" "shell suites — 1 *.test.sh suite(s) from git ls-files")"
check "names the suite it ran" "$(saw "$WORK/out6.txt" "ok   $SUITE_OK")"
check "reports the stage as passed" "$(saw "$WORK/out6.txt" "shell suites — all 1 passed")"
check "states the bounds it enforces" "$(saw "$WORK/out6.txt" "per-suite cap 60s, total budget 120s")"

# --- case 7: a NEW suite is picked up with no registration --------------------
# The whole argument for globbing over an explicit list (#378 added ci.test.sh
# and nothing noticed). Adding the file and tracking it is the entire ceremony.
echo "case 7: a newly added suite is discovered with no list to update"
suite_add "$SUITE_TWO" '#!/bin/sh
echo "second.test.sh: all cases pass"
'
setup down none
run_sut "$WORK/out7.txt"

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "the count went up on its own" "$(saw "$WORK/out7.txt" "shell suites — 2 *.test.sh suite(s)")"
check "runs the new suite" "$(saw "$WORK/out7.txt" "ok   $SUITE_TWO")"

# --- case 8: a failing suite fails the gate ------------------------------------
# The acceptance criterion, and the reason the stage exists at all.
echo "case 8: a failing suite turns the gate red before anything is compiled"
suite_add "$SUITE_BAD" '#!/bin/sh
echo "broken.test.sh: 3 check(s) FAILED"
exit 1
'
setup down none
run_sut "$WORK/out8.txt"

check "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
check "names the failing suite" "$(saw "$WORK/out8.txt" "FAIL $SUITE_BAD")"
check "shows the suite's own output" "$(saw "$WORK/out8.txt" "broken.test.sh: 3 check(s) FAILED")"
check "summarises the failure" "$(saw "$WORK/out8.txt" "shell suite(s) FAILED")"
check "still ran the passing suites" "$(saw "$WORK/out8.txt" "ok   $SUITE_OK")"
check "never reaches cargo" "$([ ! -s "$CARGO_LOG" ] && echo ok || echo no)"
suite_drop "$SUITE_BAD"

# --- case 9: the recursion guard -----------------------------------------------
# ci.test.sh drives a real ci.sh, so the gate must not run the suites again from
# inside one of them. Case 6 proved the guard is HANDED DOWN; this proves what
# receiving it does, and that the skip is announced rather than silent.
echo "case 9: CHUG_CI_SHELL_SUITES=0 skips the stage, out loud"
setup down none
SUITES_ENABLED=0
run_sut "$WORK/out9.txt"
SUITES_ENABLED=""

check "exits 0" "$([ "$STATUS" -eq 0 ] && echo ok || echo no)"
check "announces the skip" "$(saw "$WORK/out9.txt" "shell suites SKIPPED — CHUG_CI_SHELL_SUITES=0")"
check "says why the guard exists" "$(saw "$WORK/out9.txt" "cannot recurse")"
check "runs no suite" "$(absent "$WORK/out9.txt" "ok   $SUITE_OK")"

# --- case 10: the per-suite cap -------------------------------------------------
echo "case 10: a suite that hangs is killed at the per-suite cap and fails the gate"
suite_add "$SUITE_SLOW" '#!/bin/sh
sleep 30
'
setup down none
SUITE_CAP=1
run_sut "$WORK/out10.txt"
SUITE_CAP=""

check "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
check "says it was killed at the cap" "$(saw "$WORK/out10.txt" "killed at the 1s per-suite cap")"
check "names it among the failures" "$(saw "$WORK/out10.txt" "shell suite(s) FAILED")"

# --- case 11: the total budget, checked BETWEEN suites --------------------------
# An unconditional stage's cost is every job's cost, so a glob that quietly grows
# past its measured budget must fail rather than slow every gate in the fleet —
# and it must stop AT the bound, not report it afterwards, or the stage's real
# ceiling is suite-count x per-suite cap. SUITE_SLOW sorts first, so the two
# suites behind it are the evidence: they must be named as unrun, not run.
echo "case 11: passing suites that blow the total budget stop the gate where it broke"
suite_add "$SUITE_SLOW" '#!/bin/sh
sleep 2
'
setup down none
SUITE_BUDGET=1
run_sut "$WORK/out11.txt"
SUITE_BUDGET=""
suite_drop "$SUITE_SLOW"

check "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
check "names the budget and where it broke" "$(saw "$WORK/out11.txt" "crossed the 1s total budget at $SUITE_SLOW")"
check "stops there rather than finishing the loop" "$(absent "$WORK/out11.txt" "ok   $SUITE_OK")"
check "names the suites it therefore never ran" "$(saw "$WORK/out11.txt" "did NOT run and are ungated here")"
check "lists one of them" "$(saw "$WORK/out11.txt" "!!!      $SUITE_TWO")"
check "never claims the stage passed" "$(absent "$WORK/out11.txt" "shell suites — all")"

# --- case 11b: the per-suite cap must be enforceable to be announced ------------
# The announcement/mechanism invariant this file exists for, one size down: the
# header states a per-suite cap, so a host with no working `timeout` must not
# print it and quietly run the suites unbounded. Stubbed as an exit-127 `timeout`
# rather than an absent one, since the stub PATH cannot hide /usr/bin's.
echo "case 11b: a \`timeout\` that cannot run fails the stage instead of dropping the cap"
setup down none
printf '#!/bin/sh\nexit 127\n' >"$BIN/timeout"
chmod +x "$BIN/timeout"
run_sut "$WORK/out11b.txt"
rm -f "$BIN/timeout"

check "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
check "says the cap cannot be applied" "$(saw "$WORK/out11b.txt" "no working \`timeout\` on PATH")"
check "never announces a cap it is not applying" "$(absent "$WORK/out11b.txt" "per-suite cap 60s")"
check "runs no suite unbounded" "$(absent "$WORK/out11b.txt" "ok   $SUITE_OK")"
check "names the loud opt-out" "$(saw "$WORK/out11b.txt" "CHUG_CI_SHELL_SUITES=0")"
check "never reaches cargo" "$([ ! -s "$CARGO_LOG" ] && echo ok || echo no)"

# --- case 12: a glob that matches nothing --------------------------------------
# The failure mode this whole stage exists to prevent: a gate that runs, says
# nothing is wrong, and covered nothing.
echo "case 12: zero discovered suites is a broken gate, not a pass"
suite_drop "$SUITE_OK"
suite_drop "$SUITE_TWO"
setup down none
run_sut "$WORK/out12.txt"

check "exits non-zero" "$([ "$STATUS" -ne 0 ] && echo ok || echo no)"
check "says it would gate nothing" "$(saw "$WORK/out12.txt" "would gate NOTHING")"
check "never reaches cargo" "$([ ! -s "$CARGO_LOG" ] && echo ok || echo no)"

echo
if [ "$FAILURES" -eq 0 ]; then
	echo "ci.test.sh: all cases pass"
else
	echo "ci.test.sh: $FAILURES check(s) FAILED"
	exit 1
fi
