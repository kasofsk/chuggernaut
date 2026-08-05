#!/bin/sh
# Shell test for doc-staleness.sh — no NATS, no Docker.
#
# Both halves of the ledger are commit times, so every case runs inside a
# throwaway `git init` repo whose history is written with explicit
# GIT_COMMITTER_DATE: a fixture that committed everything "now" could not
# express "the file moved after the doc did", which is the only thing this
# script decides. The suite is skipped whole if git is absent.
#
# The SUT resolves its path extractor beside itself, so the fixture repos need
# no `.chug/` of their own — they exercise the real
# .chug/tasks/check-doc-facts.sh --emit-paths, which is the point of S6 sharing
# check 1 rather than copying it.
#
# Run:  .chug/tasks/doc-staleness.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/doc-staleness.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
check() { # <name> <expected-rc> <actual-rc> <output-file> <must-contain>
	name="$1"; want="$2"; got="$3"; out="$4"; needle="$5"
	if [ "$got" = "$want" ] && grep -qF "$needle" "$out"; then
		echo "ok   - $name (rc=$got)"
		pass=$((pass + 1))
	else
		echo "FAIL - $name: rc want=$want got=$got; expected output to contain: $needle"
		echo "----- output -----"; cat "$out"; echo "------------------"
		fail=$((fail + 1))
	fi
}
check_silent() { # <name> <actual-rc> <output-file> <must-NOT-contain>
	name="$1"; got="$2"; out="$3"; needle="$4"
	if [ "$got" = "0" ] && ! grep -qF "$needle" "$out"; then
		echo "ok   - $name (rc=$got)"
		pass=$((pass + 1))
	else
		echo "FAIL - $name: rc got=$got; expected output NOT to contain: $needle"
		echo "----- output -----"; cat "$out"; echo "------------------"
		fail=$((fail + 1))
	fi
}

if ! command -v git >/dev/null 2>&1; then
	echo "skip - every case (git unavailable)"
	echo
	echo "passed 0, failed 0"
	exit 0
fi

# --- The fixture history ------------------------------------------------------
# Three dated commits, in the only order that can express the question:
#   2026-01-01  the source files
#   2026-01-02  the docs that name them
#   2026-01-03  `mover.rs` changes and `gone.rs` is deleted
# So `docs/suspect.md` names a file that moved AFTER it, `docs/quiet.md` names
# one that did not, and `docs/absent.md` names one that stopped existing.
REPO="$WORK/repo"
mkdir -p "$REPO/crates/pkg/src" "$REPO/docs"
git -C "$REPO" -c init.defaultBranch=main init -q

commit_at() { # <iso-date> <message>
	GIT_AUTHOR_DATE="$1T12:00:00+0000" GIT_COMMITTER_DATE="$1T12:00:00+0000" \
		git -C "$REPO" -c user.email=t@e -c user.name=t commit -qm "$2" >/dev/null 2>&1
}

printf 'pub const A: u32 = 1;\n' > "$REPO/crates/pkg/src/quiet.rs"
printf 'pub const B: u32 = 1;\n' > "$REPO/crates/pkg/src/mover.rs"
printf 'pub const C: u32 = 1;\n' > "$REPO/crates/pkg/src/gone.rs"
git -C "$REPO" add . >/dev/null 2>&1
commit_at 2026-01-01 sources

{ printf '# Quiet\n\n'; printf 'Reads `crates/pkg/src/quiet.rs`.\n'; } > "$REPO/docs/quiet.md"
{ printf '# Suspect\n\n'; printf 'Reads `crates/pkg/src/mover.rs`.\n'; } > "$REPO/docs/suspect.md"
{ printf '# Absent\n\n'; printf 'Reads `crates/pkg/src/gone.rs`.\n'; } > "$REPO/docs/absent.md"
{ printf '# Dir\n\n'; printf 'Everything under `crates/pkg/src/`.\n'; } > "$REPO/docs/dir.md"
{ printf '# Bare\n\n'; printf 'No path claims here at all.\n'; } > "$REPO/docs/bare.md"
{ printf '# Marked\n\n'; printf 'Reads `crates/pkg/src/mover.rs`. <!-- intent -->\n'; } > "$REPO/docs/marked.md"
git -C "$REPO" add . >/dev/null 2>&1
commit_at 2026-01-02 docs

printf 'pub const B: u32 = 2;\n' > "$REPO/crates/pkg/src/mover.rs"
git -C "$REPO" rm -q crates/pkg/src/gone.rs >/dev/null 2>&1
git -C "$REPO" add . >/dev/null 2>&1
commit_at 2026-01-03 mover-moves

