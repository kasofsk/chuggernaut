#!/bin/sh
# The staleness ledger — design #415 D7, slice S6. For each tracked `*.md`, the
# set of tree files it names; if any of those files has a commit NEWER than the
# doc's own last commit, the doc is SUSPECT.
#
# SUSPECT IS NOT WRONG, and the wording holds that line everywhere below. A doc
# is listed because the code it names moved after it did, which is very often
# completely fine — the ledger says "nobody has re-read this since" and nothing
# more. #415 M7 is what an accusatory gate costs: a check whose output reads as
# a verdict is a check everyone learns to scroll past.
#
# It reaches the class no syntactic check can. `.chug/tasks/check-doc-facts.sh`
# answers "is this claim false right now"; it cannot answer "has the thing this
# doc describes moved since anyone looked". That second question is what M1
# (`crates/domain/src/state.rs` moved and seven files kept naming the old path)
# and M3 (`version.rs` bumped three times under thirteen docs) both were —
# neither was a broken claim at the moment it happened.
#
# ENTIRELY DERIVED, and that is the point. No `last-verified:` front matter, no
# dates in prose, nothing an author maintains and nothing that can itself go
# stale: git already knows when the doc changed and when the file changed.
#
# THE PATH SET IS CHECK 1'S, NOT A SECOND ANSWER. It comes from
# `check-doc-facts.sh --emit-paths`, which is check 1's extractor with the
# verdict removed — same backticked tokens, same false-positive classes refused,
# same `<!-- intent -->` / `<!-- runtime -->` / `<!-- absent -->` suppression,
# same resolution against `git ls-files` rather than the filesystem. Two
# implementations of "what paths does this doc name" is the duplication this
# program exists to remove. A claim that does NOT resolve is omitted there and
# so is silent here: it is already check 1's finding, and double-reporting it
# would train a reader to ignore both.
#
# ONLY A FILE CLAIM IS JUDGED, and this is the narrowing the measurement forced.
# A directory's "last commit" is the newest commit under an open set, so
# `docs/`, `.chug/` and `web/` are newer than almost every doc almost always —
# a predicate that is nearly constant-true is not a signal. Measured whole-tree
# on 2026-08-05 over 69 tracked docs: counting directory claims, 35 of the 61
# docs that make a claim came back suspect on 177 of 1,967 claims, and the top
# offenders were all reading "`docs/design` moved" (any design doc edited). File
# claims only: 30 of 61 on 104 of 1,718, and every top line names a specific
# source file. Same reason
# check 1 keeps directory claims and this does not: existence is a fact about a
# directory, movement is not.
#
# ADVISORY, NOT AN ERROR GATE, and that is why it is its own script rather than
# a fourth check in `check-doc-facts.sh`. That file's contract is that a
# non-zero exit means a doc states something false; suspicion is not falsity,
# and folding a maybe into a gate whose every other finding is a certainty
# devalues the certainties. Separate scripts, one shared extractor.
#
# WHERE IT BLOCKS, AND WHY THAT IS ALMOST NEVER. `--gate` takes the docs the
# current diff touches and exits 1 if one of them is suspect. In a job branch
# that set is nearly always empty by construction: editing a doc makes it the
# newest thing in the comparison. What survives is the one ordering that is a
# real finding — the branch edited the doc and THEN changed a file the doc names.
# The hook gets no `--gate`: before the commit the
# staged doc's last commit is still the old one, so every staged doc would be
# suspect and no edit could clear it inside that commit. A block nobody can
# clear except with `--no-verify` is how a ledger gets turned off.
#
# THE BLOCK CLEARS ON AN ASSERTED RE-READ, NOT ON A TIMESTAMP (job #471). A
# `Doc-reread: <path>` line in a commit message between `--since <ref>` and HEAD
# clears exactly the doc it names, and nothing else — it is an assertion per
# doc, not a blanket waiver, so a branch owing three re-reads writes three
# lines. This gate used to print "commit the doc again" as its remedy, which
# satisfies the ordering without satisfying the purpose: D7 wants the doc
# RE-READ against the change, and a timestamp cannot express attention. A
# trailer can be written falsely, like every reviewer-facing rule; the
# difference is that it is then a visible false statement in a commit message
# rather than an invisible whitespace edit. With no `--since` there is no branch
# whose messages to read, so the block stands — a caller that omits the argument
# cannot silently stop enforcing.
#
# A COMMIT MESSAGE DOES NOT SURVIVE A REBASE, WHICH IS WHY THERE IS A SECOND
# ROUTE (job #482). A job branch is rebased on every merge-conflict rework, and
# a resolution that squashes or re-authors a commit takes the trailer with it —
# so a doc that was genuinely re-read blocks again, for a reason that has
# nothing to do with the doc. Recovering the lost line is not possible where it
# matters: every work and evaluation container is a fresh `git clone
# --single-branch --filter=blob:none` (`crates/container/src/lib.rs`), so there
# is no reflog of the rebase and the orphaned commits were never fetched. What a
# fresh clone does have is the TREE. So a `Doc-reread: <path>` line that THIS
# BRANCH'S DIFF ADDS to `.chug/doc-reread` clears the same one doc, and content
# is what a rebase, a squash and a re-author all carry through.
#
# ADDED-BY-THIS-DIFF IS THE WHOLE OF THE FILE'S MEANING, and it is what keeps
# the second route from becoming a waiver. The line is read from
# `git diff <since>...HEAD`, never from the file's contents, so a line already
# on the base branch asserts nothing about this change and a merged assertion
# goes inert the moment it lands. Nothing prunes the file and nothing has to:
# every line in it is dead except the ones a diff is currently adding, which is
# also why the file is a scratch slate rather than a registry and may be
# rewritten wholesale by whichever branch next needs it.
#
# A `*.md` MOVER NEVER BLOCKS, and that is what makes the block above always
# clearable (job #454). Only a `*.md` file makes claims, so a doc is the only
# thing that can be BOTH sides of the relation — and two docs that name each
# other are a cycle with no fixed point except one commit holding both, which is
# a squash and not something a rework can reach. Job #449 and job #453 both hit
# it and both cleared it by squashing. Dropping `.md` from the blocking side
# leaves the relation doc → non-doc, which is acyclic by construction: acting on
# a flagged doc clears its row and can flip no other, because the doc so touched
# is itself a `.md` and so never a blocking mover. A cross-reference is also the
# weaker claim — one doc LINKING another is a pointer, not a statement about its
# content — and it stays in the advisory ledger, where a near-constant-true
# predicate costs a row a reader skims rather than a build.
#
# THE SECOND AXIS IS REACH, AND IT IS REPORTED FIRST (design #415 D15, slice
# S12). Everything above asks whether a doc is still true; the orphan half asks
# whether anyone can find it. Per doc under `docs/`, the number of other tracked
# `*.md` naming it — zero is a finding, non-zero is silent. Advisory for D7's
# own reason, and because an orphan is often correct: a `PROPOSED` design doc is
# uncited by construction until the work it proposes starts.
#
# THE CATALOGUE DOES NOT COUNT, and that single decision is what keeps this half
# alive. Check 5 gates `docs/README.md` to carry a row for every tracked
# `docs/**/*.md` in both directions, so if a row were a reference then no doc
# could ever be an orphan and this would report a constant. The two checks are a
# pair over one population: check 5 answers "is it catalogued", which has a
# mechanical right answer and therefore blocks, and this answers "is it read",
# which does not. So `docs/README.md` is excluded as a REFERRER — the whole
# file, index and routing prose alike — while remaining a doc that is judged.
#
# TWO ROUTES, NEITHER RE-DERIVED HERE. A backticked path claim comes from check
# 1 via `check-doc-facts.sh --emit-paths`, the same set the staleness half
# joins. A relative markdown link comes from `doc-lint.sh --emit-links`, rule
# 2's extractor with the verdict removed. Both are needed and the measurement
# says so: on the tree at this slice, path claims alone reported **7 of the 41**
# docs orphaned — #308, #310, #313, #322, #355, #372, #373 — every one of them
# false, because `docs/design/` cites its siblings as `[#313](./313-….md)` and a
# link target is not backticked. Both routes together: **0 of 41**. That is the
# honest reading of a corpus whose only recorded orphans, M4's
# `spec_original.md` and M8's #323, were deleted and cited respectively; the
# value here is as a ratchet against the next one.
#
# ONLY `docs/` IS JUDGED, and only markdown counts as a referrer. The other 32
# tracked `*.md` are reached by machinery rather than by citation — a prompt or
# task named by path from `.chug/jobs/*.yaml`, a `crates/platform-ops/templates/`
# file copied wholesale into a new project, an Xcode fixture README — so
# "nothing cites it" is not evidence about any of them, and judging the whole
# population reported 11 findings with no true positive among them — 7 once
# #415's correction had named four of the eleven by path, which is the same
# point from the other side. `docs/` is
# also exactly check 5's population, which is what makes the pair a pair.
# Referrers are every tracked `*.md`, prompts included: a doc reached only from
# `.chug/prompts/work/design.md` is read on every design job and is no orphan.
#
# Usage:
#   .chug/tasks/doc-staleness.sh                  # every tracked *.md, one line per suspect doc
#   .chug/tasks/doc-staleness.sh --staged         # the staged *.md, every suspect path
#   .chug/tasks/doc-staleness.sh <file>...        # explicit, repo-relative, every suspect path
#   .chug/tasks/doc-staleness.sh --gate [--since <ref>] <file>...
#                                                 # whole-tree counts; details only the listed docs,
#                                                 # cleared by a Doc-reread: assertion since <ref> —
#                                                 # a commit-message trailer, or a line the diff adds
#                                                 # to .chug/doc-reread
#
# The orphan half runs in the two whole-tree modes only. Reach is a property of
# the whole tree, so a scoped run cannot answer it, and `--staged` is the hook's
# ~2s budget.
#
# Exit: 0 = ran (suspicions are advisory). 1 = `--gate` and a diff-touched doc
# is suspect. 2 = the ledger could not run — a LINTER ERROR, never a clean tree.
#
# Test: .chug/tasks/doc-staleness.test.sh
set -eu
LC_ALL=C
export LC_ALL

