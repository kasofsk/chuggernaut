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
# real finding — the branch edited the doc and THEN changed a file the doc names
# — and it clears the way the author would fix it anyway, by re-reading the doc
# and committing it again. The hook gets no `--gate`: before the commit the
# staged doc's last commit is still the old one, so every staged doc would be
# suspect and no edit could clear it inside that commit. A block nobody can
# clear except with `--no-verify` is how a ledger gets turned off.
#
# Usage:
#   .chug/tasks/doc-staleness.sh                  # every tracked *.md, one line per suspect doc
#   .chug/tasks/doc-staleness.sh --staged         # the staged *.md, every suspect path
#   .chug/tasks/doc-staleness.sh <file>...        # explicit, repo-relative, every suspect path
#   .chug/tasks/doc-staleness.sh --gate <file>... # whole-tree counts; details only the listed docs
#
# Exit: 0 = ran (suspicions are advisory). 1 = `--gate` and a diff-touched doc
# is suspect. 2 = the ledger could not run — a LINTER ERROR, never a clean tree.
#
# Test: .chug/tasks/doc-staleness.test.sh
set -eu
LC_ALL=C
export LC_ALL

HERE="$(cd "$(dirname "$0")" && pwd)"

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

# --- Mode --------------------------------------------------------------------
# `detail` is off for the whole tree only: 30 suspect docs listing every newer
# file is a wall nobody reads, so the tree run prints the newest mover per doc
# and names how many it summarised. Re-run on one doc to see all of them.
gate_list="$(mktemp)"
ts_index="$(mktemp)"
claims="$(mktemp)"
trap 'rm -f "$gate_list" "$ts_index" "$claims"' EXIT
: >"$gate_list"

mode="tree"
detail=0
gate=0
case "${1:-}" in
--gate)
	shift
	mode="gate"
	gate=1
	detail=1
	for f in "$@"; do
		case "$f" in
		*.md) printf '%s\n' "$f" >>"$gate_list" ;;
		esac
	done
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
awk -F'\t' -v tsf="$ts_index" -v gatef="$gate_list" -v detail="$detail" -v gate="$gate" '
	# Docs are ranked by the GAP — how long the newest mover has sat unread —
	# and never by the date of that mover. Ranking by date puts everything this
	# repo touched today at the top, which on ~15 jobs a day is the churn and
	# not the signal; the gap sinks it and floats the doc nobody has opened in a
	# fortnight. Nothing is filtered by it: the gap is printed on every row so a
	# reader draws their own line rather than inheriting a constant from here.
	function days(sec) { return int(sec / 86400) }
	function report(doc,   i, n, shown) {
		n = cnt[doc]
		printf "  %s  (last commit %s, %d day(s) behind) — %d newer file(s):\n",
			doc, ds[doc], days(gap[doc]), n
		shown = detail ? n : 1
		for (i = 1; i <= n && i <= shown; i++)
			printf "      %s  %s  (%s:%s)\n", sdate[doc, i], spath[doc, i], doc, sline[doc, i]
		if (n > shown)
			printf "      ... and %d more — .chug/tasks/doc-staleness.sh %s lists them\n", n - shown, doc
	}
	FILENAME == tsf { ts[$1] = $2 + 0; ds[$1] = $3; next }
	FILENAME == gatef { gated[$0] = 1; next }
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
		if (ts[path] - ts[doc] > gap[doc]) gap[doc] = ts[path] - ts[doc]
	}
	END {
		total = 0; sus = 0; blocked = 0; aged = 0
		for (d in docs) {
			total++
			if (!(d in cnt)) continue
			sus++
			if (gap[d] >= 86400) aged++
			if (d in gated) blocked++
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
			if (blocked == 0) exit 0
			for (d in cnt) if (d in gated) order[d] = gap[d] + 1
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
			printf "!!!     The branch changed a file the doc names AFTER it last touched the doc,\n"
			printf "!!!     so nothing in this change has re-read one against the other. Re-read it\n"
			printf "!!!     and commit the doc again — that clears the row. (design #415 D7)\n"
			exit 1
		}
		exit 0
	}
' "$ts_index" "$gate_list" "$claims"
