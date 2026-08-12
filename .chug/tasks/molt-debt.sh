#!/bin/sh
# The molt debt reader — design #533 S3. How much shell has the corpus re-grown
# since the last molt?
#
# ADVISORY, ALWAYS. Exit 0 with debt to report, because accrued saga is not a
# defect in the commit that accrued it — the same argument that keeps
# `.chug/tasks/doc-staleness.sh` advisory. Nothing here gates a job, and
# `.chug/tasks/ci.sh` does not call it: it is a reader an operator runs before
# deciding to molt.
#
# There is deliberately NO THRESHOLD and no "molt recommended" line. A number
# nobody calibrated becomes either noise or a target. This ranks; you read the top.
#
# The watermark is the newest `job/N: molt` squash-merge commit, TREE-WIDE — the
# same commit shape check 3 of check-doc-facts.sh resolves, so it invents no
# convention and needs nothing declared: no `last-molted:` front matter and no
# dates in prose, for #415 D7's reason. One global point is meaningful because a
# molt takes the project quiescent, so everything after that commit is
# ordinary-work dirt by construction. Before the first molt every doc reports
# `never` and is measured from nothing, which is the honest answer and is how
# #533 S4's ordering gets picked.
#
# TWO GIT TRAPS, BOTH MEASURED, BOTH RECORDED IN #533's 2026-08-10 CORRECTION.
# They are why this script looks the way it does.
#
#   1. NEVER limit a diff to a doc's current path. `git diff -M --numstat
#      A..B -- docs/spec.md` reports `2714 0` — a total rewrite — because the
#      pathspec drops the old path's deletion, so no rename pair exists for
#      detection to find. `-M` does not rescue it: rename detection has been on
#      by default since git 2.9 and this repo does not set `diff.renames`. The
#      whole-tree diff reports the truth, `124 52 spec.md => docs/spec.md`. So
#      this takes ONE whole-tree diff and matches the `old => new` row. That row
#      carries the OLD path too, which is what the point-side blob must be read
#      under: look a moved doc up at the watermark by its HEAD path and the blob
#      is simply absent, so every saga section and job-number mention it already
#      carried reads as new. Same trap, two more columns.
#   2. `git log --follow --numstat` SUMMED is a different quantity. It adds
#      per-commit deltas, so it counts twice any line touched in more than one
#      commit: `+156 / -84` against the end-to-end `+124 / -52`. It is never the
#      end-to-end figure. `--follow` is used here for COUNTING COMMITS and
#      nothing else, and every numstat read uses `--pretty=tformat:` so the
#      output holds rows only — a field filter is how the figure in #544 went
#      wrong, since a rename row splits into five fields, not three.
#
# Two blobs per doc at most (the point and HEAD), never the intermediate
# history, so a `--filter=blob:none` clone fetches lazily and stays cheap.
#
# Usage:
#   molt-debt.sh                 # whole-tree reading list, ranked by growth
#   molt-debt.sh --since <ref>   # treat <ref> as the molt point (bootstrap/what-if)
#   molt-debt.sh <file>...       # only these docs
set -eu

# Diagnostics go to STDERR, not stdout. The population is built inside a loop
# whose stdout is redirected into a temp file, so a message written to stdout
# there lands in that file and the operator sees an exit code with no reason —
# which is the one thing a LINTER ERROR must never be.
debt_unrunnable() { # <line>...
	for _l in "$@"; do echo "!!! molt-debt: $_l" >&2; done
	echo "!!!     This is a LINTER ERROR, not a clean corpus: the debt went unread." >&2
	exit 2
}

command -v git >/dev/null 2>&1 || debt_unrunnable "no \`git\` on PATH — the debt is derived from history"
command -v awk >/dev/null 2>&1 || debt_unrunnable "no \`awk\` on PATH — the columns cannot be joined"

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] || debt_unrunnable "not inside a git work tree — there is no history to read"
cd "$root"
git rev-parse HEAD >/dev/null 2>&1 || debt_unrunnable "HEAD does not resolve — an empty repository has no debt to report"

