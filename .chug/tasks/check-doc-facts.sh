#!/bin/sh
# Doc-fact gate — design #415 D6 checks 1 and 2. Two mechanical claims a
# markdown file makes about THIS tree, both resolved against git and never the
# filesystem, so the verdict cannot depend on whether the caller ran
# `cargo build` (the reason .chug/tasks/check-modules.sh's header gives for its
# own shape):
#
#   1. Referenced paths exist — a backtick token that names a path in this
#      checkout must be tracked by `git ls-files`. A directory claim
#      (`crates/channel/`) is satisfied by any tracked path beneath it.
#   2. Asserted constant values agree — a backticked SCREAMING_SNAKE_CASE name
#      that resolves to an integer `pub const` in this tree, asserted with a
#      value on the same line, must state the tree's value.
#
# Both were rules 3 and 5 of .chug/tasks/doc-lint.sh and MOVED here whole (slice
# S1b). What they DECIDE is unchanged — S1a fixed check 1's precision, S1c
# settled check 2's recognised shapes, S2 swept the findings; there is no second
# copy left behind. What changed is reach and volume: whole tree, every job,
# ERROR. `doc-lint.sh` keeps well-formedness, relative links and the design
# filename shape, which are what a `docs`/`design` job alone needs.
#
# Why whole-tree and every job, rather than the changed markdown of a docs job:
# the claims are made by every job type. Job #416 was a `code` job and it
# orphaned ten `.chug/tags/` references; a check that runs only when a docs job
# touches a file is a check that misses most of the drift. Pure shell for the
# same reason as check-modules.sh — it runs BEFORE .chug/tasks/ci.sh's Rust
# early-exit, and a doc-only diff is exactly the diff that breaks it.
#
# JUDGING IS REFUSED RATHER THAN GUESSED, and that mattered more once the
# verdict became fatal (#415 M7: a noisy gate is an off gate). An unparseable or
# unclassifiable token is skipped SILENTLY. Check 1 skips a token unless it has
# a slash and its first segment is a tracked top-level entry of this repo —
# which excludes `src/api.ts` and `dispatcher/tests/execution.rs` (rooted in
# some other repo) as well as bare symbols (`JobType::validate`). It skips one
# holding a glob (`crates/*/src/lib.rs`), a `{placeholder}` or `<name>`
# template, a `$VAR`, an elision (`.../x`, `…/x`), an expression character, or a
# leading `/` or `~` — an absolute path names a container or a node, not this
# tree. A trailing `:193` or `:42-79` line citation is stripped and the file
# itself still checked.
#
# Check 2 reads exactly two line-scoped assertion shapes, both enumerated in
# `const_claim` below, and is silent on everything else — a transition
# (`2 → 3`), a target (`bump … to 5`), a past tense (`was 2`), a bound
# (`>= 2`), a value on the next line, and a number written as a word. A name
# resolving to no integer `pub const` is silent (an unknown identifier is not a
# claim about this tree, which is what keeps a design doc's proposed constant
# quiet); a name resolving to two consts that disagree is silent too, because
# there is no way to pick and a guess is worse than nothing.
#
# Three markers suppress BOTH checks on the line that carries them, for the
# three ways a claim is correct rather than stale: `<!-- intent -->` for what is
# designed but not built (and for the value a slice will bump a constant to),
# `<!-- runtime -->` for what is correctly absent from git (build output,
# operator-owned files), and `<!-- absent -->` for a line that names a path
# *because it does not exist* — a measurement of staleness, a rejected
# alternative, a recorded deletion. They are documented in STYLE.md's doc-claim
# rule. An append-only design body is not licence to hide a false present-tense
# claim behind one: if the sentence must keep the path, the sentence says what
# happened to it.
#
# The scanner runs under LC_ALL=C so the verdict is the same on every host and
# every awk: the tree carries astral-plane characters that macOS's BWK awk
# aborts on in a UTF-8 locale, and shell/awk character ranges follow the
# locale's collation order (the reason .chug/tasks/check-comments.sh pins it and
# doc-lint.sh spells its slug classes out).
#
# Usage:
#   .chug/tasks/check-doc-facts.sh            # every tracked *.md (CI, the gate)
#   .chug/tasks/check-doc-facts.sh --staged   # the staged *.md (.githooks/pre-commit)
#   .chug/tasks/check-doc-facts.sh <file>...  # explicit, repo-relative (its test suite)
#
# `--staged` is the hook's mode: scoped rather than whole-tree to keep the
# hook's ~2s budget, with one honest gap — a commit that DELETES a path other
# docs name passes the hook and fails CI. Rejecting there cannot block a commit
# CI would accept, because CI runs this unconditionally.
#
# Exit: 0 = clean. 1 = findings, each named with its file and line. 2 = the
# check could not run (no git, no awk, nothing tracked) — a LINTER ERROR, never
# a verdict, because a doc-fact check that cannot run must not read as clean.
#
# Test: .chug/tasks/check-doc-facts.test.sh
set -eu
LC_ALL=C
export LC_ALL

