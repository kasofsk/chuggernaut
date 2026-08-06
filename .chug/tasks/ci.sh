#!/bin/sh
# Project-wide CI gate (wired in .chug/jobs/_defaults.yaml). It IS this repo's
# CI — there is no .github/ workflow here to mirror (CLAUDE.md).
#
# Tier-2 (NATS) integration tests run for real here. test-utils' harness reaches
# a broker exactly two ways and only two (crates/test-utils/src/nats.rs): the
# URL in CHUG_TEST_NATS_URL, or a `nats` image started through testcontainers,
# which needs Docker. It never execs a `nats-server` binary itself. So this gate
# PROVIDES the first: a communal Docker NATS when a daemon is usable, else the
# `nats-server` the agent-rust image bakes, started here and exported as
# CHUG_TEST_NATS_URL (deploy/prod/Dockerfile.agent-rust, docs/reference/testing.md) — on by
# default since #382, CHUG_CI_LOCAL_NATS=0 opts out. When neither happens the
# tier self-skips.
#
# A URL is not the WHOLE tier, though: NatsTestServer::spawn/spawn_with_config
# never read the env var by construction (nats.rs gates that branch on `shared &&
# config.is_none()`), so the private-server files reach a broker through Docker
# or not at all. The announcement below therefore reports what start_gate_nats
# ACHIEVED and which files that mechanism cannot reach, rather than predicting
# either separately — a green gate is never silently partial (Docker-only tier-3
# tests remain out of scope and self-skip regardless).
set -eu
export CARGO_TERM_COLOR=always

