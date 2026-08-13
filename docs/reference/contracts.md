# Dispatcher contracts — formalizing the interfaces

Companion to `docs/README.md` (target factoring) and `docs/reference/structure-assessment.md`
(current-state audit). This document answers: what are the high-level
interfaces inside the dispatcher, which contracts exist today (explicitly or
implicitly), and how do we extract, formalize, and enforce them so they can
guide future implementation — in Rust or Python — toward the north star.

## The starting fact: between dispatcher modules, there are no interfaces today

The dispatcher's ~24 files are not modules with interfaces — they are
**namespaces over one shared object**. Eleven files contain `impl Core` blocks
(`core`, `exec`, `eval`, `channel`, `fleet`, `launch_queue`, `cd`, `origin`,
`reconcile`, `scan`, `triage`), each with full mutable access to all of
`Core`'s state. `eval` calling into `exec` is not crossing a boundary; it is
one object talking to itself. So the task is not "formalize the existing
interfaces" — it is "create them," and the extraction work below is how we
find out what they should be.

The code is far from contract-free, though. There is a clear formality
gradient:

**Already formal (keep — these are the model):**

- `state.rs::assert_transition` — transition legality as a total function,
  zero I/O.
- `core.rs::set_state` — a genuine single funnel: "§2.1 guard, then KV, then
  memory," with terminal-state stamping done once there.
- The ports: `ContainerBackend`, `AgentProvider`, `store`'s typed accessors.
- The wire surface: each `Msg` variant carries a doc comment naming its
  `req.*` subject and spec section.

**Semi-formal:**

