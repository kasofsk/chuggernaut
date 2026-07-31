# Design #321 — Job groups (tying a job to the thing it belongs to)

Status: IMPLEMENTED — shipped in jobs #324, #330, #331 and #332.

All three slices landed: slice A — `crates/types/src/groups.rs`, `Job.groups`
and the write paths — in #324; slice B — `crates/types/src/rollup.rs`,
`req.groups.list`/`req.designs.list` in
`crates/dispatcher/src/handlers/groups.rs`, and `GET .../groups` and
`GET .../designs` — in #330; slice C — the Designs view
(`web/src/pages/Designs.tsx`) in #331, and the group chips, filter and picker
in #332. The **Deferred** list under
[Minimum useful version](#minimum-useful-version) is untouched and still
describes what has not been built.

Note that the survey in
[Corrections](#corrections-verified-against-the-tree) and the worked example
under [Decision 8](#decision-8-status-hygiene-without-a-second-writer) quote the
`Status:` lines as they read at `00dd0dc`; several have since been amended —
including this one, and the `design/311-job-inputs` case Decision 8 uses as its
example. They are left as written, because the argument is the record of what
the tree said when it was made.

Written against the tree at `00dd0dc`. Every claim about current behavior below
was read out of `spec.md` and the source in this repo; where the brief and the
tree disagree, the tree wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree).

This doc deliberately reopens a decided direction. Job **#185** (Frozen,
`design`) records a settled shape — an implementation ticket carrying
`deps = [all children]`, where the existing Blocked → unblock-on-Done machinery
*is* the roll-up. Two of its six points survive here intact and are load-bearing
([Decision 9](#decision-9-what-happens-to-185-86-and-88)); three do not survive
contact with the tree. The supersede is argued from
[Decision 1](#decision-1-deps-cannot-carry-membership), not assumed.

Related: [spec.md](../../spec.md) §1.1 (the `Job` record, `knowledge_tags`,
derived `retry_count`/`rework_count`), §1.2 (claims), §1.4 (buckets, the `rdeps`
derived cache), §2.1 (the state machine, Draft edits, batches, the Revoked
cascade), §2.2 (release validation's three passes), §3.6 (startup rebuild), §4.3
(the job brief), §4.4 (upfront knowledge injection), §6.1/§6.2 (subjects and
HTTP), §6.3 (events), §13.4 (factory provenance), §14 (config and version skew),
Appendix: Deferred; [design-lifecycle.md](../../design-lifecycle.md) (the job
lifecycle); [STYLE.md](../../STYLE.md) (Tier 1 no-duplication and pure `types`;
Tier 2 #2 asserts, #3 bounds, #4 naming, #6 tests; Tier 3 single writer,
simplicity, zero technical debt); [crates.md](../../crates.md) (crate
ownership); [testing.md](../../testing.md) (test tiers);
[NORTH-STAR.md](../../NORTH-STAR.md) and [contracts.md](../../contracts.md)
(decider/effect factoring); [CLAUDE.md](../../CLAUDE.md) (single writer, `store`
is the only crate that talks to NATS, `types` is pure data).

Sibling designs cited for precedent: [#311](./311-job-inputs.md) (the additive
`Job` field and its skew analysis — the closest precedent for what is proposed
here), [#310](./310-scheduled-jobs.md) (derived-over-stored, and the rejected
`Job.origin` consolidation), [#308](./308-gha-port.md),
[#309](./309-host-native-execution.md), [#293](./293-worker-capacity.md),
[#313](./313-workload-identity-image-builds.md).

## Problem

On 2026-07-30 the operator asked "what's left to build before we import
beacon." Answering it required grepping the tree for symbols. That grep, re-run
in this worktree, is the whole problem statement:

| Symbol | Occurrences outside its own design doc |
| --- | --- |
| `HostBackend` ([#309](./309-host-native-execution.md)) | none |
| `SCHEDULES_MAX` ([#310](./310-scheduled-jobs.md)) | none |
| `workload_identities` ([#313](./313-workload-identity-image-builds.md)) | none |
| `INPUTS_SCHEMA_EPOCH`, `INPUTS_COUNT_MAX` ([#311](./311-job-inputs.md)) | `crates/types/src/version.rs`, `crates/types/src/job_type.rs`, `crates/types/src/inputs.rs`, `crates/types/src/job.rs` |

So #311 is shipped — `Job.inputs` is on the record, `CONFIG_SCHEMA_EPOCH` is at
2, `web/src/components/JobInputs.tsx` renders it — and the other three are not a
line of code. All four docs open `Status: PROPOSED`.

The platform holds every fact needed to say so and cannot be asked. The
adjacency is even visible in git: `job/311: design` is followed by `job/314`,
`job/315`, `job/316`, `job/317` (`code`) and `job/319` (`web`). But adjacency is
not membership, and git cannot be the carrier anyway — job 318 was a `deploy`,
and `.chug/jobs/deploy.yaml` declares `wrap_up: type: none`, so its branch was
discarded unmerged and it left **no commit at all**. A group whose members
include a deploy is invisible to git by construction.

Three concrete groups exist today with no platform representation. Two of them
break the obvious model before it is written:

- **#311** → 314, 315, 316, 317 (slices A1–A4), 319 (slice B web), and deploy
  318, which existed only to carry the epoch bump.
- **#293** → 295–301 (slices 1–7).
- **#308** → design jobs 309, 310, 311, 313 — *and* 320, an amendment to #308
  itself.

Note what #308's group contains: an amendment to its own subject, and four
design jobs that are not implementations of it. Any model that expresses only
"child implements parent" fails that case, and so does any model that requires
membership to imply an ordering.

## Corrections (verified against the tree)

Five claims that shaped the brief need adjusting. Three of them change an
argument.

1. **A revoked child does not strand the roll-up — it deletes it.** The brief
   says a revoked child leaves the impl ticket's dep unsatisfiable so it
   "blocks forever", and that #185's answer is that the operator edits the wave
   or refiles. The tree is worse than that. `JobGraph::cascade_targets`
   (`crates/domain/src/graph.rs`) walks *transitive dependents* whose state is
   `Frozen | Blocked | Ready` and returns them; `Core::revoke_job`
   (`crates/dispatcher/src/core.rs`) revokes the job and every returned target,
   publishing `job-revoked` with `{"cascaded": [...]}`. Spec §2.1's Revoked row
   says the same. #185's impl ticket sits in **Blocked** by construction — which
   is cascade-eligible. Revoking one child silently revokes the roll-up marker.
   This turns [Decision 1](#decision-1-deps-cannot-carry-membership)'s first
   bullet from a friction into a disqualifier.

2. **`Job.schedule` does not exist.** The brief names `Job.factory` *and*
   `Job.schedule` as the provenance fields to distinguish a group from.
   `crates/types/src/job.rs` has `factory: Option<String>` and no `schedule`;
   the token appears nowhere in `crates/types/`. It is a
   [#310](./310-scheduled-jobs.md) proposal and #310 is unimplemented. Two
   further details matter: `factory` carries **no** `#[serde(default)]` (it is a
   required field on the wire, unlike every field added since), and per #310's
   own correction 2 it still has no writer outside test fixtures. The provenance
   comparison therefore has exactly one real member, and that member is inert.

3. **Six design docs read `PROPOSED`, not five, and the status vocabulary
   already has four values.** `docs/design/` holds eight documents.
   `Status: PROPOSED` — 293, 308, 309, 310, 311, 313. `Status: DRAFT` — 169.
   `Status: FINDING` — 238. The count does not change the argument; the
   *variety* does. A vocabulary is already in use with no schema and no
   enforcement, which is an input to
   [Decision 8](#decision-8-status-hygiene-without-a-second-writer).
   The brief's claim that **no design doc has front-matter** is correct — all
   eight open with a `# Design …` heading and a prose `Status:` line.

4. **The tree's naming convention embeds the design job's seq, and #185's
   proposed artifact does not match it.** #185 asks for `docs/design/epics.md`
   and states that nothing durable should reference design-job seq numbers as
   identity. Every one of the eight docs in the tree is named
   `<seq>-<slug>.md`. This is not a contradiction of #185's principle — the
   *path* is the identity and it is stable whatever it contains — but it does
   mean the identity this doc keys on already exists, is already unique, and is
   already human-readable. [Decision 2](#decision-2-a-group-is-a-namespaced-string-label-on-the-job-record)
   depends on that.

5. **`Job` carries no `deny_unknown_fields`.** Verified by grep over
   `crates/types/`: every occurrence is in `crates/types/src/job_type.rs`, on a
   nested job-type block (§14.2's gate-relevant rule). Every field added to
   `Job` since the record was first written carries `#[serde(default)]`. So an
   added field is additive in both directions, exactly as #311's Skew surface 2
   records — confirmed rather than taken on trust, per
   [Decision 6](#decision-6-skew-and-why-no-epoch-moves).

## What is true today (verified)

| Thing | Where | State |
| --- | --- | --- |
| `Job.deps` are ordering edges; validated for existence, self-edge, duplicates, cycles, and *not Revoked* at release | `wiring_errors` in `crates/domain/src/release.rs`; spec §2.2 | Shipped |
| Revoke cascades transitively to `Frozen`/`Blocked`/`Ready` dependents | `JobGraph::cascade_targets` in `crates/domain/src/graph.rs`; spec §2.1 | Shipped |
| The only in-edge to `Draft` is `(Frozen, Draft)` | `crates/domain/src/state.rs`; `Core::draft_job` | Shipped |
| A Draft `PATCH` is a **full-field replace** of the creation payload | spec §2.1 Draft row; `Core::update_job` | Shipped |
| `Job.knowledge_tags` resolve to `.chug/tags/{tag}.md` at `base_ref` and are concatenated into the **work agent's system prompt** | `Exec::knowledge_block` in `crates/dispatcher/src/exec.rs`; spec §4.4 | Shipped |
| `Job.members` / `Job.batch_id` — N tickets, one branch, one run, one merge | `crates/types/src/job.rs`; spec §2.1 batches | Shipped |
| `Job.factory` provenance | `crates/types/src/job.rs` | Field exists, no writer |
| `Job.schedule` provenance | — | Does not exist |
| `Job.inputs` (#311) | `crates/types/src/job.rs`, `crates/types/src/inputs.rs` | Shipped |
| Every project's full job set held in memory | `Core::graphs: HashMap<String, JobGraph>`; `JobGraph.jobs: HashMap<u64, Job>` | Shipped |
| Derived-not-stored precedent | `retry_count`/`rework_count` (spec §1.1), `rdeps` (spec §1.4, §3.6) | Shipped |
| An operator mutating a **live** job record outside Draft | `POST .../jobs/{seq}/claim` → `claim_next`; spec §1.2 | Shipped |
| List projection with a build-time mirror check on `Job`'s field set | `JobSummary` + `job_summary_mirrors_job_fields` in `crates/types/src/job.rs` | Shipped |
| `job-updated` payload shape | `serde_json::json!({ "fields": changed })` in `crates/dispatcher/src/core.rs` | Shipped |
| Repo browser at default HEAD | `GET .../tree`, `GET .../file`, `web/src/pages/FileView.tsx` | Shipped; non-YAML files render as `<pre>` |
| Markdown renderer | `web/src/components/Markdown.tsx` | Shipped; used by `JobDetail`/`Transcript`/`DraftEditor`, **not** by `FileView` |
| Design-doc front-matter | — | No doc has any |

## Decision 1: `deps` cannot carry membership

Four findings, each read from the tree, in descending weight. The first two are
disqualifiers rather than frictions and neither appears in #185.

**1. The revoke cascade destroys the roll-up.** Per
[correction 1](#corrections-verified-against-the-tree), an impl ticket in
Blocked is cascade-eligible. Make this concrete against a real group: #293's
children include three Frozen jobs. Revoke one — a routine act; #185 itself
contemplates it — and the cascade walks the reverse edge to the Blocked impl
ticket and revokes it. The "this design is outstanding" marker is gone, the
group is gone, and the only trace is a `cascaded` array in a `job-revoked`
payload. #185's answer ("the operator edits the wave or revokes/refiles the impl
ticket") presumes the operator is *deciding* to refile. Here the platform has
already decided.

There is no version of this that is a tuning question. The cascade is correct
behavior for a prerequisite edge — a job whose upstream will never land should
not sit Blocked forever — and it is wrong for a membership edge for exactly the
same reason. One edge type cannot be both.

**2. A released impl ticket's `deps` are immutable — permanently.**
`crates/domain/src/state.rs` admits exactly one transition into `Draft`:
`(Frozen, Draft)`. `Core::draft_job`'s doc comment says "never-released", and
the guard that enforces it is the transition table itself. `PATCH` is Draft-only
(409 otherwise, spec §2.1). So once the impl ticket is released into Blocked,
**no endpoint can add a child**, ever.

That inverts #185's point 5. Waves are not a cost to price against a benefit;
they are the only mechanism available, and they carry a hole: while the ticket
stays Frozen so scope *can* grow, it is not Blocked, so the visible marker the
whole design rests on **does not exist during the discovery period** — precisely
when a group is most useful. #311's slices were not known up front; nor were the
four children of #308 plus its own amendment.

Price the Frozen→Draft→edit→finalize loop (#166) honestly too: `PATCH` is a
full-field replace of the creation payload — type, title, description, deps,
`knowledge_tags`, eval, timeout, model, inputs. Adding one seq to a list means
resending all of it, and a client that omits a field clears it. That is a
correct design for "edit a draft before committing it"; it is a bad one for
"note that another job joined this group."

**3. `deps` mean ordering, and the ordering is enforced.** A dep is not an
annotation. It holds the dependent in Blocked, and its upstreams' work is in the
dependent's base (`crates/types/src/job.rs`: "Edges are ordering: upstreams must
be Done first"). #308's group contains job 320, an amendment **to #308 itself**,
and deploy 318, which unblocked #311 by carrying its epoch bump. Neither is an
implementation of the group's subject. Recording their kinship as a dep asserts
an ordering constraint that is not merely unnecessary but false — and a false
ordering is not inert, it parks a job.

**4. A wave cannot even be released if a child was revoked first.**
`wiring_errors` rejects release with "depends on revoked job #N". So the failure
in finding 1 has a same-shaped sibling at the other end of the lifecycle.

**What survives.** Blocked → unblock-on-Done is a genuinely good roll-up *for a
genuine prerequisite*. A design that actually gates a downstream job should
still express that with `deps`, and this doc adds nothing that competes with
it. Groups and deps are orthogonal: a job can be in a group with no deps, and
depend on jobs in another group. Keeping them orthogonal is the point —
overloading `deps` is what produces all four findings above.

## Decision 2: a group is a namespaced string label on the job record

> **`Job.groups: Vec<String>`** — `#[serde(default)]`, operator-set, many per
> job, inert to execution.

Names are lowercase, matching `^[a-z0-9][a-z0-9._/-]*$`, at most
`GROUP_NAME_LEN_MAX = 128` characters. The convention — a convention, not a
platform rule — is a namespace prefix: `design/311-job-inputs` for a design
group, where the segment after `design/` is the doc's basename without `.md`;
anything else for the rest (`beacon-import`, `ops/fleet-refresh`).

### The three shapes, weighed

**Option A — a free-form label on the job record (recommended).**
*For:* it stores exactly the one fact nothing else carries and adds no new state
anywhere. It rides the record the dispatcher already exclusively writes, so the
single-writer story needs no argument. It enumerates for free from
`Core::graphs`. It reaches the UI through `JobSummary` with no join.
*Against:* group names are unvalidated strings, so a typo makes a second group
of one. That is real, and the mitigation is a UI picker over the existing names
plus the `docs/design/` listing — not server-side referential integrity, which
would cost the properties in
[Decision 5](#decision-5-membership-is-mutable-after-release-including-on-terminal-jobs).

**Option B — a registered entity with its own record.** A `groups` KV bucket,
`groups.{owner}.{project}.{name}` holding a member list, a title and a
description.
*For:* names are validated by construction, an empty group can exist (useful
for "I am about to file these"), and a group can carry metadata a label cannot.
*Against:* it is a second record that can drift from the job records — the
precise failure `rdeps` is designed around and `retry_count` avoids by not
existing. It needs a reverse index to answer "which groups is job N in", which
is a second derived cache. It reopens #89's rejected "epic as a distinct
entity". And it buys enumeration, which Option A gets for free because the
dispatcher already holds every `Job` of every project in memory. Rejected: the
one thing it uniquely offers — an empty group — is a feature whose absence is
arguably correct (see
[Decision 7](#decision-7-the-api-surface)).

**Option C — a reference to a path under `docs/design/`, and only that.**
*For:* the narrowest possible surface, and it makes the registry authoritative.
*Against:* the operator's ask explicitly includes non-design groupings, and
`docs/` already holds `runbooks/` beside `design/`. Worse, it makes membership
depend on a repo read: validating that `docs/design/311-job-inputs.md` exists
requires resolving a ref, and the ref for a Done job is a historical
`base_ref` at which the doc may not have existed. A fact about a finished
ticket should not be conditional on a git read. Rejected as too narrow *and*
as the wrong coupling — Option A can still *use* a doc path as a name without
depending on one.

### Consequences of Option A that are decisions

- **A job may belong to several groups.** Job 318 is part of
  `design/311-job-inputs` and could reasonably also be part of a
  `prod-deploys` group; job 320 amends #308 and belongs to
  `design/308-gha-port`. One-group-per-job would force the operator to choose
  and would fail #308's case outright.
- **The registry is advisory.** For a `design/`-namespaced group the registry is
  `docs/design/` — which is already enumerable through the existing
  `GET .../tree`. A group whose doc does not exist still works; it just renders
  without a title. This is exactly the knowledge-tag posture (spec §4.4: "tags
  without a file are skipped"), and it is what keeps the field writable without
  a repo read.
- **Bounds** (STYLE.md Tier 2 #3): `GROUPS_COUNT_MAX = 8` per job and the
  128-character name cap, both hard errors rather than truncation, both in a
  new pure `crates/types/src/groups.rs` mirroring `crates/types/src/inputs.rs`.
  `types` stays pure data (Tier 1), so the validator is one function shared by
  create, the Draft edit, and the group endpoint — one implementation, per
  CLAUDE.md, and one that `.chug/tasks/check-duplication.sh` will not let be
  copied.

## Decision 3: what a group is not

The design is not credible unless each of the four neighbours is separated
precisely. Each separation below is a checkable difference, not a vibe.

### Not `knowledge_tags`

Structurally these are the same thing: a `Vec<String>` on the job record,
operator-supplied at creation, unioned with a job-type default. That similarity
is the reason to state the difference sharply.

A knowledge tag is an **execution input**. `Exec::knowledge_block`
(`crates/dispatcher/src/exec.rs`) resolves the union of `JobType::knowledge` and
`Job.knowledge_tags`, reads `.chug/tags/{tag}.md` at `base_ref` for each, and
concatenates them into a `## Project Knowledge` block injected via
`--append-system-prompt` (spec §4.4, work containers only). Adding a knowledge
tag **changes what the agent is told**. Adding a group must change nothing about
the run at all.

Two consequences follow, and both are enforcement rather than prose:

1. **No code path that composes a container's environment, prompt, or resolved
   config may read `Job.groups`.** The regression test is the one §4.3 already
   uses for `cover_html`: the job brief and the container env are byte-identical
   with and without groups. That is the negative-space assert (STYLE.md Tier 2
   #2) that makes "inert to execution" a property rather than an intention, and
   it is what [Decision 5](#decision-5-membership-is-mutable-after-release-including-on-terminal-jobs)
   leans on when it argues that annotating a Done job is safe.
2. **Deliberately no shared machinery.** A knowledge tag is resolved at
   `base_ref` and is therefore pinned to the run; a group is a fact about the
   ticket that may be set long after the run finished. A shared resolution path
   would have to be wrong for one of them. What they may share is a string-shape
   validator in `types`, which is a helper, not a concept.

### Not a batch

`Job.members` / `Job.batch_id` (spec §2.1): N tickets collapse into **one**
branch, **one** run, one evaluation under the union of their criteria, and one
merge that completes every member. Members are absorbed into `Batched`, stop
being independently executable, and are un-absorbed back to Frozen when the
batch is revoked or reopened for editing.

The difference is not arity, it is *what is being changed*. A batch changes
**how the members execute**. A group is a statement **about** jobs that execute
independently and are unaffected by it. Three checkable consequences:

- A batch has its own job record and its own seq — and can therefore itself be
  a member of a group. A group has neither.
- `batch_id: Option<u64>` — a job is in at most one batch. A job is in up to
  `GROUPS_COUNT_MAX` groups.
- Batch membership is validated hard at creation (≥2 members, each
  Frozen / same-type / unbatched / not-a-batch / no-inputs, 422 otherwise; spec
  §6.2) because it is about to change how those jobs run. Group membership is
  validated for string shape and nothing else, because it is about to change
  nothing.

The operator's read that batches are "a little different" is right in the
narrow sense that both relate many jobs. They are opposite in every other
respect: a batch is a **write** against the members, a group is a **read** over
them.

### Not `deps`

[Decision 1](#decision-1-deps-cannot-carry-membership).

### Not `Job.factory` (nor the proposed `Job.schedule`)

Provenance answers *what created this job*: written once by the origination
path, never by a human, and true forever. A group answers *what this job is
part of*: written by a human and revisable as understanding improves. The shapes
rhyme; the write paths and the truth conditions do not.

Per [correction 2](#corrections-verified-against-the-tree) there is exactly one
such field in the tree and it has no writer, so there is nothing to consolidate
even if consolidation were wanted. Note the tension #310 Decision 7 identified:
it rejected folding `factory`/`schedule` into a single `Job.origin` because
`factory` is serialized into the web wire type (`web/src/api/types.gen.ts`) and
removing a serialized field is a **breaking** change against §14.1's
additive-only rule. That tension binds this doc too, in the direction it can be
obeyed for free: **add `groups`, remove nothing, rename nothing.**

## Decision 4: the edge is stored, every aggregate is derived

Revoked job #89 asked "derived or stored?" and it is the sharpest question here.
The answer is that it is two questions, and #89 conflated them.

**The membership edge is irreducibly stored.** Nothing in the tree carries it.
Git history shows adjacency but omits job 318 entirely (no commit — `wrap_up:
type: none`). Job titles say "#311 slice A1", but deriving membership from title
prose is a parser over free text that gets its first wrong answer the moment
someone writes "unrelated to #311". `deps` are false ordering
([Decision 1](#decision-1-deps-cannot-carry-membership)). A group is a human
judgement, and a human judgement has to be recorded somewhere. One `Vec<String>`
on the record the dispatcher already owns is the smallest place to record it.

**Every aggregate is derived at read time, and no count is ever stored.** The
codebase's preference is explicit and this design does not deviate: spec §1.1
says `retry_count` and `rework_count` "are not stored on the job record — they
are derived from the task log"; §1.4 and §3.6 make `rdeps` "a
dispatcher-maintained cache" rebuilt from scratch on startup;
[#310](./310-scheduled-jobs.md) Decision 5 chose deriving a schedule's anchor
from job records over a new KV bucket.

Here it is not merely preferable, it is free. `Core::graphs` is a
`HashMap<String, JobGraph>` and `JobGraph.jobs` is a `HashMap<u64, Job>` — the
dispatcher already holds every job of every project in memory. A group roll-up
is one pass over one project's values, filtering on a `Vec<String>` contains.
No bucket, no reverse index, no startup rebuild, no drift, and nothing that can
disagree with the job records because there is nothing else to disagree.

This is also what keeps the single-writer invariant untouched: the design adds
one field to a record with one writer, and zero new state anywhere else.

## Decision 5: membership is mutable after release, including on terminal jobs

This is the decision the whole design turns on, and it is the one thing #185's
shape cannot do at all.

> **`PUT /api/v1/projects/{owner}/{project}/jobs/{seq}/groups`**, body
> `{ "add": [string], "remove": [string] }`, → 200 with the updated `Job`.
> Member+. Accepted in **every** state, including `Done` and `Revoked`. Emits
> `job-updated` with `{"fields": ["groups"]}` — the existing payload shape
> (`crates/dispatcher/src/core.rs`).

`groups` is also accepted on the create body and on the Draft `PATCH`, like
every other creation field, and shape-checked there (422).

Two objections, both real, both answerable.

**"Terminal jobs are immutable."** spec's Appendix: Deferred says exactly that —
"Terminal jobs are immutable. Any fix must be appended as a new job" — in the
context of dependency invalidation. What that rule protects is the **execution
record**: what ran, against which `base_ref`, with what result, judged by which
criteria. `Job.groups` is inert to all of it by
[Decision 3](#not-knowledge_tags)'s assert: no transition reads it, no container
sees it, no evaluator is selected by it, no prompt contains it. Annotating a
finished ticket with what it was part of does not change what it did, any more
than the operator writing it on a whiteboard does.

That has to be argued rather than assumed, so here is the alternative and why
it fails: grouping only at creation. Every one of #311's children is already
Done. A model that cannot express the group that motivated the design is not a
model. Retroactive grouping is not a nice-to-have here; it is the requirement.

**"That is a second writer."** It is not. The dispatcher remains the single
writer of `jobs.*`; this is another `req.jobs.*` request into the same
single-threaded actor. The precedent is in the tree and is exactly this shape:
`POST .../jobs/{seq}/claim` (spec §1.2) has an operator mutate `claim_next` on a
live, released job. STYLE.md Tier 3's invariant is one writer, not one write
path.

**No cascade, no cleanup, no referential integrity — deliberately.** Revoking a
member does not touch the group. Deleting `docs/design/311-job-inputs.md` does
not invalidate `design/311-job-inputs`. Renaming a doc leaves the old label
behind; a rename is two operator edits, or a follow-up
(see [Minimum useful version](#minimum-useful-version)). The alternative is
repo-to-record referential integrity maintained across a git history, which is
a large machine built to protect a string. This is the same posture spec §4.4
already takes for a knowledge tag with no file.

**Why not a whole-list `PUT`?** Because add/remove is idempotent and
concurrent-safe in the way a full replace is not: two operators grouping the
same job from two tabs both succeed, where a replace loses one. The Draft
`PATCH` is a full replace for a good reason (it is finalizing a definition); this
endpoint is not that.

## Decision 6: skew, and why no epoch moves

Confirmed against `crates/types/src/version.rs` and spec §14.1/§14.2 rather than
taken on trust. Current state: `CONFIG_SCHEMA_EPOCH = 2`,
`INPUTS_SCHEMA_EPOCH = 2`.

**No bump.** `CONFIG_SCHEMA_EPOCH` is documented in `version.rs` as "the
job-type YAML schema epoch the running dispatcher understands", and the gate it
drives is `JobType::min_dispatcher`. This design adds **no job-type field**.
§14.2's hazard — config merging ahead of the binary that parses it, quietly
disabling a feature — requires a config file to merge; a group has none. #311
needed the bump for the opposite reason: a tolerated `inputs:` block meant a job
*ran* unparameterized. Nothing here changes what runs.

**Surface 1 — the `Job` record.** `Job` carries no `deny_unknown_fields`
([correction 5](#corrections-verified-against-the-tree)) and
`groups: Vec<String>` with `#[serde(default)]` deserializes empty on every
existing record. Serialize it with `skip_serializing_if = "Vec::is_empty"` so an
ungrouped record is byte-identical to what it is today — the pattern
`Job.inputs` and `Job.cover_html` already use. The old-binary-reads-new-record
direction is bounded to a dispatcher restart: there is one dispatcher and it is
the sole writer of `jobs.*`.

**Surface 2 — the create body.** `CreateJobBody`
(`crates/dispatcher/src/handlers/jobs.rs`) has no `deny_unknown_fields`, and
`api` forwards the body opaquely as `serde_json::Value`
(`crates/api/src/routes.rs`), so an N−1 dispatcher silently drops a supplied
`groups`. Unlike #311's inputs this failure is benign — the job lands ungrouped
and one `PUT` fixes it later — so it needs no `min_dispatcher` gate and no
epoch. Naming the asymmetry is the point: inputs needed the gate because the
dropped field changed what ran.

**Surface 3 — the group endpoint.** An N−1 dispatcher 404s the new subject; the
API surfaces the error. Loud, no wrong action.

**Surface 4 — the web UI.** An N−1 build ignores an unknown field. No
coordination needed.

**The one thing that is not free.** `JobSummary` mirrors `Job`'s field set and
`job_summary_mirrors_job_fields` (`crates/types/src/job.rs`) **fails the build**
until a new `Job` field is either added to the projection or listed in
`JOB_SUMMARY_EXTRA_FIELDS`. This is working as designed and it forces the right
decision here: add `groups` to the projection. It is a bounded-small
`Vec<String>` (at most 8 × 128 bytes), unlike the prose fields the projection
drops, and the list is where filtering happens
([Decision 7](#decision-7-the-api-surface)). It then flows to
`web/src/api/types.gen.ts` with everything else.

**The contrast worth stating:** a group on the **job type** would be an entirely
different analysis — a new top-level field an N−1 dispatcher tolerates into
`JobType::unknown` with a `config-warning`, §14.2's exact case. It is not
proposed. A group is a per-instance judgement, and a job type has no instance.

## Decision 7: the API surface

Two reads answer both operator questions. One of them costs nothing.

**1. Filter the jobs list by group — already free.** With `groups` on
`JobSummary`, `GET .../jobs` carries it on every row and the operator table
filters client-side, exactly as it already searches id/title/type. No new
endpoint, no query parameter, no server work. A server-side `?group=` filter is
a payload optimization for a much larger project, not a capability, and it is
deferred.

**2. `GET /api/v1/projects/{owner}/{project}/groups`** — the enumeration and the
roll-up, derived on demand from `Core::graphs`, Viewer+ (`read_project`, like
every other project read):

```json
[
  {
    "name": "design/311-job-inputs",
    "doc_path": "docs/design/311-job-inputs.md",
    "doc_status": "PROPOSED",
    "jobs": [
      { "id": 314, "type": "code", "title": "…", "state": "Done" },
      { "id": 318, "type": "deploy", "title": "…", "state": "Done" }
    ],
    "counts": { "Done": 5, "Frozen": 1 },
    "open": 1
  }
]
```

Each rule below is a decision, not a serialization detail:

- **Names come from the jobs, never from a registry.** The set is
  `distinct(job.groups)` over the project. A group exists because a job says so.
  The consequence is that an empty group cannot exist — which is the one thing
  Option B in [Decision 2](#the-three-shapes-weighed) uniquely offered, given up
  on purpose: a group with no members is a plan, and a plan with no tickets is
  what the design doc is for.
- **`counts` is a per-state histogram, not a percentage.** "5 Done, 1 Frozen" is
  the operator's actual question; a percentage discards which one is not done.
  States with a zero count are omitted.
- **`open` counts members that are not terminal**, computed with
  `JobState::is_terminal` (`crates/types/src/job.rs`, `Done | Revoked`) so it
  cannot drift from the definition batches and #310 both already use.
- **`doc_path` / `doc_status` are best-effort and present only for
  `design/`-namespaced names**, resolved against `docs/design/{rest}.md` at
  default HEAD through the existing config/tree read path. Absent when the file
  is absent. See
  [Decision 8](#decision-8-status-hygiene-without-a-second-writer)
  for how `doc_status` is extracted and what it is allowed to mean.
- **`jobs` is the summary projection, not full records.** The group view renders
  a state badge and a title; the job page is one click away.

**Deliberately not proposed: a single "design and its jobs, with the doc body"
endpoint.** The doc is one `GET .../file?path=docs/design/311-job-inputs.md`
away and that endpoint exists today. Joining them would put a git blob read on
the critical path of the group query for content the UI needs only when the
operator opens a design.

**What the UI needs, and where this doc stops.** Everything above and nothing
else: a list of groups with roll-ups, each group's jobs with `state`, and each
job's `groups` on the jobs list for filtering and chips. The `web` work — a
Designs view, chips on the job row, a filter on the table, the status line beside
the roll-up — is follow-up jobs, and this doc specifies no component, route or
layout.

## Decision 8: status hygiene without a second writer

The principle from #185 survives verbatim and is worth restating because it is
the part most tempting to abandon:

> **The repo stays the source of truth for a design's status.** The platform
> does not write front-matter behind git's back.

That is not conservatism. A design doc's only writer is a reviewed commit; a
dispatcher writing into it is a second writer of a file, and it puts
platform-authored content into a diff no reviewer approved. Whatever this design
does about staleness, it does not do that.

The *mechanism* #185 chose — an impl ticket whose work is the front-matter
amendment — is superseded by something strictly cheaper. Once the roll-up is
derivable, so is the discrepancy: `design/311-job-inputs` has six members, six
terminal, and its doc's `Status:` line reads `PROPOSED`. **The platform reports
that; the operator resolves it with an ordinary `design` amendment job.** This
is better than #185's answer in one specific way: it requires nothing to have
been filed in advance in order to notice. #185's marker only exists if somebody
remembered to create the impl ticket before the work started, which for #311 —
the case that generated this whole design — nobody did.

**How the status is read, and how little that commits to.** The tree has no
front-matter ([correction 3](#corrections-verified-against-the-tree)) and the
vocabulary already has four values in use with no schema. So:

> `doc_status` is the remainder of the **first line of the document matching
> `^Status:`**, truncated to a short bound, surfaced **verbatim and unparsed**.
> The platform compares it to nothing and infers nothing from it.

Rendering "6/6 Done · Status: PROPOSED" side by side is the entire feature, and
it works today against every one of the eight docs in the tree without changing
one of them. A *machine-checked* `implemented` needs a status vocabulary, and
the vocabulary is not this doc's to define.

**Coherence with #86.** The front-matter schema is owned by job #86
(knowledge-architecture design). This doc references it rather than redefining
it, and adds exactly one requirement in return: **a design's identity must
remain addressable as a path.** `Job.groups` stores a path-derived name
precisely so a group stays resolvable on a Done job with no repo read
([Decision 2](#decision-2-a-group-is-a-namespaced-string-label-on-the-job-record)).
If #86 introduces an `id:` field that is not the file stem, the two designs
disagree: #86 wins on the file's contents, and this doc's naming convention must
then be re-derived as "the doc's path" rather than "the doc's id". Flagged so
the two are not authored in ignorance of each other. When #86 lands a
vocabulary, upgrading `doc_status` from verbatim text to a compared value is a
small, additive follow-up.

## Decision 9: what happens to #185, #86 and #88

### #185 — partially superseded, not vacated

| #185 point | Verdict |
| --- | --- |
| 1. Two job types both producing docs; a `design` job outputs `docs/design/…` | **Stands.** Unchanged by anything here, and the tree already works this way. |
| 2. The doc is the design's stable identity; the design job is ephemeral | **Stands, and is load-bearing.** `Job.groups` names a doc path, never a job seq — even though the path happens to embed one ([correction 4](#corrections-verified-against-the-tree)). |
| 3. Implementation tracking = an impl ticket with `deps = [all children]` | **Superseded.** Disqualified by the revoke cascade and by the permanence of a released job's `deps` ([Decision 1](#decision-1-deps-cannot-carry-membership)) — both read from the tree, neither considered in #185. |
| 4. Closing the impl ticket flips the doc's front-matter | **Mechanism superseded, principle retained** ([Decision 8](#decision-8-status-hygiene-without-a-second-writer)). The repo stays authoritative and a status change is still a reviewed `design` amendment; but the platform now *notices* without a ticket having been pre-filed. |
| 5. Scope growth = waves | **Superseded.** Waves exist only to work around immutable `deps`. A mutable label needs none, and #308's group — which contains an amendment to itself — has no wave-shaped reading at all. |
| 6. Batches are invisible here | **Stands, sharpened** ([Decision 3](#not-a-batch)). A batch job carries its own seq and can itself be grouped; the roll-up observes states and nothing else. |

**What changed my mind, stated plainly.** Points 1 and 2 are right and this
design is built on them. The deps-based mechanism reads well on paper and I
expected to keep it with a thin query surface bolted on — that was the
predicted outcome going in. Two things in the tree, and only those two, made it
untenable: `cascade_targets` revokes a Blocked impl ticket when any child is
revoked, and `state.rs` admits only `(Frozen, Draft)`, so a released ticket's
deps can never be extended. Neither is a friction to be priced; each on its own
destroys the marker the design depends on, and neither is fixable without
changing what `deps` mean for every other job in the graph.

Concretely: #185's job should be revoked or refiled as an amendment, and
`docs/design/epics.md` should not be written. Its two surviving points are
absorbed here rather than left in a job description, per its own point 2.

### #86 — untouched, with one coherence requirement

Stated in [Decision 8](#decision-8-status-hygiene-without-a-second-writer).
This doc defines no front-matter and reads no field #86 might define.

### #88 — composes, and is smaller than it looks

Job #88 (Frozen, `web`) tickets "render the `docs/` tree as a browsable wiki in
the web UI". Verified against the tree: **the browsing already exists.**
`GET .../tree` and `GET .../file` are shipped (spec §6.2) and
`web/src/pages/FileView.tsx` is a "GitHub-style directory listing and file
view", one recursive tree fetch with client-side navigation. What is missing is
*rendering*: `FileView` routes `.yaml`/`.yml` to `YamlView` and everything else
to a `<pre>`, while `web/src/components/Markdown.tsx` exists and is already used
by `JobDetail`, `Transcript` and `DraftEditor`.

So this doc **neither subsumes nor blocks #88**: they compose. The group view
links at `docs/design/…` paths; #88 decides what that page looks like when you
arrive. #88 should be re-scoped to what is actually left, which is closer to
"route markdown to the existing renderer" than to "build a wiki".

## Minimum useful version

Three slices. **Slice A + B is the recommendation for what ships first** — it is
the beacon question, end to end, with no UI work.

**Slice A — the field and the write path** ("which jobs are part of this"):

1. `crates/types/src/groups.rs` — the name charset, `GROUPS_COUNT_MAX = 8`,
   `GROUP_NAME_LEN_MAX = 128`, one pure validator returning a typed error,
   tier-1 tested. Mirrors `crates/types/src/inputs.rs` in shape so there is one
   idiom for "a bounded operator-supplied string set" and nothing for
   `.chug/tasks/check-duplication.sh` to find.
2. `Job.groups: Vec<String>` with `#[serde(default, skip_serializing_if)]`, plus
   the `JobSummary` field the mirror test forces
   ([Decision 6](#decision-6-skew-and-why-no-epoch-moves)).
3. `groups` on `CreateJobBody`/`CreateJobRequest` and the Draft `UpdateJobBody`;
   shape-checked at creation (422), consistent with how `inputs` splits creation
   shape checks from release-time semantics — except that groups have no
   release-time semantics at all, by design.
4. `req.jobs.groups.*` and `PUT .../jobs/{seq}/groups`: add/remove, accepted in
   every state, `job-updated` with `{"fields": ["groups"]}`.
5. The inertness assert: a tier-1 test that the job brief and the container env
   are byte-identical with and without groups.

**Slice B — the read** ("what's left"):

6. `req.groups.list.*` and `GET .../groups`, derived from `Core::graphs`, with
   `counts`/`open` computed via `JobState::is_terminal`, and best-effort
   `doc_path`/`doc_status` for `design/`-namespaced names resolved at default
   HEAD.
7. Tier-1 tests for the derivation: a group whose members are all terminal, one
   with a revoked member, one whose name has no doc, and a job in two groups.

**Slice C — web.** A Designs/Groups view listing groups with their roll-ups;
group chips on the job row; a group filter on the jobs table; the `Status:` line
rendered beside the roll-up; a picker over existing group names on the create
form and the job page. Follow-up `web` jobs.

Deferred, in rough priority order: server-side `?group=` filtering; a
machine-checked status vocabulary once #86 lands one; renaming a group as one
operation; descriptions for non-`design/` namespaces; cross-project groups.

## Contracts this changes

Per CLAUDE.md's contract-first rule for dispatcher work:

| Contract | Change |
| --- | --- |
| `Job.groups` | New field, `Vec<String>`, empty on old records, omitted from the wire when empty. Written by three paths, all on the single-writer dispatcher: create, the Draft edit, and `req.jobs.groups.*`. **Mutable in every state, including terminal** |
| Invariant | `Job.groups` is inert to execution: no container env, no prompt, no job-type resolution and no state transition reads it. Pinned by a byte-identity test on the job brief and the container env (STYLE.md Tier 2 #2, negative space) |
| Invariant | Group names match `^[a-z0-9][a-z0-9._/-]*$`, are unique within a job, and number at most `GROUPS_COUNT_MAX`. Checked by one pure validator in `types`, shared by all three write paths |
| Invariant | No group aggregate is ever stored. Every count, every member list and every enumeration is derived from `Core::graphs` at read time |
| Invariant | A group has no existence independent of its members: the group set is `distinct(job.groups)` over the project, so an empty group is unrepresentable |
| `JobSummary` | Gains `groups`, forced by `job_summary_mirrors_job_fields`; flows to `web/src/api/types.gen.ts` |
| `req.jobs.groups.*` | New subject; `PUT .../jobs/{seq}/groups`, Member+, add/remove, any state |
| `req.groups.list.*` | New subject; `GET .../groups`, Viewer+, read-only, derived |
| `job-updated` | New `fields` value `groups`; existing payload shape unchanged |
| Bound | `GROUPS_COUNT_MAX = 8` per job; `GROUP_NAME_LEN_MAX = 128` per name. Both hard errors, not truncation |
| Golden trace | `job-created` (no groups) → `PUT .../groups` on a **Done** job → `job-updated {fields:["groups"]}` → `GET .../groups` shows the job under its group with the correct `counts`; plus a job with no groups producing a container env and job brief byte-identical to today's |
| Epoch | **None.** `CONFIG_SCHEMA_EPOCH` is the job-type schema epoch and no job-type field changes (§14.1, §14.2) |

New modules get a doc header (accepts / emits / guarantees / spec §) and a
`MODULES.md` registry row, per the direction-of-travel rule;
`.chug/tasks/ci.sh` enforces the registry.

## What this doc does not decide

- **The front-matter schema.** Owned by #86; referenced, not redefined
  ([Decision 8](#decision-8-status-hygiene-without-a-second-writer)).
- **How the wiki renders.** Owned by #88, which is smaller than its title
  suggests ([Decision 9](#88--composes-and-is-smaller-than-it-looks)).
- **Whether `deps` should ever be editable after release.** Named because
  [Decision 1](#decision-1-deps-cannot-carry-membership) leans on the fact that
  they are not. Loosening it is a §2.1 change with its own cost, and it would
  not make `deps` the right carrier for membership anyway — the revoke cascade
  and the false-ordering argument survive it untouched.
- **A `Job.origin` consolidation of `factory` and `schedule`.** Still breaking
  under §14.1 for the reason #310 Decision 7 gives, and still a generalization
  over one writerless field. This doc adds to the record and reshapes nothing.
- **Group-level actions** — revoking, releasing or re-running a group as a unit.
  A group is a lens, not a handle; giving it a verb would recreate #89's
  rejected epic entity through the back door.
- **Cross-project groups.** Every subject and every graph in the tree is
  project-scoped; a group that spans projects needs a home that does not exist.
- **Whether `design`-namespaced groups should ever be auto-created** from a
  design job's closing summary. Attractive, and it needs the factory-style
  origination story rather than a special case here.