# --- sccache liveness guard ---------------------------------------------------
# The agent image may carry an sccache binary for the WRONG arch (observed
# 2026-07-24: x86_64 sccache under qemu on an arm64 worker), and the emulated
# server can deadlock on start — cargo then parks forever waiting on its
# compiler wrapper, wedging the whole CI task with zero CPU. Probe the server
# once with a hard timeout; if it cannot answer, compile WITHOUT the wrapper
# (slower, never wedged). Loud either way.
#
# What "slower" means changed with #352: the agent-rust image no longer bakes a
# warm-target seed, so this no longer degrades to a prebuilt dep graph — it
# degrades to a fully cold compile (measured on air: 275s against 186s warm).
# Still the right trade against a wedged task, but a trip is now expensive
# enough that it must never pass unnoticed, hence the shout below.
if [ -n "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null 2>&1; then
	if timeout 15 sccache --start-server >/dev/null 2>&1 || timeout 5 sccache --show-stats >/dev/null 2>&1; then
		echo "ci: sccache server answering — wrapper enabled"
	else
		echo "!!! ci: sccache server did not answer within 15s (emulated/broken binary?) — disabling RUSTC_WRAPPER"
		echo "!!!     this run compiles COLD end to end; nothing else caches since #352 removed the warm-target seed"
		unset RUSTC_WRAPPER
	fi
fi

# --- sccache stats -----------------------------------------------------------
# Agent/CI containers compile through sccache (WORKER_CACHE_DIR, #55/#122). Each
# container starts a fresh sccache server, so its stats are inherently per-task
# (no zeroing needed). Surface how the cache performed as a compact one-line
# block on EVERY exit path that actually ran cargo — success or failure — via an
# EXIT trap keyed off `cargo_ran`. Early exits that skip the build leave the
# flag at 0 and print nothing.
cargo_ran=0
print_sccache_stats() {
	[ "${cargo_ran:-0}" -eq 1 ] || return 0
	command -v sccache >/dev/null 2>&1 || return 0
	[ -n "${RUSTC_WRAPPER:-}" ] || return 0

	stats="$(sccache --show-stats 2>/dev/null || true)"
	# Boil the table down to the 3-4 numbers that matter. Match the *total*
	# "Cache hits"/"Cache misses" rows (a digit follows the label) — not the
	# per-language "Cache hits (Rust)" rows nor "Cache hits rate". Compute the
	# rate ourselves so the "-" the table prints for zero compiles is a clean
	# "0%". "Cache size" may span two tokens ("123 MiB") or be absent when empty.
	block="$(printf '%s\n' "$stats" | awk '
		/^Cache hits[[:space:]]+[0-9]/   { hits = $NF }
		/^Cache misses[[:space:]]+[0-9]/ { misses = $NF }
		/^Cache size[[:space:]]/ {
			s = ""
			for (i = 3; i <= NF; i++) s = s (s == "" ? "" : " ") $i
			size = s
		}
		END {
			if (hits == "" || misses == "") exit 1
			total = hits + misses
			rate = (total > 0) ? sprintf("%d", (hits * 100) / total) : "0"
			if (size == "") size = "empty"
			printf "sccache: %s hits / %s misses (%s%% hit rate), cache size %s\n", \
				hits, misses, rate, size
			# A run that compiled and hit NOTHING is the shape of a broken
			# cache, not of a busy one — since #352 there is no warm-target
			# seed underneath it, so this is the whole difference between a
			# 186s gate and a 275s one. Shout it in the "!!!" idiom the tier-2
			# partition warning uses, so it survives a skim of the log.
			if (hits + 0 == 0 && misses + 0 > 0)
				printf "!!! ci: sccache hit NOTHING (%s misses) — cold or unusable node cache;\n!!!     expect a fully cold compile (SCCACHE_DIR unwritable / first build on this node / toolchain or dep bump)", misses
		}
	' || true)"

	if [ -n "$block" ]; then
		echo "$block"
	else
		# Parsing failed (unexpected table shape) — dump the raw output so the
		# numbers are never silently lost.
		echo "sccache stats (raw):"
		printf '%s\n' "$stats"
	fi
}
trap print_sccache_stats EXIT

# Tier-2 (NATS) integration test files — those that spin up a NatsTestServer
# (directly or via the require_nats! skip guards). Used for the tier summary
# and the loud partition warning below.
nats_files="$(grep -rlE 'NatsTestServer|require_nats' crates --include='*.rs' 2>/dev/null \
	| grep '/tests/' || true)"
nats_count="$(printf '%s\n' "$nats_files" | grep -c . || true)"

# The subset of those that needs a PRIVATE server. NatsTestServer::spawn and
# spawn_with_config (i.e. require_nats_config) never consult CHUG_TEST_NATS_URL,
# so these reach a broker only through Docker, whatever URL the gate exports —
# .chug/tasks/coverage.sh carries the same caveat for the same reason.
nats_private_files="$(grep -rlE 'NatsTestServer::spawn|require_nats_config' crates --include='*.rs' 2>/dev/null \
	| grep '/tests/' || true)"
nats_private_count="$(printf '%s\n' "$nats_private_files" | grep -c . || true)"

# Whether tier-2 executes, and how much of it, is never PREDICTED here: both are
# outcomes of start_gate_nats(), the single thing that decides, which sets these
# three. The announcement and the mechanism therefore cannot describe different
# worlds — the #375 defect was two independent notions of "ready" (a probe that
# counted a local `nats-server` binary the mechanism could not use) drifting
# apart, and claiming the private-server files for a URL-only mechanism is the
# same defect one size down.
nats_ready=0
nats_private_ok=0
nats_mechanism=none

announce_tier2() {
	if [ "$nats_ready" -eq 1 ] && [ "$nats_private_ok" -eq 1 ]; then
		echo "ci: tier-2 (NATS) ENABLED via $nats_mechanism — $nats_count integration file(s) execute against a real nats-server"
	elif [ "$nats_ready" -eq 1 ]; then
		echo "ci: tier-2 (NATS) ENABLED via $nats_mechanism — $((nats_count - nats_private_count)) of $nats_count integration file(s) execute in full;"
		echo "    a URL is not a Docker daemon, so the PRIVATE-server tests (NatsTestServer::spawn /"
		echo "    require_nats_config) in $nats_private_count file(s) self-skip — and cargo counts a self-skipped"
		echo "    test as passed, so the tally below over-reports them:"
		printf '      %s\n' $nats_private_files
	else
		echo "ci: tier-2 (NATS) SKIPPED — no CHUG_TEST_NATS_URL, no Docker daemon and no usable nats-server;"
		echo "    $nats_count integration file(s) self-skip (NOT executed):"
		printf '      %s\n' $nats_files
		# GOAL/2 partition: a diff that ADDS or edits tier-2 tests we did not
		# run needs a manual verification note. Flag it loudly.
		if [ -n "${changed:-}" ]; then
			IFS='
'
			added=""
			for f in $changed; do
				case "$f" in
				crates/*/tests/*.rs)
					if grep -qE 'NatsTestServer|require_nats' "$f" 2>/dev/null; then
						added="$added $f"
					fi
					;;
				esac
			done
			unset IFS
			if [ -n "$added" ]; then
				echo "!!! ci: this diff touches tier-2 (NATS) test file(s) that DID NOT run here:"
				printf '!!!      %s\n' $added
				echo "!!! A manual verification note is REQUIRED in the work summary"
				echo "!!! (run tier-2 with a nats-server before merging)."
			fi
		fi
	fi
}

# One communal NATS server for the whole gate (#206). Every test binary's
# namespaced `shared()` connects here via CHUG_TEST_NATS_URL instead of each
# spinning its own Docker container — per-binary brokers (plus lingering
# reaper lag, plus the tier-3 real-container suites) saturated a laptop
# Docker daemon and flaked the gate with setup timeouts. Per-test namespaces
# make the sharing safe; the private-server suites (prod-named or config-mode)
# deliberately ignore the env and keep their own containers.
#
# Docker first, then the baked binary: where a daemon exists the communal
# container is the measured, sibling-aware path, and even a container that fails
# to start leaves the harness able to run per-binary testcontainers — so that
# host is tier-2-ready either way. A Docker-less host is ready only if the local
# `nats-server` actually comes up, which is why `nats_ready` is set from the
# result and not from `command -v`, and is ready only for the shared-server
# files, which is why `nats_private_ok` is a separate outcome of the same
# function rather than a second guess made at announcement time.
GATE_NATS_NAME=""
GATE_NATS_PID=""
GATE_NATS_DIR=""
GATE_NATS_PORT=4222
start_gate_nats() {
	# Probed once, first, and independently of which mechanism wins: it is what
	# decides whether the private-server files can run, and a caller-provided URL
	# neither grants nor withholds it.
	if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
		nats_private_ok=1
	fi
	if [ -n "${CHUG_TEST_NATS_URL:-}" ]; then
		nats_ready=1
		nats_mechanism="the caller-provided CHUG_TEST_NATS_URL"
		echo "ci: tier-2 uses the caller-provided CHUG_TEST_NATS_URL=$CHUG_TEST_NATS_URL"
		echo "    — trusted, NOT probed here (no TCP in POSIX sh). If it is wrong the harness"
		echo "    prints a loud UNREACHABLE line per binary and falls back to a container."
		return 0
	fi
	if [ "$nats_private_ok" -eq 1 ]; then
		nats_ready=1
		nats_mechanism="a communal Docker NATS"
		start_gate_nats_docker
		return 0
	fi
	start_gate_nats_local
}

# The Docker-less path (#378), for the host class the agent-rust CI container
# actually is: the baked `nats-server` and no docker socket. Measured by #375 —
# `nats-server -js` plus CHUG_TEST_NATS_URL ran `cargo test -p store --test
# nats_store` for real, 9 tests, no Docker. Same mechanism as
# .chug/tasks/coverage.sh's start_nats, which is why both scripts agree.
#
# ON by default since #382, which fixed the four born-red dispatcher tests
# (tests/inputs.rs, tests/groups.rs, tests/origin.rs) #378 measured against it —
# so a job container with no docker socket, which is every evaluator, now runs
# the tier instead of announcing that it could have. CHUG_CI_LOCAL_NATS=0 opts
# back out. It buys the SHARED-server files only — the private-server ones stay
# dark without a daemon, which is what announce_tier2 subtracts.
start_gate_nats_local() {
	command -v nats-server >/dev/null 2>&1 || return 0
	if [ "${CHUG_CI_LOCAL_NATS:-1}" != "1" ]; then
		echo "ci: a local nats-server IS available but CHUG_CI_LOCAL_NATS=${CHUG_CI_LOCAL_NATS:-1}"
		echo "    opts out of it — tier-2 will self-skip (see start_gate_nats_local)."
		return 0
	fi
	GATE_NATS_DIR="$(mktemp -d)"
	nats-server -js -sd "$GATE_NATS_DIR" -a 127.0.0.1 -p "$GATE_NATS_PORT" \
		>"$GATE_NATS_DIR/log" 2>&1 &
	GATE_NATS_PID=$!
	i=0
	while [ "$i" -lt 100 ]; do
		if grep -q "Server is ready" "$GATE_NATS_DIR/log" 2>/dev/null; then
			CHUG_TEST_NATS_URL="nats://127.0.0.1:$GATE_NATS_PORT"
			export CHUG_TEST_NATS_URL
			nats_ready=1
			nats_mechanism="a local nats-server"
			echo "ci: gate NATS is a local nats-server at $CHUG_TEST_NATS_URL (pid $GATE_NATS_PID, no Docker)"
			return 0
		fi
		# A server that died (port 4222 already busy, unwritable store dir)
		# will never print the line — stop waiting the full 20s for it, and
		# show its own words rather than a generic timeout.
		kill -0 "$GATE_NATS_PID" 2>/dev/null || break
		i=$((i + 1))
		sleep 0.2
	done
	echo "!!! ci: the local nats-server did not come up — tier-2 will self-skip. It said:"
	sed -n '1,20p' "$GATE_NATS_DIR/log" 2>/dev/null | sed 's/^/!!!     /'
	stop_gate_nats
	return 0
}

start_gate_nats_docker() {
	GATE_NATS_NAME="chug-gate-nats-$$"
	docker run -d --rm --name "$GATE_NATS_NAME" -p 127.0.0.1:0:4222 \
		nats:2.10-alpine -js >/dev/null 2>&1 || { GATE_NATS_NAME=""; return 0; }
	# Sibling-aware addressing: when this gate itself runs INSIDE a container
	# (the CI evaluator — /.dockerenv present), the docker socket is the
	# HOST's, so the host-port mapping points at the host's localhost, which
	# a sibling cannot reach. Use the NATS container's bridge IP instead
	# (both siblings sit on the default bridge). On a bare host, the mapped
	# localhost port is the reachable address. The test harness liveness-
	# probes whichever URL we export and falls back loudly if it is wrong.
	if [ -f /.dockerenv ]; then
		addr="$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$GATE_NATS_NAME" 2>/dev/null)"
		port=""
		[ -n "$addr" ] && port="4222" && gate_url="nats://$addr:4222"
	else
		port="$(docker port "$GATE_NATS_NAME" 4222/tcp 2>/dev/null | head -1 | sed 's/.*://')"
		gate_url="nats://127.0.0.1:$port"
	fi
	if [ -z "$port" ]; then
		docker rm -f "$GATE_NATS_NAME" >/dev/null 2>&1
		GATE_NATS_NAME=""
		return 0
	fi
	# Wait for readiness so the first binary never races the server boot.
	i=0
	while [ "$i" -lt 50 ]; do
		if docker logs "$GATE_NATS_NAME" 2>&1 | grep -q "Server is ready"; then
			CHUG_TEST_NATS_URL="$gate_url"
			export CHUG_TEST_NATS_URL
			echo "ci: communal gate NATS at $CHUG_TEST_NATS_URL ($GATE_NATS_NAME)"
			return 0
		fi
		i=$((i + 1))
		sleep 0.2
	done
	docker rm -f "$GATE_NATS_NAME" >/dev/null 2>&1
	GATE_NATS_NAME=""
	return 0
}

# Spelled as `if` blocks, not `[ ] && cmd`: this runs both from the EXIT trap and
# from a failed local start, where an AND-list whose test is false is a `set -e`
# trip that would replace the gate's real exit status.
stop_gate_nats() {
	if [ -n "$GATE_NATS_NAME" ]; then
		docker rm -f "$GATE_NATS_NAME" >/dev/null 2>&1 || true
	fi
	GATE_NATS_NAME=""
	if [ -n "$GATE_NATS_PID" ]; then
		kill "$GATE_NATS_PID" 2>/dev/null || true
	fi
	GATE_NATS_PID=""
	if [ -n "$GATE_NATS_DIR" ]; then
		rm -rf "$GATE_NATS_DIR"
	fi
	GATE_NATS_DIR=""
	return 0
}

# Typecheck + bundle the operator UI. Same two commands .chug/tasks/web-publish.sh
# runs to produce the published dist, so a green CI here means the post-merge
# publish will build too — `web/package.json` defines `build` as
# `tsc -b && vite build`, giving typecheck and bundle in one step.
#
# This gate used to live in .chug/tasks/review-web.md, i.e. inside the *reviewer*.
# Reviewers read now; executing belongs to CI, and a web change that never
# touched Rust previously reached the merge gate with nothing having compiled
# it.
run_web_ci() {
	echo "ci: web changes — npm ci + codegen check + build + tests"
	# Subshell: the caller may still run the cargo gate afterwards, and that
	# expects to be at the workspace root.
	(
		cd web
		npm ci --no-audit --no-fund
		# The generated wire client (refactor-plan D2): regenerate from
		# .chug/schemas/api.schema.json and fail if the committed output differs.
		# The TypeScript mirror of the Rust `committed_schemas_are_current`,
		# and it has to live HERE rather than in a cargo test: a diff that
		# regenerates the schema without regenerating the client touches
		# crates/** and .chug/schemas/** — which is why .chug/schemas/** is a web-stage
		# trigger below — while a web-only diff never reaches cargo at all.
		npm run codegen:check
		npm run build
		# Unit tests, including the Rust→TS round trip over the emitted
		# sample payloads (web/src/api/roundtrip.test.ts).
		npm test
	)
}

run_full_ci() {
	cargo_ran=1
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

	# POSIX sh has ONE EXIT trap — the sccache stats printer already owns it, so
	# re-arm with both (a bare `trap stop_gate_nats EXIT` here would silently
	# clobber the stats printer; the #186 web-publish lesson). Armed BEFORE the
	# server starts, so nothing it starts can outlive the gate.
	trap 'stop_gate_nats; print_sccache_stats' EXIT
	start_gate_nats
	# After, never before: the announcement reports the mechanism's result.
	announce_tier2

	# Tier-2's dispatcher suites overflow libtest's 2 MiB default test-thread
	# stack in a debug build — a dozen binaries abort with `has overflowed its
	# stack` before asserting anything. Measured 2026-08-02 (#382): the deepest
	# chain is 17 NON-recursive dispatcher frames and peaks at 1899 KiB in debug
	# against 52 KiB in release, so it is unoptimized async state machines and
	# not a runaway in the code. 4 MiB clears the whole tier; 16 MiB is headroom.
	# Raised only when the tier actually runs, so a tier-1-only run is untouched.
	if [ "$nats_ready" -eq 1 ]; then
		RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"
		export RUST_MIN_STACK
	fi

	# Stream the test output live (tee) while capturing cargo's real exit code
	# through the pipe (POSIX sh has no `pipefail`).
	test_log="$(mktemp)"
	status_file="$(mktemp)"
	# `set +e` so a test failure doesn't abort before the exit code is
	# recorded; POSIX sh has no `pipefail`, so stash `$?` in a file that
	# survives the pipe to tee.
	set +e
	{
		# --no-fail-fast: a failing suite must not hide every suite after it.
		# The born-red tests behind #150/#160 stayed invisible for days because
		# the default fail-fast aborted each CI run at the first red binary.
		cargo test --workspace --no-fail-fast
		echo "$?" >"$status_file"
	} 2>&1 | tee "$test_log"
	set -e
	test_status="$(cat "$status_file")"
	[ -n "$test_status" ] || test_status=1

	# Per-tier pass tally: classify each test binary by whether its
	# `tests/<file>.rs` source is one of the NATS integration files. Strip the
	# ANSI colour codes `CARGO_TERM_COLOR=always` injects first, else they wrap
	# the `Running`/`passed` tokens and the match misses.
	nats_bases="$(printf '%s\n' $nats_files | sed 's#.*/tests/##' | tr '\n' ' ')"
	esc="$(printf '\033')"
	summary="$(sed "s/${esc}\\[[0-9;]*m//g" "$test_log" | awk -v natsfiles="$nats_bases" '
		BEGIN { n = split(natsfiles, a, " "); for (i = 1; i <= n; i++) isnat[a[i]] = 1; cur = "" }
		/Running / {
			cur = ""
			for (i = 1; i <= NF; i++) if ($i ~ /^tests\//) { f = $i; sub(/^tests\//, "", f); cur = f }
		}
		/test result:/ {
			p = 0
			for (i = 1; i <= NF; i++) if ($i ~ /^passed/) p = $(i - 1)
			if (cur != "" && isnat[cur]) { nats += p; ran++ } else { other += p }
			# One result per Running header; clear so the trailing Doc-tests
			# result lines (their header says "Doc-tests", not "Running", so
			# they never reset cur) are not misattributed to the last binary.
			cur = ""
		}
		END { printf "%d %d %d", other + 0, nats + 0, ran + 0 }
	')"
	other_passed="${summary%% *}"
	rest="${summary#* }"
	nats_passed="${rest%% *}"
	nats_ran="${rest##* }"

	if [ "$nats_ready" -eq 1 ]; then
		echo "ci: tier summary — tier-1+other: $other_passed passed; tier-2 (NATS): $nats_passed passed across $nats_ran file(s)"
		# A test that returned early because its server was unavailable is a pass
		# to cargo, so without a daemon that count includes the private-server
		# tests announce_tier2 named as self-skipping. Say so where the number is,
		# not only where the announcement was.
		if [ "$nats_private_ok" -eq 0 ]; then
			echo "ci:   — of which the private-server tests in $nats_private_count file(s) SELF-SKIPPED (no Docker);"
			echo "ci:     cargo counts a self-skipped test as passed, so tier-2's number is an upper bound."
		fi
	else
		echo "ci: tier summary — tier-1+other: $other_passed passed; tier-2 (NATS): SKIPPED (0 of $nats_count file(s) executed)"
	fi

	rm -f "$test_log" "$status_file"
	return "$test_status"
}

# --- config/binary version-skew gate (spec §14.3, job #110) ------------------
# ADVISORY AND EARLY. This is the fast half of a two-part gate, and it is NOT
# the authority: since #421 the DISPATCHER refuses to merge a branch declaring a
# `min_dispatcher` above its own CONFIG_SCHEMA_EPOCH (spec §3.3 step 0), which
# it can do without an API call, a credential or an env var, and therefore
# cannot degrade to a pass. What this gate buys is feedback minutes earlier, and
# one error the dispatcher's half cannot see: a config declaring an epoch newer
# than the code it ships BESIDE.
#
# Job-type config is read LIVE from the default branch, so a config that needs a
# newer dispatcher than the one deployed would otherwise merge and then escalate
# every job of the type at launch (the 2026-07-22 wrap_up incident). This gate
# fails a config's OWN CI when it declares `min_dispatcher` greater than the
# comparison epoch — "deploy first or gate it". Pure shell so a config-only
# change (which skips the Rust build below) is still gated in seconds.
#
# The comparison epoch is the running dispatcher's, read from its config
# snapshot (`GET $CHUG_API_URL/api/v1/platform/config` → .dispatcher.schema_epoch)
# — but ONLY when CHUG_API_URL is set, which no task container sets (see
# `container_env` in crates/dispatcher/src/exec.rs). The fallback to this
# checkout's own epoch is therefore the path that actually runs here; #417
# merged `min_dispatcher: 5` against a prod dispatcher at 4 that way. It says
# which branch it took, and it never blocks on an unreachable API.
config_schema_gate() {
	# The config files to gate: the changed ones when the diff is known, else
	# every .chug/jobs/*.yaml and .chug/schedules/*.yaml as a safe superset. A
	# schedule (design #310) carries `min_dispatcher` with the same meaning and
	# is read live from HEAD the same way, so it is gated the same way.
	_files=""
	if [ "${1:-}" = "all" ]; then
		for f in .chug/jobs/*.yaml .chug/schedules/*.yaml; do
			[ -f "$f" ] && _files="$_files$f
"
		done
	else
		IFS='
'
		for f in $changed; do
			case "$f" in
			.chug/jobs/*.yaml | .chug/schedules/*.yaml) [ -f "$f" ] && _files="$_files$f
" ;;
			esac
		done
		unset IFS
	fi
	[ -n "$_files" ] || return 0

	# Deployed epoch: best-effort fetch, else this checkout's compiled default.
	_deployed=""
	if [ -n "${CHUG_API_URL:-}" ] && command -v curl >/dev/null 2>&1; then
		_url="${CHUG_API_URL%/}/api/v1/platform/config"
		if [ -n "${CHUG_API_TOKEN:-}" ]; then
			_snap="$(curl -fsS -H "Authorization: Bearer ${CHUG_API_TOKEN}" "$_url" 2>/dev/null || true)"
		else
			_snap="$(curl -fsS "$_url" 2>/dev/null || true)"
		fi
		_deployed="$(printf '%s' "$_snap" \
			| sed -n 's/.*"schema_epoch"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
			| head -n1)"
	fi
	if [ -n "$_deployed" ]; then
		echo "ci: config-skew gate (advisory) — deployed dispatcher schema epoch is $_deployed"
	else
		# CONFIG_SCHEMA_EPOCH in crates/types/src/version.rs — the epoch this
		# checkout ships. Grep it so the gate needs no compiled binary.
		_deployed="$(sed -n 's/^pub const CONFIG_SCHEMA_EPOCH:[^=]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
			crates/types/src/version.rs 2>/dev/null | head -n1)"
		_deployed="${_deployed:-1}"
		echo "ci: config-skew gate (advisory) — no CHUG_API_URL, so no dispatcher was asked;"
		echo "ci:   comparing against this checkout's epoch $_deployed. The dispatcher refuses"
		echo "ci:   the merge itself if a config is ahead of the binary it runs (spec §14.3)."
	fi

	_gate_failed=0
	IFS='
'
	for f in $_files; do
		[ -n "$f" ] || continue
		_need="$(sed -n 's/^min_dispatcher:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$f" | head -n1)"
		[ -n "$_need" ] || continue
		if [ "$_need" -gt "$_deployed" ]; then
			echo "!!! ci: $f declares min_dispatcher: $_need but the dispatcher is at epoch $_deployed"
			echo "!!!     requires a coordinated deploy — deploy the newer dispatcher first,"
			echo "!!!     or land this config behind a version gate. (spec §14.3)"
			_gate_failed=1
		fi
	done
	unset IFS
	[ "$_gate_failed" -eq 0 ] || exit 1
}

# --- schedule config gate (design #310) --------------------------------------
# `.chug/schedules/*.yaml` is read live from default-branch HEAD, so a malformed
# cron or a name that disagrees with its file stem would merge and then simply
# never fire. `chuggernaut validate` owns the rules (crates/types/src/schedule.rs
# is the single implementation every consumer shares), so the gate runs the CLI
# rather than re-deriving them in shell. When the diff is known it fires only if
# a schedule file is in it; in the `all` fallback (no usable diff) it gates every
# schedule file the repo has. Either way a repo with no schedules pays nothing,
# and a diff touching none pays nothing on the diff-known path.
schedules_gate() {
	_scheds=""
	if [ "${1:-}" = "all" ]; then
		for f in .chug/schedules/*.yaml; do
			[ -f "$f" ] && _scheds="$_scheds$f
"
		done
	else
		IFS='
'
		for f in $changed; do
			case "$f" in
			.chug/schedules/*.yaml) [ -f "$f" ] && _scheds="$_scheds$f
" ;;
			esac
		done
		unset IFS
	fi
	[ -n "$_scheds" ] || return 0
	echo "ci: schedule gate — chuggernaut validate over the changed .chug/schedules/*.yaml"
	cargo_ran=1
	IFS='
'
	set -- $_scheds
	unset IFS
	cargo run --quiet -p chuggernaut -- validate "$@"
}

# --- docs/reference/modules.md registry-completeness gate (refactor-plan A3) -----------------
# Delegates to .chug/tasks/check-modules.sh so CI and the pre-commit hook
# (.githooks/pre-commit) run the SAME registry check — the gate used to live
# here as inline functions, which no other caller could reach. Pure shell, so it
# runs before the Rust early-exit: a docs-only diff skips cargo, and a rename or
# a dropped row is exactly a docs-shaped edit.
modules_registry_gate() {
	[ -x .chug/tasks/check-modules.sh ] || {
		echo "!!! ci: .chug/tasks/check-modules.sh is missing or not executable"
		exit 1
	}
	.chug/tasks/check-modules.sh
}

# --- copy-paste detection gate (docs/reference/style.md Tier 1, ticket A5) -------------------
# Delegates to .chug/tasks/check-duplication.sh so CI and the pre-commit hook run the
# SAME check with the same pinned jscpd and the same .jscpd.json — "clean
# locally" and "clean in CI" cannot diverge. Pure shell + npx, so it runs before
# the Rust early-exit for the diffs that skip cargo entirely.
duplication_gate() {
	[ -x .chug/tasks/check-duplication.sh ] || {
		echo "!!! ci: .chug/tasks/check-duplication.sh is missing or not executable"
		exit 1
	}
	.chug/tasks/check-duplication.sh
}

# --- comment lint (docs/reference/style.md Tier 1) ------------------------------------------
# Delegates to .chug/tasks/check-comments.sh: no comments in Rust/TypeScript
# sources except doc comments, and a doc comment is at most 2 sentences.
# Pure shell + awk, and a RATCHET over the lines the diff adds (the script
# computes its own change set the same way this file does), so it runs here —
# before the Rust early-exit — and gates a web-only diff too.
comments_gate() {
	[ -x .chug/tasks/check-comments.sh ] || {
		echo "!!! ci: .chug/tasks/check-comments.sh is missing or not executable"
		exit 1
	}
	.chug/tasks/check-comments.sh
}

# --- doc-fact gate (design #415 D6 checks 1-4) -------------------------------
# Delegates to .chug/tasks/check-doc-facts.sh: a backticked path claim resolves
# against `git ls-files`, a backticked constant asserted with a value agrees
# with the tree, a slice row claiming a landed job matches the history, and a
# concept registered in `docs/concepts.md` is defined only in the doc that owns
# it. Pure shell and unconditional, over EVERY tracked `*.md`, for
# the reason S1b moved it out of doc-lint.sh: the claims are made by every job
# type, and a check that runs only when a `docs` job touches a file misses most
# of the drift (job #416 was a `code` job and it orphaned ten references). Its
# own exit code is the gate — 1 = stale claims, 2 = the check could not run,
# and neither is a pass.
doc_facts_gate() {
	[ -x .chug/tasks/check-doc-facts.sh ] || {
		echo "!!! ci: .chug/tasks/check-doc-facts.sh is missing or not executable"
		exit 1
	}
	.chug/tasks/check-doc-facts.sh
}

# --- staleness ledger (design #415 D7) ---------------------------------------
# Delegates to .chug/tasks/doc-staleness.sh: for each doc, the tree files it
# names, and whether any of them has a commit newer than the doc. That is
# SUSPICION, not falsity — the doc-fact gate above answers "is this claim false
# now" and cannot answer "has anyone re-read this since the code moved", which
# is the class M1 and M3 both were. So the ledger prints its whole-tree counts
# and blocks on nothing except the docs THIS diff edits: failing a build for
# history nobody in the commit caused is how a ledger gets disabled.
#
# Its path set is check 1's, read through `check-doc-facts.sh --emit-paths`, so
# there is exactly one answer in the tree to "what paths does this doc name".
# ~0.9s whole-tree, the same order as the gate above and for the same reason —
# one `git log` pass over the history rather than one per path.
doc_staleness_ledger() {
	[ -x .chug/tasks/doc-staleness.sh ] || {
		echo "!!! ci: .chug/tasks/doc-staleness.sh is missing or not executable"
		exit 1
	}
	# With no usable diff there is nothing to gate ON, and gating the whole tree
	# instead would be exactly the "fails for history nobody caused" the design
	# rules out. Report and move on — unlike a falsity check, an unread ledger
	# costs nothing that this job introduced.
	[ "$diff_ok" -eq 1 ] || {
		.chug/tasks/doc-staleness.sh
		return 0
	}
	_md=""
	IFS='
'
	for f in $changed; do
		case "$f" in
		*.md) _md="$_md$f
" ;;
		esac
	done
	set -- $_md
	unset IFS
	# --since lets a doc clear its block with a `Doc-reread:` trailer instead of a
	# content edit: the gate wants attention, and a timestamp cannot express it (#471).
	.chug/tasks/doc-staleness.sh --gate --since "$base" "$@"
}

# --- shell test suites (job #385) ---------------------------------------------
# The tests OF THE GATES. Until #385 nothing executed a single `*.test.sh` —
# not this gate, not .githooks/pre-commit — so a regression in check-comments.sh,
# doc-lint.sh, this file, or any deploy script was caught only by an agent
# choosing to run the suite by hand. That is not a gate, and it is not even
# reliable diligence: these suites are written for the gate's Debian
# environment, so a hand-run on a macOS host produces false reds (BSD sed
# rejecting GNU label syntax, /var → /private/var). Wiring them in makes ONE
# environment authoritative. The measured miss #385 found on day one:
# coverage.test.sh had been red since #381 added an archive step to coverage.sh
# without teaching the suite's stub `cargo` to produce the files it archives.
#
# DISCOVERY IS A GLOB, over `git ls-files` and not `find`: a new suite is picked
# up with no second place to register it (#378 added ci.test.sh and nothing
# noticed), and tracked-files-only keeps node_modules/ and target/ out by
# construction rather than by a prune list that would rot. The cost a glob makes
# open-ended is bounded below instead, per suite and in total, and a glob that
# matched NOTHING fails rather than passing quietly — that is the shape of every
# gate this repo has caught covering less than it claimed.
#
# UNCONDITIONAL, like the five gates above: a nix-only or docs-only diff runs no
# other stage at all, and these suites are exactly what such a diff can break.
# Measured 2026-08-04 on the agent-rust container: 18 suites, 45s total, 18/18
# green on each of two consecutive runs. deploy/prod/update-refresh.test.sh alone
# is 27s (stub polling sleeps) and .chug/tasks/android-proof.test.sh 9-10s; the
# other 16 are 3s or under, so the 120s budget keeps ~75s of headroom.
# Re-measure and re-date this line whenever a suite is added — #395 added the
# 18th and this line still claimed the 17 that preceded it.
#
# BOTH BOUNDS ARE ENFORCED WHERE THEY ARE ANNOUNCED. The per-suite cap needs a
# working `timeout`, so that is PROBED before the header prints rather than
# fallen back on afterwards — an unconditional stage that announces a cap it is
# not applying is the #375 defect (a claim and a mechanism describing different
# worlds) in the one stage whose sibling test exists to pin that invariant. The
# total budget is checked BETWEEN suites and not after the loop, because a
# post-loop check bounds nothing: the loop's real ceiling would be count x cap,
# which grows with every suite the glob picks up (docs/reference/style.md Tier 2 rule 3).
CI_SUITE_TIMEOUT_SECS="${CHUG_CI_SUITE_TIMEOUT_SECS:-60}"
CI_SUITES_BUDGET_SECS="${CHUG_CI_SUITES_BUDGET_SECS:-120}"

shell_suites_gate() {
	if [ "${CHUG_CI_SHELL_SUITES:-1}" != "1" ]; then
		echo "ci: shell suites SKIPPED — CHUG_CI_SHELL_SUITES=${CHUG_CI_SHELL_SUITES}."
		echo "    Each suite is run with this set to 0, so the nested ci.sh that"
		echo "    .chug/tasks/ci.test.sh drives cannot recurse into the suites again."
		return 0
	fi

	_suites="$(git ls-files '*.test.sh' 2>/dev/null || true)"
	if [ -z "$_suites" ]; then
		echo "!!! ci: \`git ls-files '*.test.sh'\` matched nothing, so the shell-suite stage"
		echo "!!!     would gate NOTHING. Either this is not a git checkout or every suite"
		echo "!!!     was deleted; both are a broken gate, never a pass."
		exit 1
	fi
	_count="$(printf '%s\n' "$_suites" | grep -c .)"
	shell_suites_gate_require_timeout
	echo "ci: shell suites — $_count *.test.sh suite(s) from git ls-files;" \
		"per-suite cap ${CI_SUITE_TIMEOUT_SECS}s, total budget ${CI_SUITES_BUDGET_SECS}s"

	_suite_log="$(mktemp)"
	_suites_failed=""
	_suites_started="$(date +%s)"
	_elapsed=0
	IFS='
'
	for _s in $_suites; do
		unset IFS
		shell_suites_gate_run "$_s"
		_elapsed=$(($(date +%s) - _suites_started))
		if [ "$_elapsed" -gt "$CI_SUITES_BUDGET_SECS" ]; then
			shell_suites_gate_over_budget "$_s"
		fi
		IFS='
'
	done
	unset IFS
	rm -f "$_suite_log"

	if [ -n "$_suites_failed" ]; then
		echo "!!! ci: shell suite(s) FAILED:"
		printf '!!!      %s\n' $_suites_failed
		echo "!!!     These are the tests of the gates themselves — reproduce with"
		echo "!!!     \`sh <suite>\`. A macOS host can red them spuriously; the gate's"
		echo "!!!     Debian container is the authority."
		exit 1
	fi
	echo "ci: shell suites — all $_count passed in ${_elapsed}s"
}

# A FUNCTIONAL probe, not `command -v`: what has to hold is that the cap can be
# applied, which a `timeout` that exists but cannot run (a shim, a wrong-arch
# binary, macOS where it is `gtimeout` or absent) does not give.
shell_suites_gate_require_timeout() {
	if timeout 5 true >/dev/null 2>&1; then
		return 0
	fi
	echo "!!! ci: no working \`timeout\` on PATH, so the ${CI_SUITE_TIMEOUT_SECS}s per-suite cap cannot"
	echo "!!!     be applied. This stage is unconditional and runs in every job's gate, so"
	echo "!!!     an unbounded one would let a single hanging suite wedge the fleet — and"
	echo "!!!     announcing a cap that is not in force is the very drift .chug/tasks/ci.test.sh"
	echo "!!!     exists to pin. Install GNU coreutils, or set CHUG_CI_SHELL_SUITES=0 to opt"
	echo "!!!     out of the stage out loud (it then announces the skip and runs no suite)."
	exit 1
}

# Reported the moment the budget is crossed, naming what therefore never ran:
# a bound that is only checked once every suite has already run is not a bound.
shell_suites_gate_over_budget() { # <the suite that crossed it>
	echo "!!! ci: the shell suites crossed the ${CI_SUITES_BUDGET_SECS}s total budget at $1 (${_elapsed}s)."
	echo "!!!     This stage is unconditional, so its cost is every job's cost. Make the"
	echo "!!!     new suite fast, or raise CHUG_CI_SUITES_BUDGET_SECS deliberately with"
	echo "!!!     the measurement in the commit message."
	_not_reached="$(printf '%s\n' "$_suites" | awk -v last="$1" 'seen { print } $0 == last { seen = 1 }')"
	if [ -n "$_not_reached" ]; then
		echo "!!!     STOPPED there, so these suite(s) did NOT run and are ungated here:"
		printf '!!!      %s\n' $_not_reached
	fi
	if [ -n "$_suites_failed" ]; then
		echo "!!!     Already failing before the budget was crossed:"
		printf '!!!      %s\n' $_suites_failed
	fi
	rm -f "$_suite_log"
	exit 1
}

# One suite, timed and capped. CHUG_CI_SHELL_SUITES=0 is the recursion guard:
# ci.test.sh drives a real ci.sh, which must not run the suites a second time.
shell_suites_gate_run() {
	_t0="$(date +%s)"
	set +e
	CHUG_CI_SHELL_SUITES=0 timeout "$CI_SUITE_TIMEOUT_SECS" sh "$1" >"$_suite_log" 2>&1
	_rc=$?
	set -e
	_took=$(($(date +%s) - _t0))
	if [ "$_rc" -eq 0 ]; then
		echo "  ok   $1 (${_took}s)"
		return 0
	fi
	if [ "$_rc" -eq 124 ]; then
		echo "  FAIL $1 — killed at the ${CI_SUITE_TIMEOUT_SECS}s per-suite cap"
	else
		echo "  FAIL $1 (exit $_rc, ${_took}s)"
	fi
	sed -n '1,60p' "$_suite_log" | sed 's/^/       | /'
	_suites_failed="$_suites_failed $1"
	return 0
}

# Diff-aware gate: run each stage only when the change actually touches paths
# that stage owns, so docs/prompt/job-type changes still pass in seconds.
#   Rust stage: crates/**  Cargo.toml  Cargo.lock  rust-toolchain*  .chug/tasks/ci.sh
#   Web stage:  web/**
# The two are independent — a mixed diff runs both, a web-only diff runs the
# (fast) web stage and skips the cold cargo build entirely.
# The change set is HEAD vs the merge-base with origin/$BASE_BRANCH, which
# works for both the evaluation run (job branch) and the merge-gate rerun
# (candidate commit) — both sit ahead of the default branch.
#
# FAIL SAFE: if the changed set cannot be determined for any reason (missing
# BASE_BRANCH, fetch failure, missing merge-base, diff error), run the FULL
# CI — never skip on uncertainty. A successfully computed EMPTY diff is not
# uncertainty: HEAD is content-identical to the already-gated default branch
# (the no-commit case — web-publish/deploy jobs), so there is nothing to gate.
changed=""
diff_ok=0
if [ -n "${BASE_BRANCH:-}" ] \
	&& git fetch origin "$BASE_BRANCH:refs/remotes/origin/$BASE_BRANCH" >/dev/null 2>&1 \
	&& base="$(git merge-base HEAD "origin/$BASE_BRANCH" 2>/dev/null)" \
	&& [ -n "$base" ] \
	&& changed="$(git diff --name-only "$base"...HEAD 2>/dev/null)"; then
	diff_ok=1
fi
if [ "$diff_ok" -eq 1 ] && [ -z "$changed" ]; then
	echo "ci: HEAD identical to origin/$BASE_BRANCH — nothing to gate, skipping"
	exit 0
fi
# Run the config/binary version-skew gate before the Rust early-exit, so a
# config-only change (which skips the cargo build) is still gated. Gate the
# changed job/schedule yamls when the diff is known, else every one of them.
if [ "$diff_ok" -eq 1 ]; then
	config_schema_gate
	schedules_gate
else
	config_schema_gate all
	schedules_gate all
fi
# Registry-completeness runs unconditionally and before the Rust early-exit:
# a docs-only diff (which skips cargo) is exactly what breaks it.
modules_registry_gate
# Copy-paste detection (docs/reference/style.md Tier 1). Also before the Rust early-exit, and
# also unconditional: a web-only diff skips the cargo section, and duplicated TSX
# is exactly what a web-only diff introduces. The whole-repo run is ~30ms, so it
# needs no diff scoping. Its own exit code is the gate (1 = clones, 2 = the check
# could not run — neither is a pass).
duplication_gate
# Comment lint, same placement and for the same reason: a web-only diff never
# reaches the cargo section, and TSX is as able to carry banned prose as Rust.
comments_gate
# Doc facts, same placement and unconditional: a doc-only diff is exactly the
# diff that lands a stale path or a stale constant, and it never reaches cargo.
# ~0.6s for the whole tree.
doc_facts_gate
# The staleness ledger straight after, on the same population: the gate says
# which claims are false, the ledger says which docs nobody has re-read since
# their subject moved. Advisory except on the docs this diff edits.
doc_staleness_ledger
# The shell suites last among the pure-shell gates: they are the slowest of them
# (~37s against ~2s), so the cheap lints report first, and check-duplication.test.sh
# reuses the npx cache duplication_gate has just warmed.
shell_suites_gate
if [ "$diff_ok" -eq 1 ]; then
	rust_changed=0
	web_changed=0
	# Split the changed list on newlines so paths with spaces stay intact.
	IFS='
'
	for f in $changed; do
		case "$f" in
		crates/* | Cargo.toml | Cargo.lock | rust-toolchain* | .chug/tasks/ci.sh)
			rust_changed=1
			;;
		web/* | .chug/schemas/*)
			# .chug/schemas/** is a WEB trigger too: the generated TypeScript client
			# is built from .chug/schemas/api.schema.json, so a Rust type change that
			# re-emits the schema without regenerating the client leaves the
			# two out of step — and that diff touches no web/ path at all.
			web_changed=1
			;;
		esac
	done
	unset IFS
	# Web first: it is the cheap stage, so a mixed diff surfaces a broken
	# frontend in seconds instead of after a cold cargo build. Spelled as a
	# full `if` rather than `[ ] && cmd`, whose exit status is the *test's* when
	# the test fails — a `set -e` footgun that would abort CI on every
	# Rust-only diff.
	if [ "$web_changed" -eq 1 ]; then
		run_web_ci
	fi
	if [ "$rust_changed" -eq 0 ]; then
		echo "ci: no Rust changes — skipping cargo fmt/clippy/test"
		exit 0
	fi
	run_full_ci
else
	echo "ci: could not determine changed files — running full CI"
	run_web_ci
	run_full_ci
fi