since=""
if [ "${1:-}" = "--since" ]; then
	[ $# -ge 2 ] || debt_unrunnable "--since needs a ref"
	since="$2"
	shift 2
	git rev-parse --verify --quiet "$since" >/dev/null 2>&1 \
		|| debt_unrunnable "--since $since does not resolve to a commit"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- The population ----------------------------------------------------------
#
# Every tracked `*.md`: the corpus a molt sheds is the whole reference tier,
# CLAUDE.md, the prose under `.chug/` and `docs/design/` (docs/reference/docs.md's
# two kinds). Tracked-only keeps node_modules/ and target/ out by construction.
docs="$work/docs"
if [ $# -gt 0 ]; then
	for _f in "$@"; do
		git ls-files --error-unmatch "$_f" >/dev/null 2>&1 \
			|| debt_unrunnable "$_f is not tracked — debt is measured against history, not the filesystem"
		printf '%s\n' "$_f"
	done >"$docs"
else
	git ls-files '*.md' >"$docs"
fi
[ -s "$docs" ] || debt_unrunnable "no tracked markdown to measure"

# --- The watermark -----------------------------------------------------------
#
# `%H %s` over first-parent history, newest first; the first subject matching
# `job/N: molt` wins. A squash merge is one commit on main, so first-parent is
# the whole story and never descends into a branch's own commits.
watermark=""
if [ -n "$since" ]; then
	watermark="$(git rev-parse "$since")"
	echo "molt-debt: measuring from --since $since ($(git rev-parse --short "$watermark"))"
else
	watermark="$(git log --first-parent --format='%H %s' \
		| awk '/^[0-9a-f]+ job\/[0-9]+: molt$/ { print $1; exit }')"
	if [ -n "$watermark" ]; then
		echo "molt-debt: last molt was $(git rev-parse --short "$watermark")"
	else
		echo "molt-debt: no \`job/N: molt\` commit in history — the corpus has NEVER been"
		echo "molt-debt:   molted, so every doc below is measured from nothing. That is the"
		echo "molt-debt:   honest answer, not a fault, and it is what picks the first molt's"
		echo "molt-debt:   ordering. Use --since <ref> to measure from a chosen point instead."
	fi
fi

# --- Net growth, in ONE whole-tree diff (trap 1) ------------------------------
#
# `<add>\t<del>\t<path>`, where a renamed path arrives as `old => new` and a
# brace form (`docs/{a => b}/x.md`) is normalised back to the new path. One row
# per doc, `<path at HEAD>\t<net lines>\t<path at the point>`, the third field
# empty when the doc never moved. Keyed by the HEAD path, which is what the
# population holds; the old path is what the point-side blob is read under.
growth="$work/growth"
: >"$growth"
if [ -n "$watermark" ]; then
	git diff -M --numstat "$watermark..HEAD" 2>/dev/null | awk -F '\t' '
		function newpath(p) {
			if (p ~ / => /) {
				if (p ~ /\{.* => .*\}/) {
					sub(/\{[^{}]* => /, "", p)
					sub(/\}/, "", p)
					gsub(/\/\//, "/", p)
					return p
				}
				sub(/^.* => /, "", p)
				return p
			}
			return p
		}
		function oldpath(p) {
			if (p !~ / => /) return ""
			if (p ~ /\{.* => .*\}/) {
				sub(/ => [^{}]*\}/, "", p)
				sub(/\{/, "", p)
				gsub(/\/\//, "/", p)
				return p
			}
			sub(/ => .*$/, "", p)
			return p
		}
		NF >= 3 && $1 != "-" { print newpath($3) "\t" ($1 - $2) "\t" oldpath($3) }
	' >"$growth" || : >"$growth"
fi

growth_field() { # <path> <field> -> that column, or "" when the diff never mentioned it
	awk -F '\t' -v p="$1" -v f="$2" '$1 == p { print $f; found = 1; exit } END { if (!found) print "" }' "$growth"
}

# --- Per-doc counts, at most two blobs ---------------------------------------
count_saga() { grep -cE '^## (Correction|Finding|Amendment)' 2>/dev/null || true; }
count_jobrefs() { grep -oE '(#|job )[0-9]{3,}' 2>/dev/null | grep -c . || true; }

blob_counts() { # <ref|""> <path> -> "<saga> <jobrefs>"; "0 0" when absent
	if [ -z "$1" ]; then
		echo "0 0"
		return 0
	fi
	if ! git cat-file -e "$1:$2" 2>/dev/null; then
		echo "0 0"
		return 0
	fi
	_b="$work/blob"
	git show "$1:$2" >"$_b" 2>/dev/null || : >"$_b"
	printf '%s %s\n' "$(count_saga <"$_b")" "$(count_jobrefs <"$_b")"
}

# A design is deletable when its Status leads with IMPLEMENTED and is not
# IMPLEMENTED IN PART (#533 D5 part 1). The remaining three parts of that test
# are check-molt.sh's; this only flags the candidate.
deletable() { # <path> -> rc 0 if a completely-implemented design
	case "$1" in
	docs/design/*.md) ;;
	*) return 1 ;;
	esac
	[ -f "$1" ] || return 1
	_s="$(awk '/^Status:/ { sub(/^Status:[[:space:]]*/, ""); print; exit }' "$1")"
	case "$_s" in
	"IMPLEMENTED IN PART"*) return 1 ;;
	IMPLEMENTED*) return 0 ;;
	*) return 1 ;;
	esac
}

