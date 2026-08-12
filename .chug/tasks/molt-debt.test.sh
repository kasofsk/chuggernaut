#!/bin/sh
# Shell test for molt-debt.sh — no NATS, no Docker (design #533 S3).
#
# The reason this suite exists is one case: A DOC THAT MOVED. The reader's whole
# correctness risk is the pathspec trap in #533's 2026-08-10 correction — limiting
# a diff to a doc's current path drops the old path's deletion, so no rename pair
# exists and a moved doc reads as a total rewrite. That failure is silent and
# plausible: it reports a large number where a large number is expected. So the
# rename case asserts an EXACT figure, and asserts the wrong one is absent, rather
# than checking the script merely ran.
#
# Every fixture therefore needs real history — a `git init`, a `job/N: molt`
# commit to serve as the watermark, and commits after it including a `git mv`.
#
# Run:  .chug/tasks/molt-debt.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/molt-debt.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
check() { # <name> <expected-rc> <actual-rc> <output-file> <must-contain>
	name="$1"; want="$2"; got="$3"; out="$4"; needle="$5"
	if [ "$got" = "$want" ] && grep -qF -- "$needle" "$out"; then
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
	if [ "$got" = "$want" ] && ! grep -qF -- "$needle" "$out"; then
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

new_repo() { # <name> -> echoes the path
	_r="$WORK/$1"
	mkdir -p "$_r/.chug/tasks" "$_r/docs/design"
	cp "$SUT" "$_r/.chug/tasks/molt-debt.sh"
	chmod +x "$_r/.chug/tasks/molt-debt.sh"
	(
		cd "$_r"
		git init -q -b main .
		git config user.email t@t
		git config user.name t
	)
	echo "$_r"
}

commit() { # <repo> <message>
	(cd "$1" && git add -A && git commit -q -m "$2")
}

# A molt commit is only ever a reference POINT in these fixtures, so it is
# allowed to be empty. `commit` stays strict on purpose: a fixture that meant to
# change a file and did not should fail loudly rather than measure nothing.
molt_commit() { # <repo> <message>
	(cd "$1" && git add -A && git commit -q --allow-empty -m "$2")
}

run() { # <repo> [args...] -> $RC, output in $OUT
	OUT="$WORK/out"
	_r="$1"; shift
	set +e
	(cd "$_r" && sh ./.chug/tasks/molt-debt.sh "$@") >"$OUT" 2>&1
	RC=$?
	set -e
}

# The report prints `%8s  %-9s %7s %6s %9s  %s`; this is its tail from +SAGA on,
# so a column assertion is spaced by the same rule the script uses rather than by
# hand-counting.
cells() { # <+saga> <+jobrefs> <doc>
	printf '%6s %9s  %s' "$1" "$2" "$3"
}

lines() { # <count> -> that many numbered lines on stdout
	i=1
	while [ "$i" -le "$1" ]; do
		echo "line $i"
		i=$((i + 1))
	done
}

# --- Case 1: no molt commit — the `never` path -------------------------------
R="$(new_repo never)"
printf '%s\n' "# Ref" "" "A page." >"$R/docs/thing.md"
commit "$R" "job/1: docs"
run "$R"
check "no molt commit says NEVER been molted" 0 "$RC" "$OUT" "NEVER been"
check "the never path labels SINCE as never" 0 "$RC" "$OUT" "never"

# --- Case 2: a molt commit is discovered as the watermark --------------------
R="$(new_repo watermark)"
printf '%s\n' "# Ref" "" "Before the molt." >"$R/docs/thing.md"
commit "$R" "job/1: docs"
printf '%s\n' "# Ref" "" "Molted." >"$R/docs/thing.md"
molt_commit "$R" "job/2: molt"
WM="$(cd "$R" && git rev-parse --short HEAD)"
{ echo "# Ref"; echo; echo "Molted."; lines 40; } >"$R/docs/thing.md"
commit "$R" "job/3: docs"
run "$R"
check "a job/N: molt commit becomes the watermark" 0 "$RC" "$OUT" "last molt was $WM"
check "growth is measured from the watermark" 0 "$RC" "$OUT" "      40  $WM"

# --- Case 3: THE RENAME CASE ------------------------------------------------
#
# `docs/moved.md` is 100 lines at the molt, gains 10, and is then `git mv`d. Its
# true net growth is 10. The pathspec bug reports ~110 — the whole file, as if it
# had been written from scratch. Both halves are asserted, because the wrong
# answer is a plausible-looking large number rather than an error.
#
# The same trap has two more columns. The doc already carries one `## Correction`
# and one `#999` at the molt, so its true `+SAGA`/`+JOBREFS` are both 0; a reader
# that looks the doc up at the watermark under its HEAD path finds no blob and
# reports 1 and 1 — absent read as never-existed.
R="$(new_repo renamed)"
SAGA_LINE="## Correction, 2026-01-01 — already there (job #999)"
{ echo "$SAGA_LINE"; lines 99; } >"$R/docs/old-name.md"
commit "$R" "job/1: docs"
molt_commit "$R" "job/2: molt"
WM="$(cd "$R" && git rev-parse --short HEAD)"
{ echo "$SAGA_LINE"; lines 99; lines 10; } >"$R/docs/old-name.md"
commit "$R" "job/3: docs"
(cd "$R" && git mv docs/old-name.md docs/new-name.md)
commit "$R" "job/4: docs"
run "$R"
check "a moved doc reports its TRUE growth" 0 "$RC" "$OUT" "      10  $WM"
check_absent "a moved doc is not reported as a total rewrite" 0 "$RC" "$OUT" "     110  $WM"
check "the moved doc is keyed by its path at HEAD" 0 "$RC" "$OUT" "docs/new-name.md"
check "a moved doc's existing saga and jobrefs are not new" \
	0 "$RC" "$OUT" "$(cells 0 0 docs/new-name.md)"
check_absent "a moved doc's point-side blob is not read as absent" \
	0 "$RC" "$OUT" "$(cells 1 1 docs/new-name.md)"

# --- Case 4: --since overrides the discovered watermark ---------------------
R="$(new_repo since)"
printf '%s\n' "# Ref" "" "Base." >"$R/docs/thing.md"
commit "$R" "job/1: docs"
BASE="$(cd "$R" && git rev-parse --short HEAD)"
{ echo "# Ref"; echo; echo "Base."; lines 7; } >"$R/docs/thing.md"
commit "$R" "job/2: docs"
run "$R" --since "$BASE"
check "--since names the point it measured from" 0 "$RC" "$OUT" "measuring from --since $BASE"
check "--since measures growth from that point" 0 "$RC" "$OUT" "       7  $BASE"

# --- Case 5: a named-file run measures only that file ----------------------
R="$(new_repo named)"
printf '%s\n' "# A" >"$R/docs/a.md"
printf '%s\n' "# B" >"$R/docs/b.md"
commit "$R" "job/1: docs"
run "$R" docs/a.md
check "a named run includes the named doc" 0 "$RC" "$OUT" "docs/a.md"
check_absent "a named run excludes every other doc" 0 "$RC" "$OUT" "docs/b.md"

# --- Case 6: the deletable marker, both directions -------------------------
R="$(new_repo deletable)"
printf '%s\n' "# Design #900 — Spent" "" "Status: IMPLEMENTED" >"$R/docs/design/900-spent.md"
printf '%s\n' "# Design #901 — Owed" "" "Status: IMPLEMENTED IN PART" >"$R/docs/design/901-owed.md"
printf '%s\n' "# Design #902 — Argued" "" "Status: PROPOSED" >"$R/docs/design/902-argued.md"
commit "$R" "job/1: design"
run "$R"
check "a completely IMPLEMENTED design is marked deletable" 0 "$RC" "$OUT" "900-spent.md  [COMPLETE"
check_absent "IMPLEMENTED IN PART is not marked deletable" 0 "$RC" "$OUT" "901-owed.md  [COMPLETE"
check_absent "PROPOSED is not marked deletable" 0 "$RC" "$OUT" "902-argued.md  [COMPLETE"
check "the deletable total is reported" 0 "$RC" "$OUT" "NOW COMPLETE — 1 design(s)"

# --- Case 7: nothing deletable says so ------------------------------------
R="$(new_repo nodelete)"
printf '%s\n' "# Design #901 — Owed" "" "Status: IMPLEMENTED IN PART" >"$R/docs/design/901-owed.md"
commit "$R" "job/1: design"
run "$R"
check "no deletable design is stated, not implied by silence" 0 "$RC" "$OUT" "nothing is deletable yet"

# --- Case 8: advisory — debt present, still exit 0 ------------------------
#
# One heading of each spelling and one mention of each jobref shape, so the
# alternations in count_saga and count_jobrefs are pinned by the asserted cells
# rather than merely executed: +SAGA and +JOBREFS are both 3 against a watermark
# where the doc had neither.
R="$(new_repo advisory)"
printf '%s\n' "# Ref" >"$R/docs/thing.md"
commit "$R" "job/1: docs"
molt_commit "$R" "job/2: molt"
{
	echo "# Ref"
	lines 500
	echo "## Correction, 2026-01-01 — a thing (job #999)"
	echo "## Finding, 2026-01-02 — another (job 888)"
	echo "## Amendment, 2026-01-03 — a third (#777)"
} >"$R/docs/thing.md"
commit "$R" "job/3: docs"
run "$R"
check "heavy debt still exits 0" 0 "$RC" "$OUT" "advisory only"
check "new saga sections and job references are counted" \
	0 "$RC" "$OUT" "$(cells 3 3 docs/thing.md)"
check_absent "there is no threshold or recommendation" 0 "$RC" "$OUT" "recommended"

# --- Case 9: unrunnable is a LINTER ERROR, never a clean corpus -----------
R="$(new_repo badsince)"
printf '%s\n' "# Ref" >"$R/docs/thing.md"
commit "$R" "job/1: docs"
run "$R" --since no-such-ref
check "an unresolvable --since exits 2" 2 "$RC" "$OUT" "LINTER ERROR"
check_absent "an unrunnable read never prints a table" 2 "$RC" "$OUT" "+JOBREFS"

run "$R" --since
check "--since with no ref exits 2" 2 "$RC" "$OUT" "--since needs a ref"

R="$(new_repo untracked)"
printf '%s\n' "# Ref" >"$R/docs/thing.md"
commit "$R" "job/1: docs"
printf '%s\n' "# Nope" >"$R/docs/untracked.md"
run "$R" docs/untracked.md
check "an untracked file exits 2 rather than measuring the filesystem" 2 "$RC" "$OUT" "is not tracked"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