- The `Msg` enum itself (30 variants — 24 commands carrying a `Reply`, 6
  events; enumerated in `docs/reference/lifecycle-model.md`) is the *actual*
  high-level interface of
  the entire dispatcher: every input — HTTP-originated request, container
  exit, timer tick — becomes a `Msg`. It is a protocol in all but name: shapes
  are typed, but pre/postconditions and error semantics ("409 while an attempt
  is in flight") live in comments and `CoreError` conventions.

**Implicit and at-risk:**

- The invariants. "Terminal states are absorbing." "The queue holds only
  Ready jobs." "One attempt in flight per job." "rdeps is the inverse of
  deps." "Merge queue is depth-1 per project." These exist as comments,
  defensive code (`get_or_insert_with`), and assertions scattered through
  ~12k lines of integration tests — nowhere as a checkable statement.

## The three contract vocabularies

The decision-making process for a job decomposes into three kinds of
statements; each wants a different artifact.

### 1. Commands and events — what can happen

Split `Msg` conceptually into **commands** (external requests carrying a
`Reply` — claim, release, resolve, …) and **events** (facts: `TaskExit`, scan
tick, ingest arrival). For each variant: precondition (which states/conditions
it is valid in), postcondition (state written, effects emitted), and error
taxonomy. Most of this is already written in the doc comments — it needs
pulling into one place. Because `Msg` mirrors the wire, this doubles as most
of the NATS protocol spec.

### 2. Effects — what the system does about it

The missing vocabulary, and mechanically extractable: the ~460 `.await` sites
in `eval`/`exec`/`core` *are* the effect catalog. Classify each await into a
variant — `PutJob`, `LaunchContainer`, `SquashMerge`, `CreateTask`,
`PublishEvent`, `IssueCredentials`, … likely 20–30 total — and the real
interface between "deciding" and "the world" is enumerated. Once the Effect
enum exists, the north-star decider signature falls out:

```
decide_<phase>(view of state, event) → (transitions, [Effect])
```

Each phase decider (ready, work, eval, merge_gate, wrapup, escalation,
triage) becomes a named module with a named contract. **That is the
inter-module interface structure the dispatcher currently lacks: modules stop
sharing `&mut Core` and start exchanging values.**

**Status: landed (B2 + C1).** The vocabulary is
`chuggernaut_domain::effects::Effect` (28 variants, grouped in
`docs/reference/lifecycle-model.md`) with the interpreter in
`dispatcher::interpret` (`Core::interpret`) — the sole `&mut Core` coupling
deciders keep. The first decider is `chuggernaut_domain::decide::escalation`,
the C1 template every later phase copies (its shim: `Core::run_escalation`).
Two refinements the template settled:

- `transitions` is first-class — `Vec<Transition>` (the decision-stamped job
  record + target state) — and the shim applies transitions through
  `Core::set_state` **before** running the effects: the §2.1 record is the
  committed decision, .chug/tasks/events are its downstream artifacts. A crash
  between the two is healed by restart reconciliation re-deriving the
  artifacts from the stamped record (`heal_missing_escalation_task`) —
  recovery owns crash consistency, so deciders never encode write
  choreography.
- Reads (`next_task_id`, the clock, the active cycle) are **not** effects:
  the shim gathers them into the decider's read-only view.

**C2 (merge_gate) extended the template with the continuation contract:** an
effect whose result the decision needs (`SquashMerge` → outcome,
`AdvanceDefault` CAS) is emitted and the decider TERMINATES; the interpreter
returns the result as an `Outcome`, and the shim re-enters `decide` with it as
the next event against a freshly gathered view — a decision never runs on a
view the world moved under. Phase-owned scheduling state became a
decider-owned value (`MergeGateState`, swapped wholesale by the shim), which
promoted the depth-1 invariant into the type system (`gating: Option<u64>`).

**C3 (wrapup) settled the third piece: the step.** A decider returns, besides
transitions and effects, a value naming the shell bookkeeping that follows
(`WrapUpStep` — release the execution slice, unblock these seqs, re-enter with
this event). Work the pure crate cannot express — because it reads dispatcher
state that is not part of the decision — is *named* by the decider rather than
left implicit in the shim, so the shim keeps no branching of its own.

**C4 (ready) put the two together and found the gate.** The Ready phase
(`chuggernaut_domain::decide::ready`, shim `Core::run_ready`) is decided by four
events — `Released`, `DepsChanged`, `Revalidated`, `Dequeued` — and its
`ReadyStep` uses C2's continuation *to keep expensive I/O behind the decision*:
`DepsChanged` decides eligibility only, and the §2.2 Ready-transition
re-validation (ref reads, config loads) runs solely because the decider returned
`Revalidate`, its verdict re-entering as `Revalidated`. That inverts the usual
reason for the continuation contract — C2 emitted an effect because the decision
needed its result; C4 emits one so the effect never runs for a decision that was
not going to move — and it is the shape every later phase with a costly guard
should copy. The step also gained an ordering position: `Admitted`'s queue
admission and batch-membership commit run *between* the transitions and the
effects, because admitting a job to the ready queue is part of committing the
§2.1 record, not an artifact of it.

**C5 (eval) showed what a decider does with a phase's own working memory.**
The Evaluation phase (`chuggernaut_domain::decide::eval`, shim
`Core::run_eval`) decides the staged fan-out, each evaluator type's verdict,
three budgets (`eval_retries`, the evidence-free relaunch cap, `rework_budget`)
and the reduce's pass / rework / abort / escalate fork. The round it decides
over — which slots are in flight, which stages have not been created, which
already passed — became a **decider-owned value** the shim swaps per decision,
the same move C2 made with `MergeGateState`, which puts "one stage in flight
per job" in the type rather than in a comment. Two consequences worth copying:

- A launch effect's *identity* comes back as an event. Task ids exist only once
  the launch ran, so `LaunchEvalStage`/`LaunchEvaluator` re-enter as
  `StageLaunched`/`SlotRelaunched` and the decider is what lands them on the
  round — the shim never reaches into the value it handed over.
- A read the decision *might* need must be gathered before the branch that
  needs it is taken. The evidence-free relaunch count was a read-after-write on
  one branch of the old `eval_no_output_failure`; as a view field it is read on
  every eval exit and counts the retirement the decider has not emitted yet.
  Where C4 uses the continuation to keep an expensive read behind the decision,
  a cheap read simply moves into the view.

**The launch effects carry no placement, and that is a contract.**
`LaunchWorkTask`, `LaunchWrapupTask` and the eval-stage launches name a job, a
cycle and an attempt — never a node. Placement is composed on the far side of
the port, from exactly one source:

> **`ContainerLaunchConfig.node` is a pure function of the resolved job type.**
> Nothing on the `Job` record participates.

Three sites compose it today, each
`node: job_type.placement_node().map(String::from)`.
`crates/dispatcher/src/exec.rs` and `crates/dispatcher/src/eval.rs` set
`AgentRunConfig.node`, which the Claude provider copies verbatim into the launch
config it hands the backend (`crates/agent/src/claude.rs`), and
`crates/dispatcher/src/launch_queue.rs` builds a `ContainerLaunchConfig` directly
for command tasks. (`crates/worker/src/daemon.rs` rebuilds one node-side with
`node: None`; by then the placement decision has already been made.)
`JobType::placement_node(&self)` takes no `Job`, so the contract is nearly
self-evident from the signature — it is written down because a *fourth* site
could be written otherwise and nothing in the tree would catch it. The tier-1
property test behind [#311](../design/311-job-inputs.md) Decision 1,
`resolved_job_type_is_equal_for_any_two_input_maps`
(`crates/domain/src/release.rs`), guards **job-type resolution**, which a
launch-site read of `job.inputs` bypasses entirely
([#361](../design/361-per-run-placement.md)). So this is a **reviewer-enforced**
contract by design: #361 considered a mechanical check and rejected it — catching
this properly needs taint analysis, and a grep gate over a struct-field
initializer would match a coding *shape* where `.chug/tasks/check-comments.sh`
and `.chug/tasks/check-duplication.sh` match a lexical *fact*. Placement is a
fleet decision (`docs/spec.md` §3.1); a second source for `node` is a design, not a
call site. Reject one by name.

**Why the refusal is this firm.** Placement sits at the two stronger of the
three authorities that touch a run: which node a job type's containers land on
is a repo file, so
changing it costs a merge through the full evaluator and merge-gate path, and how
much capacity a node offers — including drain to zero — is a platform-admin act.
Creating a job, by contrast, is available to any member, and a triage container
may do it from inside a container whose input is externally-authored issue text.
A per-run node field would move a fleet decision to the weakest authority in the
system, and to a non-human one.

**Drain beats a pin**, which is what bounds the damage a pin can do. A launch
pinned to a node an operator has drained to zero sees no free slot, takes the
retryable capacity path and joins the queue — it never lands. So a *per-run*
pin, were one ever added, could not defeat drain, could not exceed a node's slot
count, could not spill onto an unintended node and could not land on an
out-of-service one. The residual risk such a pin would carry is co-location and
attention-steering, not capacity capture. The bound is stated about the
mechanism this section refuses to add, so that the refusal rests on the right
argument rather than on an overstated hazard.

**Capability is the currency routine placement should use; identity is the
escape hatch.** A constraint can only *shrink* the candidate set, leaving the
choice within it to policy and live load, where a name removes policy, load and
headroom from the decision at once. A node asserts a capability about itself — a
boot-time fact derived from its own configuration, with every absent field read
closed — and a job creator cannot forge one, whereas a node name is whatever
string was typed and can only be shape-checked, because the fleet list lives in
the dispatcher's environment and cannot be validated offline. On a fleet where a
class has exactly one member the two coincide, and three differences survive
that coincidence: a capability degrades correctly when the class grows, it
records the *reason* rather than the target, and it is the **node** that asserts
membership rather than the job creator — which is the authorization difference.

**A launch's required mode is read off the launch config itself, never off the
job type it came from.** A job type may declare host mode while an evaluator
level it carries has its own image and is container work, so the job type's mode
is not the launch's mode. Threading a resolved mode down from the dispatcher was
rejected for creating a second answer to one question — a placement that believed
it would route a container evaluator onto a host-only node. One selector, read
the same way by placement and by the node's own refusal, is what makes a
wrong-mode refusal unreachable rather than merely unlikely. The job type's own
resolved mode keeps its other job — deciding which fields its YAML may carry;
what is read off the launch is only *where the launch goes*.

**Capability matching has two halves, and pricing one prices half the feature.**
A capability match is an **advertisement plus a matcher**. Nothing advertises a
device today, so a node must first be made to say it holds one before any
predicate has something to read.

Two constraints hold whatever encoding is chosen. The advertisement is
**additive and absent-means-nothing**, so it moves no `WORKER_RPC_VERSION` —
but the matcher's job-type field still costs a `CONFIG_SCHEMA_EPOCH` bump, and
that is the half worth pricing. And the advertisement is a **discovered** fact
rather than an operator assertion, or it becomes a second grant surface: it
would publish the operator's allow-list onto every ping payload and into the
fleet view, and it would invite the scheduler to enforce what only the node may
enforce. One matcher with a second source list — never a second matcher.

### 3. Invariants — what must always hold

Harvest every "must"/"always"/"never" comment and defensive pattern into one
list, then make it executable: a `check_invariants(&CoreState)` function run
after every message in tests. This is cheap precisely because of the
single-writer design — all state lives in one place. It converts tribal
knowledge into a regression net, and it is the artifact that survives a
language change untouched: invariants are statements about *data*, not code.

**Status: landed (B1).** `dispatcher::invariants::check_invariants(&CoreState)
-> Vec<Violation>` is now the source of truth for this list — a pure, total
function harvesting the data invariants below. `Core::state()` hands out the
read-only `CoreState` view it takes; the `lifecycle` integration tests run it
after every message (`assert_invariants`). Add a new invariant *there*, not to
a comment. The invariants enforced (each named, with its spec §):

- `ready_queue_only_ready` (§3.1) — the ready queue holds only jobs that exist
  and are `Ready`.
- `rdeps_inverts_deps` (§1.4/§2.3) — the reverse-dependency index is the exact
  inverse of the forward `deps` edges, both directions.
- `active_is_executing` (§3.2/§3.3) — an execution slice exists only for a job
  that is executing (Work/Evaluation/WrapUp/Escalated); "one attempt in flight
  per job" is structural (the `active` map is keyed by seq).
- `merge_queue_is_wrapup` (§3.3) — every queued/gating landing job is `WrapUp`,
  and a gating seq has left the queue; "merge gate depth-1 per project" is
  structural (the `gating` map is keyed by slug).
- `terminal_is_absorbing` (§2.1) — no terminal (Done/Revoked) job is still
  referenced by the ready queue, active set, merge queue, or gating map.
- `one_live_job_per_schedule` (§1.1 schedules) — at most one non-terminal job
  carries a given schedule's provenance in a project; the backpressure the skip
  rule exists to provide, stated as data.

## Mining intent from the existing code

- **The await sites → the Effect enum.** Mechanical classification; days, not
  weeks.
- **The tests are the richest contract corpus, in the wrong form.** Each
  integration test encodes "given this setup, when this happens, these states
  and artifacts result." Convert the highest-value scenarios into **golden
  decision traces**: data files (YAML) of
  `{initial state, incoming event, expected transitions, expected effects}`.
  Generate them by instrumenting the Rust dispatcher (log every `Msg` in,
  every `set_state`, every effect out during a test run) rather than writing
  them by hand. The traces are the keystone artifact: language-neutral,
  diffable, and they serve both futures — in Rust they pin behavior during
  decider extraction; in Python they are the conformance fixtures. One
  artifact, both roads.
- **The doc comments with spec § refs → per-module contract headers.** The
  discipline already exists; it needs a home and a completeness check (the
  docs/reference/modules.md registry rows should point at these).
- **Serialize the vocabulary itself.** `Msg`, the Effect enum, `JobState`,
  and the KV record shapes are all serde types — emit JSON Schema from them
  and the formal interface definition becomes a build artifact, not a document
  that drifts. (`docs/README.md` §2, applied inward.)

## The formalization ratchet — increasing strength

1. **Documented** — contract headers per module: accepts / emits /
   guarantees / spec refs. Cheap; do immediately.
2. **Type-enforced** — deciders take a read-only view of job/graph state, not
   `&mut Core`; `Core`'s fields go private and the `impl Core` sprawl shrinks
   as each phase migrates. (Python equivalent: `Protocol`s + frozen
   dataclasses — same shape, weaker teeth, which is why layers 3–4 matter
   more there.)
3. **Executable** — the invariant checker after every message; property tests
   (random legal command sequences never violate invariants — the
   single-writer loop makes these easy to drive); golden traces in CI.
4. **Cross-implementation** — the wire-level conformance suite and
   shadow-mode decision diffing (see the Python-rewrite assessment): a second
   implementation's deciders consume the same event stream read-only and
   their `[Effect]` output is diffed against what the live dispatcher did.

## The working rule that makes it stick

**Contract-first change rule:** any job touching the dispatcher must name the
contract it changes — a `Msg` pre/postcondition, an Effect, an invariant, a
trace. If a change cannot be expressed that way, the contract it needs does
not exist yet, and writing it becomes the first commit of the job. This rule
is also precisely what makes module-scoped agent jobs safe: **the contract is
the scope.**

## Sequencing (no big-bang)

1. **Invariant checker + contract headers** — days, pure gain, zero
   restructuring.
2. **Effect catalog** from the await sites.
3. **Trace-generation instrumentation**; goldens for the lifecycle tests.
4. **First decider extraction** (`merge_gate` or `escalation`) behind those
   traces.
5. **Schema emission** for the vocabulary types (`Msg`, Effect, records).

After step 4 there is one fully-formalized phase to serve as the template,
and each subsequent job can convert another.
