#!/bin/sh
# Doc-fact gate — design #415 D6 checks 1, 2 and 3. Three mechanical claims a
# markdown file makes about THIS tree, all resolved against git and never the
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
#   3. A slice claiming a landed job matches the git history — a slice-table row
#      in `docs/design/*.md` saying `**Landed** (job #N)` must correspond to a
#      `job/N: {type}` squash-merge commit, and a doc whose head says
#      `Status: IMPLEMENTED` must have no slice row still in an unlanded state.
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
# Check 3 (slice S5) reads ONE shape, and it is the shape the retrofit of the
# remaining design-doc heads should write: `**Landed** (job #N)` in a cell of a
# markdown table row, per #362's sequencing table. That row shape is the union
# of the two conventions already in the tree — #440 carries the state in its own
# `State` column, #415 and #362 carry it in the gate cell, and a check that
# matches the row rather than the column matches both without a third
# convention. `Shipped (job #N)` is accepted as the synonym #373 already uses;
# nothing else is. A row whose state is a bare `**Landed**`, a job number that
# is not `#<digits>`, a claim in prose rather than a table row, and any markdown
# outside `docs/design/` are all SKIPPED, and a doc with no slice table produces
# no records at all — most design docs have none, and #415 M7 says a gate that
# guesses is a gate someone turns off.
#
# Rule 2 is narrower still: it fires only inside a table that already carries a
# landed claim (so the check knows it found the slice table and not some other
# one), only when the head's `Status:` word is exactly `IMPLEMENTED` — not
# `IMPLEMENTED IN PART` — and only on a cell that is exactly one unlanded state
# word (`Proposed`, `Planned`, `Deferred`, `Pending`, `Intent`, `Not started`,
# `Not landed`, `In progress`, bold or plain). A gate cell that argues its state
# in a sentence is not judged.
#
# ABSENT IS ABSENT: a job that was Revoked and one that never existed are the
# same finding, reported the same way. #415's own head named revoked job #87 as
# live work, so the distinction is real — but only the platform API knows it,
# and a gate that depends on a reachable API degrades to a silent pass (job
# #421, the config-skew gate). The remedy is identical either way: the row is
# wrong and the author rewrites it.
#
# The job currently landing is exempt, because D10 requires the implementing job
# to mark its own slice landed IN THE SAME COMMIT — so `job/N` cannot yet exist
# when that commit is gated. Its number comes from `$JOB_ID` (set in every task
# container) and from a `job/N` branch name, never from the network. Check 3
# also stands down whole when the history holds no `job/N:` commit at all or the
# checkout is shallow: with no index to resolve against, refusing is the only
# safe verdict.
#
# Three markers suppress BOTH the path and the constant check on the line that
# carries them, for the three ways a claim is correct rather than stale. They do
# not reach check 3: a slice row is a claim about a job, not about a path, and
# `<!-- intent -->` on a row asserting `**Landed**` would be a contradiction
# rather than an escape. `<!-- intent -->` is for what is designed but not
# built (and for the value a slice will bump a constant to),
# `<!-- runtime -->` for what is correctly absent from git (build output,
# operator-owned files), and `<!-- absent -->` for a line that names a path
# *because it does not exist* — a measurement of staleness, a rejected
# alternative, a recorded deletion. They are documented in docs/reference/style.md's doc-claim
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
job_index="$(mktemp)"
trap 'rm -f "$tracked_index" "$const_index" "$job_index"' EXIT
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

# --- The merged-job index check 3 resolves against ---------------------------
# One line per job this history squash-merged, read from the commit subjects
# (`job/{N}: {type}`) for the same reason check 1 reads `git ls-files`: the
# claim is about what merged, and only git knows that offline. A shallow
# checkout or a history with no such subject leaves check 3 with nothing to
# resolve against, so it stands down rather than reporting every row.
git log --format='%s' 2>/dev/null \
	| awk '/^job\/[0-9]+:[ \t]/ { sub(/^job\//, ""); sub(/:.*$/, ""); print }' \
	| sort -u >"$job_index" || : >"$job_index"
job_index_usable=1
[ -s "$job_index" ] || job_index_usable=0
[ "$(git rev-parse --is-shallow-repository 2>/dev/null || echo unknown)" = "false" ] \
	|| job_index_usable=0

# The job whose commit cannot exist yet: this one. It joins the index rather
# than being compared beside it, so the lookup stays one `grep -qx` and cannot
# be broken by the caller's `IFS`. `$JOB_ID` is set in every task container and
# the branch name carries it in a local checkout.
case "${JOB_ID:-}" in
"" | *[!0123456789]*) : ;;
*) echo "$JOB_ID" >>"$job_index" ;;
esac
_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
case "${_branch#job/}" in
"$_branch" | "" | *[!0123456789]*) : ;;
*) echo "${_branch#job/}" >>"$job_index" ;;
esac

