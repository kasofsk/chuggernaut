#!/bin/sh
# Shell test for check-doc-facts.sh — no NATS, no Docker.
#
# The cases for both checks moved here with the checks themselves (design #415
# S1b): .chug/tasks/doc-lint.test.sh created them for check 1 (S1a) and grew
# check 2's (S1c), and a suite has to follow the code it pins. What they assert
# is unchanged apart from the verdict — a finding is now an ERROR, so the
# expected rc is 1 where it used to be 0.
#
# Both checks resolve against the index — `git ls-files` and `git grep`, never
# the filesystem — so the cases run inside throwaway `git init` repos that own
# both what is tracked and what the tree's constants are, and are skipped whole
# if git is absent. $REPO drives explicit-file mode; $TREE has a HEAD commit, so
# it can tell the whole-tree default from the hook's `--staged` scoping; $DESIGN
# owns the merge history check 3 resolves against, and $CONCEPTS owns a registry
# and the two docs check 4 needs to tell an owner from everyone else.
#
# Run:  .chug/tasks/check-doc-facts.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/check-doc-facts.sh"

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

# --- The explicit-file fixture ------------------------------------------------
# `crates/pkg/target/` is present on disk and NOT tracked, standing in for build
# output: that is the fresh-worktree-vs-built-checkout divergence the check
# exists to avoid, so it must report all the same.
REPO="$WORK/repo"
mkdir -p "$REPO/crates/pkg/src" "$REPO/crates/pkg/target" "$REPO/docs"
printf 'x\n' > "$REPO/crates/pkg/src/lib.rs"
printf 'x\n' > "$REPO/crates/pkg/target/built.rs"
git -C "$REPO" -c init.defaultBranch=main init -q
git -C "$REPO" add crates/pkg/src >/dev/null 2>&1 || true

run_in() { # <dir> <arg>... -> writes rc to $RC, output to $OUT
	OUT="$WORK/out"
	_d="$1"; shift
	set +e
	(cd "$_d" && "$SUT" "$@") >"$OUT" 2>&1
	RC=$?
	set -e
}
run_sut_repo() { run_in "$REPO" "$@"; }
write_doc() { # <name> <body-line>...
	name="$1"; shift
	{ printf '# Fixture\n\n'; for l in "$@"; do printf '%s\n' "$l"; done; } \
		> "$REPO/docs/$name"
	git -C "$REPO" add "docs/$name" >/dev/null 2>&1 || true
}

# --- Check 1: referenced paths ------------------------------------------------

# 1. A tracked file and a tracked directory claim both resolve.
write_doc tracked.md 'See `crates/pkg/src/lib.rs` and `crates/pkg/` for it.'
run_sut_repo docs/tracked.md
check "tracked file and directory claims resolve" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 2. Present on disk but untracked -> a finding. This is the property that keeps
#    the verdict identical in a fresh worktree and a built checkout.
write_doc untracked.md 'Build output lands in `crates/pkg/target/built.rs` here.'
run_sut_repo docs/untracked.md
check "untracked-but-present path fails (git, not the filesystem)" 1 "$RC" "$OUT" \
	"referenced path not found -> crates/pkg/target/built.rs"

# 3. The false-positive classes are skipped silently.
write_doc globs.md 'Every `crates/*/src/lib.rs` and `crates/**/*.rs` and `docs/?.md`.'
run_sut_repo docs/globs.md
check "globs are not path claims" 0 "$RC" "$OUT" "check-doc-facts: clean"

write_doc absolute.md 'Mounted at `/dev/kvm`, `/workspace/chug-output.tar.gz`, `~/.ssh/config`.'
run_sut_repo docs/absolute.md
check "absolute and home-relative paths are not this tree" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

write_doc templates.md 'Named `crates/{name}/src/lib.rs`, `crates/<name>.yaml`, `$ROOT/x.rs`, `crates/.../x.rs`.'
run_sut_repo docs/templates.md
check "placeholder templates are patterns, not claims" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

write_doc citation.md 'Defined at `crates/pkg/src/lib.rs:193` and `crates/pkg/src/lib.rs:42-79`.'
run_sut_repo docs/citation.md
check "path:line citation resolves on the file" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 4. A citation whose FILE is missing still fails — the suffix is stripped, the
#    file is not excused.
write_doc citation-stale.md 'Defined at `crates/gone/src/lib.rs:193`.'
run_sut_repo docs/citation-stale.md
check "path:line citation still checks the file" 1 "$RC" "$OUT" \
	"referenced path not found -> crates/gone/src/lib.rs:193"

# 5. A token rooted somewhere other than this checkout is refused, not judged.
write_doc foreign.md 'Beacon has `src/api/types.gen.ts` and `dispatcher/tests/execution.rs`.'
run_sut_repo docs/foreign.md
check "foreign-rooted paths are skipped, not failed" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 6. All three markers suppress the finding on the line that carries them.
write_doc markers.md \
	'Images come from `.chug/images.yaml`. <!-- intent -->' \
	'The bundle is built into `web/dist`. <!-- runtime -->' \
	'`docs/design/epics.md` was never written. <!-- absent -->'
