#!/bin/sh
# Shared documentation lint for the `design` and `docs` job types (wired as a
# stage-1 command evaluator in jobs/design.yaml and jobs/docs.yaml). Three
# checks over the changed markdown, no external tooling required (POSIX sh +
# awk, both in the agent image):
#
#   1. Markdown well-formedness — a small vendored checker: headings need a
#      space after the `#`, and code fences must be closed. ERRORS (fail).
#      Trailing whitespace is a WARNING only.
#   2. Intra-repo links resolve — every relative markdown link `](path)` must
#      point at a file/dir that exists (anchors and `http(s)://`/`mailto:`
#      targets are skipped). A dangling relative link is an ERROR (fail).
#   3. Referenced code paths exist — backtick tokens that look like repo paths
#      (e.g. `crates/x/src/y.rs`, `tasks/ci.sh`) are stat'd against the repo
#      root. Best-effort: a missing path is a WARNING only (a doc may cite a
#      path a sibling job will create), and bare symbols (`JobType::validate`,
#      no slash) are not checked at all.
#
# Diff-aware, mirroring tasks/ci.sh: it lints only the markdown the change
# touches and self-skips cheaply when the diff has no `.md` files. Fail-safe —
# if the changed set cannot be determined, it lints every tracked `.md` rather
# than skip on uncertainty. Explicit file arguments override the diff selection
# (used by tasks/doc-lint.test.sh).
set -eu

root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# --- Select the markdown files to lint -------------------------------------
files=""
if [ "$#" -gt 0 ]; then
	# Explicit mode: lint exactly the `.md` arguments given.
	for f in "$@"; do
		case "$f" in
		*.md) files="$files$f
" ;;
		esac
	done
else
	# Diff-aware: HEAD vs the merge-base with origin/$BASE_BRANCH, like
	# tasks/ci.sh. A cleanly-computed empty (or no-markdown) diff → skip; an
	# uncomputable diff → fall back to every tracked `.md` (never skip on
	# uncertainty).
	changed=""
	diff_ok=0
	if [ -n "${BASE_BRANCH:-}" ] \
		&& git fetch origin "$BASE_BRANCH:refs/remotes/origin/$BASE_BRANCH" >/dev/null 2>&1 \
		&& base="$(git merge-base HEAD "origin/$BASE_BRANCH" 2>/dev/null)" \
		&& [ -n "$base" ] \
		&& changed="$(git diff --name-only "$base"...HEAD 2>/dev/null)"; then
		diff_ok=1
	fi
	if [ "$diff_ok" -eq 1 ]; then
		IFS='
'
		for f in $changed; do
			case "$f" in
			*.md) files="$files$f
" ;;
			esac
		done
		unset IFS
	else
		echo "doc-lint: could not determine changed files — linting all tracked .md"
		files="$(git ls-files '*.md' 2>/dev/null || true)
"
	fi
fi

# Drop blank entries.
files="$(printf '%s' "$files" | grep -v '^$' || true)"
if [ -z "$files" ]; then
	echo "doc-lint: no markdown in the diff — nothing to lint, skipping"
	exit 0
fi

# --- Lint each file ---------------------------------------------------------
# awk extracts structured records per line so the shell can resolve paths:
#   ERR:<line>:<msg>   WARN:<line>:<msg>   LINK:<line>:<target>   CODE:<line>:<tok>
# Code fences (``` / ~~~) are tracked so their contents are not linted.
extract() {
	awk '
	BEGIN { fence = 0 }
	{
		if ($0 ~ /^[[:space:]]*(```|~~~)/) { fence = !fence; next }
		if (fence) next
		if ($0 ~ /^#+[^ #]/) print "ERR:" NR ":heading needs a space after #"
		if ($0 ~ /[ \t]+$/)  print "WARN:" NR ":trailing whitespace"
		s = $0
		while (match(s, /\]\([^)]+\)/)) {
			print "LINK:" NR ":" substr(s, RSTART + 2, RLENGTH - 3)
			s = substr(s, RSTART + RLENGTH)
		}
		t = $0
		while (match(t, /`[^`]+`/)) {
			print "CODE:" NR ":" substr(t, RSTART + 1, RLENGTH - 2)
			t = substr(t, RSTART + RLENGTH)
		}
	}
	END { if (fence) print "ERR:0:unclosed code fence" }
	' "$1"
}

errors=0
warnings=0
report_err()  { echo "ERROR $1"; errors=$((errors + 1)); }
report_warn() { echo "warn  $1"; warnings=$((warnings + 1)); }

IFS='
'
for f in $files; do
	[ -f "$f" ] || continue # a deleted doc shows in the diff but has no content
	dir="$(dirname "$f")"
	out="$(extract "$f")"
	while IFS= read -r rec; do
		[ -n "$rec" ] || continue
		kind="${rec%%:*}"
		rest="${rec#*:}"
		ln="${rest%%:*}"
		val="${rest#*:}"
		case "$kind" in
		ERR) report_err "$f:$ln: $val" ;;
		WARN) report_warn "$f:$ln: $val" ;;
		LINK)
			target="${val%% *}"  # drop any `"title"` suffix
			target="${target%%#*}" # drop any #anchor
			[ -n "$target" ] || continue
			case "$target" in
			http://* | https://* | ftp://* | mailto:* | tel:* | //* | \#*) continue ;;
			*"://"*) continue ;;
			esac
			[ -e "$dir/$target" ] || report_err "$f:$ln: broken relative link -> $target"
			;;
		CODE)
			case "$val" in
			*" "* | *"://"*) continue ;;  # not a bare path
			*/*.*) [ -e "$root/$val" ] || report_warn "$f:$ln: referenced path not found -> $val" ;;
			*) continue ;;  # symbol / no-slash token — not checked
			esac
			;;
		esac
	done <<-RECORDS
		$out
	RECORDS
done
unset IFS

echo "doc-lint: $errors error(s), $warnings warning(s) across $(printf '%s\n' "$files" | grep -c . ) file(s)"
[ "$errors" -eq 0 ]
