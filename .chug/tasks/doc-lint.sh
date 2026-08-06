#!/bin/sh
# Shared documentation lint for the `design` and `docs` job types (wired as a
# stage-1 command evaluator in .chug/jobs/design.yaml and .chug/jobs/docs.yaml).
# Three checks over the changed markdown, no external tooling required (POSIX sh
# + awk, both in the agent image):
#
#   1. Markdown well-formedness — a small vendored checker: headings need a
#      space after the `#`, and code fences must be closed. ERRORS (fail).
#      Trailing whitespace is a WARNING only.
#   2. Intra-repo links resolve — every relative markdown link `](path)` must
#      point at a file/dir that exists (anchors and `http(s)://`/`mailto:`
#      targets are skipped). A dangling relative link is an ERROR (fail).
#   3. Design filenames — a `.md` directly under `docs/design/` must be named
#      `{seq}-{slug}.md`: leading digits, a hyphen, then a lowercase-kebab slug.
#      That is the shape the Designs view sorts and labels by and the shape the
#      `design/{stem}` group name joins on (crates/types/src/groups.rs), so a
#      malformed name is an ERROR (fail). The match anchors on the repo-relative
#      path — a path merely *ending* in `docs/design/x.md` is some other repo's
#      file, and a nested subdirectory is out of scope. Its character classes
#      are spelled out rather than written as `a-z` ranges: range membership in
#      shell patterns follows the locale's collation order, so under
#      en_US.UTF-8 `a-z` also spans the uppercase letters and the rule would
#      quietly stop rejecting them. Same reason .chug/tasks/check-comments.sh
#      pins LC_ALL=C — the verdict must be the same on every host.
#
# THE PATH AND CONSTANT CHECKS LIVE IN .chug/tasks/check-doc-facts.sh, not here
# (design #415 D6, slice S1b). They were rules 3 and 5 of this script and moved
# out whole — there is no second copy, because two implementations of one rule
# drift. What they check is unchanged; where they run is not: every job's
# pre-stage, over every tracked `*.md`, as an ERROR. The three checks kept here
# are the ones only a `docs`/`design` job needs.
#
# Diff-aware, mirroring .chug/tasks/ci.sh: it lints only the markdown the change
# touches and self-skips cheaply when the diff has no `.md` files. Fail-safe —
# if the changed set cannot be determined, it lints every tracked `.md` rather
# than skip on uncertainty. Explicit file arguments override the diff selection
# (used by .chug/tasks/doc-lint.test.sh).
#
# Usage:
#   .chug/tasks/doc-lint.sh [<file>...]
#   .chug/tasks/doc-lint.sh --emit-links [<file>...]
#
# `--emit-links` is rule 2's extractor with the verdict removed: it judges
# nothing and prints `<file><tab><line><tab><target>` for every intra-repo
# relative link, with the target normalized to a repo-relative path. It exists
# so the D15 orphan half of .chug/tasks/doc-staleness.sh asks rule 2 "what does
# this doc link to" rather than growing a second answer — the same arrangement
# check 1's `--emit-paths` already has with that script's staleness half.
# Whole-tree unless files are named, and it resolves nothing: a target that
# names no tracked file simply falls out of the ledger's join, and a dangling
# link is already rule 2's finding.
set -eu

emit_links=0
if [ "${1:-}" = "--emit-links" ]; then
	emit_links=1
	shift
	root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
	[ -n "$root" ] || {
		echo "doc-lint: not a git checkout — --emit-links has no tree to name paths against" >&2
		exit 2
	}
	cd "$root" || exit 2
fi

# --- Select the markdown files to lint -------------------------------------
files=""
if [ "$emit_links" -eq 1 ] && [ "$#" -eq 0 ]; then
	files="$(git ls-files '*.md' 2>/dev/null || true)
"
elif [ "$#" -gt 0 ]; then
	# Explicit mode: lint exactly the `.md` arguments given.
	for f in "$@"; do
		case "$f" in
		*.md) files="$files$f
" ;;
		esac
	done
else
	# Diff-aware: HEAD vs the merge-base with origin/$BASE_BRANCH, like
	# .chug/tasks/ci.sh. A cleanly-computed empty (or no-markdown) diff → skip; an
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
	[ "$emit_links" -eq 1 ] || echo "doc-lint: no markdown in the diff — nothing to lint, skipping"
	exit 0
fi

# --- Lint each file ---------------------------------------------------------
# awk extracts structured records per line so the shell can resolve link
# targets:  ERR:<line>:<msg>   WARN:<line>:<msg>   LINK:<line>:<target>
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
	}
	END { if (fence) print "ERR:0:unclosed code fence" }
	' "$1"
}

# `--emit-links` reads the same records and stops there. The target is joined
# onto the linking file's directory and the `.`/`..` segments are collapsed, so
# `docs/design/415-x.md` linking `../../.chug/tasks/ci.sh` emits the path the
# rest of the tree calls it by; an anchor-only or off-tree target is dropped.
if [ "$emit_links" -eq 1 ]; then
	IFS='
'
	for f in $files; do
		[ -f "$f" ] || continue
		extract "$f" | awk -v f="$f" -v dir="$(dirname "$f")" '
			function norm(p,   a, n, i, k, o, r) {
				n = split(p, a, "/"); k = 0
				for (i = 1; i <= n; i++) {
					if (a[i] == "" || a[i] == ".") continue
					if (a[i] == "..") { if (k > 0) k--; continue }
					o[++k] = a[i]
				}
				for (i = 1; i <= k; i++) r = r (i > 1 ? "/" : "") o[i]
				return r
			}
			/^LINK:/ {
				rest = substr($0, 6)
				ln = rest; sub(/:.*$/, "", ln)
				t = substr(rest, length(ln) + 2)
				sub(/ .*$/, "", t)
				sub(/#.*$/, "", t)
				if (t == "" || t ~ /:\/\// || t ~ /^(mailto|tel):/ || t ~ /^\//) next
				out = norm(dir "/" t)
				if (out != "") print f "\t" ln "\t" out
			}
		'
	done
	unset IFS
	exit 0
fi

errors=0
warnings=0
report_err()  { echo "ERROR $1"; errors=$((errors + 1)); }
report_warn() { echo "warn  $1"; warnings=$((warnings + 1)); }

IFS='
'
for f in $files; do
	[ -f "$f" ] || continue # a deleted doc shows in the diff but has no content
	# Rule 3. Unanchored `*docs/design/*` would match an absolute path too.
	case "$f" in
	docs/design/*/*) : ;; # a nested subdirectory is not a design doc
	docs/design/*.md)
		base="${f##*/}"
		seq="${base%%-*}"   # everything before the first hyphen
		slug="${base#*-}"   # ...and everything after it, minus the extension
		slug="${slug%.md}"
		name_ok=1
		case "$seq" in
		"" | *[!0123456789]*) name_ok=0 ;;
		esac
		case "$slug" in
		*[!abcdefghijklmnopqrstuvwxyz0123456789-]*) name_ok=0 ;;
		esac
		[ "$name_ok" -eq 1 ] \
			|| report_err "$f: design doc filename must be {seq}-{slug}.md, e.g. 309-host-native-execution.md"
		;;
	esac
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
		esac
	done <<-RECORDS
		$out
	RECORDS
done
unset IFS

echo "doc-lint: $errors error(s), $warnings warning(s) across $(printf '%s\n' "$files" | grep -c . ) file(s)"
[ "$errors" -eq 0 ]
