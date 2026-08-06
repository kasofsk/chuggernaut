# Task: update the docs your change makes stale

A **work-phase task**, run by the author as part of the same change (it is
referenced from `.chug/prompts/work/code.md` and `.chug/prompts/work/web.md`,
never launched as its own container). Its counterpart in the Evaluation phase
is `.chug/tasks/review-docs-updated.md`, which **can fail your job** — read
what it judges before you decide you are done.

Knowledge in this repo lives in **docs, not comments**. `.chug/tasks/check-comments.sh`
rejects every non-doc comment in the tree and caps doc comments at two
sentences, so the explanation a comment used to carry has exactly one place to
go — a doc. Docs are intentional and organized: one place to look, one place to
update, one job type (`docs`) that maintains them.

## Two kinds of doc, and they have opposite update rules

Design [#415](../../docs/design/415-knowledge-architecture.md) D1. Applying one
kind's rule to the other is the commonest mistake here, and it is why this file
was rewritten.

| | **Reference** | **Design** |
| --- | --- | --- |
| Where | `docs/spec.md`, `docs/reference/**`, `docs/README.md`, `docs/implementation-notes.md`, `docs/design-docs.md`, `CLAUDE.md`, `web/CLAUDE.md`, the prose under `.chug/` | `docs/design/*.md` |
| What it is | the system as it is now | the record of a decision and why it was taken |
| Tense | present, always | present in the head; dated past in the body |
| History | none — no `Status:`, no changelog, no "we decided" | the whole point |
| How you edit it | rewrite the sentence that is now wrong, in place | rewrite the **head** freely; extend the **body** only by appending |

There is no third kind. A plan is a design doc with a slice table
(`docs/design/210-ts-rewrite-plan.md` and `docs/design/215-refactor-plan.md` are
the two that were demoted).

### Reference docs: rewrite to current truth, and do not narrate

Present tense, describing the tree as it is after your change. **Do not write
what changed.** Why the change was made belongs in the commit message
(`docs/reference/style.md` Tier 2 rule 5); what it did belongs in your work
summary. A reference doc that accumulates "as of job #N…" sentences is one
nobody can read to find out what is true.

### Design docs: the head is mutable, the body is append-only

Design #415 D2. The **head** is the title, the `Status:` line, the decision
table, the slice table and any current-state section — everything before the
document's argument begins — and it is rewritten to current truth whenever
anything below it changes. Everything after it is the **record**.

Read for where the argument starts, not for a horizontal rule.
`docs/design/415-knowledge-architecture.md` marks the boundary explicitly, with
a rule and a `## The record` heading; most design docs do not, and in the ones
that carry a `---` at all it is usually just a section separator
(`docs/design/000-rationale.md`'s first one falls mid-introduction, well below
the head; `docs/design/309-host-native-execution.md` has a dozen). Then:

- **Never edit the body in place.** A dated statement in the body was true when
  it was written; rewriting it to match today's tree destroys the record and
  gains nothing the head does not already give a reader.
- **If the body is now wrong, append a dated correction** — the heading shape
  the tree already uses is `## Correction — YYYY-MM-DD, job #N (what it
  corrects)` — and link it from the head so the head stays the one thing a
  reader has to read.
- The title line and the `Status:` line have a parsed contract (they are what
  the operator UI's Designs view shows): `docs/reference/docs.md` is the page
  that states it, including the length bound.

### If you implemented a design slice, you update that design doc — in this commit

Design #415 D10, and it is an acceptance criterion, not a courtesy. You are the
only party who knows what *actually* landed versus what was designed; a
follow-up `docs` job would be re-deriving that from your diff, and a queue of
pending doc jobs is how the drift got this bad.

So, in the same commit as the implementation:

1. **Flip the slice row** to `**Landed** (job #N)` — `N` is *your* job — and say
   in the same cell what actually landed, not what the row proposed.
2. **Adjust the `Status:` line** and any count or "what is landed" sentence in
   the head. `Status: IMPLEMENTED` is a claim that *every* slice landed.
3. **If what you built differs from what the body argues**, append a dated
   correction saying so, and point the row at it. A slice that landed reversed
   or split is normal and is recorded, not argued away.

The two failures this rule exists to prevent are both on
`docs/design/415-knowledge-architecture.md`'s own head: job #416 landed a slice
and left the head saying nothing was implemented, and the same table named
job #87 as live work after #87 was revoked. Check 3 of
`.chug/tasks/check-doc-facts.sh` catches **neither**, and it is worth knowing
why: it resolves *over*-claims only — a `**Landed** (job #N)` row whose `job/N`
commit does not exist, and a head saying `Status: IMPLEMENTED` over a row still
unlanded. Both failures above were *under*-claims, a head owning less than the
tree had. Check 3 also **exempts your own job**, because your `job/N` commit
cannot exist while you are writing the row. Nothing mechanical will catch you
here — which is why `.chug/tasks/review-docs-updated.md` judges it.

## Do this before you submit

1. **Ask what your change made untrue.** Not "should I write a doc" — *which
   existing sentence is now wrong*. Grep the docs for the symbol, path, flag,
   job type or endpoint you touched:

   ```sh
   git grep -n '<the thing you changed>' -- '*.md'
   ```

2. **Update every hit that is now stale**, in this same commit. The docs most
   likely to go stale, and who owns what:

   | Doc | Update it when your change… |
   | --- | --- |
   | `docs/spec.md` | alters normative behavior — the data model, the state machine, a prompt contract |
   | `docs/reference/modules.md` | adds, removes, renames or re-scopes a dispatcher/domain module (CI enforces this one) |
   | `docs/reference/crates.md` | moves responsibility between crates, or adds a crate |
   | `docs/reference/testing.md` | changes what a tier covers or how to run it |
   | `docs/reference/style.md` | changes a rule or the machinery enforcing one |
   | `docs/reference/contracts.md`, `docs/reference/structure-assessment.md` | changes a dispatcher interface or the factoring those two describe |
   | `docs/reference/runbooks/` | changes an operator procedure |
   | `docs/reference/design-lifecycle.md` | changes the job or release lifecycle |
   | `CLAUDE.md`, `web/CLAUDE.md` | changes a convention that bites someone who misses it |
   | `docs/README.md` | adds a doc, or changes where a reader should be routed |
   | `docs/implementation-notes.md` | invalidates a note there, or gives one a better home — fold it into the real doc and delete the entry |
   | `docs/design/*.md` | implements one of its slices (above), or supersedes a decision it argues |
   | `docs/design/000-rationale.md` | supersedes the rationale it argues |

3. **A path you name in a doc is a claim it exists.**
   `.chug/tasks/check-doc-facts.sh` resolves every backticked path against
   `git ls-files` — every tracked `*.md`, on **every** job, and a **failure**,
   so a path that moved fails the job that moved it wherever it is cited. If the
   path is *correctly* unresolvable, mark the line rather than deleting the
   claim: `<!-- intent -->` for something designed but not built,
   `<!-- runtime -->` for build output or an operator-owned file that git will
   never hold, `<!-- absent -->` for a line whose point *is* that the path is
   gone. All three are defined in `docs/reference/style.md`'s doc-claim rule;
   none excuses a stale path.

   **A constant's value you restate is a claim too.** The same script reads a
   backticked `SCREAMING_SNAKE_CASE` name asserted with a value on the same line
   (`` `CONFIG_SCHEMA_EPOCH` is 5 ``, `` `NAME = 5` ``, a `| 5 |` table cell)
   against the `pub const` in the tree, so a restated epoch is checked wherever
   it is cited. Link to the constant instead of copying it where you can; where
   you must state it, `<!-- intent -->` is for the value a slice will bump it
   to, never for one that has simply gone stale.

4. **Prefer editing a doc to adding one.** A new page that duplicates an
   existing one is the doc-shaped version of a copy-paste clone. Add a page only
   when the subject has no home; then give it a row in `docs/README.md`'s
   catalogue — check 5 below requires one for every tracked doc under `docs/`,
   in both directions — and link it from the page that should reach it, or it is
   an orphan.

5. **Gloss and link; never define twice.** #415 D4/D5: any doc may *mention* a
   concept as often as its argument needs, but what a term *means* is written
   once, in the doc that owns it, and everywhere else is one line of gloss plus
   a link. `CLAUDE.md` and the `.chug/prompts/` are bound by this most tightly,
   because they are injected into contexts that hold nothing else to check
   against.

6. **Say what you did in your `submit_result` summary** — which docs you
   updated, or that you checked and none were stale. "No docs needed" is a fine
   answer when it is a considered one; it is not a default.

## What will fail you, and what will only warn

| Gate | Verdict | What it decides |
| --- | --- | --- |
| `.chug/tasks/check-doc-facts.sh` | **error**, every job, whole tree | backticked paths resolve (check 1); restated constants match the tree (check 2); a `**Landed** (job #N)` slice row matches a merged `job/N` commit, and `Status: IMPLEMENTED` has no unlanded row (check 3); a term `docs/concepts.md` registers is written in definitional shape only in the doc that registry names as its owner (check 4); every tracked doc under `docs/` has a `docs/README.md` catalogue row and every row names a tracked doc (check 5) |
| `.chug/tasks/check-modules.sh` | **error**, every job | `docs/reference/modules.md` lists every dispatcher/domain module and nothing else |
| `.chug/tasks/doc-lint.sh` | **error**, `docs` and `design` jobs at stage 1 | markdown well-formedness, relative links resolve, `docs/design/` filename shape |
| `.chug/tasks/review-docs-updated.md` | **error**, `code` and `web` jobs at stage 0 | the three judgement classes a script cannot reach — cross-doc state claims, behavioural claims about symbols you touched, and D10 above |
| `.chug/tasks/doc-staleness.sh` | **advisory** | a doc is *suspect* when a file it names has a newer commit than the doc. Suspect is not wrong. It blocks in exactly one case: a doc **this diff edits** that is still suspect through a **non-doc** file after your edit. Clear it by re-reading the doc and *saying so* — a `Doc-reread: <path>` trailer in a commit message on this branch, one line per doc; the gate reads the trailers your branch added since its base (job #471). Re-touching the doc still satisfies the timestamp, but the trailer is what asserts you looked. A doc your branch merely links is never the blocking mover, so reworking one doc among several that cross-reference never needs a squash (job #454) |

`.githooks/pre-commit` runs the fast half of that list over your staged files in
~2s, so a stale path or a stray comment surfaces at the commit rather than a
rework cycle later. It reports the advisory ones without blocking. A local
checkout needs `git config core.hooksPath .githooks` once.

## What is genuinely out of scope

A doc you cannot make true without guessing. If the change leaves a doc stale in
a way that needs a decision rather than an edit, say so in the summary and let a
follow-up `docs` or `design` job own it — do not invent the answer.
