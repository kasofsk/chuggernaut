#!/bin/sh
# Project-wide CI gate (wired in jobs/_defaults.yaml). Mirrors
# .github/workflows/ci.yml. Runs inside the agent container: NATS/Docker
# integration tests self-skip there (require_nats! in test-utils).
set -eu
export CARGO_TERM_COLOR=always

run_full_ci() {
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
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
# BASE_BRANCH, fetch failure, missing merge-base, empty/absent diff), run the
# FULL CI — never skip on uncertainty.
changed=""
if [ -n "${BASE_BRANCH:-}" ] \
	&& git fetch origin "$BASE_BRANCH:refs/remotes/origin/$BASE_BRANCH" >/dev/null 2>&1 \
	&& base="$(git merge-base HEAD "origin/$BASE_BRANCH" 2>/dev/null)" \
	&& [ -n "$base" ] \
	&& changed="$(git diff --name-only "$base"...HEAD 2>/dev/null)" \
	&& [ -n "$changed" ]; then
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
