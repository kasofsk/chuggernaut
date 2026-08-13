# The lifecycle model — the concrete machine

**Audience: someone reimplementing this platform in another language.** It
assumes you will not read `crates/`, and it names what you have to get right:
the state set, the event alphabet, where each transition's guard actually
lives, the effect vocabulary, the invariants, the authority split, and the
ports you must stub. Everything here is measured against the tree rather than
recalled from the design corpus.

**Boundary against [`docs/reference/design-lifecycle.md`](design-lifecycle.md).**
That page owns the *generalized* phase vocabulary — what work, evaluation,
wrap-up and triage mean as a pattern, independent of what a job produces, and
why the feedback/failure distinction is declared rather than inferred. This page
owns the *concrete machine*: states, events, transitions, effects, invariants,
authority, ports. When the two disagree about a phase's meaning, that page
wins; when they disagree about what the code does, this one does.

Two other pages carry the rest of the frame, and neither is restated here:
[`docs/spec.md`](../spec.md) is normative (§2.1 is the transition table, §3 the
dispatcher), and [`docs/reference/contracts.md`](contracts.md) argues *why* the
interfaces are shaped this way. [`docs/concepts.md`](../concepts.md) routes
every term this page merely uses.

---

## 1. States

[`docs/spec.md` §2.1](../spec.md#21-state-machine) carries a prose description
of each job state; read it there. What a modeller needs and it does not state
uniformly is the four mechanical properties below — terminality, visibility to
scheduling, whether a git branch exists, and whether a human may claim the next
attempt.

| State | Terminal | Visible to scheduling | Git branch exists | Claimable |
| --- | --- | --- | --- | --- |
| `Draft` | no | no | no | **no** — rejected, "release it before claiming" |
| `Frozen` | no | no | no | yes |
| `Batched` | no | no | no | **no** — rejected, "claim its batch, not the member" |
| `Blocked` | no | as a dependent only | no | yes |
| `Ready` | no | yes — it holds a queue entry | no | yes |
| `Work` | no | yes | yes | only if no attempt is in flight |
| `Evaluation` | no | yes | yes | yes |
| `WrapUp` | no | yes | yes | yes |
| `Escalated` | no | yes | yes | yes |
| `Stalled` | no | no — it never started | no | yes |
| `Done` | **yes** | no | deleted | no |
| `Revoked` | **yes** | no | deleted if it existed | no |

Reading the columns:

- **Terminality** is `JobState::is_terminal` in `crates/types/src/job.rs`, and
  it is exactly `Done | Revoked`. It is load-bearing in three separate places —
  the transition table's absorbing clause, the terminal-stamping of
  `completed_at`, and the schedule backpressure rule — so a reimplementation
  should make it one predicate rather than three comparisons.
- **Branch existence** is a fact about the repository, not about the record. The
  `branch` field is a string set at creation (`job/{id}`) in every state; the
  *ref* is created when the job enters Work and deleted when it reaches `Done`
  or `Revoked`. A record that names a branch is therefore not evidence the
  branch exists.
- **Claimability** is not a function of state alone. `Core::claim_job`
  (`crates/dispatcher/src/core.rs`) refuses a terminal job, a `Draft` and a
  `Batched` member outright, and otherwise refuses when a Work-phase
  non-evaluator task is `Running` — or `Pending` while the job is in `Work`.
  Every other state accepts a claim, including `Frozen` and `Stalled`, because
  the claim is a flag consumed by the *next* attempt rather than an action on
  the current one.
- **Scheduling visibility** has no single predicate in the tree. It is the
  conjunction of the checked invariants in §5: only a `Ready` job sits in the
  ready queue, only an executing job holds an execution slice, only a `WrapUp`
  job sits in a merge queue.

### Task states

Tasks are a separate, much smaller machine, in `crates/types/src/task.rs`:

- `TaskState` — `Pending`, `Running`, `Done`, `Failed`. There is **no**
  `is_terminal` helper; `Done | Failed` is spelled out at each site, which is
  worth centralizing in a reimplementation. `Pending` covers two very different
  situations, told apart by `PendingReason`: absent means an operator-facing
  item awaiting a human, and `QueuedForCapacity` means a container launch the
  fleet had no slot for. No retry budget is consumed while either waits.
- `TaskPhase` — `Work`, `Evaluation`, `MergeGate`, `WrapUp`, `Triage`,
  `Escalation`. The phase is the routing key for almost everything: which
  decider owns a `TaskExited`, which drain priority a deferred launch gets,
  which phase an escalation `Retry` resumes at, and how the UI labels a row.
  `Escalation` is stamped on the escalation task itself rather than on the phase
  that failed.
- `TaskKind` — `Command { run }`, `Agent { provider, model, prompt }`,
  `Human { prompt }`. The kind decides where the verdict comes from: a
  command's exit code, an agent's submission, or a human's resolution.
- `Performer` — only `Performer::Human`, meaning a claimed attempt an operator
  performed. Ordinary execution is implied by absence, so the field never
  restates the kind.

Neither the job state nor the task state is derivable from the other. A `Work`
job can hold a `Failed` task (the retry that is about to be created), and a
`Done` task can belong to an `Escalated` job.

---

## 2. The event alphabet

Every input to the dispatcher — an HTTP-originated request, a container exit, a
timer tick, a worker heartbeat — arrives as one variant of `Msg` in
`crates/dispatcher/src/core.rs`, over a single mpsc channel into one task.
[`docs/reference/contracts.md`](contracts.md) calls this enum the actual
high-level interface of the entire dispatcher, and it is the alphabet a
reimplementation must reproduce first: nothing else can write platform state.

**The enum has 30 variants.** Counted two independent ways over
`crates/dispatcher/src/core.rs`: the variant declarations in the `enum Msg`
block, and the arms of `Msg::label`, whose match is exhaustive and spelled out
precisely so a new variant must be named there too. Both give 30. No gate checks
an enum's arity — check 2 of `.chug/tasks/check-doc-facts.sh` verifies backticked
constants against a `pub const`, and this is not one — so treat this number, and
every figure like it, as a measurement to redo rather than a fact to trust.

### Commands — 24 variants, each carrying a `Reply`

A command is a request whose sender waits: the variant carries a
`Reply<T>` oneshot, and the actor answers on it. Answering is part of the
contract, so a command's error taxonomy (`CoreError`, which the API maps to HTTP
status) is as much interface as its success shape.

| Variant | Origin | Answers with |
| --- | --- | --- |
| `CreateJob` | `req.jobs.create.*` | the created `Job` |
| `ReleaseJob` | `POST .../release` | the state it landed in (`Ready` or `Blocked`) |
| `RevokeJob` | `POST .../revoke` | the seqs the cascade revoked |
| `UpdateJob` | `PATCH .../jobs/{seq}` | the rewritten `Job`; 409 unless `Draft` |
| `DraftJob` | `POST .../jobs/{seq}/draft` | unit; only `Frozen`→`Draft` |
| `FinalizeJob` | `POST .../jobs/{seq}/finalize` | unit; only `Draft`→`Frozen` |
| `EditMembers` | `POST .../jobs/{seq}/members` | the `Job`; Draft batches only |
| `EditGroups` | `req.jobs.groups.*` | the `Job`; accepted in **every** state |
| `SetRequireApproval` | `req.jobs.approval.*` | the `Job`; 422 once in Work |
| `ClaimJob` | `req.jobs.claim.*` | unit; 409 with an attempt in flight |
| `UnclaimJob` | `req.jobs.unclaim.*` | unit; only before the claim parks a task |
| `TriageJob` | `req.jobs.triage.*` | unit; never changes job state |
| `SubmitResult` | `req.work.submit.>` | unit |
| `SubmitEval` | `req.eval.submit.>` | unit |
| `ResolveTask` | operator inbox | unit |
| `ChannelPost` | `req.channel.update` / `.reply` | unit |
| `LinkProject` | `req.projects.link` | the `ProjectRecord` |
| `OriginRelease` | `req.origin.release` | the `ProjectRecord` |
| `OriginStatus` | `req.origin.status` | link + release state |
| `OriginSync` | `req.origin.sync` | link + release state |
| `SetNodeCapacity` | `req.fleet.capacity.set` | a 202-shaped ack, never the node's reply |
| `QueueSnapshot` | `req.queue.list.{owner}.{project}` | the live launch queue, one project |
| `Ping` | `req.health` | unit — a round trip proves the loop is draining |
| `Drain` | the SIGTERM handler | unit, once the drain completes |

