#!/bin/sh
# Shared documentation lint for the `design` and `docs` job types (wired as a
# stage-1 command evaluator in .chug/jobs/design.yaml and .chug/jobs/docs.yaml). Four
# checks over the changed markdown, no external tooling required (POSIX sh +
# awk, both in the agent image):
#
#   1. Markdown well-formedness — a small vendored checker: headings need a
#      space after the `#`, and code fences must be closed. ERRORS (fail).
#      Trailing whitespace is a WARNING only.
#   2. Intra-repo links resolve — every relative markdown link `](path)` must
#      point at a file/dir that exists (anchors and `http(s)://`/`mailto:`
#      targets are skipped). A dangling relative link is an ERROR (fail).
#   3. Referenced code paths exist — a backtick token that names a path in THIS
#      checkout must be tracked by git. A missing path is a WARNING only
#      (design #415 S1a: precision before teeth; S1b promotes it). Resolution is
#      against `git ls-files`, never the filesystem, so the verdict cannot depend
#      on whether the caller ran `cargo build` — the same reason
#      .chug/tasks/check-modules.sh's header gives for its own shape. A directory
#      claim (`crates/channel/`) is satisfied by any tracked path beneath it.
#
#      Judging is refused rather than guessed, because a token this script
#      classifies wrongly is what buried the signal: 313 warnings over every
#      tracked `.md` at the base this landed on, ~6 of them real (design #415
#      measured 256 at 28e5aa1, and 330 at the original S1a branch point; the
#      corpus moved under both). A
#      token is skipped SILENTLY unless it has a slash and its first segment is a
#      tracked top-level entry of this repo — which excludes `src/api.ts` and
#      `dispatcher/tests/execution.rs` (rooted somewhere else) as well as bare
#      symbols (`JobType::validate`). It is skipped too when it holds a glob
#      (`crates/*/src/lib.rs`), a `{placeholder}` or `<name>` template, a `$VAR`,
#      an elision (`.../x`, `…/x`), an expression character, or a leading `/`
#      or `~` — an absolute path names a container or a node, not this tree. A
#      trailing `:193` or `:42-79` line citation is stripped and the file itself
#      still checked.
#
#      Two markers suppress the warning on the line that carries them, for the
#      two ways an unresolvable path is correct rather than stale:
#      `<!-- intent -->` for what is designed but not built, `<!-- runtime -->`
#      for what is correctly absent from git (build output, operator-owned
#      files). They are documented in STYLE.md's doc-claim rule.
#   4. Design filenames — a `.md` directly under `docs/design/` must be named
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
# Diff-aware, mirroring .chug/tasks/ci.sh: it lints only the markdown the change
# touches and self-skips cheaply when the diff has no `.md` files. Fail-safe —
# if the changed set cannot be determined, it lints every tracked `.md` rather
# than skip on uncertainty. Explicit file arguments override the diff selection
# (used by .chug/tasks/doc-lint.test.sh).
set -eu

root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

# --- The tracked-path index check 3 resolves against ------------------------
# Built once from `git ls-files`; `tracked_roots` is the `|`-delimited set of
# top-level entries a path claim may start with. Outside a git checkout both are
# empty and check 3 refuses to judge rather than guessing off the filesystem.
tracked_index="$(mktemp)"
trap 'rm -f "$tracked_index"' EXIT
git -C "$root" ls-files >"$tracked_index" 2>/dev/null || : >"$tracked_index"
tracked_roots="|$(awk -F/ 'NF > 1 { print $1 }' "$tracked_index" | sort -u | tr '\n' '|')"
if [ ! -s "$tracked_index" ]; then
	echo "doc-lint: not a git checkout — referenced-path check disabled"
fi

# Tracked as a file, or as a directory with any tracked path beneath it.
path_tracked() {
	awk -v p="$1" '
		$0 == p || index($0, p "/") == 1 { hit = 1; exit }
		END { exit(hit ? 0 : 1) }
	' "$tracked_index"
}

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
		if ($0 ~ /<!--[ \t]*(intent|runtime)[ \t]*-->/) next
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
path_warnings=0
report_err()  { echo "ERROR $1"; errors=$((errors + 1)); }
report_warn() { echo "warn  $1"; warnings=$((warnings + 1)); }

IFS='
'
for f in $files; do
	[ -f "$f" ] || continue # a deleted doc shows in the diff but has no content
	# Rule 4. Unanchored `*docs/design/*` would match an absolute path too.
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
		CODE)
			[ -s "$tracked_index" ] || continue
			case "$val" in
			*/*) : ;;
			*) continue ;;  # symbol / no-slash token — not a path claim
			esac
			# Shapes that are prose about paths, not a claim one exists.
			case "$val" in
			*" "* | *"://"*) continue ;;                    # not a bare path
			*"*"* | *"?"*) continue ;;                      # glob
			*"{"* | *"}"* | *"<"* | *">"*) continue ;;      # {placeholder} / <name>
			*'$'*) continue ;;                              # $VAR interpolation
			*"..."* | *"…"*) continue ;;                    # elided middle
			*"("* | *")"* | *"'"* | *'"'* | *","*) continue ;;  # expression, not a path
			/* | "~"*) continue ;;                          # container/node path, not this tree
			esac
			# `crates/x/y.rs:193` and `:42-79` cite a line; the file is still checked.
			p="$val"
			case "$p" in
			*:*)
				suffix="${p##*:}"
				case "$suffix" in
				"" | *[!0123456789-]* | -* | *-) : ;;
				*) p="${p%:*}" ;;
				esac
				;;
			esac
			p="${p%/}" # a trailing slash is a directory claim
			case "$tracked_roots" in
			*"|${p%%/*}|"*) : ;;
			*) continue ;;  # rooted somewhere other than this checkout
			esac
			path_tracked "$p" && continue
			report_warn "$f:$ln: referenced path not found -> $val"
			path_warnings=$((path_warnings + 1))
			;;
		esac
	done <<-RECORDS
		$out
	RECORDS
done
unset IFS

echo "doc-lint: $errors error(s), $warnings warning(s) across $(printf '%s\n' "$files" | grep -c . ) file(s)"
if [ "$path_warnings" -gt 0 ]; then
	echo "doc-lint: a referenced path that is correctly unresolvable is marked on its own line —"
	echo "doc-lint:   <!-- intent -->  designed, not built    <!-- runtime -->  absent from git on purpose"
	echo "doc-lint: see STYLE.md's doc-claim rule (Tier 2). Anything else is a stale claim: fix the path."
fi
[ "$errors" -eq 0 ]
