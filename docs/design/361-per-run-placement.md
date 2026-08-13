# Design — Per-run placement: how a run picks its node (gap 10)

Status: FINDING — gap 10 needs no new Job-record field; #311 Decision 1 stands unamended and no work is opened.

Closes [#308](308-gha-port.md) gap 10, which #308 opened and deliberately
declined to decide. Verified against the tree at `c73d76b` (2026-08-01); where a
sibling doc disagrees with the source, the source wins and the disagreement is
recorded in [Corrections](#corrections-verified-against-the-tree). The **beacon**
half is different and is marked as such throughout: `~/beacon` is not checked out
in this workspace, so nothing here re-derives it — the primary evidence is the one
`runs-on:` expression [#308 §A3](308-gha-port.md#a3-beacon-already-parameterizes-placement-per-run)
recorded verbatim from the operator's 2026-07-30 inspection.

Two job types pin today, `android-proof` and `docker-proof`
([#543](543-placement-granularity.md)); the body's table row and correction
saying none does date from 2026-08-01 and are superseded on that point alone.

## The question

[#308 §A3](308-gha-port.md#a3-beacon-already-parameterizes-placement-per-run):
thirteen beacon jobs choose their runner from a `workflow_dispatch` input. The
obvious port — a job input that selects the node — is forbidden: [#311](311-job-inputs.md)
Decision 1 classifies `placement.node` as **never** selectable by an input, on
the ground that "placement is a fleet fact; an input naming a node lets a job
creator pick which host runs project code."

So: **does Chuggernaut need a per-run node-selection mechanism, and if so, what
is its currency and where does it live?**

The answer this document reaches is that gap 10 is smaller than #308 assumed.
The evidence supports it, so it is also the cheapest possible outcome: no `Job`
record change, no schema epoch bump, no weakened invariant, and the two things
that *do* need doing are already scheduled inside
[#309](309-host-native-execution.md).

## What is true today (verified in this tree)

| Fact | Where | State |
| --- | --- | --- |
| `placement: { node }` is a **job-type** field, repo-versioned, resolved at `base_ref` | `crates/types/src/job_type.rs` (`Placement`, `JobType::placement_node`) | Shipped |
| The pin threads to launch through `ContainerLaunchConfig.node` at exactly three sites | `crates/dispatcher/src/exec.rs`, `crates/dispatcher/src/eval.rs`, `crates/dispatcher/src/launch_queue.rs` — each `node: job_type.placement_node().map(String::from)` | Shipped |
| Placement itself is a pure function of policy + probed candidates + optional pin | `crates/container/src/lib.rs` (`choose_placement`, `PlacementCandidate`, `NodeLoad`) | Shipped |
| A pin is honored or it fails — never a fallback, never spillover | `choose_placement`; `docs/spec.md` §3.1 | Shipped |
| "**No labels, no anti-affinity**" — the pin is the *only* affinity control | `docs/spec.md` §3.1 | Normative today |
| Slot accounting is **observed from the node**, not ledgered by the dispatcher | `docs/spec.md` §3.1 ("the scheduler reads exactly one number per node"); `crates/worker/src/backend.rs` (`node_load`, `probe_worker`) | Shipped |
| `FleetNode.reserved` is a race patch over that observation, not a ledger | `crates/worker/src/backend.rs` — its own doc comment, and `Reservation`'s | Shipped |
| No-capacity is transient: park `Pending`, `QueuedForCapacity`, no retry budget, FIFO drain, 30-min backstop | `docs/spec.md` §3.5; `crates/dispatcher/src/launch_queue.rs` | Shipped |
| Runtime capacity control, including `slots: 0` as a full drain | `docs/spec.md` §3.1; `crates/api/src/routes.rs` `platform_fleet_capacity_set`; `crates/dispatcher/src/handlers/fleet.rs` | Shipped |
| Job creation requires **Member+** on the project | `crates/api/src/routes.rs` `jobs_create` → `member_on` | Shipped |
| Fleet capacity requires **`platform_admin`** | `crates/api/src/routes.rs` `platform_fleet_capacity_set` → `platform_admin`; `docs/spec.md` §7.5 "Platform-level config" | Shipped |
| A factory triage **agent** may create jobs from inside a container | `crates/auth/src/nats.rs` `triage_container_permissions` grants `req.jobs.create.{owner}.{project}` | Shipped |
| Inputs never reach job-type resolution, tier-1 tested | `crates/domain/src/release.rs` — `resolved_job_type_is_equal_for_any_two_input_maps` | Shipped |
| `NodeCapabilities` on `PingOk`/`WorkerAnnounce`, ingested per node | [#309](309-host-native-execution.md) §4 (P2 slice 5); `crates/types/src/worker.rs`, `crates/worker/src/backend.rs` | Shipped — advertised, visible, and read by placement's mode predicate (job #484) |
| Capability-aware `choose_placement` | [#309](309-host-native-execution.md) §5a (P2 slice 6) | Shipped (job #484) — a launch is excluded from every node not advertising its mode |
| `placement.leases` | [#309](309-host-native-execution.md) §5b (P4) | Designed, not in the tree |
| **Not one job type in this repo sets `placement`** | `.chug/jobs/*.yaml` — zero matches | The one affinity control ships unused |

That last row is worth pausing on. The platform's existing per-node steering
primitive has been available for the whole dogfooding history and no job type
uses it. That is not proof that per-run steering is unwanted, but it is the
cheapest available evidence about demand, and it points the same way as
everything below.

## Start from the prior art: Kubernetes answered this with labels

The brief is right to insist on this, and the conclusion survives contact with
the source.

Kubernetes answers "how does a workload pick a node" four ways —
`nodeSelector`, `nodeAffinity`, taints and tolerations, and extended resources —
and **all four select over properties a node advertises about itself**. Node
*identity* appears in exactly one place, `spec.nodeName`, which the documentation
and the community both treat as a debugging escape hatch: it bypasses the
scheduler entirely, so it gets no capacity checking, no filtering, and no
retry — the pod simply fails if the node cannot take it. The vocabulary of
routine placement is capability; identity is the fire escape.

The two structural reasons transfer to Chuggernaut unchanged:

1. **A name is a target; a label is a constraint.** A constraint can only
   *shrink* the candidate set — the scheduler still chooses within it. A name
   removes the scheduler from the decision. Everything the platform knows about
   load, health and drain is applied to a constraint and bypassed by a name.
2. **A name goes stale in a way a label does not.** `gumbo` reads as a machine.
   "Has a host docker daemon and a warm buildx cache" reads as the reason, and
   stays true when the machine is replaced, renamed, or joined by a second one.

That is direct, independent evidence for the position
[#309 §5a](309-host-native-execution.md#5a-capability-aware-placement) already
argues from Chuggernaut's own side: `NodeCapabilities { modes, platform,
resources_enforced, leases }`, with `choose_placement` gaining a required-mode
predicate applied before the existing `free <= 0` and out-of-service checks.

### What is explicitly **not** being proposed

**Adopting k3s or Kubernetes as an execution backend is out of scope and is not
on the table.** k8s is cited here only as a design source for the *selection
vocabulary*. The exclusion stands on its own, already-decided grounds:

- [`docs/design/000-rationale.md`](000-rationale.md) picks a fleet of plain Docker daemons as the v1
  production substrate precisely because "placement intelligence lives in the
  dispatcher", and reserves the Kubernetes Jobs backend "for consumers who
  outgrow a small fleet."
- `crates/container/src/k8s.rs` is a six-line stub — a struct and a `TODO` — and
  `crates/container/Cargo.toml` carries no `kube` or `k8s-openapi` dependency,
  only a comment noting that a backend impl would add them.
- A second scheduler underneath a single-writer dispatcher is the
  reconcile-against-a-second-writer problem `docs/design/000-rationale.md` names when it excludes
  workflow engines "at any scale."

Borrowing a vocabulary costs nothing and commits to nothing. A later reader
should not mistake the citation for a proposal.

## What the thirteen beacon jobs actually vary

The brief asks for more than "they vary something", and the primary evidence
answers it. This is the expression #308 §A3 recorded verbatim:

```yaml
runs-on: ${{ inputs.runner == 'cloud' && 'ubuntu-latest' || fromJSON('["self-hosted", "linux", "x64", "gumbo"]') }}
```

Three facts are readable out of that string alone, with no access to beacon:

1. **It is a one-bit switch, not a node picker.** The input is compared to
   exactly one literal, `'cloud'`. Whatever its declared type, the workflow
   consumes a single boolean out of it. Eleven jobs, one bit each.
2. **The self-hosted branch is a label list, not a name.**
   `["self-hosted", "linux", "x64", "gumbo"]` — three of the four entries are
   plainly capability labels (self-hosted-ness, OS, architecture). Only `gumbo`
   is identity-shaped, and in a fleet where gumbo is the only self-hosted
   linux/x64 runner it is a **class of one**: a label that happens to have one
   member, not a machine address.
3. **The hosted branch cannot name a machine even in principle.**
   `ubuntu-latest` is a runner *class*; GitHub gives no vocabulary for naming an
   individual hosted VM.

So **neither side of the toggle names a machine.** Beacon is already selecting
by label at both ends. The brief's hypothesis — that beacon names a runner
because GHA gave it no other vocabulary — turns out to understate the case: GHA
*did* give it a label vocabulary, beacon used it, and the single identity-shaped
token in the expression is a class of one.

The two macOS jobs are recorded in #308 §A3 as doing "the same for macOS"; their
literal expression was not captured, so their exact shape is **unverified**.
Whatever it is, the axis is a platform property.

### Decomposing the bit

What does the toggle actually select between? `ubuntu-latest` is ephemeral,
clean, generic, metered, and infinitely parallel. `gumbo` is (per
[#308](308-gha-port.md)'s survey) a persistent NixOS box with a host docker
daemon, `/var/lib/github-runner/.buildx-cache-*` directories, more hardware,
zero marginal cost — and singular, therefore a contention point. Four distinct
axes are bundled into that one bit:

| Axis | What it really is | Chuggernaut's answer | Status |
| --- | --- | --- | --- |
| **A. Capability** — needs a host docker daemon, a warm cache, an emulator, macOS | A *requirement*, not a choice | [#309 §5a](309-host-native-execution.md#5a-capability-aware-placement) mode/capability predicate; #309 §9 declared caches; [#322](322-macos-native-runtime.md) for the macOS platform | **Shipped** for the mode predicate (P2 slice 6); the declared caches and the macOS platform are still designed |
| **B. Load shedding** — "run this one on the cloud today, gumbo is busy" | A *scheduling* decision | `choose_placement` under `Busyness`/`Headroom` already picks the least-loaded node automatically; the §3.5 capacity queue absorbs the rest | **Shipped** |
| **C. Node health / maintenance** — "don't send work to gumbo at all right now" | A *fleet operator* action | `PUT /api/v1/platform/fleet/{node}/capacity` with `slots: 0` — a full drain, `platform_admin` only | **Shipped** |
| **D. Cost** — hosted minutes are metered, gumbo is sunk cost | A billing decision | No analogue: every node in the fleet today is owned hardware | Absent, and see [triggers](#what-would-change-this-conclusion) |

Axis A is not a per-run choice at all under Chuggernaut's model. If a job needs
gumbo's properties, flipping it to "cloud" produces a failure or a
pathologically slow run — the toggle is only safe because the human operating it
knows which jobs tolerate which branch. A declared requirement removes the
ability to choose wrong, which is strictly better than exposing the choice.

Axis B is the one that looks like it needs a per-run lever and does not. Beacon
needs a human load balancer because GHA gives a self-hosted runner its own queue
with no overflow path: a job routed to `gumbo` waits behind gumbo's queue, and
this input is the only escape onto the hosted pool. Chuggernaut's unpinned
placement *is* that overflow path, continuously and without a human: an
unpinned launch goes to whichever in-service node has headroom, and when none
does it is parked `Pending` with `pending_reason: QueuedForCapacity`, burns no
retry budget, drains FIFO on the next container exit, and escalates with
`no_free_slots_timeout` only after the 30-minute backstop (`docs/spec.md` §3.5).
Porting axis B as a per-run field would be **importing a manual workaround for a
problem the target platform does not have.**

Axis C is the sharp one, and it is already solved on the correct side of the
system. "Gumbo is busy / degraded / being rebuilt" is a statement about the
fleet, not about thirteen jobs. Beacon expresses it thirteen times, once per
dispatch, because GHA gave it nowhere else to say it. Chuggernaut says it once,
as a platform admin, and every job — including ones nobody thought about —
respects it immediately. **One fleet action beats thirteen per-run overrides**,
and it is the only form of the lever that cannot be forgotten on the fourteenth
job.

Axis D is the residue, and it is genuinely absent. It is also genuinely
inapplicable: there is no metered node kind in the fleet — nodes arrive either
as `DOCKER_NODES` seeds or as announcing workers (`crates/worker/src/backend.rs`,
`NodeHandle`), and none of them bills by the minute.

**Conclusion: after decomposition, nothing is left that wants a per-run node
selection.** Axis A is a requirement (designed), B and C are shipped, D does not
apply to this fleet.

## Correction: the property test does not guard the shape gap 10 would take

This is the most important thing verified for this document, and it corrects
design #308 §A3 rather than restating it.

The test exists and is where #308 says it is:
`resolved_job_type_is_equal_for_any_two_input_maps`, a `#[test]` in
`crates/domain/src/release.rs`'s test module — tier 1 by
[`docs/reference/testing.md`](../reference/testing.md), pure, no I/O. It builds four input maps
(including one deliberately containing `image`, `eval`, `secrets` and `prompt`
keys), varies `job.inputs` across them, and asserts that
`with_job_evaluators(job_type, &job)` returns an **equal** `JobType` every time.
It has not moved and has not changed shape.

What it guards is exactly what its name says: **job-type resolution**. And
placement is not resolved there. `ContainerLaunchConfig.node` is composed at
launch, at the three sites listed above, each reading
`job_type.placement_node()`. A change of the form

```rust
node: job.inputs.get("runner").cloned().or_else(|| job_type.placement_node().map(String::from)),
```

in `crates/dispatcher/src/exec.rs` leaves the resolved `JobType` byte-identical,
passes the property test, and would arguably not even violate Decision 1's
literal wording — the job type was resolved without reading inputs; the *launch
config* was then overridden after the fact.

So #308 §A3's sentence "Threading `Job.inputs` into config resolution fails that
test" is true and is not the whole story: the natural shape of an
input-driven placement hack does not go through config resolution. **The
invariant's text is broader than its enforcement.** Anyone citing the test as
the reason gap 10 is blocked is citing it one step too far.

The gap is cheap to close and worth closing as its own contract, per docs/reference/style.md's
contract-first rule:

> **`ContainerLaunchConfig.node` is a pure function of the resolved job type.**
> Nothing on the `Job` record participates.

That is already true and already nearly self-evident from the signature —
`JobType::placement_node(&self)` takes no `Job` — but nothing in the tree
*states* it, so nothing stops the fourth call site from being written
differently. Recording it in [`docs/reference/contracts.md`](../reference/contracts.md) beside the
other launch-path contracts, and in the `docs/spec.md` §3.1 sentence that already
says "No labels, no anti-affinity", makes it a rule a reviewer can reject by
name. A mechanical check was considered and rejected: catching it properly needs
taint analysis, and a grep gate over a struct-field initializer is brittle in a
way `.chug/tasks/check-comments.sh` and `check-duplication.sh` are not — those
match a lexical fact, this would match a coding shape.

## Options for where per-run placement could live

Laid out honestly, including the ones rejected.

### Option A — nothing new (recommended)

Per-run node selection is not added. Capability-aware placement
([#309 §5a](309-host-native-execution.md#5a-capability-aware-placement)) covers
axis A; the existing policy plus the §3.5 capacity queue covers axis B; runtime
capacity control covers axis C; `placement.node` remains the reviewed,
repo-versioned escape hatch for genuine one-node facts.

*For:* Costs nothing. Weakens no invariant. Adds no `Job` field, no schema
epoch, no wire change, no authorization surface. Leaves #311 Decision 1's
totality intact. It is also the only option that keeps `placement` a
*reviewed* decision.

*Against:* It removes an operator affordance beacon has today. If a case turns up
that genuinely needs "this run, on that machine, now", the answer is a job-type
edit — a commit, a review, and a merge — measured in minutes, not seconds. That
is a real cost and this document accepts it deliberately: the friction is the
control ([Authorization](#authorization-who-may-pin-a-run)). It also assumes
axis-D never appears; see [triggers](#what-would-change-this-conclusion).

### Option B — `Job.placement_node: Option<String>`, a first-class record field

The shape #308 §A3 guessed at: a per-job override beside `Job.timeout` and
`Job.model`, distinct from `inputs`, therefore not weakening the property test
at all (it does not touch `JobType` resolution — as established above, neither
would the forbidden version).

*For:* Smallest thing that could work. Real precedent in the tree: `Job.timeout`
and `Job.model` do override job-type fields per job, and #311 Decision 1 itself
points at that door for its "never (by inputs)" rows. Would port beacon's habit
one-for-one.

*Against*, and this is decisive on three independent counts:

1. **It re-grants exactly the capability Decision 1's *reasoning* forbids.** The
   stated harm is "a job creator picks which host runs project code." The harm
   is identical whether the value arrives as `inputs.runner` or as
   `job.placement_node`. The invariant's letter survives; its purpose does not.
   A design that satisfies a rule by relabelling the field is not satisfying the
   rule.
2. **The `timeout`/`model` precedent does not transfer, and the reason is
   structural.** Both are safe because they are **Work-phase-scoped**:
   `Job::timeout`'s doc comment says "for Work tasks only; evaluators keep the
   type default", and `Job::model` says the same. That scoping is what keeps a
   per-job override off the gate path. A node pin cannot inherit it. The
   properties that make a node worth pinning to — a warm cargo/buildx cache, a
   device, a platform — are needed *most* by the evaluator: in this repo the
   expensive cache-sensitive work is `.chug/tasks/ci.sh` running as the `ci`
   command evaluator, not the work task. A Work-only pin is useless for every
   motivating case, and a pin that covers evaluators is a per-job field steering
   the gate — the thing `docs/reference/design-lifecycle.md` and #311 both refuse.
3. **It would be settable by an agent.** `triage_container_permissions`
   (`crates/auth/src/nats.rs`) grants a factory triage container
   `req.jobs.create.{owner}.{project}`. Job creation is a Member+ action at the
   API (`jobs_create` → `member_on`) and an in-container capability for triage
   agents. Adding a node name to the creation payload hands a fleet-steering
   primitive to a code path whose input is, by design, externally-authored issue
   text.

### Option C — a per-run *capability constraint* (never a name)

A run may **narrow** placement — "this run also requires `host` mode" — but may
never name a node. Monotone: it can only shrink the candidate set.

*For:* Genuinely safer than Option B, and for a stateable reason (see
[Authorization](#authorization-who-may-pin-a-run)). Fails closed: an
unsatisfiable constraint yields `NoCapacity`, which is the transient §3.5 path,
not a wrong placement.

*Against:* It is a mechanism in search of a use. A capability the *job type* did
not declare is a job type that is wrong, and fixing it there is a one-line edit
in the file that already carries every other execution contract. Narrowing
per-run can make a job queue or escalate but can never make it go where the
operator wants — so it does not solve axis B or C either. **Rejected now, but
recorded as the shape to reach for** if a real case appears: additive-only,
narrowing-only, exactly as `Job.eval` is additive over the type's evaluators.

### Option D — say it on the fleet, not on the job (already shipped)

For axes B and C the correct object is the node, not the run. This is not a
proposal; it is a pointer at machinery that exists:
`PUT /api/v1/platform/fleet/{node}/capacity` (`crates/api/src/routes.rs`,
`crates/dispatcher/src/handlers/fleet.rs`), persisted as intent in the
`platform` bucket, pushed to the daemon as `set_slots`. `slots: 0` is a full
drain: the node becomes placement-inert, running containers are never killed,
and queued launches wait for other capacity (`docs/spec.md` §3.1).

Note the property this buys, because it also bounds the damage of any future
pin: **drain beats a pin.** A pinned launch onto a drained node sees
`free <= 0`, gets `NoCapacity`, and queues — it does not land. So an operator
retains ultimate control over a node even against a pinned job type. That is a
real safety property of the current design and worth stating explicitly; it is
also why the DoS concern about pinning is bounded to "queue and eventually
escalate" rather than "occupy a node the operator is trying to empty."

### Option E — an input selects the node (forbidden, and rightly)

Recorded for completeness. Forbidden by #311 Decision 1. As established above,
the shipped property test would **not** catch the launch-site form of it, which
makes this the option most likely to be implemented by accident. That is the
argument for writing the contract down.

## Are node names the right currency at all?

No — and the beacon evidence says so more strongly than the k8s analogy does.

Names should stay where `placement.node` already puts them: a repo-versioned,
reviewed, job-type-level escape hatch for a fact that is genuinely about one
machine and cannot be phrased as a property (`docs/spec.md` §3.1's "the one affinity
control"). That is the same role `spec.nodeName` plays in Kubernetes, and it is
the right role.

The currency of routine selection should be capability, for the reasons above
plus one specific to this platform: `choose_placement` is a **pure function**
(`crates/container/src/lib.rs`) that already takes a policy, a candidate list
and an optional pin, and is unit-tested with no daemon. A capability predicate
is one more argument to a function whose whole design invites it (docs/reference/style.md Tier
2 rule 1: decision logic pure, effects elsewhere). A per-run name is not a new
argument — it is a new *source* for an existing one, and the interesting
question with a new source is always authorization, never mechanism.

Be honest about the limit: on a fleet with one simulator, `leases: [ios-sim]`
selects the same machine a pin would. Capability and identity coincide when the
class has one member. Three differences survive that coincidence, and they are
the whole argument:

- It **degrades correctly** when the class grows to two.
- It **records the reason** in the config, so the audit trail says "needed a
  simulator" and not "said gumbo".
- The **node** asserts membership, not the job creator. That is the
  authorization difference below.

## Authorization: who may pin a run

Pinning is a capacity and trust decision, and the tree's current answer is
consistent in a way that is easy to miss:

| Decision | Object | Authority today |
| --- | --- | --- |
| Which node a job type's containers land on | The repo file | Whoever can merge to the project's default branch — i.e. the full evaluator + merge-gate path (`docs/spec.md` §3.3) |
| How much capacity a node offers, including drain to zero | The fleet | `platform_admin` (`docs/spec.md` §7.5 "Platform-level config") |
| Creating a job | The job | **Member+** (`jobs_create` → `member_on`), plus any factory triage container (`triage_container_permissions`) |

Placement is, today, a decision made at the two *stronger* authorities. Option B
would move it to the weakest one — and to a non-human one. That is the
authorization argument in a sentence.

**Why a capability request is meaningfully less dangerous than a name pin**,
stated concretely rather than by assertion:

1. **A constraint cannot steer; a name can.** A capability request shrinks the
   candidate set and leaves the *choice within it* to the platform's policy and
   live load. A name removes policy, load, and headroom from the decision. The
   fleet's own scheduler stays in the loop for one and is bypassed for the
   other.
2. **The node asserts a capability; the job creator asserts a name.** Under
   [#309 §4](309-host-native-execution.md#4-capability-advertisement)
   capabilities ride the `ping` reply — the node says what it can do, boot-time
   facts derived from its own configuration, with every absent field failing
   closed (`modes` ⇒ container-only, `leases` ⇒ empty). A job creator cannot
   forge one. A node name is unforgeable in the opposite direction: it is
   whatever string the creator typed, shape-checked only (`[A-Za-z0-9_-]+`,
   `docs/spec.md` §3.1), because the fleet list lives in the dispatcher's env and
   cannot be validated offline.
3. **Deliberate co-location is only reachable by name.** Nodes are shared:
   `WORKER_CACHE_DIR` gives a node's jobs a common sccache directory (`docs/spec.md`
   §3.1), and under [#309](309-host-native-execution.md) §8/§10 host nodes are
   explicitly the weaker-isolation kind. A creator who can name a node can
   choose to land beside a specific victim task and its shared host state. A
   creator who can only request "host mode" lands wherever the fleet has room.
4. **A capability fails closed and loudly.** Unsatisfiable ⇒ `NoCapacity` ⇒ the
   §3.5 queue ⇒ `no_free_slots_timeout` escalation after 30 minutes, with a
   distinct message (#309 §5a keeps "no node advertises host mode" separate from
   "no free slots") — a diagnosis, not a silent wrong placement.
5. **It is auditable as a reason.** `Job.inputs` is deliberately an audit
   surface — immutable once `base_ref` is pinned, ordered for comparability
   (`crates/types/src/job.rs`). A recorded capability requirement reads as
   *why*; a recorded node name reads as *what*, and a reviewer cannot tell a
   legitimate one from an exfiltration attempt.

What a name pin does **not** buy an attacker, stated so the argument is not
overclaimed: it cannot defeat drain (Option D above), it cannot exceed the
node's slot count, it cannot cause spillover onto an unintended node, and it
cannot land on an out-of-service node. The residual risk is co-location and
attention-steering, not capacity capture.

## What #311 Decision 1's invariant buys, and what an amendment would cost

**What it buys**, precisely: the job type a run executes under is a pure
function of `(base_ref, job type name)` and nothing else. That is what makes the
guarantee expressible as an *equality* rather than as a list of safe fields, and
the equality is what makes it testable in one tier-1 property test that no
future schema field can invalidate. #311 says this itself and the phrasing is
the load-bearing part: "if no job-type field may be chosen by an input, then no
substitution engine needs to exist", and the classification "does not have to be
maintained as a list somebody can forget to extend."

**The cost of any exception is therefore not the exception.** It is that the
property stops being total. The moment one field is input-selectable, the
invariant becomes "inputs never reach *gate-deciding* config resolution", which
requires a maintained classification of which fields are gate-deciding — exactly
the artifact #311 abolished, reintroduced to buy back one operator convenience.

This document therefore does **not** amend Decision 1, and recommends against
amending it for gap 10. Two things are worth adding to it instead, neither of
which weakens it:

1. The launch-site contract above — `ContainerLaunchConfig.node` is a function
   of the job type alone — which *extends* Decision 1's guarantee to the place
   the invariant's text already implies but its test does not reach.
2. A note in #308 §A3 that the property test's coverage stops at config
   resolution, so a future reader does not over-rely on it.

## Interaction with the sibling designs

**With [#309 §5a](309-host-native-execution.md#5a-capability-aware-placement) /
P2 (capability advertisement).** This document's recommendation *is* #309 §5a
plus nothing. Gap 10 does not add a slice to #309's phasing; it adds a reason
for P2 to happen, and a second consumer for `NodeCapabilities.platform` (the two
macOS beacon jobs, alongside [#322](322-macos-native-runtime.md)). Nothing here
changes #309's sequencing constraint that capabilities land after #293 job 3, in
`probe_worker`, on the reply path.

**With `placement.node`.** Unchanged, and re-affirmed as the escape hatch. The
one addition is the contract statement above, which describes existing behavior
rather than altering it. `docs/spec.md` §3.1's "No labels, no anti-affinity" is
already scheduled to be amended by #309 slice 3 for the mode predicate; the
per-run wording belongs in the same edit, and the honest form is *"placement is
selected by node-advertised capability and, as an escape hatch, by an explicit
job-type pin; no per-run mechanism exists"*.

**With [#309 §5b](309-host-native-execution.md#5b-exclusive-resources-device-leases)
/ P4 (device leases).** Discussed next — the brief asks a genuine question there
and it deserves its own answer.

## The §5b question: extended resources, or a lease table?

The brief's proposal is worth taking seriously: Kubernetes models device
exclusivity as a countable **extended resource** that the scheduler subtracts
alongside CPU and memory, so exclusivity falls out of ordinary capacity
accounting — no separate table, no separate acquire/release lifecycle, and
therefore no revoke-leak footgun. Chuggernaut appears to have the accounting
already: `FleetNode.reserved`, the `place_lock` held across read-load →
`choose_placement` → reserve, and `Reservation`'s `Drop`.

**It does not transfer, and the reason is more fundamental than the one #309
§5b gives.**

### #309 §5b's objection is right but understated

Design #309 §5b rejects `Reservation` as the lease's home because its drop "would free
the device the moment the task *started*" — `FleetBackend::launch` binds it as
`_reservation` for exactly one call (`crates/worker/src/backend.rs`). That is
factually correct. As an argument it is weak, because a longer-lived guard is
trivially writable; taken alone the objection reads as an implementation detail
rather than a reason.

### The real reason: there is no ledger for exclusivity to fall out of

Kubernetes gets extended resources nearly free because its scheduler **keeps a
ledger**: allocatable minus the summed `requests` of every pod *bound* to the
node, maintained in the scheduler's cache with entry lifetime equal to the pod's
lifetime. Exclusivity is a subtraction against a number the scheduler owns.

Chuggernaut deliberately inverted this. `docs/spec.md` §3.1: "the node owns its
capacity, and the scheduler reads exactly one number per node" — a worker
"counts its own running `chuggernaut.managed` containers" and reports it on
`ping`; a docker endpoint is listed through `load_by_node`
(`crates/worker/src/backend.rs`, `node_load`). Occupancy is likewise "rebuilt
from the live containers the backend reports — never from in-memory
bookkeeping" (`docs/spec.md` §3.1). **Capacity here is observation-derived, not
ledgered.**

`FleetNode.reserved` is not the ledger. Its own doc comment says what it is:
in-flight launches "this dispatcher has *placed* on the node but whose
containers the node's live count does not yet report", incremented under the
placement lock and decremented "once the launch RPC returns (the container then
exists and the node counts it)". That is the exact analogue of the Kubernetes
scheduler's **assume cache** — the optimistic bookkeeping covering the window
between "decided" and "the observation catches up" — not of its resource ledger.

So "ride the reservation machinery" is asking the assume cache to become the
ledger. That is not reuse; it is introducing the second, ledger-shaped
accounting the platform does not have, for one dimension, while slots keep the
observed one. Two dimensions with two different sources of truth inside one
`choose_placement` call is a worse shape than one small table in the actor.

### Three concrete consequences

1. **The k8s-faithful version needs a node-side mechanism.** For a device to be
   "observed occupied" the way containers are, the *node* must report it — and
   #309 §5b already rejected node-side leases, correctly: "its own
   acquire/release protocol, its own crash story, and its own reconciliation —
   three mechanisms to avoid one `HashMap` in a process that is single-threaded
   by design." Observation also lags in a way that matters: a task holds a
   simulator from when it *grabs* it, not from when its container starts, so two
   tasks placed in one probe window both read the device free. For slots a stale
   read is a suboptimal placement; for a device it is two tasks on one
   simulator — a correctness failure, not a performance one.
2. **Home matters more than lifetime.** Put the table in `FleetBackend` and
   §3.6 cannot rebuild it: the backend sees `ContainerId`s, not tasks or job
   types, so after a dispatcher restart it cannot know which recovered `Running`
   task declared which lease. #309 §5b names this and it holds.
3. **Moving the decision below the actor creates the race it is trying to
   avoid.** `reserved` exists because "agent launches run on their own spawned
   tasks, so their `place()` calls race" (`crates/worker/src/backend.rs`). A
   held-set snapshot threaded through `ContainerLaunchConfig` on an actor turn
   and consumed later, inside a spawned launch, can be stale when `place`
   finally runs. #309 §5b's pin-required form — decide on the actor turn,
   immediately before `backend.launch`, where the single-threaded actor holds
   the table — is not a limitation working around the design. It is what makes
   the decision race-free.

**Verdict: #309 §5b stands.** Its recommendation to require `placement.node`
alongside `placement.leases`, and to keep `held: HashMap<(Node, LeaseName),
TaskId>` in the actor, is correct. Two amendments to its *reasoning* are worth
recording: the rejection of `Reservation` should rest on the ledger argument
above rather than on the drop-timing detail, and the deferred "filter candidates
on lease availability" end state should note that its blocker is the spawned-
launch race, not merely snapshot freshness.

### A refinement that moves the footgun, without pretending `reserved` is a ledger

The brief's instinct — that a separate acquire/release lifecycle is a leak
waiting to happen — is sound, and #309 §5b concedes it: "miss that second one
and a lease leaks forever on every revoke." There is a way to get the
by-construction property without borrowing the reservation machinery:

> **Derive the held set instead of maintaining it.** Stamp a task's declared
> leases onto its **task record** at launch, beside the `pending_reason` and
> `queued_at` that §3.5 already persists there. Then `held(node, lease)` is a
> *query* rather than a table.

Everything turns on what that query is keyed on, and the obvious key is wrong.

**The naive form leaks exactly as the table does.** Key the query on the *task*
— "is there a live, non-terminal task on that node whose record declares that
lease?" — and revoke never clears it, for two independent reasons:

- `Core::close_pending_tasks` (`crates/dispatcher/src/core.rs`) closes
  **human tasks only**. It `continue`s unless the task's kind is
  `TaskKind::Human` or its `performed_by` is `Some(Performer::Human)`, which its
  own doc comment states ("a revoked job's Pending human/escalation tasks"). A
  container task parked `Pending` with `pending_reason: QueuedForCapacity` is
  untouched.
- A **Running** container task's record is never transitioned by revoke at all.
  `Core::kill_running_containers` calls `backend.kill(cid)` and writes nothing;
  `revoke_job` then removes the job from `self.active`, so the resulting exit
  hits the discard branch in `Core::on_task_exited`
  (`crates/dispatcher/src/exec.rs`, *"no exec state
  (job revoked or completed); ignoring"*) and the record stays `Running` — by
  design. That discard is precisely *why* #309 §5b needs an explicit release
  beside `close_pending_tasks`.

So a task-keyed derivation holds the device forever, and worse than the table
does: the stale `Running` record is persisted, so the phantom hold survives a
restart instead of being cleared by one, and there is no release site left to
audit.

**Key it on the job instead, and the property comes back.** `revoke_job` does
`set_state(&mut j, JobState::Revoked)` for the target and every cascade target,
and `JobState::is_terminal` is `Done | Revoked` (`crates/types/src/job.rs`). So
the query becomes:

> is there a task on that node, in a live state, whose **job** is non-terminal,
> whose record declares that lease?

Why this shape is worth considering:

- **No release site.** Revoke's own state write is the release. The footgun
  disappears rather than being test-covered, and the `Done` path — the discard
  branch's other half — is covered by the same predicate.
- **§3.6 rebuilds it for free, but only in this form.** Job records are
  persisted, so a job-keyed query is reconstructed from the store on startup
  with no reconciliation work. A *task*-keyed one is not: `Core::reconcile`
  (`crates/dispatcher/src/reconcile.rs`) only recovers jobs in
  `Work`/`Evaluation`/`WrapUp` and leaves a revoked job's stale `Running`
  records exactly as it found them.
- **It matches the platform's own precedent.** Occupancy is derived from live
  containers "never from in-memory bookkeeping" (`docs/spec.md` §3.1); the launch
  queue persists `queued_at` on the task record so FIFO order survives restart.
  This is the same move.
- **It stays in the actor**, so consequence 3 above is unaffected.

The honest costs, because they are real:

- **Correctness now depends on a second record's state** — a coupling the
  maintained table does not have. The derivation is only leak-free if *every*
  path that abandons a `Running` task record leaves its job terminal. Revoke
  satisfies that on the same turn; the wrap-up escalation path
  (`WrapUpStep::EscalatedDropExec`, `crates/dispatcher/src/eval.rs`) drops exec
  state with the job `Escalated`, which is **not** terminal, and today that is
  harmless only because the task in question has already exited. That is an
  invariant a P4 implementer must assert (docs/reference/style.md Tier 2 rule 2), not one the
  shape gives away.
- It denormalizes a job-type-derived fact onto the task record. #309 §5b
  explicitly relies on config being read live — "a job type edited between
  launch and restart to drop a lease simply does not re-acquire it — correct,
  because job-type config is read live." Stamping inverts that: the lease a task
  holds becomes what it declared *at launch*. That is arguably the more correct
  semantics for a device already in use, but it is a change, and it should be
  decided rather than absorbed.
- The derivation is a scan over live tasks per launch decision. At this fleet's
  scale (single-digit nodes, tens of live tasks) that is free, and docs/reference/style.md
  Tier 3 prefers the simpler shape over the faster one — but it should be
  bounded like everything else (docs/reference/style.md Tier 2 rule 3).

The third handle — the actor's own `self.active` map, which revoke removes on
the same turn — is **not** a candidate. Triage tasks are exempt from it by
construction (the same branch special-cases `TaskPhase::Triage`), so a triage
container would hold a device invisibly.

This is offered as a **refinement to #309 §5b, not a replacement for it**, and
it is a narrower one than it first appears: it removes the release *site*, not
the release *obligation* — the obligation moves onto "a stale task record
implies a terminal job", which is an assertion someone must write and hold. The
decision #309 §5b makes (lease table in the actor, pin required, capacity-queue
semantics for an unavailable lease) is unchanged; only the table's
representation moves from maintained to derived. Whoever implements P4 should
decide between the two on that trade — an explicit release beside
`close_pending_tasks`, which is one line and one test, against a derivation with
no release but a cross-record invariant to defend — and on the config-liveness
question above. If the invariant looks hard to hold, **#309 §5b's explicit
release is simply right** and nothing in this document depends on the refinement
being adopted.

## Is this needed before the beacon import?

No, and #308 already sequenced it that way: its phase table lists gap 10 as
"design first, an open question, **not scheduled work**", with no dependency
edge into any other phase. This document confirms the ordering was right and
strengthens it — the parts of gap 10 that are real (axis A) already sit inside
design #309 P2, which phase 2 depends on anyway; the parts that are operational
(axes B and C) are shipped; and the part that is absent (axis D) does not apply.

Concretely, the beacon import can proceed with `placement.node` on the handful of
job types that genuinely need gumbo, and nothing else. The thirteen dispatch
inputs do not need porting — they need *decomposing*, which is what the table
above does.

## Recommendation

1. **Do not add a per-run placement field to the `Job` record.** Option A.
   #311 Decision 1 stands unamended, `CONFIG_SCHEMA_EPOCH` is untouched, and no
   wire record changes.
2. **Record the launch-site contract** — `ContainerLaunchConfig.node` is a pure
   function of the resolved job type — in `docs/reference/contracts.md`, and note in #308 §A3
   that the property test's coverage stops at config resolution. This is the one
   piece of new work gap 10 generates, and it is a `docs` job.
3. **Land [#309 §5a](309-host-native-execution.md#5a-capability-aware-placement)
   as designed** when P2 arrives; it is the whole of gap 10's real content.
   Amend `docs/spec.md` §3.1's "No labels, no anti-affinity" in #309 slice 3 to state
   the per-run position explicitly rather than leaving it inferable.
4. **Keep `placement.node` as the reviewed escape hatch**, unchanged.
5. **#309 §5b stands**, with the two reasoning amendments and the derived-held-
   set refinement recorded above for whoever implements P4.

## What would change this conclusion

Stated as triggers, so a future reader can check them rather than re-argue the
whole document:

- **A metered or burst node kind joins the fleet** (axis D becomes real). Even
  then the answer is a node *class* plus a policy — "prefer owned hardware,
  spill to burst" is a placement policy, `PLACEMENT_POLICY` is already
  platform-wide config (`docs/spec.md` §12.4) — not a per-run node name.
- **A case appears where the same job type must run in two different places on
  different runs, and the difference is not derivable from the job type.** That
  is the one shape none of A–D covers. Option C (narrowing capability
  constraints, additive-only, never a name) is the shape to reach for, and it
  should be argued on its own case, not on beacon's.
- **The `placement.node` escape hatch starts getting edited per-run** — i.e.
  commits that only flip a pin. That is the demand signal Option A is betting
  will not appear. It is cheap to detect: the repo history shows it.
- **A fleet with more than one node of a class**, at which point capability
  selection starts paying off visibly and any lingering desire for names should
  be re-read as a desire for a better label.

## Corrections verified against the tree

- **"Thirteen beacon workflows"** (the brief) — #308 §A3 says thirteen **jobs**:
  eleven linux plus two macOS. Several jobs can share a workflow file, so the
  number of files involved is at most thirteen and probably fewer. Unverifiable
  here; `~/beacon` is not checked out in this workspace.
- **The property test as gap 10's blocker** — it exists, unmoved and unchanged,
  at `crates/domain/src/release.rs`, named
  `resolved_job_type_is_equal_for_any_two_input_maps`. But it guards job-type
  resolution only and would not catch a launch-site input read. See
  [the correction above](#correction-the-property-test-does-not-guard-the-shape-gap-10-would-take).
- **Config paths** — #308 predates nothing relevant here, but for the record all
  job-type config now lives under `.chug/` (`docs/spec.md` §1.1); `.chug/jobs/*.yaml`
  in this repo contains **zero** `placement:` blocks.
- **`crates/container/src/k8s.rs`** — still a six-line stub with no `kube` or
  `k8s-openapi` dependency, as the brief states.
- **#309 §5b's `Reservation` objection** — accurate as to fact
  (`FleetBackend::launch` binds `_reservation` for one call) but understated as
  an argument; see [the §5b section](#the-5b-question-extended-resources-or-a-lease-table).
- **What revoke does to task records** — `Core::revoke_job` kills running
  containers without writing their records and closes **only** human/escalation
  Pending tasks (`Core::kill_running_containers` and
  `Core::close_pending_tasks`, `crates/dispatcher/src/core.rs`). A revoked job's
  container task records are left `Running` deliberately; the exit is discarded
  by `Core::on_task_exited`'s stale-exit branch
  (`crates/dispatcher/src/exec.rs`). Any lease scheme that reads task state must
  account for this — see the refinement above.

## Related

- [#308 — Porting beacon's GitHub Actions](308-gha-port.md), §A3 and gap 10.
- [#311 — Job inputs](311-job-inputs.md), Decision 1.
- [#309 — Host-native execution](309-host-native-execution.md), §4, §5a, §5b, P2, P4.
- [#322 — A native (macOS) execution runtime](322-macos-native-runtime.md).
- [#293 — Worker capacity](293-worker-capacity.md) — the observation path #309 P2 sequences behind.
- [`docs/spec.md`](../spec.md) §1.1 (`placement:`, `inputs:`), §3.1 (placement, fleet, capacity control), §3.5 (launch capacity queue), §7.5 (permission rules).
- [`docs/design/000-rationale.md`](000-rationale.md) — the Docker-fleet decision and the k8s/workflow-engine exclusion.
- [`docs/reference/contracts.md`](../reference/contracts.md), [`docs/reference/style.md`](../reference/style.md), [`docs/reference/testing.md`](../reference/testing.md).
- [`docs/design-docs.md`](../design-docs.md) — the header contract this document follows.