HERE="$(cd "$(dirname "$0")" && pwd)"
reread_file=".chug/doc-reread"

ledger_unrunnable() { # <line>...
	for _l in "$@"; do echo "!!! doc-staleness: $_l"; done
	echo "!!!     This is a LINTER ERROR, not a clean ledger: the docs went unread."
	exit 2
}

command -v git >/dev/null 2>&1 || ledger_unrunnable "no \`git\` on PATH — the ledger is derived from history"
command -v awk >/dev/null 2>&1 || ledger_unrunnable "no \`awk\` on PATH — the ledger cannot be joined"

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] || ledger_unrunnable \
	"not a git checkout — both halves of the comparison are commit times, so" \
	"    outside a checkout there is nothing to compare."
cd "$root" || ledger_unrunnable "cannot enter the repo root $root"

extractor="$HERE/check-doc-facts.sh"
[ -x "$extractor" ] || ledger_unrunnable \
	"$extractor is missing or not executable — the ledger reads check 1's path" \
	"    extractor rather than carrying a second one."

linker="$HERE/doc-lint.sh"
[ -x "$linker" ] || ledger_unrunnable \
	"$linker is missing or not executable — the orphan half reads rule 2's link" \
	"    extractor rather than carrying a second one."

# --- Mode --------------------------------------------------------------------
# `detail` is off for the whole tree only: 30 suspect docs listing every newer
# file is a wall nobody reads, so the tree run prints the newest mover per doc
# and names how many it summarised. Re-run on one doc to see all of them.
gate_list="$(mktemp)"
reread_list="$(mktemp)"
ts_index="$(mktemp)"
claims="$(mktemp)"
orphan_pop="$(mktemp)"
orphan_refs="$(mktemp)"
trap 'rm -f "$gate_list" "$reread_list" "$ts_index" "$claims" "$orphan_pop" "$orphan_refs"' EXIT
: >"$gate_list"
: >"$reread_list"