run_sut_repo docs/markers.md
check "intent, runtime and absent markers suppress the finding" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 7. A marker is line-scoped: the unmarked line beside it still fails.
write_doc marker-scope.md \
	'Images come from `.chug/images.yaml`. <!-- intent -->' \
	'State lives in `crates/dispatcher/src/state.rs`.'
run_sut_repo docs/marker-scope.md
check "a marker does not leak to the next line" 1 "$RC" "$OUT" \
	"referenced path not found -> crates/dispatcher/src/state.rs"

# --- Check 2: constant values -------------------------------------------------
# The fixture owns the tree's side: `CONFIG_SCHEMA_EPOCH` is 7 here, so every
# claim of 6 below is a mismatch and every claim of 7 agrees. `MAX_BLOB_BYTES`
# is expression-valued and `DUPLICATE_EPOCH` resolves to two disagreeing consts
# — both are refusals to judge, not findings.
printf 'pub const CONFIG_SCHEMA_EPOCH: u32 = 7;\npub const MAX_BLOB_BYTES: usize = 16 * 1024;\n' \
	> "$REPO/crates/pkg/src/version.rs"
printf 'pub const DUPLICATE_EPOCH: u32 = 7;\n' > "$REPO/crates/pkg/src/one.rs"
printf 'pub const DUPLICATE_EPOCH: u32 = 8;\n' > "$REPO/crates/pkg/src/two.rs"
git -C "$REPO" add crates/pkg/src >/dev/null 2>&1 || true

# 8. Every recognised assertion shape, each disagreeing with the fixture: one
#    finding naming both values, and nothing else.
shape_n=0
for shape in \
	'The epoch `CONFIG_SCHEMA_EPOCH` is `6` today.' \
	'`CONFIG_SCHEMA_EPOCH` is currently 6.' \
	'`CONFIG_SCHEMA_EPOCH` is already **6** in the tree.' \
	'`CONFIG_SCHEMA_EPOCH` = **6** in the tree.' \
	'`CONFIG_SCHEMA_EPOCH` == 6 in the tree.' \
	'Bump `CONFIG_SCHEMA_EPOCH` (currently `6`) in the same commit.' \
	'| `CONFIG_SCHEMA_EPOCH` | 6 | the job-type schema epoch |' \
	'Version.rs holds `CONFIG_SCHEMA_EPOCH = 6` today.' \
	'Version.rs holds `pub const CONFIG_SCHEMA_EPOCH: u32 = 6;` today.'; do
	shape_n=$((shape_n + 1))
	write_doc "shape-$shape_n.md" "$shape"
	run_sut_repo "docs/shape-$shape_n.md"
	check "mismatched value fails: $shape" 1 "$RC" "$OUT" \
		"stale constant -> CONFIG_SCHEMA_EPOCH is 7 in the tree, not 6"
	check "mismatched value fails exactly once: $shape" 1 "$RC" "$OUT" \
		"1 stale constant claim(s)"
done

# 9. A claim that agrees with the tree is silent, in both shapes.
write_doc agrees.md \
	'The epoch `CONFIG_SCHEMA_EPOCH` is `7` today.' \
	'Version.rs holds `CONFIG_SCHEMA_EPOCH = 7`.'
run_sut_repo docs/agrees.md
check "a matching value passes silently" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 10. Everything that is not a value claim about today's tree stays silent. A
#     mention with no value is the class that must never fire (#415 M7) — and it
#     matters more now the verdict is fatal; the rest are shapes the check
#     refuses to parse rather than guess.
quiet_n=0
for quiet in \
	'The dispatcher compares `CONFIG_SCHEMA_EPOCH` before it merges.' \
	'Bump `CONFIG_SCHEMA_EPOCH` 6 → 7 in the same commit.' \
	'Bump `CONFIG_SCHEMA_EPOCH` to 9 when the parser changes.' \
	'`CONFIG_SCHEMA_EPOCH` was 6 when job inputs landed.' \
	'A config declaring `CONFIG_SCHEMA_EPOCH` >= 6 parks pre-Work.' \
	'`CONFIG_SCHEMA_EPOCH` is 6th in the table.' \
	'`PROJECT_IMAGE_SCHEMA_EPOCH` is 6 once the slice lands.' \
	'`DUPLICATE_EPOCH` is 6 in one of the two files.' \
	'`MAX_BLOB_BYTES` is 16384 bytes.' \
	'`CONFIG_SCHEMA_EPOCH` is `6` <!-- intent -->'; do
	quiet_n=$((quiet_n + 1))
	write_doc "quiet-$quiet_n.md" "$quiet"
	run_sut_repo "docs/quiet-$quiet_n.md"
	check "not a value claim, stays silent: $quiet" 0 "$RC" "$OUT" \
		"check-doc-facts: clean"
done

# 11. The claim must be on the line that names the constant.
write_doc next-line.md '`CONFIG_SCHEMA_EPOCH` is' '6 as of this writing.'
run_sut_repo docs/next-line.md
check "a value on the next line is not a claim" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 12. Both checks report in one run, and the summary counts them separately.
write_doc mixed.md \
	'State lives in `crates/dispatcher/src/state.rs`.' \
	'The epoch `CONFIG_SCHEMA_EPOCH` is `6` today.'
