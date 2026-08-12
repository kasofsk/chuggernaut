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

A second, narrower disagreement sits inside `docs/spec.md` itself and points the
same way. §2.1's rows and the code agree that a failed Ready-transition
re-validation and a failed launch-time validation park the job **`Stalled`** —
`Effect::Stall` targets `JobState::Stalled`, and `crates/domain/src/decide/ready.rs`
and `crates/domain/src/decide/work.rs` both emit it. §2.2's prose says
"transitions to Escalated" for both. The table and the code are consistent; the
prose is the outlier.

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
  suppression.
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

**Allowed to fail at:** anything, as a typed `BackendError`. One failure is not
an error but a *decision input* — `BackendError::NoCapacity` means the fleet had
no free slot, and the correct response is to queue the launch rather than to
fail the task. Everything else keeps fail-the-task semantics. A node that cannot
be listed during a fleet sweep is skipped rather than failing the sweep, so one
unreachable node never blocks the others.

**Ordering and idempotence the core assumes:** `remove` is idempotent — an
already-removed container is success. Log byte offsets are stable (container
logs are append-only), so a poller advances monotonically by passing back the
returned offset, and the same offsets address the harvested log file after exit.
Order is preserved *within* stdout and stderr but **not across them** — this is
measured, not assumed, so do not build a feature on interleaving. `logs` is an
after-exit read; the bounded tail read is the one usable while running. Read
everything you need *before* `remove`: the container's logs and filesystem
vanish with it.

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