doc_facts_unrunnable() { # <line>...
	for _l in "$@"; do echo "!!! check-doc-facts: $_l"; done
	echo "!!!     This is a LINTER ERROR, not a clean tree: the doc claims went unchecked."
	exit 2
}

command -v git >/dev/null 2>&1 || doc_facts_unrunnable "no \`git\` on PATH — doc claims resolve against the index"
command -v awk >/dev/null 2>&1 || doc_facts_unrunnable "no \`awk\` on PATH — the scanner cannot run"

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] || doc_facts_unrunnable \
	"not a git checkout — a path claim resolves against \`git ls-files\`, never the" \
	"    filesystem, so outside a checkout there is nothing to resolve against."
cd "$root" || doc_facts_unrunnable "cannot enter the repo root $root"

# --- The tracked-path index check 1 resolves against -------------------------
# `tracked_roots` is the `|`-delimited set of top-level entries a path claim may
# start with; a token rooted anywhere else belongs to some other repo.
tracked_index="$(mktemp)"
const_index="$(mktemp)"
trap 'rm -f "$tracked_index" "$const_index"' EXIT
git ls-files >"$tracked_index" 2>/dev/null || : >"$tracked_index"
[ -s "$tracked_index" ] || doc_facts_unrunnable \
	"\`git ls-files\` listed nothing — check 1 has no index to resolve against."
tracked_roots="|$(awk -F/ 'NF > 1 { print $1 }' "$tracked_index" | sort -u | tr '\n' '|')"

# Tracked as a file, or as a directory with any tracked path beneath it.
path_tracked() {
	awk -v p="$1" '
		$0 == p || index($0, p "/") == 1 { hit = 1; exit }
		END { exit(hit ? 0 : 1) }
	' "$tracked_index"
}

# --- The `pub const` index check 2 resolves against --------------------------
# `NAME<tab>VALUE` for every integer-literal `pub const` in a tracked `.rs`
# file. The literal must terminate the initializer, so an expression-valued
# const (`16 * 1024 * 1024`) is absent rather than half-read, and a doc stating
# its arithmetic result is not second-guessed.
git grep -hE \
	'^[[:space:]]*pub const [A-Z][A-Z0-9_]*[[:space:]]*:[^=]*=[[:space:]]*[0-9][0-9_]*(_?[ui](8|16|32|64|size))?[[:space:]]*;' \
	-- '*.rs' 2>/dev/null \
	| awk '
		{
			name = $0
			sub(/^[[:space:]]*pub const /, "", name)
			sub(/[[:space:]]*:.*$/, "", name)
			value = $0
			sub(/^[^=]*=[[:space:]]*/, "", value)
			sub(/[^0-9_].*$/, "", value)
			gsub(/_/, "", value)
			if (name != "" && value != "") print name "\t" value
		}
	' | sort -u >"$const_index" || : >"$const_index"

# The tree's value for a name, or nothing when it names no integer `pub const`
# or names two that disagree — both are refusals to judge, never a guess.
const_value() {
	awk -F'\t' -v n="$1" '
		$1 == n { if (found && $2 != value) multi = 1; value = $2; found = 1 }
		END { if (found && !multi) print value }
	' "$const_index"
}

