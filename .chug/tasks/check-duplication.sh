#!/bin/sh
# Copy-paste detection gate (STYLE.md Tier 1 `live`). Runs jscpd over the whole
# repo and FAILS on any clone at all — `threshold: 0` in .jscpd.json, no ratchet.
#
# Why gate this at all: duplicated logic drifts apart, and the copy that didn't
# get the fix is where the next bug lives. Agent-written code duplicates far more
# readily than human-written code — an agent that cannot find the existing helper
# writes a second one — so this is a higher-value gate here than in a
# human-authored codebase.
#
# Why the version is PINNED EXACTLY (`jscpd@5.0.5`, never `@5`):
#   * jscpd v5 is a Rust rewrite (the `cpd` binary), a different implementation
#     from the v4 JavaScript tool — different config semantics, different clone
#     sets. v4 docs do not apply. In v5, `ignorePattern` is the glob-exclusion
#     key (in v4 that name meant an in-file regex, where globs silently failed).
#   * A floating major resolves at run time, so the pre-commit hook's npx cache
#     and CI could silently run different releases; 5.0.4 → 5.0.5 changed
#     `ignorePattern` matching. Bump this deliberately, with the clone set
#     re-measured in the same change.
#
# Why `**/*.gen.ts` is in .jscpd.json's ignorePattern (refactor-plan D2):
# generated code is not refactorable. `web/src/api/types.gen.ts` repeats itself
# exactly where its SOURCE does — `JobSummary` is a projection of `Job`, so the
# two carry the same field docs — and the only way to satisfy the gate would be
# to hand-edit a file stamped DO-NOT-EDIT, which the codegen drift check then
# fails. The duplication that matters is duplication a human wrote; the source
# of these files (the Rust `types` crate) is itself under this gate.
#
# CI (.chug/tasks/ci.sh) and the pre-commit hook both call THIS script, so "clean
# locally" and "clean in CI" cannot diverge. The whole-repo run costs ~30ms, so
# it is unconditional — no diff scoping, and none should be added: a web-only or
# docs-only diff exits ci.sh before the cargo section, and those are exactly the
# diffs most likely to introduce duplicated TSX.
#
# Usage:
#   .chug/tasks/check-duplication.sh            # scan the repo root
#   .chug/tasks/check-duplication.sh <path>...  # scan only these paths (used by the
#                                         # shell test; same config either way)
#
# Exit: 0 = no clones. 1 = clones found (the report names both copies; extract
# the shared code into a helper, or — for a deliberate exception — bracket the
# region with `jscpd:ignore-start` / `jscpd:ignore-end` comments AND a comment
# saying why). 2 = the check could not run (no node/npx, or the npm registry is
# unreachable); that is a broken gate, never a pass.
set -eu

JSCPD_VERSION="5.0.5"

# The repo root is two levels above this script (it lives in `.chug/tasks/`),
# not the caller's cwd: the pre-commit hook, CI and a shell test all invoke it
# from different directories and must all scan (and configure from) the same
# tree. Resolved with shell builtins only, so the diagnostics below still work
# on a stripped PATH.
case "$0" in
*/*) here="${0%/*}" ;;
*) here="." ;;
esac
root="$(cd "$here/../.." && pwd)"
cd "$root"

config="$root/.jscpd.json"
if [ ! -f "$config" ]; then
	echo "!!! check-duplication: $config is missing — the duplication gate cannot run"
	exit 2
fi

if ! command -v npx >/dev/null 2>&1; then
	echo "!!! check-duplication: no \`npx\` on PATH, so jscpd cannot run."
	echo "!!!     The gate is NOT satisfied — install node in the image, or vendor jscpd."
	exit 2
fi

if [ "$#" -gt 0 ]; then
	targets="$*"
else
	targets="$root"
fi

echo "check-duplication: jscpd@$JSCPD_VERSION over $targets (threshold 0)"

# `--exit-code` fails on ANY clone, which is what `threshold: 0` means without
# depending on the reported percentage rounding to something above zero.
out="$(mktemp)"
set +e
# shellcheck disable=SC2086 # word-split $targets into separate path arguments
npx --yes "jscpd@$JSCPD_VERSION" --no-colors --exit-code -c "$config" $targets >"$out" 2>&1
status=$?
set -e
cat "$out"

# A run that produced no verdict line did not measure anything (npx could not
# fetch the package, the registry is unreachable, node is broken). Its non-zero
# exit must NOT be reported as duplication, and its zero exit must not pass.
if ! grep -qE 'clones|No duplicates found' "$out"; then
	rm -f "$out"
	echo "!!! check-duplication: jscpd produced no report (see output above)."
	echo "!!!     Most likely npx could not fetch jscpd@$JSCPD_VERSION from the npm"
	echo "!!!     registry. The gate is NOT satisfied — fix connectivity or vendor the"
	echo "!!!     tool; do not merge on an unrun duplication check."
	exit 2
fi
rm -f "$out"

if [ "$status" -ne 0 ]; then
	echo "!!! check-duplication: duplicated code found (threshold is 0 — STYLE.md Tier 1)."
	echo "!!!     Each report names both copies. Extract the shared body into a helper"
	echo "!!!     (named after its caller, STYLE.md Tier 2 rule 4) rather than raising a"
	echo "!!!     threshold. Reproduce locally with: .chug/tasks/check-duplication.sh"
	exit 1
fi

echo "check-duplication: no duplicated code"
