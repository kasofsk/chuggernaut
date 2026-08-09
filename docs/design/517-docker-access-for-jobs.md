# Design #517 — Docker access for jobs, accepted (amending #309 §10 and #313 Decision 0)

Status: IMPLEMENTED IN PART — S1 (job #518), S2 (job #521), S3 (job #522), S4 (job #519) and S5a (job #523) landed; S5b is open, S6 deferred. The grant mechanism exists, the deploy can now declare one, and nothing is granted: no node declares one, and host-mode access is already live.

Written against the tree at `ff3258a`. Every claim about current behavior below
was read out of the source and out of [`docs/spec.md`](../spec.md), not inferred
from the sibling designs; where a sibling design and the tree disagree, the tree
wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree). The measurement that
prompted the decision is job #516's read-only probe on `gumbo-air-0`, quoted
in full below and not re-run here.

This document does three things: it **records a decision the operator has
taken**, in the shape [#313](313-workload-identity-image-builds.md)'s D1–D4 were
recorded in job #409; it **amends two rules that decision contradicts**, by
appending to the docs that hold them rather than rewording them; and it
**decides the one question the measurement left open** — whether container jobs
get the socket too.

## Current state

*The mutable head ([#415](415-knowledge-architecture.md) D2): rewritten to
current truth whenever anything below it changes. Everything after this section
is append-only.*

**Four slices are built; nothing is granted. One thing was already live.** As of
**2026-08-09**, S1 has landed: `JOB_` is a reserved secret/var prefix alongside
`CHUG_` (`docs/spec.md` §4.1, §5.3), so the `JOB_PROJECT` every node-side
allow-list matches on can no longer be moved by a job type's `vars:` — which
also closes [correction 3](#corrections-verified-against-the-tree) against the
**shipped** KVM grant. S2 has landed under that seal: every launch now carries
`JOB_TYPE`, so the `(project, job type)` key S3's allow-list needs is observable
at the node, and [correction 2](#corrections-verified-against-the-tree) is
historical. S4 has landed beside them, advertising the access. **S3 has landed
on top of all three**: `container::docker::DockerGrant` binds the node's socket
into the launches a node's own `WORKER_DOCKER_GRANTS` names, matched on both
stamps, failing closed everywhere else. It grants nothing today — **no node
declares a socket or an allow-list**, which is S5 — so every launch on the live
fleet is byte-identical to what it was.

Agent host tasks on `gumbo-air-0` reach a working docker daemon, and every
[`.chug/jobs/mac-proof.yaml`](../../.chug/jobs/mac-proof.yaml) run since
[#490](490-agent-work-on-a-mac.md) slice 6 has had that access. It was never
granted and it is not declared anywhere; it is a consequence of the task user
owning the colima socket. That is the production posture this document accepts
rather than closes.

**S5a (job #523) makes a grant declarable without hand-editing a node.** The
deploy composes the node's whole run spec, so until it forwarded these two knobs
the only way to declare one was an edit the next deploy overwrote. It still
grants nothing — no `chuggernaut.env` in this repo names a socket or an entry —
and a node declaring neither produces a byte-identical run spec, asserted in
`deploy/prod/build-worker.test.sh`. What is left of S5 is the operator's own
config (S5b).

**[Correction 1](#corrections-verified-against-the-tree) is closed (job #525,
2026-08-09).** `WORKER_HOST_PROJECTS` exists: fail-closed, enforced at every
host launch in `container::host::HostTenancy`, and refused at the deploy when a
node declares `host` with no list. The correction stands as written — it was
accurate against the tree it measured — and the containment story D1's
acceptance leans on is now the one the docs assert. It grants nothing here
either: no node declares a tenancy in this repo, and `gumbo-air-0`'s is the
operator's to declare
([`docs/reference/runbooks/worker-host-projects.md`](../reference/runbooks/worker-host-projects.md)).

**S4 (job #519) makes that posture visible.** Every daemon now probes at boot
whether it reaches a docker endpoint and advertises the answer as
`NodeCapabilities.docker_reachable`, for both modes, defaulting false. Nothing
is granted, withheld or bound by it — it is an audit record. S3's grant
mechanism has since landed and no node declares one yet (S5), and withholding
host-mode access is still S6.

| # | Decision | Argued in |
| --- | --- | --- |
| **D1** | **Jobs may use docker, and this is wanted rather than tolerated.** The escalation to node root is **accepted, not mitigated**, under a stated condition | [The decision](#the-decision-and-the-argument-that-carries-it), [The cost](#the-cost-stated-precisely), [The trigger](#the-revisit-trigger-stated-as-a-condition) |
| **D2** | **The mechanism stays node-side.** [#309 §10](309-host-native-execution.md#10-trust-and-tenancy)'s *shape* clause survives intact — a node-side allow-list entry, never a job-type field the platform honors on request. Only the default and the justification invert | [What survives of §10](#what-survives-of-the-309-rule) |
| **D3** | **Container jobs get the socket, allow-listed node-side per (project, job type),** failing closed. This is [#313](313-workload-identity-image-builds.md) **B-I**, whose rejection is hereby reversed — not a fifth option | [The container question](#the-question-this-job-decides-container-jobs), [Naming the shape](#naming-the-adopted-shape-honestly-this-is-b-i) |
| **D4** | **Host-mode docker becomes advertised, not enforced.** The node reports the access as a capability so it is auditable; withholding it needs per-task users and waits on [#309](309-host-native-execution.md) P3 | [The host question](#the-host-question-ambient-access-and-what-can-actually-be-done-about-it) |

**The revisit trigger, and it is nearer than it looks.** D1 holds *while every
job runs code the operator wrote or vendored*. It stops holding the moment a job
runs untrusted code. `docs/spec.md` §5.3's linked-origin sync already
fast-forwards `integration` onto an origin main that took external commits, and
`integration` is the base every job branch is cut from — so for a linked project
whose origin accepts third-party merges, the condition is **satisfied today**,
not at some future adoption. See [the trigger](#the-revisit-trigger-stated-as-a-condition).

| Slice | Content | State |
| --- | --- | --- |
| **S1** | Make the node's allow-list key unshadowable: reserve the dispatcher-composed `JOB_*` stamps the way `docs/spec.md` §5.3 reserves `CHUG_`, or move the matched name under that prefix. **Prerequisite for S3, and a fix to the shipped KVM grant** ([correction 3](#corrections-verified-against-the-tree)) | **Landed** (job #518) — the first fix, as a prefix: `exec::reserved_env_prefix` is the one decision site release validation refuses on and injection skips on. `BASE_BRANCH`/`REPO_URL`/`NATS_URL` deliberately stay declarable ([why](#which-of-the-two-fixes-s1-took-job-518-2026-08-09)) |
| **S2** | Put the job type's name on the launch: one dispatcher-composed env entry in `container_env` ([`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs)). No schema field, no epoch, no `WORKER_RPC_VERSION` bump | **Landed** (job #521): `JOB_TYPE`, composed with the base stamps and sealed by S1's prefix. None of the three costs was spent ([what S2 landed](#what-s2-landed-job-521)) |
| **S3** | A `DockerGrant` beside `KvmGrant` ([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs)): a node-side socket path plus a `(project, job type)` allow-list, bound into matching launches only, empty granting nobody | **Landed** (job #522): `WORKER_DOCKER_SOCKET` + `WORKER_DOCKER_GRANTS`, fail-closed at parse, at boot and at every launch. `docs/spec.md` §3.1's owed amendment landed with it ([what S3 landed](#what-s3-landed-job-522)) |
| **S4** | Advertise the access on `NodeCapabilities` ([`crates/types/src/worker.rs`](../../crates/types/src/worker.rs)) for **both** modes, defaulting false so a daemon predating the field promises nothing. D4's audit half | **Landed** (job #519): `docker_reachable`, additive with no `WORKER_RPC_VERSION` bump, probed by `worker::docker_access` at boot for both modes and never a boot refusal. See [what S4 landed](#what-s4-landed-job-519) |
| **S5a** | *Deploy plumbing:* forward `WORKER_DOCKER_SOCKET` and `WORKER_DOCKER_GRANTS` through [`deploy/prod/build-worker.sh`](../../deploy/prod/build-worker.sh), with pre-flight refusals mirroring the daemon's, and the operator runbook | **Landed** (job #523): both per-node overridable, unset byte-identical, four refusals ahead of the restart. The containerized-daemon precondition S3 left open is answered ([what S5a landed](#what-s5as-deploy-plumbing-landed-job-523)) |
| **S5b** | *Node config:* the allow-list entry itself and the pin on one builder node — [#313](313-workload-identity-image-builds.md) S8, minus the proxy | Proposed — operator config, and the pin waits on [#313](313-workload-identity-image-builds.md) S9's `build-image` |
| **S6** | *Deferred:* per-task users ([#309](309-host-native-execution.md) P3 §8), the only mechanism that can withhold host-mode docker | Deferred — D4's enforcement half |

[#313](313-workload-identity-image-builds.md) S6 (the operator's provider
registration), S7 (a registry confirmed), S9 (`build-image`) and S10
(`promote`/`rollback`) are **unaffected** and keep their own numbering; only its
S8 changes content.

---

## What was measured, 2026-08-09 (job #516)

A read-only probe, run as a host task on `gumbo-air-0`:

```
docker info  → exit 0   colima, Server 29.5.2, 1 container, 8 images
docker ps    → exit 0
DOCKER_HOST  → unset
active context: colima → ~/.colima/default/docker.sock
socket: exists, mode 0600, owned by worksalot:staff — the user host tasks run as
/var/run/docker.sock: absent; no `docker` group exists on the node
```

**Nothing granted this, and the control that was supposed to prevent it works.**
[`crates/container/src/host.rs`](../../crates/container/src/host.rs) composes a
host task's environment rather than inheriting it: `task_env` starts from a
two-name floor, adds the rebased launch env, the workspace and the exit paths,
and `spawn_task` clears the daemon's environment first. Its test
`a_host_task_inherits_nothing_the_dispatcher_did_not_declare` asserts the exact
key set and asserts `DOCKER_HOST` is not in it. That test genuinely holds. The
probe agrees with it — `DOCKER_HOST` was unset.

The access arrives by a different route, and the route is the point:

1. The floor `task_env` carries from the daemon is `PATH` and `HOME` (the
   `INHERITED` array in the same file), and `HOME` is the daemon's.
2. On this node the daemon runs as the login user, so `HOME` is that user's.
3. The docker CLI needs no `DOCKER_HOST`. Absent one it resolves the **active
   context** from `~/.docker/config.json` under `HOME`, and that context names
   `~/.colima/default/docker.sock`.
4. The socket is mode `0600` owned by that same user, because colima runs under
   the same login. Access is by **file ownership** — not group membership, and
   not environment.

> **An environment-composition guarantee bounds what a task is *told*. It says
> nothing about what the task's uid may *open*.** The two are different
> questions and only the first was ever answered.

That is a [`docs/reference/style.md`](../reference/style.md) Tier 2 rule 7 error
one layer down. The rule says to re-derive every host fact inside the namespace
that will use it, and it names existence, identity and provenance as three
separate questions. There is a fourth this instance adds: **reachability by
uid**, which is invisible to all three. The host task's namespace *has* the
socket; nobody asked it, because the environment answer looked like the whole
answer.

## What this falsifies

**[#309 §10](309-host-native-execution.md#10-trust-and-tenancy)** says: *"host
tasks do not get the docker socket … A job type that needs one is a node-side
allow-list entry, never a job-type field the platform honors on request."* The
first clause is **false on this node as configured**, and has been since #490
slice 6 put agent host work on the air. The rule was written as a prohibition
and read as a statement of fact; it was neither — it was an unenforced
intention, and the node's uid quietly settled the matter the other way.

**[#313 Decision 0](313-workload-identity-image-builds.md#decision-0-the-308-vs-309-contradiction-resolved)**
resolved the #308-vs-#309 contradiction by ruling that *"#308's premise is
wrong"*. [#308 §D](308-gha-port.md#d-image-build-and-push-5-workflows--open)'s
premise — *"a host node has a real docker daemon … and the question stops being
interesting"* — was **right about the capability** and wrong about the
consequence. Decision 0 was **right that the capability must not be
accidental**, and its three counts against "dissolves" all survive: the gumbo
analogy still does not transfer to a mixed-mode node, a daemon still answers
*may I build* and not *may I push*, and there is still nothing to push to. What
Decision 0 got wrong is narrower than its own framing: it treated the socket's
absence as a fact it could reason from, and the socket was present.

The measurement is what separates the two claims. Neither doc was in a position
to know: nothing in the tree observes a node's docker reachability from inside a
task, which is exactly what S4 exists to fix.

## The decision, and the argument that carries it

> **Jobs may use docker. This is wanted, not merely tolerated.**

The operator's reasoning, recorded as the argument rather than paraphrased into
a conclusion:

1. **Real workloads need it.** beacon runs OpenAPI client generation in docker,
   and there are many such uses. An agent job being able to run a docker command
   is a capability a forge should have, not a hole to plug. A forge that cannot
   run the build its consumer already runs is not a smaller attack surface; it
   is a forge the consumer does not move onto.
2. **It is parity, not a regression.** beacon's builds already ran on this same
   machine as this same user, under a self-hosted GitHub Actions runner — a
   self-hosted runner *is* this property, and extends exactly this trust. Moving
   beacon onto chuggernaut adds no exposure class it did not already have.
3. **Chuggernaut v2 is a per-consumer forge** — single-tenant, running the
   operator's own projects. `CLAUDE.md` states this as a design premise, and
   [#309 §10](309-host-native-execution.md#10-trust-and-tenancy) already leans on
   it for host tenancy. Accepting the socket is that premise applied
   consistently, not an exception carved out of it.

Argument 2 is the load-bearing one and it is **secondhand**, marked the way
[#313 correction 2](313-workload-identity-image-builds.md#corrections-verified-against-the-tree)
marks its beacon claims. `~/beacon` is not in this workspace and the runner
directory is gone; what remains is residue of `~/actions-runner/_work/beacon/beacon`
in the node's `claude-cli-nodejs` cache, dated June 2026. That is evidence the
runner ran there, not a reading of its configuration. Nothing here re-derives
what the runner was permitted.

## The cost, stated precisely

**You cannot have docker without node root.** A container can bind-mount the
host filesystem, so any job holding the socket can:

1. `docker inspect chug-worker` and learn the host path of the daemon's key
   mount — `deploy/prod/build-worker.sh` runs the daemon with
   `-v $HOME/chuggernaut-worker/keys:/data/keys:ro`;
2. bind that path into a container of its own, yielding `worker.creds` (the
   node's NATS credential) and `worker_git`;
3. subscribe `req.worker.{node}.>` with that credential — and `docs/spec.md`
   §3.1 states a launch request carries "prompt, per-job credentials, harness
   config" **inline**.

So the socket does not merely grant node root. It grants the ability to
**receive other jobs' minted credentials**. This is
[#313 correction 5](313-workload-identity-image-builds.md#corrections-verified-against-the-tree)
verbatim, and it is the escalation that correction used to reject B-I.

**It is not mitigated. It is accepted.** Three things follow that a reader
should not have to infer:

- **The blast radius is the platform's own execution substrate**, not one
  project's containers. A job holding the socket on a node can read every other
  container on it, and through the node credential can reach jobs placed
  elsewhere.
- **Accepting it does not shrink it.** Nothing below narrows the capability; D3
  narrows only *which launches receive it*, and D4 makes it *visible*. A job on
  the allow-list has precisely the escalation described above.
- **The escalation is unauditable after the fact today.** No record says which
  containers held a socket. S4 is what turns that from unknown into reported,
  and it is worth doing for that reason alone even if no container is ever
  granted one.

### The revisit trigger, stated as a condition

> **This holds while every job runs code the operator wrote or vendored. It
> stops holding the moment a job runs untrusted code — a contributor's pull
> request, an imported third-party repo, any project the operator does not
> control.**

Written as a condition so it can be checked rather than felt. Two things must be
said about it, because the condition is closer to satisfied than the sentence
suggests:

- **Linked-origin sync already admits external commits.** `docs/spec.md` §5.3:
  with no open release, `integration` fast-forwards onto origin main "when it has
  nothing unreleased (external commits flow in)". `integration` is the default
  branch every job branch is cut from. So for a linked project whose origin main
  takes third-party merges, third-party code is *already* what a job runs. The
  trigger does not fire on adopting some future feature; it fires on a project
  configuration that exists. **Do not put a linked project with external
  contributors on the allow-list.**
- **Untrusted *input* is a near neighbour the trigger does not cover.**
  `docs/spec.md` §13.2's ingest delivers external event payloads into a triage
  agent's context, and §1.1's `cover_html` carries operator-supplied HTML. The
  trigger is about code an agent *runs*; an agent that runs trusted code under
  the steering of untrusted text is a different route to the same place. Named
  as a residual, not folded into the operator's trigger — widening someone
  else's decision is not this document's to do.

## What survives of the #309 rule

The rule inverts; its **mechanism clause does not**, and keeping the two apart
is what makes this an amendment rather than a repeal.

| Clause | Status |
| --- | --- |
| "host tasks do not get the docker socket" | **Inverted.** False as measured, and the default is now the other way |
| "a node-side allow-list entry" | **Kept**, and made the mechanism for containers too (D2, D3) |
| "never a job-type field the platform honors on request" | **Kept, unweakened.** See [C4 below](#the-question-this-job-decides-container-jobs) |
| The blast-radius table's docker row | **Kept as analysis**, with its verdict reversed rather than its facts |
| "Are host nodes single-tenant? Yes, by policy, and enforced at the node" | **Half wrong** — see [correction 1](#corrections-verified-against-the-tree). The policy is stated; nothing enforces it |

The distinction that keeps the mechanism clause meaningful under D3: **a
node-side allow-list that names a job type is the node consenting to a name the
project chose. A job-type field is the project granting itself node root.** The
first requires an operator edit on the node for every grant; the second requires
a merge. [#367 §2.1](367-android-emulator-execution.md)'s table row — "Should
[the docker socket] ever be a job-type field? **No, permanently.** #309 §10 is
right and it is not a phasing statement" — **stands**, and this document does
not reopen it. Only the premise that the socket is absent changes; #367's
`/dev/kvm`-versus-socket comparison, including its "What it grants" row, is
untouched.

## Naming the adopted shape honestly: this is B-I

[#313 B1](313-workload-identity-image-builds.md#b1-the-build-mechanism) weighed
four shapes and wrote of the first:

> **B-I — raw docker socket bound into a pinned job type's containers (#308
> D1).** … *Against:* correction 5 — the socket yields `docker inspect
> chug-worker`, the host path of its `:ro` key mount, and from there the node's
> NATS credential and git key … **Rejected.**

**That rejection is reversed.** The shape adopted here is B-I: the real socket,
bound node-side into the containers of allow-listed launches, with the
escalation consciously accepted. It is not a fifth option and presenting it as
one would be a way of not saying that a rejection was overturned.

**No fact in B-I's "against" is now false.** What changed is the acceptance, and
the change has a date and an owner. The two clauses of that rejection deserve
separate treatment:

- The credential escalation is **accepted** (above).
- "It also breaks §10.1's *No host volume mounts* and narrows §3.1's single
  documented bind exception, which is justified there by carrying *no job
  state*" — this is **still true and still a real cost**. `docs/spec.md` §3.1's
  exception is a "small closed class of worker-provisioned node properties",
  each justified by a property the socket does not have. Adding the socket to
  that class widens it from *accelerators and read-only toolchains* to *a
  capability*. A spec amendment is owed and is a `docs` job, not this one's; the
  honest reading in the interim is that the class has a third member whose
  justification is a decision rather than a property.

### The ladder this preserves

Superseding B-IV **without deleting its argument** is what keeps the fallback
cheap. If [the trigger](#the-revisit-trigger-stated-as-a-condition) fires, the
pre-argued escalation path is already written and priced:

1. **B-I** — the real socket, node-side allow-list. *Adopted.*
2. **B-IV** — the daemon's socket behind a deny-by-default filtering proxy;
   `POST /build`, `POST /session`, push, tag, image-inspect permitted,
   `/containers` denied. Its cost analysis (a filter is only as good as its rule
   list; cross-project cache poisoning; invisible to config so a mis-placed job
   fails at the build command) stands unedited.
3. **B-III** — a build op on the worker daemon, the `refresh` precedent
   generalized. Best isolation; a second execution lifecycle inside the daemon.
4. **B-II** — rootless buildkit in an ordinary container. Needs launch-path
   security options and turns the local cache into a registry cache.

Read that as a ladder with the platform's current rung marked, not as a history
of discarded ideas. #313 B1's own advice — "if the proxy's allow-list proves too
coarse, escalate to B-III, not to B-I" — inverts in direction and keeps its
ordering.

## What half B reduces to

With B-I adopted, most of [#313](313-workload-identity-image-builds.md) half B
collapses:

| Piece | Fate |
| --- | --- |
| **B-IV's proxy, its verb allow-list, its pinned proxy image** | **Superseded.** No proxy is built. The reasoning stays standing as rung 2 of the ladder |
| **B-IV's `NodeCapabilities` dependency on #309 for the *diagnostic*** | **Kept, and generalized** — S4 advertises the access itself, which serves the same "fails at the build command" complaint |
| **S8** | **Reduced** from "proxy + allow-list + `placement.node` pin" to "allow-list + pin". Becomes this document's S5, gated on S3 |
| **B2 — registry auth falls out of half A** | **Unchanged, and now the whole of the build mechanism.** Its sketch is the shape: authenticate with half A's credential, `docker build`, `docker push`. Its two non-obvious properties survive verbatim — registry auth rides the request (`X-Registry-Auth`) so the node holds no standing push credential, and the IAM binding stays per (project, job type, container) |
| **B3 — build cache** | **Unchanged in conclusion, simpler in argument.** The cache was never the job container's; it is BuildKit's, on the daemon, already exercised on every deploy and already pruned. B3's obligations (per-project cache `id`s; the existing `--keep-storage` prune must cover the new volume) survive intact — the shared cache is still not a boundary |
| **B4 — tagging discipline** | **Unchanged, and good regardless of how docker is reached.** Push `{repo}:{sha}` first, record `{repo}@sha256:{digest}` as the task's `structured` result, move `:latest`/`:prod` only afterward and only as a separate job keyed on a digest input. This is what fixes the two beacon workflows #308 found pushing `:latest` with no rollback handle |
| **S6, S7** | **Unaffected.** The provider registration and the registry confirmation are operator work that no mechanism change touches |
| **S9, S10** | **Unaffected in content**, and their S8 dependency gets cheaper |

**Half B's shape after this document:** a command job type that authenticates
with half A's injected credential, runs `docker build` against the node's own
daemon through a bound socket, pushes by SHA, and records the digest. That is
B2's sketch plus a node-side bind, and it is most of what #308 §D asked for
three documents ago.

## The question this job decides: container jobs

Tonight's measurement is about **host** tasks. Most agent jobs are **container**
tasks and have no socket at all. The operator's intent — agents should be able
to run docker commands — reaches further than host mode, so this document must
decide it rather than inherit it.

### What the node can observe today

This is the constraint that shapes the options, and it is not what the sibling
designs assume.

- **The job type's name is not on the launch, anywhere.**
  `ContainerLaunchConfig` ([`crates/container/src/lib.rs`](../../crates/container/src/lib.rs))
  is `{ image, cmd, env, files, cpu_limit, memory_limit, node, runtime_env }`.
  The dispatcher-composed env (`container_env` in
  [`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs))
  carries `JOB_ID`, `JOB_PROJECT`, `JOB_BRANCH`, `BASE_BRANCH`, `REPO_URL`,
  `NATS_URL`, the channel-role stamps and the task-origin stamps — and no type
  name.
- **So `JOB_PROJECT` and `image` are the whole observable set**, which is what
  `KvmGrant::admits` ([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs))
  matches on and what [#367](367-android-emulator-execution.md) correction 5
  recorded. Every node-side allow-list in the tree —
  `WORKER_KVM_PROJECTS`, `WORKER_NIX_PROJECTS` — keys on the project alone.
- **Keying on the job type is therefore a change, and a cheap one.** One entry
  added in `container_env` (S2). The env is already a `HashMap` on the launch,
  so this costs no schema field, no `CONFIG_SCHEMA_EPOCH` bump and no
  `WORKER_RPC_VERSION` bump. It is small, but it is not nothing, and it is the
  first time a node keys policy on something other than a project.

### Options

**C1 — nothing. Host mode only, containers unchanged.**
*For:* zero work, and the measured posture is already this. *Against:* it makes
docker access a property of which execution mode a job type happened to choose,
which is an accident rather than a policy — and mode is chosen for Xcode and
emulators, not for docker. Most agent jobs are containers, so C1 delivers
approximately none of the intent. **Rejected.**

**C2 — every container on a docker-enabled node.**
*For:* zero config, the `WORKER_CACHE_DIR` shape exactly, and the cheapest thing
that could work. *Against:* every `code` job, every agent evaluator and every
other project's containers on that node get node root. This is where the cost
changes character rather than degree: D1 accepts an escalation for **workloads
that need docker**, and C2 extends it to every workload that happens to be
co-placed. [#367](367-android-emulator-execution.md)'s D1 was rejected for the
same reason at a much lower stake. **Rejected.**

**C3 (recommended) — node-side allow-list, keyed per (project, job type).**
The `KvmGrant` shape: a node-side socket path plus a list of
`owner/project:job_type` entries; the bind is added only for launches that
match; an empty list grants nobody, so enabling the socket on a node is one act
and granting it to a workload is another.
*For:* it is D2's mechanism verbatim, so it complies with #309 §10's surviving
clause rather than carving it out. It costs the dispatcher one env entry and the
schema nothing. It fails closed at every layer — no socket configured, no
allow-list, no match. And blast radius stays scoped to the job types the
operator actually chose, which is what makes D1's acceptance a per-workload
decision instead of a fleet-wide one.
*Against, honestly:*
- **It is invisible to the project's config.** A job type needing docker on a
  node without the entry fails at the docker command with `Cannot connect to the
  Docker daemon` — loud, late, and diagnosable only from the container log. This
  is B-IV's own complaint and it transfers unchanged; S4 is the mitigation, and
  `placement.node` is the interim answer.
- **It puts a project-chosen string in operator config.** Renaming a job type
  silently revokes its grant. That is the failure mode to prefer (revocation,
  not escalation), but it is a real operational trap and belongs in the node's
  runbook.
- **The matched name is shadowable today** — see below, and S1.

**C4 — a job-type field, e.g. `runtime.docker: true`.**
*For:* the honest end state on paper. The requirement would live in the project
repo where `CLAUDE.md`'s per-consumer-forge principle wants config to live; it
would be reviewed through the merge gate like any other job-type change; and it
would degrade into a placement predicate when a second docker node appears.
*Against, and decisive:* it is exactly the shape #309 §10's surviving clause
forbids, and the clause is right for a reason that D1 does not touch. A field
the platform honors on request means a **merge** grants node root — and the
merge gate is agent-driven, on a repo whose main branch a job's own evaluators
approve. That is a self-granting loop; the node-side entry is not, because it
requires an act outside the system being granted access to. It would also cost a
`CONFIG_SCHEMA_EPOCH` bump on its own (`deny_unknown_fields` on the nested
blocks — [`crates/types/src/job_type.rs`](../../crates/types/src/job_type.rs)),
which is a real price for the wrong shape. **Rejected, and #367 §2.1's
"permanently" is not weakened.**

### Which job types

**This document decides the mechanism and names no job types.** Job types are
project-owned, repo-versioned config; picking them is the operator's act, and
the allow-list exists so that act stays outside the repo. Two rules to carry
into it:

- **The first consumer should be a build type** — [#313](313-workload-identity-image-builds.md)
  S9's `build-image` — because it is the workload the whole of half B was
  argued for, and because a command job type has no agent steering it.
- **A general `code` or `web` type on the list is inside D1's acceptance but at
  its widest.** The operator's intent explicitly reaches agent jobs running
  docker commands, so this is permitted, not discouraged by sleight of hand.
  State the consequence plainly: an agent job type with the socket means *the
  agent* holds node root for the duration, and the [trigger](#the-revisit-trigger-stated-as-a-condition)
  is the only thing standing between that and an untrusted-code job.

### The prerequisite: the matched key is shadowable

**A node-side allow-list keyed on the launch env is only as trustworthy as the
env, and the env is not currently sealed.** In `container_env`
([`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs)) the
base stamps are inserted first and the job type's declared `vars` are resolved
from KV and inserted **after** them. Only the `CHUG_` prefix is reserved —
`docs/spec.md` §5.3 and the release-validation check that enforces it — and
`JOB_PROJECT` matches the `[A-Za-z0-9_]+` charset vars are validated against at
write time. So a job type declaring `vars: [JOB_PROJECT]`, over a KV record at
`{owner}.{project}.JOB_PROJECT`, **overwrites the value the node matches on**.

Be precise about who could do this, because overstating it is its own error:
writing that KV record takes project-scoped API access, which in a single-tenant
forge is the operator's, and the declaration takes a merge. It is not an
anonymous escalation. What it is, is a **grant key that project-side config can
move** — so the allow-list stops being a statement the node alone controls,
which is the entire property D2 kept #309 §10's mechanism clause for. It is also
true of the **shipped** KVM grant today, not only of this proposal, and has been
latent because `/dev/kvm` is — per #367's own analysis — not in the socket's
class. A root-equivalent grant is a different matter:

> **Do not ship a node-side socket grant keyed on a name project config can
> shadow.** S1 is a prerequisite, not a follow-up.

Two fixes, either sufficient: extend the reserved-prefix rule to the
dispatcher-composed `JOB_*` stamps (the smaller change, and it matches the
reasoning `docs/spec.md` §5.3 already gives for the task-origin stamps — "neither
may be shadowed by project config"), or move the matched identity under the
`CHUG_` prefix that is already sealed. The second is cleaner and costs a
migration of the KVM grant's matcher; the first is one line of validation and
leaves every existing name alone. **Prefer the first**, and note that it seals
`JOB_ID` and `JOB_BRANCH` as a side effect, which is a gain rather than a cost.
`BASE_BRANCH`, `REPO_URL` and `NATS_URL` are dispatcher-composed too and carry
no shared prefix — whether they join the rule is a question for S1, not one this
document settles.

## The host question: ambient access, and what can actually be done about it

On a host node the access is **ambient** rather than granted: a job type that
declares nothing has docker, because the task's uid owns the socket. An
undeclared capability cannot be audited from the job type, which is a hygiene
cost even where the capability is wanted.

**The asymmetry between the two modes is the whole finding here:**

| | Container mode | Host mode |
| --- | --- | --- |
| How the capability arrives | **added** by a bind the node composes | **inherited** from the uid the task runs as |
| Default | denied, for free | granted, for free |
| To grant | add an allow-list entry | nothing to do |
| To deny | omit the entry | change the task's uid, its `HOME`, or the socket's ownership |

So "make it an explicit declaration" is cheap for containers and expensive for
host tasks, and pretending otherwise would produce a declaration that grants
what is already granted and denies nothing.

**Options for the host half:**

- **H1 — leave it ambient and undocumented.** Rejected: it is the current state,
  and the current state is what a probe had to discover.
- **H2 — a node-side declaration that gates host launches the way C3 gates
  container launches.** *Against:* there is no bind to withhold. A gate that
  refuses to *launch* an undeclared host job type on a docker-capable node is
  enforceable but absurd — it would refuse `mac-proof`, whose access is
  incidental and harmless, while granting nothing to anyone. A gate that claims
  to withhold the socket while the uid still owns it is worse: a control that
  reports success and does nothing is how the first clause of #309 §10 came to
  be believed.
- **H3 (recommended) — advertise it, do not pretend to enforce it.** The node
  probes whether the daemon's own view reaches a docker daemon and reports it on
  `NodeCapabilities` ([`crates/types/src/worker.rs`](../../crates/types/src/worker.rs)),
  defaulting false so a daemon predating the field promises nothing — the shape
  `agent_cli` already uses for #490 D3. The fleet view then answers "which nodes
  can a job reach docker from", for both modes, and the answer is a measurement
  rather than a config file's intention.
- **H4 — per-task users.** [#309](309-host-native-execution.md) P3 §8's
  per-task user boundary is the **only** mechanism that can actually withhold
  host-mode docker, because it is the only one that changes the uid. Real, and
  not this document's to schedule.

> **D4: an advertised capability is an audit record, not a grant.** S4 makes the
> access visible and dated; S6 (per-task users) is where withholding it becomes
> possible, and it is **deferred**, named, with its dependency stated.

The hygiene cost is therefore **reduced, not eliminated**, and this document says
so rather than closing the item. What a reader gets after S4 is: every node's
docker reachability is reported, so no future design reasons from an assumed
absence again.

## Corrections (verified against the tree)

Three claims that shaped this decision or a sibling doc do not survive contact
with the source.

1. **`WORKER_HOST_PROJECTS` does not exist.** Five design docs name it —
   [#309 §10](309-host-native-execution.md#10-trust-and-tenancy) as "enforced at
   the node", plus [#313](313-workload-identity-image-builds.md),
   [#355](355-project-task-images.md), [#367](367-android-emulator-execution.md)
   and [#322](322-macos-native-runtime.md) citing it as precedent — and it
   appears in no source file, no deploy script and no nix module.
   [`crates/worker/src/config.rs`](../../crates/worker/src/config.rs) parses
   `WORKER_MODES`, `WORKER_HOST_ROOT`, `WORKER_KVM_PROJECTS` and
   `WORKER_NIX_PROJECTS`; there is no host-projects list. Host single-tenancy
   today is `placement.node` plus the fact that one node serves `host` at all.
   **This matters here:** the containment story D1's acceptance leans on is
   weaker than the docs assert, and the acceptance should be read against the
   tree's enforcement rather than the doc's.
2. **The job type's name is not on the launch wire.** Recorded above; it is the
   reason S2 exists and the reason every existing node-side allow-list keys on a
   project instead.
3. **`JOB_PROJECT` is shadowable by a declared var.** Recorded above; it is a
   live weakness in the shipped KVM grant, not only a hazard for the proposed
   one, and S1 is its fix.

## What this makes wrong elsewhere

- **[#309 §10](309-host-native-execution.md#10-trust-and-tenancy)** — amended by
  an appended, dated section in that document rather than a reworded rule; its
  head links the amendment.
- **[#313](313-workload-identity-image-builds.md)** — Decision 0 amended, B-IV
  and S8 superseded, by the same append-plus-head-pointer treatment.
- **[#308 §D](308-gha-port.md#d-image-build-and-push-5-workflows--open)** — its
  retraction note says #313 Decision 0 "supersedes both D1 and D2 above with a
  third shape". D1 is now the adopted shape, so that clause is false; a one-line
  dated pointer is added under it. Nothing else in #308 changes, and its
  "**Do not port two of these as-is**" paragraph is reinforced by B4 surviving.
- **`docs/spec.md` §3.1's bind-mount exception class, and §10.1's "No host
  volume mounts"** — a socket bind is a third member of a class whose two
  members are justified by properties it lacks. A spec amendment is owed and is
  a `docs` job; S3 should not land without it.
- **[#367 §2.1](367-android-emulator-execution.md)** — **not** made wrong. Its
  docker-socket column described a capability the fleet did not have and now
  knows it does; its verdict rows (root-equivalence, never a job-type field) are
  unchanged, and correction 3 above strengthens rather than contradicts its D2.
- **[#490](490-agent-work-on-a-mac.md), [#440](440-native-worker-daemon.md),
  [#322](322-macos-native-runtime.md)** — deliberately untouched. #490's slice 6
  is where the measured access began, and recording that here is a citation, not
  an amendment.

## What this document does not decide

- **Which job types go on the allow-list.** Operator config, per the section
  above.
- **Whether the socket bind belongs in `docs/spec.md` §3.1's exception class or
  in a new class of its own.** A `docs` job's, owed before S3.
- **When per-task users land.** [#309](309-host-native-execution.md) P3's
  ordering is its own.
- **Anything about the registry, the provider registration, or the tagging
  discipline.** [#313](313-workload-identity-image-builds.md) S6, S7 and B4 are
  unaffected and keep their owners.

## What S4 landed (job #519)

The audit half of D4, and nothing else: no grant, no allow-list, no socket
bound into any launch, and no change to how a launch is composed.

- **`NodeCapabilities.docker_reachable`**
  ([`crates/types/src/worker.rs`](../../crates/types/src/worker.rs)) —
  `#[serde(default)]`, false when absent, so a daemon predating the field
  promises nothing rather than accidentally advertising access. Additive on both
  transports and **no `WORKER_RPC_VERSION` bump**, for the reason
  [`crates/types/src/version.rs`](../../crates/types/src/version.rs) gives: an
  N-1 daemon simply omits the key and reads as false, which is the same thing it
  meant before the field existed.
- **What it claims, and what it does not.** *This node's daemon reached a docker
  endpoint.* It is not *this launch gets the socket* — under S3 a container
  launch would get one only if the node's allow-list names its
  `(project, job type)` — and the field's doc comment, `docs/spec.md` §3.1 and
  the accessor `DockerAccess::reachable` all say the first rather than the
  second.
- **`worker::docker_access`**
  ([`crates/worker/src/docker_access.rs`](../../crates/worker/src/docker_access.rs))
  probes at boot beside `discover_agent_cli`, for **both** modes and every node
  — a container-only node's answer is as much an audit record as a Mac's.
- **The probe resolves the endpoint the way the docker CLI does**, and that is
  not a decoration. H3 says "the daemon's own view", and on `gumbo-air-0` that
  view has **no** `/var/run/docker.sock`: the CLI resolves the active context
  under `HOME` to `~/.colima/default/docker.sock`, and `container::host`'s
  inherited floor gives a host task the daemon's `HOME`. A probe that asked only
  the conventional socket would have advertised `false` on the one node this
  document measured as having access — a control reporting the opposite of the
  truth, which is the failure mode [H2](#the-host-question-ambient-access-and-what-can-actually-be-done-about-it)
  rejects. So the candidates are `DOCKER_HOST`, then the active context under the
  daemon's `HOME`, then the node's configured `WORKER_DOCKER_ENDPOINT`, first one
  that answers, deduplicated.
- **A probe is a probe.** The reachability question is asked with the API's own
  `GET /_ping` (`container::docker::endpoint_answers`), so discovery creates,
  starts and stops nothing; a fixture unix socket in the worker suite asserts the
  exact request line, which is what keeps that property from silently regressing.
  Each candidate is bounded by a two-second timeout.
- **Never a boot refusal.** `enforce_host_capacity` and
  `enforce_host_supervision` refuse a boot by design; this does not. A node that
  reaches no endpoint logs what it asked and boots — byte-identical in behaviour
  apart from the new advertised `false`.

Not done here, and still open: the shadowable grant key (S1), the job type's
name on the launch (S2), the `DockerGrant` itself (S3), the node config (S5) and
per-task users (S6). Nothing in this slice narrows or widens the capability any
node already had.

## Which of the two fixes S1 took (job #518, 2026-08-09)

**The first — the reserved prefix — and as a prefix rather than a name list.**
`JOB_` joins `CHUG_` as a name a job type may not declare in `vars:` or
`secrets:`: the declaration is a release-validation error, and injection skips
it as the same defense in depth the `CHUG_` rule already carries. The rule is
written up in `docs/spec.md` §4.1, cross-referenced from §2.2's validation
checklist, §5.3 and §3.1's two grant paragraphs.

**One decision site, because two would drift.** `exec::reserved_env_prefix`
([`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs)) answers
*which reserved prefix, and why*, and both `container_env`'s loops and
`release::static_errors_kv` call it. A name refused at release but injected
anyway — or the reverse — is the same class of hole as the one being closed, and
a shared predicate is what makes that unrepresentable rather than merely
untested. It returns the reason as well as the prefix so the refusal reads
`var 'JOB_PROJECT' uses the reserved 'JOB_' prefix (dispatcher-composed launch
stamps, which a node's allow-list matches on)` — the "why" was the thing the
brief asked for and the thing a bare "reserved" does not give.

**Why a prefix and not the five names.** The stamps `container_env` composes are
`JOB_ID`, `JOB_PROJECT`, `JOB_BRANCH`, `JOB_SHA` and — on the eval path —
`JOB_TASK_ID`, and a name list would have to be edited in step with that
composition. A prefix seals them, seals whatever S2 adds under the same
spelling, and needs no maintenance to keep covering the set. This is the gain
the section above predicted for `JOB_ID` and `JOB_BRANCH`, extended.

**`BASE_BRANCH`, `REPO_URL` and `NATS_URL` stay declarable, deliberately.** The
question the section above left to S1, answered on the criterion that makes the
defect a defect: a **grant key** is a name something *outside* the container
reads to decide about the launch. `JOB_PROJECT` is one today
(`KvmGrant::admits`, `nix` grants) and the job-type name will be one after S2;
those three are read only by the container itself, so shadowing one misconfigures
the shadowing project's own job and moves no policy. Reserving them would narrow
project-owned config with no defect to point at, and the reservation is easier
to widen later than to walk back. `NATS_CREDS` and `CHANNEL_ROLE` are the same
case and left alone for the same reason — the container's NATS permissions are
minted server-side per role, so a shadowed `CHANNEL_ROLE` grants nothing.

**No `.chug/jobs/*.yaml` needed editing**, in this repo or by implication: no job
type here declares `vars:` at all, and the reservation reaches nothing outside
the two prefixes. No `WORKER_RPC_VERSION` bump and no `CONFIG_SCHEMA_EPOCH` bump:
the launch wire is unchanged to the byte for every job type that validates today,
and the change is a refusal on config that was already never meant to work — a
project that *had* shipped a `JOB_`-prefixed var would newly fail release
validation, which is the intended and only behaviour change.

**What this does not do.** It does not seal the env against a node that composes
its own, against an image whose entrypoint rewrites the variable before the
task's command reads it, or against anything after the launch: the guarantee is
that *the value the dispatcher put on the wire is the dispatcher's*, which is
exactly what a node-side allow-list needs and no more. S3 still must not ship
without `docs/spec.md` §3.1's amendment, which this job does not touch.

## What S2 landed (job #521)

**One env entry, and none of the three costs the slice was allowed to spend.**
`container_env` ([`crates/dispatcher/src/exec.rs`](../../crates/dispatcher/src/exec.rs))
now composes `JOB_TYPE` alongside `JOB_ID`, `JOB_PROJECT` and `JOB_BRANCH`, in
the same `HashMap::from` literal and therefore **before** the job type's declared
`vars` and `secrets` are resolved from KV. Both halves of the shape S1 established
hold: the stamp is composed first, and S1's `JOB_` prefix is what makes composing
it first mean something — a job type declaring `JOB_TYPE` in `vars:` or `secrets:`
is a release-validation error and injection skips it, with no edit to
`reserved_env_prefix` needed, because a prefix seals a name the day it is written.

**The value is `JobType.name`, and the alternative is worth naming.** The other
candidate was the job record's `type` — the `jobs/{stem}.yaml` stem the dispatcher
routed on, which is also what `GET /api/v1/…/job-types` answers with
(`handlers::jobtypes::list_job_types` reports the stem and reads only
`display_name`/`description` out of the file). The two differ only for a project
whose file stem and declared `name:` disagree, which nothing validates. `name`
wins because the platform **already** keys an outside-the-container grant on it:
`auth::workload` mints the workload-identity subject as
`project:{project}:type:{name}` ([#313](313-workload-identity-image-builds.md)
D4), so a cloud IAM binding is written against that spelling today. Two grant
mechanisms answering "which job type is this" differently would be a trap worth
more than the stem's marginal familiarity. The divergence is a real operational
edge and belongs in S5's node runbook beside C3's rename trap, not in a
validation rule this slice invents.

**What was not spent, checked rather than asserted:** no
`CONFIG_SCHEMA_EPOCH` bump (no job-type field exists to declare), no
`WORKER_RPC_VERSION` bump (`ContainerLaunchConfig.env` is already a
`HashMap<String, String>` on the wire, so this is a value inside a field both
sides have had all along), no `.chug/jobs/*.yaml` edit, and no change to any
node. A worker that ignores the key behaves exactly as it did — the byte
difference in a launch is one more env pair, which is the same class of change
as a job type adding a `var`.

**Nothing consumes it.** `KvmGrant::admits` still matches on `JOB_PROJECT`
alone; no `DockerGrant` exists; no socket is bound. S3 is what reads this, and
S3 still must not ship without `docs/spec.md` §3.1's amendment (a `docs` job,
still owed).

**The regression that matters is the one pinning S1 and S2 together.**
`release::tests::the_dispatcher_composed_job_stamps_cannot_be_declared` now
covers `JOB_TYPE` as a declared var *and* as a declared secret, and
`exec::tests::injection_skips_exactly_the_names_release_validation_refuses`
covers the injection side — the two ends of the single predicate. At tier 2,
`tests/inputs.rs::an_input_free_job_launches_a_byte_identical_eval_env` pins the
whole composed key set, so the stamp's arrival was a deliberate edit to a sorted
list rather than a silent addition, and asserts the value on both the work and
the eval container.

**[Correction 2](#corrections-verified-against-the-tree) is now historical**, as
is the first bullet of [what the node can observe
today](#what-the-node-can-observe-today): the job type's name *is* on the launch.
Both are left as written, per the append-only rule; this section is the current
reading.

## What S3 landed (job #522)

**C3, as the section above specifies it, and nothing is granted by it.** The
mechanism exists; the [decision about which job types](#which-job-types) is
still the operator's, and no node in the fleet declares a socket. A node that
declares nothing produces a byte-identical launch — asserted, not assumed.

- **`DockerGrant` beside `KvmGrant`**
  ([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs)):
  `{ socket, allowed }`, with `DockerGrant::admits` the single decision site the
  way `KvmGrant::admits` is. `DockerBackend::with_docker_grant` is
  worker-daemon-only, so the dispatcher's backend still passes `None` and stays
  bind-mount-free.
- **Matched on both stamps, never on one.** `admits` reads `JOB_PROJECT` **and**
  `JOB_TYPE` out of the composed launch env and admits only an exact pair; a
  launch missing either is admitted by nothing. This is the first node-side
  grant in the platform that is not keyed on the project alone, which is the
  whole of what S2 bought.
- **An entry is parsed, not pattern-matched at launch.**
  `DockerGrantEntry::parse` is the one place `owner/project:job_type` is
  spelled, so what an operator may write and what the matcher accepts cannot
  drift. A malformed entry is refused **at the declaration** rather than kept as
  a grant that silently never matches — the failure mode that would look
  identical to a working deny.
- **Fail closed at three layers, and each says so.** `WORKER_DOCKER_GRANTS`
  refuses a malformed or repeated entry and `WORKER_DOCKER_SOCKET` refuses a
  relative path or a store hash (hard config errors,
  [`crates/worker/src/config.rs`](../../crates/worker/src/config.rs)); the
  daemon refuses to **boot** when the declared socket is absent from its own
  view, with the socket declared but the allow-list empty logged as a warning
  and granting nobody; and `build_host_config` binds nothing for a launch the
  allow-list does not name.
- **The daemon's own view is the one that answers**, per
  [`docs/reference/style.md`](../reference/style.md) Tier 2 rule 7 and the
  fourth question [this document added](#what-was-measured-2026-08-09-job-516)
  to it. `docker_grant_refusal` asks `chug-worker`'s view, so on a node whose
  daemon is still a container the socket must be mounted into `chug-worker`
  itself — a node-provisioning precondition that belongs to S5's runbook,
  stated in the refusal rather than left to be discovered.
- **The bind is writable and carries no env.** A client cannot connect through a
  read-only bind, so the socket is a writable mount at `/var/run/docker.sock` —
  the conventional path, which is exactly where a docker client with no
  `DOCKER_HOST` looks. No `DOCKER_HOST` is injected: the launch env stays free
  of a name that would read as a promise on the many launches that get nothing.
- **Boot refusal rather than a silent drop, deliberately.** The alternative —
  log the unreachable socket and serve on without the grant — keeps the node's
  capacity, and was rejected because it is the shape of failure this whole
  document was written about: a control that reports success and does nothing.
  The KVM precedent refuses the boot for the same reason and is quoted in the
  message.
- **`docs/spec.md` §3.1's amendment landed here**, which
  [the section above](#naming-the-adopted-shape-honestly-this-is-b-i) said S3
  must not ship without. The closed class of bind exceptions now names a third
  member and says plainly that it is in the class **by a decision rather than a
  property**, §10.1's *No host volume mounts* carries the same qualification,
  and §4.1's "nothing consumes `JOB_TYPE` yet" is now false and gone.
- **What was not spent:** no `CONFIG_SCHEMA_EPOCH` bump, no `WORKER_RPC_VERSION`
  bump, no `.chug/jobs/*.yaml` edit, no schema field and no change to what the
  dispatcher puts on the wire. The launch config is untouched; the grant is read
  entirely out of node config and the env the dispatcher already composed.

**Not done here:** the node config that would grant anything (S5, including the
`chug-worker` mount a containerized daemon needs and the rename trap C3 names),
and per-task users (S6). No proxy and no verb allow-list — B-IV stays
[superseded](#the-ladder-this-preserves), rung 2 of the ladder.

## What S5a's deploy plumbing landed (job #523)

**The two knobs travel through the deploy, and nothing is granted by that.**
S3's mechanism was reachable only by hand-editing a node's environment file,
which `deploy/prod/build-worker.sh` rewrites on the next deploy — the shortfall
job #511 fixed for `WORKER_SLOTS_MAX`, in the same place and the same shape.

- **Forwarded as two separate acts**, beside the KVM pair they mirror:
  `WORKER_DOCKER_SOCKET` says the node has a socket to give and
  `WORKER_DOCKER_GRANTS` says which `(project, job type)` launches may hold it,
  both per-node overridable through the derived `<VAR>_<node>` resolution with no
  second code path, both trimmed and whitespace-only-reads-as-unset because that
  is `crates/worker/src/config.rs`'s own reading.
- **Unset stays unset, and it is asserted rather than assumed.** A node
  declaring neither produces the byte-identical run spec case 2a's golden
  already pinned, so this slice is inert on the live fleet — the property S3 was
  careful to buy and this one must not spend.
- **Four pre-flight refusals, each naming the daemon function that would refuse
  it**: a relative or store-hashed socket (`parse_stable_path`), an entry
  `DockerGrantEntry::parse` rejects, a repeated entry (`parse_docker_grants`),
  and a declared socket that is not a socket on the node
  (`docker_grant_refusal`). The daemon runs under `Restart=always`/`KeepAlive`,
  so each of these passed through would replace a working daemon with one the
  supervisor boot-loops out of the fleet.
- **The entry shape is not respelled.** The scan asks exactly what
  `DockerGrantEntry::parse` asks — an owner, a project name with no second
  slash, a job type with no second colon — because that parser is the one place
  the spelling lives and a second copy here would be the drift it was written to
  prevent.
- **The precondition S3 left to this slice, answered by conversion rather than
  by a mount.** `docker_grant_refusal` asks `chug-worker`'s own view, so a
  containerized daemon would need the socket mounted into itself — and this
  script composes **no container run spec** any more (design #440 D1/D2): it
  installs the native unit and removes any leftover `chug-worker` container in
  the same run. So neither branch of "add the bind or refuse for it" is
  representable; the node ends the run natively supervised, its own view *is* the
  node's, and the `[ -S ]` probe over ssh asks the boot refusal's exact question
  in advance. The container case is **unreachable from the deploy path, not
  dead** — the Mini's dispatcher and api are native and the nuc and the air were
  converted, so no live node is in it — and it is stated in the refusal message,
  in the runbook §3 and in `deploy/prod/env.example` for a node an operator
  recreates by hand.
- **Two shapes warn rather than refuse**, because the daemon accepts both: a
  socket with an empty allow-list (it grants nobody, which is the fail-closed
  default said out loud) and an allow-list with no socket (`docker_grant`
  returns `None`, so those launches receive nothing and fail at the docker
  command). Refusing either would be a rule this slice invented.
- **The runbook is `docs/reference/runbooks/worker-docker-grant.md`**, on
  `docs/reference/runbooks/worker-kvm.md`'s model, and it declares nothing that
  would work if pasted: which job types get the socket is the operator's act,
  which is what the [Which job types](#which-job-types) section keeps outside the
  repo.

**Not done here, and it is the rest of S5:** no node declares a socket, no
allow-list entry exists in this tree, and no `placement.node` pin — that half
waits on [#313](313-workload-identity-image-builds.md) S9's `build-image`, which
is the first consumer this document names. No `crates/worker` or
`crates/container` change, no epoch bump, no `WORKER_RPC_VERSION` bump and no
`.chug/jobs/*.yaml` edit.
