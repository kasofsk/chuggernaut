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

# --- sccache liveness guard ---------------------------------------------------
# The agent image may carry an sccache binary for the WRONG arch (observed
# 2026-07-24: x86_64 sccache under qemu on an arm64 worker), and the emulated
# server can deadlock on start — cargo then parks forever waiting on its
# compiler wrapper, wedging the whole CI task with zero CPU. Probe the server
# once with a hard timeout; if it cannot answer, compile WITHOUT the wrapper
# (slower, never wedged). Loud either way.
if [ -n "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null 2>&1; then
	if timeout 15 sccache --start-server >/dev/null 2>&1 || timeout 5 sccache --show-stats >/dev/null 2>&1; then
		echo "ci: sccache server answering — wrapper enabled"
	else
		echo "ci: WARNING sccache server did not answer within 15s (emulated/broken binary?) — disabling RUSTC_WRAPPER for this run"
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

# One communal NATS server for the whole gate (#206). Every test binary's
# namespaced `shared()` connects here via CHUG_TEST_NATS_URL instead of each
# spinning its own Docker container — per-binary brokers (plus lingering
# reaper lag, plus the tier-3 real-container suites) saturated a laptop
# Docker daemon and flaked the gate with setup timeouts. Per-test namespaces
# make the sharing safe; the private-server suites (prod-named or config-mode)
# deliberately ignore the env and keep their own containers. Best-effort: if
# Docker cannot start it, tests fall back to per-binary containers (or skip,
# exactly as without it).
GATE_NATS_NAME=""
start_gate_nats() {
	[ -n "${CHUG_TEST_NATS_URL:-}" ] && return 0 # caller provided one
	command -v docker >/dev/null 2>&1 || return 0
	docker info >/dev/null 2>&1 || return 0
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

stop_gate_nats() {
	[ -n "$GATE_NATS_NAME" ] && docker rm -f "$GATE_NATS_NAME" >/dev/null 2>&1
	GATE_NATS_NAME=""
}

# Typecheck + bundle the operator UI. Same two commands tasks/web-publish.sh
# runs to produce the published dist, so a green CI here means the post-merge
# publish will build too — `web/package.json` defines `build` as
# `tsc -b && vite build`, giving typecheck and bundle in one step.
#
# This gate used to live in tasks/review-web.md, i.e. inside the *reviewer*.
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
		# schemas/api.schema.json and fail if the committed output differs.
		# The TypeScript mirror of the Rust `committed_schemas_are_current`,
		# and it has to live HERE rather than in a cargo test: a diff that
		# regenerates the schema without regenerating the client touches
		# crates/** and schemas/** — which is why schemas/** is a web-stage
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

	announce_tier2

	start_gate_nats
	# POSIX sh has ONE EXIT trap — line ~61 already owns it for the sccache
	# stats, so re-arm with both (a bare `trap stop_gate_nats EXIT` here would
	# silently clobber the stats printer; the #186 web-publish lesson).
	trap 'stop_gate_nats; print_sccache_stats' EXIT

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

# --- MODULES.md registry-completeness gate (refactor-plan A3) -----------------
# The module registry (MODULES.md) is what jobs get scoped against, so it must
# not drift from the tree the way crates.md's dispatcher map did: every
# dispatcher and domain module file has a row, and every row names a real module
# file. Pure shell so it runs BEFORE the Rust early-exit — a docs-only diff skips
# cargo, and a rename or a dropped row is exactly a docs-shaped edit, so a check
# living only in the Rust tests would be silently bypassed by the changes most
# likely to break it (the config_schema_gate precedent).

# The module names a src tree contributes to the registry: every `*.rs` under it,
# src-relative with `.rs` stripped, `<dir>/mod.rs` collapsing to `<dir>` so a
# named context directory registers under its own name (refactor-plan C8), and
# the crate root (`lib.rs`) excluded.
modules_on_disk() {
	find "$1" -name '*.rs' | while IFS= read -r f; do
		rel="${f#"$1"/}"
		rel="${rel%.rs}"
		case "$rel" in
		lib) ;;
		*/mod) echo "${rel%/mod}" ;;
		*) echo "$rel" ;;
		esac
	done | sort -u
}