mode="tree"
detail=0
gate=0
case "${1:-}" in
--gate)
	shift
	mode="gate"
	gate=1
	detail=1
	_want_since=""
	_since=""
	for f in "$@"; do
		if [ -n "$_want_since" ]; then
			_since="$f"
			_want_since=""
			continue
		fi
		case "$f" in
		--since) _want_since=1 ;;
		*.md) printf '%s\n' "$f" >>"$gate_list" ;;
		esac
	done
	[ -z "$_want_since" ] || {
		echo "doc-staleness: --since needs a ref" >&2
		exit 2
	}
	if [ -n "$_since" ]; then
		# A base that does not resolve reads every assertion as absent, which is
		# the failure this route exists to stop — so it is a LINTER ERROR and not
		# a quiet block.
		git rev-parse --verify --quiet "$_since^{commit}" >/dev/null 2>&1 || ledger_unrunnable \
			"--since $_since does not name a commit — with no base there is no" \
			"    branch whose assertions to read, so every re-read would read as absent."
		# Route 1: the branch's commit messages, where an author asserting a
		# re-read is making a visible statement (job #471).
		git log "$_since..HEAD" --format=%B 2>/dev/null |
			sed -n 's/^[[:space:]]*Doc-reread:[[:space:]]*//p' |
			sed 's/[[:space:]]*$//' |
			grep -v '^$' >>"$reread_list" || :
		# Route 2: the same line, ADDED BY THIS DIFF to `.chug/doc-reread`. Read
		# from the diff and never from the file, so what the base already carried
		# asserts nothing (job #482).
		git diff "$_since...HEAD" -- "$reread_file" 2>/dev/null |
			sed -n 's/^+[[:space:]]*Doc-reread:[[:space:]]*//p' |
			sed 's/[[:space:]]*$//' |
			grep -v '^$' >>"$reread_list" || :
	fi
	set --
	;;
