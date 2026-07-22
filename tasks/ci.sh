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
		cargo test --workspace
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