rows="$work/rows"
: >"$rows"
short_wm=""
[ -n "$watermark" ] && short_wm="$(git rev-parse --short "$watermark")"

while IFS= read -r doc; do
	[ -n "$doc" ] || continue

	if [ -n "$watermark" ]; then
		lines="$(growth_field "$doc" 2)"
		# Absent from the diff means untouched since the watermark — 0, not blank.
		[ -n "$lines" ] || lines=0
		commits="$(git log --follow --oneline "$watermark..HEAD" -- "$doc" 2>/dev/null | grep -c . || true)"
		# A doc that moved is read at the point under the path it had THERE, so its
		# existing saga and jobrefs are not all counted as new (trap 1, columns 4-5).
		was="$(growth_field "$doc" 3)"
		[ -n "$was" ] || was="$doc"
		set -- $(blob_counts "$watermark" "$was")
		saga_before="${1:-0}"; refs_before="${2:-0}"
		since_col="$short_wm"
	else
		# Never molted: measured from nothing, so growth is the file itself.
		lines="$(git show "HEAD:$doc" 2>/dev/null | grep -c '' || true)"
		commits="$(git log --follow --oneline HEAD -- "$doc" 2>/dev/null | grep -c . || true)"
		saga_before=0; refs_before=0
		since_col="never"
	fi

	set -- $(blob_counts HEAD "$doc")
	saga_now="${1:-0}"; refs_now="${2:-0}"

	saga=$((saga_now - saga_before))
	refs=$((refs_now - refs_before))
	[ "$saga" -ge 0 ] || saga=0
	[ "$refs" -ge 0 ] || refs=0

	mark=""
	deletable "$doc" && mark="  [COMPLETE — deletable]"

	printf '%s\t%s\t%s\t%s\t%s\t%s%s\n' \
		"$lines" "$since_col" "$commits" "$saga" "$refs" "$doc" "$mark" >>"$rows"
done <"$docs"

# --- Report ------------------------------------------------------------------
echo
printf '%8s  %-9s %7s %6s %9s  %s\n' "+LINES" "SINCE" "COMMITS" "+SAGA" "+JOBREFS" "DOC"
sort -t "$(printf '\t')" -k1,1nr -k6,6 "$rows" | awk -F '\t' '
	{ printf "%8s  %-9s %7s %6s %9s  %s\n", $1, $2, $3, $4, $5, $6 }
'

# The milestone signal: what became deletable, and the reading it licenses.
del_n="$(grep -c 'COMPLETE — deletable' "$rows" || true)"
del_lines="$(awk -F '\t' '$6 ~ /COMPLETE — deletable/ { n += $1 } END { print n + 0 }' "$rows")"
echo
if [ "$del_n" -gt 0 ]; then
	echo "molt-debt: NOW COMPLETE — $del_n design(s), $del_lines line(s) deletable."
	echo "molt-debt:   Status alone is D5 part 1. The other three parts (every slice"
	echo "molt-debt:   landed, no surviving doc citing it, every non-doc citation stubbed)"
	echo "molt-debt:   are .chug/tasks/check-molt.sh's, which judges the deletion itself."
else
	echo "molt-debt: no design is completely IMPLEMENTED — nothing is deletable yet."
fi
echo "molt-debt: advisory only, and there is no threshold here on purpose — a number"
echo "molt-debt:   nobody calibrated becomes noise or a target. Read the top rows."
exit 0