--staged)
	mode="staged"
	detail=1
	;;
"") : ;;
*)
	mode="explicit"
	detail=1
	;;
esac

if [ "$mode" = "staged" ]; then
	set --
	population="$($extractor --emit-paths --staged)" || ledger_unrunnable \
		"check-doc-facts.sh --emit-paths --staged could not run"
else
	population="$($extractor --emit-paths "$@")" || ledger_unrunnable \
		"check-doc-facts.sh --emit-paths could not run"
fi
printf '%s\n' "$population" | grep -v '^$' >"$claims" || : >"$claims"

# --- The orphan half: who names this doc? ------------------------------------
# Reported before the staleness half so the `--gate` block, when there is one,
# stays the last thing on the screen. `$claims` is already the whole-tree path
# set in both of these modes, so only the link route costs a second call.
catalogue="docs/README.md"
if [ "$mode" = "tree" ] || [ "$mode" = "gate" ]; then
	git ls-files -- '*.md' 2>/dev/null | awk '/^docs\/.*\.md$/' >"$orphan_pop" || : >"$orphan_pop"
	{ cat "$claims"; "$linker" --emit-links 2>/dev/null || :; } >"$orphan_refs" || : >"$orphan_refs"
	awk -F'\t' -v popf="$orphan_pop" -v cat="$catalogue" '
		FILENAME == popf { pop[$0] = 1; order[++n] = $0; next }
		$1 == cat { next }
		{ if (($3 in pop) && $1 != $3) seen[$3] = 1 }
		END {
			orph = 0
			for (i = 1; i <= n; i++) if (!(order[i] in seen)) orph++
			if (n == 0) exit 0
			if (orph == 0) {
				printf "doc-staleness: all %d doc(s) under docs/ are named by something other than\n", n
				printf "doc-staleness:   the catalogue — no orphans (design #415 D15).\n"
				exit 0
			}
			printf "doc-staleness: %d of %d doc(s) under docs/ have ZERO inbound references:\n", orph, n
			for (i = 1; i <= n; i++) if (!(order[i] in seen)) printf "  %s — no inbound reference\n", order[i]
			printf "doc-staleness:   Nothing in the tree names them but %s, which carries a\n", cat
			printf "doc-staleness:   row for every doc by construction (check 5) and so is no evidence\n"
			printf "doc-staleness:   that a reader can reach one. UNREFERENCED IS NOT WRONG, and this\n"
			printf "doc-staleness:   is advisory like the rest of the ledger: a PROPOSED design doc is\n"
			printf "doc-staleness:   uncited until the work it proposes starts.\n"
		}
	' "$orphan_pop" "$orphan_refs"