Two of these are commands by shape and events by nature: `SubmitResult` and
`SubmitEval` are facts a container reports about itself, and they carry a
`Reply` only because the agent's MCP call wants an acknowledgement. `Drain` is
the other oddity — a process-lifecycle signal rather than a request, and the
only command that stops the loop.

`Ping` deserves its own note for a reimplementation: it is a no-op that proves
the *single-threaded state loop* is draining messages, which is a strictly
stronger liveness signal than "the process is up". Build one.

### Events — 6 variants, facts the world reports

| Variant | Reported by | What it means |
| --- | --- | --- |
| `TaskExited` | a container monitor task | a task's container exited, with its `TaskExit` |
| `TaskContainerStarted` | the launch forwarder | the container id, the instant it launches — providers only surface it after exit, so without this the record reads `container_id: null` for the whole run |
| `LaunchDeferred` | an agent launch task | the fleet had no slot; queue the launch instead of spending a retry budget |
| `Scan` | the internal ticker | the periodic sweep: task timeouts, job deadlines, schedule occurrences, launch-queue drain, config republish |
| `WorkerAnnounce` | the `event.worker.announce` subscriber | a worker daemon's heartbeat, carrying the whole announce because its capacity is only applied when its `(capacity_epoch, capacity_generation)` pair clears the node's watermark |
| `CapacityPushed` | the spawned `set_slots` RPC | one capacity push's outcome, carrying the value it pushed so a reply that lost a race is dropped |

Four of the six are posted from *inside* the dispatcher crate and by nothing
else — `TaskExited`, `TaskContainerStarted`, `LaunchDeferred`, `CapacityPushed`.
That is the pattern that keeps concurrency out of the state machine: the
concurrent work (waiting on a container, driving an RPC) runs off the actor
thread and its *result* comes back as an event. A reimplementation that instead
lets those tasks write state has lost the single-writer property, whatever else
it keeps.

`Scan` is the only variant whose reply is optional (`Option<Reply<()>>`): the
ticker sends none, and a test triggering a scan synchronously supplies one.