job_merged() { # <seq>
	grep -qx "$1" "$job_index"
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

# Check 3's scanner, over a `docs/design/*.md` only. Emits
# `SLICE:<line>:<seq>` for each landed claim, and — inside a table that carries
# one, in a doc whose status is exactly `IMPLEMENTED` — `UNLANDED:<line>:<row
# label>|<state>` for each row still in an unlanded state.
doc_facts_scan_slices() {
	awk '
	function trim(c) {
		sub(/^[ \t]*[*_]*[ \t]*/, "", c)
		sub(/[ \t]*[*_]*[ \t]*$/, "", c)
		return c
	}
	function flush_table(   i) {
		if (landed) for (i = 1; i <= held; i++) print pending[i]
		landed = 0
		held = 0
	}
	BEGIN {
		fence = 0; implemented = 0; seen_status = 0; in_table = 0; landed = 0; held = 0
		unlanded["proposed"] = 1; unlanded["planned"] = 1; unlanded["deferred"] = 1
		unlanded["pending"] = 1; unlanded["intent"] = 1; unlanded["not started"] = 1
		unlanded["not landed"] = 1; unlanded["in progress"] = 1
	}
	{
		if ($0 ~ /^[[:space:]]*(```|~~~)/) { fence = !fence; next }
		if (fence) next
		if (!seen_status && $0 ~ /^Status:/) {
			seen_status = 1
			s = $0
			sub(/^Status:[ \t]*/, "", s)
			st = match(s, /^[A-Za-z][A-Za-z ]*/) ? substr(s, 1, RLENGTH) : ""
			sub(/[ \t]+$/, "", st)
			implemented = (st == "IMPLEMENTED")
		}
		if ($0 !~ /^[ \t]*\|/) { if (in_table) flush_table(); in_table = 0; next }
		in_table = 1
		t = $0
		while (match(t, /(Landed|Shipped)[*_ \t]*\(job #[0-9]+/)) {
			hit = substr(t, RSTART, RLENGTH)
			t = substr(t, RSTART + RLENGTH)
			sub(/^[^#]*#/, "", hit)
			print "SLICE:" NR ":" hit
			landed = 1
		}
		if (!implemented) next
		n = split($0, cells, "|")
		label = n > 1 ? trim(cells[2]) : ""
		for (i = 2; i <= n; i++) {
			state = trim(cells[i])
			if (state != "" && (tolower(state) in unlanded))
				pending[++held] = "UNLANDED:" NR ":" label "|" state
		}
	}
	END { if (in_table) flush_table() }
	' "$1"
}

# One `SLICE` record: silent unless the history holds no such merge.
doc_facts_check_slice() { # <file> <line> <seq>
	[ "$job_index_usable" = 1 ] || return 0
	job_merged "$3" && return 0
	echo "!!! check-doc-facts: $1:$2: slice claims a job that never merged -> job #$3"
	slice_findings=$((slice_findings + 1))
	return 0
}

# One `UNLANDED` record: the doc says IMPLEMENTED and this row says otherwise.
doc_facts_check_unlanded() { # <file> <line> <label>|<state>
	_label="${3%%|*}"
	_state="${3#*|}"
	[ -n "$_label" ] || _label="(unlabelled)"
	echo "!!! check-doc-facts: $1:$2: Status: IMPLEMENTED but slice $_label is $_state"
	slice_findings=$((slice_findings + 1))
	return 0
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
slice_findings=0
checked=0

IFS='
'
for f in $files; do
	[ -f "$f" ] || continue # a deleted doc shows in a diff but has no content
	checked=$((checked + 1))
	out="$(doc_facts_scan "$f")"
	case "$f" in
	docs/design/*.md) out="$out
$(doc_facts_scan_slices "$f")" ;;
	esac
	while IFS= read -r rec; do
		[ -n "$rec" ] || continue
		kind="${rec%%:*}"
		rest="${rec#*:}"
		ln="${rest%%:*}"
		val="${rest#*:}"
		case "$kind" in
		PATH) doc_facts_check_path "$f" "$ln" "$val" ;;
		CONST) doc_facts_check_const "$f" "$ln" "$val" ;;
		SLICE) doc_facts_check_slice "$f" "$ln" "$val" ;;
		UNLANDED) doc_facts_check_unlanded "$f" "$ln" "$val" ;;
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
	echo "!!!     (\`{repo}:{path}\`). See docs/reference/style.md's doc-claim rule (Tier 2)."
fi
if [ "$const_findings" -gt 0 ]; then
	echo "!!! check-doc-facts: $const_findings stale constant claim(s)."
	echo "!!!     A constant's value is owned by the tree: restate it from the source, or"
	echo "!!!     link instead. Inside an append-only body, say when it was true"
	echo "!!!     (\"was 2 when this landed\") rather than leaving a false present tense."
	echo "!!!       <!-- intent -->  the value a slice will bump it to, not the value today"
fi

if [ "$slice_findings" -gt 0 ]; then
	echo "!!! check-doc-facts: $slice_findings slice claim(s) the git history does not support."
	echo "!!!     A slice row says a job landed. Either the job never merged (a revoked job"
	echo "!!!     and one that never existed read the same here — both mean the row is wrong),"
	echo "!!!     or the doc's head says IMPLEMENTED while a row is still unlanded."
	echo "!!!     Write the row as \`**Landed** (job #N)\` only once \`job/N\` is merged;"
	echo "!!!     the job doing the landing is exempt, because it writes the row in the same"
	echo "!!!     commit (design #415 D10)."
fi

_findings=$((path_findings + const_findings + slice_findings))
if [ "$_findings" -ne 0 ]; then
	echo "!!!     Reproduce locally with: .chug/tasks/check-doc-facts.sh"
	exit 1
fi

echo "check-doc-facts: clean ($checked markdown file(s) checked, $mode)"
