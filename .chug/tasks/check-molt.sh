#!/bin/sh
# The molt accounting gate — design #533 S2, the stage-0 command evaluator of
# `.chug/jobs/molt.yaml`.
#
# It cannot ask "was anything lost", because a molt loses the saga BY DESIGN and
# a gate asking that question fails every molt. So it asks whether the books
# balance: five things that are wrong however good the prose is.
#
#   1. A landed-slice claim that VANISHED. `**Landed** (job #N)` in a design
#      table is a claim about merged history; a molt may delete the whole doc,
#      but it may not quietly drop or alter the row while keeping the doc.
#   2. A surviving doc whose LAST inbound reference went with the deletion. It is
#      still true and nothing can reach it, which #415 D15 calls unreachable
#      however true it is. Non-zero at base and zero at HEAD is the finding;
#      zero at both is pre-existing and not this molt's doing.
#   3. A deleted design that was not ELIGIBLE (#533 D5, the four-part test).
#   4. A deleted path still CITED from code, config or generated output with no
#      stub left at it. Those citations are contracts — #415 S9's precedent.
#   5. A deletion with no `.chug/molt-ledger` line, so no named class licensed
#      it. The ledger is read from the lines the diff ADDS, exactly as
#      `.chug/doc-reread` is: a rebase cannot destroy the assertion, and a line
#      already merged never becomes a standing waiver.
#
# It reuses `check-doc-facts.sh --emit-paths` and `doc-lint.sh --emit-links`
# rather than re-deriving either — the same join `doc-staleness.sh` makes, and
# the reason a doc's referrers have one definition in this tree instead of three.
#
# A base it cannot materialise exits 2 as a LINTER ERROR. A loss gate that could
# not run must never read as "nothing lost", which is the one way this script
# can do real damage.
set -eu

molt_unrunnable() { # <line>...
	for _l in "$@"; do echo "!!! check-molt: $_l"; done
	echo "!!!     This is a LINTER ERROR, not a balanced molt: the books went unread."
	exit 2
}

command -v git >/dev/null 2>&1 || molt_unrunnable "no \`git\` on PATH — the molt is judged against history"
command -v awk >/dev/null 2>&1 || molt_unrunnable "no \`awk\` on PATH — the sets cannot be joined"

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] || molt_unrunnable "not inside a git work tree — nothing to compare"
cd "$root"

extractor=".chug/tasks/check-doc-facts.sh"
linker=".chug/tasks/doc-lint.sh"
ledger=".chug/molt-ledger"
for _need in "$extractor" "$linker"; do
	[ -f "$_need" ] || molt_unrunnable "$_need is missing — the referrer sets have no extractor"
done

# --- The base -----------------------------------------------------------------
#
# Same computation as `.chug/tasks/ci.sh` and `check-comments.sh`: HEAD against
# the merge-base with origin/$BASE_BRANCH, which is what an evaluation run gets.
# Unlike those two, a missing base is fatal here rather than a widening to the
# whole tree: every check below is a BEFORE-and-AFTER, so there is no
# conservative fallback that still answers the question.
base=""
if [ -n "${BASE_BRANCH:-}" ] \
	&& git fetch origin "$BASE_BRANCH:refs/remotes/origin/$BASE_BRANCH" >/dev/null 2>&1; then
	base="$(git merge-base HEAD "origin/$BASE_BRANCH" 2>/dev/null || true)"
fi
if [ -z "$base" ] && [ -n "${BASE_BRANCH:-}" ]; then
	base="$(git merge-base HEAD "$BASE_BRANCH" 2>/dev/null || true)"
fi
[ -n "$base" ] || molt_unrunnable \
	"could not resolve a base to compare against (BASE_BRANCH=${BASE_BRANCH:-unset})." \
	"    Every check here is a before-and-after, so there is no safe fallback:" \
	"    widening to the whole tree would answer a different question quietly."

echo "check-molt: comparing HEAD against $base (BASE_BRANCH=${BASE_BRANCH:-unset})"

work="$(mktemp -d)"
base_tree="$work/base"
trap 'cd "$root" 2>/dev/null || :; git worktree remove --force "$base_tree" >/dev/null 2>&1 || :; rm -rf "$work"' EXIT