# --- Select the markdown to check -------------------------------------------
mode="tree"
if [ "${1:-}" = "--staged" ]; then
	mode="staged"
	files="$(git diff --cached --name-only --diff-filter=ACMR -- '*.md' 2>/dev/null || true)"
elif [ "$#" -gt 0 ]; then
	mode="explicit"
	files=""
	for f in "$@"; do
		case "$f" in
		*.md) files="$files$f
" ;;
		esac
	done
else
	files="$(git ls-files -- '*.md' 2>/dev/null || true)"
fi

files="$(printf '%s\n' "$files" | grep -v '^$' || true)"
if [ -z "$files" ]; then
	echo "check-doc-facts: no markdown to check ($mode) — nothing to do"
	exit 0
fi

# --- Scan ---------------------------------------------------------------------
# awk emits one record per candidate token so the shell can resolve it against
# the two indexes:  PATH:<line>:<token>   CONST:<line>:<name>=<claimed>
# Code fences (``` / ~~~) are tracked so their contents are not scanned — a
# fenced block is an example, not a claim.
doc_facts_scan() {
	awk '
	# The value a line asserts for the name that precedes `rest`, or "" when the
	# line asserts none. The two-character lookahead is what keeps `5th` and a
	# dotted `5.2` out.
	function asserted_value(rest,   after, hit) {
		if (match(rest, /^[ \t]*(=|==|is|is currently|is already|is now)[ \t]*[`*]*[0-9]+[`*]*/) \
			|| match(rest, /^[ \t]*\([ \t]*(currently|now)[ \t]+[`*]*[0-9]+[`*]*[ \t]*\)/) \
			|| match(rest, /^[ \t]*\|[ \t]*[`*]*[0-9]+[`*]*[ \t]*\|/)) {
			after = substr(rest, RSTART + RLENGTH, 2)
			if (after ~ /^[0-9A-Za-z]/ || after ~ /^\.[0-9]/) return ""
			hit = substr(rest, RSTART, RLENGTH)
			gsub(/[^0-9]/, "", hit)
			return hit
		}
		return ""
	}
	# `NAME=<claimed>` when this backticked token carries a value claim, either
	# inside the backticks (`NAME = 5`, `NAME: u32 = 5`, a quoted `pub const`
	# line) or in the text immediately after them.
	function const_claim(tok, rest,   name, value) {
		if (tok ~ /^(pub const )?[A-Z][A-Z0-9]*(_[A-Z0-9]+)+[ \t]*(:[ \t]*[A-Za-z0-9_]+)?[ \t]*=[ \t]*[0-9]+;?$/) {
			name = tok
			sub(/^pub const /, "", name)
			sub(/[ \t]*[:=].*$/, "", name)
			value = tok
			sub(/^[^=]*=[ \t]*/, "", value)
			sub(/[^0-9].*$/, "", value)
			return name "=" value
		}
		if (tok ~ /^[A-Z][A-Z0-9]*(_[A-Z0-9]+)+$/) {
			value = asserted_value(rest)
			if (value != "") return tok "=" value
		}
		return ""
	}
	BEGIN { fence = 0 }
	{
		if ($0 ~ /^[[:space:]]*(```|~~~)/) { fence = !fence; next }
		if (fence) next
		if ($0 ~ /<!--[ \t]*(intent|runtime|absent)[ \t]*-->/) next
		t = $0
		while (match(t, /`[^`]+`/)) {
			tok = substr(t, RSTART + 1, RLENGTH - 2)
			t = substr(t, RSTART + RLENGTH)
			print "PATH:" NR ":" tok
			claim = const_claim(tok, t)
			if (claim != "") print "CONST:" NR ":" claim
		}
	}
	' "$1"
}

# One `PATH` record: a finding, or silence when the token is not a claim this
# checkout can judge.
doc_facts_check_path() { # <file> <line> <token>
	case "$3" in
	*/*) : ;;
	*) return 0 ;; # symbol / no-slash token — not a path claim
	esac
	case "$3" in
	*" "* | *"://"*) return 0 ;;                       # not a bare path
	*"*"* | *"?"*) return 0 ;;                         # glob
	*"{"* | *"}"* | *"<"* | *">"*) return 0 ;;         # {placeholder} / <name>
	*'$'*) return 0 ;;                                 # $VAR interpolation
	*"..."* | *"…"*) return 0 ;;                       # elided middle
	*"("* | *")"* | *"'"* | *'"'* | *","*) return 0 ;; # expression, not a path
	/* | "~"*) return 0 ;;                             # container/node path, not this tree
	esac
	# `crates/x/y.rs:193` and `:42-79` cite a line; the file is still checked.
	_p="$3"
	case "$_p" in
	*:*)
		_suffix="${_p##*:}"
		case "$_suffix" in
		"" | *[!0123456789-]* | -* | *-) : ;;
		*) _p="${_p%:*}" ;;
		esac
		;;
	esac
	_p="${_p%/}" # a trailing slash is a directory claim
	case "$tracked_roots" in
	*"|${_p%%/*}|"*) : ;;
	*) return 0 ;; # rooted somewhere other than this checkout
	esac
	path_tracked "$_p" && return 0
	echo "!!! check-doc-facts: $1:$2: referenced path not found -> $3"
	path_findings=$((path_findings + 1))
	return 0
}

# One `CONST` record, same contract: silent unless the tree disagrees.
doc_facts_check_const() { # <file> <line> <name>=<claimed>
	[ -s "$const_index" ] || return 0
	_name="${3%%=*}"
	_claimed="${3#*=}"
	_actual="$(const_value "$_name")"
	[ -n "$_actual" ] || return 0 # unknown or ambiguous: not judged
	[ "$_claimed" = "$_actual" ] && return 0
	echo "!!! check-doc-facts: $1:$2: stale constant -> $_name is $_actual in the tree, not $_claimed"
	const_findings=$((const_findings + 1))
	return 0
}

path_findings=0
const_findings=0
checked=0

IFS='
'
for f in $files; do
	[ -f "$f" ] || continue # a deleted doc shows in a diff but has no content
	checked=$((checked + 1))
	out="$(doc_facts_scan "$f")"
	while IFS= read -r rec; do
		[ -n "$rec" ] || continue
		kind="${rec%%:*}"
		rest="${rec#*:}"
		ln="${rest%%:*}"
		val="${rest#*:}"
		case "$kind" in
		PATH) doc_facts_check_path "$f" "$ln" "$val" ;;
		CONST) doc_facts_check_const "$f" "$ln" "$val" ;;
		esac
	done <<-RECORDS
		$out
	RECORDS
done
unset IFS

if [ "$path_findings" -gt 0 ]; then
	echo "!!! check-doc-facts: $path_findings stale path claim(s)."
	echo "!!!     Fix the path, or mark the line if it is correctly unresolvable:"
	echo "!!!       <!-- intent -->  designed, not built"
	echo "!!!       <!-- runtime --> absent from git on purpose (build output, operator files)"
	echo "!!!       <!-- absent -->  named BECAUSE it does not exist (a staleness measurement,"
	echo "!!!                        a rejected alternative, a recorded deletion)"
	echo "!!!     A path that resolves in another repo takes no marker — qualify it"
	echo "!!!     (\`{repo}:{path}\`). See STYLE.md's doc-claim rule (Tier 2)."
fi
if [ "$const_findings" -gt 0 ]; then
	echo "!!! check-doc-facts: $const_findings stale constant claim(s)."
	echo "!!!     A constant's value is owned by the tree: restate it from the source, or"
	echo "!!!     link instead. Inside an append-only body, say when it was true"
	echo "!!!     (\"was 2 when this landed\") rather than leaving a false present tense."
	echo "!!!       <!-- intent -->  the value a slice will bump it to, not the value today"
fi

_findings=$((path_findings + const_findings))
if [ "$_findings" -ne 0 ]; then
	echo "!!!     Reproduce locally with: .chug/tasks/check-doc-facts.sh"
	exit 1
fi

echo "check-doc-facts: clean ($checked markdown file(s) checked, $mode)"
