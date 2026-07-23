#!/bin/sh
# Project-wide CI gate (wired in jobs/_defaults.yaml). Mirrors
# .github/workflows/ci.yml.
#
# Tier-2 (NATS) integration tests run for real here: the agent-rust image bakes
# a `nats-server` binary, and test-utils' harness spawns it as an ephemeral
# process (deploy/prod/Dockerfile.agent-rust, testing.md). When NEITHER a
# `nats-server` binary NOR a Docker daemon is present the harness self-skips —
# so this script announces the tier state up front and prints a per-tier
# summary afterward, and a green gate is never silently partial (Docker-only
# tier-3 tests remain out of scope and self-skip regardless).
set -eu
export CARGO_TERM_COLOR=always

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
			printf "sccache: %s hits / %s misses (%s%% hit rate), cache size %s", \
				hits, misses, rate, size
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

# Prefer a local nats-server binary (what the CI image bakes), else a Docker
# daemon — mirrors NatsTestServer::spawn's mechanism order.
nats_ready=0
if command -v nats-server >/dev/null 2>&1; then
	nats_ready=1
elif docker info --format '{{.ServerVersion}}' >/dev/null 2>&1; then
	nats_ready=1
fi

announce_tier2() {
	if [ "$nats_ready" -eq 1 ]; then
		echo "ci: tier-2 (NATS) ENABLED — $nats_count integration file(s) execute against a real nats-server"
	else
		echo "ci: tier-2 (NATS) SKIPPED — no nats-server binary or Docker daemon;"
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

run_full_ci() {
	cargo_ran=1
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

	announce_tier2

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
	else
		echo "ci: tier summary — tier-1+other: $other_passed passed; tier-2 (NATS): SKIPPED (0 of $nats_count file(s) executed)"
	fi

	rm -f "$test_log" "$status_file"
	return "$test_status"
}

# --- config/binary version-skew gate (spec §14, job #110) --------------------
# Job-type config is read LIVE from the default branch, so a config that needs a
# newer dispatcher than the one deployed would otherwise merge and then escalate
# every job of the type at launch (the 2026-07-22 wrap_up incident). This gate
# fails a config's OWN CI when it declares `min_dispatcher` greater than the
# DEPLOYED dispatcher's schema epoch — "deploy first or gate it" — before it can
# merge. Pure shell so a config-only change (which skips the Rust build below)
# is still gated in seconds.
#
# The deployed epoch is read from the running dispatcher's config snapshot
# (`GET $CHUG_API_URL/api/v1/platform/config` → .dispatcher.schema_epoch). When
# that is not reachable (no CHUG_API_URL / no token / offline CI) the gate falls
# back to comparing against this checkout's own epoch — still catching a config
# that requires an epoch newer than the code it ships beside. It never blocks on
# an unreachable API.
config_schema_gate() {
	# The job-type files to gate: the changed ones when the diff is known, else
	# every jobs/*.yaml as a safe superset.
	_files=""
	if [ "${1:-}" = "all" ]; then
		for f in jobs/*.yaml; do
			[ -f "$f" ] && _files="$_files$f
"
		done
	else
		IFS='
'
		for f in $changed; do
			case "$f" in
			jobs/*.yaml) [ -f "$f" ] && _files="$_files$f
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
		echo "ci: config-skew gate — deployed dispatcher schema epoch is $_deployed"
	else
		# CONFIG_SCHEMA_EPOCH in crates/types/src/version.rs — the epoch this
		# checkout ships. Grep it so the gate needs no compiled binary.
		_deployed="$(sed -n 's/^pub const CONFIG_SCHEMA_EPOCH:[^=]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
			crates/types/src/version.rs 2>/dev/null | head -n1)"
		_deployed="${_deployed:-1}"
		echo "ci: config-skew gate — dispatcher not reachable; comparing against this checkout's epoch $_deployed"
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
			echo "!!!     or land this config behind a version gate. (spec §14)"
			_gate_failed=1
		fi
	done
	unset IFS
	[ "$_gate_failed" -eq 0 ] || exit 1
}

# Diff-aware gate: only run the (slow) Rust build/test when the change
# actually touches Rust-relevant paths. This lets docs/web/prompt/job-type
# changes pass CI in seconds. Rust-relevant globs (see case list below):
#   crates/**  Cargo.toml  Cargo.lock  rust-toolchain*  tasks/ci.sh
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
# changed job yamls when the diff is known, else every jobs/*.yaml.
if [ "$diff_ok" -eq 1 ]; then
	config_schema_gate
else
	config_schema_gate all
fi
if [ "$diff_ok" -eq 1 ]; then
	rust_changed=0
	# Split the changed list on newlines so paths with spaces stay intact.
	IFS='
'
	for f in $changed; do
		case "$f" in
		crates/* | Cargo.toml | Cargo.lock | rust-toolchain* | tasks/ci.sh)
			rust_changed=1
			break
			;;
		esac
	done
	unset IFS
	if [ "$rust_changed" -eq 0 ]; then
		echo "ci: no Rust changes — skipping cargo fmt/clippy/test"
		exit 0
	fi
	run_full_ci
else
	echo "ci: could not determine changed files — running full CI"
	run_full_ci
fi