run_sut_repo docs/mixed.md
check "a path and a constant finding are both reported" 1 "$RC" "$OUT" \
	"1 stale path claim(s)"
check "a path and a constant finding are counted separately" 1 "$RC" "$OUT" \
	"1 stale constant claim(s)"

# --- Reach: whole-tree by default, staged for the hook ------------------------
# $TREE has a HEAD commit, so a staged-scoped run and a whole-tree run see
# different populations. This is the S1b change the rest of the suite cannot
# express: a doc nobody touched is still checked.
TREE="$WORK/tree"
mkdir -p "$TREE/crates/pkg/src" "$TREE/docs"
printf 'x\n' > "$TREE/crates/pkg/src/lib.rs"
git -C "$TREE" -c init.defaultBranch=main init -q
printf '# Old\n\nState lives in `crates/dispatcher/src/state.rs`.\n' > "$TREE/docs/old.md"
git -C "$TREE" add . >/dev/null 2>&1
git -C "$TREE" -c user.email=t@e -c user.name=t commit -qm base >/dev/null 2>&1

# 13. No arguments: every tracked `*.md`, including one this commit never touched.
run_in "$TREE"
check "whole-tree by default catches an untouched doc" 1 "$RC" "$OUT" \
	"docs/old.md:3: referenced path not found -> crates/dispatcher/src/state.rs"

# 14. `--staged` with nothing staged checks nothing and says so.
run_in "$TREE" --staged
check "staged mode with an empty index self-skips" 0 "$RC" "$OUT" \
	"no markdown to check (staged)"

# 15. `--staged` is scoped to the commit: the stale committed doc is out of
#     scope, which is the hook's ~2s budget bought at a named cost.
printf '# New\n\nSee `crates/pkg/src/lib.rs`.\n' > "$TREE/docs/new.md"
git -C "$TREE" add docs/new.md >/dev/null 2>&1
run_in "$TREE" --staged
check "staged mode checks only the staged markdown" 0 "$RC" "$OUT" \
	"check-doc-facts: clean (1 markdown file(s) checked, staged)"

# --- Check 3: a slice claiming a landed job ----------------------------------
# $DESIGN owns its own history, which is what check 3 resolves against: two
# squash-merge subjects and nothing else, so #381 and #442 merged here and #999
# never did. `JOB_ID` is cleared because the suite may run inside a task
# container whose own job number would otherwise be exempt.
DESIGN="$WORK/design"
mkdir -p "$DESIGN/docs/design"
git -C "$DESIGN" -c init.defaultBranch=main init -q
JOB_ID=""
export JOB_ID
design_commit() { # <subject>
	printf 'x\n' > "$DESIGN/docs/design/.keep"
	git -C "$DESIGN" add docs >/dev/null 2>&1
	git -C "$DESIGN" -c user.email=t@e -c user.name=t commit -q --allow-empty -m "$1" >/dev/null 2>&1
}
design_commit 'job/381: code'
design_commit 'job/442: code'
write_design() { # <name> <line>...
	name="$1"; shift
	{ for l in "$@"; do printf '%s\n' "$l"; done; } > "$DESIGN/docs/design/$name"
	git -C "$DESIGN" add "docs/design/$name" >/dev/null 2>&1 || true
}
run_design() { run_in "$DESIGN" "$@"; }

# 17. Both shapes in the tree — #362/#415's state-in-the-gate-cell and #440's
#     own State column — resolve against the same history, which is why check 3
#     matches the row and not a column.
write_design 900-both-shapes.md \
	'# Design #900' '' 'Status: PROPOSED.' '' '## Slices' '' \
	'| Slice | What | Gate on |' \
	'| --- | --- | --- |' \
	'| **S0** | The first slice | **Landed** (job #381), with the sweep it needed |' \
	'' \
	'| # | Slice | Depends on | State |' \
	'| --- | --- | --- | --- |' \
	'| 1 | The other table | — | **Landed** (job #442) |'
run_design docs/design/900-both-shapes.md
check "landed slices matching the history pass" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 18. The #416 defect: a row claiming a job the history does not hold. A revoked
#     job and one that never existed are the same finding, deliberately.
write_design 901-never-merged.md \
	'# Design #901' '' 'Status: PROPOSED.' '' \
	'| Slice | What | Gate on |' \
	'| --- | --- | --- |' \
	'| **S1** | A slice that did not land | **Landed** (job #999) |'
run_design docs/design/901-never-merged.md
check "a slice claiming a job that never merged fails" 1 "$RC" "$OUT" \
	"docs/design/901-never-merged.md:7: slice claims a job that never merged -> job #999"

# 19. The #415-head defect: IMPLEMENTED over a row that is still Proposed.
write_design 902-implemented.md \
	'# Design #902' '' 'Status: IMPLEMENTED — every slice shipped.' '' \
	'| # | Slice | State |' \
	'| --- | --- | --- |' \
	'| 1 | The one that landed | **Landed** (job #381) |' \
	'| 2 | The one that did not | Proposed |'
