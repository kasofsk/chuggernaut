# Design #415 — Knowledge architecture: one definition per concept, and prose that cannot go quietly stale

Status: IMPLEMENTED IN PART — decisions taken with the operator 2026-08-04; S0 and S8 landed (S8 reversing [D8](#decisions)), the rest is intent. See [Slices](#slices).

Measured against the tree at `28e5aa1` (2026-08-04). Every number below was read
out of that commit, not carried over from the brief; the commands are given so a
reader can re-run them rather than trust them. The branch was later rebased onto
`69e48b2`; the M-table still reproduces at the stated sha, and the reference
counts in [what the move costs](#what-it-costs-honestly) were re-measured at
`d781496` and are labelled there. Two figures shifted at the new base: #313 grew
to 1,428 lines (M6 → 16,055), and `spec.md`'s age-key line moved from 2201 to
2217 — a `path:line` citation going stale inside this document, in the four
commits it took to write it, which is exactly why [check 1](#two-markers-not-one)
verifies the file and not the line number.

Supersedes job #86 (*inaugural design doc: docs, wiki, tags, and blessed
practices*), Frozen since the first hundred jobs. #86's five-section scope was
right; this doc is written with the ~320 jobs of failure corpus #86 was drafted
without.

## Current state

*This section is the **mutable head**: it is rewritten to current truth whenever
anything below it changes. Everything after the horizontal rule is
append-only — the original argument and its dated corrections, never edited.
The head is what you read to know where things stand; the body is what you read
to know why. This doc follows its own [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head).*

### The rule, in one sentence

**A fact is written once, in the doc that owns it; everywhere else links to it.
A doc that asserts something about the tree is making a checkable claim, and
what can be checked mechanically is.**

### Decisions

| # | Decision | Where argued |
| --- | --- | --- |
| **D1** | Two kinds of doc, opposite update rules: **reference** (present tense, no history) and **design** (append-only decision record). There is no third kind — plans become design docs with slice tables | [D1](#d1-two-kinds-of-doc-and-only-two) |
| **D2** | Every design doc opens with a **mutable current-state head**; the body below is append-only | [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head) |
| **D3** | `docs/concepts.md` is an **index of pointers**, not a glossary — a concept keeps its natural home | [D3](#d3-the-concept-registry-routes-it-does-not-hold) |
| **D4** | Ban duplicate **definitions**; allow duplicate **mentions** | [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions) |
| **D5** | `CLAUDE.md` may **gloss and link**, never define | [D5](#d5-claudemd-may-gloss-never-define) |
| **D6** | Four mechanical checks in one pure-shell `check-doc-facts.sh`, resolved against **git, not the filesystem** | [D6](#d6-four-mechanical-checks) |
| **D7** | A **git-derived staleness ledger** marks docs *suspect*, not wrong | [D7](#d7-the-staleness-ledger) |
| **D8** | ~~Knowledge tags become **pointers, not payload**~~ — **reversed while landing** (job #416, [S8](#slices)): knowledge is delivered as **payload**, and `.chug/tags/` is empty | [D8](#d8-tags-point-they-do-not-carry), superseded by the [2026-08-04 correction](#correction--2026-08-04-job-416-d8-reversed-m1-and-m5-restated) |
| **D9** | `review-docs-updated` gets **narrow, blocking** teeth | [D9](#d9-the-evaluator-judges-only-what-a-script-cannot) |
| **D10** | The **implementing job** updates the design doc it implements | [D10](#d10-the-implementing-job-owns-the-update) |
| **D11** | A **ratchet, not a flag day** | [D11](#d11-a-ratchet-not-a-flag-day) |
| **D12** | Every doc lives under `docs/`; the root keeps only `README.md`, `CLAUDE.md` and `wiki/`. `.chug/` prose stays put and every rule reaches it; `wiki/` is diagrams, not knowledge | [D12](#d12-where-everything-lives) |

### The target tree

```
README.md              project entry point (absorbs INSTALL.md)
CLAUDE.md              agent working notes — gloss and link only (D5)
docs/
  README.md            docs index (absorbs NORTH-STAR.md's routing role)
  spec.md              normative
  concepts.md          the registry (D3)
  reference/           present tense, no history, no status
    style.md  testing.md  crates.md  contracts.md  modules.md
    design-lifecycle.md  structure-assessment.md
    runbooks/
  design/
    NNN-*.md           mutable head + append-only body (D2)
  implementation-notes.md
.chug/                 unchanged — a product interface, not a doc tree (D12)
  prompts/  tasks/     (tags/ emptied by #416 when S8 reversed D8)
wiki/                  Obsidian vault; diagrams, not prose — exempt (D12)
```

### Slices

| Slice | What | Gate on |
| --- | --- | --- |
| **S0** | Triage `progress.md`: amend `spec.md` §5.1 (the artifact-store clause is false) and §10.2's single-age-key claim, repoint `.claude/skills/chug/SKILL.md`, then delete the file | **Landed** (job #432), in that order. §5.1 now states the NATS-internal store and keeps the S3/Minio deferral as its own claim; §10.2 names both age identities (§12.1 and the infrastructure appendix follow); the skill points at `spec.md` and the repo's docs |
| **S1a** | Fix `doc-lint.sh` rule 4's four false-positive classes in place and resolve against git; the two markers. Still a warning, still `docs`/`design`-scoped | — |
| **S1b** | Move that logic into `check-doc-facts.sh` — pre-stage, every job, whole-tree, **error** — and delete rule 4 from `doc-lint.sh`. Once S2 has cleared the real findings | S1a, S2 |
| **S1c** | Check 2 (constant values) + a `.test.sh` suite covering both | S1a |
| **S2** | The one-time sweep: the ~6 real path findings, the `*_SCHEMA_EPOCH` restatements, `state.rs` in its seven files; delete `spec_original.md` and `wiki/Welcome.md` | S1a |
| **S3** | **The move** — every doc into `docs/`, per D12; updates the ~730 references (283 of them outside markdown, 134 for `STYLE.md` alone) and `check-modules.sh`'s own path | S1b, S2 |
| **S4** | `docs/concepts.md` + the seed concept set + check 4 (definitional shape) | S3 |
| **S5** | Design-doc heads retrofitted (D2), plans demoted to design docs (D1), check 3 (slice ↔ merged job) | S3 |
| **S6** | The staleness ledger (D7) | S3 |
| **S7** | `docs-update.md` rewritten around D1/D10; `review-docs-updated` given D9's teeth | S5 |
| **S8** | Tags become pointers (D8) — **reversed while landing**: `.chug/tags/` is empty, and the four job types name `STYLE.md` / `NORTH-STAR.md` in `knowledge:`, delivered as payload rather than as a pointer | **Landed** (job #416), which replaced job #87 (now Revoked) rather than being it, and did not wait on S3. The reversal is argued in the 2026-08-04 correction at the end of the body; [D8](#d8-tags-point-they-do-not-carry) as written above it is superseded |
| — | `security-assessment.md` (1,147 lines, untracked) | **Out of scope**; its own job |

**Two rows are landed — S0 and S8.** Every other row above is intent, marked as
such per STYLE.md's doc-claim rule — which this document is partly written to
make enforceable.

This head went stale **within a day of merging**: job #416 landed S8 on
2026-08-04, appended its correction to the body per [D10](#d10-the-implementing-job-owns-the-update),
and left the table above still saying nothing was implemented and still naming
job #87 as live work. That is the drift **check 3** (slice ↔ merged job,
[S5](#slices)) exists to catch, and check 3 does not exist yet — so a human had
to notice. Recorded as evidence for the ordering, not filed away.

---

## The record

*Append-only from here. Corrections are appended with a date and never edited
into the prose above them.*

## The problem, measured

Not "docs drift" in the abstract. Seven measurements at `28e5aa1`:

| # | Measurement | Value |
| --- | --- | --- |
| M1 | Files naming `crates/dispatcher/src/state.rs` or `dispatcher::state` — **which does not exist** | **7** |
| M2 | Repo-relative path claims in markdown / of those, not tracked in git | **292 / 20** |
| M3 | `*_SCHEMA_EPOCH` mentions in markdown, against one `pub const` in `crates/types/src/version.rs` | **92**, across **13** files |
| M4 | `spec_original.md`: lines / inbound references / last touched | **1,251 / 0 / 2026-07-20** |
| M5 | Lines shared between `.chug/tags/north-star-blessed-practices.md` and `STYLE.md`, which define the same rules | **2 of 60** |
| M6 | `docs/design/`: docs / total lines / largest | **17 / 16,024 / 1,440** |
| M7 | `doc-lint.sh` "referenced path not found" warnings on a whole-tree run / of those, real | **256 / ~6** |

**M1 is the one that matters most, and it is not cosmetic.** The seven files are
`CLAUDE.md`, `STYLE.md`, `testing.md`, `NORTH-STAR.md`,
`.chug/prompts/work/design.md`, `.chug/tags/north-star-blessed-practices.md` and
`docs/design/169-handoff-continuity.md`. CLAUDE.md's mention is **normative** —
"`dispatcher::state` and release validation are the correctness core — keep
their branch coverage near-total" — so the file every agent reads first
instructs it to protect a module that is not there. A stale count is
embarrassing; a stale directive is acted on.

**M7 is the finding that reframes the work, and it was found last.** A path
check already exists — `.chug/tasks/doc-lint.sh` rule 4 — and it has named
`crates/dispatcher/src/state.rs` as unresolvable ever since the module was
removed. The signal was never absent; it was never *aimed*, and it never bit.
Both halves are structural:

- **It is diff-scoped.** The selection block (`.chug/tasks/doc-lint.sh:42-79`)
  lints the `*.md` in `git diff --name-only $base...HEAD`; the whole-tree
  `git ls-files '*.md'` is the fallback taken only when the diff is
  *uncomputable* ("never skip on uncertainty"). In a job container
  `BASE_BRANCH` is set, so a job sees warnings for the files it touched. Only
  two of M1's seven files backtick the path — `.chug/prompts/work/design.md:31`
  and `docs/design/169-handoff-continuity.md:94` — so the warning surfaced only
  for a job that happened to edit one of those two. CLAUDE.md, where the claim
  is *normative*, names `dispatcher::state` in prose and is not a path claim at
  all: no scoping would have caught it.
- **It does not fail.** Rule 4 reports `warn` (`.chug/tasks/doc-lint.sh:167`),
  and the gate exits on `errors` alone (line 179).

The **256** in the table is therefore a number the gate essentially never
produces in a job — it is what a whole-tree run prints. That is the finding, not
a footnote to it: the check with the right idea was pointed at the wrong
population and carried no consequence.

Of those 256, roughly **six** are real. The rest are four distinct
false-positive classes: 49 contain a glob (`crates/*/src/lib.rs`), 58 are
absolute or in-container paths (`/dev/kvm`, `/chuggernaut/ssh/id`), a large
group are placeholder templates (`.chug/jobs/{job_type}.yaml`,
`docs/design/{seq}-{slug}.md`), and a further group are `path:line` citations
whose file exists and whose `:193` suffix does not
(`crates/dispatcher/src/release.rs:193`).

> **A gate with a 2% true-positive rate is a gate that is off.** The lesson is
> not that we lack a check; it is that a noisy check trained everyone —
> including every reviewing agent — to scroll past the one line that mattered.

**M5 is the finding that determines the whole approach.** The knowledge tag and
STYLE.md both define this repo's blessed practices, and they share **two lines
out of sixty**. The tag is a *paraphrase*, not a copy. Whatever detects
duplicated facts here cannot be a clone detector.

Reproduce M1, M2 and M5:

```sh
git grep -l 'dispatcher/src/state\.rs\|dispatcher::state'
git ls-files > /tmp/tracked.txt
git grep -ho '`[a-zA-Z0-9_./-]*/[a-zA-Z0-9_./-]*`' -- '*.md' | tr -d '`' \
  | sed 's/[.,;:]$//' | sort -u \
  | grep -E '^(crates|docs|deploy|web|fixtures|\.chug|\.githooks|\.claude)/'
comm -12 <(sort -u .chug/tags/north-star-blessed-practices.md) <(sort -u STYLE.md)
```

## Why the duplication gate cannot be extended

The obvious first thought is that this repo already forbids copy-paste —
`.chug/tasks/check-duplication.sh` runs a pinned `jscpd@5.0.5` at
`threshold: 0`, STYLE.md Tier 1 — so point it at prose. That does not work, and
the reason is worth recording because it is the reason the answer is *ownership*
rather than a bigger clone detector.

1. **`.jscpd.json` ignores `**/*.md` deliberately.**
2. **Lifting the ignore changes nothing.** jscpd 5.0.5 reports `Files analyzed: 0`
   for markdown even when given `--format markdown --formats-exts markdown:md`.
   It is a tokenizer over programming-language grammars; markdown is not one.
3. **Even a working markdown tokenizer would find nothing.** `minLines: 10` and
   `minTokens: 80` against M5's two shared lines is not close.

> **Code duplication is token-identical. Fact duplication is a second author
> saying the same thing differently.** Same word, different problem, different
> instrument.

There is a corollary that shapes [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions):
general semantic-duplicate detection is undecidable, so nothing here attempts
it. What *is* decidable is the **syntactic signature of a definition**, applied
to a **known list of terms**.

## D1. Two kinds of doc, and only two

- **Reference** — describes the system as it is now. Present tense, no history,
  no status line, no "we decided". `spec.md`, `testing.md`, `crates.md`,
  `contracts.md`, `MODULES.md`, `STYLE.md`, `design-lifecycle.md`,
  `structure-assessment.md`, the runbooks.
- **Design** — an append-only decision record. `docs/design/*.md`.

**There is no third kind.** Today the root carries four documents that are
neither: `refactor-plan.md`, `ts-rewrite-plan.md`, `NORTH-STAR.md` and
`progress.md`. A "plan" describes a future, so it is not reference; it is not
one decision, so it does not look like a design doc. In practice that means it
belongs to no update rule and decays unobserved — `ts-rewrite-plan.md` has not
been touched since 2026-07-24.

The resolution is demotion, not a new category:

- **A plan is a design doc with a slice table.** `refactor-plan.md` and
  `ts-rewrite-plan.md` get numbers, a [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head)
  head and slice rows, and inherit the whole machinery — including
  [D7](#d7-the-staleness-ledger), which will immediately mark
  `ts-rewrite-plan.md` suspect. That is the correct verdict, and no rule had to
  be written to reach it.
- **`NORTH-STAR.md` is a routing doc**, which is what a docs index is. It
  becomes `docs/README.md` ([D12](#d12-where-everything-lives)).
- **`progress.md` is a session diary** and is deleted, after triage — see
  [S0](#s0-progressmd-is-three-things-fused).

## D2. Every design doc opens with a mutable current-state head

The head carries `Status:`, the decision table, and the slice table. It is
rewritten freely to current truth. Everything below the rule is append-only:
the original argument, and corrections appended with a date, never edited into
the prose above them.

**This is not stylistic.** M6 says the design corpus is 17 docs, 16,024 lines,
largest 1,440. Pure append-only grows that without bound, and a doc nobody
finishes is stale in the way that matters. The head bounds the reading cost of
*knowing where things stand* at ~50 lines while the body keeps the full record.

`docs/design/362-binary-artifacts.md` already approximates the shape — its
sequencing table carries `**Landed** (job #381)` cells — and is the model.

**The friction is real and is accepted.** Every slice merge edits the head, and
that is the first place an author will be tempted to cheat. Check 3
([D6](#d6-four-mechanical-checks)) is what makes cheating detectable: a slice
claiming `Landed (job #N)` must correspond to a merged `job/N` commit.

## D3. The concept registry routes; it does not hold

`docs/concepts.md` maps concept → owning `doc#anchor`. It does **not** contain
the definitions.

The alternative — lift every definition into a glossary — makes
[D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions)'s check trivial
(any definition outside the glossary is a violation) and was rejected anyway.
A definition divorced from the argument that motivates it is worth less: "the
dispatcher is the single writer" means something in STYLE.md, surrounded by the
reasoning about why single-threaded state management is a design constraint
rather than a performance note. Extracted into an alphabetical list it becomes a
sentence to memorize.

The registry is `MODULES.md`'s shape, and `.chug/tasks/check-modules.sh` is the
gate to copy: pure shell, runs before the Rust early-exit so a docs-only diff
cannot bypass it, called by both CI and the pre-commit hook so "clean locally"
and "clean in CI" cannot diverge, and it names each offending row.

**The seed set is the terms that already have competing definitions**: `single
writer`, `merge gate`, `job branch`, `job type`, `evaluator`, `claim`,
`epoch`, `slice`, `tier`. Roughly a dozen. The criterion for adding one: a term
that a reader must understand to read another doc, and that more than one doc
currently explains.

## D4. Ban duplicate definitions, allow duplicate mentions

This is the line that keeps prose readable, and it is deliberately not the
strongest available rule.

- **A mention is free.** Any doc may write "the dispatcher is the single writer"
  in passing, as many times as the argument needs.
- **A definition is owned.** What the term *means* is written once.

Enforcement is syntactic and scoped to **registered** terms only: a definitional
shape outside the owning doc is a violation. Two shapes cover the corpus —
`**Term.**` opening a list item, and `**Term** is|are|means|refers to`.

That narrow rule catches the real instance. `STYLE.md:231` and
`.chug/tags/north-star-blessed-practices.md:50` both open with the identical
string `- **Single writer.** The dispatcher is the only writer of job records,`
— and per M5 the rest of the two passages diverge, which is exactly why nothing
token-based was going to find them.

**Over-normalization is the failure mode to avoid.** A doc that links every term
and asserts nothing reads worse than mild redundancy. Nothing stronger than the
above should land without evidence that this was insufficient.

## D5. CLAUDE.md may gloss, never define

CLAUDE.md restates other docs on purpose: it is the first thing every agent
reads and its value is front-loading what bites you. A file of bare links would
force a dozen doc-opens before work starts.

> **One line of gloss plus a link to the owner. Never a second definition.**

A drifting gloss is far less harmful than a competing definition, and the link
gives a reader somewhere to check. The reason CLAUDE.md gets a rule rather than
an exemption is M1: the `state.rs` directive is *in CLAUDE.md*, so exempting it
would exempt the single most damaging instance in the tree.

## D6. Four mechanical checks

One pure-shell `.chug/tasks/check-doc-facts.sh`, in the pre-stage beside
`check-modules.sh`, called by CI and the pre-commit hook.

| # | Check | Catches |
| --- | --- | --- |
| 1 | A backticked repo-relative path resolves, or carries a marker — **`doc-lint.sh` rule 4's logic, fixed and moved here; not a second copy** | M1, M2, M7 |
| 2 | A backticked `SCREAMING_SNAKE` name that resolves to a `pub const`, asserted with a value on the same line, matches the tree | M3 |
| 3 | A slice claiming `Landed (job #N)` corresponds to a merged `job/N` commit; `Status: IMPLEMENTED` requires every slice landed | [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head) drift |
| 4 | A registered term in definitional shape appears only in its owning doc | M5 |

### Check 1 already exists; precision before teeth

Per M7, `doc-lint.sh` rule 4 does this job today and found M1 before anyone
else did. It fails on three counts, and the order in which they are fixed is not
negotiable:

1. **Precision.** Four false-positive classes — globs, absolute/in-container
   paths, `{placeholder}` and `<name>` templates, and `path:line` citations —
   drown ~6 real findings in 256 warnings.
2. **Teeth.** Rule 4 reports `warn` (`.chug/tasks/doc-lint.sh:167`), and the
   gate exits on `errors` alone (line 179).
3. **Reach.** It runs at stage 1 of `docs` and `design` jobs only
   (`.chug/jobs/docs.yaml:29`, `.chug/jobs/design.yaml:30`), over the job's
   diff. A `code` job editing CLAUDE.md runs it not at all.

> **Fix precision first, then promote to error.** Promoting first fails every
> docs job on 250 false positives, and the certain outcome is that someone turns
> the rule off — leaving us worse off than the noisy version, which at least
> printed the truth.

This is the same shape as job #407's finding that 48% of the test suite's wall
time was spent by tests that skip: the machinery was present and its output was
not believable, so nobody used it.

### Where check 1 runs, and over what

Fixing precision without fixing reach would leave the M1 file — CLAUDE.md, most
often edited by `code` jobs — outside the gate that exists to protect it. So the
logic **moves** to `check-doc-facts.sh`, and rule 4 in `doc-lint.sh` is deleted
rather than kept in parallel; `doc-lint.sh` keeps well-formedness, filename shape
and relative-link resolution, which are the things a `docs`/`design` job alone
needs.

| | `doc-lint.sh` rule 4, today | Check 1, after [S1b](#slices) |
| --- | --- | --- |
| Runs on | `docs` / `design` jobs, stage 1 | **every job**, pre-stage, beside `check-modules.sh` (`.chug/tasks/ci.sh:559-570`) |
| Population | the job's diff | **every tracked `*.md`** |
| Verdict | `warn`; gate exits on errors | **error** |

Whole-tree, not diff-scoped, and the precedent is exact: `check-comments.sh`
enforces its rule 1 over the entire tree and leaves only the two-sentence cap as
a ratchet, because a whole-tree rule is only affordable once the tree is clean.
[S2](#slices) is what buys that — the sweep of the 10 stale paths must land
*before* the promotion, which is why [D11](#d11-a-ratchet-not-a-flag-day)'s
"ratchet, not flag day" applies to the *definition* ban and not to this check.
The price is that from S1b onward, any job that lands a doc naming a path that
later moves fails until someone fixes it — which is the intended cost, and the
reason check 1 is the only one of the four that goes whole-tree.

In the pre-commit hook it runs **staged-scoped and rejecting**, exactly as
`check-comments.sh --staged` does (`.githooks/pre-commit:225-247`) and unlike
`doc-lint.sh`, which is advisory there only because CI runs it on two job types
(`.githooks/pre-commit:295-312`). Once check 1 is unconditional in CI, rejecting
at the commit cannot block a commit CI would accept — the hook's contract holds.
It is scoped rather than whole-tree to keep the hook's ~2s budget, and the honest
cost of that is one gap: a commit that *deletes* a path other docs name passes
the hook and fails CI. [D7](#d7-the-staleness-ledger)'s ledger is what surfaces
that class at the commit, which is why the two are not redundant.

### Resolve against git, not the filesystem

Discovered while measuring M2, and it would have been a real defect. The first
run of the path check used `[ -e ]` and reported **22** unresolvable claims in a
fresh worktree versus **16** in a built checkout. The six-claim difference is
`target/`, `web/dist` and `deploy/dev/data/` — build output that exists on a
developer's machine and not in CI.

> **The check resolves against `git ls-files`.** A filesystem check makes the
> gate's verdict depend on whether the caller has run `cargo build`, which is
> the precise divergence `check-modules.sh`'s header exists to prevent.

### Two markers, not one

The 20 untracked claims are three different things, and collapsing them would
force authors to lie:

- **Stale** (10) — `crates/dispatcher/src/state.rs`, `crates/channel/`,
  `crates/forge-ingest`, `crates/platform-ops/templates/smoke/`,
  `.chug/tags/backend.md`, `docs/design/epics.md`, `docs/BOOTSTRAP.md`,
  `deploy/prod/BOOTSTRAP.md`, `deploy/prod/install-systemd.sh`,
  `deploy/prod/systemd/`. These are the bug. Fix them.
- **Intent** (4) — `.chug/images.yaml`, `.chug/images/`, `.chug/schedules/`,
  `.claude/CLAUDE.md`. Designed, not built. `<!-- intent -->`.
- **Runtime** (6) — `web/dist`, `target/`, `deploy/dev/data/`,
  `deploy/dev/out/…`, `deploy/prod/chuggernaut.env` (operator-owned, on the
  Mini), `deploy/dev/data/keys/claude.token`. Correctly absent from git and
  correctly named in docs. `<!-- runtime -->`.

The `intent` marker is what finally gives STYLE.md's existing doc-claim rule
teeth. That rule already says *"check it, or mark it as intent"* — and marking
has never been a syntax, so the rule has never been enforceable.

### What check 4 cannot do, and what took its slot

The decisions as taken numbered check 4 as *"a count carries a marker naming
what derives it."* **The definitional-shape check took that slot instead**, and
the substitution is deliberate: [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions)
is the decision this whole document turns on and it says "enforce
syntactically", which requires a check; the count rule cannot have one.

A script cannot decide whether a bare number in prose is a count of something in
the tree. It can verify a *marked* count — `<!-- derived: git ls-files '*.md' |
wc -l -->` is re-runnable — but the marker is what makes it checkable, and a
marker is exactly what a drifting count does not carry. Verifying marked counts
is therefore **dropped from [S4](#slices)**, not deferred: a check that fires
only on the honest cases and is silent on every dishonest one buys nothing and
costs a maintained script. Requiring the marker, and rejecting an unmarked
count, stays a **reviewer** rule under [D9](#d9-the-evaluator-judges-only-what-a-script-cannot).

Overstating a gate's coverage is the defect this document exists to fix. It
would be absurd to commit it here — including by letting a check quietly change
identity between the decision and the design.

## D7. The staleness ledger

For each doc, the set of tree paths it names. If any of those paths has commits
newer than the doc's own last commit, the doc is **suspect** — not wrong.

Entirely derived from git. No new syntax, no `last-verified:` front matter for
anyone to forget, nothing to keep in step.

This reaches the class no syntactic rule can: `version.rs` moving while 13 docs
restate its constants (M3), or `state.rs` disappearing while seven files still
name it (M1). Neither is a broken *claim* at the moment it happens — the claim
is simply no longer checked by anything.

Advisory in the pre-commit hook; a blocking finding only when the current diff
touches a doc the ledger already marks suspect. A ledger that fails builds for
history nobody caused is a ledger people disable.

## D8. Tags point; they do not carry

Attached knowledge reaches a work agent as a deterministic instruction to *read*
named pages from its checkout, not as inlined text. This is #86 §3 and job #87,
both of which predate this doc and neither of which has run.

It matters here because it dissolves the strongest objection to
[D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions): that a knowledge
tag must paraphrase a doc in order to stand alone in a context that lacks it. If
the tag names the page instead of copying it, there is no second copy to drift —
and M5 stops being possible rather than merely being forbidden.

**Decided: #87 stays its own job, and [S8](#slices) *is* #87** — not a
replacement for it and not a second ticket covering the same ground. This
document supplies the policy #87 was missing and nothing else; the work itself
is dispatcher code (how attached knowledge is rendered into a work context),
which is a different job type from every other slice here and has a written
brief already. Superseding it would mean re-deriving that brief to gain
nothing. #86 is superseded because it decided the same questions this doc
decides; #87 is not, because it implements rather than decides.

The dependency runs one way: #87 should not land before
[D3](#d3-the-concept-registry-routes-it-does-not-hold) and
[S3](#slices), since a pointer is only as good as the page it names and the
pages move.

## D9. The evaluator judges only what a script cannot

`.chug/tasks/review-docs-updated.md` is currently inert **by design**, and it
names the reason: *"How docs are managed … is a decision the project has not
made yet, and a gate that guesses at a policy is worse than one that visibly
holds the slot until the policy exists."* This document is that decision, so the
slot can be filled.

It gets **blocking** teeth, scoped to what [D6](#d6-four-mechanical-checks)
cannot reach:

- cross-doc state claims — one doc asserting another's status or content;
- behavioral claims about symbols the diff touched.

Everything mechanically decidable stays in the shell gate, so an agent is never
what stands between a correct change and a merge on a question a script could
have answered.

**It reads; it does not run.** Agent evaluators launch under the read-only
`Review` profile (`spec.md` §4.3) — no `cargo`, no `npm`. A doc claim that can
only be settled by executing something is out of its reach and belongs in
`.chug/tasks/ci.sh`.

## D10. The implementing job owns the update

A code job implementing slice N flips that slice's cell and adjusts the head, in
the same commit, as an acceptance criterion. The author is the only party who
knows what *actually landed* versus what was designed; a follow-up job would be
re-deriving that from a diff, and a queue of pending doc jobs is how the current
state was reached.

**This requires rewriting `.chug/tasks/docs-update.md`.** Its rule 4 today reads
*"Do not narrate the change in a doc. Docs describe the system as it is now, in
the present tense."* Under [D1](#d1-two-kinds-of-doc-and-only-two) that is
exactly right for reference docs and exactly wrong for design docs, and an agent
reading it literally will decline to update a slice table. The contradiction is
load-bearing: it is why #313 could carry `CONFIG_SCHEMA_EPOCH (currently 1)`
through four epoch bumps.

## D11. A ratchet, not a flag day

Precedent is exact. `.chug/tasks/check-comments.sh` enforces its first rule over
every tracked source — the tree holds zero non-doc comments since job #342 — and
leaves the two-sentence doc-comment cap as a ratchet on changed lines only.

So:

- **Swept once**, because the sets are small and known: the 20 untracked path
  claims (M2), the `*_SCHEMA_EPOCH` restatements (M3), `state.rs` in its seven
  files (M1).
- **Ratcheted from here**: the append-only rule, the definition ban, the
  mutable-head requirement.

**`spec_original.md` is deleted.** M4: 1,251 lines, zero inbound references,
untouched since 2026-07-20. Git history preserves it, and a dead spec that an
agent can grep into and believe is the single largest instance of this
document's subject.

### S0. `progress.md` is three things fused

It is not deleted blind. Audited at `28e5aa1`, it decomposes into:

1. **A crate status table** (7 status rows) — a *third* copy of what `crates.md`
   and `MODULES.md` own. Job #408 updated a row in it. Delete; those docs own it.
2. **A session narrative** — its header says "Update it at the end of each
   implementation session" and `**As of:** 2026-07-18 (session 9)`, declaring
   itself 17 days stale while being edited today. Git history owns this.
3. **One unamended spec deviation, still live.** Line 397 records two "spec
   deviations to amend". The `age_artifacts` one **was** amended — `spec.md`
   names that key in three places. The other was not: `spec.md:1563` still reads
   "No separate artifact store for v1", which is now false twice over, once for
   the NATS-internal artifacts store and once for the `outputs` bucket
   [#362](362-binary-artifacts.md) S1/S2 landed. A smaller sibling:
   `spec.md:2217` still says "**the** age private key is dispatcher-only",
   singular, when the bucket rows name two — the distinction
   [#313](313-workload-identity-image-builds.md) A2 leaned on.

And `.claude/skills/chug/SKILL.md` tells every agent to read `progress.md` "for
dispatcher internals rather than guessing" — the M1 failure again, in the skill
this time.

So S0 is: amend `spec.md`, repoint the skill, **then** delete. In that order.

## D12. Where everything lives

The root holds `README.md`, `CLAUDE.md` and `wiki/` (below). Everything else
that is a document lives under `docs/`, per the tree in the head.

| From | To | Why |
| --- | --- | --- |
| `spec.md` | `docs/spec.md` | Normative; earns top level |
| `STYLE.md`, `testing.md`, `crates.md`, `contracts.md`, `MODULES.md` | `docs/reference/` | Describe the system as it is |
| `design-lifecycle.md`, `structure-assessment.md` | `docs/reference/` | Both describe current behavior, not a decision |
| `design.md` | `docs/design/000-rationale.md` | It argues a position |
| `refactor-plan.md`, `ts-rewrite-plan.md` | `docs/design/NNN-*.md` | [D1](#d1-two-kinds-of-doc-and-only-two): a plan is a design with slices |
| `NORTH-STAR.md` | `docs/README.md` | A routing doc is a docs index |
| `INSTALL.md` | `README.md` | The root entry point should be what a human lands on |
| `spec_original.md`, `progress.md` | deleted | [D11](#d11-a-ratchet-not-a-flag-day), [S0](#s0-progressmd-is-three-things-fused) |
| `.chug/prompts/`, `.chug/tags/`, `.chug/tasks/*.md` | **stay** | The platform reads them from `.chug/` (`spec.md` §1.1); moving them breaks the product, not a link. Every rule still reaches them — below |
| `wiki/` | **stays, reclassified** | Diagrams, not prose; `Welcome.md` deleted — below |

### `.chug/prompts/` — stays put, and every rule reaches it

The work and review prompts are not exempt prose. They are read by **every**
agent on **every** job, they carry path claims, and `.chug/prompts/work/design.md`
is one of M1's seven files — line 31 hands each design job
`crates/dispatcher/src/state.rs` as its worked example of a path citation, so
the prompt teaching agents to cite paths accurately cites a module that does not
exist.

They do not move: the dispatcher loads them from `.chug/` by spec §1.1, and the
path is a product interface, not a link. What applies to them:

- **Check 1, no special case.** Neither `doc-lint.sh` nor the check that replaces
  it applies any directory filter — the selection block
  (`.chug/tasks/doc-lint.sh:42-79`) picks `*.md` and nothing narrower — so
  `.chug/prompts/`, `.chug/tags/` and the root docs are one population whenever
  they are in scope. After [S1b](#slices) that scope is every tracked `*.md` on
  every job, which is how the prompt's own stale citation gets caught.
- **[D5](#d5-claudemd-may-gloss-never-define)'s rule, verbatim.** A prompt may
  gloss a concept in one line and link its owner; it may not define one. The
  argument is identical to CLAUDE.md's and stronger by exposure: a prompt is
  injected into a context that may hold nothing else, so a definition drifting
  there is a definition the agent cannot check.
- **[D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions) as a consumer,
  and [D7](#d7-the-staleness-ledger).** A registered term in definitional shape
  in a prompt is a violation like anywhere else. Prompts are among the likeliest
  docs to be marked suspect, which is the point.

[D8](#d8-tags-point-they-do-not-carry) is the tag-shaped half of the same rule:
tags become pointers. Prompts stay instructions — an instruction is not a fact
about the tree, so nothing here asks them to become links to somewhere else.

### `wiki/` — not prose, and therefore not knowledge

`wiki/` is an Obsidian vault: `.obsidian/` config, `Chuggernaut Diagram.canvas`
(the operator's pre-implementation sketch of the ticket/work/evaluation model,
added 2026-07-19 in `ee94657`, before `spec.md` existed), two default `*.base`
view stubs, and `Welcome.md` — four lines of Obsidian's shipped boilerplate,
unedited. It has **zero** inbound references from anywhere in the tree
(`git grep 'wiki/'` outside this doc returns nothing), and its only change since
creation was job #364 moving two canvas nodes.

The name is actively misleading: `spec.md` §9.4 says **the `docs/` tree is the
wiki**, so the one directory called `wiki/` is the one that is not it.

The decision is to **keep it, exempt it, and shrink it**:

- It is **not a document** under [D1](#d1-two-kinds-of-doc-and-only-two) — it
  is a diagram surface. No kind, no update rule, no owner, and no check reads
  it. Verified: the canvas asserts nothing about the tree — no repo paths, no
  constants, no statuses — so there is nothing in it for a check to be right or
  wrong about.
- **The rule that keeps that true:** if a note there ever states a fact about
  the system, that fact belongs in `docs/` and the note becomes a link. The
  exemption is for diagrams, not for a second prose tree growing outside the
  gates.
- **`Welcome.md` is deleted in [S2](#slices)** with `spec_original.md`. It is
  the vault's only `*.md`, it is vendor boilerplate that says nothing, and it is
  currently inside `doc-lint.sh`'s population — a real doc's worth of scan
  budget spent on "This is your new *vault*."

**Deleting the vault outright was the alternative, and it was rejected.** The
`spec_original.md` argument — dead prose that agents grep into and believe —
does not transfer: the canvas is not prose, nothing greps into it, and no agent
has ever been misled by it. Its cost is a directory name; its value is the
operator's, and it is not the platform's call to bin someone's thinking surface
to save eight tracked files. Untracking it (`.gitignore`) was likewise rejected:
job #364 shows work does get tasked against it, and a job cannot edit a file its
checkout does not contain.

### What it costs, honestly

Measured at `d781496`, `git grep -oE` over the fourteen moving filenames:
**~730 occurrences across 120 files** (739 raw, less the 8 that are
`work/design.md` and `review-design.md` matching the `design.md` alternative).
Of those, **283 occurrences in 77 files are outside markdown** — gates, prompts,
job-type YAML, Rust doc comments. `STYLE.md` alone accounts for **134
occurrences in 44 files** there:

```sh
git grep -o 'STYLE\.md' -- ':!*.md' | wc -l   # 134
git grep -l 'STYLE\.md' -- ':!*.md' | wc -l   # 44
```

Two thirds of that is one cluster — `crates/test-utils/` and its two guard tests
carry 78 of the 134, as the strings the lint guards assert on. Those are not
links and will not 404; they are citations that go stale silently, which is the
worse failure and the one no linker catches. `check-modules.sh` hard-codes its own
registry path. This is the largest link-breaking change the repo will make.

Which forces the sequencing: **[S1b](#slices) before [S3](#slices).** The path
check must be *failing* before the move, not merely printing. Landing the move first would leave no
way to know it was done correctly, and "we moved every doc and think we caught
every link" is exactly the class of unchecked claim this document exists to
eliminate.

## Rejected alternatives

**Extend jscpd to markdown.** Does not work and would not help — see
[above](#why-the-duplication-gate-cannot-be-extended).

**A glossary that owns the definitions.** Makes check 4 trivial. Rejected in
[D3](#d3-the-concept-registry-routes-it-does-not-hold): it strips each concept
from the argument that motivates it.

**Pure append-only design docs, with no mutable head.** The strongest honesty
guarantee — nothing can be silently rewritten. Rejected on M6: the corpus is
already at the readability limit, and #308 shows what reconstructing current
state from an original plus N corrections reads like.

**`last-verified:` front matter.** Cheap, and catches things no syntactic rule
can. Rejected because a date is a promise, and an unkept promise is one more
false claim in a document about false claims. [D7](#d7-the-staleness-ledger)
derives the same signal from git, which nobody has to remember.

**Transclusion, or a doc build step.** The strongest possible single-sourcing.
Rejected on a hard constraint: these files are read **raw, out of a checkout, by
agents**. A doc whose meaning depends on a build step is a doc an agent reads
wrong.

## What would refute this

Stated as triggers, so a future reader can check rather than re-argue.

- **Check 4's false-positive rate is material.** If registered terms in
  definitional shape flag prose that is not a second definition, the syntactic
  approach is wrong and the answer is a smaller registered set, not a cleverer
  regex.
- **The mutable head is edited to match reality less often than the body.** That
  would mean [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head)
  moved the staleness rather than removing it. Check 3 is the detector.
- **The staleness ledger marks everything suspect.** A signal that fires on most
  docs most of the time is noise. If that happens the path set per doc is too
  broad — narrow it to paths in backticks, not every mention.
- **The move breaks something no check caught.** Then check 1 is under-scoped —
  most likely because references in non-markdown files (283 across 77 files for
  the moving names; 134 for `STYLE.md` alone) are outside its globs, which lint
  only `*.md`.

## Related

Job #86 (superseded — its five-section brief is still the right scope), #87 (the
tag half, unrun); jobs #375, #378, #382, #383, #385, #394 (the corpus STYLE.md's
doc-claim rule was written from), #409 (the most recent instance, and the one
that prompted this); `STYLE.md` Tier 1 (`check-comments.sh`,
`check-duplication.sh`) and the Tier 2 doc-claim rule; `.chug/tasks/docs-update.md`,
`.chug/tasks/review-docs-updated.md`, `.chug/tasks/check-modules.sh`,
`.chug/tasks/doc-lint.sh`, `.jscpd.json`; `MODULES.md`;
[#362](362-binary-artifacts.md) (the head-and-slice-table shape to model, and
the `outputs` bucket S0 turns on); [#313](313-workload-identity-image-builds.md)
A2 (the two-age-keys distinction S0 restores); `spec.md` §4.3, §5.1, §10.2.

## Correction — 2026-08-04, job #416 (D8 reversed; M1 and M5 restated)

**D8 was half an answer, and the half it gave was the wrong one.** It conflated
two independent questions: what a knowledge tag *is*, and how attached knowledge
is *delivered*. Turning delivery into a pointer aimed at the same distilled tag
file would have left the second definition — and M5 — exactly where they were,
plus a file read. Job #416 splits them and decides both:

- **What the tag is.** `.chug/tags/north-star-blessed-practices.md` is deleted.
  `knowledge:` now names a repo page by path, so `.chug/jobs/{code,design,docs,web}.yaml`
  declare `STYLE.md` and `NORTH-STAR.md` — the documents that define the rules,
  not a restatement of them. This is D4 applied to the most deliberate violation
  of it in the tree, and it is the part D8 was right that M5 needed.
- **How it is delivered: payload, not a pointer** — the reverse of D8. Once the
  paraphrase is gone the pointer buys nothing and costs a guarantee. Its bytes
  are not saved, only deferred and made conditional: an agent that obeys the
  pointer reads the same 25,974 bytes into an uncached mid-conversation turn,
  and an agent that does not obey it works without the rules. Payload puts them
  in the cached system prefix, where they are certainly present.

**The measured price of the reversal**, which #87 asserted away and D8 inherited:
the composed `## Project Knowledge` block goes from **3,948 to 26,030 bytes**
(+22,082, ~5.5k tokens) per work container, on the four job types that are
essentially every agent job this repo runs. That is the cost of delivering the
definition instead of a summary of it, paid once per prompt prefix against a job
that spends hundreds of thousands of tokens.

The tag mechanism itself survives — `.chug/tags/{tag}.md`, `req.tags.list` and
the UI listing are untouched, and a bare `knowledge:` entry still resolves there.
This repo simply has no tag left to list.

**M1 is now 4, not 7**, and two of the three that went are the substance rather
than the arithmetic. Deleting the tag file took one. The other two are the
payload itself: under A1 the thing that replaces the tag *is* `STYLE.md` and
`NORTH-STAR.md` delivered verbatim, so the stale `dispatcher::state` in
`STYLE.md`'s Tier 2 rule 6 and in `NORTH-STAR.md`'s correctness-core bullet
would have reached every `code`/`design`/`docs`/`web` agent from two files
instead of one — M1's "a stale directive is acted on", now amplified by
delivery. Both read `chuggernaut_domain::state`, which exists. The *bare*
`state.rs` mentions in those two files resolve to `crates/domain/src/state.rs`
and are left alone; `CLAUDE.md`, `testing.md`, `.chug/prompts/work/design.md`
and [#169](169-handoff-continuity.md) are the remaining four and stay with the
truth-pass slice that owns them (this document is a fifth grep hit only because
it quotes the string). **M5 is no longer reproducible**: the file one
side of the `comm -12` reads
is gone, which is the intended outcome rather than a drift in the measurement.
**S8 is done** and did not wait on S3 — a page path is only as good as the page
it names, but `STYLE.md` and `NORTH-STAR.md` are two names in four config files,
so the move updates them the same way it updates its other ~730 references.

### The skew window this opens, and why it is deliberately not gated

`knowledge:` is not a new field. The four configs parse identically on both
sides of the skew boundary; what moved is the *resolution rule* for their
values, and that ships in the dispatcher binary while the configs are read live
from the default branch (spec §14). So between this merge and the `deploy` job
that ships the same SHA, the running N-1 dispatcher reads every entry as a tag
name — appending `.md` to `STYLE.md` — finds nothing, and skips it; with both
entries skipped
the four job types launch with **no `## Project Knowledge` block at all**. The
whole block, not part of it, and silently: the skip logs at `debug!` on that
binary. The window is operator-paced, not automatic — it closes when a `deploy`
job ships, and `deploy` declares no `knowledge:`, so nothing here can delay its
own remedy.

That window is knowingly accepted rather than declared with a
`CONFIG_SCHEMA_EPOCH` bump and `min_dispatcher:`, on §14.1's own test:

- **The N-1 behaviour is graceful degradation, not a dropped constraint.** The
  three gated features are gated because the job runs *wrong* — `inputs:`
  unparameterized, `runtime:` against the image's toolchain,
  `workload_identities:` rejecting the whole config and parking the type. Here
  the job runs as declared with a smaller system prompt, and every Tier 1 rule
  the block carries is machine-enforced regardless by `.githooks/pre-commit` and
  `.chug/tasks/ci.sh`.
- **What survives the window is the pointer.** `CLAUDE.md` reaches the agent
  from the checkout whatever the dispatcher's version, and it names `STYLE.md`
  and `NORTH-STAR.md` as where the rules live. Inside the window, delivery
  degrades to exactly the pointer form D8 preferred — which is the honest reason
  the degradation is tolerable, and not a coincidence.
- **Declaring it costs more than it buys.** `min_dispatcher` above the deployed
  epoch parks every `code`, `design`, `docs` and `web` job pre-Work under
  `config_schema_skew` (§14.2) until an operator retries each one: a hard stop
  on essentially every agent job the repo runs, in exchange for a fuller prompt.
  Trading a graceful degradation for a platform stall inverts what §14 is for.

Recorded rather than left implicit, because §14 exists so that merging config
can never *silently* become deploying config, and this is a change whose N-1
behaviour is silent by construction.