# The registry rows for one src tree: the first-column backticked name of every
# table row under a `## ` heading naming that tree. Matching on the src path (not
# a crate nickname) is what lets a context get its own `## ` section — its
# heading carries the same path prefix. Sections for other trees, and the prose
# before the first heading, are scoped out.
modules_registry_rows() {
	awk -v want="$1" '
		/^## / { insection = (index($0, want) > 0) }
		insection && /^\|[[:space:]]*`/ {
			row = $0
			sub(/^\|[[:space:]]*`/, "", row)
			sub(/`.*/, "", row)
			print row
		}
	' MODULES.md | sort -u
}

# Both directions for one src tree: no module without a row, no row without a
# module. Sets _gate_failed rather than exiting, so one run reports every drift.
modules_registry_compare() {
	_dir="$1"
	_files="$(modules_on_disk "$_dir")"
	_rows="$(modules_registry_rows "$_dir")"
	for m in $_files; do
		printf '%s\n' "$_rows" | grep -qx "$m" || {
			echo "!!! ci: $_dir/$m.rs has no row in MODULES.md"
			echo "!!!     (refactor-plan A3 registry drift — add a one-line contract row)"
			_gate_failed=1
		}
	done
	for m in $_rows; do
		printf '%s\n' "$_files" | grep -qx "$m" || {
			echo "!!! ci: MODULES.md lists module \`$m\` with no $_dir/$m.rs (or $_dir/$m/mod.rs)"
			echo "!!!     (refactor-plan A3 registry drift — remove or fix the stale row)"
			_gate_failed=1
		}
	done
}

modules_registry_gate() {
	_disp_dir="crates/dispatcher/src"
	[ -d "$_disp_dir" ] || return 0
	if [ ! -f MODULES.md ]; then
		echo "!!! ci: MODULES.md is missing — the module registry gate cannot run"
		exit 1
	fi

	_gate_failed=0
	modules_registry_compare "$_disp_dir"
	# Same drift check for the pure domain crate (refactor-plan C1) and for
	# each context crate carved out of the dispatcher (C9). A context that
	# leaves for its own crate keeps its registry section — it is still what
	# jobs get scoped against — so the gate has to follow it out.
	for _extra_dir in crates/domain/src crates/platform-ops/src; do
		[ -d "$_extra_dir" ] && modules_registry_compare "$_extra_dir"
	done

	[ "$_gate_failed" -eq 0 ] || exit 1
	echo "ci: MODULES.md registry gate — dispatcher, domain and context modules are in sync"
}

# --- copy-paste detection gate (STYLE.md Tier 1, ticket A5) -------------------
# Delegates to tasks/check-duplication.sh so CI and the pre-commit hook run the
# SAME check with the same pinned jscpd and the same .jscpd.json — "clean
# locally" and "clean in CI" cannot diverge. Pure shell + npx, so it runs before
# the Rust early-exit for the diffs that skip cargo entirely.
duplication_gate() {
	[ -x tasks/check-duplication.sh ] || {
		echo "!!! ci: tasks/check-duplication.sh is missing or not executable"
		exit 1
	}
	tasks/check-duplication.sh
}

# Diff-aware gate: run each stage only when the change actually touches paths
# that stage owns, so docs/prompt/job-type changes still pass in seconds.
#   Rust stage: crates/**  Cargo.toml  Cargo.lock  rust-toolchain*  tasks/ci.sh
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
# changed job yamls when the diff is known, else every jobs/*.yaml.
if [ "$diff_ok" -eq 1 ]; then
	config_schema_gate
else
	config_schema_gate all
fi
# Registry-completeness runs unconditionally and before the Rust early-exit:
# a docs-only diff (which skips cargo) is exactly what breaks it.
modules_registry_gate
# Copy-paste detection (STYLE.md Tier 1). Also before the Rust early-exit, and
# also unconditional: a web-only diff skips the cargo section, and duplicated TSX
# is exactly what a web-only diff introduces. The whole-repo run is ~30ms, so it
# needs no diff scoping. Its own exit code is the gate (1 = clones, 2 = the check
# could not run — neither is a pass).
duplication_gate
if [ "$diff_ok" -eq 1 ]; then
	rust_changed=0
	web_changed=0
	# Split the changed list on newlines so paths with spaces stay intact.
	IFS='
'
	for f in $changed; do
		case "$f" in
		crates/* | Cargo.toml | Cargo.lock | rust-toolchain* | tasks/ci.sh)
			rust_changed=1
			;;
		web/* | schemas/*)
			# schemas/** is a WEB trigger too: the generated TypeScript client
			# is built from schemas/api.schema.json, so a Rust type change that
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