run_design docs/design/902-implemented.md
check "IMPLEMENTED over an unlanded row fails" 1 "$RC" "$OUT" \
	"docs/design/902-implemented.md:8: Status: IMPLEMENTED but slice 2 is Proposed"

# 20. Most design docs carry no slice table at all. Silence is what keeps the
#     fleet running (#415 M7), so it gets a case of its own — including a table
#     that is not a slice table.
write_design 903-no-slices.md \
	'# Design #903' '' 'Status: IMPLEMENTED — no slices here.' '' \
	'It argues a position and sequences nothing.' '' \
	'| Thing | Where | State |' \
	'| --- | --- | --- |' \
	'| The decider | in the domain crate | Shipped |' \
	'| The second one | nowhere yet | Proposed |'
run_design docs/design/903-no-slices.md
check "a design doc with no slice table is silent" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 21. A row the check cannot parse is skipped, never guessed at.
unparsed_n=0
for unparsed in \
	'| **S1** | A slice | **Landed** |' \
	'| **S1** | A slice | **Landed** (job #S1) |' \
	'| **S1** | A slice | Landed in job 999 |' \
	'| **S1** | A slice | **Landed** (in job 999) |'; do
	unparsed_n=$((unparsed_n + 1))
	write_design "904-unparsed-$unparsed_n.md" \
		'# Design #904' '' 'Status: PROPOSED.' '' \
		'| Slice | What | Gate on |' '| --- | --- | --- |' "$unparsed"
	run_design "docs/design/904-unparsed-$unparsed_n.md"
	check "an unparseable row is skipped: $unparsed" 0 "$RC" "$OUT" "check-doc-facts: clean"
done

# 22. A claim outside a table row is prose, not a slice, and is not judged.
write_design 905-prose.md \
	'# Design #905' '' 'Status: PROPOSED.' '' \
	'**Landed** (job #999) for the coverage job — this is a paragraph, not a slice.'
run_design docs/design/905-prose.md
check "a landed claim in prose is not a slice row" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 23. `IMPLEMENTED IN PART` is not `IMPLEMENTED`; rule 2 does not fire on it.
write_design 906-in-part.md \
	'# Design #906' '' 'Status: IMPLEMENTED IN PART — S1 landed, S2 intent.' '' \
	'| # | Slice | State |' '| --- | --- | --- |' \
	'| 1 | The one that landed | **Landed** (job #381) |' \
	'| 2 | The one that did not | Proposed |'
run_design docs/design/906-in-part.md
check "IMPLEMENTED IN PART is not IMPLEMENTED" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 24. Rule 2 needs the slice table identified by a landed row in it. Without
#     one, the check does not know which table it is reading, and says nothing.
write_design 907-no-landed-row.md \
	'# Design #907' '' 'Status: IMPLEMENTED — shipped in jobs #314-#319.' '' \
	'| # | Job | Depends on |' '| --- | --- | --- |' \
	'| 1 | The slice | — |' '| 2 | The next one | Proposed |'
run_design docs/design/907-no-landed-row.md
check "a table with no landed row is not judged as a slice table" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 25. Check 3 is scoped to `docs/design/`; the same row elsewhere is not a slice.
printf '| **S1** | A slice | **Landed** (job #999) |\n' > "$DESIGN/docs/notes.md"
git -C "$DESIGN" add docs/notes.md >/dev/null 2>&1
run_design docs/notes.md
check "a landed row outside docs/design/ is not a slice claim" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 26. The job doing the landing is exempt — D10 has it write the row in the same
#     commit, so `job/N` cannot exist yet when that commit is gated. Both
#     sources of its number are tested: the container's `JOB_ID`, and a `job/N`
#     branch in a local checkout.
write_design 908-inflight.md \
	'# Design #908' '' 'Status: PROPOSED.' '' \
	'| Slice | What | Gate on |' '| --- | --- | --- |' \
	'| **S1** | The slice this job is landing | **Landed** (job #700) |'
JOB_ID=700 run_design docs/design/908-inflight.md
check "the landing job is exempt via JOB_ID" 0 "$RC" "$OUT" "check-doc-facts: clean"
run_design docs/design/908-inflight.md
check "without that exemption the same row fails" 1 "$RC" "$OUT" \
	"slice claims a job that never merged -> job #700"
git -C "$DESIGN" checkout -q -b job/700
run_design docs/design/908-inflight.md
check "the landing job is exempt via the job/N branch name" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"
git -C "$DESIGN" checkout -q main

# 27. A history holding no `job/N:` commit gives check 3 nothing to resolve
#     against, so it stands down rather than reporting every row. $REPO has no
#     commits at all.
mkdir -p "$REPO/docs/design"
printf '%s\n' '# Design #909' '' 'Status: PROPOSED.' '' \
	'| Slice | What | Gate on |' '| --- | --- | --- |' \
	'| **S1** | A slice | **Landed** (job #999) |' > "$REPO/docs/design/909-no-history.md"
