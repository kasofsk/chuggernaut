# Design #543 — Placement granularity: what a task needs, and where it says so

Status: IMPLEMENTED IN PART — S1, S2 and S3 landed (jobs #550, #556, #551); S4 remains.

Written against the tree at `fa1c414` (2026-08-10). Every claim about current
behaviour was read out of the source named beside it rather than out of a
sibling design doc **or out of this job's own brief**; the seven findings that
reading produced are recorded in
[Corrections](#corrections-verified-against-the-tree), two of them corrections
to the brief itself.

## Current state

*This section is the mutable head: it is rewritten to current truth whenever
anything below it changes. Everything after the horizontal rule is append-only —
the argument and its dated corrections, never edited
([#415](415-knowledge-architecture.md) D2).*

| Fact | Where | State |
| --- | --- | --- |
| Mode resolves per task | `ContainerLaunchConfig::required_mode()`, [`crates/container/src/lib.rs`](../../crates/container/src/lib.rs) | Landed (jobs #479, #507) |
| Placement is decided per task | `choose_placement`, same file | Landed |
| Mode and limit enforceability are matched | `requirements()` → `LaunchRequirements` | Landed (job #524) |
| A task's declared environment is matched | `PlacementCandidate::serves_env`, same file | Landed (job #550) |
| A task's requirements *other than* mode, limits and environment are matched | — | **Not expressible.** A pin names a machine instead |
| `NodeCapabilities.envs` has a reader | [`crates/types/src/worker.rs`](../../crates/types/src/worker.rs) | **Yes** since job #550: placement admits a launch declaring a node-interpreted `runtime.env` only on a node advertising it |
| `NodeCapabilities.agent_cli` / `.docker_reachable` have readers | same | **No** outside the worker that sets them |
| `NodeCapabilities.leases` is ever populated | same | **No** — hardcoded empty, and correctly so ([#309](309-host-native-execution.md) P4 is unbuilt) |
| A node advertises a device or named feature | `node_capabilities`, [`crates/worker/src/daemon.rs`](../../crates/worker/src/daemon.rs) | **No** — no field carries one, so S4 builds the advertisement before the matcher |
| A node-side grant is visible to placement | `DockerGrant::admits`, [`crates/container/src/docker.rs`](../../crates/container/src/docker.rs) | **No**, and D6 keeps it that way |
| A node-side grant can see a launch's level | same | The docker one can and does — `CHUG_PHASE=Work` (S3, job #551). The other three stay level-blind, per section 7 |

**Two** job types pin today, `android-proof` and `docker-proof`, and that is the
whole of `.chug/jobs/` carrying a `placement:` block. Each pins for a requirement
it cannot otherwise state — section 2's third column — and neither is reachable
by S1: `android-proof` declares a top-level `image:` and no `runtime:` block, so
it has no `runtime.env` for any matcher to read, and `docker-proof`'s requirement
is a grant D6 keeps invisible to placement. `mac-proof` states its requirement as
an env and names no machine. S1 needed no schema field and no epoch; S4 needs one
`CONFIG_SCHEMA_EPOCH` bump **and** a node-side advertisement this tree does not
yet have, and no epoch has moved.

## Decisions

- **D1. Capability is the placement input; a name is an override.** A task states
  what it needs of a node; the scheduler finds one. `placement.node` remains, for
  the case where the requirement genuinely *is* a named machine, and stops being
  the way ordinary requirements are expressed.
- **D2. Match `envs` at placement (S1).** `runtime.env` is a declared requirement
  that already exists on the job type, and `NodeCapabilities.envs` is the
  advertisement that already exists on the node. Matching them needs **no new
  field and no epoch bump** — it is job #524's slice one level up. Its reach is
  narrow and section 5 measures it: `envs` is built from the discovered Xcode set
  alone, so `mac-proof` is S1's one live consumer today.
- **D3. `placement.features` ([#367](367-android-emulator-execution.md) A4) is the
  general form of D2, and its trigger is already met.** One rule — a declared
  requirement matched against an advertised capability — of which `runtime.env`
  is the member that already exists. A4 is gated on *"only when the pin stops
  expressing the requirement"*; section 2 argues that is true now, and `/dev/kvm`
  is a requirement in this tree that is not an env and never will be. A4 costs
  more than it says — an epoch *and* a node-side advertisement nothing provides
  today (Correction 7) — and section 6 prices both halves.
- **D4. `placement.node` does not become per-level.** Not because the harm is
  imaginary — section 3 measures it, and S1 does not remove all of it — and not
  because the two cost the same: the per-level pin is the *cheaper* of the
  two. It is declined because both spend the same irreducible thing (one
  `CONFIG_SCHEMA_EPOCH` bump and a permanent config field), only one of them
  expresses the requirement, and what the cheaper one buys after S1 is one slot
  of two on a container node. Section 6 makes that case in full.
- **D5. A node-side grant is scoped to work, not to every level.** Job #542
  measured an evaluator of an allow-listed type holding the socket, the `ci` one
  appended by `.chug/jobs/_defaults.yaml` included — an evaluator whose author
  never asked for node root. This amends [#517](517-docker-access-for-jobs.md) D3.
  The level discriminator must be `CHUG_PHASE`, for the reason in section 7.
- **D6. A grant is enforced at the node and is never a placement input.** D5 and
  D1 agree on **granularity** — both are per-task, not per-job — and deliberately
  disagree on **who decides**: the scheduler matches capabilities a node
  advertises, and the node alone enforces the consent it never advertises.
  Section 7 states what that costs and what would have to change to revisit it.

## Slices

| # | What | State |
| --- | --- | --- |
| **S1** | Match `runtime.env` against `NodeCapabilities.envs` in `choose_placement`, with a refusal naming the ref and the node's set | **Landed** (job #550) — no field, no epoch; [the note](#note--2026-08-10--s1-landed-envs-has-a-reader-job-550) records the one decision it had to make |
| **S2** | Drop the pin from `mac-proof` only, and assert placement is unchanged | **Landed** (job #556) — one pin, one file, no epoch; [what it removed and what it did not](#what-s2-landed-2026-08-10-job-556) |
| **S3** | Scope `DockerGrant::admits` to work-level launches keyed on `CHUG_PHASE` (D5), and amend #517 D3 | **Landed** (job #551) — [what it changed](#what-s3-landed-2026-08-10-job-551) |
| **S4** | `placement.features` as the general form (#367 A4): the node-side advertisement first, then the matcher, with its epoch bump | Proposed — gated on S1, so one matcher serves both |

---

## 1. What is already per-task, measured

The seam this design is about is narrower than it looks, because three quarters
of it already works.

- **Mode.** `required_mode()` reads *that launch's* own `image`: `Some` resolves
  to container, `None` to host. It is not a property of the job.
- **Runtime environment.** `level_runtime_env()` (job #507) hands a container
  level under a host job type no `runtime.env` at all.
- **The match.** `requirements()` yields `{mode, resource_limits}` and
  `choose_placement` filters candidates on `serves(mode)` and `bounds(...)`
  (job #524).
- **No pin is required.** There is no validation rule obliging a host job type to
  declare `placement.node`; one that omits it validates clean. The test in
  [`crates/types/src/job_type.rs`](../../crates/types/src/job_type.rs) says so in
  its own words — *"Placement is per task, not per job (design #361, #490 D4)"*.

So "host work, container CI, one job" is shipped, and `.chug/jobs/mac-proof.yaml`
runs it in prod. **A task already needs capability on the node it lands on, and a
job already only needs capability somewhere in the cluster.** What is missing is
the vocabulary for a task to say which capability.

## 2. A pin names a machine, not a requirement

```rust
pub struct Placement {
    pub node: Option<String>,
}
```

One field, a node name, shape-checked only — `validate()` is offline and cannot
know the fleet, so a pin onto a node that does not exist is a launch-time
failure, not a config error.

The consequence is that a requirement is expressed by naming something that
*happens* to satisfy it:

| Job type | Needs | Says | Is that need an `env`? |
| --- | --- | --- | --- |
| `mac-proof` | Xcode 26.5 and a simulator | `node: air` | Yes — `runtime.env: xcode:26.5` |
| `android-proof` | `/dev/kvm`, plus the SDK/JDK/flutter mounts | `node: nuc` | **No.** It declares a top-level `image:` and no `runtime:` block at all |
| `docker-proof` | a granted docker socket | `node: nuc` | **No**, and D6 keeps it that way |

That third column is the correction the review forced, and it is load-bearing
for everything below: **two of the three current pins express requirements that
`envs` cannot carry**, so no amount of env matching removes them. That is also
[#367](367-android-emulator-execution.md) A4's own trigger — *"only when the pin
stops expressing the requirement"* — met in this tree today rather than at some
future node.

`android-proof`'s own comment is the clearest statement of the problem, written
before this design existed:

> phase A4 — `placement.features` plus a capability predicate — is exactly what
> later replaces this pin. Until a second KVM node exists, a name is the honest
> expression of the requirement.

Three things follow, and the third is the one that bites:

1. **Nothing validates the requirement.** Remove the SDK from the nuc and
   `android-proof` still places, then fails inside the container.
2. **A second capable node does not help.** The pin routes to the one named.
3. **The requirement is invisible to every reader** — including `choose_placement`,
   which is the one that would act on it.

## 3. The pin is job-scoped in a task-scoped system

`placement.node` lives on the job type, and **three** call sites read
`job_type.placement_node()` when composing a launch:
[`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs) (work),
[`crates/dispatcher/src/eval.rs`](../../crates/dispatcher/src/eval.rs)
(evaluators) and
[`crates/dispatcher/src/launch_queue.rs`](../../crates/dispatcher/src/launch_queue.rs)
(the queued retry). So one pin binds every level of the job, including levels
whose resolved mode has nothing to do with why the pin was written. That is the
blast radius of a *"do not inherit"* change, and it is small: three readers, and
the three pinned job types named in section 2 — a closed set, because
`placement:` appears in no other file under `.chug/jobs/`.

The cost is not merely untidy, and it has two sizes:

- **On the air, it is the whole node.** `enforce_host_capacity`
  ([`crates/worker/src/daemon.rs`](../../crates/worker/src/daemon.rs)) forces a
  host-capable node to **one slot** node-wide. `mac-proof`'s `ci` evaluator is an
  ordinary Linux container that would run anywhere; the pin carries it to
  `gumbo-air-0`, where it takes the node's only slot and blocks the host work the
  node exists for.
- **On the nuc, it is one slot of two.** `gumbo-nuc-0` is container-only and runs
  at `WORKER_SLOTS_nuc=2` ([`deploy/prod/README.md`](../../deploy/prod/README.md)),
  so the pinned `ci` evaluators of `android-proof` and `docker-proof` — neither of
  which needs KVM or the socket — occupy half the node's capacity rather than all
  of it. Real, and an order of magnitude milder.

S1 removes the sharp one and leaves the mild one standing, because section 2's
third column says `android-proof` and `docker-proof` cannot be unpinned by env
matching. Section 6 is where the residue is answered; D4 declines to answer it
with a per-level pin, and states why.

## 4. Seven capabilities are advertised, and placement reads two

`NodeCapabilities` carries `modes`, `platform`, `resources_enforced`, `leases`,
`envs`, `agent_cli` and `docker_reachable`. `PlacementCandidate` — the shape
`choose_placement` actually filters on — holds `modes` and `resources_enforced`
and nothing else. `platform` is not the third reader: its own doc comment says it
is *"diagnostic only, and never a placement filter on its own"*.

So five of the seven advertised facts reach no scheduler, and they are unread for
four different reasons, which is the part worth separating:

- **`envs` is the live gap.** `mac-proof` declares `runtime.env: "xcode:26.5"`;
  `gumbo-air-0` advertises `envs: ["xcode:26.5"]` (job #489); **nothing matches
  them.** The pin routes the task, and the environment reference is resolved at
  the node afterwards, failing there if absent. The declaration and the
  advertisement are already the two halves of a match nobody performs. This one
  is an oversight, and S1 closes it.
- **`leases` is not merely unread — it is never populated.** `node_capabilities`
  in [`crates/worker/src/daemon.rs`](../../crates/worker/src/daemon.rs) sets it to
  an empty vector unconditionally. Correct: #309 P4 is unbuilt and trigger-gated
  on a host node needing to run a second, non-device-bound task concurrently. A
  field that is wired end-to-end and always empty is cheaper than a schema
  migration later, and recording it here is what keeps a future reader from
  filing it as a bug.
- **`agent_cli` (#490 D3) has no reader either**, and that one *is* the same shape
  as `envs`: a host-capable node probes for an agent CLI, advertises the answer,
  and nothing filters on it. It is not in scope here — no job type currently
  declares an agent host launch that could be misplaced — but it is the next
  instance of this pattern, and S1's matcher is the shape that would close it.
- **`docker_reachable` (#517 D4) is deliberately not a placement input.** It says
  *this node's daemon reached a docker endpoint*, never *this launch is granted
  one*, and #517 was careful about the distinction. Correction 3 and D6 are what
  that costs.

The pattern is worth naming once, with its members' current states rather than a
headline count — the count is what went wrong in the draft this reworks. In the
last fortnight this repo has found: `resources_enforced` advertised with no
reader (closed by job #524); `HostBackend::admit` warning where #309 §7 required a
refusal (also closed by #524, and `admit` now returns `BackendError::Launch`);
[#490](490-agent-work-on-a-mac.md)'s `simctl spawn` finding attributing to a
session what was a property of its argument; `WORKER_HOST_PROJECTS` named by five
docs and absent from the source — **closed by job #525, which is why Correction 4
exists**; and now `envs`, `agent_cli` and `leases`. **An advertisement is not a
mechanism, and the gap between them is invisible in exactly the way a green test
is.**

## 5. Matching `envs` at placement (S1), and how far it reaches

The cheap slice, and the one whose reach the draft overstated.

`runtime.env` is already resolved **per level** (job #507): a container level
under a host job type carries none. So the requirement is already exactly
per-task, and matching it is a filter beside `serves(mode)`:

- a launch carrying a node-interpreted `runtime.env` is admitted only by a node
  advertising it in `envs`
- a launch carrying none is unaffected — every existing job type is unchanged
- a `nix:` reference names a build rather than a node fact and is never listed
  (the field's own doc comment says so), so it must not be matched this way

The refusals matter as much as the match: unpinned and unmeetable is
`NoCapacity` with the ref named — an ordinary queue entry that clears when a
capable node appears — and pinned-but-unmeetable is a hard `Launch` error, for
the reason the mode pin already is: a pin never falls back.

**How far this reaches, measured rather than assumed.** `node_capabilities`
builds `envs` from `xcodes.envs()` and nothing else, under a `debug_assert` whose
own words are *"only a host-capable node discovers an environment"*. So in this
tree `envs` **is** the discovered Xcode set on a Mac. One job type declares a
`runtime.env` a node interprets, and it is `mac-proof`. S1 therefore has exactly
one live consumer, and S2 drops exactly one pin.

**What that one buys**: `mac-proof`'s pin becomes removable, and its stated
reason survives the removal *better* than it does today. That comment worries
that *"an unpinned release could satisfy `host` on some future second Mac and
prove nothing about this one."* Under S1 a second Mac carrying a different Xcode
does not match `xcode:26.5`; one carrying the same Xcode is a legitimate host for
the proof. The requirement is enforced instead of approximated. And it is the
pin whose removal matters most — section 3's sharp case, a whole host node's only
slot.

**What it does not buy**: nothing on the nuc. `android-proof` has no `runtime:`
block, `docker-proof`'s requirement is a grant, and neither pin moves.

## 6. The residue: `placement.features`, and the case against a per-level pin

Two mechanisms could remove the nuc pins' `ci` levels from the nuc, and the
draft this reworks priced them as identical. They are not, and the corrected
price is what D4 turns on.

Both add a field to `Placement`, which carries `deny_unknown_fields`
([`crates/types/src/job_type.rs`](../../crates/types/src/job_type.rs)) — so an
N-1 dispatcher rejects the *whole* config and parks every job of the type (spec
§14.2) rather than tolerating the field. That is precisely the shape
`WORKLOAD_IDENTITY_SCHEMA_EPOCH`'s own doc comment gives as its reason to exist,
so each mechanism costs a `CONFIG_SCHEMA_EPOCH` bump, a frozen per-feature
constant beside it, and a `validate()` rule obliging a config that uses the field
to declare `min_dispatcher`. **`features` costs one thing more**: nothing in this
tree advertises `/dev/kvm` (Correction 7), so S4 is two halves — an
advertisement and a matcher — where a per-level pin is one.

| | Per-level `placement.node` | `placement.features` (#367 A4) |
| --- | --- | --- |
| Coordinated cost | one schema epoch | one schema epoch |
| Uncoordinated cost | none | a node-side advertisement, plus its matcher |
| Fixes section 3's residue | yes — an unpinned `ci` level runs anywhere | yes — a `ci` level declaring no features is unconstrained |
| Expresses the requirement | **no** — still a machine name | yes — `kvm` matched against what the node would then advertise |
| Validates it | no | yes, at placement, with a named refusal |
| Survives a second capable node | no | yes |
| Inheritance question | unavoidable, and both answers are bad (section 6a) | none — a level declares what it needs, like `image` |

The draft this reworks declined the per-level pin on the grounds that D2 would
remove the pins entirely; section 5 shows it removes one of three, so that
argument was withdrawn and the table replaced it. The table's cost row was then
wrong in the other direction, and with it corrected the per-level pin is the
**cheaper** of the two. D4 therefore cannot rest on "same price, better thing".
It rests on three narrower claims:

- **The irreducible half is equal.** The epoch is the *coordinated* cost — one
  number two deploy generations must agree about, and a `min_dispatcher` line
  every author of a config using the field then carries forever. Both mechanisms
  spend exactly one, and neither can be had without it. What `features` spends
  extra is ordinary work inside one crate: `NodeCapabilities` rides `PingOk` and
  `WorkerAnnounce` as an `Option` and every field of it is `#[serde(default)]`,
  which is the additive shape [`crates/types/src/version.rs`](../../crates/types/src/version.rs)'s
  rule 2 requires and tolerates — so the advertisement needs no epoch of its own,
  and costs labour rather than coordination.
- **The benefit is not equal.** Section 3 measured what a per-level pin buys
  *after* S1: one slot of two on `gumbo-nuc-0`, a container node. An epoch is a
  poor price for half a container node's capacity, and it buys no statement of
  what either nuc job actually needs — finding 1 survives it intact.
- **A config field is forever.** Config is repo-versioned and per-consumer (spec
  §1.1), so a field that ships is a field every project's YAML may use and no
  later job can quietly withdraw. Buying the workaround first means spending an
  epoch on it and *then* a second on the fix, with the workaround still in the
  schema and its inheritance rule (section 6a) still to be honoured.

So: not "the same cost", but "the same irreducible cost, and only one of the two
is the fix". The residue is not an argument for a finer workaround; it is the
second half of the argument for S4.

### 6a. The inheritance question, for the record

The brief asked it and it should be answered even though D4 declines the field.
A per-level `placement.node` has two possible semantics and neither is good:

- **Inherit unless declared** preserves today's behaviour exactly and therefore
  buys nothing until someone edits a job type — the harm in section 3 persists
  until three files are touched, and a field that changes nothing on the day it
  ships is a field nobody remembers exists.
- **Do not inherit** is the task-scoped semantics the rest of the system has, and
  it silently unpins every existing evaluator at the epoch bump: `mac-proof`'s
  `ci` (desirable), `android-proof`'s and `docker-proof`'s (also desirable), and
  any future type whose author wrote one pin meaning all levels (a silent
  behaviour change at a version boundary). Three call sites and three job types
  is a small blast radius — but "small and silent" is how a placement bug ships.

`placement.features` has no third semantics to choose: a level declares what it
needs, exactly as `image` and `runtime.env` already do, and a level declaring
nothing needs nothing.

### 6b. What S4 must build first, and what it still cannot do

**The matcher has nothing to read today.** `NodeCapabilities` carries no feature
list; `leases` — the field whose name suggests one — is hardcoded empty (section
4), and the only `leases: ["kvm"]` anywhere in the tree is a test fixture in
[`crates/types/src/worker.rs`](../../crates/types/src/worker.rs). So S4 begins by
making a node *say* it holds `/dev/kvm`, as an additive `#[serde(default)]` field
in the shape #309 §4 established for every other capability. Only then does the
second half — the task declaring `features: [kvm]`, and the match refusing a node
without it instead of failing inside the container — have anything to filter on.
A4 does not name that half, and a reader pricing
[#367](367-android-emulator-execution.md) A4 at "one schema field" is pricing
half of it.

Even with both halves, S4 does **not** by itself make either nuc pin *safely*
removable, because both requirements are grant-gated as well as
capability-gated. `WORKER_KVM_PROJECTS` is a per-project allow-list
(`owner/project` entries) and `WORKER_DOCKER_GRANTS` a per-`(project, job type)`
one (`owner/project:job_type` entries, section 7); the node enforces both and
advertises neither. A launch placed on a capable-but-ungranting node is refused
at the node, correctly and loudly, but it is refused after placement rather than
before it.

That is D6's cost, and section 7 is where it is paid deliberately.

D4 should be revisited if any of these turns out true:

- a requirement appears that is a **node identity** rather than a capability — a
  specific attached device, a specific network position — and belongs on one
  level only
- S1 lands and a pin survives on a job type for a reason neither S1 nor S4 can
  express
- section 3's slot cost shows up on a node where no pin exists to remove

## 7. Grants: the same seam, the opposite answer (D5, D6)

Job #542 measured, from inside an evaluator container on the nuc:

> this **EVALUATOR** container holds `/var/run/docker.sock`. `DockerGrant::admits`
> matched a launch at eval level, so the node's allow-list is per job type and not
> per level.

`admits` reads `JOB_PROJECT` and `JOB_TYPE` out of the composed launch env, and
an evaluator launch carries both stamps, so the match succeeds at every level by
construction. For `docker-proof` that is harmless — it is what the proof
measured. For the `build-image` type
[#313](313-workload-identity-image-builds.md) S9 will add, it means the `ci`
evaluator also runs as node root.

The asymmetry with placement is the point. A pin is **the type's author** saying
where their work runs. A grant is **the node's operator** consenting to a
workload — and `ci` is appended by `.chug/jobs/_defaults.yaml`, not written by
the type's author, so granting a type silently grants an evaluator nobody
declared. #517 D1 accepts a node-root escalation for *"the workload the whole of
half B was argued for"*, which is a build step, not a test runner.

Hence D5: scope the match to work-level launches. An evaluator that genuinely
needs the socket is then an explicit extension rather than a side effect, which
is the same fail-closed shape #517 chose everywhere else.

**S3 must key on `CHUG_PHASE`, and the reason is the same one `admits` already
relies on.** `container_env` in
[`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs) stamps
**two** level discriminators — `CHANNEL_ROLE` (`work`/`eval`) and `CHUG_PHASE`
(`Work`/`Evaluation`) — and only the second is under a reserved prefix.
[`docs/spec.md`](../spec.md) §4.1 and §5.3 seal `JOB_` and `CHUG_` and nothing
wider, so a job type's `vars:` may legally declare `CHANNEL_ROLE`. An
implementation reaching for the nearer-looking name would hand a job type a way
to declare itself work level and re-obtain node root — the exact hole the
reservation exists to close, reintroduced by the slice that was meant to narrow
the grant. `CHUG_PHASE`, or a new `JOB_`-prefixed stamp; never `CHANNEL_ROLE`.

**D6, and why the two answers agree on granularity and differ on authority.**
The obvious symmetry would be to make grants a placement input too — advertise
what a node would grant, and let `choose_placement` avoid nodes that will refuse.
It is declined here for two reasons and one non-reason:

- It puts the operator's allow-list on the wire, into every fleet view and every
  ping payload. #517 D2 keeps the grant in the node's own environment file
  precisely so that no merge and no API call can move it; advertising it does not
  move it, but it does publish it.
- It invites the scheduler to *enforce* what only the node may enforce. The
  moment placement filters on a grant, a filtering bug reads as a grant, and the
  property that a node's allow-list is a statement the node alone controls stops
  being structural and starts being a convention two crates share.
- The non-reason: it is not needed for correctness. `DockerGrant::admits`,
  `HostTenancy::admits` ([`crates/container/src/host.rs`](../../crates/container/src/host.rs))
  and the KVM and nix allow-lists all fail closed at launch, so a
  misplacement is a refused launch, never a silent grant.

So a grant stays node-enforced and placement-blind, and the pin stays the honest
expression of a grant-gated requirement — which is exactly why S2 drops one pin
and not three.

Four node-side grants share this shape, and what they share is that **none of
them can see a level** — every one admits work and evaluator launches alike.
Three key on `JOB_PROJECT` alone: `WORKER_HOST_PROJECTS`
([`crates/worker/src/config.rs`](../../crates/worker/src/config.rs), enforced by
`HostTenancy::admits`), `WORKER_KVM_PROJECTS` and `WORKER_NIX_PROJECTS`
([`crates/worker/src/nix.rs`](../../crates/worker/src/nix.rs)), each parsing
`owner/project` entries. The fourth, `WORKER_DOCKER_GRANTS`, already keys one
step finer — `owner/project:job_type` — and is level-blind anyway, which is
finding 4 in one line: refining the key along the *type* axis never reached the
axis that mattered. D5 narrows
the last and **not** the other three, deliberately: the socket is the one whose
key #517 D3 calls root-equivalent, and `/bin` access to a node's docker daemon is
categorically unlike a device node (#367's analysis), a tenancy boundary, or a
nix store the node already trusts. Narrowing a tenancy boundary to work level
would in fact be wrong — an evaluator of a host job type must be admitted by the
same tenancy its work was, or the job cannot finish. They are named so the next
reader knows they were considered and why the answer differs.

One latent consequence of D6 is worth stating before it bites. `mac-proof` is
unpinned by S2 and placed by mode plus `envs` — but `WORKER_HOST_PROJECTS` is a
grant placement cannot see, so the day a **second** host node joins the fleet
carrying `xcode:26.5` and a different tenancy, placement may route the proof to a
node that refuses it at launch. Today that is unreachable (one host node), the
refusal is loud, and #309 §3.5's queue-and-retry is what handles it. It is the
first case that would reopen D6, and it should be measured before a second host
node is admitted rather than after.

## What this does not decide

- **Whether `features` and `envs` share a wire shape.** D3 says one mechanism;
  the encoding is S4's. What S4 must not do is invent a second matcher — it
  should be S1's predicate with a second source list, or the two will drift.
- **How a node advertises a feature.** Correction 7 establishes that S4 needs an
  advertisement; whether that is a new `features` list, a repurposing of the
  empty `leases` vector, or a discovered set built the way `envs` is, is S4's to
  decide. Two constraints hold whichever it picks: it is additive and
  `#[serde(default)]`-absent-means-nothing (so no worker-RPC epoch), and it is a
  *discovered* fact rather than an operator assertion, or it becomes a second
  grant surface D6 rejects.
- **Whether `agent_cli` becomes a placement input.** Section 4 names it as the
  next instance of the same pattern; no job type today can be misplaced by its
  absence, so nothing here acts on it. It should be decided when the first
  agent-shaped host job type outside `mac-proof` is written.
- **Whether an unmeetable requirement should ever escalate rather than queue.**
  S1 queues, following the mode precedent. A requirement no node will ever
  advertise queues forever, which is #309 §5a's existing behaviour and not made
  worse here.
- **What placement does about a node whose advertisement is stale.** The daemon
  re-announces on capability change; nothing reconciles a node that lied. Out of
  scope, and unaffected by S1.
- **The disclosure question D6 rests on.** Whether advertising a grant set is
  acceptable, and whether an advisory grant filter could stay advisory, is a real
  question this document answers with "not now" rather than "never". What must be
  measured first: whether any fleet surface (`ping` payloads, the fleet view)
  is readable by anyone the allow-list is not already known to.

## Corrections (verified against the tree)

1. **A host job type does not require a pin.** #309 P0's row describes the
   prototype as *"routed by `placement.node`, `slots: 1`"*, which was true of P0
   and is read by later readers as a standing rule. It is not one: `validate()`
   has no such rule, and the job-type test asserting per-task placement parses a
   host type carrying no `placement` and expects no error. The pins in
   `.chug/jobs/` are choices.
2. **`NodeCapabilities.envs` has no consumer, and is narrower than its name.**
   [#322](322-macos-native-runtime.md) W4's row reports the discovered set as
   *"advertised as `NodeCapabilities.envs`"*, which is accurate, and reads as
   though placement uses it. It does not; the only match performed on a
   `runtime.env` is the node's own resolution at launch. It is also **only** the
   Xcode set — `node_capabilities` builds it from `xcodes.envs()` alone — so
   "environments a node serves" describes an intent the field does not yet have.
3. **`docker_reachable` cannot substitute for a grant at placement.** #517 S4 is
   explicit that the field claims the node reaches a daemon and not that a launch
   is granted one — correct, and it means the field cannot be used to place
   `docker-proof`. (The field is `NodeCapabilities.docker_reachable`;
   [`crates/worker/src/docker_access.rs`](../../crates/worker/src/docker_access.rs)
   is the module that probes it, and the draft this reworks confused the two
   names.) The pin is load-bearing under D6, and removing it on the strength of
   this field would place the job on a node where it silently receives nothing.
4. **`WORKER_HOST_PROJECTS` is in the source, and job #525 is what put it there.**
   This job's own brief lists it among controls "named by five docs and absent",
   inheriting a finding that was already closed. In this tree it is
   `WorkerConfig::host_projects` in
   [`crates/worker/src/config.rs`](../../crates/worker/src/config.rs), parsed from
   the environment and enforced by `HostTenancy::admits` / `refusal` in
   [`crates/container/src/host.rs`](../../crates/container/src/host.rs) as a hard
   `BackendError::Launch`; `git log -S` names `job/525: code` as the commit that
   introduced it. The pattern in section 4 is real; this member of it is closed,
   and the brief's instruction to re-verify rather than inherit is exactly what
   catches it.
5. **`envs` is the xcode set, so S1's reach is one job type.** The draft this
   reworks asserted that S1 would make `android-proof`'s pin removable too.
   `android-proof` declares a top-level `image:` and no `runtime:` block, so it
   has no `runtime.env` for any matcher to read; dropping its pin would place it
   wherever the fleet is freest and fail rung 1 — the failure its own comment
   predicts in as many words. S2 drops one pin.
6. **The last schema epoch was spent eight jobs ago, not by #401.** This job's
   brief prices a `placement` field with *"#401 spent the last one"*, and the
   draft this reworks repeated it. `git log -L` on the constant in
   [`crates/types/src/version.rs`](../../crates/types/src/version.rs) reads:
   `job/314` (job `inputs:`), `job/376` (schedule `inputs:`), `job/401` (the
   `runtime:` block), `job/413` (`workload_identities:`) and `job/535` (the
   per-agent `tools:` grant, [#533](533-molt.md) S1) — five bumps, the most
   recent **eight jobs before this one**. The epoch is therefore a *routine but
   declared* cost, spent about once per feature that cannot be tolerated
   additively, not a rarity. That cuts against the draft's framing and mildly
   *for* S4: what makes the epoch a real cost is the `min_dispatcher` rule it
   forces on every config that uses the field, not its scarcity.
7. **Nothing in this tree advertises `/dev/kvm`, so #367 A4 is two halves.**
   A4 reads as a job-type field plus *"a capability predicate"*, and the draft
   this reworks wrote as though the node already advertised the capability.
   `node_capabilities` in
   [`crates/worker/src/daemon.rs`](../../crates/worker/src/daemon.rs) builds
   `modes`, `platform`, `resources_enforced`, `envs`, `agent_cli` and
   `docker_reachable`, and sets `leases` empty; no field names a device. The one
   `leases: ["kvm"]` in the tree is a `host_capable()` test fixture in
   [`crates/types/src/worker.rs`](../../crates/types/src/worker.rs) — which is
   exactly how the misreading happens. S4 must add the advertisement before the
   matcher has anything to read; section 6 prices both.

## Note — 2026-08-10 — S1 landed: `envs` has a reader (job #550)

`choose_placement` now filters on the environment a launch declares, as section
5 specified: `LaunchRequirements` carries the level's resolved `runtime.env`
verbatim, `PlacementCandidate` carries the node's `NodeCapabilities.envs`, and
`serves_env` sits in the candidate loop beside `serves(mode)` and `bounds`. The
refusals are the two section 5 asked for — unpinned and unmeetable is
`NoCapacity` naming the reference and every node's advertisement, pinned and
unmeetable is a hard `Launch` error naming the reference and that node's set. No
schema field, no epoch, no `.chug/jobs/` edit; `mac-proof` still pins, which is
S2's to remove.

**The one decision the slice had to make, and S4 inherits it.** Section 5 says a
`nix:` reference must never be matched this way, which leaves two readings of
"node-interpreted": everything that is not `nix:`, or the scheme set a node
actually advertises. It is the second — `types::job_type::env_is_node_advertised`
is `xcode:`-prefixed and nothing else — because `envs` is built from
`xcodes.envs()` alone (section 5), so under the first reading a reference in
some future third scheme would match against a set no node ever puts it in and
queue forever, replacing a loud node-side `unservable_scheme` refusal with a
silent wait. The cost is the mirror of that: a scheme added to the node's
resolver and to `envs` without being added to this predicate is placed
unfiltered, exactly as today. That predicate is the seam #543's "what this does
not decide" reserves for S4 — a second source list on one matcher, never a
second matcher.

## What S3 landed (2026-08-10, job #551)

D5, and only D5. The placement half of this document is untouched: no
`PlacementCandidate` field, no matcher, no pin removed, and no schema epoch.

- **One condition, ahead of the pair.** `DockerGrant::admits`
  ([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs))
  returns false unless the launch env carries `CHUG_PHASE=Work`, then matches
  `(JOB_PROJECT, JOB_TYPE)` against the allow-list as before. A launch carrying
  another level, an empty one, or no level stamp at all is admitted by nothing —
  the fail-closed reading a missing project or type already had.
- **Section 7's trap, avoided by construction rather than by care.** The level is
  read from `CHUG_PHASE` and never from the `CHANNEL_ROLE` beside it, because
  only the first is under a prefix `docs/spec.md` §4.1 seals. The two literals
  the dispatcher stamps and the node matches are now pinned against each other by
  a test (`exec::tests::the_level_stamp_a_node_side_grant_scopes_on_is_spelled_once`),
  which reads `container::docker::PHASE_ENV` and `PHASE_WORK` — exported for that
  purpose — so a node reading `Work` while the dispatcher stamps something else
  fails a unit test rather than failing open on a live node.
- **Wrap-up keeps the socket; a merge-gate re-run does not.** A `wrap_up.run`
  command launches under `ChannelRole::Work`
  ([`crates/dispatcher/src/eval.rs`](../../crates/dispatcher/src/eval.rs)) and so
  stamps `Work`, while the merge gate re-runs *evaluators* and stamps
  `Evaluation`. That is the authorship line D5 argues from rather than an
  accident of the enum: `wrap_up:` is the type author's own block, `ci` is not.
  An operator-dispatched **triage** agent
  ([`crates/dispatcher/src/forge_ingest/triage.rs`](../../crates/dispatcher/src/forge_ingest/triage.rs))
  stamps `Evaluation` as well and so receives nothing — the case where the old
  behaviour handed node root to an *agent* nobody declared, and the one this
  slice is most worth having for.
- **The tests are the level cases, and each was red before the change**
  (`container::docker::tests::a_docker_grant_reaches_work_level_launches_only`):
  work admitted, `Evaluation` refused, no stamp refused, a lowercase `work`
  refused, a launch declaring `CHANNEL_ROLE=work` at `CHUG_PHASE=Evaluation`
  refused, and a job type the allow-list never named refused at every level. The
  node-declares-no-grant case stays byte-identical and is still asserted where it
  was. A `debug_assert` on the built `HostConfig` states the negative space: the
  socket rides no launch at any level but work, whatever the allow-list says.
- **What a `docker-proof` run should now print**, since its `identity` evaluator
  exists to report exactly this: `no /var/run/docker.sock here, so the grant is
  work-level as #543 S3 scoped it`, and no finding. The work container is
  unchanged. A socket still present in that evaluator means the node's
  `chug-worker` predates this change — the daemon composes the bind, so it clears
  with a worker deploy — and the script names that cause first and still exits 0,
  because failing there would measure the fleet's version rather than the branch.
- **One knowingly-stale sentence is left behind, and it is left on purpose.** The
  `eval:` comment in
  [`.chug/jobs/docker-proof.yaml`](../../.chug/jobs/docker-proof.yaml) still
  reads that this is "the measurement nobody has decided (#517)" and that "the
  grant looks per job type rather than per level", which D5 decided and this
  slice made false. The ticket's acceptance criteria forbade editing
  `.chug/jobs/*.yaml` at all, and no gate reads a YAML comment, so the debt is
  recorded here rather than fixed quietly: the next job to touch that file —
  [#313](313-workload-identity-image-builds.md) S9's `build-image` type is the
  expected one — repoints it at
  [`.chug/tasks/docker-proof-identity.sh`](../../.chug/tasks/docker-proof-identity.sh)'s
  rewritten header.
- **The other three grants stay level-blind**, per section 7: narrowing
  `WORKER_HOST_PROJECTS` would break a host job type whose evaluators must be
  admitted by the same tenancy its work was, and `/dev/kvm` and the nix store are
  not in the socket's class.

## What S2 landed (2026-08-10, job #556)

One pin, in one file, exactly as section 5 measured: the `placement:` block is
gone from [`.chug/jobs/mac-proof.yaml`](../../.chug/jobs/mac-proof.yaml) and
nothing else in `.chug/jobs/` is touched. No schema field, no epoch, no source
change to `choose_placement` — S1 built the whole mechanism and this slice is
the config catching up to it.

- **The comment was rewritten, not deleted, and that is half the slice.** The
  file argued *for* the pin in its own words — *"an unpinned release could
  satisfy `host` on some future second Mac and prove nothing about this one"* —
  and a comment left standing over a removed block is how a later reader
  restores it. It now argues for the match: a second Mac carrying a different
  Xcode does not advertise `xcode:26.5` and cannot take the proof, one carrying
  26.5 is a legitimate host for it, and the subject was always the toolchain
  rather than the machine.
- **Three things were checked before the block came out**, because *placeable*
  and *servable* are separate questions. The declared `runtime.env` is
  `xcode:26.5`, which `types::job_type::env_is_node_advertised` reads as
  node-interpreted (the `xcode:` scheme S1 fixed on); `gumbo-air-0` advertises
  exactly that set and is the only node doing so — an `envs` entry is what
  `crates/worker/src/xcode.rs` discovers on the node it runs on (job #489), and
  the air is the only Mac in the fleet; and it is the only node advertising
  `host` at all, which is [`deploy/prod/README.md`](../../deploy/prod/README.md)'s
  claim rather than this doc's — `WORKER_MODES` names runtimes and never a
  count, `container` is its default and what the whole fleet runs, and
  `gumbo-air-0` has advertised `host` since 2026-08-08. **No second host-capable
  node existed when this ran**, so the removal is safe under either dispatcher
  generation: one carrying S1 holds the work to the air by the `envs` match, and
  one predating S1 holds it there by the mode filter alone, since no other node
  serves `host`. The two readings agree today, which is what makes this slice a
  config change with no deploy ordering.
- **The `ci` evaluator is what the removal actually frees.** Section 3's sharp
  case: the pin bound *every* level, so an ordinary Linux container needing
  nothing of the machine took a host node's one node-wide slot
  ([#490](./490-agent-work-on-a-mac.md) D4, `enforce_host_capacity`).
  Unpinned it places by busyness like any other container task.
- **`android-proof` and `docker-proof` keep their pins**, for the reason section
  2's third column gives and Correction 5 restates. `android-proof` declares a
  top-level `image:` and no `runtime:` block, so it has no `runtime.env` for any
  matcher to read, and its `/dev/kvm` is advertised by no field at all — that
  advertisement is S4's first half (Correction 7). `docker-proof`'s requirement
  is a grant, which D6 keeps invisible to placement on purpose. Neither pin is
  removable by env matching, and this slice did not try.
- **The latent consequence in section 7 is now live rather than hypothetical.**
  `mac-proof` is placed by mode plus `envs`, and `WORKER_HOST_PROJECTS` is a
  grant placement cannot see, so a second host node carrying `xcode:26.5` under
  a different tenancy could be selected and would refuse at launch — loudly,
  requeued by #309 §3.5. That is the first case that reopens D6, and it should
  be measured before such a node is admitted rather than after.
- **The guard is
  [`crates/test-utils/tests/placement_guard.rs`](../../crates/test-utils/tests/placement_guard.rs)**,
  where the `envs` claim already lived. Two new tests read this repo's own
  `.chug/jobs/` against a fleet carrying a **second** Mac: the proof stays on the
  air when that Mac is equally idle but carries another Xcode, queues rather than
  falling onto it when the air is busy, and does land on it when it carries the
  same Xcode — the last being the case the pin would have refused. Both read
  `placement_node()` from the file, so restoring the pin fails them by name.
  `the_predicate_moves_no_job_type_in_this_repo` is unaffected: it compares a
  level's placement with and without its declared environment, and both sides
  move together.
- **Nothing runs by accident.** `mac-proof` is not in `.chug/jobs/_defaults.yaml`
  and is wired into no gate, so the next placement this decides is whenever an
  operator releases the type by hand.