fi

# --- The commit-time index both halves resolve against -----------------------
# One `git log` pass rather than one per path: ~70 docs and ~320 distinct paths
# would be hundreds of invocations, and the whole history walked once is 0.05s
# against 0.6s for the loop. `%cs` rides along so the report can name the date
# without a second call — mawk has no `strftime`, and mawk is what the gate's
# Debian container runs. A file changed ONLY inside a merge commit is not listed
# by `--name-only` and so reads as older than it is; this history squash-merges,
# and under-suspicion is the safe direction for an advisory anyway.
git log --no-renames --format='@@@%ct %cs' --name-only 2>/dev/null | awk '
	/^@@@/ { ct = substr($0, 4) + 0; ds = substr($0, 4); sub(/^[0-9]+ /, "", ds); next }
	NF == 0 { next }
	{ if (!($0 in t) || ct > t[$0]) { t[$0] = ct; d[$0] = ds } }
	END { for (p in t) print p "\t" t[p] "\t" d[p] }
' >"$ts_index" || : >"$ts_index"
[ -s "$ts_index" ] || ledger_unrunnable \
	"\`git log\` named no files — a shallow or empty history has no ledger to derive."

# --- Join --------------------------------------------------------------------
# A doc with no commit of its own (added but never committed) is skipped: it is
# newer than everything by construction, so there is nothing to suspect it of.
awk -F'\t' -v tsf="$ts_index" -v gatef="$gate_list" -v rrf="$reread_list" -v rrfile="$reread_file" -v detail="$detail" -v gate="$gate" '
	# Docs are ranked by the GAP — how long the newest mover has sat unread —
	# and never by the date of that mover. Ranking by date puts everything this
	# repo touched today at the top, which on ~15 jobs a day is the churn and
	# not the signal; the gap sinks it and floats the doc nobody has opened in a
	# fortnight. Nothing is filtered by it: the gap is printed on every row so a
	# reader draws their own line rather than inheriting a constant from here.
	function days(sec) { return int(sec / 86400) }
	function report(doc,   i, n, shown, mark) {
		n = cnt[doc]
		printf "  %s  (last commit %s, %d day(s) behind) — %d newer file(s):\n",
			doc, ds[doc], days(gap[doc]), n
		shown = detail ? n : 1
		for (i = 1; i <= n && i <= shown; i++) {
			mark = (gate && spath[doc, i] ~ /\.md$/) ? "  [cross-reference — not blocking]" : ""
			printf "      %s  %s%s  (%s:%s)\n", sdate[doc, i], spath[doc, i], mark, doc, sline[doc, i]
		}
		if (n > shown)
			printf "      ... and %d more — .chug/tasks/doc-staleness.sh %s lists them\n", n - shown, doc
	}
	FILENAME == tsf { ts[$1] = $2 + 0; ds[$1] = $3; next }
	FILENAME == gatef { gated[$0] = 1; next }
	FILENAME == rrf { attested[$0] = 1; next }
	{
		doc = $1; line = $2; path = $3
		if (!(doc in ts)) next
		if (!(path in ts)) next          # a directory claim has no single history
		docs[doc] = 1
		if (ts[path] <= ts[doc]) next
		n = ++cnt[doc]
		# Newest mover first within a doc, so the head of each list is the most
		# recent thing that moved under it.
		for (i = n; i > 1 && ts[spath[doc, i - 1]] < ts[path]; i--) {
			spath[doc, i] = spath[doc, i - 1]
			sline[doc, i] = sline[doc, i - 1]
			sdate[doc, i] = sdate[doc, i - 1]
		}
		spath[doc, i] = path; sline[doc, i] = line; sdate[doc, i] = ds[path]
		# The blocking half of the relation excludes a `*.md` mover, so it is
		# doc → non-doc and therefore acyclic. See the header.
		if (path !~ /\.md$/) hard[doc] = 1
		if (ts[path] - ts[doc] > gap[doc]) gap[doc] = ts[path] - ts[doc]
	}
	END {
		total = 0; sus = 0; blocked = 0; aged = 0; crossref = 0; cleared = 0
		for (d in docs) {
			total++
			if (!(d in cnt)) continue
			sus++
			if (gap[d] >= 86400) aged++
			if (!(d in gated)) continue
			if (d in hard) {
				if (d in attested) cleared++
				else blocked++
			} else crossref++
		}
		if (sus == 0) {
			printf "doc-staleness: no doc is suspect (%d doc(s) with file claims read).\n", total
			exit 0
		}
		printf "doc-staleness: %d of %d doc(s) with file claims are suspect — a file they name\n", sus, total
		printf "doc-staleness:   has moved since they last did. SUSPECT IS NOT WRONG: this is a\n"
		printf "doc-staleness:   reading list, not a finding (design #415 D7). %d sit a day or\n", aged
		printf "doc-staleness:   more behind, listed first; the rest is same-day churn.\n"
		# `--gate` details only what this diff edits: CI runs on every job, and
		# thirty rows of history nobody in this commit caused is how the header
		# stops being read. The whole list is one command away and named here.
		if (gate) {
			printf "doc-staleness:   .chug/tasks/doc-staleness.sh reads the whole ledger.\n"
			if (crossref > 0) {
				printf "doc-staleness:   %d doc(s) this diff edits are suspect only through another\n", crossref
				printf "doc-staleness:   *.md* — a cross-reference is a pointer, not a claim about the\n"
				printf "doc-staleness:   content it points at, and doc-names-doc is the one ordering no\n"
				printf "doc-staleness:   rework commit can clear. Not blocking (design #415 D7, job #454).\n"
			}
			if (cleared > 0) {
				printf "doc-staleness:   %d doc(s) carry a Doc-reread: assertion on this branch — the\n", cleared
				printf "doc-staleness:   author asserts they read it against the change. Cleared.\n"
			}
			if (blocked == 0) exit 0
			for (d in cnt)
				if ((d in gated) && (d in hard) && !(d in attested)) order[d] = gap[d] + 1
			sus = blocked
		} else {
			for (d in cnt) order[d] = gap[d] + 1
		}
		for (k = 0; k < sus; k++) {
			best = ""; bestv = -1
			# The doc name breaks a tie, so the report is byte-identical run to
			# run rather than following the hash order of awk.
			for (d in order)
				if (order[d] > bestv || (order[d] == bestv && d < best)) { bestv = order[d]; best = d }
			report(best)
			delete order[best]
		}
		if (blocked > 0) {
			printf "!!! doc-staleness: %d doc(s) above are edited by this diff and still suspect.\n", blocked
			printf "!!!     The branch changed a non-doc file the doc names AFTER it last touched\n"
			printf "!!!     the doc, so nothing in this change has re-read one against the other.\n"
			printf "!!!     Re-read it, then assert that you did — one line per doc, either as a\n"
			printf "!!!     trailer in a commit message on this branch, or as a line THIS DIFF ADDS\n"
			printf "!!!     to %s:\n", rrfile
			printf "!!!         Doc-reread: <path>\n"
			printf "!!!     ONLY THE FILE SURVIVES A REBASE. A rework that squashes or re-authors\n"
			printf "!!!     your commits takes the trailer with it and this block comes back, so if\n"
			printf "!!!     you wrote one and it is gone, that is what happened — assert it in the\n"
			printf "!!!     file instead. The file is read from the diff, never from its contents:\n"
			printf "!!!     a line the base already carried asserts nothing. An assertion says you\n"
			printf "!!!     LOOKED; committing the doc unchanged would satisfy a timestamp without\n"
			printf "!!!     satisfying that, which is what this gate used to accept.\n"
			printf "!!!     (design #415 D7, jobs #471 and #482)\n"
			exit 1
		}
		exit 0
	}
' "$ts_index" "$gate_list" "$reread_list" "$claims"
