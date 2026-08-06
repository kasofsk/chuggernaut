# Design #415 — Knowledge architecture: one definition per concept, and prose that cannot go quietly stale

Status: IMPLEMENTED — D1–D15 decided; S0 through S12 all landed, in jobs #416
through #468. One clause of S11 is deferred with cause rather than done: the
`wiki/` prose note it would have consumed is untracked, so it is out of every
job's reach.

The decisions D1–D12 were taken with the operator on 2026-08-04; S8 reversed
[D8](#d8-tags-point-they-do-not-carry) while landing (job #416), S1c landed
as `doc-lint.sh` rule 5 (job #437), and job #436 landed S2, emptying the
path-warning list — see the
[2026-08-05 correction](#correction--2026-08-05-job-436-s2-swept-and-a-third-marker).
S1b landed on 2026-08-05 (job #438): both checks are
`.chug/tasks/check-doc-facts.sh`, whole-tree and blocking on every job — see the
[S1b correction](#correction--2026-08-05-job-438-s1b-landed-and-the-eleven).
S3 landed on 2026-08-05 (job #441): the root holds `README.md` and `CLAUDE.md`
and nothing else — see the
[S3 correction](#correction--2026-08-05-job-441-s3-the-move). Job #443 cleared
that correction's nineteen doc comments and paid the doc-length ratchet they
carried; the gate-scope gap that let them through is **recorded and left open**,
not closed — see the
[#443 finding](#finding--2026-08-05-job-443-the-doc-comment-residue-and-the-gate-scope-it-fell-through).
S5 split while landing (job #444): **S5a** is check 3, the slice ↔ merged-job
check, and it is live — see the
[S5a correction](#correction--2026-08-05-job-444-s5a-check-3-landed-and-s5-split).
**S5b** — the head retrofit — landed on 2026-08-05 (job #445): every
`docs/design/*.md` now carries a `Status:` line, eleven of them carry a slice
table in check 3's shape (nine retrofitted, plus this doc and
[#440](440-native-worker-daemon.md), which already had one), and the three
demoted plans say what they are. What it
deliberately did *not* do is invent a slice table for a doc that had none — see
the [S5b correction](#correction--2026-08-05-job-445-s5b-the-head-retrofit).
S6 landed on 2026-08-05 (job #446): `.chug/tasks/doc-staleness.sh` is the
git-derived ledger, advisory in CI and in the hook, and at that job's base
**30 of the 61 docs that make a file claim** came back suspect — 7 of them by a
day or more. That number moves with every commit and is meant to. It blocks
only on a doc the current diff edits, and [D7](#d7-the-staleness-ledger)'s
pre-commit half of that rule turned out to be unclearable and was not built —
see the
[S6 correction](#correction--2026-08-05-job-446-s6-the-ledger-and-the-block-that-could-not-clear).
The CI half then proved unclearable too, one level up: it escalated jobs #449
and #453, both of which could only be landed by squashing the branch. Job #454
took a `*.md` mover off the **blocking** side of the ledger — a cross-reference
is a pointer, and doc-names-doc is the only edge that can form a cycle — which
makes the gate satisfiable in one rework commit while the advisory reading list
keeps the edge. See the
[#454 correction](#correction--2026-08-06-job-454-d7-the-gate-that-a-rework-could-not-clear).
S4 landed on 2026-08-05 (job #449), last of the original programme:
`docs/concepts.md` holds **12** rows and check 4 is live. Its measured yield is
**one** duplicate in the whole tree *under the two shapes D4 gates* — the value
is preventive, and the head says so rather than claiming a sweep it did not
make. A third shape the corpus uses more than either gated one is
[measured and deliberately unmodelled](#the-shape-d4-did-not-name). `Status:` is still
`IMPLEMENTED IN PART` for a reason the ticket did not have: S9–S12 were appended
after the slice list was written, and none of them is built. See the
[S4 correction](#correction--2026-08-05-job-449-s4-check-4-and-the-yield-it-does-not-have).
S7 landed on 2026-08-05 (job #448): `.chug/tasks/docs-update.md` is rewritten
around [D1](#d1-two-kinds-of-doc-and-only-two)/[D10](#d10-the-implementing-job-owns-the-update)
and `.chug/tasks/review-docs-updated.md` is no longer a placeholder — it blocks
on **three** classes rather than [D9](#d9-the-evaluator-judges-only-what-a-script-cannot)'s
two, and the doc-table paths S7's brief said to fix were already correct —
job #441 had fixed them. See the
[S7 correction](#correction--2026-08-05-job-448-s7-the-instructions-and-the-evaluators-teeth).
Job #450 landed no slice: it deleted every **suite case count** from this head
and from `docs/reference/testing.md`. The `.chug/tasks/check-doc-facts.test.sh`
total had drifted through three values, and two of job #449's reviewers derived
it differently from each other — neither by running it. Running it at the base
job #449 branched from reproduces the standing value the two were jointly
correcting, so the number they condemned had been right for the tree it
described and stale only because the suite had since grown. That is the count
class [check 4 could not own](#what-check-4-cannot-do-and-what-took-its-slot),
and its first real instance argues the reviewer-rule framing is too weak — see
the [#450 finding](#finding--2026-08-05-job-450-a-case-count-two-reviewers-derived-differently).
D13–D15 and S9–S12 were added on 2026-08-05
by job #435, written against the tree at `810a91b`; their three measurements were
read out of that commit and three claims in the ticket that proposed them did not
survive it — see the
[2026-08-05 amendment](#amendment--2026-08-05-job-435-structural-health).
S9 and S10 landed on 2026-08-06 (job #461): `docs/reference/docs.md` states the
policy in the present tense and absorbs `docs/design-docs.md`, which is reduced
to a pointer rather than deleted; `docs/README.md` carries a catalogue row per
tracked doc under `docs/`, and check 5 compares the two sets both ways as an
error in every job's pre-stage — see the
[S9/S10 correction](#correction--2026-08-06-job-461-s9-and-s10-the-policy-doc-and-the-catalogue).
S11 landed on 2026-08-06 (job #467): `docs/overview.md` is the synthesis page,
written as [D13](#d13-the-synthesis-page-is-a-reference-doc) asks — routing
tables whose left cell is a question and whose right cell is the doc that
answers it, so a line on the page cannot go stale unless the doc it links stops
answering. Its row's second clause was **not** done and is not claimed: the
`wiki/` prose note it names is untracked, and an untracked file is the
operator's to move, not a job's — see the
[S11 correction](#correction--2026-08-06-job-467-s11-the-synthesis-page-and-the-wiki-clause-that-could-not-run).
S12 landed on 2026-08-06 (job #468), last of the programme to be built:
`.chug/tasks/doc-staleness.sh` reports each doc's inbound-reference count and
**zero of the 41 docs under `docs/` is an orphan** at that base. The catalogue
is excluded as a referrer — the decision that keeps the check from being
constant-true — and the count needs a second route, `doc-lint.sh --emit-links`,
because `docs/design/` cites its siblings by relative link rather than by
backticked path. With S11 already merged, that is every slice, and `Status:`
moves for the first time — see the
[S12 correction](#correction--2026-08-06-job-468-s12-the-orphan-half-and-the-two-routes).

Measured against the tree at `28e5aa1` (2026-08-04). Every number below was read
out of that commit, not carried over from the brief; the commands are given so a
reader can re-run them rather than trust them. The branch was later rebased onto
`69e48b2`; the M-table still reproduces at the stated sha, and the reference
counts in [what the move costs](#what-it-costs-honestly) were re-measured at
`d781496` and are labelled there. Two figures shifted at the new base: #313 grew
to 1,428 lines (M6 → 16,055), and `docs/spec.md`'s age-key line moved from 2201 to
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
| **D3** | `docs/concepts.md` is an **index of pointers**, not a glossary — a concept keeps its natural home | [D3](#d3-the-concept-registry-routes-it-does-not-hold)  <!-- intent --> |
| **D4** | Ban duplicate **definitions**; allow duplicate **mentions** | [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions) |
| **D5** | `CLAUDE.md` may **gloss and link**, never define | [D5](#d5-claudemd-may-gloss-never-define) |
| **D6** | Four mechanical checks in one pure-shell `check-doc-facts.sh`, resolved against **git, not the filesystem** — [D15](#d15-structural-health-index-completeness-and-orphans) adds a fifth, live since job #461 | [D6](#d6-four-mechanical-checks) |
| **D7** | A **git-derived staleness ledger** marks docs *suspect*, not wrong | [D7](#d7-the-staleness-ledger) |
| **D8** | ~~Knowledge tags become **pointers, not payload**~~ — **reversed while landing** (job #416, [S8](#slices)): knowledge is delivered as **payload**, and `.chug/tags/` is empty | [D8](#d8-tags-point-they-do-not-carry), superseded by the [2026-08-04 correction](#correction--2026-08-04-job-416-d8-reversed-m1-and-m5-restated)  <!-- absent --> |
| **D9** | `review-docs-updated` gets **narrow, blocking** teeth | [D9](#d9-the-evaluator-judges-only-what-a-script-cannot) |
| **D10** | The **implementing job** updates the design doc it implements | [D10](#d10-the-implementing-job-owns-the-update) |
| **D11** | A **ratchet, not a flag day** | [D11](#d11-a-ratchet-not-a-flag-day) |
| **D12** | Every doc lives under `docs/`; the root keeps only `README.md`, `CLAUDE.md` and `wiki/`. `.chug/` prose stays put and every rule reaches it; `wiki/` is diagrams, not knowledge | [D12](#d12-where-everything-lives) |
| **D13** | A **synthesis page** — a doc that states no new fact and decides nothing — is **reference**, bound by [D5](#d5-claudemd-may-gloss-never-define)'s gloss-and-link rule verbatim. It lives at `docs/overview.md`, live since job #467 | [D13](#d13-the-synthesis-page-is-a-reference-doc) |
| **D14** | The doc **policy** is a reference doc (`docs/reference/docs.md`, absorbing `docs/design-docs.md`); this document keeps the **argument** | [D14](#d14-the-policy-is-reference-415-is-the-argument) |
| **D15** | **Structural health** is a second axis, and two mechanisms, both live: **check 5** compares `docs/README.md`'s catalogue against the tree both ways (blocking, job #461); the ledger reports **inbound-reference count**, zero being a finding (advisory, job #468) | [D15](#d15-structural-health-index-completeness-and-orphans) |

### The target tree

```
README.md              project entry point (absorbs INSTALL.md)
CLAUDE.md              agent working notes — gloss and link only (D5)
docs/
  README.md            docs index (absorbs NORTH-STAR.md's routing role)
                       + catalogue: one row per tracked doc (D15, check 5)
  overview.md          the synthesis page — gloss and link only (D13)
  spec.md              normative
  concepts.md          the registry (D3)
  reference/           present tense, no history, no status
    docs.md            the doc policy, present tense; absorbs design-docs.md (D14)
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
| **S0** | Triage `progress.md`: amend `docs/spec.md` §5.1 (the artifact-store clause is false) and §10.2's single-age-key claim, repoint `.claude/skills/chug/SKILL.md`, then delete the file | **Landed** (job #432), in that order. §5.1 now states the NATS-internal store and keeps the S3/Minio deferral as its own claim; §10.2 names both age identities (§12.1 and the infrastructure appendix follow); the skill points at `docs/spec.md` and the repo's docs |
| **S1a** | Fix `doc-lint.sh` rule 4's four false-positive classes in place and resolve against git; the two markers. Still a warning, still `docs`/`design`-scoped | **Landed** (job #433) — the classes turned out to be five, and the whole-tree count fell 313 → 66. See [S1a as landed](#s1a-as-landed-re-measured) |
| **S1b** | Move that logic into `check-doc-facts.sh` — pre-stage, every job, whole-tree, **error** — and delete rule 4 from `doc-lint.sh`. Once S2 has cleared the real findings | **Landed** (job #438) — both checks moved, ~0.6s whole-tree; the eleven constant mismatches S2 left standing had to be dated first. See the [S1b correction](#correction--2026-08-05-job-438-s1b-landed-and-the-eleven) |
| **S1c** | Check 2 (constant values) + a `.test.sh` suite covering both | **Landed** (job #437) — check 2 is rule 5 of `doc-lint.sh`, a warning like check 1, and found **11** real mismatches across 6 design docs. See [S1c as landed](#s1c-as-landed) |
| **S2** | The one-time sweep: the ~6 real path findings, the `*_SCHEMA_EPOCH` restatements, `state.rs` in its seven files; delete `spec_original.md` and `wiki/Welcome.md` | **Landed** (job #436) — 66 → **0** path warnings whole-tree; added the third marker `absent`; both files deleted. See the [2026-08-05 correction](#correction--2026-08-05-job-436-s2-swept-and-a-third-marker)  <!-- absent --> |
| **S3** | **The move** — every doc into `docs/`, per D12; updates the ~730 references (283 of them outside markdown, 134 for `docs/reference/style.md` alone) and `check-modules.sh`'s own path | **Landed** (job #441) — fourteen `git mv`s in one commit, the runbooks folded into `docs/reference/runbooks/` with them. See the [S3 correction](#correction--2026-08-05-job-441-s3-the-move) |
| **S4** | `docs/concepts.md` + the seed concept set + check 4 (definitional shape) | **Landed** (job #449) — 12 rows, one owner each; check 4 reads D4's two shapes, narrowed so a term must OPEN a sentence, and found exactly **one** duplicate under them. See the [S4 correction](#correction--2026-08-05-job-449-s4-check-4-and-the-yield-it-does-not-have) |
| **S5a** | Check 3 (slice ↔ merged job) — the detector for the drift this head suffered twice | **Landed** (job #444) — one shape, `**Landed** (job #N)` in a `docs/design/*.md` table row, resolved against the `job/N:` squash-merge subject; a doc with no slice table is silent. See the [S5a correction](#correction--2026-08-05-job-444-s5a-check-3-landed-and-s5-split) |
| **S5b** | Design-doc heads retrofitted (D2), plans demoted to design docs (D1) | **Landed** (job #445) — 22 docs given a `Status:` line, nine given a lifted slice table, no table invented for a doc that had none. See the [S5b correction](#correction--2026-08-05-job-445-s5b-the-head-retrofit) |
| **S6** | The staleness ledger (D7) | **Landed** (job #446) — `.chug/tasks/doc-staleness.sh`, sharing check 1's extractor through a new `--emit-paths` mode; **30 of 61** docs suspect, 7 of them by a day or more. Directory claims are out and the pre-commit block is not built, both on measurement. See the [S6 correction](#correction--2026-08-05-job-446-s6-the-ledger-and-the-block-that-could-not-clear) |
| **S7** | `docs-update.md` rewritten around D1/D10; `review-docs-updated` given D9's teeth | **Landed** (job #448) — `docs-update.md` now opens on the reference/design split and makes the D10 head update an author's step; `review-docs-updated.md` is blocking on **three** classes, not two — D10 compliance joined the two D9 names, because check 3 exempts the landing job. See the [S7 correction](#correction--2026-08-05-job-448-s7-the-instructions-and-the-evaluators-teeth) |
| **S8** | Tags become pointers (D8) — **reversed while landing**: `.chug/tags/` is empty, and the four job types name `docs/reference/style.md` / `docs/README.md` in `knowledge:`, delivered as payload rather than as a pointer | **Landed** (job #416), which replaced job #87 (now Revoked) rather than being it, and did not wait on S3. The reversal is argued in the 2026-08-04 correction at the end of the body; [D8](#d8-tags-point-they-do-not-carry) as written above it is superseded  <!-- absent --> |
| **S9** | `docs/reference/docs.md` — the policy as present-tense rules ([D14](#d14-the-policy-is-reference-415-is-the-argument)); absorbs `docs/design-docs.md`, which the target tree above omits ([M9](#three-more-measurements)) | **Landed** (job #461) — the policy doc states D1–D7, D10 and D15 in the present tense; `docs/design-docs.md` keeps its path as a pointer, because four of its inbound references sit in append-only bodies and in code |
| **S10** | `docs/README.md` gains a one-line catalogue row per tracked doc; **check 5** compares catalogue ↔ tree both ways, `check-modules.sh`'s shape ([D15](#d15-structural-health-index-completeness-and-orphans)) | **Landed** (job #461) — the catalogue covers every tracked `docs/**/*.md` including itself, and check 5 fails both ways while skipping an unparseable row in silence |
| **S11** | `docs/overview.md` — the synthesis page ([D13](#d13-the-synthesis-page-is-a-reference-doc)); any `wiki/` prose note resolved into it and reduced to a link | **Landed** (job #467) — the page is seven routing tables and a discipline note, and states no fact that is not behind a link. The `wiki/` clause is **deferred, not done**: the only prose note there is untracked, so it is out of every gate's reach and out of a job's — the same verdict `security-assessment.md` carries. See the [S11 correction](#correction--2026-08-06-job-467-s11-the-synthesis-page-and-the-wiki-clause-that-could-not-run) |
| **S12** | The staleness ledger also reports inbound-reference count; zero is a finding ([D15](#d15-structural-health-index-completeness-and-orphans)) | **Landed** (job #468) — **0 of 41** docs under `docs/` are orphans, the catalogue excluded as a referrer and two routes counted; the population is `docs/` alone and the referrers are every tracked `*.md`, both on measurement. See the [S12 correction](#correction--2026-08-06-job-468-s12-the-orphan-half-and-the-two-routes) |
| — | `security-assessment.md` (1,147 lines, untracked) | **Out of scope**; its own job |

**All sixteen rows are landed — S0, S1a, S1b, S1c, S2, S3, S4, S5a, S5b, S6, S7, S8, S9, S10, S11 and S12.**
No row is intent any longer, so none now carries the marker docs/reference/style.md's doc-claim
rule — which this
document is partly written to make enforceable — asks of one. This sentence read *six* and omitted
S3 until job #444 corrected it, four jobs after S3 merged: a count in the head is
exactly the class of claim [check 4 cannot own](#what-check-4-cannot-do-and-what-took-its-slot),
so it is a reviewer's to catch.

This head went stale **within a day of merging**: job #416 landed S8 on
2026-08-04, appended its correction to the body per [D10](#d10-the-implementing-job-owns-the-update),
and left the table above still saying nothing was implemented and still naming
job #87 as live work. That is the drift **check 3** (slice ↔ merged job,
[S5a](#slices)) exists to catch, and it did not exist yet — so a human had to
notice. Check 3 now exists (job #444) and **would still not have caught either
half**: both are *under*-claims — a head saying `PROPOSED — no slice
implemented` over a slice that had merged, and a row naming #87 as future work
after it was revoked — and check 3 resolves claims that a job *did* land. The
correction is recorded rather than argued away in
[the S5a correction](#correction--2026-08-05-job-444-s5a-check-3-landed-and-s5-split);
the under-claim is [D7](#d7-the-staleness-ledger)'s and
[D9](#d9-the-evaluator-judges-only-what-a-script-cannot)'s, not check 3's.

### S1a as landed, re-measured

Measured on the base S1a landed on, both figures from the same command —
`git ls-files '*.md' | xargs sh .chug/tasks/doc-lint.sh`, counting lines matching
`referenced path not found`: **313 before, 66 after**, both re-measured on the
merged base `6ec4891` after [S0](#slices) deleted `progress.md` (at the original
S1a branch point they were 330 and 64). The
verdict is now byte-identical in an unbuilt checkout and in one carrying
`target/`, `web/dist`, `deploy/dev/data/`, `deploy/prod/chuggernaut.env` <!-- runtime -->
— **66 either way**, where the old check reported 313 unbuilt against 301 built.
That is the divergence [resolving against git](#resolve-against-git-not-the-filesystem)
was for. Both markers work and are line-scoped; `.chug/tasks/doc-lint.test.sh` grew
cases covering the classes, the two markers and the git-not-filesystem
property.

Three corrections to what is written below, all discovered by re-measuring:

- **The classes are five, not four.** A token rooted somewhere other than this
  checkout — `src/api.ts` from `web/CLAUDE.md`, `dispatcher/tests/execution.rs`
  from `docs/reference/testing.md` — is neither a glob, an absolute path, a template nor a
  citation, and it was the largest silent class. It is refused by requiring the
  first segment to be a tracked top-level entry.
- **The `.chug/tags/` directory is not in the tree** <!-- absent --> — job #416 deleted its one
  file. What S1a read as eleven stale citations is two different things, and S2
  separated them: the **tag mechanism is live** (`.chug/tags/{tag}.md`,
  `req.tags.list`, `types::config_paths`), so a citation of the *location* is a
  correct claim about a product interface that this repo happens not to
  populate, and only a citation of the deleted
  `north-star-blessed-practices.md` is stale. The first is written in its
  generic form, the second is marked `absent`.
- **A doc that names a path *because it does not exist* is a third case, and S2
  decided it** — 41 of the 66 warnings were in this document, which cites the
  stale paths as its subject. Neither `intent` nor `runtime` is true of such a
  line, and there is no rewrite: a measurement of staleness cannot be recorded
  without writing the path it measured. Hence the third marker,
  `<!-- absent -->`, defined in [S1a's marker set](#two-markers-not-one) and in
  docs/reference/style.md's doc-claim rule. [S1b](#slices)'s whole-tree promotion is no longer
  blocked on this question.
- **A path that resolves in another repo takes no marker.** `infra/README.md`
  named beacon's workload-identity terraform root as a bare path, which implies
  this tree; it is now written `kasofsk/beacon:infra/gcp-workload-id/`. Rewriting
  beats a fourth marker because the bare path misleads a human reader too — the
  marker would have fixed only the checker.

### S1c as landed

Check 2 landed as **rule 5 of `.chug/tasks/doc-lint.sh`**, beside check 1 rather
than in a new script, because [S1b](#slices) moves both together — and it did:
both checks and the cases covering them now live in `.chug/tasks/check-doc-facts.sh`
and `.chug/tasks/check-doc-facts.test.sh` (job #438), whole-tree and blocking.
`.chug/tasks/doc-lint.test.sh` — which #433 created for check 1 — covered both
in well under CI's 60s per-suite cap. **No case count is stated here**:
`.chug/tasks/check-doc-facts.test.sh` prints its own `passed N, failed N` total
on every run, and transcribing it is the
defect recorded in the
[job #450 finding](#finding--2026-08-05-job-450-a-case-count-two-reviewers-derived-differently).

Re-measured on this base rather than carried from [M3](#the-problem-measured):
`*_SCHEMA_EPOCH` is mentioned **105 times across 16 markdown files**
(`git grep -o '_SCHEMA_EPOCH' -- '*.md'`), up from the 92/13 measured at
`28e5aa1`. Of those, check 2 reports **11 real mismatches**, in six design docs
— #308 (2), #311 (1), #313 (2), #321 (1), #322 (4), #355 (1) — and **no false
positives** over all 70 tracked `.md`. They are left standing: [S2](#slices)
owns the sweep and decides which are stale claims and which want
`<!-- intent -->`. S2 landed and left all eleven standing — every one is a dated
statement inside an append-only body, correct as history and wrong only as
present tense; see the
[2026-08-05 correction](#correction--2026-08-05-job-436-s2-swept-and-a-third-marker).
Two of the eleven are the ones this design was written
about: #313's restatement of #311's Skew section, and #322's audit note.

Three design decisions this slice owned, all resolved toward silence:

- **A value assertion is one of two line-scoped shapes**, and nothing else: the
  value inside the backticks (`` `NAME = 5` ``, `` `NAME: u32 = 5` ``, a quoted
  `pub const` line), or the bare name in backticks followed immediately by
  `= 5` / `== 5`, `is`/`is currently`/`is already`/`is now` `5`, a `(currently 5)`
  that closes on the value, or `| 5 |` as the very next table cell. A **`2 → 3`
  transition, a `bump … to 5`, a past tense, a bound and a value on the next
  line are all skipped** — `bump … to 5` in particular names a *target*, so it
  is right by construction until a later bump makes it wrong, and warning on it
  would fire on correct intent. This is
  [M7](#the-problem-measured)'s lesson applied before the first run rather than
  after it.
- **The tree's side is an index of every integer-literal `pub const` in a
  tracked `.rs` file**, built with `git grep` for the same reason check 1 reads
  `git ls-files`. A name resolving to **no** const is silent (a design doc's
  proposed constant is not a claim about this tree); a name resolving to **two**
  consts that disagree is silent (there is no way to pick); an
  expression-valued const (`16 * 1024 * 1024`) never enters the index, so a doc
  stating its arithmetic result is not second-guessed.
- **A number written as a word is out of scope** and stays so. "epoch five" is
  rare here and parsing it buys noise, not signal.

Both markers suppress check 2 on the line that carries them, because they
already suppress check 1 there — and `<!-- intent -->` is the right escape for a
doc naming the epoch a slice will bump *to*.

---

## The record

*Append-only from here. Corrections are appended with a date and never edited
into the prose above them.*

## The problem, measured

Not "docs drift" in the abstract. Seven measurements at `28e5aa1`:

| # | Measurement | Value |
| --- | --- | --- |
| M1 | Files naming `crates/dispatcher/src/state.rs` or `dispatcher::state` — **which does not exist** | **7**  <!-- absent --> |
| M2 | Repo-relative path claims in markdown / of those, not tracked in git | **292 / 20** |
| M3 | `*_SCHEMA_EPOCH` mentions in markdown, against one `pub const` in `crates/types/src/version.rs` | **92**, across **13** files |
| M4 | `spec_original.md`: lines / inbound references / last touched | **1,251 / 0 / 2026-07-20** |
| M5 | Lines shared between `.chug/tags/north-star-blessed-practices.md` — a file job #416 has since deleted — and `docs/reference/style.md`, which defined the same rules | **2 of 60**  <!-- absent --> |
| M6 | `docs/design/`: docs / total lines / largest | **17 / 16,024 / 1,440** |
| M7 | `doc-lint.sh` "referenced path not found" warnings on a whole-tree run / of those, real | **256 / ~6** |

**M1 is the one that matters most, and it is not cosmetic.** The seven files are
`CLAUDE.md`, `docs/reference/style.md`, `docs/reference/testing.md`, `docs/README.md`,
`.chug/prompts/work/design.md`, `.chug/tags/north-star-blessed-practices.md` — <!-- absent -->
gone since, deleted by job #416 — and `docs/design/169-handoff-continuity.md`.
CLAUDE.md's mention is **normative** —
"`dispatcher::state` and release validation are the correctness core — keep
their branch coverage near-total" — so the file every agent reads first
instructs it to protect a module that is not there. A stale count is
embarrassing; a stale directive is acted on.

**M7 is the finding that reframes the work, and it was found last.** A path
check already exists — `.chug/tasks/doc-lint.sh` rule 4 — and it has named
`crates/dispatcher/src/state.rs` as unresolvable ever since the module was <!-- absent -->
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
docs/reference/style.md both define this repo's blessed practices, and they share **two lines
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
`threshold: 0`, docs/reference/style.md Tier 1 — so point it at prose. That does not work, and
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
  no status line, no "we decided". `docs/spec.md`, `docs/reference/testing.md`, `docs/reference/crates.md`,
  `docs/reference/contracts.md`, `docs/reference/modules.md`, `docs/reference/style.md`, `docs/reference/design-lifecycle.md`,
  `docs/reference/structure-assessment.md`, the runbooks.
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

`docs/concepts.md` maps concept → owning `doc#anchor`. It does **not** contain <!-- intent -->
the definitions.

The alternative — lift every definition into a glossary — makes
[D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions)'s check trivial
(any definition outside the glossary is a violation) and was rejected anyway.
A definition divorced from the argument that motivates it is worth less: "the
dispatcher is the single writer" means something in docs/reference/style.md, surrounded by the
reasoning about why single-threaded state management is a design constraint
rather than a performance note. Extracted into an alphabetical list it becomes a
sentence to memorize.

The registry is `docs/reference/modules.md`'s shape, and `.chug/tasks/check-modules.sh` is the
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

That narrow rule catches the real instance. `docs/reference/style.md:231` and
`.chug/tags/north-star-blessed-practices.md:50` — since deleted by job #416 — <!-- absent -->
both opened with the identical string
`- **Single writer.** The dispatcher is the only writer of job records,`
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

One pure-shell `.chug/tasks/check-doc-facts.sh`, in the pre-stage beside <!-- intent -->
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
`target/`, `web/dist` and `deploy/dev/data/` — build output that exists on a <!-- runtime -->
developer's machine and not in CI.

> **The check resolves against `git ls-files`.** A filesystem check makes the
> gate's verdict depend on whether the caller has run `cargo build`, which is
> the precise divergence `check-modules.sh`'s header exists to prevent.

### Two markers, not one

The 20 untracked claims are three different things, and collapsing them would
force authors to lie:

- **Stale** (10) — `crates/dispatcher/src/state.rs`, `crates/channel/`, <!-- absent -->
  `crates/forge-ingest`, `crates/platform-ops/templates/smoke/`, <!-- absent -->
  `.chug/tags/backend.md`, `docs/design/epics.md`, `docs/BOOTSTRAP.md`, <!-- absent -->
  `deploy/prod/BOOTSTRAP.md`, `deploy/prod/install-systemd.sh`, <!-- absent -->
  `deploy/prod/systemd/`. These are the bug. Fix them. <!-- absent -->
- **Intent** (4) — `.chug/images.yaml`, `.chug/images/`, `.chug/schedules/`, <!-- intent -->
  `.claude/CLAUDE.md`. Designed, not built. `<!-- intent -->`.
- **Runtime** (6) — `web/dist`, `target/`, `deploy/dev/data/`, <!-- runtime -->
  `deploy/dev/out/…`, `deploy/prod/chuggernaut.env` (operator-owned, on the <!-- runtime -->
  Mini), `deploy/dev/data/keys/claude.token`. Correctly absent from git and <!-- runtime -->
  correctly named in docs. `<!-- runtime -->`.

The `intent` marker is what finally gives docs/reference/style.md's existing doc-claim rule
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
`Review` profile (`docs/spec.md` §4.3) — no `cargo`, no `npm`. A doc claim that can
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

1. **A crate status table** (7 status rows) — a *third* copy of what `docs/reference/crates.md`
   and `docs/reference/modules.md` own. Job #408 updated a row in it. Delete; those docs own it.
2. **A session narrative** — its header says "Update it at the end of each
   implementation session" and `**As of:** 2026-07-18 (session 9)`, declaring
   itself 17 days stale while being edited today. Git history owns this.
3. **One unamended spec deviation, still live.** Line 397 records two "spec
   deviations to amend". The `age_artifacts` one **was** amended — `docs/spec.md`
   names that key in three places. The other was not: `docs/spec.md:1563` still reads
   "No separate artifact store for v1", which is now false twice over, once for
   the NATS-internal artifacts store and once for the `outputs` bucket
   [#362](362-binary-artifacts.md) S1/S2 landed. A smaller sibling:
   `docs/spec.md:2217` still says "**the** age private key is dispatcher-only",
   singular, when the bucket rows name two — the distinction
   [#313](313-workload-identity-image-builds.md) A2 leaned on.

And `.claude/skills/chug/SKILL.md` tells every agent to read `progress.md` "for
dispatcher internals rather than guessing" — the M1 failure again, in the skill
this time.

So S0 is: amend `docs/spec.md`, repoint the skill, **then** delete. In that order.

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
| `.chug/prompts/`, `.chug/tags/{tag}.md`, `.chug/tasks/*.md` | **stay** | The platform reads them from `.chug/` (`docs/spec.md` §1.1); moving them breaks the product, not a link. Every rule still reaches them — below |
| `wiki/` | **stays, reclassified** | Diagrams, not prose; `Welcome.md` deleted — below |

### `.chug/prompts/` — stays put, and every rule reaches it

The work and review prompts are not exempt prose. They are read by **every**
agent on **every** job, they carry path claims, and `.chug/prompts/work/design.md`
is one of M1's seven files — line 31 hands each design job
`crates/dispatcher/src/state.rs` as its worked example of a path citation, so <!-- absent -->
the prompt teaching agents to cite paths accurately cites a module that does not
exist.

They do not move: the dispatcher loads them from `.chug/` by spec §1.1, and the
path is a product interface, not a link. What applies to them:

- **Check 1, no special case.** Neither `doc-lint.sh` nor the check that replaces
  it applies any directory filter — the selection block
  (`.chug/tasks/doc-lint.sh:42-79`) picks `*.md` and nothing narrower — so
  `.chug/prompts/`, `.chug/tags/{tag}.md` and the root docs are one population whenever
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
added 2026-07-19 in `ee94657`, before `docs/spec.md` existed), two default `*.base`
view stubs, and `Welcome.md` — four lines of Obsidian's shipped boilerplate,
unedited. It has **zero** inbound references from anywhere in the tree
(`git grep 'wiki/'` outside this doc returns nothing), and its only change since
creation was job #364 moving two canvas nodes.

The name is actively misleading: `docs/spec.md` §9.4 says **the `docs/` tree is the
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
job-type YAML, Rust doc comments. `docs/reference/style.md` alone accounts for **134
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
  the moving names; 134 for `docs/reference/style.md` alone) are outside its globs, which lint
  only `*.md`.

## Related

Job #86 (superseded — its five-section brief is still the right scope), #87 (the
tag half, unrun); jobs #375, #378, #382, #383, #385, #394 (the corpus docs/reference/style.md's
doc-claim rule was written from), #409 (the most recent instance, and the one
that prompted this); `docs/reference/style.md` Tier 1 (`check-comments.sh`,
`check-duplication.sh`) and the Tier 2 doc-claim rule; `.chug/tasks/docs-update.md`,
`.chug/tasks/review-docs-updated.md`, `.chug/tasks/check-modules.sh`,
`.chug/tasks/doc-lint.sh`, `.jscpd.json`; `docs/reference/modules.md`;
[#362](362-binary-artifacts.md) (the head-and-slice-table shape to model, and
the `outputs` bucket S0 turns on); [#313](313-workload-identity-image-builds.md)
A2 (the two-age-keys distinction S0 restores); `docs/spec.md` §4.3, §5.1, §10.2.

## Correction — 2026-08-04, job #416 (D8 reversed; M1 and M5 restated)

**D8 was half an answer, and the half it gave was the wrong one.** It conflated
two independent questions: what a knowledge tag *is*, and how attached knowledge
is *delivered*. Turning delivery into a pointer aimed at the same distilled tag
file would have left the second definition — and M5 — exactly where they were,
plus a file read. Job #416 splits them and decides both:

- **What the tag is.** `.chug/tags/north-star-blessed-practices.md` is deleted. <!-- absent -->
  `knowledge:` now names a repo page by path, so `.chug/jobs/{code,design,docs,web}.yaml`
  declare `docs/reference/style.md` and `docs/README.md` — the documents that define the rules,
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
payload itself: under A1 the thing that replaces the tag *is* `docs/reference/style.md` and
`docs/README.md` delivered verbatim, so the stale `dispatcher::state` in
`docs/reference/style.md`'s Tier 2 rule 6 and in `docs/README.md`'s correctness-core bullet
would have reached every `code`/`design`/`docs`/`web` agent from two files
instead of one — M1's "a stale directive is acted on", now amplified by
delivery. Both read `chuggernaut_domain::state`, which exists. The *bare*
`state.rs` mentions in those two files resolve to `crates/domain/src/state.rs`
and are left alone; `CLAUDE.md`, `docs/reference/testing.md`, `.chug/prompts/work/design.md`
and [#169](169-handoff-continuity.md) are the remaining four and stay with the
truth-pass slice that owns them (this document is a fifth grep hit only because
it quotes the string). **M5 is no longer reproducible**: the file one
side of the `comm -12` reads
is gone, which is the intended outcome rather than a drift in the measurement.
**S8 is done** and did not wait on S3 — a page path is only as good as the page
it names, but `docs/reference/style.md` and `docs/README.md` are two names in four config files,
so the move updates them the same way it updates its other ~730 references.

### The skew window this opens, and why it is deliberately not gated

`knowledge:` is not a new field. The four configs parse identically on both
sides of the skew boundary; what moved is the *resolution rule* for their
values, and that ships in the dispatcher binary while the configs are read live
from the default branch (spec §14). So between this merge and the `deploy` job
that ships the same SHA, the running N-1 dispatcher reads every entry as a tag
name — appending `.md` to `docs/reference/style.md` — finds nothing, and skips it; with both
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
  from the checkout whatever the dispatcher's version, and it names `docs/reference/style.md`
  and `docs/README.md` as where the rules live. Inside the window, delivery
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

## Amendment — 2026-08-05, job #435 (structural health)

Written against the tree at `810a91b`. Every number below was read out of that
commit; three claims in the ticket that asked for this amendment did not survive
the reading and are corrected in place rather than carried over. Nothing here is
implemented — [S9](#slices) through [S12](#slices) are intent, and no script,
catalogue or page named below exists yet.

The branch was then merged onto `08473e3`, which landed [S1c](#s1c-as-landed).
Every measurement below was re-run there: M9's line count and the orphan loop are
unchanged, the `design-docs.md` reference count moved 7 → 8 exactly as the
footnote below predicts, and one `path:line` citation moved again and is
relabelled where it appears.

### The second axis

Everything above measures one property: whether a doc's claims about the tree
are still **true**. [Check 1](#check-1-already-exists-precision-before-teeth)
resolves its paths, check 2 its constants, check 3 its slice claims, and
[D7](#d7-the-staleness-ledger) marks it suspect when the tree moves underneath
it. All four ask the same question of a document the reader has already found.

None of them asks whether the reader can find it. A doc can be perfectly true,
perfectly current, cited by nothing, listed nowhere, and read by no one — and
every gate in this document passes it. That is **structural health**, and it is
a different axis from factual truth, not a corner of it.

The prompt was reading this document against Karpathy's LLM-wiki pattern
(<https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f>). Most of
that pattern is rejected below, and for one structural reason: it is written for
a wiki over **immutable** sources, where the corpus is the ground truth and the
only question is retrieval. Here the ground truth is a tree that moves under the
prose, so D6 and D7 remain the harder and more important half. What the pattern
does contribute is the navigability axis and its two instruments — an index that
must be complete, and orphans — and those are worth taking.

### Three more measurements

Continuing the [table above](#the-problem-measured), at `810a91b`:

| # | Measurement | Value |
| --- | --- | --- |
| M8 | `docs/design/323-paste-a-prompt-onboarding.md`: lines / inbound references by filename / by `#323` / status / last touched | **1,330 / 0 / 0 / `PROPOSED` / 2026-07-30** |
| M9 | `docs/design-docs.md`: lines / files that reference it / appearances in [D12](#d12-where-everything-lives)'s target tree | **147 / 7 / 0** |
| M10 | Design docs in `docs/design/` with **zero** inbound references by either route | **1 of 18** |

```sh
git grep -l -- 323-paste-a-prompt-onboarding.md | grep -v '^docs/design/323-'
git grep -nE '#323([^0-9a-fA-F]|$)'
git grep -l 'design-docs\.md' | wc -l
grep -c 'design-docs' docs/design/415-knowledge-architecture.md
for f in $(git ls-files 'docs/design/*.md'); do b=$(basename "$f"); s=${b%%-*}
  printf '%s %s %s\n' "$b" \
    "$(git grep -l -- "$b" -- ":!$f" | wc -l)" \
    "$(git grep -lE "#$s([^0-9a-fA-F]|\$)" -- ":!$f" | wc -l)"
done
```

**Those commands stop reproducing the moment this amendment merges, and that is
the point rather than a defect.** Every figure above is at `810a91b`, the base
this section was written against. From the merge onward the M8 greps return this
file, the `design-docs.md` count is 8 rather than 7, and the target tree in the
head names `design-docs.md` once — because writing the two of them down is the
smallest possible instance of the fix [S9](#slices) and [S10](#slices)
generalize. The [2026-08-04 correction](#correction--2026-08-04-job-416-d8-reversed-m1-and-m5-restated)
carries the same footnote for M1, which this document is "a fifth grep hit only
because it quotes the string". And being cited is not the same as being
reachable: #323's sole inbound reference is now a design doc arguing that
nothing references it.

**M8 is [M4](#the-problem-measured) recurring, and that is the finding.** M4 is
`spec_original.md` — 1,251 lines, zero inbound, untouched — and it was found by
a hand-written query while writing this document. #323 was found by re-running
that same query four days later for an unrelated reason. Nothing between the two
findings noticed; nothing would have. Under the plan as it stands
[S3](#slices) moves the file, [check 1](#where-check-1-runs-and-over-what)
verifies its paths and [D7](#d7-the-staleness-ledger) may mark it suspect — the
whole apparatus operating correctly on a document no one can reach.

The `#323` grep is worth stating precisely, because the naive form lies: bare
`git grep '#323'` returns one hit, `web/src/styles.css:226`, which is the hex
colour `#32302f`. Word-bounded, the count is zero.

**Correction 1: M8's stated cause does not hold.** The ticket attributed the
orphaning to #323's title line — `# Design — paste-a-prompt onboarding: …`, with
no `#NNN` — described as unlike every sibling. The tree says otherwise. **Five**
of the eighteen omit the number: #309, #322, #323, #361 and #362. The other four
are cited **16, 7, 6 and 7** times by filename and **27, 13, 7 and 21** times by
number. A missing `#NNN` in the title makes a doc marginally harder to cite; it
demonstrably does not make it uncitable, and four counterexamples is not a
mechanism. So M10 replaces the causal claim with the population it belongs
to: #323 is the only orphan in the corpus, and the next-lowest is
[#169](169-handoff-continuity.md) at 1 by name and 2 by number.

That correction matters for the design rather than for the record. If the cause
had been the title shape, the fix would be a naming rule — a cheap regex in
[`doc-lint.sh`](../../.chug/tasks/doc-lint.sh) rule 4's neighbourhood. It is
not. Nothing distinguishes #323 mechanically except that no document names it,
which is a **set** property and needs a set comparison to see.

**Correction 2: M9's omission has a sibling, deliberately not slice'd here.**
`docs/design-docs.md` is the design-doc header contract, cited by
`.chug/prompts/work/design.md`, `.chug/tasks/review-design.md`,
`docs/implementation-notes.md`, `web/src/pages/Designs.tsx` and three design
docs — seven files, one of them the prompt every design job reads. The target
tree names it zero times. It is not decided against and not deferred; it was not
seen. The same is true of `docs/reference/runbooks/`, which the target tree relocates to
`docs/reference/runbooks/` <!-- intent --> while [D12](#d12-where-everything-lives)'s
move table has no row for it. One omission is an oversight; two in a
fourteen-row table is the argument for check 5, and this document is the second
instance of its own subject in four days.

**Correction 3: the ticket's third measurement is not reproducible here, and the
reason is load-bearing.** It described `wiki/Chuggernaut Structure Notes.md` <!-- runtime -->
as a 179-line untracked note colliding with `docs/reference/style.md`'s definition of *single
writer*. That file is not in this checkout and has never been in this
repository's history — `git log --all -- 'wiki/*Structure*'` is empty, and
`git status --porcelain wiki/` is clean. Untracked work does not reach a job
container. The figure is therefore recorded as **operator-reported and
unverified**, not as a measurement.

Two things the tree does confirm, both of which survive the correction:

- The `docs/reference/style.md:231` citation was `docs/reference/style.md:247` at `810a91b` and is
  `docs/reference/style.md:253` at `08473e3` — the `- **Single writer.**` line moved twice, the
  second time because job #437 added six lines above it while this amendment was
  being written, so the number went stale between drafting the bullet and
  merging it. That citation appears in
  [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions) above, in the
  append-only body, and stays as written; it is the third `path:line` citation
  in this document to go stale during this document's own lifetime, after the
  two the preamble at the top already names, and the only one to go stale twice.
  It is also the third reason check 1 verifies the file and never the line.
- A **tracked** definitional-shape collision for the same term exists today:
  `docs/design/293-worker-capacity.md:116` opens a numbered list item
  `**Single writer.** The dispatcher owns the fleet record` — D4's registered
  shape, a paraphrase rather than a copy, inside `docs/`. Check 4 has a live
  target without needing anyone's vault.

And the correction sharpens D13 rather than weakening it. A note that exists
only as an untracked file on an operator's machine is invisible to every gate
here — including check 5, which resolves against `git ls-files` by
[D6](#d6-four-mechanical-checks)'s rule. The demand for a synthesis page cannot
be met by extending a check's reach, because the writing that evidences the
demand is out of reach by construction. It is met by giving the prose somewhere
in `docs/` to go.

Finally, M10 answers one of the [refutation triggers](#what-would-refute-this-added)
added below before it is asked: the orphan signal fires on **1 doc in 18** at
this base, not on most of them. Sharp today, and re-measurable by the loop in the
block above.

### D13. The synthesis page is a reference doc

[D1](#d1-two-kinds-of-doc-and-only-two)'s "two kinds and no third" stands, and
nothing here adds a third. What the target tree lacks is anywhere to put a
document that **states no new fact and decides nothing** — whose entire value is
holding the system in one reading, so that someone arriving cold gets the shape
before the detail. `docs/README.md` <!-- intent --> is an index: it routes, it
does not narrate. `docs/spec.md` <!-- intent --> is normative and long. CLAUDE.md
front-loads what bites you, which is a different job from explaining what the
thing is.

The evidence that the demand is real, and that having nowhere to put it does not
suppress it, is the note of [Correction 3](#three-more-measurements) — operator-reported
rather than measured, and the demand it evidences is not contingent on its line
count. Someone wrote that prose into the **one directory
[D12](#d12-where-everything-lives) exempts**. That is precisely the failure D12's
own rule anticipates — *"if a note there ever states a fact about the system,
that fact belongs in `docs/` and the note becomes a link."* A rule that is
violated the first time the need arises is a rule with a missing destination,
not a rule that needs enforcing harder.

**Resolution: a synthesis page is reference.** Present tense, no history, no
status line — [D1](#d1-two-kinds-of-doc-and-only-two)'s reference rule
unmodified. And it is bound by
[D5](#d5-claudemd-may-gloss-never-define)'s rule **verbatim**: one line of gloss
plus a link to the owner, never a second definition.

The argument is CLAUDE.md's, and stronger by construction. CLAUDE.md gets the
rule rather than an exemption because [M1](#the-problem-measured)'s most damaging
instance lives in it. A synthesis page has no other purpose than restating other
docs, so it is the single document in the tree most likely to grow a competing
definition. The doc whose whole job is restatement is the last one that should
be trusted to restate freely.

It lives at `docs/overview.md` <!-- intent -->.

**[D12](#d12-where-everything-lives)'s `wiki/` exemption survives unweakened.**
It was written for diagrams, and D12 argues correctly that a canvas is not a
document. But that exemption only holds if prose has somewhere else to go —
otherwise the exempt directory is the path of least resistance, which is exactly
what [Correction 3](#three-more-measurements) reports happening. D13 supplies the
destination; D12's rule keeps its teeth.

### D14. The policy is reference; #415 is the argument

After [S1](#slices) through [S8](#slices) land, there is still **no present-tense
statement of what the doc rules are**. An agent asking "how do I update a design
doc" has one place to look: this file. Which by then is 900-plus lines in which
[D8](#d8-tags-point-they-do-not-carry) is reversed by a correction 300 lines
below it, and its head — the thing
[D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head) built to bound
exactly this reading cost — carries slice status, not rules.

That is D2 unapplied to its own author's output. D2 bounds *knowing where things
stand*. It does not bound *knowing what the rules are*, because a design doc is
the wrong container for a rule: an append-only decision record is optimized for
"why did we choose this", and a rule needs to be readable in the present tense
without its history.

So the split follows [D1](#d1-two-kinds-of-doc-and-only-two) rather than
inventing anything:

- **`docs/reference/docs.md` <!-- intent -->** holds the rules, present tense:
  D1's two kinds, D2's head/body discipline, D4's mention-vs-definition line,
  D5's gloss rule, [D13](#d13-the-synthesis-page-is-a-reference-doc), and what
  each check rejects.
- **This document keeps the argument** — the measurements, the rejected
  alternatives, the record of why. Nothing is deleted from it.

It **absorbs `docs/design-docs.md` as a section rather than deleting it**. That
file is already reference-shaped and already correct; per M9 it is cited by seven
files, one of which is the design work prompt. Deleting it would break the
contract every design job is handed, and leaving it beside a policy doc would put
the header contract in two places, which is [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions)
against itself.

Its seven inbound references are [S3](#slices)'s problem either way — S3 already
rewrites ~730 of them. Folding the absorption into [S9](#slices) means the file
moves once, with S3, rather than landing in `docs/reference/` <!-- intent -->
and then being merged again.

### D15. Structural health: index completeness, and orphans

Two mechanisms, kept deliberately separate, because one is a **set comparison**
with a right answer and the other is a **signal** with a judgement attached.

**Check 5 — blocking, in `.chug/tasks/check-doc-facts.sh` <!-- intent -->.**
`docs/README.md` <!-- intent --> gains a catalogue: every tracked doc, one row,
link plus a one-line summary. Check 5 compares both directions — no tracked
`docs/**/*.md` without a row, no row without a file — and names every offender.

The design is already written and does not need re-deriving.
`modules_registry_compare` (`.chug/tasks/check-modules.sh:57-75`) is the loop:
both directions, and it sets `_gate_failed` rather than exiting, so one run
reports every drift instead of the first. [D3](#d3-the-concept-registry-routes-it-does-not-hold)
already names that script as the gate to copy for the concept registry. The docs
catalogue is the *same instrument at a different target*, which is why
[S4](#slices) and [S10](#slices) should be written together rather than
reinvented apart — one comparison function, three callers.

**Check 5 cannot catch M8 by itself, and that is stated rather than glossed.** A
catalogued doc can still be cited by nothing; completeness of an index is not
reachability. What check 5 makes impossible is M8's *cause* — writing the
catalogue row is the moment a missing `#NNN`, a title that names no number, or a
doc no one can summarize in one line becomes visible to a human. It converts a
silent absence into a required act of authorship.

**The orphan signal — advisory, in the ledger.** Per doc, the count of files
elsewhere naming it; zero is a finding. Entirely git-derived, exactly like
[D7](#d7-the-staleness-ledger): no new syntax, no front matter, nothing to keep
in step, and the same verdict vocabulary — ***suspect*, not wrong**.

It stays advisory for D7's own stated reason: *a ledger that fails builds for
history nobody caused is a ledger people disable.* An orphan is often nobody's
fault and sometimes correct — a `PROPOSED` design doc may be uncited by
construction, because the jobs that would cite it have not run. Blocking on that
would train the same scroll-past reflex the
[2% true-positive gate](#the-problem-measured) already demonstrated in this repo.

The two are not redundant, and the split is the point: check 5 answers *is it
catalogued* — a question with a mechanical right answer, so it blocks. The
ledger answers *is it read* — a question whose right answer depends on
intent, so it reports.

### The slices, and why they pair

[S9](#slices) through [S12](#slices) are in the head's slice table. Two
things about their ordering are decisions rather than bookkeeping:

- **S9 gates on [S5](#slices), not just [S3](#slices).** The policy doc states
  D2's head discipline as a present-tense rule, and S5 is what retrofits that
  discipline onto the existing heads. Writing the rule before the corpus obeys it
  would ship a reference doc that is false on arrival — this document's subject,
  committed by the document that names it.
- **[S10](#slices) and [S4](#slices) share an implementation.** Both are
  `check-modules.sh`'s both-directions compare pointed at a different registry.
  Written apart they become two loops that drift; written together they are one.

[S11](#slices) gates on [S4](#slices) because a synthesis page bound by D5's rule
needs the concept registry to link *into* — gloss-and-link requires an owner to
link to, and D3's registry is what supplies the address.

### Rejected alternatives, structural half

**A `log.md` — an append-only chronological record of ingests, queries and lints,
with a parseable prefix.** Rejected: this is `progress.md`, which
[S0](#s0-progressmd-is-three-things-fused) deleted on the finding that git
history already owns it. The `job/N` commit prefix is a better key than a
hand-maintained one — it is written by the platform rather than by an agent that
may forget — and check 3 already resolves slice claims against it. Adopting a
log would recreate, under a new name, the file this plan removed three days ago.

**A search index (`qmd` or equivalent).** Rejected on scale. At `810a91b` the
corpus is **70 tracked `*.md` totalling 30,566 lines**, of which **24 files and
20,022 lines** are under `docs/` — `git ls-files '*.md' | xargs wc -l`. Against an
agent that already has `git grep` over a checkout it holds locally, an index at
that size buys nothing. It also costs more than it looks: to be *relied on* it
would have to exist in `.chug/tasks/ci.sh`, in `.githooks/pre-commit` and in
every task container, which is a toolchain dependency in three places for a
corpus one `grep` reads in milliseconds. Revisit if the catalogue itself grows
past what a reader will read — a threshold check 5 makes observable, since the
catalogue's length *is* the doc count.

**YAML front matter — `kind:`, tags, Dataview-style queries.** Rejected as
redundant. The path already encodes the kind: `docs/design/` is a design doc and
`docs/reference/` <!-- intent --> is a reference doc, by
[D12](#d12-where-everything-lives)'s tree, and `doc-lint.sh` rule 4 already
enforces the design half's filename shape. A `kind:` field would be a second copy
of a fact the filesystem holds — which is this document's subject, committed in
the machinery meant to prevent it. Same argument that rejected
[`last-verified:`](#rejected-alternatives), minus the broken promise.

**Pointing the Obsidian vault at `docs/`** — graph view over the real corpus,
which would render [D3](#d3-the-concept-registry-routes-it-does-not-hold)'s
registry and D15's orphans directly and interactively. **Not rejected, and not a
slice.** It is `.obsidian/` configuration, operator-owned, affecting no gate and
no agent, so it needs no decision here. Recorded only because M8 is precisely the
finding a graph view surfaces at a glance: an unconnected node, visible without
running anything.

### What would refute this, added

Continuing [the triggers above](#what-would-refute-this).

- **Check 5's catalogue is maintained by rote and its summaries decay.** Then the
  row is bookkeeping without value, and the answer is to shrink check 5 to
  presence only — dropping the summary column and keeping the set comparison.
  [D7](#d7-the-staleness-ledger) covering `docs/README.md` <!-- intent --> is the
  detector: a catalogue whose own file goes stale while the docs it lists move is
  the exact shape the ledger reports.
- **The orphan signal fires on most design docs.** Plausible by construction — a
  `PROPOSED` doc may be uncited because the work has not started. Currently
  refuted (M10: 1 of 18), and re-measurable. If it does happen, scope the signal
  to docs whose last commit is older than the ledger's staleness window, so it
  fires on the conjunction — old *and* uncited — rather than on either half.
- **`docs/overview.md` <!-- intent --> grows definitions anyway.** Then
  [D13](#d13-the-synthesis-page-is-a-reference-doc)'s borrowed
  [D5](#d5-claudemd-may-gloss-never-define) rule is insufficient for a document
  whose entire purpose is restatement, and the answer is that the synthesis page
  is **generated from the catalogue** rather than written — not a looser rule. A
  page assembled from `docs/README.md` <!-- intent -->'s rows cannot hold a
  definition, because it holds nothing that was not already a link.

## Correction — 2026-08-05, job #436 (S2 swept, and a third marker)

**66 → 0**, measured whole-tree at both ends with
`git ls-files '*.md' | xargs sh .chug/tasks/doc-lint.sh`, on base `810a91b`.
S1a's decomposition held for most of the list and was wrong about three things,
each of which changed what the fix had to be.

### What was actually stale, and what replaced it

| Stale claim | Replaced by |
| --- | --- |
| `crates/dispatcher/src/state.rs` — `CLAUDE.md`, `docs/reference/testing.md` (×2), `.chug/prompts/work/design.md`, [#169](169-handoff-continuity.md) | `crates/domain/src/state.rs` — the module moved to the pure crate under refactor-plan C1 <!-- absent --> |
| `.chug/tags/backend.md` in the `code` project template | `.chug/tags/{tag}.md`, the generic form; the template teaches a *consumer's* layout <!-- absent --> |
| `.chug/schedules/` as a bare directory ([#310](310-scheduled-jobs.md)) | `.chug/schedules/*.yaml` — the shape `schedules.rs` and `ci.sh` actually read | <!-- absent -->
| beacon's `infra/gcp-workload-id/` (`infra/README.md`) | `kasofsk/beacon:infra/gcp-workload-id/` — repo-qualified | <!-- absent -->

Everything else resolved to a marker: `intent` for [D12](#d12-where-everything-lives)'s
target tree and for [#323](323-paste-a-prompt-onboarding.md)/[#355](355-project-task-images.md)'s
unbuilt proposals, `runtime` for build output and operator-owned files, `absent`
for the rest.

### Three findings that corrected the plan

- **`dispatcher::state` was never broken.** M1 counts files naming
  `crates/dispatcher/src/state.rs` **or** `dispatcher::state` and calls the <!-- absent -->
  module non-existent; `CLAUDE.md`'s normative line was cited as the worst
  instance. But `crates/dispatcher/src/lib.rs:45` re-exports
  `chuggernaut_domain::{… state}`, so `dispatcher::state` resolves. Only the
  **file path** was stale. The directive was pointing at real code the whole
  time — the docs now name `crates/domain/src/state.rs` as the definition site
  and say the re-export exists, because a reader who greps the dispatcher's
  `src/` for the file still finds nothing.
- **The `.chug/tags/*` citations were not eleven stale references.** Job #416
  deleted `north-star-blessed-practices.md`, not the tag mechanism — its own
  correction says so explicitly. Citing the deleted file is stale; citing
  `.chug/tags/{tag}.md` is a correct claim about a live product interface that
  this repo does not populate. Marking the second class stale, or the first
  class intent, would each have been a lie in a different direction.
- **No `*_SCHEMA_EPOCH` number outside an append-only design body is wrong.**
  M3's 92 mentions across 13 files decompose into symbolic references
  (`docs/spec.md`, `CLAUDE.md`, `docs/implementation-notes.md` — no number to rot)
  and dated statements inside design bodies, which are correct *as history*.
  The one present-tense number in a reference doc, `infra/README.md`'s
  `CONFIG_SCHEMA_EPOCH` (5), is right. So S2 fixed none — the exposure is real
  but it is [S1c](#slices)'s mechanical check, not a hand-edit. S1c landed
  (job #437) while this branch was open and **confirms the reading**: rule 5
  reports 11 mismatches, all eleven inside append-only design bodies, none in a
  reference doc. The conclusion was reached by hand and is now checked.

### The third marker

`<!-- absent -->` — a line that names a path *because it does not exist*. The
head's [S1a findings](#the-problem-measured) flagged that 41 of the 66 warnings
were in this document and that neither existing marker fit; the reason no
rewrite fits either is that a measurement of staleness cannot be recorded
without writing the path it measured. `intent` would claim these paths are
coming, `runtime` that they exist on a machine; both are false. Its guard
against becoming a general silencer is that a reader who deletes the marker must
still read the line as asserting the path is gone.

**That guard binds this document's own body, and three lines failed it on the
first pass.** M5, the M1 file list, and the [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions)
worked example each named the deleted `.chug/tags/north-star-blessed-practices.md` <!-- absent -->
in the present tense and carried an `absent` marker — a live claim silenced,
which is the one move this slice was written to prevent. A [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head)
body cannot be rewritten to dodge it, but it can be **annotated**: each now says
the file is gone and who deleted it, leaving the argument untouched. That is the
shape [#169](169-handoff-continuity.md)'s edge citation already took. An
append-only body is not a licence to leave a false present-tense claim standing
behind a marker — if the sentence must keep the path, the sentence says what
happened to it.

A path resolving in **another repo** was the fourth case S1a did not anticipate.
It takes no marker: qualify it (`{repo}:{path}`) or write the per-project config
slot generically. A marker would have satisfied the checker and left the prose
misleading, which is the wrong half of the problem to solve.

### A [D10](#d10-the-implementing-job-owns-the-update) failure, one job after the rule merged

**Ten of the twelve** `.chug/tags/*` warnings were **created by job #416** — it
deleted `north-star-blessed-practices.md` and left the references behind: six
citations of the directory, which had no other file, plus four of the file
itself (one carrying a `:50` line citation). The brief for this job counted
eleven; the twelfth and the two it miscounted are the `code` template's
`backend.md`, which `git log --all -- .chug/tags/backend.md` shows **never
existed in this repo** — it is an example of a *consumer's* tag file, so #416
did not orphan it and it is the cross-repo case, not a D10 failure.

D10 says the implementing job updates what its change made stale; #416 merged
after D10 did. The rule existed and was not followed, which is evidence for
[S1b](#slices)'s teeth rather than against them: D10 is a rule an agent can mean
to follow and still miss, and rule 4 at whole-tree-error would have caught all
ten at the gate.

## Correction — 2026-08-05, job #438 (S1b landed, and the eleven)

Both checks are `.chug/tasks/check-doc-facts.sh` now — whole tree, every job's
pre-stage, exit non-zero on any finding — and `doc-lint.sh` carries neither.
What they *decide* is byte-identical to what rules 3 and 5 decided the moment
before the move: the same 0 path findings and the same 11 constant findings over
the same 68 files. The suite followed the code into
`.chug/tasks/check-doc-facts.test.sh` (47 cases, 0.27s), `doc-lint.test.sh`
keeps the 9 for the three checks it still owns, and the repo's shell-suite count
is 20.

### The gate was met for check 1 and not for check 2

The brief for this job read [S2](#slices)'s "66 → **0**" as clearing the way for
both checks. It cleared check 1. Check 2 still reported the **eleven**
[S1c](#s1c-as-landed) found and S2 deliberately left standing, and eleven
findings in the tree is a promotion that fails every job in the fleet on its
first run — the exact outcome [D6](#d6-four-mechanical-checks)'s "fix precision
first, then promote" was written to prevent.

So the eleven were dated before the promotion landed, by the remedy the
[#436 correction](#correction--2026-08-05-job-436-s2-swept-and-a-third-marker)
already established for an append-only body: not a marker, and not a rewrite of
the argument, but **the tense**. A present-tense `CONFIG_SCHEMA_EPOCH` assertion
became a dated one — `is 2` became `was 2 when this landed`; a past-tense or
dated statement is not a claim about today's tree, so the check is silent on it
and a reader is no longer misinformed. Two lines in
[#322](322-macos-native-runtime.md) were proposals rather than history — the
`RUNTIME_SCHEMA_EPOCH` it proposed at 3 landed as `4` in job #401 — and now say
so, which is the same annotation move applied to a value instead of a path.

That the eleven were left for a later slice is not a criticism of S2: S2's own
conclusion was that none of them was a *stale claim* in a reference doc, and it
was right. What it missed is that "correct as history" and "safe under a
whole-tree error gate" are different properties, and only the second one is what
[S1b](#slices) needed. A finding a gate cannot afford to leave standing is a
finding, whatever the tense of the sentence carrying it.

### The hook, measured

`--staged` and rejecting, per [D6](#d6-four-mechanical-checks): **0.16s** for the
six markdown files this job staged, against the hook's ~2s budget. D6's gap
stands — a commit that only *deletes* a path other docs name passes the hook and
fails CI. It is now a smaller price than D6 assumed: the whole-tree run is
**0.62s**, so closing the gap is affordable if it ever bites, and the reason to
leave it scoped is D6's decision rather than the cost.

### Two guards, both pinned

A tree the check cannot judge exits **2** — a `LINTER ERROR`, distinct from both
verdicts, following `check-comments.sh`'s precedent. That matters more here than
there: this gate is now fatal fleet-wide, so "the check did not run" must never
render as "the docs are clean". The pre-commit hook translates that 2 into a
loud skip rather than a blocked commit, and both halves have cases
(`check-doc-facts.test.sh`, `.githooks/pre-commit.test.sh`). The other guard is
[M7](#the-problem-measured)'s: an unparseable or unclassifiable token is skipped
**silently**. Ten cases pin the classes that must stay quiet, because a noisy
gate is an off gate and the noise is now fatal.

## Correction — 2026-08-05, job #441 (S3, the move)

S3 landed in one commit. The root now holds `README.md` and `CLAUDE.md` and
nothing else; `INSTALL.md` became `README.md`, `NORTH-STAR.md` became
`docs/README.md`, and the rest went where [D12](#d12-where-everything-lives)'s
table says. The two plans took the number of the job that wrote them —
`docs/design/215-refactor-plan.md` and `docs/design/210-ts-rewrite-plan.md` —
which is the existing convention and the only one that does not invent a
sequence. Their [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head)
heads and slice tables are [S5](#slices)'s work, not this commit's.

### The runbooks moved too, and the count was larger than the table

[Correction 2](#correction--2026-08-05-job-436-s2-swept-and-a-third-marker)
noticed that D12's move table has no row for `docs/runbooks/` while the target <!-- absent -->
tree relocates it. This commit resolved that in the tree's favour: the four
runbooks are `docs/reference/runbooks/`, folded in here so they move once. That
added 35 references (20 in markdown, 15 outside — `nix/chug-node/options.nix`,
three `deploy/prod/` scripts, `crates/types/src/rollup.rs`, two skills) to the
fourteen names' own **1,017** (709 in markdown, 308 outside), counted at
`1a67105` with a token-exact regex rather than a substring grep. That is 287
above [what it costs](#what-it-costs-honestly)'s ~730, and the reason is the
measurement, not the tree: the earlier figure excluded relative forms
(`../../STYLE.md`) and this one does not.

### What the gate could not see, and what was run instead

[S1b](#slices)'s `check-doc-facts.sh` reads `*.md`, so it was blind to 308 of
the references and to every hard-coded path in a gate. Three things covered
that half:

- `git grep -nP` per old name, token-exact, across **every** tracked file. The
  residue is 31 lines in this document plus the 19 doc comments below. The 31
  are D12's move table, D1's "today the root carries", the target-tree fence,
  the `comm -12` command from [M5](#the-problem-measured), and this correction's
  own prose and table. Those name the pre-move paths *because that is what they
  measured*; rewriting them would edit an append-only body into a tautology
  (`spec.md → spec.md`). Everywhere else the count is zero.
- The gates that hard-code a path were repointed and re-run:
  `.chug/tasks/check-modules.sh` reads `docs/reference/modules.md`, and its
  fixture in `.chug/tasks/modules-registry.test.sh` builds the directory the
  registry now lives in.
- `.chug/jobs/*.yaml`'s `knowledge:` entries are resolved by
  `crates/dispatcher/src/knowledge.rs` as repo-relative paths, so the four job
  types now name `docs/reference/style.md` and `docs/README.md`. An entry that
  no longer resolves is a job that starts with an empty knowledge block, which
  is the failure S3 was most able to cause and the one no gate would have
  reported.

### The ratchet that stopped 19 references, and why they stayed

`check-comments.sh` rule 2 — the two-sentence doc-comment cap — is a **ratchet**:
it reports a block only when the diff adds a line inside it, so ~500 over-long
doc comments are grandfathered until something touches them. Nineteen of them
contain a path reference this move had to rewrite, and a one-word path edit
un-grandfathers the whole block. Clearing them means deleting or rehousing real
prose in `crates/dispatcher`, `crates/types`, `crates/worker` and three test
guards — four of which (`crates/types/src/job.rs` ×2, `job_type.rs`, `task.rs`)
are the source of five `.chug/schemas/*.json` descriptions and three
`web/src/api/types.gen.ts` ones, which are emitted from those comments and so
move only when they do.

That is a `code` job's change, and putting it inside a 133-file move would make
the move unreviewable, which is the one thing the slice could not afford. So the
rewrite was **reverted inside exactly those nineteen blocks** and they still name
the pre-move basenames:

| File | Names |
| --- | --- |
| `crates/dispatcher/src/eval.rs`, `crates/dispatcher/src/exec.rs` (×2) | `contracts.md` (×2), `STYLE.md` |
| `crates/dispatcher/src/handlers/groups.rs` (×2), `crates/dispatcher/src/handlers/jobs_reply.rs` | `STYLE.md` (×2), `design-lifecycle.md` |
| `crates/dispatcher/src/invariants.rs`, `crates/dispatcher/tests/common/mod.rs`, `crates/dispatcher/tests/groups.rs` | `contracts.md` (×2), `refactor-plan.md`, `STYLE.md` |
| `crates/domain/src/decide/work.rs`, `crates/worker/src/backend.rs` | `STYLE.md` (×2) |
| `crates/test-utils/tests/boundary_guard.rs`, `crates/test-utils/tests/lint_guard.rs` | `STYLE.md` (×2) |
| `crates/types/src/job.rs` (×2), `job_type.rs`, `task.rs` | `design-lifecycle.md` (×4) |
| `crates/types/src/platform.rs`, `crates/types/src/rollup.rs` | `STYLE.md` (×2) |

The generated artifacts carry the pre-move basename too, and for the same reason:
`.chug/schemas/api.schema.json`, `.chug/schemas/job-type.schema.json` and
`web/src/api/types.gen.ts` are emitted from those four comments, so they were
regenerated from the reverted sources rather than hand-edited to the new paths.
`committed_schemas_are_current` in `crates/cli/src/schema.rs` asserts byte
equality between the committed files and what the types generate, so the pair
cannot drift: the comment and its schema description move together, in the
`code` job that pays the ratchet's debt, or not at all.

They are stale prose, not a broken link: nothing resolves them, and no gate reads
them — `check-doc-facts.sh` scans `*.md` only, so no gate will report them
decaying either. The finding is that **`docs/` and the doc-length ratchet
interact**, which neither [D6](#d6-four-mechanical-checks) nor this slice
predicted — a rename cannot reach a grandfathered comment without paying that
comment's debt.

### Two things left standing on purpose

`docs/design-docs.md` did not move — [S9](#slices) absorbs it into
`docs/reference/docs.md` <!-- intent --> and moving it first would move it
twice. And `docs/README.md` is still `NORTH-STAR.md`'s prose under a new name:
it routes, which is why D12 sends it here, but the catalogue that makes it an
index is [S10](#slices)'s.

## Finding — 2026-08-05, job #443 (the doc-comment residue, and the gate scope it fell through)

The nineteen doc comments
[#441 left standing](#the-ratchet-that-stopped-19-references-and-why-they-stayed)
now name the post-move paths. Section anchors were kept — "STYLE.md Tier 2 #3"
became "docs/reference/style.md Tier 2 #3" — because the rule number is the half
a reader needs and only the document half was ever broken. One citation named
`refactor-plan.md`, which S3 moved to `docs/design/215-refactor-plan.md`; it
travelled with the rest.

### Paying the ratchet was the change; the rename was nineteen tokens

The substitution changes no sentence count — `.md` is not a terminator to
`check-comments.sh`'s scanner — so nothing was *pushed* over the two-sentence
cap. Every one of the nineteen blocks was already 3–6 sentences and
grandfathered, and the edit is simply what made rule 2 look at them: all
nineteen failed the gate on the first commit. So the debt #441 deferred was paid
here rather than deferred again. All nineteen now sit at two sentences or fewer,
seven bullets and one new `crates/types/src/platform.rs` section carry the prose
that would not compress into `docs/implementation-notes.md`
([D12](#d12-where-everything-lives)'s archive), and the four schemars
descriptions among them were **regenerated** into
`.chug/schemas/api.schema.json`, `.chug/schemas/job-type.schema.json` and
`web/src/api/types.gen.ts` rather than hand-edited — `committed_schemas_are_current`
asserts byte equality either way.

### The gate's scope is markdown, and that is a limit, not an oversight

Every one of the nineteen survived a green `.chug/tasks/check-doc-facts.sh` on
every job since S1b. That is the check working as specified:
[D6](#d6-four-mechanical-checks) scopes it to `*.md`, because the thing it set
out to verify is a **doc's** claim about the tree. The residue lives outside that
scope, and outside it is the larger half. Measured on this branch with `git grep
-o`, `docs/reference/style.md` appears **122** times inside markdown and **149**
times outside it; `docs/reference/contracts.md`, **32** inside and **50**
outside, all fifty of them in `.rs`. For the documents the rules actually live
in, most references to them are somewhere no gate reads.

### A citation is not a path claim, so check 1 does not extend to it

Widening check 1 to `*.rs` verbatim would be the wrong repair. Check 1 resolves
a **path claim**: a backticked, repo-relative path a doc asserts exists. None of
the nineteen made one. "STYLE.md Tier 2 #3" is a **citation** — a document named
by title, unbackticked, in prose, beside the section that carries the rule, in a
comment that never had to resolve to compile. A rule that caught it would have to
be citation-shaped: it would need the set of documents that may be named by
title, it would have to treat the section anchor as part of the token rather than
as noise, and it would have to stay silent about the hundreds of ordinary `docs/`
paths already in those same files. Different corpus, different token grammar,
different false-positive profile — a second check, not a wider first one.

Whether that belongs in an existing slice, in a new one, or nowhere at all is
**left open for the operator**. The argument against is real: a stale citation is
a wrong pointer, not a broken build, and the whole-tree `git grep` a rename
already runs found all nineteen without any gate's help. This job deliberately
did not touch `check-doc-facts.sh` — it blocks every job in the fleet, and
widening its corpus on the back of a cleanup is how a gate acquires a false
positive nobody budgeted for.

### What still carries the pre-move names, on purpose

`STYLE.md`, `contracts.md`, `design-lifecycle.md` and `refactor-plan.md` survive
in markdown in exactly one file — this one — across
[D1](#d1-two-kinds-of-doc-and-only-two) and D12's move tables,
[M5](#the-problem-measured)'s `comm -12` command, #441's residue table, and this
section. They name the pre-move paths because that is what they measured, and
rewriting an append-only body into `spec.md → spec.md` would turn a measurement
into a tautology. That is
[#441's ruling](#what-the-gate-could-not-see-and-what-was-run-instead) and it
holds unchanged.

## Correction — 2026-08-05, job #444 (S5a: check 3 landed, and S5 split)

**S5 was two jobs, and only the first is here.** Check 3 is
`.chug/tasks/check-doc-facts.sh`'s third rule, live on every job at the same
pre-stage as checks 1 and 2 and with the same verdict — an error. The head
retrofit and the plan demotion are S5b and are untouched: they want the check to
exist first, so the shape they write is verified as it lands rather than
audited afterwards.

**One shape, and it is the shape S5b should write:** `**Landed** (job #N)` in a
cell of a markdown table row, in `docs/design/*.md`. That was not a choice
between #362's convention and #440's — the check matches the **row**, not a
column, and the two conventions differ only in which column carries the state
(#440 declares a `State` column; #362 and this document's head put it in the
gate cell). Matching the row covers both without inventing a third.
`Shipped (job #N)` is accepted as the synonym [#373](373-project-toolchains.md)
already uses, and its `(job #384, <sha>)` is why the job number is read
without requiring the closing parenthesis. Nothing else is a claim: a bare
`**Landed**`, a job number that is not `#<digits>`, the same sentence in prose
rather than a table row, and any markdown outside `docs/design/`.

**Absent is absent — decided, not overlooked.** A slice naming a job that was
**Revoked** and one naming a job that never existed produce the same finding,
because git records neither. The distinction is real (the row naming revoked #87
above is the instance) and only the platform API knows it, which is precisely
why the gate does not reach for it: job #421 is the precedent, where a gate that
asks an API it may not be able to reach degrades to a silent pass. Absent is the
safer verdict and the remedy is identical either way — the row is wrong, and its
author rewrites it.

**The job doing the landing is exempt**, or [D10](#d10-the-implementing-job-owns-the-update)
would be unsatisfiable: the row and the merge commit are the same commit, so
`job/N` cannot exist when CI gates it. Its number comes from `$JOB_ID`, set in
every task container, and from a `job/N` branch name in a local checkout —
never from the network. This row is the first user of that exemption.

**What it does not catch, stated rather than implied.** Check 3 resolves claims
that a job *did* land. Both drifts recorded in the head above are the opposite
shape — a `Status: PROPOSED — no slice implemented` head over a slice that had
merged, and a row naming #87 as future work after its revocation — and **check 3
would have caught neither.** An omission makes no claim for git to contradict.
Detecting it needs the doc-to-commit association nothing in the tree records,
which is [D7](#d7-the-staleness-ledger)'s ledger (a doc older than the paths it
names is *suspect*) and [D9](#d9-the-evaluator-judges-only-what-a-script-cannot)'s
reviewer. D6's check 3 is implemented exactly as it is written there; the
half of the motivating failure it reaches is the `Status: IMPLEMENTED` rule,
which fires only on the strict word — `IMPLEMENTED IN PART`, this head's own
status, is not it.

**Silence is the design, and it is most of the corpus.** Measured at this
commit: 22 design docs, of which **4 carry a slice claim at all** — #362 with 3
claims, #373 with 2, #440 with 1 and this one with 8 (including the S5a row
above), 14 in total, every one of them resolving. The other 18 produce no
records; M7's lesson is
that a gate which guesses is a gate somebody turns off, and check 3 is now an
error in the pre-stage of every job, so a false positive stops the fleet rather
than annoying an author. It stands down whole, rather than reporting every row,
when the history holds no `job/N:` commit or the checkout is shallow — with no
index to resolve against, refusing is the only safe verdict.

Cost, re-measured at this branch's merge with `e41c294`: the whole-tree run is
**0.80s** against 0.78s for the same script without check 3, best of three on
this container over 69 tracked `*.md`. `check-doc-facts.test.sh` is
**63 cases in 0.45s**, of which 16 are check 3's and 10 of those assert silence:
no slice table, four unparseable rows, prose, `IMPLEMENTED IN PART`, a table
with no landed row, markdown outside `docs/design/`, and a history holding no
`job/N:` commit at all.

## Correction — 2026-08-05, job #445 (S5b: the head retrofit)

**Every design doc now carries a `Status:` line, and the corpus is 22.** The
three that had none were the plans [D1](#d1-two-kinds-of-doc-and-only-two)
demoted — `000-rationale.md`, `210-ts-rewrite-plan.md`, `215-refactor-plan.md` —
and two of the three are **dormant**, which is now what their status says rather
than something a reader infers from a git date. Design #210 is superseded and
unexecuted; #215 was partly executed and has had no owner since 2026-07-31;
`000-rationale.md` is foundational and not normative. Marking either plan `PROPOSED` would have been
the comfortable lie: it reads as scheduled work.

**Scoped by what each doc already had, and the boundary was the whole point.**
Nine docs had a sequencing, phase or slice table and got a head slice table
lifted from it in check 3's shape (designs 293, 308, 309, 313, 322, 362, 367,
372 and 373). The lift is a *state* table — slice id, a one-line what, and
the state — with the body's own table left untouched and linked, because several of
those rows are a paragraph each and duplicating them would double the doc to say
nothing new. **No slice table was invented.** [#169](169-handoff-continuity.md)
is the case that decided the rule — its Part 6 is nine prioritized tickets with
sizes, which looks exactly like a slice table until you try to fill in the state
column: no ticket carries a job number and nothing maps a `T`-label to one, so
its head says that instead of guessing.

**One decisions table was lifted and the rest of the decision half was left
undone — this is the retrofit's other boundary.**
[D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head) asks the head
to carry a decisions table as well as a slice table, and one doc already had
one: [#313](313-workload-identity-image-builds.md)'s dated `Decisions taken,
2026-08-04` is a four-row table whose own preamble says it exists "so a slice can
cite one line rather than re-read a section", so D1–D4 were lifted into its head
as written, minus the `Rejected, and why it stays` column — that stays written
once, in the body. Nowhere else was there a table to lift.
[#373](373-project-toolchains.md) has five numbered `Decision` sections and
[#362](362-binary-artifacts.md) two; four of #373's five headings state the
verdict in the heading, as both of #362's do, but the reasoning under them is pages
of argument with no one-line rationale to carry up, so those rows would have to
be *written* rather than lifted — the same fabrication the invented-slice-table
rule forbids one column over. [#440](440-native-worker-daemon.md) has a
decisions table only because its author wrote it with the doc, which is what D2
expects going forward. The other eight tier-A heads therefore carry the state
half and not the decision half; filling it in is a per-doc rewrite by somebody
who can re-read the argument, and it is left undone deliberately rather than
overlooked.

**Four docs were deliberately not touched.** [#310](310-scheduled-jobs.md),
[#311](311-job-inputs.md) and [#321](321-job-groups.md) already open with a
multi-line `Status:` naming every shipping job and what stayed deferred — the
head's job, done under a different name — and [#440](440-native-worker-daemon.md)
already carries a decisions table and a `State`-column slice table. Converting
any of them would have been a heading rename over prose that is already correct.

**Two findings the retrofit produced that no gate could have.** First, a phase
can be satisfied by somebody else's job: [#322](322-macos-native-runtime.md)'s W1
and N2 are what #309 P0 (job #434) and job #401 landed for
[#373](373-project-toolchains.md), so this document's two cheapest macOS phases
were paid for at container scope by tickets that never cited it. Second, a slice
can land *short of what it asked for* and read as done —
[#372](372-chug-node-modules.md) slice 1 shipped `flake.nix` and `nix/chug-node/`
in job #383 but not the `nix flake check` stage in the same bullet, so nothing in
this repo's CI evaluates those modules. Both are under-claims of the kind the
[S5a correction](#correction--2026-08-05-job-444-s5a-check-3-landed-and-s5-split)
says check 3 cannot see; both are now written down where a reader will hit them.

**Measured after the retrofit**, same command shape as S5a's count: **11** of the
22 design docs carry a slice claim, up from 4, and **46** rows resolve against a
`job/N:` squash-merge commit, up from 14. `check-doc-facts.sh` is clean
whole-tree over 69 markdown files. This document's own S5b row is the second
user of [D10](#d10-the-implementing-job-owns-the-update)'s same-commit
exemption — the S5a row was the first, and it was written to make this one
possible.

## Correction — 2026-08-05, job #446 (S6: the ledger, and the block that could not clear)

[S6](#slices) landed as `.chug/tasks/doc-staleness.sh`. What
[D7](#d7-the-staleness-ledger) decided survives intact: the ledger is derived
from git and nothing else, it says **suspect** rather than wrong, and no author
maintains anything. Three things it decided in the abstract did not survive
contact with the measurement, and this section records all three rather than
quietly widening or narrowing the rule.

### It is a second script, not a fourth check

`check-doc-facts.sh` has one contract — a non-zero exit means a doc states
something **false**. Suspicion is not falsity, and folding a maybe into a gate
whose every other finding is a certainty devalues the certainties. So the ledger
is its own script with its own exit meaning, and the two share the one thing
that must not fork: the answer to *what paths does this doc name*.

That answer is check 1's, reached through a new `check-doc-facts.sh
--emit-paths` — the extractor with the verdict removed, printing
`<file><tab><line><tab><path>` for every claim that resolves. Same backticked
tokens, same refused false-positive classes, same `<!-- intent -->` /
`<!-- runtime -->` / `<!-- absent -->` suppression, same resolution against
`git ls-files`. A claim that does **not** resolve is omitted, so it is reported
once as check 1's error and never again as a suspicion.

### Only file claims are judged, and the numbers are why

Measured whole-tree at this commit's base, 69 tracked `*.md`, of which **61**
make at least one resolvable path claim (1,967 claim sites, 316 distinct paths):

| Path set | Suspect docs | Suspect claims |
| --- | --- | --- |
| Every resolvable claim, directories included | **35 of 61** | 177 of 1,967 |
| File claims only | **30 of 61** | 104 of 1,718 |

The first row is not a signal. A directory's "last commit" is the newest commit
under an open set, so `docs/`, `.chug/` and `web/` are newer than nearly every
doc nearly always — the top of that list read `docs/design moved`, which is true
of any commit that edits any design doc. Check 1 keeps directory claims because
*existence* is a fact about a directory; movement is not. So the ledger reads
only claims that name a tracked **file**, which is the narrowing
[what would refute this](#what-would-refute-this) called for, applied before
shipping rather than after.

**30 of 61 is a usable signal, but only because of how it is ordered.** Ranked
by the date of the newest mover, the list is worthless: this repo lands ~15 jobs
a day, so everything touched today floats to the top and the doc nobody has
opened in a fortnight sinks. Ranked by the **gap** — how long the newest mover
has sat unread — the same 30 rows separate cleanly: **7 sit a day or more
behind**, led by `.claude/skills/claim/SKILL.md` and
`.claude/skills/local-web-tweak/SKILL.md` at 12 days, and the remaining 23 are
same-day churn. Nothing is filtered by that: the gap is printed on every row and
the counts are printed in the header, so a reader draws the line rather than
inheriting a constant from the script. A threshold would have been the easy
version and the wrong one — it would hide a doc and a file that genuinely
diverged inside one day.

**The count is a flow, not a stock, and the landing commit proves it.** This
job's own commit touches `.chug/tasks/ci.sh`, `check-doc-facts.sh` and
`.githooks/pre-commit`, which between them are named by a great many docs — so
the moment it landed the ledger read **46 of 61** rather than 30, with the same
7 at a day or more. That is the correct answer and not a regression: the docs
naming those gates genuinely have not been re-read against them. A reader
comparing two runs is comparing two trees, which is why the *gap* column and the
day-or-more split matter more than the headline.

### The pre-commit block of D7 cannot clear, so it was not built

[D7](#d7-the-staleness-ledger) says *"advisory in the pre-commit hook; a
blocking finding only when the current diff touches a doc the ledger already
marks suspect."* The second half does not work at the commit, and the reason is
arithmetic rather than taste:

- At the hook, the staged doc still carries its **old** last commit. So every
  doc being edited is suspect, and **no edit made in this commit can clear it** —
  the thing that clears it is the commit the hook is refusing. The only escape
  is `git commit --no-verify`, which also disables the comment lint, the
  doc-fact check and the formatting. That is strictly worse than no ledger.

So the hook **reports and never rejects**. In CI the same rule is
non-vacuous and clearable, and it is wired: `--gate <docs the diff touches>`
exits 1 when one of them is still suspect, which requires the branch to have
edited the doc and *then* changed a file it names. Re-reading the doc and
committing it again clears the row, which is the fix anyway.

One wider rule was considered and rejected: block when the diff touches a
**file** that some doc names, making that doc newly suspect. It is
non-circular and clearable, but it fires on nearly every code change — which is
precisely the "fails for history nobody caused" failure D7 named, arriving by a
different door.

### Cost, and where it runs

0.85s whole-tree, the same order as the doc-fact gate beside it, and ~0.2s in
`--staged`. Both halves of the comparison are one `git log` invocation: the
whole history walked once with `--name-only` is 0.05s, against ~0.5s for one
`git log -1` per distinct path, so the loop the brief warned about never had to
exist. `%cs` rides along on the same pass so the report can name the date
without a second call — mawk has no `strftime`, and mawk is what the gate's
Debian container runs. That cost fits the hook's ~2s budget, so it is advisory
**there as well as in CI**, not CI-only.

### What it does not do

It does not fix anything it finds. The 30 rows are a reading list and a later
job's work — and [S12](#slices), the inbound-reference count, is still intent.

## Correction — 2026-08-05, job #448 (S7: the instructions, and the evaluator's teeth)

S7 changed **instructions and nothing else**. No gate's behaviour moved, no
script was edited, no job-type config changed shape: `.chug/tasks/docs-update.md`
was rewritten around [D1](#d1-two-kinds-of-doc-and-only-two) and
[D10](#d10-the-implementing-job-owns-the-update), and
`.chug/tasks/review-docs-updated.md` stopped being a placeholder. Both were
already wired — the slot's own comment in `.chug/jobs/code.yaml` said to give it
teeth by rewriting the prompt, no config change needed, and that is literally
all it took. (This commit replaces that comment, so the sentence it quoted is
now only here.)

### The doc table was already fixed, so one of S7's stated tasks was empty

S7's **brief** said the rewrite must fix a doc table still naming `spec.md`,
`MODULES.md` and `crates.md` — [D10](#d10-the-implementing-job-owns-the-update)
itself never mentions that table; it requires the rewrite for rule 4 alone.
**There was nothing to fix.**
Job #441 rewrote every row of that table in the same commit as
[S3](#slices) — `15dccc6` shows the nine-line hunk — and
`check-doc-facts.sh` reported both task files clean at this job's base. The
instruction to fix them was itself the stale claim, written before S3 landed and
never re-read; it is recorded here rather than quietly skipped, because "the
brief said fix it and there was nothing to fix" is exactly the kind of finding
that otherwise disappears. The table grew rows for `docs/reference/contracts.md`,
`docs/reference/runbooks/` and `web/CLAUDE.md`, which is a different change.

### The evaluator judges three classes, not D9's two

D9 names two: cross-doc state claims, and behavioural claims about symbols the
diff touched. The file as written adds a third — **D10 compliance**, a diff
implementing a slice that leaves the design doc's head alone. This is not a
widening of the gate's remit but a consequence of where
[S5a](#slices) landed: check 3 **exempts the job doing the landing**, since
`job/N` cannot exist while job N is writing the row. So D10 is a rule with no
mechanical enforcement anywhere, and the only two failures on record
(jobs #416 and #87, both on this head) are of exactly that shape. It is judged
narrowly — only when the diff plainly implements a named slice — because "is
this change that slice" is arguable, and an arguable finding on the critical
path of every `code` job is how a gate gets ignored.

The file is written to **pass by default** and says so in a rule of its own: a
finding must name the file, the line, the sentence, what is untrue and what was
read to show it, or it is not a finding. Everything mechanically decidable is
listed by owner and explicitly disclaimed — `check-doc-facts.sh`,
`check-modules.sh`, `doc-lint.sh`, the ledger's *suspicion*, and code review
itself. So is the read-only `Review` profile (`docs/spec.md` §4.3): a claim that
needs something executed to settle belongs in `.chug/tasks/ci.sh`, and the next
author is told that in the file rather than discovering it.

### Four assertions that this job's own diff had to clear, and they are class 1

Giving the evaluator teeth makes every doc that describes it as inert false —
which is the **cross-doc state claim** class the evaluator now judges, arriving
on the job that created it. Four had to be fixed in the same commit:
`CLAUDE.md`'s "(currently inert) evaluator", `docs/reference/style.md`'s "wired
but deliberately inert until the project decides how docs are managed", and the
`eval:` comments in `.chug/jobs/code.yaml` and `.chug/jobs/web.yaml`. None is
machinery; all four are prose about a gate, in four files, none of which any
mechanical check reads for meaning.

D9's own paragraph above still opens *"is currently inert **by design**"*. That
sentence is body, it was true when written, and it is corrected here rather
than edited — which is [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head)
working as specified, and the distinction the rewritten
`.chug/tasks/docs-update.md` now teaches.

## Correction — 2026-08-05, job #449 (S4: check 4, and the yield it does not have)

[S4](#slices) landed: `docs/concepts.md` is the registry, check 4 is the gate,
and it is the last slice of the design as decided.

**The yield is one, and it was known in advance.** Whole-tree, across all 71
tracked `*.md`, check 4 found exactly **one** duplicate definition: the
`**Single writer.**` in `docs/design/293-worker-capacity.md`, which the
[2026-08-05 amendment](#three-more-measurements) had already identified as its
one live target. [M5](#the-problem-measured)'s original instance was gone before
this slice started — job #416 deleted the tag that carried it. So this check
removes almost nothing. **It stops the next one**, and a gate justified by a
yield it does not have is the overstatement this whole document exists to
remove.

Re-measured on this base, which is what the seed set was chosen from:

```sh
git ls-files '*.md' | xargs grep -hE \
  '^[[:space:]]*([-*+]|[0-9]+\.)[[:space:]]+\*\*[A-Za-z][A-Za-z0-9_ /-]{0,30}\.\*\*'
```

**150 occurrences of the first shape, 146 distinct labels.** Four repeat, and
three of them (`the env`, `opt-in per node`, `disk`) are section labels rather
than concepts. The corpus does not have a duplication problem in this shape; it
has one instance, in a tree of 71 documents. That is the honest scale.

### Twelve rows, three owners, and a criterion

`job`, `task`, `job type`, `job branch`, `merge gate` and `epoch` route to
`docs/spec.md`; `work`, `evaluation`, `wrap-up`, `triage` and `evaluator` to
`docs/reference/design-lifecycle.md`; `single writer` to `docs/reference/style.md`.
Every one was checked for a second definition **before** it was registered,
because a row over two definitions fails the gate on merge and the temptation
then is to weaken the rule.

Three of the terms [D3](#d3-the-concept-registry-routes-it-does-not-hold)
proposed are **deliberately not registered**, and the reason is the same for
all three: the word names two things. A `tier` is a test tier or a rule tier; a
`claim` is a human's claim on a work attempt or a doc's claim about the tree; a
`slice` has no doc that defines it at all. One row per concept is the registry's
rule, so an ambiguous word waits for a doc to own each sense rather than being
forced into one.

`linked-origin` is a fourth, and it is a **finding rather than a scoping
decision**: it is defined twice today, in `README.md` and in
`.claude/skills/chug-install/SKILL.md`, in the same words. Registering it would
fail the gate; resolving it is a decision about how self-contained a skill must
be for an agent that has no checkout, which is not this slice's to make. It is
recorded in `docs/concepts.md` so the next author inherits the measurement
rather than re-deriving it.

### The shape D4 did not name

The corpus's commonest definitional form is neither of D4's two. It is the
em-dash label — `- **Job** — the node in the graph: the unit of delivery`
(`docs/reference/design-lifecycle.md`), `- **Work** — the fallible action` in the
same file, `**Job** = graph node` in `.claude/skills/chug/SKILL.md`. Check 4
does not see it, because D4 named two shapes and widening a gate that errors in
every job's pre-stage is not a thing to do on a hunch about the corpus.

The consequence is worth stating rather than leaving for a reader to find, and
it is narrower than the shape's frequency suggests. Most of those em-dash lines
are the owner's own: `work`, `evaluation` and `wrap-up` are registered to
`docs/reference/design-lifecycle.md`'s `## The lifecycle model`, the section
holding all three em-dash lines — a doc explaining its own term is what the row
is for. What is left is three terms: `job`, `task` and `job type` are registered
to `docs/spec.md`, and the lifecycle doc's *other* section — `## Vocabulary` —
explains all three in em-dash form. Whether that is a second definition or a
routing summary is a
judgement about those two documents, not a regex; making it a rule would mean
deciding it for every future pair at once. So it is a measurement here, next to
the `linked-origin` finding, for whoever decides it.

### The narrowing D4 did not specify, and why it is not a weakening

D4's second shape is `**Term** is|are|means|refers to`. Applied literally to the
tree it fires on two lines that are plainly mentions:

- `docs/spec.md`'s escalation table — *"…with `action: Retry` and **Work** is
  what failed"*, twice, for `Work` and for `Evaluation`;
- `docs/design/361-per-run-placement.md` — *"whose **job** is non-terminal"*.

Both would be false positives, and check 4 runs in the pre-stage of **every**
job as an error, so a false positive does not annoy anyone — it stops the fleet.
The check therefore requires the term to **open a sentence**: at the start of
the line, after a list or quote marker, or after `.`/`:`/`!`/`?`, in each case
optionally behind `a`/`an`/`the`. `A **job** is a node in a DAG` (`docs/spec.md`
§1.1, the actual definition) passes that test; `and **Work** is what failed`
does not. It is a narrowing in the direction
[M7](#the-problem-measured) argues for — refuse rather than guess — and it is
what makes the phase vocabulary registrable at all.

Inline code spans are stripped before either shape is read, which is why this
document can quote `- **Single writer.** The dispatcher is the only writer` in
three places without stating a second definition. Table cells, headings and
sentences continued onto a second line are skipped in silence.

### CLAUDE.md is held to the rule, and needed no exemption

[D5](#d5-claudemd-may-gloss-never-define) already decided this and the
implementation only had to not undo it: the file that carried
[M1](#the-problem-measured)'s normative directive to protect a deleted module is
the last file to exempt. Nothing had to change there — a gloss plus a link is a
**mention**, and mentions are free, so `CLAUDE.md` passes the gate as written.
The same reasoning covers `.chug/prompts/`. The registry's own file passes for
the same structural reason rather than by exception: it routes in table cells
and defines nothing.

### The one duplicate, fixed rather than exempted

`docs/design/293-worker-capacity.md`'s principle 5 opened
`**Single writer.** The dispatcher owns the fleet record`. It was never a
competing definition in substance — it *applies* the principle
`docs/reference/style.md` owns to a second record — but it is written in the
shape that says "here is what this term means", and a reader who lands there
learns the term from a doc that does not argue it. The label now names what it
asserts about the fleet record and links the owner; the principle it holds the
design to is unchanged, and a dated line in that doc's head says an append-only
body was edited and why. Fixing the instance rather than adding a marker is the
rule the gate is worth having: three markers exist for a claim that is
*correct* and unresolvable, and a second definition is neither.

### Why `Status:` is still IMPLEMENTED IN PART

The ticket for this slice expected the head to move to `IMPLEMENTED`, on the
reasoning that S4 was the last row. It was the last row **of the design as
decided** — S0–S8, D1–D12. It is not the last row of the table: job #435
appended [S9–S12](#slices) with the D13–D15 amendment, and its own line says
*"Nothing in that amendment is implemented"*. `docs/reference/docs.md`, <!-- intent -->
check 5, `docs/overview.md` <!-- intent --> and the inbound-reference signal do
not exist. Writing `IMPLEMENTED` over them would be a false present-tense claim
in the head of the document whose one-sentence rule is that a doc asserting
something about the tree is making a checkable claim — and check 3 would not
have caught it, because those rows carry dependency cells rather than state
words. The head says which set is complete instead.

### What it costs, and what would refute it

Whole-tree, the doc-fact gate goes from ~0.92s to ~0.98s — check 4 is ~0.06s
over 71 files, one extra `awk` pass each. `.chug/tasks/check-doc-facts.test.sh`
is at **82** cases, fourteen of them check 4's, still well inside CI's 60s
per-suite cap. Read that total off the suite's own closing line rather than
counting call sites: `check_absent` scores a case too, so a hand-count that
greps for `check ` alone comes out three low.

The refutation trigger stated [above](#what-would-refute-this) stands unchanged
and is now testable: if registered terms in definitional shape flag prose that
is not a second definition, the answer is a **smaller registered set**, not a
cleverer regex. One more, learned here: if authors start writing definitions in
a third shape to stay under the gate, the rule has taught avoidance rather than
ownership, and the registry — not the shape list — is what to shrink.

## Finding — 2026-08-05, job #450 (a case count two reviewers derived differently)

**The first real instance of the count class, and it is worse than
[check 4's dropped rule](#what-check-4-cannot-do-and-what-took-its-slot)
assumed.** The number of cases in `.chug/tasks/check-doc-facts.test.sh` — the
suite for this design's own checks — was stated in two documents, in three
successive values, none of them derived twice the same way. Job #449 spent its
**fourth and final** evaluation cycle on it, and the two reviewers who blocked on
it **did not agree with each other**:

| Cycle | Derivation shown | Total | What the suite prints |
| --- | --- | --- | --- |
| 3 | 51 static invocations + 28 from loops | **79** | no base, before or after |
| 4 | 50 + 9×2 + 10 + 4 | **82** | #449's branch, and this one |
| — | the standing value both were correcting, which cycle 4 derived as three high | **68** | the base #449 branched from |

Both reviewers showed their arithmetic. Both were reading the same file. Neither
ran it. This job ran it, at both bases, and the fourth column is the result:
`passed 68, failed 0` at `7bb86df` (the tip of `main` #449 branched from) and
`passed 82, failed 0` here, #449 having added fourteen cases in between.

So one of the two derivations was right — and **its supporting claim was not**.
The standing 68 was not three high; it was exactly what the suite printed at the
base it described, and it went wrong only because the suite grew underneath it.
A count read out of the source was used to condemn a count that a thirty-second
run would have vindicated.

### Why reading it is genuinely hard, and this is the point

The failure is not carelessness — it is that *"how many cases does this suite
have"* has **no single answer in the source**. At this base the same file
supports three defensible denominators, two of which are not what the suite
reports:

- **54** call sites of the two assertion helpers, `check` and `check_absent`
  (`grep -cE '^[[:space:]]*check(_absent)?[[:space:]]'`);
- **44** numbered `# N.` case headings, the file's own idea of a *case*, several
  of which group more than one assertion (`grep -cE '^# [0-9]+\.'`);
- **82** assertions actually executed, because some call sites sit inside `for`
  loops over shape lists — the only one of the three the suite itself reports.

A reviewer counting sites, a reviewer counting loop expansions and a reader
taking the printed total are counting three different things while all believing
they are checking the same claim. Two competent readers reaching two answers is
the expected outcome of that, not an accident — and a number three successive
authors derived three different ways is not a claim a reader can check.

### What was done, and why it is (a) rather than (b)

The present-tense counts are **deleted**, not marked:

- `docs/reference/testing.md` states the suite's cost (`0.65s`, `0.14s`) and no
  longer states its case totals; a bullet there now says the number is one
  command away and names what makes timings different — a timing is a
  measurement of a host and a date, so it is stated with both, while a total the
  runtime prints on every run is not a measurement at all.
- This document's head no longer states one either. The `S1c as landed` section
  also claimed `.chug/tasks/doc-lint.test.sh` *"now covers both at 51 cases"*,
  which had been false since [S1b](#slices) moved both checks and their cases
  into `.chug/tasks/check-doc-facts.test.sh` (job #438) — the count outlived the
  suite it counted.

(b) — marking each count with the command that derives it — was rejected on this
instance's own evidence. The marker convention assumes an author who can produce
the number; here the derivation is the hard part, and **both reviewers who
attempted it came unstuck on exactly that step**. A marker would have made two
of three numbers re-runnable, not right. The suite prints `passed N, failed N`
on every run, so the alternative to a transcribed count is not an unanswerable
question — it is one command.

**The body's counts stay, including the one immediately above.** `47 cases` in
the [#438 correction](#correction--2026-08-05-job-438-s1b-landed-and-the-eleven),
`63 cases in 0.45s` in the [#444
correction](#correction--2026-08-05-job-444-s5a-check-3-landed-and-s5-split) and
the **82** in the [#449
correction](#correction--2026-08-05-job-449-s4-check-4-and-the-yield-it-does-not-have)
are [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head) history:
each was true at its own commit and is a record of what that slice cost, not a
claim about today. #449's is verified correct here rather than edited out,
which is what an append-only body means. That is also the trap — a reader
comparing a dated body figure with a present-tense reference doc sees a
contradiction that is not one. Only the **head** and the **reference docs**
speak in the present tense, and only they are swept here.

### What this says about D6

[Check 4's substitution](#what-check-4-cannot-do-and-what-took-its-slot) argued
that a script cannot decide whether a bare number is a count of something in the
tree, so *"a count carries a marker"* stays a reviewer rule under
[D9](#d9-the-evaluator-judges-only-what-a-script-cannot). **The first half still
holds; no gate is proposed here and none should be.** What this instance refutes
is the implicit second half — that a reviewer rule is *adequate*. Four reviewers
across four cycles had this number in front of them; it survived, and the two
who finally challenged it produced conflicting replacements, one of which
condemned a figure that was true. The reviewer rule did not fail through
inattention. It failed at its ceiling.

So the rule is stated at full strength: **a count a reader cannot derive twice
the same way does not belong in prose at all** — delete it and name the command,
and reserve the marker for numbers whose derivation is a single unambiguous
command (`21` suites is `git ls-files '*.test.sh' | wc -l`; a suite's case total
is not anything you can grep). A count that is *correct* is not thereby safe:
68 was correct, and fourteen new cases falsified it without touching the
sentence that carried it. This is a strengthening of a reviewer rule, not a new
mechanism, and it is deliberately not a gate: the reasoning above about what a
script can decide has not changed.

The self-referential part is worth stating plainly, because it is the argument.
This is the document whose thesis is that prose making checkable claims about
the tree must be checked — and the one uncheckable claim class it knowingly left
to reviewers is the class that then drifted three times, inside its own
documentation, under four reviews.

## Correction — 2026-08-06, job #454 (D7: the gate that a rework could not clear)

[D7](#d7-the-staleness-ledger)'s one blocking case — `--gate` on a doc the diff
edits that is still suspect — turned out to be **unsatisfiable on a multi-commit
branch whose docs name each other**, and it escalated two jobs before anyone
read it as a defect rather than as an author's oversight. Both were cleared by
the same remedy, and it is a remedy no rework commit can reach:

- **Job #449** (S4), cycle 2: two commits, the first's docs naming files the
  second moved. The reviewer's advice was *"fold the branch's two commits into
  one — the merge is a squash anyway"*.
- **Job #453**, cycle 3: a rework that touched **one** doc,
  `docs/reference/runbooks/macos-host-supervision-proof.md`, which made it newer
  than `docs/design/440-native-worker-daemon.md`,
  `docs/reference/testing.md` and `docs/implementation-notes.md` — all three of
  which name it. Two of those were also in the diff, so `blocked = 2` and
  `.chug/tasks/ci.sh` exited 1. 440 names the runbook twice, once in a table cell
  and once in a link label; the runbook names 440. Substance passed both times.

### Why it has no fixed point, and why that is arithmetic

`--gate` blocks where `ts[path] > ts[doc]` — strictly newer, so **equal clears**.
Editing the flagged doc is therefore the remedy, and it works whenever the mover
stays put. It does not work when the mover is itself a doc the branch is also
editing: touching A to clear it against B makes A newer than B, and if B names A
the suspicion simply changes ends. Two docs that name each other admit exactly
one solution — both in the same commit — which is a squash, and a squash is not
something a rework cycle can produce. The unsatisfiability is not a bug in the
implementation of D7; it is D7 asking a question that has no answer for that
pair.

The class is precisely delimited, and that is what makes the fix small. Only a
`*.md` file makes claims, so a `*.md` file is the only thing that can appear on
**both** sides of the relation. Every cycle therefore runs doc → doc, and no
other edge can form one.

### The fix: a `*.md` mover never blocks

`.chug/tasks/doc-staleness.sh` still counts a doc → doc edge in the advisory
ledger, labelled `[cross-reference — not blocking]` in a `--gate` report. It no
longer lets one block. The blocking relation is doc → non-doc, which is acyclic
by construction, and that buys a property the gate did not have before:

> Re-touching exactly the flagged docs, in one commit, clears every blocking
> row and can create none — because the docs it touches are `*.md` and a `*.md`
> is never a blocking mover.

The gate is now satisfiable in one commit, always, with no squash and no second
rework cycle.

The second argument is independent of the cycle and reaches the same place. The
extractor emits a path record for **every** backticked token on a non-fenced
line, so a table cell and a link label put a doc in the population exactly as a
sentence about its content does — which is how job #453's two blocking rows
arose. A doc *linking* a doc is a pointer, not an assertion about what is on the
other end; #415's whole architecture is built on cross-links
([D3](#d3-the-concept-registry-routes-it-does-not-hold),
[D5](#d5-claudemd-may-gloss-never-define)), so making each link a staleness edge
makes the predicate nearly constant-true. That is the same reason directory
claims were dropped in the [S6
correction](#correction--2026-08-05-job-446-s6-the-ledger-and-the-block-that-could-not-clear):
*a predicate that is nearly constant-true is not a signal*. The difference is
that it is dropped only from the **blocking** half here — on the reading list a
constant-true row costs a line a reader skims, and the gate is already a
narrowing of the ledger rather than a copy of it.

### The three alternatives, and what each gives up

The ticket named four options. Three of them fail the acceptance criterion that
the genuine ordering must **still fire** — the branch edited a doc and *then*
changed a file it names, across separate commits — because all three erase
intra-branch ordering wholesale:

| Option | Why not |
| --- | --- |
| Compare against the **merge-base**, treating every doc the branch touches as current at tip | Intra-branch ordering stops existing, so the genuine case is exactly what it deletes. It answers the cycle by answering nothing |
| **Exclude docs the branch edits** | Same, arriving sooner: the doc in the genuine case is one the branch edited |
| Gate against a **synthetic squash** | Every path in the branch gets one timestamp, so every intra-branch comparison is equal and clears. Also mutates or shadows a worktree inside a gate, for a result identical to the first row |
| **Keep it, emit the remedy, make it self-clearing** | The remedy is emitted (it always was). Self-clearing is the thing that is *impossible* in the cycle — see the arithmetic above |

What the chosen narrowing gives up is real and worth naming: **a doc that
describes another doc's content, where that other doc changed and nobody
re-read the first, no longer blocks.** `docs/concepts.md`'s routing rows and
`docs/README.md`'s reading order are the shape of it. Two things make it
acceptable — the advisory ledger still lists it on every job, so it is
surfaced rather than lost, and
`.chug/tasks/review-docs-updated.md` judges cross-doc state claims as its
**first** class, which is the same failure reaching a reader who can tell a
pointer from a claim.

### What did not change

Checks 1–4 are untouched, the extractor is untouched — the ledger still reads
its path set through `check-doc-facts.sh --emit-paths` rather than carrying a
second answer — and `.chug/tasks/ci.sh` keeps its stage order. Narrowing the
gate on the mover side rather than in the extractor is deliberate: check 1
*should* keep verifying that a link label and a table cell resolve, and that is
the same records this reads.

## Correction — 2026-08-06, job #461 (S9 and S10: the policy doc, and the catalogue)

[S9](#slices) and [S10](#slices) landed together, as
[the pairing argument](#the-slices-and-why-they-pair) said S10 and S4 should:
check 5 is `modules_registry_compare`'s both-directions loop pointed at a third
registry, and writing it beside check 4 rather than months later is what keeps
it one instrument instead of two that drift.

What landed:

- **`docs/reference/docs.md`** — the rules in the present tense: D1's two kinds
  and their opposite update rules, D2's mutable head and append-only body, the
  absorbed header contract, D3/D4's one-definition line, D5's gloss rule,
  D15's catalogue, D10's landing-job duty, and a table of the five gates saying
  which are errors and which report. It states no rule this document does not
  already decide, and links here for every *why*.
- **`docs/README.md`'s catalogue** — one row per tracked doc under `docs/`,
  **40** of them at this commit (`git ls-files -- docs | grep -c '\.md$'`),
  each a link and a one-line summary.
- **Check 5**, in `.chug/tasks/check-doc-facts.sh` beside the other four, with
  cases in its suite for a doc with no row, a row naming no doc, both sides
  agreeing, and an unparseable row.
- **The two places an author is told about the row** —
  `.chug/prompts/work/docs.md` and `.chug/prompts/work/design.md`. A gate that
  errors in every job's pre-stage over a row nobody was told to write is the
  two-file chore [D15](#d15-structural-health-index-completeness-and-orphans)
  set out to avoid; the design prompt also read as *forbidding* the row, since
  its rule 2 says not to edit unrelated docs.

### Three shape decisions, and the reason each went the way it did

- **The population is `docs/**/*.md`, and the catalogue catalogues itself.**
  Excluding the index from its own table would have been one exception, in the
  one place a reader looks to learn the rule; carrying it costs a row. The
  alternative also puts a filename inside the gate that the gate then has to
  keep in step with the doc, which is the coupling `check-modules.sh` avoids by
  deriving both sides.
- **A row is read for the markdown link in its first cell and nothing else.**
  Check 5 errors in every job's pre-stage, so [M7](#the-problem-measured)
  applies with full force: a prose row, a heading anchor, a link out of `docs/`
  and an external URL are skipped in **silence**. Skipping is safe here in a way
  it is not in checks 1–4, because the other direction still reports the doc
  that row meant to name — a skipped row degrades to "this doc has no row",
  which is a finding an author can act on, not a hole.
- **A tracked `docs/README.md` with no parsable `## The catalogue` table is a
  LINTER ERROR**, exactly as the concept registry is (job #449). Renaming that
  heading would otherwise stand the check down over the whole corpus in silence,
  and a doc-fact check that cannot run must not read as clean.

### What deviates from D14, and why

[D14](#d14-the-policy-is-reference-415-is-the-argument) says the policy doc
absorbs `docs/design-docs.md` "as a section rather than deleting it". The
content is absorbed; **the file stays, reduced to a pointer.** Deleting it would
have meant rewriting its inbound references, and at this commit four of the
eight sit where a `docs` job should not reach: three are citations inside
**append-only design bodies** (#313, #355, #361), where a citation records what
was read at the time, and one is a comment in `web/src/pages/Designs.tsx` —
source, which this job type does not edit.

The four references that asserted the page *states* or *defines* the contract
were repointed, because after the absorption that claim is false: the design
work prompt, `.chug/tasks/review-design.md`, one line of
`.chug/tasks/docs-update.md`, and one note in `docs/implementation-notes.md`.

That leaves a pointer page in the catalogue, which is the honest state and says
so in its own first line. It is also the smallest instance of what
[S12](#slices) will measure: a doc whose only remaining value is that other
documents name it.

### And what deviates from the brief, and why

The brief for this job said not to change `.chug/tasks/docs-update.md`'s rules —
S9 *describes* them and must not restate them as a second definition. **Rule 4
changed anyway.** It said a new page is wired in by linking it "from
`docs/README.md` **or** from the page that should reach it"; check 5 makes the
row mandatory, so as written it told an author to do a thing the same commit's
gate would then reject in the pre-stage of every job. A rule that contradicts a
gate is worse than a rule that duplicates one. It now says both — give it a row,
*and* link it from the page that should reach it. The gate table's
`check-doc-facts.sh` row also gained checks 4 and 5, which it predated.

Neither edit restates a definition, so [D4](#d4-ban-duplicate-definitions-allow-duplicate-mentions)
holds and check 4 is unaffected: rule 4 *mentions* the catalogue and routes to
`docs/reference/docs.md` for its shape, which is where S9 put it. The same
reasoning covers the two work prompts above — they name the requirement and
link; they do not define it.

### Two things measured while landing this

- **This document's own `Status:` line was over the bound the header contract it
  was absorbing states.** It ran to **161** characters against
  `DOC_STATUS_LEN_MAX`, so the Designs view had been serving it cut
  mid-sentence; the rewritten line is 83. The head that
  [D2](#d2-every-design-doc-opens-with-a-mutable-current-state-head) built to
  bound reading cost was failing the one bound the platform actually enforces —
  which is [D14](#d14-the-policy-is-reference-415-is-the-argument)'s argument
  arriving as evidence rather than as prose.
- **Check 5 costs about 0.02s**: three whole-tree runs took 3.05s before and
  3.10s after, on 72 tracked `*.md`. It is one `awk` over one file and two
  set comparisons, which is why it can afford to be unconditional.

`Status:` stays `IMPLEMENTED IN PART`. [S11](#slices) and [S12](#slices) are not
built, and job #449 argued that case correctly — check 3 now verifies it, so a
head claiming otherwise over an unlanded row fails its own gate.

## Correction — 2026-08-06, job #467 (S11: the synthesis page, and the wiki clause that could not run)

[S11](#slices) landed as `docs/overview.md`, and half of its row did not land at
all. Both halves are recorded here because the row above is now marked
`**Landed**`, and a landed row that quietly drops a clause is the drift
[check 3](#d6-four-mechanical-checks) exists to make visible.

### What the page is

Eight sections, seven of them a two-column table: the left cell is a question a
reader arrives with, the right cell is the document that answers it. That shape
was chosen for one property — **a row cannot go stale unless the doc it links
stops answering its question**, which is a far rarer event than the thing the
doc describes changing. It links 23 distinct destinations across 42 links, and
it names no symbol and no constant: the only tree paths on the page are the
documents it routes to, four config directories under `.chug/`, and
`.chug/tasks/ci.sh`, which is itself a destination rather than a claim.

[D13](#d13-the-synthesis-page-is-a-reference-doc) predicted the failure mode
exactly — *"the doc whose whole job is restatement is the last one that should be
trusted to restate freely"* — and the discipline turned out to be a writing
constraint rather than an editing one. Two sentences were written and then
withdrawn for stating something checkable rather than routing to it, and the
test that caught both was the one S11's brief supplied: *would this need editing
if the thing it describes changed?* One asserted that the repo carries no
`.github/` workflow <!-- absent --> — true, load-bearing, and owned by
`CLAUDE.md`, which is where the surviving sentence sends the reader instead. The
other counted the rows of the table beneath it, which is
[the class job #450 recorded](#finding--2026-08-05-job-450-a-case-count-two-reviewers-derived-differently)
and which no gate here catches.

**Check 4 never fired**, on any draft. That is worth stating rather than
celebrating: the page mentions six of the twelve registered concepts — `job`,
`job type`, `task`, `work`, `evaluation` and `merge gate` — and defines none,
but it also avoids the two gated shapes by construction, since a table cell is
skipped in silence
([`docs/concepts.md`](../concepts.md#what-the-gate-reads-here)). The gate's
silence is therefore consistent with the page being right and would also be
consistent with it being wrong in a shape D4 does not name — which is
[the shape D4 did not name](#the-shape-d4-did-not-name), arriving in the one doc
most exposed to it. The check that actually bites here is a reader's.

### The wiki clause could not be done, and is deferred rather than dropped

S11's row reads *"any `wiki/` prose note resolved into it and reduced to a
link"*. Measured on this branch's base:

```sh
git ls-files wiki/ | wc -l   # 8 — .gitignore, 4 .obsidian/ config files,
                             #     Chuggernaut Diagram.canvas, 2 .base stubs
ls wiki/                     # the same set, in a job checkout
```

Not one of them is prose. The note the clause is aimed at — the one
[Correction 3](#three-more-measurements) reports and that
[D13](#d13-the-synthesis-page-is-a-reference-doc) treats as the evidence its
demand is real — is **untracked**, and a job cannot act on it for two
independent reasons:

1. **It is not in the checkout.** A work container clones the platform's bare
   repo, which holds tracked objects only, so the file is not merely
   unauthorised to touch — it is absent. `ls wiki/` above is run in that
   checkout and lists the same eight paths git does.
2. **It is the operator's.** Committing someone's untracked working file is a
   decision about their material, not a doc chore, and this repo already has the
   precedent: `security-assessment.md` sits in the slice table as
   **out of scope, its own job** for the same reason.

So the clause is **deferred with cause**, not satisfied and not silently
dropped. If the operator tracks the note, resolving it into `docs/overview.md`
and reducing it to a link is a follow-up job — and a small one, because the
destination now exists, which was D13's entire point.

This is also the second time this document has met the same wall from a
different side.
[D13](#d13-the-synthesis-page-is-a-reference-doc) already argues that the demand
for a synthesis page *"cannot be met by extending a check's reach, because the
writing that evidences the demand is out of reach by construction."* Neither can
the clause that would consume it. Everything a job here can do about an untracked
file is give its content somewhere to go; that is done.

### What else changed, and what deliberately did not

- `docs/README.md` gained the catalogue row check 5 requires, and its opening
  paragraph now sends a cold reader to `docs/overview.md` first. A row satisfies
  the gate; the pointer is what makes the page reachable, and
  [D15](#d15-structural-health-index-completeness-and-orphans) is explicit that
  those are different properties.
- **No concept was registered.** The page wanted no term that
  [`docs/concepts.md`](../concepts.md) lacks, and a synthesis page is the worst
  possible place to discover one: a row there constrains every other doc, so it
  should be argued from the doc that would own the definition, not from the doc
  that merely links it.
- **No existing doc was moved, absorbed or rewritten.** Overview links; it does
  not consume. The one edit outside it and its catalogue row is this head.
- `Status:` stays `IMPLEMENTED IN PART`. [S12](#slices) is unbuilt, and job #449
  argued that case before check 3 could verify it.

## Correction — 2026-08-06 (job #468): S12, the orphan half and the two routes

[S12](#slices) landed. `.chug/tasks/doc-staleness.sh` now reports, ahead of the
staleness half, every tracked `docs/**/*.md` that **no other tracked `*.md`
names**. Advisory, exactly as [D15](#d15-structural-health-index-completeness-and-orphans)
specifies, and the ledger's exit code is untouched by it. Four things were
decided by measurement rather than by the brief, and each is a narrowing.

**The catalogue does not count, and that is the whole check.** Since job #461
every tracked doc under `docs/` has a `docs/README.md` row, and check 5 fails
both ways to keep it so. A row is therefore evidence of nothing: if one counted
as an inbound reference then no doc could ever be an orphan and this half would
report a constant — the same defect
[M7](#the-problem-measured) records from the other end, a predicate whose value
is fixed. So `docs/README.md` is excluded as a **referrer**, the whole file and
not merely the table, while staying a doc that is itself judged.
[D15](#d15-structural-health-index-completeness-and-orphans) already draws the
line this implements: check 5 answers *is it catalogued* and the ledger answers
*is it read*, and the second question is only worth asking if the first cannot
supply its answer.

**Counting only path claims is wrong, and the measurement is what says so.** The
brief asked for check 1's extractor, and check 1 is a *path-claim* extractor: a
backticked token that resolves against `git ls-files`. Run alone over the tree it
reported **7 of the 41** docs orphaned —
[#308](308-gha-port.md), [#310](310-scheduled-jobs.md),
[#313](313-workload-identity-image-builds.md),
[#322](322-macos-native-runtime.md), [#355](355-project-task-images.md),
[#372](372-chug-node-modules.md) and [#373](373-project-toolchains.md) — and
every one is false. #313 alone is named by eight files. The reason is one line of
corpus convention: `docs/design/` cites its siblings as
`[#313](./313-workload-identity-image-builds.md)`, and a link target is not
backticked. A rule that reports seven false positives and no true one is
[job #446](#correction--2026-08-05-job-446-s6-the-ledger-and-the-block-that-could-not-clear)'s
directory-claim finding again, so it was narrowed the same way — by adding the
route the corpus actually uses, not by shipping the count.

**So there are two routes, and neither is written here.** A backticked path claim
comes from `check-doc-facts.sh --emit-paths`, which the staleness half already
reads. A relative markdown link comes from `doc-lint.sh --emit-links`, added by
this slice: rule 2's extractor with the verdict removed, printing
`<file><tab><line><tab><target>` with the target joined onto the linking file's
directory and its `.`/`..` segments collapsed. It resolves nothing — an off-tree
target simply falls out of the join, and a dangling link is already rule 2's
finding. That is deliberately the same arrangement `--emit-paths` has: the
alternative was a second link-target scanner in the ledger, and two
implementations of one rule drift. Both routes together: **0 of 41**.

**Only `docs/` is judged.** Over all 73 tracked `*.md` the count was **11** when
this paragraph was first written, and none of the eleven was a finding:
`.chug/prompts/review/code.md`,
`.chug/prompts/work/manual.md` and `.chug/tasks/review-docs.md` are named by
path from `.chug/jobs/*.yaml`; the `crates/platform-ops/templates/` and
`deploy/dev/templates/` markdown is copied wholesale into a new project; and
`fixtures/mobile/ios/Runner/Assets.xcassets/LaunchImage.imageset/README.md` is
Xcode boilerplate. All of them are reached by machinery rather than by citation,
so "nothing names it" is not a statement about any of them. `docs/` is also
exactly check 5's population, which is what makes the pair a pair. Referrers
stay every tracked `*.md`, prompts included — a doc read on every design job
because `.chug/prompts/work/design.md` names it is no orphan.

**The same measurement now returns 7, and this paragraph is why.** Naming four
of the eleven by path — the three prompts and the Xcode README above — made them
cited, so re-running it at this base finds the other seven and not them. Over
`docs/` the count is unmoved, because a doc that had to be *written about* to
acquire its only reference is exactly the finding; over the machinery-reached
population it means the number measures whether someone has recently discussed a
file, which is not reachability and is why that population is out. The
observer effect is a property of any citation count and the reason to keep the
population small enough that every member is expected to be cited by prose.

**Is zero a usable signal?** It is the honest reading, and it is not a check that
cannot fire. The corpus has produced exactly two orphans on record —
[M4](#the-problem-measured)'s `spec_original.md`, deleted by [S2](#slices), and
[M8](#three-more-measurements)'s #323, which now has one inbound reference and
is at the bottom of the distribution with [#169](169-handoff-continuity.md).
Both were found by a hand-written query, four days apart, and nothing in between
noticed. The value here is the ratchet: the next one is reported on the job that
creates it. The distribution is the thing to watch rather than the count — if
the floor drifts toward zero, the refutation trigger
[D15 already names](#what-would-refute-this-added) applies and the signal should
be scoped to old *and* uncited rather than to either half.

The orphan half runs in the two **whole-tree** modes only. Reach is a property of
the whole tree, so a scoped run cannot answer it, and `--staged` is the
pre-commit hook's ~2s budget. It costs **0.09s** — one extra `doc-lint.sh
--emit-links` pass, the path set being already in hand.

### `Status:` moves, and this is the job that gets to move it

This slice was written expecting to leave the head at `IMPLEMENTED IN PART`,
because [S11](#slices) was open when it started. S11 merged first (job #467)
while it was in flight, so the rebase that brought this correction onto the
current base is also what made every row in the table `**Landed**`. Job #449's
argument, which check 3 now enforces, says a head may say `IMPLEMENTED` exactly
when no row contradicts it. None does. `Status:` moves for the first
time since this document was written, on the last of its sixteen slices.

It is worth being exact about what that word now claims, because the programme
finished with two things not done:

- **S11's `wiki/` clause is deferred with cause**, as its own correction
  records — the prose note is untracked, so no job can reach it. The head says
  so rather than letting `IMPLEMENTED` absorb it.
- **`security-assessment.md` was never in scope**; its row has said so from the
  first draft and still does.

Neither is an unlanded slice, which is the only thing check 3 reads, and neither
is hidden — a head that says `IMPLEMENTED` over a deferred clause it does not
mention is the drift [D10](#d10-the-implementing-job-owns-the-update) exists to
prevent, so the clause travels in the head with the word.

One thing the rebase changed underneath this correction, recorded because it is
the check running on its own author: `docs/overview.md` is S11's page and the
newest doc in the tree, so it joined this half's population between the
measurement and the merge. It is **not** an orphan — but the only thing naming
it, once `docs/README.md` is excluded as a referrer, is
[the S11 correction](#correction--2026-08-06-job-467-s11-the-synthesis-page-and-the-wiki-clause-that-could-not-run)
above. Had job #467 landed the page and its catalogue row alone, the tree's
newest and most central doc would have been this half's first true positive on
the day it was written. That is the exclusion earning its keep.
