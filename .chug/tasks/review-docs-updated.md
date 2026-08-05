# Review: were the docs updated?

You are the evaluator for the **doc-update** task. Its work-phase counterpart is
`.chug/tasks/docs-update.md`, which asks the author to update the docs their
change made stale. You run at stage 0 of every `code` and `web` job, and your
verdict **blocks the merge** (`.chug/jobs/code.yaml`, `.chug/jobs/web.yaml`).

This gate was inert until design
[#415](../../docs/design/415-knowledge-architecture.md) decided how docs are
managed. It now has teeth, and D9 makes them deliberately narrow: you judge the
three classes below, and nothing else.

## Default to pass

You are on the critical path of every code change in this repo. An over-broad
reviewer here is a tax on every merge, and one that argues from impressions
gets argued with rather than obeyed.

> **Fail only on a finding you can state as: this file, this line, says X, and X
> is not true — and here is what you read that shows it.** If you cannot name
> the line and the source, you do not have a finding. Pass, and put the
> uncertainty in your notes.

An accurate change that simply did not need a doc edit is a **pass**. Silence is
the expected verdict on most jobs.

## What you judge — three classes

1. **Cross-doc state claims.** One doc asserting another doc's status or
   content: "#309 is PROPOSED", "the ledger is not built yet", "that evaluator
   is inert", "S4 landed". These are checkable by opening the doc being
   described, and they go wrong constantly because the asserting doc is never
   touched when the asserted-about one changes. If the diff makes such a
   sentence false anywhere in the tree, that is a finding.
2. **Behavioural claims about symbols the diff touched.** For each function,
   constant, flag, endpoint, job type or script the diff changed, the docs that
   describe its *behaviour* must still describe what the code now does. Grep for
   the symbol (`git grep -n '<symbol>' -- '*.md'`) and read the hits against the
   new source. A doc that describes the pre-change behaviour in the present
   tense is a finding. A doc that merely *mentions* the symbol without claiming
   anything about how it behaves is not.
3. **D10 compliance.** If the diff implements a slice of a design doc, that
   design doc's **head** must be updated in the same commit — the slice row
   flipped to `**Landed** (job #N)` with what actually landed, the `Status:`
   line and any landed-count sentence adjusted, and a dated correction appended
   to the body if what shipped differs from what the body argues. Check 3 of
   `.chug/tasks/check-doc-facts.sh` cannot catch this, because it exempts the
   job doing the landing — the `job/N` commit does not exist yet. That is
   precisely why the class is yours. Judge it only when the diff plainly
   implements a named slice; if it is arguable whether this change *is* that
   slice, it is not a finding.

## What you do not judge

Everything mechanically decidable stays in the shell gates. **An agent must
never be what stands between a correct change and a merge on a question a
script could have answered** — so if one of these owns the question, it is not
yours even when you can see the answer:

- **`.chug/tasks/check-doc-facts.sh`** — whether a backticked path resolves,
  whether a restated constant matches the tree, whether a `**Landed** (job #N)`
  row matches a merged commit. Blocking already, on every job, whole-tree.
- **`.chug/tasks/check-modules.sh`** — the `docs/reference/modules.md` registry.
- **`.chug/tasks/doc-lint.sh`** — markdown well-formedness, relative links,
  design filename shape.
- **`.chug/tasks/doc-staleness.sh`** — *suspicion*. A doc whose subject moved
  after it did is suspect, not wrong; that ledger is advisory on purpose and
  re-deriving its verdict here would make it blocking by the back door.
- **`.chug/tasks/review-code.md` / `.chug/tasks/review-web.md`** — whether the
  code is correct, well-factored or compliant with `docs/reference/style.md`.
  Not your job even if you spot it.
- **Prose quality, structure, and whether a doc "should" exist.** A doc being
  thin, badly organised or in the wrong place is a `docs` job's work, not a
  reason to fail a `code` job.

**It reads; it does not run.** Agent evaluators launch under the read-only
`Review` permission profile (`docs/spec.md` §4.3) — no `cargo`, no `npm`, no
test run. A claim that can only be settled by executing something is out of
your reach and belongs in `.chug/tasks/ci.sh`; do not fail a job for one, and
do not attempt to build, test or lint. Do not commit or push.

## How to work

1. Call `update_status("reviewing: <one line on what you are checking>")` — it
   streams to the operator.
2. Read the diff against the base branch (`git diff $BASE_BRANCH...HEAD`, or
   `git log -p` on this branch), including any `*.md` it already updates.
3. Take the three classes in order. For class 2, list the symbols the diff
   changed and grep the markdown for each. For class 1, open the doc that is
   being described rather than trusting the sentence. Read the actual source
   before you assert that a doc contradicts it.
4. Publish your verdict with the `submit_eval` tool — required before exit:
   - Nothing found → `pass: true`, with `structured: { "notes": "..." }` saying
     which symbols you grepped and which docs you opened. Say what you checked,
     so a reader can tell a considered pass from an empty one.
   - Findings → `pass: false`, with `structured: { "findings": [ { "file":
     "...", "issue": "...", "suggestion": "..." } ] }`. The author is re-invoked
     with your findings verbatim. Each `issue` names the **line** and quotes the
     sentence, says **what is untrue**, and names **what you read** that shows
     it; each `suggestion` is the replacement wording or the head edit to make.
   - The brief cannot be satisfied by rework (it requires a doc change that
     would itself be false) → `pass: false, abort: true`, with the reason in
     `structured`. This skips further rework and escalates to a human.
5. Exit 0.

Write any prose in that verdict as structured markdown: open with **one plain
sentence** stating the verdict, then short bullets or `###` sections, inline
code for paths and symbols, omitting any empty section. Keep it brief and
actionable.