git -C "$REPO" add docs/design >/dev/null 2>&1 || true
run_sut_repo docs/design/909-no-history.md
check "no job/N commit in the history stands check 3 down" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# --- Check 4: one definition per registered concept ---------------------------
# $CONCEPTS owns a registry (`docs/concepts.md`), the doc that owns the one
# registered term, and a second doc to write about it from. The registry is read
# whatever files are passed, so a case names only the doc it is judging.
CONCEPTS="$WORK/concepts"
mkdir -p "$CONCEPTS/docs/reference"
git -C "$CONCEPTS" -c init.defaultBranch=main init -q
write_concept_doc() { # <path-under-docs> <line>...
	name="$1"; shift
	{ for l in "$@"; do printf '%s\n' "$l"; done; } > "$CONCEPTS/docs/$name"
	git -C "$CONCEPTS" add "docs/$name" >/dev/null 2>&1 || true
}
run_concepts() { run_in "$CONCEPTS" "$@"; }
write_registry() { # <row>...
	write_concept_doc concepts.md '# Concepts' '' '## The registry' '' \
		'| Concept | Defined in | What the row settles |' \
		'| --- | --- | --- |' "$@"
}
# The owner's heading carries an em dash, so the slug the row must name is the
# double-hyphen one GitHub produces — the shape the real registry uses.
write_concept_doc reference/owner.md '# Owner' '' '## Tier 3 — principles' '' \
	'- **Single writer.** The dispatcher is the only writer of job records.'
write_registry '| `single writer` | [`docs/reference/owner.md`](reference/owner.md#tier-3--principles) | The one writer |'

# 28. The registry resolves, and the owner writing its own definition is what
#     the row is FOR — the exemption falls out of the row, not a file list.
run_concepts docs/concepts.md docs/reference/owner.md
check "a resolving registry and the owner's own definition pass" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 29. D4's first shape, `**Term.**` opening a list item, in a doc that does not
#     own the term. The finding names the doc, the line and the owner.
write_concept_doc other.md '# Other' '' \
	'- **Single writer.** The dispatcher owns the fleet record as well.'
run_concepts docs/other.md
check "a list-item definition outside the owner fails" 1 "$RC" "$OUT" \
	'docs/other.md:3: second definition of "single writer" — owned by docs/reference/owner.md'

# 30. D4's second shape, `**Term** is`, with the term opening a sentence.
write_concept_doc other.md '# Other' '' \
	'A **single writer** is the shape every state record is held to.'
run_concepts docs/other.md
check "a sentence-opening definition outside the owner fails" 1 "$RC" "$OUT" \
	'docs/other.md:3: second definition of "single writer"'

# 31. A mention is free, however many times an argument needs it. The narrowing
#     that buys this is the sentence-opening rule: `and **X** is what failed`
#     (docs/spec.md's escalation table) is a mention, not a definition.
write_concept_doc other.md '# Other' '' \
	'The dispatcher is the **single writer** of job records, and every' \
	'caller goes through it; on Retry, **single writer** is what failed.' '' \
	'- A **single writer** keeps the state machine single-threaded.'
run_concepts docs/other.md
check "mentions of a registered term are silent" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 32. A doc QUOTING a definition states none — #415 quotes this exact line three
#     times — and neither does a fenced sample.
write_concept_doc other.md '# Other' '' \
	'The owner opens `- **Single writer.** The dispatcher is the only writer`.' '' \
	'```' '- **Single writer.** The dispatcher is the only writer of job records.' '```'
run_concepts docs/other.md
check "a quoted or fenced definition is not a second one" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 33. An UNREGISTERED term in definitional shape is invisible, however it is
#     written. Adding a row is what turns the gate on for a concept.
write_concept_doc other.md '# Other' '' \
	'- **Merge queue.** The queue is FIFO and the dispatcher drains it.' '' \
	'A **merge queue** is what serialises the landings.'
run_concepts docs/other.md
check "an unregistered term in definitional shape is invisible" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 34. A row whose anchor names no heading routes a reader into a doc that does
#     not say it, which is worse than no row.
write_registry '| `single writer` | [`docs/reference/owner.md`](reference/owner.md#tier-9-principles) | The one writer |'
run_concepts docs/concepts.md
check "a registry row whose anchor does not resolve fails" 1 "$RC" "$OUT" \
	'"single writer" names no heading in docs/reference/owner.md -> #tier-9-principles'

# 35. Same for a row whose owning doc is not tracked, resolved against the index
#     like every other claim here.
write_registry '| `single writer` | [`docs/reference/gone.md`](reference/gone.md#tier-3--principles) | The one writer |'
run_concepts docs/concepts.md
check "a registry row owned by an untracked doc fails" 1 "$RC" "$OUT" \
	'"single writer" is owned by an untracked doc -> docs/reference/gone.md'

# 36. One row per concept: two owners for one term is the ambiguity the registry
#     exists to remove.
write_registry \
	'| `single writer` | [`docs/reference/owner.md`](reference/owner.md#tier-3--principles) | The one writer |' \
	'| `single writer` | [`docs/reference/owner.md`](reference/owner.md#owner) | Again |'
