#!/bin/sh
# MODULES.md registry-completeness gate (refactor-plan A3).
#
# The module registry (MODULES.md) is what jobs get scoped against, so it must
# not drift from the tree the way crates.md's dispatcher map did: every
# dispatcher and domain module file has a row, and every row names a real module
# file. Pure shell so it runs BEFORE .chug/tasks/ci.sh's Rust early-exit — a
# docs-only diff skips cargo, and a rename or a dropped row is exactly a
# docs-shaped edit, so a check living only in the Rust tests would be silently
# bypassed by the changes most likely to break it (the config_schema_gate
# precedent).
#
# CI (.chug/tasks/ci.sh) and the pre-commit hook (.githooks/pre-commit) both
# call THIS script, so "clean locally" and "clean in CI" cannot diverge. It
# takes no arguments and reads the tree under the CURRENT directory (the repo
# root for both callers, a fixture root for .chug/tasks/modules-registry.test.sh).
#
# Exit: 0 = registry and tree agree. 1 = drift (each offending module or row is
# named), or MODULES.md is missing.
set -eu

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

modules_registry_gate