**There is no ingest event.** [`docs/spec.md` §13](../spec.md#132-ingest) specs
an ingest stream, and `crates/api/src/ingest.rs` durably appends to it, but no
subscriber in the dispatcher consumes `ingest.*` and no `Msg` variant carries
one. A 202 from the ingest endpoint means "durably appended", not "triaged" —
and today nothing triages. A reimplementation should treat factories and ingest
as unbuilt rather than as an event source to mirror.

---

## 3. Transitions — where the guards actually live

[`docs/spec.md` §2.1](../spec.md#21-state-machine) holds the normative
transition table: **47 data rows**, counted directly. It is complete, it is the
one owner of the state machine, and it is deliberately **not** restated here — a
second copy would begin drifting the day it was written.

What §2.1 lacks for an implementer is that its Guard and Effect columns are
frequently prose cross-references: "see §3.3", "§3.2 step 9", "per §3.5",
"docs/reference/design-lifecycle.md". Those rows cannot be implemented from the
table alone. The mapping below is the missing half — for each row family, the
module that actually decides it and what the decision turns on.

The deciders are pure functions in `crates/domain/src/decide/`, each with the
signature `decide(view, event) -> (transitions, effects, step)`. The dispatcher's
shim gathers the view, calls the decider, applies transitions through
`Core::set_state`, then runs the effects. `Core::set_state` is the single funnel:
it calls `assert_transition` first, stamps `completed_at` on a terminal target,
writes KV, then updates the in-memory graph.

| §2.1 row family | What the table defers | Decider | What it actually decides |
| --- | --- | --- | --- |
| `(creation)`→`Frozen` \| `Draft` | — | none; `Core::create_job` with `crates/domain/src/decide/authoring.rs` | batch member admissibility, the dep/eval union a batch inherits, the auto-description |
| `Draft`→`Draft` (field edit, member edit) | "Job is Draft", "Draft batch" | `Core::update_job`, `Core::edit_members`, over the same authoring primitives | full-field replace vs per-candidate membership rules; **no state write happens** — see the finding at the end of this section |
| `Draft`→`Frozen` | "Job is Draft" | `Core::finalize_job` + authoring | the same validation release does, then parks re-batchable instead of scheduling |
| `Frozen`→`Draft` | "never released" | `Core::draft_job` | un-absorbing the batch's members so membership can be edited |
| `Frozen`→`Batched`, `Batched`→`Frozen` | "§2.1 batches" | `crates/domain/src/decide/authoring.rs` | exists, `Frozen`, same type, not already batched, not itself a batch, carries no inputs |
| `Draft`/`Frozen`→`Ready` \| `Blocked` | "All deps Done", "§2.2", "§1.1" | `crates/domain/src/decide/ready.rs`, event `Released` | admit vs park; pinning `base_ref` at the *validated* HEAD; materializing declared input defaults exactly once; committing a Draft batch's membership |
| `Blocked`→`Ready` \| `Stalled` | "re-validation of static config at `base_ref`" | `crates/domain/src/decide/ready.rs`, events `DepsChanged` → `Revalidated` | eligibility **first**, then the verdict. `DepsChanged` decides eligibility only and returns `ReadyStep::Revalidate`; the ref reads and config loads run solely because of that, and their result re-enters as `Revalidated`. A clean pass admits at the fresh HEAD; any error emits `Effect::Stall` |
| `Ready`→`Work` | — | `crates/domain/src/decide/ready.rs` event `Dequeued`, then `crates/domain/src/decide/work.rs` event `Entered` | whether the job may still take the slot it waited for (revoked or escalated meanwhile → forfeit), then the launch-time contract and the §2.2 launch pass |
| `Ready`→`Stalled` (deadline) | "§3.5" | `crates/dispatcher/src/scan.rs` → `crates/domain/src/decide/escalation.rs` with `EscalationKind::Stall` | the one-shot job deadline elapsed before work started |
| `Ready`→`Stalled` (launch validation) | "declared secret or var missing…", "§14.2 skew" | `crates/domain/src/decide/work.rs`, event `Entered` | **which park.** A job still `Ready` has no work task, so it parks `Stalled` (Retry/Revoke only); the identical failure on a rework re-entry is post-work and parks `Escalated` |
| `Work`→`Work` (container fail, human `Fail`, infra loss) | "attempt ≤ `work_retries`" | `crates/domain/src/decide/work.rs`, events `Exited`, `Declined`, `InfraLost` | one retry policy for all three. The two axes callers differ on — whether the branch is recovered or preserved, and what context carries forward — are *values* on `WorkStep::Retry`, not separate policies. `InfraLost` relaunches the **same** attempt and spends no budget, bounded by `infra_relaunch_cap` |
| `Work`→`Evaluation` | "§3.2 step 9", "§3.3 staged evaluation" | `crates/domain/src/decide/work.rs` (`Exited` → `OutputChecked`), then `crates/domain/src/decide/eval.rs` (`Entered`) | the finish-line guard: "did the branch move?" is a ref read, so the decider asks for it and the answer re-enters. Then stage 0 launches and later stages stay **uncreated**, so a failing stage leaves the ones after it unbuilt |
| `Work`→`Escalated` | "attempt > `work_retries`" | `crates/domain/src/decide/work.rs` → `crates/domain/src/decide/escalation.rs` | budget exhaustion, a human `Fail` with no retries, or launch validation on a rework re-entry |
| `Evaluation`→`Evaluation` (stage advance, retry) | "§3.3 staged evaluation", "attempt ≤ `eval_retries`" | `crates/domain/src/decide/eval.rs`, events `SlotExited`, `SlotResolved`, `StageLaunched`, `SlotRelaunched` | the verdict source per evaluator kind, and the distinction that matters most: a **verdict-less** exit is infrastructure loss, not a product failure |
| `Evaluation`→`Work` | "evaluated cycle N ≤ `rework_budget`" | `crates/domain/src/decide/eval.rs` reduce → `EvalStep::Rework` | one `reworks_used` spent; the branch is **preserved** and `base_ref` unchanged |
| `Evaluation`→`Escalated` (3 rows) | "infra error", "budget exhausted", "`work.type: command`" | `crates/domain/src/decide/eval.rs` reduce → `EvalStep::EscalatedDropExec` | a required **abort** verdict skips the rework budget entirely — not satisfiable by rework |
| `Evaluation`→`WrapUp` | "enqueue on the per-project merge queue" | `crates/domain/src/decide/merge_gate.rs`, `decide_enqueue` | the only producer of this edge, and the enqueue is idempotent so a re-finalized job cannot queue twice |
| `Evaluation`→`Done` | "`wrap_up: none`" | `crates/domain/src/decide/eval.rs` → `crates/domain/src/decide/wrapup.rs` (`Completing`) | branch deletion, the `Done` stamp, the announcement, the dependents fan-out |
| `WrapUp`→`WrapUp` (merge gate) | "see §3.3" | `crates/domain/src/decide/merge_gate.rs`, `decide` driven by `MergeGateState::next_candidate` | the depth-1 serialization, the candidate build against moved HEAD, the gate rounds, and the promote CAS |
| `WrapUp`→`WrapUp` (`wrap_up.run`) | "see §3.2" | `crates/domain/src/decide/wrapup.rs`, event `Landed` | whether a publish command holds the job in WrapUp, or it completes directly |
| `WrapUp`→`Work` (conflict, gate failure) | "see §4.3", "see §3.3" | `crates/domain/src/decide/merge_gate.rs` | the gate-fix fast path vs the full rework loop, bounded per landing; either way the rework budget is **not** consumed and `base_ref` moves to current HEAD |
| `WrapUp`→`Done` | a four-clause conjunction | `crates/domain/src/decide/merge_gate.rs`, then `crates/domain/src/decide/wrapup.rs` | the squash (a no-op when there are no commits), then terminal bookkeeping and a batch's fan-out |
| `WrapUp`→`Escalated` (config skew) | "§3.3 step 0, §14.3" | the merge-gate landing path | the branch's declared `min_dispatcher` against the running binary's epoch. Lands nothing; the queue advances |
| `WrapUp`→`Escalated` (hard failure, publish non-zero) | "docs/reference/design-lifecycle.md", "§3.2" | `crates/domain/src/decide/wrapup.rs`, event `PublishExited` | the merge is **never undone** — only the external publish failed. The queue advances past the job rather than wedging |
| `Escalated`→`Work` \| `Evaluation` \| `WrapUp` | "`action: Retry` and X is what failed" | `Core::escalation_retry` in `crates/dispatcher/src/exec.rs`, dispatching on the failing task's phase; the WrapUp arm is `crates/domain/src/decide/wrapup.rs` event `RetryRequested` | **which phase resumes is read off the failing task**, so a Retry never re-runs work that already succeeded |
| `Stalled`→`Ready` \| `Stalled` | "the failed step succeeds" | `Core::prework_retry` in `crates/dispatcher/src/exec.rs` → `crates/domain/src/decide/ready.rs` | re-running the same re-validation; a second failure creates a new Human task and the job stays `Stalled` |
| any non-terminal→`Revoked` | a seven-clause effect cell | `Core::revoke_job` in `crates/dispatcher/src/core.rs`; no decider | the cascade set (transitively, `Frozen`/`Blocked`/`Ready` dependents only), closing Pending human tasks with a synthetic resolution, unhooking the merge gate, and returning a revoked batch's members to `Frozen` |

`crates/domain/src/decide/schedule.rs` is the seventh decider and owns no §2.1
row: it decides whether a schedule occurrence is due and whether it fires or is
skipped. Its whole rule is one value per schedule — the anchor an occurrence
must fall strictly after — which is what makes a run of missed occurrences
coalesce to exactly one fire instead of a backfill storm.

That anchor has **three disjoint branches and is never a `max` of them**. A
schedule that has never fired anchors on an in-memory first-seen instant; one
whose latest job is terminal anchors on that job's completion, falling back to
its creation for a record written before the completion field existed; one whose
latest job is non-terminal has no anchor at all, because no fire is possible
there. Taking the maximum looks equivalent and destroys catch-up — the
first-seen value resets to now on every restart, and downtime *is* a restart, so
first-seen would dominate the maximum after every outage and suppress exactly
the fire the coalescing rule exists to produce.

The **skip** event is bounded by a *second* value rather than by the anchor.
While a schedule is blocked its inputs are byte-identical on every tick, so the
dedupe key is an in-memory last-skipped occurrence, and the interval searched for
a reportable occurrence starts strictly after the fire that is currently
blocking. Any weaker lower bound emits a spurious skip trailing every real fire.
The last-skipped value is safe to lose on restart: a restart re-emits at most
one event for the occurrence currently blocked. The first-seen instant is safe
to lose only for a schedule that has fired at least once — one that never has
can be starved indefinitely by restarts more frequent than its own period, and
self-heals the moment one occurrence is observed.

### Finding: §2.1 and `crates/domain/src/state.rs` agree on every edge but one

`crates/domain/src/state.rs` is ~120 lines and is the sole authority on
transition legality: `assert_transition(from, to)` is a total pure function, and
no state write in the dispatcher bypasses it. Comparing its edge set against
§2.1's 47 rows, edge by edge:

- Every edge `assert_transition` permits has a §2.1 row.
- Every §2.1 row's edge is permitted, **except `Draft`→`Draft`** — two rows (the
  `PATCH` full-field replace, and the Draft-batch member edit).
  `assert_transition(Draft, Draft)` returns `Err`, and the negative case in
  `state.rs`'s own test list does not mention the pair either way.

This is not a live defect: both Draft-edit paths persist with `jobs.put`
directly and never call `Core::set_state`, so the rejection is unreachable. It
is a **representational disagreement** about what a table row is for. §2.1 uses
a self-row for "request accepted, state unchanged" — the same shape it uses for
`Work`→`Work` and `Evaluation`→`Evaluation`, which *do* pass through the funnel.
`state.rs` enumerates only edges that reach the funnel. A reimplementer reading
either artifact alone gets a defensible but different answer about whether
`Draft`→`Draft` is a transition.

Left as a finding rather than resolved in prose, because which side is wrong is
a decision about the platform: either the table should distinguish
state-changing rows from accepted-no-op rows, or `assert_transition` should
admit `Draft`→`Draft` and the edit paths should route through the funnel like
everything else.

A second, narrower disagreement sat inside `docs/spec.md` itself and pointed the
same way; it is **closed**, and the record is kept because the two are easy to
confuse. §2.1's rows and the code agree that a failed Ready-transition
re-validation and a failed launch-time validation park the job **`Stalled`** —
`Effect::Stall` targets `JobState::Stalled`, and `crates/domain/src/decide/ready.rs`
and `crates/domain/src/decide/work.rs` both emit it. §2.2's prose said
"transitions to Escalated" for both, against its own table and against the code,
and was simply wrong; both sentences now read `Stalled`. Unlike the finding
above, that one needed no decision — nothing was in tension but the prose.

---

## 4. The effect vocabulary

An `Effect` is one thing the dispatcher does *about* a decision — a write to the
world through a port, never a decision itself. The enum is
`chuggernaut_domain::effects::Effect` in `crates/domain/src/effects.rs`; the
interpreter is `Core::interpret` in `crates/dispatcher/src/interpret.rs`, which
is the only place an effect meets a port and holds the only remaining `&mut Core`
coupling the deciders keep.

**The enum has 28 variants.** Counted over the variant declarations and
independently over the arms of `Effect::port`, which the interpreter is kept in
lock-step with; both give 28.

Reads are deliberately **not** effects. `jobs.get`, `tasks.list_for_job`,
`counters.next`, the clock — these feed the decider's view of the world, they are
not its output. Getting this backwards is the commonest way a
decider/effects split stops being testable.

**Job creation is not in the vocabulary, and that is the one seam that does not
close.** There is a `PutJob` and no `CreateJob`, because allocating a job seq is
I/O and a decider cannot mint one — while gathering a pre-allocated seq into the
view instead would burn an id on every scan tick for the overwhelmingly common
"nothing is due". So the schedule decider returns fire *decisions* rather than
effects, and the shell creates and releases each job. A reimplementation is free
to close this seam; it is named here because it is the one decision that needs
an id the decider cannot mint. A decision output that is not an effect is
otherwise ordinary — the five lifecycle-phase deciders each return a step value
beside their effects (`ReadyStep`, `WorkStep`, `EvalStep`, `WrapUpStep`, and
`EnqueueStep`/`LandingStep` from the merge gate), while `escalation::decide`
returns transitions and effects alone — but a decision the shell must
*originate* is not.

| Group | Variants |
| --- | --- |
| State writes | `SetJobState`, `PutJob`, `AppendRdep`, `RemoveRdep`, `PutProject`, `WriteKv` |
| Task lifecycle | `CreateTask`, `PutTask` |
| Container control | `KillContainer`, `RemoveContainer` |
| Launches | `LaunchWorkTask`, `LaunchWrapupTask`, `LaunchEvalStage`, `LaunchEvaluator`, `LaunchGateStage`, `LaunchGateFix`, `DeferLaunch` |
| VCS | `SquashMerge`, `CreateSquashCandidate`, `AdvanceDefault`, `RebaseOntoWithConflict`, `DeleteBranch` |
| Publishing | `PublishEvent`, `PublishStatus` |
| Credentials | `IssueCredentials` |
| Composites | `EnterWork`, `Escalate`, `Stall` |

Four of these need a sentence each, because their shape is not obvious from the
name:

- `CreateTask` versus `PutTask`. The first persists a *freshly created* task and
  announces it in one indivisible action, because a stored task with no event is
  one the operator UI never learns about and an event with no record is a
  phantom. The second is an update to a task that already exists.
- `SetJobState` versus `PutJob`. The first goes through the §2.1 funnel; the
  second persists a record without a state change (definition edits, cover
  stamps). A reimplementation that offers only one of these will either bypass
  the transition guard or be unable to edit a Draft.
- The three composites are named for the multi-step actions the shell owns:
  `Escalate` and `Stall` each create a Human task, flip the state and announce;
  `EnterWork` re-enters the Work phase for a *rework* cycle. Cycle-1 Work entry
  is deliberately **not** this effect — it is a step the Ready decider returns,
  because it belongs to the Work phase's own contract.
- `IssueCredentials` mints a per-job SSH certificate scoped `ReadWrite` (Work
  phase, may push) or `ReadOnly` (eval phase). The domain crate mirrors the auth
  crate's type rather than depending on it, so the vocabulary stays plain
  serializable data.

### The continuation contract

**An effect whose result the decision needs is emitted, and then the decider
terminates.** The interpreter returns the result as an outcome, and the shim
re-enters `decide` with it as the next event, against a **freshly gathered
view**. A decision never runs on a view the world moved under.

That is a rule, not an optimization. It is what makes a pure decider safe in a
system where every interesting answer requires I/O:

- `SquashMerge` and `CreateSquashCandidate` come back as a merge outcome;
  `AdvanceDefault`'s compare-and-swap comes back as either success or
  `PromoteRefused`, which is a **decision input** ("HEAD moved again —
  refinalize"), never an error.
- `RebaseOntoWithConflict` reports conflicts as data rather than as failure.
- A launch effect's *identity* comes back as an event. Task ids exist only once
  the launch ran, so `LaunchEvalStage` and `LaunchEvaluator` re-enter as
  stage-launched and slot-relaunched events, and the decider is what lands them
  on the round they belong to. The shim never reaches into the value it handed
  over.
- The same mechanism is used in the *opposite* direction to keep expensive I/O
  behind a decision: the Ready decider's `DepsChanged` decides eligibility only
  and asks for re-validation, so the ref reads and config loads never run for a
  job that was not going to move.

The corollary a reimplementation must respect: a cheap read the decision
*might* need must be gathered into the view **before** the branch that needs it
is taken. A read-after-write on one branch of a decision is exactly the bug the
view is meant to prevent.

### Launch effects carry no placement

`LaunchWorkTask`, `LaunchWrapupTask` and the eval-stage launches name a job, a
cycle and an attempt — never a node. Placement is composed on the far side of
the port, from exactly one source, and
[`docs/reference/contracts.md`](contracts.md) states the contract and the four
sites that compose it. Read it there; it is not restated here.

One consequence is worth stating where the effects are, because it surprises
people: placement is **job-scoped in a task-scoped system**. The pin is a
job-type field, so it binds every level the job launches — the work agent, the
work command, every evaluator including the appended `ci` one, the merge gate,
wrap-up, and a queued relaunch of any of them — including levels whose resolved
mode has nothing to do with why the pin was written. There is no per-level
override, and design [#543](../design/543-placement-granularity.md) D4 declines
to add one — **not** because the harm is imaginary (it is measured, and
environment matching does not remove all of it) and **not** because a per-level
pin costs more (corrected, it costs less), but because only one of the two
expresses the requirement: a level states what it *needs* (`image`,
`runtime.env`), never where it runs. D4 names its own revisit conditions.

### Launch effects mint per container

A launch granted no cloud identity must carry nothing, and that is **asserted at
the injection site** rather than assumed: no credential file, and no vendor
credential variable pointing into the directory those files live in — pinned by
a test in which a sibling container of the same job does hold one. That second
half is judged by **value, not by name**, because a project may legitimately set
the vendor variable itself in its own `vars:`. The assert sits on the path every
launch that resolves a declaration passes through — work agent and work command,
command and agent evaluators, wrap-up, and the capacity queue's resume, which
re-resolves and mints afresh rather than reusing a token from before the wait.

The gap a reimplementation should design out: triage and escalation launches sit
outside that path, because they launch no declaring block and so never reach the
injection site. **A launch path added the way triage was added bypasses the
assert silently rather than tripping it.**

### Launch effects carry a prompt, and that is the whole handoff

A launch effect names no node, but it does carry an assembled prompt, and
**everything one phase hands the next lives in that string.** A reimplementation
that gets states, effects and invariants right and this wrong produces a system
whose agents start blind, so it belongs in the model rather than in the prompt
templates.

Four named context blocks are assembled into the prompt at launch — the
predecessor block ahead of the prompt file's content, the other three after it:

- **Predecessor** — attempt N → N+1, same phase and cycle.
- **Rework context** — an evaluation failure, or a merge conflict, into the next
  work cycle, carrying every evaluator's result plus operator-origin entries.
- **Re-review context** — reviewer cycle N → N+1.
- **Gate-fix** — a compile-class gate failure into a scoped fix task.

Two edges carry nothing today, and design
[#169](../design/169-handoff-continuity.md) rates both as **high-severity gaps
rather than as choices**: work → evaluation, where the work agent's own summary
and structured result reach the evaluator by no path at all, and a dependency
reaching `Done` → the dependent's brief. Each has a proposed block and a ticket.
The edge that *is* ratified as deliberately lean is work → wrap-up, where the
publish command wants environment variables and nothing else. Most embedded
streams are tail-capped by a named constant; structured findings are the
exception and are embedded uncapped, which #169 carries as its own ticket.

**The framing rule is load-bearing and is not a style choice.** Predecessor
output is presented as *claims and history, never as instructions* — the
successor's own verification is authoritative, and a claim it did not verify is
not evidence. Three of the four blocks say so in a sentence of their own; the
rework block is the one that does not, and #169 proposes the framing sentence as
a family-wide convention rather than recording it as one. A block without that
framing converts independent review into rubber-stamping, which is the one
failure that makes the whole evaluation phase worthless.

---

## 5. Invariants

Invariants are the artifact that survives a language change untouched, because
they are statements about **data**, not about code. Six are executable today, in
`check_invariants(&CoreState) -> Vec<Violation>`
(`crates/dispatcher/src/invariants.rs`), a pure and total function over a
read-only view of the single writer's in-memory state. The integration suites
run it after every message. This is cheap precisely because all the state lives
in one place.

Stated as properties of data:

1. `ready_queue_only_ready` (spec §3.1) — every entry in the ready queue names a
   job that exists and is `Ready`.
2. `rdeps_inverts_deps` (§1.4/§2.3) — the reverse-dependency index is the exact
   inverse of the forward `deps` edges. Checked in both directions: every
   forward edge has its reverse, and no reverse edge is invented.
3. `active_is_executing` (§3.2/§3.3) — an execution slice exists only for a job
   that is `Work`, `Evaluation`, `WrapUp` or `Escalated`.
4. `merge_queue_is_wrapup` (§3.3) — every job in a merge queue, queued or
   gating, is `WrapUp`; and a gating seq has already left the queue, so it never
   appears in both.
5. `terminal_is_absorbing` (§2.1) — no terminal job is still referenced by the
   ready queue, the active set, a merge queue, or a gating slot.
6. `one_live_job_per_schedule` (§1.1 schedules) — at most one non-terminal job
   in a project carries a given schedule's provenance. A schedule's finished runs
   stack up freely; two live ones mean the backpressure failed.

"Non-terminal" in invariant 6 means anything but `Done` or `Revoked` —
**including `Frozen`, `Stalled` and `Escalated`** — so the skip rule that
invariant protects is also the repeated-failure suppression: a schedule failing
nightly for a week produces one escalation and six skip events, not seven
identical escalations. The cost is that an unattended escalation silently
disables the schedule; nothing re-arms it, so "my nightly stopped running" and
"my nightly is failing" become the same operator-visible condition,
distinguishable only by looking. That is deliberate, and the mitigations it
rests on are the escalation task's own notification and the skip event naming
the job that blocked it. Note also that skip is not merely the cheapest overlap
policy: cancel-previous was rejected **on its merits** — auto-revoking a live
job an operator may be mid-diagnosis on is a surprising loss — and a shared
concurrency group has nothing to be built on, because there is **no user-facing
concurrency primitive**. If either is ever wanted it is its own design, and must
not arrive as a field on a schedule file.

Two properties are enforced **structurally** rather than checked, and a
reimplementation should reach for the same move where its type system allows:
"one attempt in flight per job" holds because the active map is keyed by seq, and
"the merge gate is depth-1 per project" holds because `MergeGateState.gating` is
an `Option<u64>` that cannot hold a second seq.

### Finding: the invariants that live only in tests

The following are "given this, then that must hold" facts the integration suites
in `crates/dispatcher/tests/` assert, but which no invariant function states.
Each is something a reimplementation has to preserve without being told. They
are listed here as a **gap**, not as machinery — naming them is the deliverable,
and adding a check is a code job's business.

Three of them are gaps by construction, because `CoreState` simply does not
carry the state they constrain: it borrows the graphs, the ready queue, the
active map and the merge gates, and nothing else.

- **No task-record invariants at all.** `CoreState` holds no tasks, so nothing
  checks that a task's `job_seq` names an existing job, that a task resolves to
  a terminal state exactly once, that at most one non-evaluator Work task is
  live per job, or that a task terminal in KV has left the operator inbox. Every
  one of these is asserted somewhere in `crates/dispatcher/tests/execution.rs`,
  `crates/dispatcher/tests/claim.rs` and
  `crates/dispatcher/tests/gate_and_human.rs`.
- **No launch-queue invariants.** The §3.5 capacity queue is not in `CoreState`.
  Test-only: a deferred launch's task is `Pending` with `QueuedForCapacity`; a
  launch's `queued_at` is **preserved** across re-defer, so a launch that keeps
  missing a slot still escalates on time; a task appears at most once in the
  queue; finishing-phase launches drain ahead of work
  (`crates/dispatcher/tests/execution.rs`).
- **No fleet or capacity invariants.** Occupancy against advertised slots, and
  the monotonicity of a node's `(capacity_epoch, capacity_generation)` watermark,
  are asserted only in `crates/dispatcher/tests/dynamic_fleet.rs` and
  `crates/dispatcher/tests/fleet_e2e.rs`.

The rest are gaps about state `CoreState` *does* carry:

- **The graph is acyclic.** Checked at release time — `crates/domain/src/release.rs`
  calls `JobGraph::creates_cycle`, which lives on the graph type in
  `crates/domain/src/graph.rs` — and never again. A cycle
  reachable another way — a Draft re-pointed and released, a record written
  before the rule — would not be reported by `check_invariants`, even though the
  graphs are right there in the view.
- **`base_ref` is pinned exactly from `Ready` onward.** A `Ready` job has
  `Some`, a `Blocked` job has `None`; asserted in
  `crates/dispatcher/tests/inputs.rs` and `crates/dispatcher/tests/lifecycle.rs`.
  This is the invariant every downstream read depends on — after the
  Ready-transition, the moving default branch is never consulted again.
- **`completed_at` is set if and only if the job is terminal.** `Core::set_state`
  stamps it on a terminal target; nothing checks the biconditional.
- **The revoke cascade leaves no live dependent.** After a revoke, no
  transitively-reachable `Frozen`/`Blocked`/`Ready` dependent is still
  non-terminal, and a revoked batch's members are all back to `Frozen` with
  `batch_id` cleared. Asserted in `crates/dispatcher/tests/lifecycle.rs`
  (`revoke_cascades_through_pending_dependents`) and
  `crates/dispatcher/tests/batch.rs`.
- **`batch_id` and `Batched` agree.** `batch_id` is `Some` exactly for a job
  that is (or was) a member; a batch is never nested inside a batch; members
  share the batch's type. Enforced at authoring time, asserted in
  `crates/dispatcher/tests/batch.rs`, checked by no invariant.
- **A pinned job's `inputs` never move.** Once materialized at the
  Ready-transition, a later change to the type's declaration does not rewrite
  them (`crates/dispatcher/tests/inputs.rs`).
- **A claim covers exactly one attempt.** The claim flag is consumed by the
  decision that honours it, so an attempt is either launched or parked, never
  both, and never twice (`crates/dispatcher/tests/claim.rs`).
- **A failing eval stage leaves later stages uncreated.** The absence of a task
  is the assertion, which is why it is easy to lose in a rewrite
  (`crates/dispatcher/tests/golden_traces.rs`).
- **A context block is a pure function of persisted records.** A restart between
  deciding a launch and performing it must yield a byte-identical prompt: every
  block in §4 is assembled at launch time from the job and task records, the
  artifact store and the bare repo, and the in-memory execution slice may cache
  but never originate. The predecessor, re-review and gate-fix blocks satisfy
  this, as does the triage prompt. The operator-handoff path does **not**,
  because the execution slice rebuilt after a restart carries an empty evaluation
  context; escalation resolutions do not either, for a different reason — they
  are never forwarded at all. The rework block sits between: it renders from the
  in-memory slice, and survives a restart only because reconciliation re-enters
  evaluation and regenerates the results. Nothing pins any of this today —
  golden decision traces exist, but they record an effect as a label and capture
  no prompt text, so the property comes for free only once a trace carries the
  launch effect's prompt.

`crates/dispatcher/tests/golden_traces.rs` is worth a reimplementer's attention
for a second reason: each fixture under `crates/dispatcher/tests/traces/` records,
per scenario, the ordered `(transitions, effects)` the dispatcher produced — the
scenario's own setup and inputs live in the Rust test that drives `Core`, and the
fixture's `event` field is a prose label for the scenario rather than a
machine-readable input. That makes them a language-neutral record of expected
output rather than a replayable conformance suite: porting one means re-creating
its setup, but the pass/fail comparison transfers unchanged.

---

## 6. Authority — who decides what

Nothing in the tree states this split, and it is the first thing a
reimplementation has to get right, because it determines what may be
concurrent. The rule is one process, one thread, one writer: the dispatcher's
core actor. [`docs/reference/style.md`](style.md) argues why a second writer is
the wrong shape rather than a scaling decision.

**Global — the core actor decides, and no other component may:**

- **Dependency admission and dependent unblocking.** Whether a `Blocked` job's
  deps are all `Done`, and the fan-out when one reaches `Done`. It is global
  because it reads other jobs' states.
- **Revoke cascades.** The transitive dependent set, which of those states
  cascade and which are left alone, closing their Pending human tasks, and
  unhooking every reference from the ready queue and the merge gate.
- **Batch completion fan-out.** One merge completing N members, then each
  member's own dependents unblocking as if it had run individually.
- **Capacity accounting and queue priority.** Which node has a free slot,
  whether a launch is deferred or fails, the drain order (finishing-phase
  launches ahead of work), the max-queue-wait backstop, and the operator's
  capacity intent versus a node's observed report.
- **Merge-gate serialization.** One landing in flight per project, at depth 1.
  The FIFO, the hold an open origin release places on it, and the drain
  suppression. That hold is a set of project slugs, and it is **core state, not
  the forge-ingest half's private state**: the gate reads it when deciding
  whether a job may land, and the core rebuilds it at startup. Taking it,
  clearing it when the release merges or closes, and pumping the gate afterwards
  all happen inside the single writer today, in the forge-ingest half's own
  code, which is safe precisely because the two halves share one actor. It is
  the **boundary** that would break it: a reimplementation putting forge-ingest
  behind a crate or process line must make the hold an effect the core applies,
  or it has a second writer of merge-gate state.
- **Retry, rework and restart-reconciliation dispatch.** The budgets are
  per-job values, but *spending* them is a core decision, and restart
  reconciliation re-derives every mid-execution job from the task log before the
  message loop starts a single message.

**Job-local — decided from one job's own record and its type:**

the resolved job type and its evaluator list (the type's `eval` is a floor a job
may add to, never remove from); the job's cycle, attempt and remaining budgets;
which evaluator verdicts a round has collected and what the reduce makes of
them; the branch's contents; the composed brief an agent reads; the per-job
timeout and model overrides; the job's input values.

The practical test for a reimplementation: **a decision is global exactly when
it reads or writes state belonging to a job other than the one being decided.**
Every item in the first list does; nothing in the second does. That is what
makes the second list safely parallelizable and the first list not.

### Node-local — the node decides, and neither the core nor the repo may

A third authority sits beside the core actor and the job record, and it is the
one a reimplementation is most likely to collapse into the other two. A node
**advertises** physical capacity — its modes, the environments it resolved,
whether it can bound a declared limit — and the scheduler may match a declared
requirement against that. Advertising a *device* is the obvious next member and
no node advertises one yet. A node separately **grants** capability out of its
own config, and never advertises what it granted. Capabilities a launch receives
beyond its image, command and environment — a device passthrough, a toolchain
mount, the docker socket — are composed on the far side of the port and matched
against identity stamps the core already put on the launch. The socket's grant
reads all three of project, job type and phase; the device and toolchain grants
match on the project alone.

**The docker socket is neither a launch-config field nor a job-type field, and
that is a security property rather than a layering preference.** A field the
platform honours on request means a **merge** grants node root — and the merge
gate is agent-driven on a repo whose own evaluators approve it, so that is a
self-granting loop. A node-side entry is not, because it requires an act outside
the system being granted access to. A reimplementation that lets a job type
*request* a root-equivalent node grant has reintroduced the loop however
carefully it validates the request.

The test for which side a capability falls on is **blast radius**, not
convenience, and the two ends of the class are not alike. A device like
`/dev/kvm` is in the class of a cpu or memory limit — a physical capacity a job
*could* legitimately declare and a scheduler legitimately match, once more than
one node holds it; it sits node-side today for economic reasons rather than on
principle, and it is not free of consequence either, since guest-to-host escapes
are a live CVE class and the grant is deliberately narrow. A docker socket is
root-equivalence on the node and so may **never** become a job-type field. Both
halves fail closed at the node: with no allow-list entry there is no device, no
mount and no socket, so a misplaced launch runs *without* the capability and
fails loudly at the command that needed it, never silently holding it. A tenancy
or per-project-user refusal goes further and refuses the launch outright.

Two consequences worth designing for rather than discovering:

- **Advertising the grant set is rejected**, so that placement could avoid a node
  that will refuse. It would put the operator's allow-list on the wire, and the
  moment placement filters on a grant, a filtering bug reads as a grant.
- **A requirement can therefore be grant-gated as well as capability-gated**, and
  capability matching alone never makes a grant-gated task safely unpinned: the
  launch is placed on a capable node and refused *there*, after placement rather
  than before it — and a grant refusal is a hard launch error, not a requeue.
  What keeps this latent rather than live is that each grant-gated class has one
  node today; the pins stay until a second one exists.

**An exclusive-resource lease is a primitive this model deliberately does not
have.** Container-mode device work does not need one, because the per-container
and read-only parts of it namespace cleanly and the only genuinely shared thing
left is memory, which is already a declared resource the runtime enforces. A
lease earns its cost only where a task mutates machine-global state a container
cannot namespace — a macOS simulator is the live case. The capability field for
it is advertised empty by every node and read by nothing. One residual travels
with the argument: if a container's device toolchain ever needs *write* access
into its shared tree, the namespaces-cleanly premise lapses — and the answer
there is a per-container copy-up of the mutable subset, still not a lease.

One consequence worth stating outright, because it is easy to lose: container
monitoring, agent runs and capacity RPCs *are* concurrent. They are made safe
not by locking but by shape — each runs off the actor thread and reports its
result back as one of the six events in §2. Sequential state transitions and
concurrent I/O are not in tension here; the event alphabet is the seam that
separates them.

---

## 7. The environment boundary

Four ports stand between the machine and the world. A reimplementation must
stub all four to test anything, so their promises matter more than their
signatures.

### `ContainerBackend` (`crates/container/src/lib.rs`)

**Promises:** launch a container and return an opaque id; block until exit and
return the code; kill; inspect (`None` when not found); read logs; copy a file
out; find a file by name under a directory; remove an exited container; list
managed containers that have exited or are still running, across every node.

**The port has no pull.** `launch` presumes the image already exists on the
node: the launch config carries no registry, no auth and no pull policy, and the
Docker implementation creates the container with no preceding fetch. So a
missing image surfaces as a launch failure, which fails the task and spends a
retry. A reimplementation must therefore keep apart the image a task *runs
inside* — a launch precondition on the platform's critical path — from an image a
job *produces*, which is an artifact whose absence fails one job loudly in its
own log. Conflating the two is the first mistake available here.

**Allowed to fail at:** anything, as a typed `BackendError`. One failure is not
an error but a *decision input* — `BackendError::NoCapacity` means the fleet had
no free slot, and the correct response is to queue the launch rather than to
fail the task. Everything else keeps fail-the-task semantics. A node that cannot
be listed during a fleet sweep is skipped rather than failing the sweep, so one
unreachable node never blocks the others.

**The criterion for classifying a new failure is whether it can clear without a
human.** A condition that converges on its own belongs with `NoCapacity`, where
the launch queue retries it under a bound; a condition that can never clear
without a config change must be a hard launch error, because queueing it buys a
long silence with a known answer. A tenancy refusal is of the second kind and
must never be spelled as the retryable one. A *missing capability* splits: when
no node in the fleet advertises the mode, the environment, or the ability to
bound a declared limit, the launch queues as ordinary capacity pressure — a node
can acquire the capability — but a pin carrying that launch to a node lacking it
is refused hard, because a pin never falls back. Raising the queue's single
bound to cover a slow-converging condition would degrade the wedged-fleet
diagnostic that bound is sized for; the answer is a distinct queue reason with
its own longer bound, not a larger shared one.

**A refusal to serve one *kind* of work is raised at launch, never at boot.** A
node that cannot serve agent-shaped launches — no agent CLI, no runnable channel
binary of its own — refuses those launches by name and keeps every other slot it
has. Refusing at boot would take the node's whole capacity down for a capability
most launches never need. A precondition for serving a *mode at all* is the
opposite case and is refused at boot, before the node ever advertises the mode:
a supervision unit it cannot create, or a slot count it cannot honour.

**Ordering and idempotence the core assumes:** `remove` is idempotent — an
already-removed container is success. Log byte offsets are stable (container
logs are append-only), so a poller advances monotonically by passing back the
returned offset, and the same offsets address the harvested log file after exit.
Order is preserved *within* stdout and stderr but **not across them** — this is
measured, not assumed, so do not build a feature on interleaving. `logs` is an
after-exit read; the bounded tail read is the one usable while running. Read
everything you need *before* `remove`: the container's logs and filesystem
vanish with it.

That idempotence is a promise the **port** makes, not one a runtime gives. Under
a container daemon "delete the task" is atomic by construction; any other
runtime holds an ordinary directory with two writers — a reaper still writing an
exit status while a sweep unlinks. Such a backend must therefore **detach before
deleting**: rename atomically to a sibling name the task-id parser rejects, then
delete the renamed path, and sweep leftover detached trees at construction. Get
it wrong and a loud "leaked disk" is reported when nothing leaked. The failure is
invisible unloaded and deterministic under load, which is why a full test run
finds it and a targeted one does not.

**Capacity is observation-derived, not ledgered.** A node counts its own running
managed containers and reports one number; occupancy is rebuilt from what the
backend reports rather than from in-memory bookkeeping. The core's reservation
count is *not* a ledger — it covers launches this dispatcher has placed but whose
containers the node's count does not yet report, and is released when the launch
call returns. It is an assume-cache over the observation lag. A reimplementation
that adds a second, genuinely ledgered dimension beside the observed one has put
two sources of truth inside a single placement decision.

**A node's own managed scan fails loud, never empty.** A scan that cannot read
its root, or cannot read one entry, returns a typed error and never a partial
success — occupancy blindness is detected *only* by that call erroring, so a
node-local implementation that swallows a scan failure reports a busy node as
idle. The fleet-level aggregation over many nodes is the deliberate exception
noted above: an unreachable node is skipped and flagged out of service rather
than failing the sweep for every other node.

Which way an entry the scan cannot make sense of should fall is a real choice
with real costs — failing closed toward occupied wastes a slot, failing open
reaps live work — and the two listings answer it at different grains. An entry
whose *identity* is unreadable still counts as **running**, because it occupies
a slot whatever it belongs to. An entry whose *liveness* cannot be established —
no exit code, no live-set claim, no matching process — is reported terminal
instead, because reporting a dead task as running hangs it until its timeout
rather than failing it loudly.

### The paths above the port are wire paths

`/workspace` and `/chuggernaut` are **wire paths, not host paths**. Everything
above the port names them literally — the clone destination, an injected file's
path, the copy-out and find-by-name arguments, and the environment *values* that
embed one — and each implementation decides what they address: a container
backend takes them literally, a host backend maps them into a per-task directory
and rebases the embedded values by the same substitution. That value rewriting
runs against a **closed allow-list of variables** known to carry a wire path; a
wire prefix beginning a path in the value of any *other* variable is refused
rather than rewritten, which is what catches the next consumer interpolating one
instead of letting a credential silently point at nothing. An occurrence that
continues a longer segment — a repo URL ending `/chuggernaut.git` — names no
wire path and passes through untouched.

The mapping is **total in both directions**. A path outside both prefixes is a
hard error naming it, never a fall-through to the node's own filesystem; and a
real path the reverse mapping cannot express as a wire path is refused rather
than returned raw. Resolving these prefixes *below* the port is what makes a
host backend possible at all; a reimplementation that resolves them **above**
it, letting callers compute real node paths, can only ever run a task in a
container.

One environment fact no stub models, and it bites the first time a backend
composes a bind host-side: on macOS there is no Linux kernel, so the container
daemon runs inside a VM and **a bind's source path is resolved by the daemon,
which means inside the VM, not on the host**. A host path the VM does not share
produces no error — the daemon creates an empty directory at that path inside the
VM and binds *that*, so the mount silently carries nothing. Treat "the VM shares
this prefix" as a declared node fact, never an inferred one.

### Executing without a daemon that remembers

Where nothing external remembers exit statuses, **the authority for a task's exit
status is the task, never the supervisor that launched it.** A supervisor that
reaps its own child loses the code across a mid-task replacement, and a liveness
rule then reports a task that succeeded as failed. The guarantee protecting
in-flight work differs by mode, and neither half covers a reboot: container work
is drained across a supervisor swap, while a supervisor holding a live *host*
task **refuses** the swap outright, naming the task.

So the launch is wrapped to write its own status into the task's directory,
atomically, after the command returns; the supervisor's reaper is only a backstop
and an in-memory live set covers the window before the file lands. **A task
directory never transitions out of having an exit code, and nothing outside the
supervisor and the task it launched ever writes into that directory.** Liveness
for a directory without one is answered by that live set first — the tasks this
supervisor instance spawned and has not yet written a code for — and then, for
anything it does not claim, which is the case a supervisor restart creates, by a
**(pid, process start time)** pair, because a pid is recycled where a container
id is stable. A task answering neither is reported terminal under a synthetic
failure code, since reporting it as still running hangs it until its timeout
instead of failing it loudly.

Two refinements the design corpus specifies and the tree does not carry: a
**boot generation** recorded at launch, which closes the reboot case with no pid
reasoning at all, and **persisting** that synthetic verdict once so the
directory becomes self-describing. Today it is recomputed on every inspect —
stable per call, but not durable. <!-- intent -->

### Bounds are per-mode, and one whole class is unbounded

Resource enforcement is advertised as a single node-scoped capability but
**read per the launch's resolved mode**: a node serving both runtimes answers
for its container launches and never for its host ones, which are bounded by
nothing. The advertisement is the coarser of the two and is an accepted wart —
a dual-mode node advertises enforcement as true while every host task it runs is
unbounded — so the bit is honest only once the predicate narrows it, and an
operator reading the fleet view directly is reading the container answer. A
launch declaring cpu or memory limits is placed only on a node that can bound it
— queued as ordinary capacity pressure when none can, and refused hard, naming
the field and the node, when a pin carries it to one that cannot.

**The live constraint that follows: a job type that runs host work must declare
neither a cpu nor a memory limit.** Only the dispatcher-side task timeout bounds
a host task, and that is mode-independent.

**A known gap, stated because it is invisible otherwise:** declared cpu and
memory limits reach **command** launches only. The agent provider builds its
launch config with both unset, so an agent work container has never been bounded
by either field on any node, whatever its job type declares. The placement
predicate above is correct either way and simply never fires for an agent launch.
Closing it changes what real jobs may consume, so it is a fleet-capacity decision
rather than a bug fix.

### A launch call has a budget, and node-side work must fit inside it

Any work the backend does on the node *before* the container exists has to fit
inside the launch call's own budget. When it does not, the caller abandons the
call with a transport error — which is **not** the retryable capacity signal, so
the task fails rather than requeues — while the node finishes the work and
launches anyway, leaving a container running for a task already failed, under an
id the core never recorded. A pre-launch step therefore takes its bound from the
call's budget minus a reserve for the create and the reply, and a larger value is
refused when the config is parsed. A bound the call cannot contain converts a
loud, named refusal into a silent orphan.

A node-side resource pinned for one task's lifetime is **named by the task id,
released at exit, and needs a reaper of its own**: the fleet sweep reconciles
containers, not the resources beside them, so a container it removes leaves the
pin behind. Naming the pin by task id is what makes a crashed supervisor's leak
greppable rather than invisible. The reaper is best-effort — a failed cleanup
leaks disk and must never fail a job.

### Why these are ports and not enum variants

A port earns its place through its **contracts** rather than its vocabulary. The
transient-versus-fatal split, log-offset monotonicity and remove-idempotence are
what the core's behaviour is written against, so a second implementation that
satisfies the port satisfies them *by construction*, where an inline variant
satisfies them only by review.

Two of the promises above are properties of the first implementation rather than
requirements on any: the cross-stream ordering caveat is vacuous for a backend
with a single merged stream, and what `remove` reclaims need not be an overlay. A
reimplementation should name each port after its contract, not after the first
thing that implemented it.

### `AgentProvider` (`crates/agent/src/lib.rs`)

**Promises:** run an agent container to completion and return its output; report
which channel mode it supports.

**Allowed to fail at:** launching or running, as a typed error. Note the
asymmetry that shaped the event alphabet: the provider **erases** `NoCapacity`,
which is why a spawned agent launch signals capacity pressure back as
`Msg::LaunchDeferred` instead of the caller seeing it inline.

**Ordering and idempotence:** the container id arrives through a one-shot
side-channel the instant the container launches, *before* the blocking wait,
because the provider only surfaces it in the output after exit. Firing it is
best-effort — a provider that launches no container never calls it, and
reporting more than once or never is harmless. A reimplementation that skips
this leaves `container_id` null for the whole run and cannot kill a container it
launched.

### `store` (`crates/store/src/lib.rs`)

**Promises:** typed accessors over NATS KV and streams — `JobStore`,
`TaskStore`, `ProjectStore`, `CounterStore`, `RdepsStore`, `StepStore`, plus
secrets, vars, artifacts and the worker RPC surface, and durable event publishing.

**This is the only crate that talks to NATS.** That is the whole point of the
boundary: the KV and stream surface is reachable in exactly one place, so a port
swap is a rewrite of one crate rather than a search across the workspace. A
reimplementation gets the same benefit only if it holds the same line.

**Allowed to fail at:** every call, as a typed store error. Note what it does
*not* offer: no transaction, and no compare-and-swap except where a port
explicitly exposes one (`AdvanceDefault` on the VCS side). The single writer is
what makes that acceptable — there is no second writer to race, so the code
needs no CAS and must not grow one as a substitute for the single-writer
property.

**Ordering and idempotence:** KV puts are last-write-wins, which is safe only
under the single-writer rule; `CounterStore::next` is the id allocator, so a
reimplementation must not pre-allocate ids speculatively (allocating one per
scan tick to see whether a schedule is due would burn ids for the overwhelmingly
common "nothing is due").

**The worker RPC surface is request-reply, and it must stay off the durable
path.** A launch request carries the task's decrypted secrets and is sent as a
request under a deadline — nothing lands in a stream, so a launch payload has no
durable copy, no replay and no retention. The same crate's *event* path does
persist, and the difference is deliberate: a reimplementation that routes
launches through the event stream to reuse its delivery guarantees gives every
secret a retained copy. Confidentiality on the wire rests on the private network,
not on the transport.

### `vcs` (`crates/vcs/src/lib.rs`)

**Promises:** bare-repo management and git operations — no working tree exists
anywhere. Branch operations are ref updates; a squash-merge is a written tree
plus a commit object. The operations the lifecycle depends on are creating and
resetting a branch, resolving a ref, squash-merging, building a squash
candidate, advancing the default branch under a CAS, rebasing with conflicts
reported as data, and deleting a branch.

**Allowed to fail at:** git plumbing and repository I/O, as a typed error — and
this is precisely the failure the wrap-up path escalates on rather than
retrying. Two outcomes are **not** failures and must not be modelled as such: a
merge conflict is data the conflict-rework decision consumes, and a refused
compare-and-swap on the default branch means "HEAD moved again", which
re-enters the landing decision.

**Ordering and idempotence:** the single writer serializes all mutations, and
concurrent reads are safe. A squash-merge with no commits beyond the base is a
no-op rather than an error. The default-branch advance is the one genuine CAS in
the system: it takes the expected old head, and its refusal is a decision input.

---

## Related

- [`docs/spec.md`](../spec.md) — normative behaviour; §2.1 owns the transition
  table this page maps.
- [`docs/reference/design-lifecycle.md`](design-lifecycle.md) — the generalized
  phase vocabulary this page implements.
- [`docs/reference/contracts.md`](contracts.md) — why the interfaces are shaped
  this way, and the formalization ratchet.
- [`docs/reference/crates.md`](crates.md) — which crate owns what.
- [`docs/reference/modules.md`](modules.md) — the per-module contract lines jobs
  are scoped against.
- [`docs/reference/testing.md`](testing.md) — which tier a test for any of this
  belongs at.
- [`docs/README.md`](../README.md) — the catalogue, and the target factoring
  this machine is refactoring toward.
