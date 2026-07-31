# Design #310 — Scheduled jobs (time-triggered job creation)

Status: IMPLEMENTED — the [Minimum useful version](#minimum-useful-version)
shipped in jobs #359 (items 1, 2 and 6: the config format, the cron parser and
the `chuggernaut validate` gate) and #360 (items 3, 4 and 5: `Job.schedule`
provenance, `domain::decide::schedule`, `dispatcher::schedules`, the
`scan_schedules` tick and the `schedule-fired`/`schedule-skipped` events).
Everything this doc lists as deferred — `auto_release: false`, the platform
health surface `schedule-invalid` needs, a UI badge, `timezone:`, `inputs:` —
is still deferred.

Written against the tree at `55f6595`. Every claim about current behavior below
was read out of `spec.md` and the source in this repo;
where the brief and the tree disagree, the tree wins and the disagreement is
recorded in [Corrections](#corrections-verified-against-the-tree).

Doc 2 of 4 extracting implementable specs from
[design #308](./308-gha-port.md). §E of that doc is the rationale and the
motivating case (`flutter-integration-tests` runs nightly); it fixes the
location of the config and two semantics questions and leaves the rest here.

Related: [spec.md](../../spec.md) §1.1 (the `Job` record, job types, the config
root), §2.1 (state machine), §2.2 (release validation's three passes), §3.5
(the scan and the launch capacity queue), §6.3 (events), §13 (task factories
and ingest), §14 (config and version skew), Appendix: Deferred;
[design-lifecycle.md](../../design-lifecycle.md) (per-job additive evaluators,
the eval floor); [STYLE.md](../../STYLE.md) (Tier 2 #1 deciders, #3 bounds, #6
tests; Tier 3 single writer, simplicity);
[NORTH-STAR.md](../../NORTH-STAR.md) and [contracts.md](../../contracts.md)
(decider/effect factoring); [CLAUDE.md](../../CLAUDE.md) (per-consumer forge,
"the evaluation gates ARE the CI").

## Problem

Chuggernaut has no way to say "run this every night". Every job that exists was
created by an operator (`POST .../jobs`, `crates/dispatcher/src/handlers/jobs.rs`)
or is planned to be created by a factory triage agent that does not exist yet.
A recurring integration suite, a nightly dependency audit, a weekly cleanup —
none of them have a trigger.

The temptation is to treat this as trivial: `scan.rs` already ticks every 30
seconds, so parse a cron string and call `create_job`. The reason it is not
trivial is that the spec already contains a *different* answer to a
near-identical question — §13's task factories — and shipping a second,
subtly-different answer beside it is how a codebase ends up with two mechanisms
for one idea. So this doc's first job is to decide whether schedules *are*
factories, and everything else follows from that answer.

## Corrections (verified against the tree)

The brief is accurate in most of its detail. Six claims — five from the brief
and one from `spec.md` itself — need adjusting, and each changes something
downstream.

1. **"Reusing factory machinery may be the right answer."** There is no factory
   machinery to reuse. Verified: `crates/api/src/ingest.rs` is a two-line doc
   comment with no code; `crates/types/src/ingest.rs` defines `IngestEvent` and
   nothing else; there is no route, no `factories/` reader, no durable
   consumer, no `batch_window` handling (`batch_window` appears only in doc
   comments in `crates/types/src/job_type.rs` and `crates/types/src/duration.rs`).
   `store::buckets::INGEST_TOKENS` and `STREAM_INGEST` exist as names in
   `crates/store/src/buckets.rs`. So "share the factory mechanism" can only
   mean *share the design and write the shared part first* — schedules would be
   the first implementation of it, not a consumer of an existing one. This
   materially changes the sequencing argument in
   [Decision 1](#decision-1-schedules-are-the-other-trigger-source-not-a-second-mechanism).

2. **"Compare `Job.factory`."** The field exists (`crates/types/src/job.rs`,
   and `CreateJobRequest::factory` in `crates/dispatcher/src/core.rs`) but has
   **no writer**: every call site passes `None` except test fixtures. Nor does
   the `job-created` event carry it — `Core::create_job` publishes
   `serde_json::json!({})`, so §13.4's "carry `factory` on the record *and*
   `job-created` event" is unimplemented on both halves. The provenance
   *pattern* is therefore a precedent to follow, not a working surface to
   extend.

3. **#308's "What is true today" table lists "Ingest → triage → jobs (GitHub
   origin) | `crates/dispatcher/src/forge_ingest/` | Shipped".** That module's
   `triage` member is **operator-dispatched advisory triage over a stuck job**
   (spec §1.2): its own header states it "never drives a job transition" and it
   creates no jobs (`crates/dispatcher/src/forge_ingest/triage.rs`). It is not
   factory triage. Nothing in the tree turns an external event into a job.

4. **"`scan.rs` … currently timeouts/deadlines only, NOT triggering."** Correct
   that it originates no jobs, but it is not a read-only maintenance scan:
   `scan_job_deadlines` calls `Core::stall`/`Core::escalate`, which transition
   jobs and create Human tasks, and `run_scans` drives the launch-queue drain
   and republishes the config snapshot. Adding job creation there is a
   difference of degree, not of kind — which weakens (but does not settle) the
   "it wants its own loop" argument in
   [Decision 8](#decision-8-the-tick-rides-run_scans-the-decision-is-a-pure-decider).

5. **§13.5's `factory-invalid` event has nowhere to be published.** Spec §6.3
   defines *every* event subject as job-scoped
   (`job.events.{owner}.{project}.{seq}.{event_type}`), and
   `Core::publish` takes a `seq` — there is no non-job event path. Even
   `container-reaped` is attributed to its owning job
   (`crates/dispatcher/src/reconcile.rs`). A schedule whose *file* is invalid
   has no job to attach to, so it inherits the same hole; see
   [Decision 7](#decision-7-release-policy-provenance-and-events).

6. **Web Push fires on `task-created`, not on job events.** §11's delivery
   mechanism is explicit: "For each `task-created` event where `kind` is
   `Human`". There is no `job-escalated` or `job-created` subscription. §13.4's
   claim that "a Web Push notification (§11) fires on `job-created` events
   carrying `factory` provenance" therefore does not follow from §11 as written
   — a factory-created Frozen job notifies nobody until it produces a Human
   task. This is a spec inconsistency, not a design change, and it is noted
   here because [Decision 9](#decision-9-failure-escalation-and-repeated-failure-suppression)
   would otherwise inherit it: an escalating scheduled job *does* notify, but
   via the escalation task it creates, not via `job-escalated`.

## What is true today (verified)

| Thing | Where | State |
| --- | --- | --- |
| 30-second dispatcher tick inside the single-writer loop | `SCAN_INTERVAL` + `Msg::Scan` in `crates/dispatcher/src/core.rs`; `Core::run_scans` in `crates/dispatcher/src/scan.rs` | Shipped |
| Scan transitions jobs and creates Human tasks | `scan_job_deadlines` → `Core::stall`/`Core::escalate` | Shipped |
| Dispatcher-side job creation + release | `Core::create_job`, `Core::release_job` (`crates/dispatcher/src/core.rs`) | Shipped |
| Repo config read at a ref, `.chug/` then repo-root fallback | `crates/types/src/config_paths.rs`, `crates/dispatcher/src/project_config.rs` | Shipped |
| Flat config-directory listing (`jobs`, `tags`) | `project_config::entries` | Shipped |
| Default-branch HEAD resolution | `RepoManager::default_branch` + `resolve_ref` (`crates/vcs/src/lib.rs`) | Shipped |
| Duration-string parsing | `crates/types/src/duration.rs` (`parse_duration`) | Shipped |
| Offline YAML validation + merge-time skew gate | `crates/cli/src/validate.rs`, `.chug/tasks/ci.sh` | Shipped, job types only |
| Pure deciders returning effects | `crates/domain/src/decide/`, `crates/domain/src/effects.rs` | Shipped (`escalation.rs` is the template) |
| `Job.factory` provenance | `crates/types/src/job.rs` | Field exists, no writer |
| Factories, ingest endpoint, triage-created jobs | `spec.md` §13 | Specced, absent from the tree |
| Cron parsing, timezone database | — | No dependency in the workspace (`chrono` without `chrono-tz`) |

## Decision 1: schedules are the *other* trigger source, not a second mechanism

**The shared core is real, small, and nameable.** Three rules in §13 are not
about ingest at all — they are about *anything that creates a job without an
operator*:

1. **Trigger config is repo-versioned and HEAD-tracked** (§13.3): a flat
   directory of YAML under the config root, read from default-branch HEAD
   rather than a pinned `base_ref`, invalid entries skipped and reported,
   never blocking dispatch.
2. **Origination is dispatcher-side** (§13.4): the dispatcher creates the job
   with trigger provenance on the record and, under an `auto_release` policy,
   immediately runs release validation (§2.2) — a validation failure leaves the
   job Frozen and surfaces an event rather than escalating.
3. **Backpressure is at-most-one-in-flight** (§13.4): at most one non-terminal
   job per trigger instance; a trigger that fires faster than its jobs finish
   produces *no more jobs*, not a backlog.

All three apply verbatim to a clock. A schedule should implement exactly these
rules, with exactly this vocabulary, and §13's factories should adopt the same
implementation when they land.

**What a clock source contributes** is one thing and one thing only: deciding
*when* a job is due. Concretely, a cron matcher and an anchor derived from the
last job this schedule produced (see
[Decision 5](#decision-5-missed-ticks-coalesce-to-one-and-last-fired-is-derived)).
There is no consumer, no ack, no batch window, no payload.

**But a schedule is not a factory**, and modelling it as one is the alternative
this doc rejects. Three differences, in descending weight:

- **The agent in the loop is constitutive of a factory.** §13.1: "Factories
  never create jobs directly — an agent is always in the loop," and
  direct-payload-to-job templating is explicitly listed under Appendix:
  Deferred ("Direct-mode factories"). A schedule has no payload to judge. The
  decision "is it 02:00 on a weekday" is deterministic and belongs in a pure
  function; routing it through an agent adds tokens, latency and
  nondeterminism, which is precisely the argument #308 already took ("Triggers:
  cron only. Not ingest, not factories").
- **The correctness question is inverted.** A factory owes JetStream an ack
  contract — events are acked only when the triage job reaches a terminal state
  so a crash redelivers the batch (§13.4). Its failure mode is *losing* an
  event. A tick has no payload to lose; its failure mode is *firing twice*.
  Consumer sequence state and last-fired state are not the same state.
- **Fan-out differs.** A factory produces N jobs an agent chose; a schedule
  produces exactly one job the config named.

So the honest framing: **a schedule is the first *direct-mode* trigger** — the
thing the spec Appendix defers for factories — with a clock as its source. That
is one mechanism with a pluggable source, not two mechanisms, and not a factory
with a fake event stream.

### The rejected alternatives, honestly

**(A) A schedule is a factory whose `source` is a synthetic ingest subject.**
The dispatcher publishes an `IngestEvent` to
`ingest.{owner}.{project}.__schedule_{name}` on the tick; everything downstream
is §13 unchanged. This is genuinely attractive: one config format, one code
path, and the tick becomes ~20 lines.

It fails on three counts. It forces a triage agent into every nightly run — or
forces direct-mode factories, which is the same design work as this doc plus
the ingest plumbing. It replaces a one-timestamp durable state with a durable
consumer whose ack semantics are wrong for a clock (redelivering a "tick" event
after a crash re-fires an occurrence the dispatcher may already have honored).
And it blocks the whole feature on §13, which is unimplemented and unscheduled
— per correction 1, "reuse" here buys nothing because there is nothing built.

**(B) A wholly separate scheduler subsystem** — its own config vocabulary, its
own provenance field family, its own release policy, its own in-flight rule.
This is what "just add cron" turns into by default. Rejected under STYLE.md
Tier 3 (simplicity; a simpler shape would do) and because the three rules above
were already argued once in §13 — restating them with different words is how
`auto_release` and `auto-release`, `factory` and `trigger` end up meaning
almost-the-same thing.

**(C) Recommended: one origination core, two sources.** Schedules land the
core; §13's factory work adopts it. The cost is honest and worth stating: the
first implementer pays for generality it does not yet need (a provenance
*predicate* written over two fields when only one has a writer). That cost is
about twenty lines and it is the difference between "the second trigger source
is a config file" and "the second trigger source is a rewrite."

## Decision 2: config format and location

`.chug/schedules/{name}.yaml`, read from **default-branch HEAD**, resolved
through `types::config_paths` like every other config directory (so the
repo-root fallback works for pre-config-root projects, per §1.1). This is the
proposal in #308 §E, and it is right for the reason CLAUDE.md gives: a schedule
change ships in the same commit as the job type it fires, reviewed and gated by
the same CI.

```yaml
name: string            # required; unique within the repo; must equal the file stem
job_type: string        # required; the .chug/jobs/{job_type}.yaml this schedule creates
cron: string            # required; 5-field UTC cron expression (Decision 3)
enabled: bool           # optional; default true
title: string           # optional; the created job's title; default "{name}"
description: string     # required when the target type is work.type: agent (Decision 6);
                        #   optional otherwise. The §4.3 job brief the run receives.
auto_release: bool      # optional; default TRUE — diverges from §13.3 (Decision 7)
min_dispatcher: int     # optional; §14.2 skew gate, same meaning as on a job type
```

**Schema tolerance follows §14.1/§14.2 exactly.** A schedule file is config
read live from HEAD, so it carries the same hazard that burned every `web` job
on 2026-07-22: it can merge ahead of the binary that parses it. Therefore
unknown *top-level* fields are tolerated with a warning (a future field means a
feature is quietly off), and `min_dispatcher` gates a file that genuinely needs
a newer dispatcher. There are no nested blocks in v1; when one appears it keeps
`deny_unknown_fields`, per §14.2's gate-relevant rule.

**Validation, and where it runs.** Two layers, mirroring how job types are
already handled:

- **Merge time** — extend `chuggernaut validate` (`crates/cli/src/validate.rs`)
  to accept `.chug/schedules/*.yaml` and wire it into `.chug/tasks/ci.sh`
  beside the existing config-skew and `MODULES.md` gates. This is the layer
  that actually protects operators, because in this repo "enforced in CI" means
  "enforced by an evaluator" and a schedule file cannot reach HEAD without
  passing one.
- **Reload time** — parse failure, a `cron` that does not parse, a `job_type`
  naming a file that does not exist at HEAD, a `name` disagreeing with the file
  stem, or a missing `description` for an agent target: the file is **skipped**
  and the rest of the project's schedules load normally. §13.3's precedent
  holds here without modification — an invalid trigger file never blocks
  dispatch. Reporting is the part that does not carry over cleanly; see
  [Decision 7](#decision-7-release-policy-provenance-and-events).

**Bounds** (STYLE.md Tier 2 #3): a project loads at most `SCHEDULES_MAX`
schedule files (proposed 64). Entries beyond the cap are refused and logged
rather than silently truncated, and the per-tick work is therefore bounded by a
constant per project.

## Decision 3: the expression is 5-field cron, in UTC, full stop

**Cron over an interval vocabulary.** The obvious narrower option is
`every: 24h` using the existing `parse_duration` — no new grammar, no new
dependency, no timezone question. It is the wrong shape for the motivating
case. "Nightly at 02:00" is not an interval: an interval has no phase, so after
every dispatcher restart the run drifts to a new time of day, and the whole
point of a nightly suite is that it is not running when people are working. An
interval vocabulary also cannot express "weekdays only" or "the first of the
month" without growing into cron by accretion.

Cron's other advantage is the port itself: #308's category E is GitHub Actions
`schedule:` blocks, which are already cron strings. A ported workflow's
schedule is a copy, not a translation.

**The accepted grammar is a deliberate subset** — five fields
(`minute hour day-of-month month day-of-week`), each of `*`, `N`, `N-M`,
`*/S`, or a comma-list of those. No `@daily`/`@weekly` aliases, no `L`/`W`/`#`,
no seconds field, no year field. The classic ambiguity must be pinned in the
spec and in tests: **when both day-of-month and day-of-week are restricted
(neither is `*`), an occurrence matches if *either* matches** — the POSIX OR
rule. This is the single most common source of cron bugs and the most common
source of disagreement between cron libraries.

**Implementation: hand-rolled matcher in `types`, not a dependency.** The
matcher is a pure function over `(expression, DateTime<Utc>)`, it must live in
`types` (which stays sync and I/O-free, per CLAUDE.md), the accepted grammar is
a strict subset of what any crate offers — so a dependency's extra syntax would
have to be *rejected* anyway to keep configs portable — and it is exhaustively
testable at tier 1 of `testing.md`. Roughly a hundred lines of parse plus a
match predicate.

The honest counter-argument: hand-rolled date logic is exactly the category of
code that looks trivial and is not, and STYLE.md's "new dependencies state
their justification" is a rule about *adding* dependencies, not a rule against
them. If the grammar ever grows past the subset above, take a vetted crate then
— and check two things when doing so, because crates differ on both: the field
count (several accept a seconds field, shifting every position) and the DOM/DOW
rule.

**Timezone: UTC only, and the field does not exist in v1.** Not the
dispatcher's host timezone — that would make a schedule's meaning depend on
where the dispatcher happens to run, which contradicts repo-versioned config
being the source of truth; moving the deployment between hosts would silently
reschedule everything. Not an explicit `timezone:` field either, yet: it costs a
timezone database dependency (`chrono-tz`, not currently in the workspace) and
buys, for the motivating case, nothing. GitHub Actions runs `schedule:` in UTC
for the same reason, so the ported workflows need no adjustment.

**DST therefore does not arise in v1** — that is the point of choosing UTC, and
it is a better answer than picking a DST tie-break rule. For the record, when a
`timezone:` field does land, the rules should be:

- **Spring forward** (a nominal time that does not exist): the occurrence fires
  at the first instant after the gap. It is not skipped — a nightly job
  vanishing once a year is a worse surprise than one running an hour late once
  a year.
- **Fall back** (a nominal time that occurs twice): it should fire **once**, on
  the first occurrence — and this is the case that needs an explicit rule,
  which is worth stating plainly because it is the strongest argument for
  staying on UTC in v1. The anchor rule in
  [Decision 5](#decision-5-missed-ticks-coalesce-to-one-and-last-fired-is-derived)
  does **not** suppress the repeat by itself: fall back maps one nominal time
  to two absolute instants `T1` and `T2 = T1 + 1h`, and a job that fires at
  `T1` and completes at `T1 + 5min` sets the anchor to `T1 + 5min`, which `T2`
  is strictly after. The repeat fires. Suppressing it requires the state model
  to dedupe on the **nominal** occurrence — recording the last-fired local
  wall-clock occurrence beside the absolute anchor, so a nominal instant is
  consumed once however many absolute instants it maps to. That is a second
  piece of schedule state and a second comparison in the decider, and it buys
  nothing until `timezone:` exists. Whoever lands `timezone:` lands this with
  it; it is not free.

## Decision 4: overlap policy is **skip**, and it is not really a choice

When an occurrence comes due and a job from this schedule is still non-terminal:
**skip the occurrence, emit one event, and leave the running job alone.**

"Non-terminal" means any state other than Done or Revoked — deliberately
including **Escalated**, **Stalled** and **Frozen**. This is not a new
predicate: `JobState::is_terminal` is exactly `Done | Revoked`
(`crates/types/src/job.rs`). A nightly job that escalated blocks the next
night's run until an operator resolves it. That is the intended behavior and it
is also, for free, the repeated-failure suppression the brief asks for in
[Decision 9](#decision-9-failure-escalation-and-repeated-failure-suppression).

**A skipped occurrence is *consumed*, not deferred.** It never runs — not
later, not when the blocking job finishes. This has to be said here and honored
by the state model, or "skip" quietly degrades into "queue with depth one": an
occurrence skipped on Tuesday would come back and fire at whatever odd hour the
blocking job happened to terminate. [Decision
5](#decision-5-missed-ticks-coalesce-to-one-and-last-fired-is-derived) makes it
true by construction by anchoring on the blocking job's *completion*.

The alternatives:

- **Queue.** A backlog of stale runs. Seven queued nightly integration runs
  tell an operator nothing the one latest run does not, and an unbounded queue
  violates STYLE.md Tier 2 #3 outright. Bound it and you have chosen a depth;
  the only defensible depth is 1, which *is* skip.
- **Allow concurrent.** Mechanically fine — two jobs, two branches, two merges
  — but it removes the only backpressure and is actively wrong for the
  motivating case, where the nightly suite contends for a device
  (#308 H.5's exclusive resources). A schedule that fires faster than its job
  completes would multiply until the fleet saturated, and §3.5's launch queue
  would absorb the damage as a capacity stall rather than as the misconfiguration
  it is.
- **Cancel previous** (GitHub Actions' `concurrency: cancel-in-progress`). This
  means auto-revoking a live job on a timer. Revoke is terminal and
  operator-facing (§2.1: any non-terminal → Revoked, killing tasks and closing
  Human tasks); silently revoking work an operator may be mid-diagnosis on,
  because a clock ticked, is a surprising loss. Rejected.

Worth naming plainly: GitHub Actions expresses all of this as a `concurrency`
group, and **Chuggernaut has no user-facing concurrency primitive**. Skip is
therefore not merely the easiest option — it is the only one expressible
without inventing one. If a real need for cancel-previous or a shared
concurrency group appears, that primitive deserves its own design; it should
not arrive as a schedule field.

## Decision 5: missed ticks coalesce to one, and last-fired is *derived*

**Semantics: fire late, once.** If occurrences pass while the dispatcher is
down, the first tick after recovery fires **exactly one** job, then arms for
the next future occurrence. Six hours of downtime on an hourly schedule
produces one job, not six and not zero. This is #308 §E's answer and it is
right: the value of a recurring job is almost always in its most recent run.

Firing zero (skip everything missed) is the plausible alternative and it is
wrong for the common case — a nightly suite that silently does not run after a
deploy is exactly the failure this feature exists to prevent, and deploys are
frequent here.

**The durable state, and where it lives.** Two options:

- **(a) A new KV bucket.** `schedules.{owner}.{project}.{name}` →
  `{last_fired_at, last_job_seq}`, alongside the buckets in
  `crates/store/src/buckets.rs`.
- **(b) Derive it from the job records — recommended.** The dispatcher already
  holds every job in memory (`Core::graphs`), so the read is free and involves
  no new writer, no new bucket, and no new record that can drift from reality.

(b) is the shape this codebase already prefers: §1.1 states `retry_count` and
`rework_count` "are not stored on the job record — they are derived from the
task log", and the `rdeps` index is explicitly "a derived cache" rebuilt on
startup (§3.6 step 1). It also keeps the single-writer story trivial, because
there is no second piece of trigger state to keep consistent with the job
records.

**The anchor rule, stated precisely.** Everything in this decision reduces to
one value per schedule — the **anchor**, the instant an occurrence must be
strictly after in order to fire. Let `latest` be the most recent job carrying
this schedule's provenance, by `created_at`:

| `latest` | Anchor | Behavior |
| --- | --- | --- |
| none (never fired) | `first_seen_at` (in-memory, set when the dispatcher first loads the file) | No backfill |
| non-terminal | n/a — no fire is possible | Blocked; the schedule does not fire at all ([Decision 4](#decision-4-overlap-policy-is-skip-and-it-is-not-really-a-choice)) |
| terminal (Done or Revoked) | `latest.completed_at` | Catch-up across restarts; skipped occurrences consumed |

The decider then fires **exactly one** job if any occurrence falls in
`(anchor, now]`, and none otherwise. The blocked row has no anchor **because no
occurrence can fire there** — the anchor's only job is to bound the fire
interval, and that branch never fires. It is deliberately not "the anchor the
prior job would have had": the *reporting* rule in
[Decision 7](#decision-7-release-policy-provenance-and-events) needs a lower
bound while blocked, and that bound is `latest.created_at` — a different value
from the anchor, by design, for a different purpose. Note also that the anchor
is *not*
`max(last_fired_at, first_seen_at)`: `first_seen_at` resets to "now" on every
restart, and downtime *is* a restart, so `max`-ing the two would make
`first_seen_at` dominate after every outage and destroy catch-up entirely. The
two cases are disjoint on purpose.

`completed_at` is the right durable field for the fired case: it is stamped on
the job record at the terminal transition through the single write funnel
(`Core::set_state` in `crates/dispatcher/src/core.rs`, which stamps
`job.completed_at.get_or_insert_with(Utc::now)` on every terminal transition),
it survives restart in KV, and `JobState::is_terminal` is `Done | Revoked` —
matching Decision 4's in-flight definition exactly, so the two rules cannot
drift apart. Records predating the field deserialize `completed_at` to `None`
(it is `Option<DateTime<Utc>>` in `crates/types/src/job.rs`); the
fallback is `latest.created_at`, which is always present.

Three traces pin the properties:

- **Catch-up.** Hourly schedule; last job created 02:00, completed 02:20;
  dispatcher down 02:30–08:30. On recovery the anchor is 02:20 — a KV value,
  untouched by the restart. Occurrences 03:00…08:00 all fall in
  `(02:20, 08:30]`, so **one** job fires at 08:30 and the schedule arms for
  09:00. One, not six, not zero.
- **No backfill.** A new `0 * * * *` merged at 08:30 has no prior job, so the
  anchor is `first_seen_at` = 08:30 and the first fire is 09:00. There is no
  epoch anywhere in the computation.
- **Skips are consumed, not deferred.** Nightly at 02:00; Monday's job fires
  and escalates; Tue and Wed 02:00 are skipped; an operator revokes the job
  Wednesday 10:00. The anchor becomes the revoke's `completed_at` = 10:00, so
  Tue and Wed 02:00 are behind it and gone. The next fire is Thursday 02:00 —
  **not** an off-schedule run minutes after the operator cleared the
  escalation. This is the second thing the derived state must express, and it
  is why the anchor is the blocking job's *completion*, not its creation.

The visible consequence of consuming skips, stated rather than buried: a
schedule whose job outlives its period runs slower than its cron says. Hourly
with a 90-minute job fires roughly every two hours, because the next occurrence
must be strictly after the last completion. That is the correct reading of
"skip", and the alternative (deferring skipped occurrences) is what produces
the 10:00 surprise above.

Its honest weaknesses, both bounded:

- Derived state cannot distinguish "never fired" from "fired and the job
  creation failed", because a failed creation leaves no job. A creation failure
  is loud (logged, and the tick is retried at the next occurrence), and a
  *persistently* failing creation is a broken project, not a schedule bug.
- Because `first_seen_at` is in-memory, a schedule that has **never** fired can
  be starved by restarts more frequent than its own period — a dispatcher
  redeployed every 45 minutes could keep re-arming an hourly schedule that has
  no prior job. It self-heals the moment one occurrence is observed (the anchor
  moves to KV-backed state), and it cannot affect a schedule that has ever run.

What would force (a) is a requirement to record fire attempts that produced no
job, or sub-tick precision. Neither exists.

**The tick granularity bound.** `SCAN_INTERVAL` is 30 seconds and cron's finest
granularity is one minute, so every occurrence is observed at most 30 seconds
late. Worth noting because it is a genuine advantage of the anchor rule over
the point-match implementation one reaches for first: because the decider asks
"does *any* occurrence fall in `(anchor, now]`" rather than "does `now` match
the expression", a slow tick coalesces rather than drops, and the design has no
hidden dependency on `SCAN_INTERVAL ≤ 60s`. The observation-latency bound is
still worth a comment at the constant — a schedule can never fire *early*, and
is late by at most one scan interval — but correctness does not rest on it.

**Recovery thundering herd.** After a long outage every schedule in every
project fires on the same tick. The bound already exists: jobs are created and
released, then their containers queue on the §3.5 launch capacity queue, which
is FIFO, priority-ordered, and bounded by a maximum queue wait. The per-tick
creation work is bounded by `SCHEDULES_MAX` per project. No additional
throttle is needed, and adding one would duplicate the queue.

## Decision 6: any job type, with the description question answered

**Any type. The gate is the repo, not a platform allow-list.** A schedule file
reaches HEAD only by merging through the project's own job pipeline — reviewer
plus CI. Adding a nightly `deploy` therefore already requires a reviewed,
merged commit, which is the same gate that governs adding a `deploy` job type
at all. A platform-level denylist would be the platform second-guessing a
per-consumer forge, and CLAUDE.md is explicit that config travels with the
project repo.

The honest caveat, stated rather than legislated: a scheduled command job with
`wrap_up: none` and an external effect — `.chug/jobs/deploy.yaml` is the
archetype — is the genuinely dangerous shape, because nothing about it is
undone by revoking the job. The **convention** (not a platform rule) should be
that such schedules set `auto_release: false`, so the timer prepares the run
and a human taps release. A `human` work type is also fine and needs no special
case: a scheduled `human` job parks a recurring Human task, which is a
legitimate use (a weekly checklist).

**Where the description comes from.** `Job.description` is "the ticket body;
injected into work and eval prompts as the §4.3 job brief" (§1.1), so a
scheduled agent job needs one. It comes from the schedule file: `description`
is **required when the target type declares `work.type: agent`** and optional
otherwise (a `command` job's ticket is not injected into anything that reads
it). A long ticket is a YAML block scalar. Rejected: a `description_file:`
pointing at a repo path — two ways to say one thing, for no gain over a block
scalar, and one more path to resolve at a second ref.

Every occurrence gets the *same* description, which is correct for the
motivating case ("run the nightly integration suite") and is the boundary at
which [Decision 10](#decision-10-the-seam-for-job-inputs-doc-3) takes over.

**A schedule naming a job type that does not exist** is invalid at load: the
loader resolves `.chug/jobs/{job_type}.yaml` at the same HEAD and skips the
schedule if it is absent or unparseable, exactly like §13.3's `triage`
existence check.

**A job type deleted out from under a live schedule** needs no new machinery,
and inventing some would be the mistake. Three existing behaviors cover it:

- At the **next reload** the schedule becomes invalid and stops firing
  (skipped, reported).
- A job **already created** but not yet released fails **release-time**
  validation (§2.2 pass 1) — under `auto_release` that leaves it Frozen with a
  `schedule-release-failed` event, per
  [Decision 7](#decision-7-release-policy-provenance-and-events).
- A job **already released** carries its own pinned `base_ref` and is
  unaffected by HEAD; if the file it needs is missing at that ref it takes the
  existing **Ready-transition** path (§2.2 pass 2 → `Blocked→Stalled`), which
  is a pre-work human intervention, not a new failure mode.

## Decision 7: release policy, provenance, and events

**`auto_release` defaults to `true`** — the one place this design deliberately
diverges from §13.3, where the factory default is `false`.

The argument is not "convenience". Under
[Decision 4](#decision-4-overlap-policy-is-skip-and-it-is-not-really-a-choice),
Frozen is non-terminal and therefore counts as in-flight. A schedule defaulting
to `auto_release: false` would create one Frozen job and then **never fire
again** until an operator released it — the default would be a deadlock. The
asymmetry with factories is principled: a factory's `false` default gates an
*agent's judgment* about what work should exist, whereas a schedule creates the
job the repo already committed to, at the time the repo already committed to.
There is no judgment to gate.

`auto_release: false` remains available and its semantics are exactly the
above, stated as a feature: the schedule fires once, parks a Frozen job for
approval, and does not fire again until that job reaches a terminal state.
That is the right behavior for the scheduled-deploy convention in
[Decision 6](#decision-6-any-job-type-with-the-description-question-answered).

**Release-validation failure mirrors §13.4 exactly**: the job stays Frozen and
an event surfaces the errors; it does **not** escalate. And because Frozen is
in-flight, the schedule stops — one stuck job and one event, not one per night.

**Provenance: a new `Job.schedule: Option<String>`** beside `Job.factory`,
defaulting `None` on old records (the established pattern for every recent
field in §1.1). Set only by the origination path, immutable after creation.

The alternative — replacing both with a single
`Job.origin: Option<{kind, name}>` — is the better long-term shape and is
rejected for now on two grounds. It is a **breaking** wire change, not an
additive one: `factory` is serialized into the API wire type consumed by the
web client (`web/src/api/types.gen.ts`), so removing it violates §14.1's
additive-only rule and needs an epoch bump plus a coordinated deploy. And it
would be a generalization over one real writer. The unifying work is done
instead by writing the in-flight **predicate** once, over both fields — about
twenty lines, and the seam at which a future `origin` consolidation happens.

**Events.** Mirroring §13.5's naming, with the subject each is published on
made explicit — because per correction 5 the event stream is job-scoped and
this is where §13.5 is underspecified:

| Event type | Published on | Trigger |
| --- | --- | --- |
| `schedule-fired` | the **created** job | An occurrence fired; includes `schedule`, `occurrence_at` |
| `schedule-skipped` | the **blocking** job | An occurrence came due while a prior run was non-terminal; includes `schedule`, `occurrence_at`. **At most one per occurrence**, not one per scan tick |
| `schedule-release-failed` | the Frozen job | `auto_release` validation failed; includes `errors` |

The `at most one per occurrence` bound matters: a naive implementation emits on
every 30-second tick while a run is blocked, which is 2,880 events a day for
one stuck nightly job.

**The bound needs state, and the anchor cannot supply it.** A skip creates no
job and — by [Decision 5](#decision-5-missed-ticks-coalesce-to-one-and-last-fired-is-derived) —
does not move the anchor either; the anchor only advances when the blocking job
*completes*. So every tick while blocked sees byte-identical inputs and would
re-derive the same verdict. The dedupe key is therefore a **separate**
in-memory field, `last_skipped_occurrence: Option<DateTime<Utc>>`, held beside
`first_seen_at` in the dispatcher's schedule table: while blocked, the decider
computes the newest occurrence in **`(latest.created_at, now]`** and emits
`schedule-skipped` only when it differs from `last_skipped_occurrence`.

The lower bound is `latest.created_at` — *strictly after the fire that is
currently blocking* — and not the anchor, which is undefined in this branch
(see the note under [Decision 5](#decision-5-missed-ticks-coalesce-to-one-and-last-fired-is-derived)'s
table). `ScheduleView.latest` already carries `created_at`, so this needs no new
input. Getting it wrong here is not a corner case: with any weaker bound, the
occurrence that *just fired* is still inside the interval, so a nightly
`0 2 * * *` firing at 02:00:15 would emit `schedule-skipped {occurrence_at:
02:00}` on the very next tick at 02:00:45 — a spurious skip trailing every
single real fire, on the job that occurrence created. With `latest.created_at`
the first post-fire tick finds no occurrence in the interval and emits nothing;
the first genuine skip is the next night's 02:00. Coalesced occurrences behave
correctly for the same reason: they precede the catch-up job's `created_at`, so
they are consumed silently rather than reported as skips.

Consequence to state
rather than discover: a restart clears it, so a dispatcher restart can re-emit
one event for the currently-blocked occurrence — at most one per occurrence per
dispatcher lifetime, which is within the bound's intent and nowhere near the
2,880.

`job-created` should also carry `{"schedule": name}` in its payload — noting
per correction 2 that it currently carries an empty object, so §13.4's
equivalent claim about `factory` is a thing to *implement here*, not to extend.

**`schedule-invalid` has no home, and this doc does not invent one.** There is
no non-job event subject and no platform-level config-warning surface; §14.2
already names that surface as "a follow-up, not yet wired". For v1 an invalid
schedule file is a `tracing::warn!` only. This is a real operability gap and it
should be stated rather than papered over — the mitigation is that
`chuggernaut validate` in CI stops almost every invalid file *before* it
merges, so the runtime path is a fallback, not the primary defense. Wiring
`schedule-invalid`, `factory-invalid` and §14.2's config warnings into one
platform health surface is the correct fix and is a separate piece of work.

**UI and history.** `Job.schedule` flows through the API wire type into
`web/src/api/types.gen.ts` and the jobs list can badge and filter on it — but
note honestly that **nothing in `web/` renders `factory` today** (it appears
only in generated types and test fixtures), so "distinguishable in the UI" is
new `web` work, not a free consequence of adding the field. In history it is
free: the field is on the immutable record and the events name the schedule.

## Decision 8: the tick rides `run_scans`; the decision is a pure decider

**Where the tick lives:** one more call in `Core::run_scans`
(`crates/dispatcher/src/scan.rs`), `scan_schedules`, after the existing scans.
Per correction 4 this is a smaller step than the brief assumes — `run_scans`
already transitions jobs and creates tasks — and it inherits the single-writer
property for free, which is the entire reason a restart cannot double-fire.

**Rejected: its own loop.** A separate timer would still have to send a message
into the actor, because the dispatcher is the single writer of job records and
that must not change (STYLE.md Tier 3; CLAUDE.md). So a second loop buys only
phase independence from the other scans, and costs a second timer, a second
message variant, and a new ordering question with `Drain` (§3.6: while draining
the core initiates no new work — a schedule tick must respect that, and riding
`run_scans` means it does so by construction). The 30-second tick is already
comfortably finer than the one-minute cron floor.

**But the decision is not `scan.rs`'s to make.** Per STYLE.md Tier 2 #1 and
NORTH-STAR's direction of travel, new decision logic goes into a pure decider,
not a new `impl Core` method:

```text
domain::decide::schedule::decide(
    schedules: &[ScheduleConfig],   // loaded config, already validated
    views: &[ScheduleView],         // one per schedule; see below
    now: DateTime<Utc>,
) -> (Vec<ScheduleFire>, Vec<Effect>)

ScheduleView {
    latest: Option<(JobState, DateTime<Utc>, DateTime<Utc>)>,
        // state; created_at (bounds the skip-report interval while blocked);
        // completed_at-or-created_at (the anchor once terminal)
    first_seen_at: DateTime<Utc>,          // in-memory; used only when `latest` is None
    last_skipped_occurrence: Option<DateTime<Utc>>, // in-memory; the schedule-skipped dedupe key
}
```

`latest` is the most recent job carrying this schedule's provenance, projected
down to the three values the anchor rule needs — not the whole `Job`, so the
decider cannot reach for anything else. `first_seen_at` and
`last_skipped_occurrence` are the only two pieces of in-memory schedule state
in the design; both are safe to lose on restart, per Decision 5 and Decision 7.

Pure, zero `.await`, exhaustively testable at tier 1 — the cron matcher, the
coalescing rule, the skip rule and the enabled flag are all decided here.
`scan.rs` gathers the read inputs and performs the origination.

One wrinkle worth naming so the implementer does not fight it: **job creation
cannot be an `Effect` today.** `crates/domain/src/effects.rs` has `PutJob` but
no `CreateJob`, and allocating a job seq is I/O (`CounterStore::next`), so the
decider cannot mint one. The established pattern is to gather such a value as a
*read input* (the decider template in `crates/domain/src/lib.rs` does exactly
this with `next_task_id`), but pre-allocating a seq on every tick would burn
ids for the overwhelmingly common "nothing is due" case. So the decider returns
`Vec<ScheduleFire>` — a decision, not an effect — and the shell calls the
existing `Core::create_job` / `Core::release_job` for each. Promoting creation
to a real `Effect` is a larger change than schedules need; it is a seam, not
this doc's work.

**Config loading does no git I/O on the tick.** Reading `.chug/schedules/` at
HEAD means a repo tree read plus a file read per schedule — subprocess work
that must not run on the single-writer loop every 30 seconds. The schedule
table is instead held in memory and refreshed:

- at **startup**, and
- after every **squash-merge to the default branch** (the dispatcher performs
  the merge, so it knows), exactly as §13.3 specifies for factories, plus
- a **bounded periodic refresh** (proposed: every 20th tick, ~10 minutes) as a
  backstop, because a project's default branch can move without a Chuggernaut
  merge — `RepoManager::advance_default` and origin sync are both such paths.

The refresh itself is off the critical path in the sense that a stale table
delays a schedule change by at most one refresh interval; it never misfires.

## Decision 9: failure, escalation, and repeated-failure suppression

**#308 §E's observation holds, and it checks out against the tree.** A failing
scheduled job is just a job that escalates: §2.1's `Work→Escalated` and
`Evaluation→Escalated` rows create a Human escalation task and publish
`job-escalated`, and `crates/types/src/job.rs`'s `Escalation` struct records
the reason, detail, failing task and timestamp on the record. That task lands
in the operator inbox with no schedule-specific machinery. The GitHub-issue
-filing hop that a GHA nightly needs does disappear — Chuggernaut's escalation
*is* the issue, and it already reaches the operator's phone via the escalation
task's `task-created` Web Push (§11 fires on `task-created` where `kind` is
`Human`; see correction 6).

**Repeated-failure suppression is already implied and needs no new mechanism.**
Because Escalated is non-terminal, it counts as in-flight, so the next
occurrence is skipped ([Decision 4](#decision-4-overlap-policy-is-skip-and-it-is-not-really-a-choice)).
A schedule failing nightly for a week produces **one** escalation and six
`schedule-skipped` events, not seven identical escalations. This falls out of
the backpressure rule rather than being bolted onto it, which is a good sign
the rule is the right one.

**The honest cost of that, stated plainly:** an unattended escalation silently
disables the schedule. There is no timer that re-arms it; a human must resolve
or revoke the job. That is the correct default for an orchestrator whose whole
premise is that a human owns the escalation — but it means "my nightly stopped
running" and "my nightly is failing" are the same operator-visible condition,
distinguishable only by looking. The mitigations already exist (the Human
escalation task's `task-created` Web Push; the `schedule-skipped` event naming
the blocking job) and no new one is proposed. If a project genuinely wants a schedule to keep firing through
failures, that is an argument for a `on_previous_failure: fire | skip` field —
deliberately not in v1, because it should be justified by a real operator
complaint rather than by symmetry.

## Decision 10: the seam for job inputs (doc 3)

A schedule plausibly wants to parameterize the job it creates — a `target`, an
environment, the occurrence timestamp. **This doc designs none of that**, and
the dependency is one-directional: schedules do not block on inputs, but the
`inputs:` field on a schedule file should be added by doc 3's design, passed
through the same origination path, not invented here.

The one thing this design must **not** do, and reviewers should reject if it
appears: templating in `description` (`{{ occurrence_at }}` and friends). A
template language in a ticket body is parameterization with none of the typing,
declaration or gate-safety that `design-lifecycle.md`'s constraints on per-job
overrides demand — and it would be the hardest thing to remove once configs
depend on it. Likewise, resist adding an ad-hoc `CHUG_OCCURRENCE_AT` env var:
that is an input, and it should arrive as one.

## Minimum useful version

Everything above except the parts explicitly deferred. Concretely, what could
ship first and be genuinely useful for `flutter-integration-tests`:

1. `.chug/schedules/{name}.yaml` with `name`, `job_type`, `cron`, `enabled`,
   `title`, `description`. No `auto_release` field — hardcoded `true`. No
   `timezone`. No `inputs`.
2. A pure 5-field UTC cron parser and matcher in `types`, tier-1 tested,
   including the DOM/DOW OR rule.
3. `Job.schedule` provenance, plus `schedule` in the `job-created` payload.
4. `domain::decide::schedule` — the anchor rule, coalescing, and the skip rule
   as a pure decider, with the three Decision 5 traces as tier-1 tests;
   `scan_schedules` in `crates/dispatcher/src/scan.rs` as the shell; the
   schedule table refreshed at startup, after squash-merge, and on the periodic
   backstop.
5. `schedule-fired` and `schedule-skipped` events; invalid files logged.
6. `chuggernaut validate` accepts `.chug/schedules/*.yaml`; `.chug/tasks/ci.sh`
   gates them.

Deferred to follow-ups, in rough priority order: `auto_release: false` (needed
before anyone schedules a `deploy`); the platform health surface that gives
`schedule-invalid` and `factory-invalid` somewhere to go; a UI badge and filter
on trigger provenance; `timezone:`; `inputs:` (doc 3).

## Contracts this changes

Per CLAUDE.md's contract-first rule for dispatcher work:

| Contract | Change |
| --- | --- |
| `Job.schedule` | New optional field; `None` on old records; written only by the origination path; immutable after create |
| Invariant | At most one non-terminal job per `(project, schedule)`. Asserted before the create and re-asserted on read-back (STYLE.md Tier 2 #2's pair-across-NATS pattern) |
| Invariant | A schedule never fires for an occurrence at or before its **anchor** — `latest.completed_at` when a prior job exists, `first_seen_at` when none does (never the max of the two). This one property gives no-backfill, catch-up across restarts, and skipped-occurrences-consumed. It does *not* give DST fall-back safety — see [Decision 3](#decision-3-the-expression-is-5-field-cron-in-utc-full-stop) |
| Invariant | `schedule-skipped` is emitted at most once per occurrence per dispatcher lifetime, and never for an occurrence at or before the blocking job's `created_at` |
| Invariant | The anchor is monotonically non-decreasing for a given schedule, and every fire strictly advances it |
| `decide::schedule::decide` | New pure decider; zero `.await`; returns fire decisions and events, never performs an effect |
| `Core::run_scans` postcondition | Adds: after the turn, every enabled, valid, due, unblocked schedule has created exactly one job |
| Bound | `SCHEDULES_MAX` schedule files per project; a schedule is observed at most one `SCAN_INTERVAL` after its occurrence and never before it |
| Golden trace | New: `schedule-fired` → `job-created` → `job-released` → `job-started` for one occurrence, and a second occurrence producing `schedule-skipped` instead |

New modules get a doc header (accepts / emits / guarantees / spec §) and a
`MODULES.md` registry row, per the direction-of-travel rule; `.chug/tasks/ci.sh`
enforces the registry.

## What this doc does not decide

- **Job inputs / parameterization** — doc 3, and the reason this doc stops at a
  single static `description`.
- **A concurrency primitive.** Skip is the only overlap policy expressible
  today; cancel-previous and shared concurrency groups need a primitive that
  does not exist, and it is not a schedule field.
- **The platform health surface** that `schedule-invalid`, `factory-invalid`
  and §14.2's config warnings all need. Named as a gap, scoped elsewhere.
- **Whether factories ship at all.** This doc asserts that when they do, they
  should adopt the origination core schedules land — not that §13 is next.
- **Anything in #308's other categories.** Host-native execution, image builds
  and the OIDC issuer are separate.
