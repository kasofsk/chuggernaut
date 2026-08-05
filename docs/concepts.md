# Concepts — one owner per term

A **routing table**, not a glossary. Each row maps a concept to the one doc that
defines it and the heading that holds the definition. The definition itself
stays where it is argued, because a definition divorced from its argument is
worth less: "the dispatcher is the only writer of job records" means something in
`docs/reference/style.md`, surrounded by the reasoning about why single-threaded
state management is a design constraint rather than a performance note. Lifted
into an alphabetical list it becomes a sentence to memorise. That is design
[#415](design/415-knowledge-architecture.md) D3, and it is why this file holds
no definitions of its own.

`docs/reference/modules.md` is the precedent — a registry the tree is checked
against, rather than a second place for the truth to live.

## What the gate reads here

`.chug/tasks/check-doc-facts.sh` check 4 enforces [#415](design/415-knowledge-architecture.md)
D4: ban duplicate **definitions**, allow duplicate **mentions**. A mention is
free, in any doc, as often as an argument needs one. A *definitional shape*
outside the owning doc is a finding, in either of two shapes — `**Term.**`
opening a list item, and `**Term** is|are|means|refers to` where the term opens
a sentence.

Three properties keep the gate from firing on prose that is not a second
definition, which matters because check 4 runs in the pre-stage of **every** job,
as an error:

- **Only a registered term is looked at.** An unregistered bolded term is
  invisible however it is written. Adding a row is what turns the gate on for a
  concept, so the cost of a wrong row is paid by whoever writes it.
- **The owner is exempt because its row names it**, not because a list of exempt
  files says so — there is no second place for that exemption to drift out of
  step with the registry.
- **What cannot be parsed confidently is skipped in silence**: a term inside
  inline code (a doc quoting a definition states none), a term inside a fence, a
  table cell, a heading, and a sentence that continues onto the next line.

Two shapes are not all of them. The em-dash label — `- **Term** — what it is` —
is commoner in this tree than either gated shape and is **not** checked, because
widening a gate that errors in every job's pre-stage needs an argument, not a
hunch. So a row here is not a claim that the tree holds no other explanation of
the term; it is a claim about where the definition is argued. The measurement
and what it leaves undecided are in
[#415](design/415-knowledge-architecture.md#the-shape-d4-did-not-name).

`CLAUDE.md` is held to the rule, not exempted from it. It restates other docs on
purpose — it is read first and its value is front-loading what bites you — and
[#415](design/415-knowledge-architecture.md) D5 already gives it the rule that
makes that safe: one line of gloss plus a link to the owner, never a second
definition. A gloss passes here for free, because a gloss is a mention. The
argument against exempting the file is #415's own: the M1 defect, a normative
directive to protect a module that had been deleted, was in `CLAUDE.md`, so a
file-level exemption would exempt the most damaging instance in the tree. The
same reasoning covers `.chug/prompts/`, which is injected into contexts that may
hold nothing else.

## The criterion for a row

Both halves, or no row:

1. a reader must understand the term to read some *other* doc, and
2. more than one doc explains it.

The registry is roughly a dozen rows and is meant to stay that way. A hundred
rows would be the glossary D3 rejected, reached by a different route — and every
row is a live constraint on how every other doc may write about that term.
Before adding one, check that the term has exactly **one** definition today: a
row over two definitions fails the gate on merge, and the temptation then is to
weaken the rule rather than fix the duplicate.

## The registry

| Concept | Defined in | What the row settles |
| --- | --- | --- |
| `job` | [`docs/spec.md` §1.1](spec.md#11-job) | The unit of delivery: a node in the DAG, with a branch, a base ref and a lifecycle |
| `task` | [`docs/spec.md` §1.2](spec.md#12-task) | One execution — container, agent or human action. Both phases run tasks |
| `job type` | [`docs/spec.md` §1.1](spec.md#job-type) | The declarative definition in `.chug/jobs/{type}.yaml`; a job is an instance of one |
| `job branch` | [`docs/spec.md` §5.1](spec.md#51-branch-management) | `job/{id}`, created at Work entry, deleted at Done; scratch, not the deliverable |
| `merge gate` | [`docs/spec.md` §3.3](spec.md#merge-gate) | The wrap-up check that the tested stacking still holds when HEAD moved |
| `epoch` | [`docs/spec.md` §14.1](spec.md#141-the-n1-wire-contract) | The monotonic integer declaring one wire surface's generation, for N±1 skew |
| `work` | [`docs/reference/design-lifecycle.md`](reference/design-lifecycle.md#the-lifecycle-model) | The fallible phase: the action a job exists to perform |
| `evaluation` | [`docs/reference/design-lifecycle.md`](reference/design-lifecycle.md#the-lifecycle-model) | Independent judgment of the work, with three outcomes and not two |
| `wrap-up` | [`docs/reference/design-lifecycle.md`](reference/design-lifecycle.md#the-lifecycle-model) | Making the outcome durable: platform-owned, designed to be infallible |
| `triage` | [`docs/reference/design-lifecycle.md`](reference/design-lifecycle.md#the-lifecycle-model) | The outlet when automation has run out — never a retry loop in disguise |
| `evaluator` | [`docs/reference/design-lifecycle.md`](reference/design-lifecycle.md#vocabulary) | An evaluation *slot declared on the type*; the thing that executes is a task |
| `single writer` | [`docs/reference/style.md`](reference/style.md#tier-3--principles) | The dispatcher owns every state record; a second writer is the wrong shape |

## Terms deliberately not registered

Measured rather than assumed, and recorded so the next author does not re-derive
it:

- `slice`, `tier`, `claim` — each names two things here. A tier is a test tier
  (`docs/reference/testing.md`) or a rule tier (`docs/reference/style.md`); a
  claim is a human's claim on a work attempt or a doc's claim about the tree; a
  slice has no single doc that defines it at all. One row per concept is the
  registry's rule, so an ambiguous word waits for a doc to own each sense.
- `linked-origin` — genuinely defined twice today, in `README.md` and in
  `.claude/skills/chug-install/SKILL.md`, in the same words. Registering it
  without resolving that would fail the gate on merge, and resolving it is a
  decision about how self-contained a skill must be for an agent that has no
  checkout. Left as a finding, not papered over.
