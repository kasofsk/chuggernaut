# Chuggernaut — an overview

**Audience:** anyone arriving cold — a new contributor, or an agent on its first
job here — who wants the shape of the whole system before any of the detail.

**This page states no fact of its own.** Every row below is one line of gloss and
a link to the document that owns the thing; where the two disagree, the linked
document is right and the line here is the bug. That is design
[#415](design/415-knowledge-architecture.md) D13: a synthesis page is reference
material, held to the same
[gloss-and-link rule](reference/docs.md#claudemd-and-the-prompts-gloss-they-never-define)
as [`CLAUDE.md`](../CLAUDE.md). So read this as a map, and open the link before
you act on anything it points at.

## Start here

| If you want to | Read |
| --- | --- |
| stand up an instance | [`README.md`](../README.md) — the install path |
| know what the platform must do | [`docs/spec.md`](spec.md) — the normative behaviour, and the source of truth |
| know why it is shaped this way | [`docs/design/000-rationale.md`](design/000-rationale.md) |
| know what a word here means | [`docs/concepts.md`](concepts.md) — the term, and the doc that defines it |
| start changing the code | [`CLAUDE.md`](../CLAUDE.md) — what bites you first |
| find any other document | [`docs/README.md`](README.md#the-catalogue) — one row per doc in this tree |

## The pieces, and the doc that describes each

| Piece | Described in |
| --- | --- |
| The workspace: which crate owns what, and the rules between them | [`docs/reference/crates.md`](reference/crates.md#crates) |
| The units a job can be scoped to, one contract line each | [`docs/reference/modules.md`](reference/modules.md) |
| Why a given module is written the way it is | [`docs/implementation-notes.md`](implementation-notes.md) |
| The dispatcher: what it drives, and how it recovers | [`docs/spec.md` Part 3](spec.md#part-3-dispatcher) |
| The HTTP and NATS surfaces the api bridges | [`docs/spec.md` Part 6](spec.md#part-6-api-layer) |
| What an agent container is handed, and by what contract | [`docs/spec.md` Part 4](spec.md#part-4-agent-interface) |
| Repos, branches and merges | [`docs/spec.md` Part 5](spec.md#part-5-version-control) |
| The operator UI, and the conventions a change to it must hold | [`web/CLAUDE.md`](../web/CLAUDE.md) |
| Identity, secrets, knowledge, security | [`docs/spec.md`](spec.md) Parts [7](spec.md#part-7-identity-and-access), [8](spec.md#part-8-secrets-and-variables), [9](spec.md#part-9-knowledge), [10](spec.md#part-10-security) |
| How the wire and the config survive a version gap | [`docs/spec.md` Part 14](spec.md#part-14-config--version-skew) |

## How a job moves

The same journey is documented from several angles; which one you want depends
on the question you are asking.

| The question | Where it is answered |
| --- | --- |
| Which states exist, and which transitions are legal | [`docs/spec.md` §2.1](spec.md#21-state-machine) |
| What each phase is *for*, and the vocabulary that goes with it | [`docs/reference/design-lifecycle.md`](reference/design-lifecycle.md#the-lifecycle-model) |
| How the dispatcher runs work, judges it, and escalates | [`docs/spec.md`](spec.md) §§[3.2](spec.md#32-work-execution), [3.3](spec.md#33-evaluation), [3.4](spec.md#34-escalation) |
| What lands on the base branch, and what is checked before it does | [`docs/spec.md` §5.1](spec.md#51-branch-management), [§14.3](spec.md#143-merge-time-gate) |

When a word in one of those rows turns out to be doing more work than you
expected, [`docs/concepts.md`](concepts.md) is the first stop: it maps the term
to the one doc that defines it, and says on what criterion a term earns a row.

## What configures it, and where that lives

Job configuration is repo-versioned and travels with the project
([`CLAUDE.md`](../CLAUDE.md), "conventions that bite if you miss them"), so
"how is this job wired up" is a question about a checkout rather than about a
control plane. Which files, and what each may declare:

| What | Where it is described |
| --- | --- |
| `.chug/` as the config root, and everything the platform reads from it | [`docs/spec.md` §1.1](spec.md#11-job) |
| This repo's own job types, prompts, gates and schemas | [`CLAUDE.md`](../CLAUDE.md) — `.chug/jobs/`, `.chug/prompts/`, `.chug/tasks/`, `.chug/schemas/` |
| Runs on a clock, that no operator starts | [`docs/spec.md` §1.1](spec.md#schedules) — schedules, and [§3.5](spec.md#35-timeout-and-deadline) for the tick that fires them |
| Runs an external event stream starts | [`docs/spec.md` Part 13](spec.md#part-13-task-factories-and-ingest) |

## What holds a change to a standard

What this repo uses for CI is the thing most likely to surprise you — read
[`CLAUDE.md`](../CLAUDE.md)'s CI section before concluding anything about what
is or is not gated here.

| What it holds you to | Where |
| --- | --- |
| The tiered blessed practices a reviewer rejects by name | [`docs/reference/style.md`](reference/style.md) |
| Which test tier a new test belongs in, and what each costs | [`docs/reference/testing.md`](reference/testing.md) |
| How docs are written, and which gate fails a job versus warns it | [`docs/reference/docs.md`](reference/docs.md#what-checks-this-and-what-only-reports-it) |
| What the merge gate itself runs | [`.chug/tasks/ci.sh`](../.chug/tasks/ci.sh), glossed in [`CLAUDE.md`](../CLAUDE.md) |

## Where it runs

| Environment | Runbook |
| --- | --- |
| The standing production instance | [`deploy/prod/README.md`](../deploy/prod/README.md) |
| A full platform on one developer machine | [`deploy/dev/README.md`](../deploy/dev/README.md) |
| Terraform roots, and who is allowed to apply them | [`infra/README.md`](../infra/README.md) |
| Worker nodes, capacity, KVM, out-of-band deploys | [`docs/reference/runbooks/`](reference/runbooks/) |

## Where it is going

The tree as it stands and the tree as it is aimed are two different documents,
and mistaking one for the other is the usual way a restructuring job goes wrong.

| What it covers | Where |
| --- | --- |
| The target factoring every incremental change moves toward | [`docs/README.md`](README.md#the-target-factoring) |
| The audit of how far the current tree is from it | [`docs/reference/structure-assessment.md`](reference/structure-assessment.md) |
| Extracting the dispatcher's internal interfaces on the way there | [`docs/reference/contracts.md`](reference/contracts.md) |
| Every decision taken, and the argument for it | [`docs/design/`](design/), catalogued in [`docs/README.md`](README.md#the-catalogue) |

## How to keep this page honest

A sentence here that would need editing when the system changes is a sentence in
the wrong file: it belongs to the doc that owns the thing, and this page should
carry the link instead. The rules that follow from that — the two kinds of doc,
the catalogue row every new page needs, and the gates that check both — are in
[`docs/reference/docs.md`](reference/docs.md), and the argument for them is
design [#415](design/415-knowledge-architecture.md).