git worktree add --detach "$base_tree" "$base" >/dev/null 2>&1 \
	|| molt_unrunnable "could not materialise $base in a worktree — the base corpus is unreadable"

findings=0
finding() { # <line>...
	for _l in "$@"; do echo "!!! check-molt: $_l"; done
	findings=$((findings + 1))
}

# --- The diff -----------------------------------------------------------------
deleted="$work/deleted"
added_ledger="$work/added-ledger"
git diff --name-only --diff-filter=D "$base"..HEAD >"$deleted" 2>/dev/null || : >"$deleted"
git diff -U0 "$base"..HEAD -- "$ledger" 2>/dev/null \
	| sed -n 's/^+\([^+].*\)/\1/p' >"$added_ledger" || : >"$added_ledger"

deleted_designs="$work/deleted-designs"
grep '^docs/design/.*\.md$' "$deleted" >"$deleted_designs" 2>/dev/null || : >"$deleted_designs"

if [ ! -s "$deleted" ]; then
	echo "check-molt: no deletions in this diff — checks 3-5 have no subject"
fi

# --- Referrer sets, base and HEAD ---------------------------------------------
#
# `<referrer>\t<line>\t<target>` from both emitters, which is the shape
# doc-staleness.sh already joins. A doc's own rows and `docs/README.md`'s
# catalogue row are excluded: check 5 of check-doc-facts.sh gates that catalogue
# to hold a row for EVERY doc, so counting it would make the answer constant.
refs_of() { # <dir> -> stdout: <referrer>\t<target>
	(
		cd "$1" || exit 1
		{
			sh ./.chug/tasks/check-doc-facts.sh --emit-paths 2>/dev/null || :
			sh ./.chug/tasks/doc-lint.sh --emit-links 2>/dev/null || :
		} | awk -F '\t' '
			NF >= 3 && $1 != "docs/README.md" && $1 != $3 { print $1 "\t" $3 }
		' | sort -u
	)
}

base_refs="$work/base-refs"
head_refs="$work/head-refs"
refs_of "$base_tree" >"$base_refs" || molt_unrunnable \
	"the referrer extractors failed against the base tree — the before-set is unknown"
refs_of "$root" >"$head_refs" || molt_unrunnable \
	"the referrer extractors failed against HEAD — the after-set is unknown"

inbound_count() { # <refs-file> <target> -> stdout: count
	awk -F '\t' -v t="$2" '$2 == t { n++ } END { print n + 0 }' "$1"
}

# Who still names a DELETED path at HEAD. This cannot come from the emitters,
# and the reason is worth stating because it is not a bug in either: `--emit-paths`
# prints a claim only when it RESOLVES, deliberately, so that an unresolvable one
# is reported once by check 1 of check-doc-facts.sh instead of twice. A path this
# molt just deleted resolves nowhere, so it is invisible to that set by design.
#
# So this is a literal search over tracked files, which also reaches the citers
# check-doc-facts.sh never scans: it reads `*.md` only, and a citation sitting in
# a `.ts`, `.yaml` or generated file is exactly the contract #415 S9 is about.
# The ledger is excluded because it names every shed path as its whole purpose.
citers_at_head() { # <deleted-path> <md|nonmd> -> stdout: space-separated files
	git grep -l --fixed-strings -- "$1" 2>/dev/null \
		| awk -v want="$2" -v self="$1" -v led="$ledger" '
			$0 == self || $0 == led { next }
			{ is_md = ($0 ~ /\.md$/) }
			(want == "md" && is_md) || (want == "nonmd" && !is_md) { print }
		' | sort -u | tr '\n' ' '
}

