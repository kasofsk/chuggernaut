# Run the molt

You are working **on the chuggernaut codebase itself** — the platform running
you. The **Job Brief** appended below names this molt's scope: which docs are in
it, and which fully-implemented designs it may delete. If there is no Job Brief,
call `submit_result` with a summary explaining that no ticket was provided, and
exit non-zero. **A molt without a brief has no deletion set, and you must not
invent one.**

A molt is the one job type that **removes true sentences**. Every other doc job
removes false ones or adds missing ones. Read `docs/design/533-molt.md` first —
its head is the current state and D4 is the list of what survives. Read
`docs/reference/docs.md` for the two kinds of doc and their opposite update
rules.

**The default is keep.** You are shedding the *saga* — the narration of how a
decision was reached — not the decision, and not anything still binding future
work. When a passage could be either, keep it.

## What survives, and you must name the class

Five classes. A passage you shed needs no justification; a passage you shed
must, however, have been *considered* against all five, and every shed you record
in the ledger names the class that licensed it.

1. **A live constraint** — anything that still forbids or requires an action
   today. Promote it into a reference doc; a constraint buried at line 900 of an
   append-only body already exists-and-is-unread.
2. **A rejected alternative whose alternative is still purchasable.** The test is
   *availability*, not correctness, so nearly all of these survive. May compact
   to one line; may not drop. This is the highest-risk class — see below.
3. **An open hole** — anything unclosed, unverified, unrun, or deferred with a
   cause. **Immune regardless of age.** It reads exactly like saga (old, names a
   job, narrates a failure) and is the most valuable content in the corpus.
4. **A measurement still serving as evidence** for a decision that still stands.
   A measurement whose only role was to justify a step already taken is saga.
5. **A fact about the world** — how a tool behaves, what a platform does. The
   fact survives; the jobs that discovered it do not.

**The highest risk in this entire job** is deleting a rejected alternative
because it reads as settled prose. It is invisible to mechanical accounting (it
names no path, constant or link), invisible among legitimate deletions in a diff,
and stays invisible until someone re-proposes the rejected thing with no argument
to hand. It *looks* exactly like saga: old, names a job, discusses a decision
that is now simply how the system works. When you are unsure whether a passage is
a rejection, it is a rejection.

## Four phases

You are an **orchestrator**. The reading cost is the whole corpus and it
parallelises, so fan out with `Task`; your job type grants it.

**Phase 1 — Survey. Parallel, read-only, one subagent per doc in scope.** Each
returns a structured proposal and **writes nothing**: per passage, its
disposition, the class licensing a shed, and for each survivor, which reference
doc should own it. This is where the reading goes, and it parallelises perfectly
because nothing writes.

**Phase 2 — Reconcile. You, serially.** Merge the proposals. Two things become
visible only here, which is the reason this phase exists: several designs all
pushing a constraint at the same reference doc (dedupe and order it centrally),
and **two proposals asserting the same fact differently — which is a finding,
not a merge conflict.** Resolve it by reading, not by picking. Output is the
write plan and a **partition of destination files**.

**Phase 3 — Apply. Parallel, writing, partitioned by destination file.** One
subagent per destination doc, carrying every promotion into it. Deletions are
trivially disjoint. Because phase 2 computed the partition, no two agents touch
a file and there is nothing to merge.

**Phase 4 — Repoint and gate. You, serially.** Inbound-reference repointing is
inherently global, so you own it. A deleted doc's referrers split three ways:

- **The referrer is also being deleted** — free.
- **The referrer is a surviving doc** — the citation exists because that doc
  needed the fact, so **the fact must land in a reference doc first** and the
  citation repoints there. This is the molt's core act.
- **The referrer is code, config, or generated output** — the path is a
  contract. **Leave a stub at it.** Generated files (`web/src/api/*.gen.ts`,
  `web/src/api/wire-samples.json`, `.chug/schemas/*.json`) cannot be hand-edited
  and their source is in `crates/`, so editing them would flip the cargo gate.

Then run the gates yourself before committing: `.chug/tasks/check-molt.sh`,
`.chug/tasks/check-doc-facts.sh`, `.chug/tasks/doc-lint.sh`.

## Three prohibitions on subagents

State all three in every subagent prompt you write. They are not stylistic.

1. **A subagent must NEVER call `submit_result` or `submit_eval`.** Those
   terminate the run. A child calling one ends this job with a partial result,
   and `mcp__chuggernaut-channel` *is* available to it through the inherited
   settings — so the prohibition has to be said, not assumed. Subagents return
   structured text to you. **Only you report.**
2. **A dead subagent must be loud.** Count returns against the set you
   dispatched. A fan-out where three of forty agents die and you proceed produces
   a molt that silently skipped three docs — worse than one that failed. Report
   any shortfall in `structured` and do not paper over it.
3. **No worktrees, and no git merge between subagents.** The partition makes
   conflicts impossible by construction. Merging would convert a *semantic*
   disagreement — two docs stating one fact differently — into a *textual* one,
   which is the worst possible way to discover it.

Keep the phase-2 input compact: pointers, one-line rationales and class codes,
not prose. Forty full proposals will not fit, and the schema is what keeps them
small.

## The ledger

`.chug/molt-ledger` is one line per shed passage: what it said, **the named class
that licensed it**, and where the survivor went. Each subagent's structured
return *is* its ledger fragment — the class is a schema field, so an agent that
cannot name a class cannot emit the shed. Concatenate the fragments.

It is read from the lines your **diff adds**, exactly as `.chug/doc-reread` is,
so a rebase cannot destroy it and a line already merged never becomes a standing
waiver. `.chug/tasks/check-molt.sh` cross-checks its coverage against your diff.

## Rules that will fail the job if you break them

- **Append-only is append-only.** A design doc's body below its `---` is never
  edited. The licensed exception is **deletion of a whole fully-implemented
  design** named in your brief — which edits no body, because removing a file is
  not rewriting one. Heads are mutable and are the point of S4.
- **No source edits.** Doc comments are a `code` job's business. Touching
  `crates/**` or `web/**` turns a seconds-long CI into a cold cargo build.
- **A deletion needs its eligibility**, checked by `check-molt.sh`: `Status:`
  leading with `IMPLEMENTED` and not `IMPLEMENTED IN PART`; every slice row
  `**Landed** (job #N)`; no surviving tracked `*.md` citing it after your
  repointing; every code/config/generated citation answered by a stub.
- **Do not delete a design your brief does not name**, however eligible it looks.

## Finishing

1. One commit on the current branch (you are already on the job branch), then
   push. `.githooks/pre-commit` runs the fast doc gates as an advisory pass — act
   on what it prints.
2. Narrate with `update_status` at least four times — once per phase.
3. Call `submit_result` with:
   - `summary`: a short markdown report (this becomes the merge commit body).
     Open with **one plain markdown-free sentence** stating the outcome, then
     `###` sections — `### What changed`, `### What was shed, by class`,
     `### How verified`, `### Notes`. Omit an empty section. Include the counts:
     docs surveyed, subagents dispatched and returned, passages shed per class,
     designs deleted, referrers repointed, stubs left.
   - `structured`: `{ "files_changed": [...], "subagents_dispatched": N,
     "subagents_returned": N, "shed_by_class": {...}, "notes": "..." }`.
4. Exit 0.

Two evaluators read your output adversarially: `check-molt.sh` balances the
books, and `.chug/tasks/review-molt-loss.md` fans out readers instructed to name
a fact the base stated that your HEAD does not. Neither can be satisfied by a
tidy diff — expect to be asked *which class licensed this*.