run_sut() { # <arg>... -> writes rc to $RC, output to $OUT
	OUT="$WORK/out"
	set +e
	(cd "$REPO" && "$SUT" "$@") >"$OUT" 2>&1
	RC=$?
	set -e
}

# --- The four fixtures the ledger is specified by -----------------------------

# 1. A file that moved AFTER the doc: suspect, and the report names the file.
run_sut
check "a file newer than the doc makes it suspect" 0 "$RC" "$OUT" "docs/suspect.md"
check "the suspect row names WHICH file moved" 0 "$RC" "$OUT" "crates/pkg/src/mover.rs"
check "the suspect row names WHEN it moved" 0 "$RC" "$OUT" "2026-01-03"
check "the row names the doc line the claim is on" 0 "$RC" "$OUT" "(docs/suspect.md:3)"

# 2. A file older than the doc: silent.
check_silent "a file older than the doc is silent" "$RC" "$OUT" "docs/quiet.md"

# 3. A path that no longer exists is check 1's finding, not a second report here.
check_silent "a vanished path is not double-reported" "$RC" "$OUT" "docs/absent.md"

# 4. A doc naming no paths: silent.
check_silent "a doc with no path claims is silent" "$RC" "$OUT" "docs/bare.md"

# --- The narrowing, and the markers -------------------------------------------

# 5. A directory claim has no single history — `crates/pkg/src/` is newer than
#    every doc the moment anything under it changes, so it is not judged.
check_silent "a directory claim is not a staleness signal" "$RC" "$OUT" "docs/dir.md"

# 6. The doc-claim markers suppress the ledger exactly as they suppress check 1,
#    because the path set IS check 1's.
check_silent "a marked line is out of the path set" "$RC" "$OUT" "docs/marked.md"

# 7. Suspicion is never phrased as a verdict.
check "the report says suspicion is not wrongness" 0 "$RC" "$OUT" "SUSPECT IS NOT WRONG"

# --- Scoping ------------------------------------------------------------------

# 8. One doc, explicitly: every newer file, not just the newest.
run_sut docs/suspect.md
check "explicit mode judges just that doc" 0 "$RC" "$OUT" "crates/pkg/src/mover.rs"

# 9. A doc whose claims are all older reports clean rather than saying nothing.
run_sut docs/quiet.md
check "a clean doc says so" 0 "$RC" "$OUT" "no doc is suspect"

# 10. A doc with no claims at all is counted as having none, not as clean-ish.
run_sut docs/bare.md
check "a doc with no file claims is read and reported clean" 0 "$RC" "$OUT" \
	"0 doc(s) with file claims"

# --- `--gate`: the only place it blocks ---------------------------------------

# 11. A suspect doc that THIS diff edits is the one blocking finding.
run_sut --gate docs/suspect.md
check "--gate fails on a diff-touched suspect doc" 1 "$RC" "$OUT" \
	"edited by this diff and still suspect"

# 12. A doc the diff edits that is NOT suspect passes.
run_sut --gate docs/quiet.md
check "--gate passes a diff-touched doc that is current" 0 "$RC" "$OUT" \
	"doc(s) with file claims are suspect"

# 13. A diff touching no markdown gates on nothing, and still reports the counts.
run_sut --gate
check "--gate with no docs is a report, not a finding" 0 "$RC" "$OUT" \
	"reads the whole ledger"

# 14. A suspect doc the diff does NOT touch is never a blocking finding — that
#     is history nobody in this commit caused.
run_sut --gate docs/quiet.md
check_silent "an untouched suspect doc does not block" "$RC" "$OUT" \
	"edited by this diff"

# --- A prerequisite that is missing is loud, and is not a pass ----------------

# 15. Outside a git checkout there are no commit times to compare, so the ledger
#     refuses — exit 2, a LINTER ERROR, distinct from "nothing is suspect".
NONGIT="$WORK/nongit"
mkdir -p "$NONGIT"
printf '# Plain\n\nCites `crates/pkg/src/quiet.rs`.\n' > "$NONGIT/plain.md"
OUT="$WORK/out"
set +e
(cd "$NONGIT" && "$SUT" plain.md) >"$OUT" 2>&1
RC=$?
set -e
check "non-git root is a LINTER ERROR, not a clean ledger" 2 "$RC" "$OUT" \
	"not a git checkout"
check "the LINTER ERROR says the docs went unread" 2 "$RC" "$OUT" \
	"the docs went unread"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