# --- Check 1: a landed-slice claim may not vanish -----------------------------
#
# Only for docs that still exist at HEAD. A deleted doc takes its rows with it
# legitimately (#533 D3), and the eligibility test in check 3 is what judges
# that deletion instead.
landed_of() { # <dir> -> stdout: <doc>\t<job#>
	(
		cd "$1" || exit 1
		git ls-files 'docs/design/*.md' 2>/dev/null | while IFS= read -r _d; do
			[ -f "$_d" ] || continue
			awk -v doc="$_d" '
				/^[[:space:]]*\|/ {
					t = $0
					while (match(t, /(Landed|Shipped)[*_ \t]*\(job #[0-9]+/)) {
						m = substr(t, RSTART, RLENGTH)
						sub(/^.*#/, "", m)
						print doc "\t" m
						t = substr(t, RSTART + RLENGTH)
					}
				}
			' "$_d"
		done | sort -u
	)
}

# The unlanded slice states, spelled once because two checks below read them and
# a molt is irreversible if they disagree. It is check 3 of check-doc-facts.sh's
# set, normalized the way `unlanded_word` normalizes a cell: lowercased, and with
# the interior space of `not started` closed up along with every other.
unlanded_states=' proposed planned deferred pending intent notstarted notlanded inprogress '

# The awk half of the same, interpolated into both programs that need it.
unlanded_awk='
	function unlanded_word(c) {
		gsub(/[*_`[:space:]]/, "", c)
		c = tolower(c)
		return (c != "" && index(states, " " c " ") > 0) ? c : ""
	}
'

# A slice table is a markdown row whose cells hold a recognised slice state —
# landed or not.
has_slice_table() { # <path> -> rc 0 if the doc still carries one
	[ -f "$1" ] || return 1
	awk -v states="$unlanded_states" "$unlanded_awk"'
		/^[[:space:]]*\|/ {
			if ($0 ~ /(Landed|Shipped)[*_ \t]*\(job #[0-9]+/) { found = 1; exit }
			n = split($0, cell, "|")
			for (i = 2; i <= n; i++) {
				if (unlanded_word(cell[i]) != "") { found = 1; exit }
			}
		}
		END { exit(found ? 0 : 1) }
	' "$1"
}

base_landed="$work/base-landed"
head_landed="$work/head-landed"
landed_of "$base_tree" >"$base_landed" 2>/dev/null || : >"$base_landed"
landed_of "$root" >"$head_landed" 2>/dev/null || : >"$head_landed"

while IFS="$(printf '\t')" read -r _doc _job; do
	[ -n "${_doc:-}" ] || continue
	# The doc itself is gone: legitimate, and check 3 judges it.
	grep -qx "$_doc" "$deleted" 2>/dev/null && continue
	# Reduced to a STUB — no slice table left at all. Also legitimate: a stub
	# exists because a non-doc path is a contract (#415 S9), and demanding it
	# keep the table it was shed to remove would make the stub pointless. What
	# check 1 is for is a row silently REWRITTEN — `**Landed** (job #N)` turned
	# back into `Proposed` — which leaves the table in place.
	has_slice_table "$_doc" || continue
	if ! grep -qx "$(printf '%s\t%s' "$_doc" "$_job")" "$head_landed" 2>/dev/null; then
		finding \
			"$_doc: the landed claim for job #$_job is gone, but the doc is not." \
			"    A slice row is a claim about merged history, and a molt does not" \
			"    unmake a merge. Delete the whole doc (D3) or keep the row."
	fi
done <"$base_landed"

# --- Check 2: no surviving doc may lose its last referrer ---------------------
while IFS= read -r _doc; do
	[ -n "${_doc:-}" ] || continue
	case "$_doc" in
	docs/*.md | docs/*/*.md | docs/*/*/*.md) ;;
	*) continue ;;
	esac
	grep -qx "$_doc" "$deleted" 2>/dev/null && continue
	_before="$(inbound_count "$base_refs" "$_doc")"
	[ "$_before" -gt 0 ] || continue
	_after="$(inbound_count "$head_refs" "$_doc")"
	if [ "$_after" -eq 0 ]; then
		finding \
			"$_doc: had $_before inbound reference(s) at the base and has none now." \
			"    Unreachable is not the same as untrue (#415 D15): the doc survived" \
			"    and every citation of it went with the deletions. Repoint one."
	fi
done <<EOF
$(git ls-files 'docs/*.md' 2>/dev/null)
EOF

# --- Check 3: a deleted design must have been eligible ------------------------
#
# #533 D5's four parts. Parts 1 and 2 are read from the BASE copy of the doc,
# because at HEAD there is nothing to read.
while IFS= read -r _doc; do
	[ -n "${_doc:-}" ] || continue
	_src="$base_tree/$_doc"
	[ -f "$_src" ] || continue

	_status="$(awk '/^Status:/ { sub(/^Status:[[:space:]]*/, ""); print; exit }' "$_src")"
	case "$_status" in
	IMPLEMENTED\ IN\ PART*)
		finding \
			"$_doc: deleted while its Status is \`IMPLEMENTED IN PART\`." \
			"    D3 licenses deleting a COMPLETELY implemented design, precisely" \
			"    because that needs no exception to append-only. This one is not."
		;;
	IMPLEMENTED*) ;;
	*)
		finding \
			"$_doc: deleted while its Status is \`${_status:-<none>}\`." \
			"    Only a design whose Status leads with IMPLEMENTED may be deleted."
		;;
	esac

	# Part 2: no slice row may still be unlanded, checked only in a doc that
	# carries a landed claim somewhere. That narrowing is per DOC where check 3
	# of check-doc-facts.sh makes it per TABLE, so an unrelated table in a
	# deleted design can trip this one — fail-safe, and a deletion is the act
	# that deserves the stricter of the two.
	if grep -qE '(Landed|Shipped)[*_ \t]*\(job #[0-9]+' "$_src"; then
		_unlanded="$(awk -v states="$unlanded_states" "$unlanded_awk"'
			/^[[:space:]]*\|/ {
				n = split($0, cell, "|")
				for (i = 2; i <= n; i++) {
					w = unlanded_word(cell[i])
					if (w != "") { print w }
				}
			}
		' "$_src" | sort -u | tr '\n' ' ')"
		if [ -n "$_unlanded" ]; then
			finding \
				"$_doc: deleted with slice row(s) still unlanded ($_unlanded)." \
				"    Every slice must read \`**Landed** (job #N)\` before the doc goes;" \
				"    an unlanded slice is a design still owed, not a design spent."
		fi
	fi

	# Part 3: no surviving tracked *.md may still cite it.
	_md_citers="$(citers_at_head "$_doc" md)"
	if [ -n "$_md_citers" ]; then
		finding \
			"$_doc: deleted, but still cited by surviving doc(s): $_md_citers" \
			"    The citation exists because that doc needed the fact, so the fact" \
			"    lands in a reference doc and the citation repoints there."
	fi
done <"$deleted_designs"

# --- Check 4: a deleted path cited from non-doc needs a stub ------------------
while IFS= read -r _doc; do
	[ -n "${_doc:-}" ] || continue
	_non_md="$(citers_at_head "$_doc" nonmd)"
	[ -n "$_non_md" ] || continue
	if [ ! -e "$_doc" ]; then
		finding \
			"$_doc: deleted with no stub, but still named by non-doc file(s): $_non_md" \
			"    A path named from code, config or generated output is a contract" \
			"    (#415 S9). Generated files cannot be hand-edited and their source" \
			"    is in crates/, so leave a stub at the path instead."
	fi
done <"$deleted"

# --- Check 5: every deletion needs a ledger line ------------------------------
if [ -s "$deleted" ]; then
	while IFS= read -r _doc; do
		[ -n "${_doc:-}" ] || continue
		if ! grep -qF "$_doc" "$added_ledger" 2>/dev/null; then
			finding \
				"$_doc: deleted with no line added to $ledger." \
				"    Every shed names the class that licensed it. The ledger is read" \
				"    from the lines this diff ADDS, so a line already merged is not" \
				"    an answer and a rebase cannot destroy a real one."
		fi
	done <"$deleted"
fi

# --- Verdict ------------------------------------------------------------------
_del_n="$(awk 'END { print NR + 0 }' "$deleted")"
_led_n="$(awk 'END { print NR + 0 }' "$added_ledger")"
if [ "$findings" -eq 0 ]; then
	echo "check-molt: balanced ($_del_n deletion(s), $_led_n ledger line(s) added)"
	exit 0
fi
echo "!!! check-molt: $findings finding(s) — the molt's books do not balance."
echo "!!!     None of these is a judgement about prose: each is a fact the diff"
echo "!!!     asserts and the tree contradicts. docs/design/533-molt.md D3-D5 are"
echo "!!!     the rules, and .chug/tasks/review-molt.md judges the aim separately."
exit 1
