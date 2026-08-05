# Task: update the docs your change makes stale

A **work-phase task**, run by the author as part of the same change (it is
referenced from `.chug/prompts/work/code.md` and `.chug/prompts/work/web.md`,
never launched as its own container). Its counterpart in the Evaluation phase
is `.chug/tasks/review-docs-updated.md`.

Knowledge in this repo lives in **docs, not comments**. `.chug/tasks/check-comments.sh`
rejects every non-doc comment in the tree and caps doc comments at two
sentences, so the explanation a comment used to carry has exactly one place to
go — a doc. Docs are intentional and organized: one place to look, one place to
update, one job type (`docs`) that maintains them.

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
   | `CLAUDE.md` | changes a convention that bites someone who misses it |
   | `docs/` (the wiki) | changes something a reader is being taught — the operator-facing behavior of a feature |
   | `docs/implementation-notes.md` | invalidates a note there, or gives one a better home — fold it into the real doc and delete the entry |
   | `docs/design/000-rationale.md`, `docs/design/*.md` | supersedes a decision one of them argues |

3. **A path you name in a doc is a claim it exists.**
   `.chug/tasks/check-doc-facts.sh` resolves every backticked path against
   `git ls-files` — every tracked `*.md`, on **every** job, and a **failure**,
   so a path that moved fails the job that moved it wherever it is cited. If the
   path is *correctly* unresolvable, mark the line rather than deleting the
   claim: `<!-- intent -->` for something designed but not built,
   `<!-- runtime -->` for build output or an operator-owned file that git will
   never hold, `<!-- absent -->` for a line whose point *is* that the path is
   gone. All three are defined in docs/reference/style.md's doc-claim rule; none excuses a
   stale path.

   **A constant's value you restate is a claim too.** The same script reads a
   backticked `SCREAMING_SNAKE_CASE` name asserted with a value on the same line
   (`` `CONFIG_SCHEMA_EPOCH` is 5 ``, `` `NAME = 5` ``, a `| 5 |` table cell)
   against the `pub const` in the tree, so a restated epoch is checked wherever
   it is cited. Link to the constant instead of copying it where you can; where
   you must state it, `<!-- intent -->` is for the value a slice will bump it
   to, never for one that has simply gone stale.

4. **Prefer editing a doc to adding one.** A new page that duplicates an
   existing one is the doc-shaped version of a copy-paste clone. Add a page only
   when the subject has no home; then link it from the index that should reach
   it, or it is an orphan.

5. **Do not narrate the change in a doc.** Docs describe the system as it is
   now, in the present tense. Why the change was made belongs in the commit
   message (docs/reference/style.md Tier 2 rule 5); what it did belongs in the work summary.

6. **Say what you did in your `submit_result` summary** — which docs you
   updated, or that you checked and none were stale. "No docs needed" is a fine
   answer when it is a considered one; it is not a default.

## What is genuinely out of scope

A doc you cannot make true without guessing. If the change leaves a doc stale in
a way that needs a decision rather than an edit, say so in the summary and let a
follow-up `docs` or `design` job own it — do not invent the answer.