run_concepts docs/concepts.md
check "two rows claiming one concept fail" 1 "$RC" "$OUT" \
	'two rows claim the concept "single writer"'

# 37. A broken row is wrong whatever the diff touched, so the hook's `--staged`
#     run reports it even with nothing staged rather than exiting 0.
run_concepts --staged
check "a broken registry fails a --staged run with nothing staged" 1 "$RC" "$OUT" \
	"unresolvable row(s) in docs/concepts.md"

# 38. With no registry in the tree, check 4 stands down whole: $REPO holds a
#     definitional line and no `docs/concepts.md`.
write_doc no-registry.md '- **Single writer.** The dispatcher owns every record.'
run_sut_repo docs/no-registry.md
check "no registry stands check 4 down" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 39. Every list marker the shape-1 matcher accepts is one the term extractor
#     reads, so a `*` bullet is judged exactly as a `-` one is.
write_registry '| `single writer` | [`docs/reference/owner.md`](reference/owner.md#tier-3--principles) | The one writer |'
write_concept_doc other.md '# Other' '' \
	'* **Single writer.** The dispatcher owns the fleet record as well.' '' \
	'1. **Single writer.** And the release record.'
run_concepts docs/other.md
check "a *-bulleted definition outside the owner fails" 1 "$RC" "$OUT" \
	'docs/other.md:3: second definition of "single writer" — owned by docs/reference/owner.md'
check "a numbered definition outside the owner fails" 1 "$RC" "$OUT" \
	'docs/other.md:5: second definition of "single writer"'

# 40. A registry in the tree whose table the parser cannot find would stand
#     check 4 down in silence, so it is a LINTER ERROR rather than a clean tree.
write_concept_doc concepts.md '# Concepts' '' '## Concept routing' '' \
	'| Concept | Defined in |' '| --- | --- |' \
	'| `single writer` | [`docs/reference/owner.md`](reference/owner.md#tier-3--principles) |'
run_concepts docs/other.md
check "a registry whose table does not parse is a LINTER ERROR" 2 "$RC" "$OUT" \
	"no row parsed under"

# --- Check 5: the docs catalogue, both directions -----------------------------
# $CATALOGUE owns a `docs/README.md` catalogue and the docs it is compared
# against. Check 5 is a set comparison over the whole index, so it runs whatever
# markdown the caller selected — these cases take the whole-tree default.
CATALOGUE="$WORK/catalogue"
mkdir -p "$CATALOGUE/docs/reference"
git -C "$CATALOGUE" -c init.defaultBranch=main init -q
write_catalogue_doc() { # <path-under-docs> <line>...
	name="$1"; shift
	{ for l in "$@"; do printf '%s\n' "$l"; done; } > "$CATALOGUE/docs/$name"
	git -C "$CATALOGUE" add "docs/$name" >/dev/null 2>&1 || true
}
run_catalogue() { run_in "$CATALOGUE" "$@"; }
write_catalogue() { # <row>...
	write_catalogue_doc README.md '# Index' '' '## The catalogue' '' \
		'| Doc | What it is |' '| --- | --- |' "$@"
}
write_catalogue_doc notes.md '# Notes' '' 'Prose.'
write_catalogue_doc reference/style.md '# Style' '' 'Prose.'
catalogue_rows() {
	write_catalogue \
		'| [`docs/README.md`](README.md) | This index |' \
		'| [`docs/notes.md`](notes.md) | The notes |' \
		'| [`docs/reference/style.md`](reference/style.md) | The practices |' "$@"
}

# 41. Both sides agreeing passes — including the catalogue's row for itself,
#     which is what keeps the rule free of an exception to remember.
catalogue_rows
run_catalogue
check "a catalogue agreeing with the tree passes" 0 "$RC" "$OUT" "check-doc-facts: clean"

# 42. A row the parser cannot read is SKIPPED IN SILENCE, never failed: check 5
#     errors in every job's pre-stage, so a shape it did not anticipate must not
#     stop the fleet. A prose row, a link out of `docs/` and a heading anchor are
#     all rows that name no doc, and none of them is a finding.
catalogue_rows \
	'| the runbooks | a prose row carrying no link |' \
	'| [the root README](../README.md) | outside docs/ |' \
	'| [a section](notes.md#notes) | an anchor, not a document |'
run_catalogue
check "a malformed catalogue row is skipped, not failed" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 43. Direction one: a tracked doc with no row. The message carries the row to
#     paste, because a gate that only says no is how the second half of a
#     two-file act gets forgotten again.
catalogue_rows
write_catalogue_doc reference/testing.md '# Testing' '' 'Prose.'
run_catalogue
check "a tracked doc with no catalogue row fails, naming the doc" 1 "$RC" "$OUT" \
	"docs/README.md: no catalogue row for docs/reference/testing.md"
check "the missing-row finding hands the author the row" 1 "$RC" "$OUT" \
	'| [`docs/reference/testing.md`](reference/testing.md) |'

