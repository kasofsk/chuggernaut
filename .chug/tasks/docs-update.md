# Task: update the docs your change makes stale

A **work-phase task**, run by the author as part of the same change (it is
referenced from `.chug/prompts/work/code.md` and `.chug/prompts/work/web.md`,
never launched as its own container). Its counterpart in the Evaluation phase
is `.chug/tasks/review-docs-updated.md`.

Knowledge in this repo lives in **docs, not comments**. `.chug/tasks/check-comments.sh`
rejects every non-doc comment a diff adds and caps doc comments at two
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
   | `spec.md` | alters normative behavior — the data model, the state machine, a prompt contract |
   | `MODULES.md` | adds, removes, renames or re-scopes a dispatcher/domain module (CI enforces this one) |
   | `crates.md` | moves responsibility between crates, or adds a crate |
   | `testing.md` | changes what a tier covers or how to run it |
   | `STYLE.md` | changes a rule or the machinery enforcing one |
   | `CLAUDE.md` | changes a convention that bites someone who misses it |
   | `docs/` (the wiki) | changes something a reader is being taught — the operator-facing behavior of a feature |
   | `design.md`, `docs/design/*.md` | supersedes a decision one of them argues |

3. **Prefer editing a doc to adding one.** A new page that duplicates an
   existing one is the doc-shaped version of a copy-paste clone. Add a page only
   when the subject has no home; then link it from the index that should reach
   it, or it is an orphan.

4. **Do not narrate the change in a doc.** Docs describe the system as it is
   now, in the present tense. Why the change was made belongs in the commit
   message (STYLE.md Tier 2 rule 5); what it did belongs in the work summary.

5. **Say what you did in your `submit_result` summary** — which docs you
   updated, or that you checked and none were stale. "No docs needed" is a fine
   answer when it is a considered one; it is not a default.

## What is genuinely out of scope

A doc you cannot make true without guessing. If the change leaves a doc stale in
a way that needs a decision rather than an edit, say so in the summary and let a
follow-up `docs` or `design` job own it — do not invent the answer.
