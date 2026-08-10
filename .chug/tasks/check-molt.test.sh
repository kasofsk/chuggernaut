#!/bin/sh
# Shell test for check-molt.sh — no NATS, no Docker (design #533 S2).
#
# Every check in the gate is a BEFORE-and-AFTER, so each case needs a repo with
# real history: a base commit holding the pre-molt corpus, then a second commit
# doing the shedding, with BASE_BRANCH pointing at the first. That is why these
# fixtures are more than a file drop — a `git init` plus two commits is the
# smallest thing that can express the question the gate asks.
#
# The gate reuses check-doc-facts.sh --emit-paths and doc-lint.sh --emit-links,
# so both are copied into every fixture beside the script under test. A fixture
# missing them would exercise the LINTER ERROR path instead of the check, which
# is a real case here and has its own assertion rather than being an accident.
#
# Run:  .chug/tasks/check-molt.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/check-molt.sh"
FACTS="$HERE/check-doc-facts.sh"
LINT="$HERE/doc-lint.sh"

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

check_absent() { # <name> <expected-rc> <actual-rc> <output-file> <must-NOT-contain>
	name="$1"; want="$2"; got="$3"; out="$4"; needle="$5"
	if [ "$got" = "$want" ] && ! grep -qF "$needle" "$out"; then
		echo "ok   - $name (rc=$got)"
		pass=$((pass + 1))
	else
		echo "FAIL - $name: rc want=$want got=$got; expected output NOT to contain: $needle"
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

# --- Fixture construction -----------------------------------------------------
#
# new_repo <name> populates a repo with the gate and its two extractors, a
# minimal docs/README.md catalogue (check 5 of check-doc-facts.sh gates it, and
# the gate under test excludes it from referrer counting), and commits it as the
# base on branch `main`.
new_repo() { # <name> -> echoes the path
	_r="$WORK/$1"
	mkdir -p "$_r/.chug/tasks" "$_r/docs/design" "$_r/docs/reference"
	cp "$SUT" "$_r/.chug/tasks/check-molt.sh"
	cp "$FACTS" "$_r/.chug/tasks/check-doc-facts.sh"
	cp "$LINT" "$_r/.chug/tasks/doc-lint.sh"
	chmod +x "$_r/.chug/tasks/"*.sh
	: >"$_r/.chug/molt-ledger"
	printf '%s\n' "# Docs" "" "| Doc | What |" "| --- | --- |" >"$_r/docs/README.md"
	echo "$_r"
}

commit_base() { # <repo>
	(
		cd "$1"
		git init -q -b main .
		git config user.email t@t; git config user.name t
		git add -A && git commit -qm base
	)
}

# The molt commit must sit on its OWN branch. On `main` the merge-base with
# BASE_BRANCH is HEAD itself, so the diff is empty and every deletion check goes
# quiet — which is how the first draft of this suite got five false passes.
commit_molt() { # <repo>
	(
		cd "$1"
		git checkout -q -b job/molt
		git add -A && git commit -qm molt
	)
}

run_gate() { # <repo> -> $RC, output in $OUT
	OUT="$WORK/out"
	set +e
	(cd "$1" && BASE_BRANCH=main sh ./.chug/tasks/check-molt.sh) >"$OUT" 2>&1
	RC=$?
	set -e
}

# A design doc that is completely implemented and fully landed — the shape D5
# says may be deleted. `job #1` needs no real merge here: the gate reads the row
# to judge eligibility, and resolving it against history is check 3 of
# check-doc-facts.sh, a different gate.
eligible_design() { # <path>
	printf '%s\n' \
		"# Design #900 — A spent design" \
		"" \
		"Status: IMPLEMENTED" \
		"" \
		"| Slice | What | Gate on |" \
		"| --- | --- | --- |" \
		"| S1 | the whole thing | **Landed** (job #1) |" \
		>"$1"
}

catalogue_row() { # <repo> <doc-path>
	printf '| [`%s`](%s) | a doc |\n' "$2" "${2#docs/}" >>"$1/docs/README.md"
}

# --- Case 1: a diff with no deletions balances --------------------------------
R="$(new_repo clean)"
printf '%s\n' "# Ref" "" "A page." >"$R/docs/reference/thing.md"
catalogue_row "$R" "docs/reference/thing.md"
commit_base "$R"
printf '%s\n' "# Ref" "" "A page, edited." >"$R/docs/reference/thing.md"
commit_molt "$R"
run_gate "$R"
check "a molt with no deletions balances" 0 "$RC" "$OUT" "balanced"

# --- Case 2: an unresolvable base is a LINTER ERROR, never a clean bill -------
R="$(new_repo nobase)"
commit_base "$R"
OUT="$WORK/out"
set +e
(cd "$R" && unset BASE_BRANCH; sh ./.chug/tasks/check-molt.sh) >"$OUT" 2>&1
RC=$?
set -e
check "no BASE_BRANCH exits 2 as a LINTER ERROR" 2 "$RC" "$OUT" "LINTER ERROR"
# The needle is the success LINE, not the word: the LINTER ERROR text says "not a
# balanced molt", so a bare "balanced" matches the very message being asserted
# against and the case would pass while proving nothing.
check_absent "an unrunnable gate never says balanced" 2 "$RC" "$OUT" "check-molt: balanced ("

# --- Case 3: a missing extractor is a LINTER ERROR ----------------------------
R="$(new_repo noextractor)"
commit_base "$R"
rm -f "$R/.chug/tasks/doc-lint.sh"
run_gate "$R"
check "a missing extractor exits 2" 2 "$RC" "$OUT" "LINTER ERROR"

# --- Case 4: check 1 — a landed row may not vanish from a surviving doc -------
R="$(new_repo landed_dropped)"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
commit_base "$R"
printf '%s\n' \
	"# Design #900 — A spent design" \
	"" \
	"Status: IMPLEMENTED" \
	"" \
	"| Slice | What | Gate on |" \
	"| --- | --- | --- |" \
	"| S1 | the whole thing | Proposed |" \
	>"$R/docs/design/900-spent.md"
commit_molt "$R"
run_gate "$R"
check "check 1 catches a dropped landed claim" 1 "$RC" "$OUT" "the landed claim for job #1 is gone"

# --- Case 5: check 1 does NOT fire when the whole doc is deleted --------------
#
# The deletion takes its rows with it legitimately (D3); check 3 judges it.
R="$(new_repo landed_with_doc)"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
commit_base "$R"
rm -f "$R/docs/design/900-spent.md"
sed -i.bak '/900-spent/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
printf 'shed docs/design/900-spent.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check_absent "deleting the doc is not a dropped landed claim" 0 "$RC" "$OUT" "the landed claim"

# --- Case 6: check 3 — an IMPLEMENTED IN PART design may not be deleted -------
R="$(new_repo in_part)"
printf '%s\n' \
	"# Design #901 — Half built" \
	"" \
	"Status: IMPLEMENTED IN PART" \
	"" \
	"| Slice | What | Gate on |" \
	"| --- | --- | --- |" \
	"| S1 | done | **Landed** (job #1) |" \
	>"$R/docs/design/901-half.md"
catalogue_row "$R" "docs/design/901-half.md"
commit_base "$R"
rm -f "$R/docs/design/901-half.md"
sed -i.bak '/901-half/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
printf 'shed docs/design/901-half.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check "check 3 refuses to delete IMPLEMENTED IN PART" 1 "$RC" "$OUT" "IMPLEMENTED IN PART"

# --- Case 7: check 3 — an unlanded slice row blocks the deletion --------------
#
# One fixture, three spellings, because the gate licenses an irreversible act: a
# state it fails to recognise is a slice still owed whose doc goes anyway. The
# rows cover the canonical word, one of the two states an earlier draft dropped
# outright, and one written lowercase with emphasis and an interior space.
R="$(new_repo unlanded)"
printf '%s\n' \
	"# Design #902 — Owed" \
	"" \
	"Status: IMPLEMENTED" \
	"" \
	"| Slice | What | Gate on |" \
	"| --- | --- | --- |" \
	"| S1 | done | **Landed** (job #1) |" \
	"| S2 | not done | Proposed |" \
	"| S3 | later | Not landed |" \
	"| S4 | now | **in progress** |" \
	>"$R/docs/design/902-owed.md"
catalogue_row "$R" "docs/design/902-owed.md"
commit_base "$R"
rm -f "$R/docs/design/902-owed.md"
sed -i.bak '/902-owed/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
printf 'shed docs/design/902-owed.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check "check 3 catches an unlanded slice" 1 "$RC" "$OUT" "still unlanded"
check "check 3 reads the canonical Proposed" 1 "$RC" "$OUT" "proposed"
check "check 3 reads Not landed" 1 "$RC" "$OUT" "notlanded"
check "check 3 reads a lowercase emphasised state" 1 "$RC" "$OUT" "inprogress"

# --- Case 8: check 5 — a deletion with no ledger line -------------------------
R="$(new_repo noledger)"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
commit_base "$R"
rm -f "$R/docs/design/900-spent.md"
sed -i.bak '/900-spent/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
commit_molt "$R"
run_gate "$R"
check "check 5 catches a deletion with no ledger line" 1 "$RC" "$OUT" "no line added to"

# --- Case 9: check 5 reads the DIFF, not the file -----------------------------
#
# A ledger line already present at the base is not an answer: it would make a
# merged line a standing waiver, which is the mistake .chug/doc-reread exists to
# avoid. Same deletion as case 8, with the line committed in the base.
R="$(new_repo staleledger)"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
printf 'shed docs/design/900-spent.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_base "$R"
rm -f "$R/docs/design/900-spent.md"
sed -i.bak '/900-spent/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
commit_molt "$R"
run_gate "$R"
check "a pre-existing ledger line is not a waiver" 1 "$RC" "$OUT" "no line added to"

# --- Case 10: check 3 — a surviving doc still citing the deleted design ------
R="$(new_repo still_cited)"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
printf '%s\n' "# Ref" "" "See \`docs/design/900-spent.md\` for the argument." \
	>"$R/docs/reference/thing.md"
catalogue_row "$R" "docs/reference/thing.md"
commit_base "$R"
rm -f "$R/docs/design/900-spent.md"
sed -i.bak '/900-spent/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
printf 'shed docs/design/900-spent.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check "check 3 catches a surviving doc still citing it" 1 "$RC" "$OUT" "still cited by surviving doc"

# --- Case 11: check 4 — a non-doc citation needs a stub ----------------------
#
# The citer must NOT be markdown. A `.md` file anywhere — including a prompt
# outside `docs/` — is check 3's business, and check-doc-facts.sh check 1 sees it
# too. Check 4 exists for the citers nothing else scans, so the fixture uses a
# YAML config, which is the shape that actually motivated it.
R="$(new_repo needs_stub)"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
mkdir -p "$R/.chug/jobs"
printf '%s\n' "name: thing" "# argued in docs/design/900-spent.md" \
	>"$R/.chug/jobs/thing.yaml"
commit_base "$R"
rm -f "$R/docs/design/900-spent.md"
sed -i.bak '/900-spent/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
printf 'shed docs/design/900-spent.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check "check 4 catches a non-doc citation with no stub" 1 "$RC" "$OUT" "still named by non-doc file"

# --- Case 12: a stub at the path satisfies check 4 ---------------------------
R="$(new_repo stubbed)"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
mkdir -p "$R/.chug/jobs"
printf '%s\n' "name: thing" "# argued in docs/design/900-spent.md" \
	>"$R/.chug/jobs/thing.yaml"
commit_base "$R"
printf '%s\n' "# Design #900 — deleted by a molt" "" "Status: IMPLEMENTED" "" \
	"This design was shed. Its argument is in the reference tier." \
	>"$R/docs/design/900-spent.md"
printf 'shed docs/design/900-spent.md body — class: saga; survivor: docs/reference\n' \
	>>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check_absent "a stub at the path satisfies check 4" 0 "$RC" "$OUT" "still named by non-doc file"

# --- Case 13: check 2 — a surviving doc losing its LAST referrer -------------
#
# `docs/reference/orphan.md` survives and is true; the only thing that cited it
# went with the deletion, so nothing can reach it (#415 D15). The catalogue row
# does not count — check 5 of check-doc-facts.sh gates docs/README.md to hold a
# row for every doc, so counting it would make the answer constant.
R="$(new_repo lost_referrer)"
printf '%s\n' "# Orphan" "" "A true page." >"$R/docs/reference/orphan.md"
catalogue_row "$R" "docs/reference/orphan.md"
printf '%s\n' \
	"# Design #900 — A spent design" \
	"" \
	"Status: IMPLEMENTED" \
	"" \
	"| Slice | What | Gate on |" \
	"| --- | --- | --- |" \
	"| S1 | the whole thing | **Landed** (job #1) |" \
	"" \
	"The rule now lives in \`docs/reference/orphan.md\`." \
	>"$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
commit_base "$R"
rm -f "$R/docs/design/900-spent.md"
sed -i.bak '/900-spent/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
printf 'shed docs/design/900-spent.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check "check 2 catches a doc losing its last referrer" 1 "$RC" "$OUT" "and has none now"

# --- Case 14: check 2 does NOT fire on a doc that was ALREADY unreferenced ----
#
# Zero at the base and zero at HEAD is pre-existing, not this molt's doing.
# Blaming a molt for it is how a gate earns a reputation for noise.
R="$(new_repo already_orphan)"
printf '%s\n' "# Orphan" "" "Nothing cited this before either." \
	>"$R/docs/reference/orphan.md"
catalogue_row "$R" "docs/reference/orphan.md"
eligible_design "$R/docs/design/900-spent.md"
catalogue_row "$R" "docs/design/900-spent.md"
commit_base "$R"
rm -f "$R/docs/design/900-spent.md"
sed -i.bak '/900-spent/d' "$R/docs/README.md" && rm -f "$R/docs/README.md.bak"
printf 'shed docs/design/900-spent.md — class: saga; survivor: none\n' >>"$R/.chug/molt-ledger"
commit_molt "$R"
run_gate "$R"
check_absent "a pre-existing orphan is not the molt's finding" 0 "$RC" "$OUT" "and has none now"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
