# Design #533 — The molt: shedding the doc corpus at a milestone

Status: IMPLEMENTED IN PART — the machinery is built and no molt has run yet.
S1–S3 are landed; S4–S6 are the first molt itself, and the slice table says where each stands.

Written against the tree at `ef11e80` (2026-08-09). Every number here was read out
of that commit and the command is given so a reader can re-run it rather than
trust it. One prerequisite is already live and is not a slice of this design: the
heading-anchor invariant, check 6 of `.chug/tasks/check-doc-facts.sh`, landed in
jobs #531 and #532 before this document was argued. A molt rewrites headings and
moves docs, so it needed a corpus where a dangling `#anchor` fails a job; that
corpus now exists, at zero dangling links out of 1,307.

## Current state

*This section is the **mutable head**: it is rewritten to current truth whenever
anything below it changes. Everything after the horizontal rule is append-only —
the argument and its dated corrections, never edited. The head is what you read to
know where things stand; the body is what you read to know why. This follows
[#415](415-knowledge-architecture.md) D2.*

### The rule, in one sentence

**A molt removes true sentences.** Every other doc job removes false ones or adds
missing ones; a molt is the only one whose product is a corpus that states
strictly fewer facts than it did, and whose every removal is named and licensed by
a rule.

### Why the existing gates cannot cover it

The five *gates* in `docs/reference/docs.md` — the rows that fail a job, as
against the readers that only report — are all built to catch a doc saying
something **wrong**. Shedding produces docs that say something **less**. Nothing
in the tree can tell compaction from amputation, which is why a molt needs its own
verification and its own job type rather than being a large `docs` job.

### Decisions

| # | Decision | Where argued |
| --- | --- | --- |
| **D1** | A molt is **per project, at a milestone** — one event, with other work stopped, not a routine per-doc chore | [D1](#d1-per-project-at-a-milestone) |
| **D2** | Sheddings are **deleted**. No archive directory, no retrieval key: git holds the history and the job records hold the work | [D2](#d2-there-is-nowhere-for-sheddings-to-go) |
| **D3** | A **completely-implemented** design doc is deleted outright rather than compacted, which keeps the licensed exception to append-only as small as possible | [D3](#d3-delete-the-doc-do-not-compact-the-body) |
| **D4** | Five **surviving-fact classes**, named so a reviewer rejects by name; everything else is saga, and the default is keep | [D4](#d4-what-survives-and-the-default-is-keep) |
| **D5** | A mechanical **eligibility test** for deletion, so a gate can hold it rather than a reviewer's judgement alone | [D5](#d5-when-a-design-doc-may-be-deleted) |
| **D6** | Execution is **one job with a fan-out inside it**, and the apply phase is **partitioned by destination file** so conflicts cannot form | [D6](#d6-one-job-a-fan-out-and-a-partition) |
| **D7** | **Quiescence is operator discipline**, and it is also what makes "dirtied since the last molt" a well-defined question | [D7](#d7-quiescence-is-discipline-and-it-buys-a-watermark), then [the 2026-08-10 correction](#correction-2026-08-10--s3s-rename-detection-requirement-was-misdiagnosed-job-544) — D7's stated requirement for S3's reader is wrong |

### What the corpus looks like now

Measured at `ef11e80`:

| Figure | Value | Command |
| --- | --- | --- |
| Design corpus | 28,327 lines, 24 docs | `git ls-files 'docs/design/*.md' \| xargs wc -l` |
| Lines below a doc's first `## Correction`/`Finding`/`Amendment` | **17,949 — 63%** | per-doc, first such heading to EOF |
| Docs whose `Status:` is completely `IMPLEMENTED` | **7 docs, 11,508 lines — 40%** | `grep -m1 '^Status:'` per doc |
| Job-number references in *reference* docs | ~578 | `grep -oE '(#\|job )[0-9]{3}'` over the reference tier |
| Anchored links, and dangling ones | 1,307 and **0** | check 6, live since job #532 |

`440-native-worker-daemon.md` is the shape of the problem: 3,451 lines and 22
dated appendages, 19 of them `## Correction`, of which five (jobs #455–#459) are
one debugging chain whose durable
residue is a single sentence. Its head, whose stated purpose in
`docs/reference/docs.md` is to spare a reader "reconstructing the present from an
original plus N corrections", opens with three paragraphs doing exactly that.

### The deletable set

The seven docs whose `Status:` leads with `IMPLEMENTED` and is not `IMPLEMENTED IN
PART`, with the referrers that survive them. **The counting rule** — because S5
and S6 must recompute the same numbers, and a plain `git grep -l` gives a larger
answer: a referrer is a tracked file naming the doc by a backticked path claim
(`check-doc-facts.sh --emit-paths`) or a relative link (`doc-lint.sh
--emit-links`), excluding the doc itself, `docs/README.md`'s catalogue row, and
any doc in this table. Counted that way the figures below are ±1; a full-path
`git grep` reaches 18 for `321-job-groups.md` and is counting mentions rather
than citations.

| Doc | Lines | Surviving referrers | Of which non-doc |
| --- | --- | --- | --- |
| `docs/design/440-native-worker-daemon.md` | 3,451 | 13 | 3 |
| `docs/design/415-knowledge-architecture.md` | 2,853 | **21** | 3 |
| `docs/design/490-agent-work-on-a-mac.md` | 1,755 | 11 | 4 |
| `docs/design/311-job-inputs.md` | 993 | 12 | **6** |
| `docs/design/310-scheduled-jobs.md` | 841 | 5 | 0 |
| `docs/design/293-worker-capacity.md` | 838 | 7 | 0 |
| `docs/design/321-job-groups.md` | 777 | 10 | **7** |

**#415 is excluded from the first molt**, and that exclusion is a decision rather
than a deferral — see [what 415 costs](#what-415-costs-and-why-it-is-not-in-the-first-molt).

### Slices

| Slice | What | Gate on |
| --- | --- | --- |
| **S1** | Per-type `tools:` grant so a job type may declare `Task`/`Workflow`, epoch-gated as `workload_identities:` is | **Landed** (job #535) |
| **S1b** | The **deploy** carrying `TOOLS_SCHEMA_EPOCH` to the running dispatcher, because a config declaring the new epoch cannot merge until a dispatcher carrying it runs (spec §14.3) | **Landed** — the dispatcher runs `fa1c414`, reporting `schema_epoch: 6` |
| **S2** | The machinery: `.chug/jobs/molt.yaml`, its work prompt and evaluators, and `.chug/tasks/check-molt.sh` with a `.test.sh` sibling. A **human** approves at stage 3 — see [the correction](#correction-2026-08-10--the-molt-ends-in-a-human-not-in-an-adversary-job-562) | **Landed** (job #548) |
| **S3** | `.chug/tasks/molt-debt.sh` — the git-derived reader that says how much shell has re-grown since the last molt. D7's stated requirement for it was wrong; [the 2026-08-10 correction](#correction-2026-08-10--s3s-rename-detection-requirement-was-misdiagnosed-job-544) is what it was built to | **Landed** (job #573) |
| **S4** | The first molt's cheap half: the reference tier and `CLAUDE.md`, where narrating a change is *already* out of policy, and the 24 design **heads**, which are already mutable | Proposed |
| **S5** | The deletions — six of the seven docs above, with every surviving referrer repointed or stubbed | Proposed |
| **S6** | #415's own disposition, decided on its own evidence rather than by the rule that covers the other six | Proposed |

S1 through S3 are landed, so `Status:` is `IMPLEMENTED IN PART`: **the molt can
now be run, and has not been.** S4–S6 are the molt itself, and S4 is the first one.

S3's **column set was S3's own choice** — D7 fixed the watermark, the advisory
stance and the absence of a threshold, and nothing else. The reader emits net
growth, the watermark it measured from, commits, new saga sections and new
job-number mentions, plus a `[COMPLETE — deletable]` marker on any design whose
`Status:` satisfies D5 part 1. That marker is the milestone signal and the input to
S5; the other three parts of the eligibility test stay `check-molt.sh`'s, because
only that gate sees a diff. With no molt commit in history the reader measures every
doc from nothing and labels it `never`, which is the honest answer and is how S4's
ordering gets picked.

S1b was S2's real prerequisite rather than S1 — the capability being in the tree is
not the same as a dispatcher that parses it. Its cell carries no `(job #N)` because
a deploy job merges nothing, so there is no `job/N: deploy` squash for check 3 to
resolve; a bare `**Landed**` is the shape that check skips, and the sha is the
evidence in its place. S4 must precede S5, for reasons the body gives; the rest is
orderable.

Two things S2 settled that the body below predates, because implementing them
changed the answer. **`check-molt.sh` cannot source a deleted doc's surviving
citations from `check-doc-facts.sh --emit-paths`**: that emitter prints a claim
only when it *resolves*, deliberately, so a path a molt just deleted is invisible
to it by design. The gate uses a literal `git grep` over tracked files instead,
which also reaches the citers `check-doc-facts.sh` never scans at all — it reads
`*.md` only, and a citation sitting in a `.yaml`, a `.ts` or a generated file is
exactly the contract a stub exists to keep. And **a stub is exempt from the
vanished-landed-row check**: a stub drops its slice table by definition, so
demanding it keep the table it was shed to remove would make stubs pointless. What
that check is for is a row silently *rewritten* — `**Landed** (job #N)` turned back
into `Proposed` — which leaves the table in place.

### Not registered as a concept

`molt` gets no `docs/concepts.md` row. That registry's criterion needs both halves
— a reader must need the term to read some *other* doc, **and** more than one doc
must explain it — and the second fails while this is the only doc that explains it.
Registering it early would also turn check 4 on tree-wide for a word every molt
commit writes. If `docs/reference/docs.md` later absorbs the shed rule as
present-tense policy, two docs will explain it and the row becomes correct then.

---

## The record

Append-only from here. The head above is the current state; what follows is the
argument, and any later correction is appended and linked from the head.

## D1. Per project, at a milestone

The unit is the project, not the document. A molt is taken when the project
reaches a milestone: work stops, the corpus is shed whole, and work resumes.

The alternative — molt each doc as it gets long — was considered and rejected on
two grounds. The first is that the expensive act is **cross-doc**: a fact worth
keeping usually belongs in a reference doc rather than in the design body it is
being lifted out of, and a per-doc molt cannot move a fact from doc A into doc B
without becoming a corpus-wide edit anyway. The second is that per-doc molting has
no natural trigger. A doc is never obviously "long enough", and a rule with a
threshold invites the threshold to become the target.

A milestone supplies the trigger from outside the corpus, which is what keeps the
decision honest. It also makes the shedding legible: one commit, one event, one
before and after.

## D2. There is nowhere for sheddings to go

Shed prose is deleted. Not moved to an archive directory, not preserved behind a
retrieval key in the head, not collapsed into a terminal section the head
disclaims. Deleted.

The record of the work is not what a molt destroys, and this is the load-bearing
observation: the jobs remain in git history and in the platform's job records, so
the *what happened* is recoverable by anyone who needs it. What a molt destroys is
the corpus's obligation to carry the narration in the reading path.

Three alternatives, and the argument is that there is no third option:

- **A tracked archive** is still in the corpus that the *next* molt must molt. It
  converts a one-time cleanup into a permanent liability, which is precisely the
  failure the concept exists to fix. It also costs a `docs/README.md` catalogue row
  per file, and is an orphan unless something links it.
- **An untracked archive** is unmaintainable by construction. #415 S11's `wiki/`
  clause is deferred permanently for exactly this reason: an untracked file is the
  operator's to move, not a job's. Out of every gate's reach is out of every job's
  reach.
- **A terminal section the head disclaims** relabels reading cost without reducing
  it. #415 already has that shape, with a rule and a `## The record` heading, and it
  is the second-longest document in the tree. An agent told to read a design loads
  the whole file regardless of what the heading above the second half says.

One rule survives from that reasoning and is not negotiable: **a design doc's path
never dies.** #415 S9 reduced `docs/design-docs.md` to a pointer rather than
deleting it, because four of its inbound references sat in append-only bodies and
in code. A molt shrinks and removes content; where a path is a contract, the path
stays as a stub.

## D3. Delete the doc, do not compact the body

`docs/reference/style.md`'s doc-claim rule states outright that an append-only
design body "cannot be rewritten, but it can be annotated". A molt that compacted
bodies in place would need that rule excepted, and the exception would be the most
dangerous thing in this design: every future agent would read the molt commit as
precedent for editing bodies at will.

Deleting the doc needs no such exception. Removing a file does not edit an
append-only body; it removes the body along with its head. So the licensed
exception shrinks from "a molt may rewrite any body" to "a molt may delete a design
doc that meets [D5](#d5-when-a-design-doc-may-be-deleted)'s test" — a much smaller
grant, and one a gate can check.

That leaves the docs that are *not* completely implemented, whose bodies keep
growing. This design deliberately does not solve that. S4 takes their **heads**,
which are already mutable and need no new licence, and measurement afterwards
decides whether body compaction is ever worth its exception. The heads are also
where the reading cost concentrates: 440's is roughly 130 lines of chronology, and
the head of #415 spends 113 lines narrating 18 corrections before reaching its
current-state section.

## D4. What survives, and the default is keep

The unit of judgement is the passage, not the sentence, because that is the
granularity at which this corpus accretes — the saga arrives as whole
`### What this does not do` sections, not as stray clauses.

One question sits under every class: *if a future agent never read this passage,
what would it do wrong?* Nothing → shed. Re-litigate a question the project has
already paid for → it survives, and that keep outranks every shed.

| Class | Survives because | Example at `ef11e80` |
| --- | --- | --- |
| **Live constraint** | It binds a file somebody writes tomorrow | #309 §7's rule that a job type must not declare `resources.cpu` or `resources.memory` to run host jobs on macOS — buried at line 918 of an append-only body, so it exists and is unread. A molt **promotes** it into reference rather than keeping it |
| **Rejected alternative still purchasable** | Its whole value is preventive | #309's rejection of `DOCKER_NODES` static config, which relocates a physical fact "into operator-typed config that goes silently wrong after a `nixos-rebuild`". Availability, not correctness, is the test, so nearly all rejections survive; a molt may compact one to a line, and may not drop it |
| **Open hole** | An unpaid debt reads exactly like saga — old, names a job, narrates a failure — and is the highest-value content in the corpus | #415's head recording a gate-scope gap "left open, not closed". **Immune regardless of age** |
| **Measurement still serving as evidence** | The rule it justifies is still live | #415's M7, that the signal "was never absent; it was never *aimed*". A spent count whose property a gate now maintains sheds, because the gate is the better witness |
| **Fact about the world** | It is not about us | That `systemd-run --scope` expands the command line itself. The five jobs that discovered it are the saga; the behaviour is not |

The asymmetry is deliberate. Four classes describe what to keep and one sentence
describes what to shed, because a destructive operation must be biased toward
keeping and the bias belongs in the rule rather than in a reviewer's temperament.

## D5. When a design doc may be deleted

Four conditions, all mechanical, so `check-molt.sh` <!-- intent --> can hold them
rather than resting on a reviewer:

1. `Status:` leads with `IMPLEMENTED` and is not `IMPLEMENTED IN PART`.
2. Every slice row reads `**Landed** (job #N)`. Check 3 of
   `.chug/tasks/check-doc-facts.sh` already resolves those rows against a real
   `job/N: {type}` squash-merge commit, so this condition is free.
3. No surviving tracked `*.md` cites it, after the same commit's repointing.
4. Every code, config or generated citation resolves to a stub at the original
   path.

Condition 4 is where the work is, and two instances in the deletable set show why
it cannot be waived. `321-job-groups.md` is cited from **generated** files —
`web/src/api/types.gen.ts`, `web/src/api/wire-samples.gen.ts` and
`web/src/api/wire-samples.json`, whose exact bytes a cargo test asserts, which is
why `web/.prettierignore` exists — so those citations cannot be hand-edited at all
and must be fixed at their source in `crates/`, or left pointing at a stub.
`311-job-inputs.md` is cited from `.chug/jobs/rollback.yaml`, which is a config
contract rather than prose.

### What #415 costs, and why it is not in the first molt

Design #415 satisfies all four conditions and should still not be deleted yet. It has 21
surviving referrers, including `docs/reference/docs.md`, `docs/reference/style.md`,
`docs/concepts.md`, `docs/overview.md`, `CLAUDE.md`, and two evaluator prompts
under `.chug/tasks/`. More decisively, `docs/reference/docs.md` routes readers to
it **by name** for the question "why is this rule this way", which is #415 D14's
own split of policy from argument.

So what is load-bearing in #415 is the **reasoning**, not the record of work — and
[D2](#d2-there-is-nowhere-for-sheddings-to-go)'s defence of deletion is that the
record survives in git and in the job records. That defence does not reach an
argument. Deleting #415 means promoting each rule's rationale into
`docs/reference/docs.md` beside the rule it explains, which is a larger and
different job than deleting a finished design. S6 exists to decide it on its own
evidence.

## D6. One job, a fan-out, and a partition

The molt is one job, one branch, one commit. The parallelism lives inside the work
task: the work agent orchestrates subagents and reconciles their output itself.
That is what makes "per project" true of the job and not merely of the campaign,
and it removes the need for a slice-per-cluster table that would otherwise encode
ordering the reconcile step can compute.

Four phases:

1. **Survey** — parallel, read-only, one subagent per doc. Each returns a
   structured proposal: per passage, the disposition and the class licensing it,
   and for each survivor, which reference doc should own it. No edits. This is
   where the reading cost goes, and it parallelises perfectly because nothing
   writes.
2. **Reconcile** — serial, in the orchestrator. Merging the proposals makes two
   things visible in one place that no single subagent can see: several designs
   pushing a fact into the same reference doc, which is deduplicated centrally;
   and two proposals asserting the same fact differently, which is a finding worth
   more than either proposal.
3. **Apply** — parallel, writes, **partitioned by destination file**.
4. **Repoint and gate** — serial. Inbound-reference repointing is inherently
   global, so the orchestrator owns it, and then runs the gates locally before
   committing once.

**The partition is the whole trick, and the alternative is worse than it looks.**
Giving each subagent a worktree and merging them converts a *semantic*
disagreement — two docs stating the same fact differently — into a *textual*
conflict, which is the worst available way to discover it: the resolution is
performed by whoever is holding the conflict markers, with none of the context that
produced either side. Partitioning by destination file means no two agents touch a
file and there is nothing to merge. Phase 2 can compute the partition because it
holds every proposal.

Two consequences worth writing down. A subagent must never call `submit_result` or
`submit_eval`, because those are how a run terminates meaningfully and a child
calling one would end the job with a partial result; only the orchestrator reports.
And a dead subagent must be loud — a fan-out where three of forty agents die and
the orchestrator proceeds produces a molt that silently skipped three docs, which
is worse than one that failed.

## D7. Quiescence is discipline, and it buys a watermark

Nothing in the platform can express "no other job may run". At-most-one-in-flight
is per schedule; the per-project merge queue serialises merges rather than work;
work-attempt exclusion is per job. A project-level lease would be new domain
state, a new invariant and a revoke path, built for a ritual that runs a handful of
times, so this design does not ask for one. Quiescence is stated in the runbook and
held by the operator.

It earns more than tidiness, though, and this is the argument for taking it
seriously. Because nothing else lands during a molt, **everything after the molt
commit is ordinary-work dirt by construction** — which is what makes "how much has
the corpus re-grown since the last molt" a well-defined question rather than a
guess. If ordinary jobs interleaved with a molt, the watermark would mean nothing.

S3's reader takes that watermark from the newest `job/N: molt` squash-merge commit,
tree-wide: the same commit shape check 3 already resolves, so it invents no
convention and needs nothing declared — no `last-molted:` front matter and no dates
in prose, for #415 D7's reason. Two properties are required rather than optional.
Rename detection must be on, because a molt *is* a doc reorganisation: without it
`docs/spec.md` reads as a total rewrite (+2,705 / −0) where the true figure is
+243 / −71, since job #441 moved it. And the reader must be advisory, never a gate:
accrued saga is not a defect in the commit that accrued it, which is the same
argument that keeps `.chug/tasks/doc-staleness.sh` advisory.

There is deliberately **no threshold and no "molt recommended" line**. A number
nobody calibrated becomes either noise or a target. The reader ranks and the
operator reads the top.

## What this design does not do

- It does not compact an append-only body. Only deletion of a finished design is
  licensed, and only under D5.
- It does not gate on a line-count or byte delta. Such a budget rewards deleting
  the compact, load-bearing passages, since a rejected-alternatives block is a
  dozen dense lines and a debugging chain is five hundred loose ones. The molt
  reports its delta; nothing fails on it.
- It does not script shedding. A regex deleting lines that match a job number
  would produce a passing, catastrophic diff. The judgement is the product.
- It does not touch doc comments in sources. Those are a `code` job's business,
  and touching them turns a seconds-long gate run into a cold cargo build.
- It does not decide #415. S6 does.

## The risk this design is most likely to be defeated by

An agent sheds a rejected-alternative passage because it reads as settled prose,
and the project re-litigates a question it has already paid for.

Every property of that failure is bad. It is invisible to mechanical accounting,
because the lost sentence names no path, no constant and no link. It is invisible
in review, because a deletion inside a diff of legitimate deletions has no
signature. And it stays invisible for months, until someone proposes the rejected
thing again and no argument is to hand. Worst of all, a mis-shed rejection looks
exactly like saga: it is old, it names a job, and it discusses a decision that is
now simply how the system works.

Three mitigations, overlapping because none is sufficient alone. The shed record
requires a **named class** per removal, so writing the wrong class down is the
moment the error becomes visible to its author. A ratchet counts the corpus's own
preventive vocabulary — `Rejected`, `Why not`, `does not`, `unverified`,
`What this does not do` — and refuses a fall without a named row, which is a proxy
but the only mechanical signal aimed at the right sentences. And the adversarial
evaluator is pointed specifically at rejections and open holes rather than asked
whether anything was lost, because the general question is unanswerable while the
targeted one is not.

## Related

- [`docs/reference/docs.md`](../reference/docs.md) — the doc policy this design
  proposes one exception to, and the five gates that read the corpus.
- [`docs/reference/style.md`](../reference/style.md) — the doc-claim rule, the
  markers, and the sentence about append-only bodies that D3 keeps intact.
- [#415](415-knowledge-architecture.md) — the knowledge architecture, whose S2
  sweep is the closest thing the tree has to a molt and whose S9 precedent is why a
  path can outlive its document.

## Correction, 2026-08-10 — S3's rename-detection requirement was misdiagnosed (job #544)

D7's paragraph on the watermark stated a requirement for S3's reader and got both
halves wrong: the figure it cites was never measured, and the property it demands is
one git already provides. The second half matters more, because an implementer
following it as written would have produced a reader that silently reports every
moved doc as a total rewrite while appearing to have taken the precaution.

**The figure.** The paragraph gives `docs/spec.md` as `+243 / −71` since job #441
moved it. That pair corresponds to no measurement. With
`git diff -M --numstat 15dccc6~1..<sha> | grep 'spec\.md'`:

| At | Figure |
| --- | --- |
| `ccb9cad`, the commit that wrote the sentence | `+112 / −49` |
| `fa1c414`, this correction | `+124 / −52` |

**The diagnosis.** The paragraph says "rename detection must be on". Rename
detection **is** on: `diff.renames` has defaulted to true since git 2.9, this repo
does not set it (`git config --get diff.renames` is empty), and both gits that have
measured this — 2.39.5 in the job container, 2.50.1 on the operator's host — are
well past 2.9. Passing `-M` changes nothing. What actually produces the
total-rewrite misreading is the **pathspec** — limiting the diff to the doc's
current path drops the deletion of the old path, so no rename pair exists for the
detection to find, with `-M` or without it. Four forms over `15dccc6~1..fa1c414`:

| Form | Result |
| --- | --- |
| `git diff -M --numstat A..B -- docs/spec.md` | `2714 0 docs/spec.md` — `-M` present and useless |
| `git diff -M --numstat A..B`, filtered for `spec.md` | `124 52 spec.md => docs/spec.md` |
| `git diff --numstat A..B`, no `-M` at all | `124 52 spec.md => docs/spec.md` |
| `git diff -M --numstat A..B -- docs/spec.md spec.md` | `124 52 spec.md => docs/spec.md` |

The original sentence's `+2,705 / −0` re-measures as `+2,714 / −0`, which is the
other reason to quote a sha: that number grows with every edit to the doc.

**So S3's requirement is restated.** It is not "turn `-M` on" but **never limit the
diff to a single doc's current path** — take the whole-tree diff and match the
`old => new` row, or name both paths in the pathspec.

One further trap, since it produces a plausible wrong number rather than an obvious
one: `git log --follow --numstat` **summed** is a different quantity, and the
command is quoted here because this correction's own first draft got it wrong.
Summing both columns of `git log --follow --numstat --pretty=tformat:
15dccc6~1..fa1c414 -- docs/spec.md` gives `+156 / −84` over 31 rows, against the
end-to-end `+124 / −52`, because per-commit deltas count twice any line touched in
more than one commit. The range matters as much as the method: dropping the `~1`
excludes the move commit's own `9 9  spec.md => docs/spec.md` row and yields a third
plausible number, `+147 / −75`. Neither may be printed as the end-to-end figure.
`--follow` stays correct for **counting commits**, which is the only thing D7 uses
it for.

Three neighbouring figures were re-measured and are **right**, so they are recorded
here rather than changed. D4's "113 lines narrating 18 corrections" for #415 holds —
that doc carries exactly 18 dated appendages and opens `## Current state` at line
130. The 440 figures in the head are correct as of `ef11e80`, the tree this document
was written against; 440 is now 3,564 lines with 20 corrections, which is drift in
another doc and precisely what S3 exists to report. And
`415-knowledge-architecture.md`'s statement about the epoch `infra/README.md`
carried sits inside that doc's append-only body as a dated 2026-08-05 finding. The
epoch has moved since, and the sentence is still correct as history — which is the
reading that paragraph itself argues for. Editing it would break append-only in
order to introduce an error. That this correction's own first draft tripped the
stale-constant gate on that very sentence is the argument in miniature: a number
restated outside the body that dated it goes wrong on its own.

## Correction, 2026-08-10 — the molt ends in a human, not in an adversary (job #562)

D6 gave the molt three evaluators and stopped there: the accounting gate, the
reviewer of aim, and the loss adversary. That stack is complete in the sense that
every *mechanical* objection is spent by the end of it, and incomplete in the
sense that matters, which this design's own risk section already states without
drawing the conclusion.

The failure a molt is most likely to be defeated by is a shed **rejected
alternative whose alternative is still purchasable**. Every property of it
defeats the stack above. It names no path, no constant and no link, so
`check-molt.sh` cannot see it — that gate balances books, and this loss balances.
It has no signature in a diff of legitimate deletions, so a reviewer of aim reads
past it. And the loss adversary can only refute what it thought to look for,
which is the general limit of an adversary aimed at a corpus rather than at a
claim: it is strong on facts that were *stated* and weak on arguments that were
*settled*.

What is left is a question no evaluator here holds: **will this project miss
this?** That is not a fact about the tree, so no gate can resolve it, and it is
not a property of the diff, so no reader of the diff can either. It is knowledge
of what the project is about to do next, and only a person has it. So `molt`
takes a `type: human` evaluator at **stage 3** — last, so the person is asked for
judgement rather than for something a script would have caught — and is the only
shipped job type that ends in an approval a person gives.

The consequence for the work agent is larger than the yaml line. A human deciding
in minutes about a shedding of thousands of lines reads one artifact, so the
summary stops being a changelog and becomes **the review surface**: close calls
first, promotions named with their destination so the approver can confirm the
fact arrived, and anything deliberately kept said out loud. A summary listing
only the obvious deletions hides precisely the decisions worth reviewing, and
`.chug/tasks/approve-molt.md` says a reviewer may fail it for that alone. This is
the one job type where a long summary is correct, because brevity elsewhere
serves a reader who can go and look, and here the summary *is* the looking.

This does not weaken the three machine stages, and it must not be read as
licensing them to relax. It puts a person where the argument always said the
residual risk was.