# 44. Direction two: a row naming a path the index does not hold — the drift
#     that leaves a reader clicking into a doc that was deleted or renamed.
catalogue_rows '| [gone](gone.md) | A doc that is not there |'
write_catalogue_doc reference/testing.md '# Testing' '' 'Prose.'
git -C "$CATALOGUE" rm -q --cached docs/reference/testing.md >/dev/null 2>&1
rm -f "$CATALOGUE/docs/reference/testing.md"
run_catalogue
check "a catalogue row naming no tracked doc fails, naming the row" 1 "$RC" "$OUT" \
	"catalogue row names no tracked doc -> docs/gone.md"

# 45. A catalogue whose table the parser cannot find would stand check 5 down in
#     silence over every doc in the tree, so it is a LINTER ERROR — the same
#     verdict the concept registry gets for the same reason.
write_catalogue_doc README.md '# Index' '' '## Contents' '' \
	'| Doc | What it is |' '| --- | --- |' \
	'| [`docs/notes.md`](notes.md) | The notes |'
run_catalogue
check "a catalogue whose table does not parse is a LINTER ERROR" 2 "$RC" "$OUT" \
	"no row parsed under"

# 46. No `docs/README.md` in the tree stands check 5 down whole, which is what
#     keeps this script usable in a checkout that catalogues nothing.
git -C "$CATALOGUE" rm -q --cached docs/README.md >/dev/null 2>&1
run_catalogue
check "no docs/README.md stands check 5 down" 0 "$RC" "$OUT" "check-doc-facts: clean"

# --- `--emit-paths`: check 1's extractor with the verdict removed -------------
# The mode the staleness ledger (design #415 D7, S6) reads, so there is one
# answer in the tree to "what paths does this doc name". It must agree with
# check 1 on every classification and disagree with it on every verdict.

# 17. It prints `<file><tab><line><tab><path>` for a claim that resolves, and
#     exits 0 rather than judging.
write_doc emit-ok.md 'See `crates/pkg/src/lib.rs` for it.'
run_sut_repo --emit-paths docs/emit-ok.md
check "--emit-paths emits a resolving claim with its line" 0 "$RC" "$OUT" \
	"docs/emit-ok.md	3	crates/pkg/src/lib.rs"

# 18. An UNRESOLVABLE claim is omitted, not emitted: it is already check 1's
#     finding, and the ledger reporting it again would double-count it.
run_sut_repo --emit-paths docs/untracked.md
check_absent "--emit-paths omits what check 1 already fails on" 0 "$RC" "$OUT" \
	"crates/pkg/target/built.rs"

# 19. The classification is check 1's, not a looser one — a glob is no more a
#     path claim here than it is there.
run_sut_repo --emit-paths docs/globs.md
check_absent "--emit-paths refuses the same false-positive classes" 0 "$RC" "$OUT" \
	"crates"

# 20. It judges nothing: a file check 1 FAILS on emits at exit 0 beside a clean
#     one, and prints no finding.
run_sut_repo --emit-paths docs/untracked.md docs/emit-ok.md
check "--emit-paths never reports a verdict" 0 "$RC" "$OUT" \
	"docs/emit-ok.md	3	crates/pkg/src/lib.rs"
check_absent "--emit-paths prints no finding line" 0 "$RC" "$OUT" \
	"!!! check-doc-facts:"

# --- Check 6: an `#anchor` names a heading that exists ------------------------
# $ANCHOR owns the headings and the links to them. The slug index is whole-tree
# even when the judged set is one file, so a cross-doc case needs both docs
# tracked and only one of them passed.
ANCHOR="$WORK/anchor"
mkdir -p "$ANCHOR/docs/sub" "$ANCHOR/crates/pkg/src"
git -C "$ANCHOR" -c init.defaultBranch=main init -q
: > "$ANCHOR/crates/pkg/src/lib.rs"
git -C "$ANCHOR" add crates/pkg/src/lib.rs >/dev/null 2>&1 || true
write_anchor_doc() { # <path-under-docs> <line>...
	name="$1"; shift
	{ for l in "$@"; do printf '%s\n' "$l"; done; } > "$ANCHOR/docs/$name"
	git -C "$ANCHOR" add "docs/$name" >/dev/null 2>&1 || true
}
run_anchor() { run_in "$ANCHOR" "$@"; }

# The target doc: an identifier heading, a symbol heading, a duplicate pair, and
# a heading inside a fence that must anchor nothing.
write_anchor_doc a.md '# Target' '' \
	'## 2. The trait'"'"'s container assumptions, method by method' '' 'Prose.' '' \
	'### `XDG_RUNTIME_DIR`, answered' '' 'Prose.' '' \
	'## Correction, 2026-08-05 — §1 already shipped' '' 'Prose.' '' \
	'## Verification' '' 'Prose.' '' \
	'## Verification' '' 'Prose.' '' \
	'```' '## Not A Heading' '```'

# 47. The slug drops a symbol and an em dash but KEEPS underscores, so an
#     identifier heading is linkable. This is a regression guard with a scar: a
#     slugger that treats `_` as emphasis reports every `XDG_RUNTIME_DIR`-style
#     link in the tree as broken, which is a false positive in the pre-stage of
#     every job.
write_anchor_doc ok.md '# Links' '' \
	'See [tail](a.md#2-the-traits-container-assumptions-method-by-method).' \
	'See [ident](a.md#xdg_runtime_dir-answered).' \
	'See [symbol](a.md#correction-2026-08-05--1-already-shipped).'
