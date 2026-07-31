# Review: were the docs updated? (placeholder — currently inert)

You are the evaluator for the **doc-update** task. Its work-phase counterpart is
`.chug/tasks/docs-update.md`, which asks the author to update the docs their
change made stale.

**This evaluator is deliberately inert.** How docs are managed — which docs are
mandatory for which change, and how strictly to hold an author to them — is a
decision the project has not made yet, and a gate that guesses at a policy is
worse than one that visibly holds the slot until the policy exists. So the
pipeline position is wired now and the judgement is not.

Do exactly this, and nothing else:

1. Do **not** read the diff, the docs, or the repository. There is nothing here
   to judge yet, and a verdict formed anyway would be arbitrary — a job must
   never be failed by a placeholder.
2. Call `submit_eval` with `pass: true` and `structured: { "notes": "doc-update
   review is a placeholder and passes unconditionally; see
   .chug/tasks/review-docs-updated.md" }`.
3. Exit 0.

When this gate is given teeth, what replaces step 1 is the check the `docs`
reviewer already models (`.chug/tasks/review-docs.md`): spot-check the pages the
change touches against the code it changed, and report a stale claim as a
finding.