run_anchor docs/ok.md
check "resolving anchors pass, underscores kept and \`§\` dropped" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 48. A repeated heading takes GitHub's `-1` suffix in document order, so the
#     second one is reachable and the first keeps the bare slug.
write_anchor_doc dup.md '# Links' '' \
	'First [one](a.md#verification), second [two](a.md#verification-1).'
run_anchor docs/dup.md
check "a repeated heading is linkable via the -1 suffix" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 49. The failure this check exists for: a hand-typed anchor that dropped the
#     heading's tail. Nothing else in the tree catches it.
write_anchor_doc truncated.md '# Links' '' \
	'See [tail](a.md#2-the-traits-container-assumptions).'
run_anchor docs/truncated.md
check "a truncated anchor fails, naming the target doc" 1 "$RC" "$OUT" \
	"names no heading in docs/a.md -> #2-the-traits-container-assumptions"

# 50. Keeping a character the slug drops is the same finding, and it is how a
#     `§` or an em dash typed into an anchor by hand goes wrong.
write_anchor_doc symbol.md '# Links' '' \
	'See [sym](a.md#correction-2026-08-05--§1-already-shipped).'
run_anchor docs/symbol.md
check "an anchor keeping a dropped symbol fails" 1 "$RC" "$OUT" \
	"names no heading in docs/a.md"

# 51. A same-file anchor resolves against its own headings.
write_anchor_doc self.md '# Own' '' 'Jump to [there](#the-section).' '' '## The section'
run_anchor docs/self.md
check "a same-file anchor resolves against its own headings" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 52. A relative link out of a subdirectory is normalised before the lookup, so
#     `../a.md` is judged as `docs/a.md` and not as a path nobody tracks.
write_anchor_doc sub/deep.md '# Deep' '' \
	'Up to [tail](../a.md#2-the-traits-container-assumptions-method-by-method).'
run_anchor docs/sub/deep.md
check "a ../ link is normalised before the lookup" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 53. A heading inside a fence is a code sample and anchors nothing.
write_anchor_doc fenced.md '# Links' '' 'See [nope](a.md#not-a-heading).'
run_anchor docs/fenced.md
check "a heading inside a fence anchors nothing" 1 "$RC" "$OUT" \
	"names no heading in docs/a.md -> #not-a-heading"

# 54. THREE THINGS ARE SKIPPED IN SILENCE, because each belongs to someone else:
#     a fragment on a source file is a line anchor, an external URL is another
#     document, and an untracked target is doc-lint's broken-link finding.
write_anchor_doc skipped.md '# Links' '' \
	'Source [line](../crates/pkg/src/lib.rs#L10).' \
	'External [page](https://example.com/x#frag).' \
	'Untracked [doc](gone.md#anywhere).'
run_anchor docs/skipped.md
check "a line anchor, an external URL and an untracked target are all skipped" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 55. `--emit-paths` judges nothing, here as everywhere: doc-staleness.sh reads
#     it for the path set and must never inherit an anchor verdict.
run_anchor --emit-paths docs/truncated.md
check_absent "--emit-paths does not judge an anchor" 0 "$RC" "$OUT" \
	"names no heading"

# --- A doc absent from the worktree is a skip, not a crash --------------------
# The scan loop has always skipped one; checks 4 and 6 read the same files
# through awk, which aborts on a file it cannot open, so they skip it too.

# 56. An explicit argument naming nothing is judged as nothing — the verdict a
#     `docs/gone.md` on the command line got before the anchor gate existed.
run_anchor docs/gone.md
check "an explicit path that is not there is a clean skip" 0 "$RC" "$OUT" \
	"check-doc-facts: clean"

# 57. Whole-tree with a TRACKED doc missing from the worktree — what a plain
#     `mv` of a doc leaves behind — still judges every doc that is there.
mv "$ANCHOR/docs/a.md" "$ANCHOR/a.md.away"
run_anchor
check "a tracked doc absent from the worktree does not abort the run" 1 "$RC" "$OUT" \
	"check-doc-facts:"
check_absent "the absent doc is skipped rather than opened" 1 "$RC" "$OUT" \
	"cannot open"
mv "$ANCHOR/a.md.away" "$ANCHOR/docs/a.md"

# --- A prerequisite that is missing is loud, and is not a pass ----------------

# 16. Outside a git checkout the check refuses to judge rather than falling back
#     to the filesystem — exit 2, a LINTER ERROR, distinct from both verdicts.
NONGIT="$WORK/nongit"
mkdir -p "$NONGIT"
printf '# Plain\n\nCites `crates/pkg/src/lib.rs`.\n' > "$NONGIT/plain.md"
run_in "$NONGIT" plain.md
check "non-git root is a LINTER ERROR, not a pass" 2 "$RC" "$OUT" \
	"not a git checkout"
check "the LINTER ERROR says the claims went unchecked" 2 "$RC" "$OUT" \
	"the doc claims went unchecked"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
