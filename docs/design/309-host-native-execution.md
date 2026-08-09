# Design — Host-native execution (node kind, selector, capabilities, exclusive resources)

Status: PROPOSED; **P0 landed 2026-08-05 (job #434)** — `HostBackend`,
`WORKER_MODES` routing and the `slots: 1` enforcement, on for `gumbo-air-0`
since [#490](490-agent-work-on-a-mac.md) slice 6 and off on every other node —
and **P1 landed 2026-08-07 (jobs #401, #478, #479)**: `runtime.mode: host` became a
legal declaration that nothing placed by
([P1 as landed](#p1-as-landed-2026-08-07--the-host-rows-field-rules-job-478)),
and a dual-mode node now routes each launch by its declared mode
([P1 as landed, per-launch routing](#p1-as-landed-2026-08-07--per-launch-mode-routing-job-479))
— and **P2 landed 2026-08-07 (jobs #483, #484)**: `NodeCapabilities` rides both
worker transports and is ingested in `probe_worker`
([the 2026-08-07 note](#note-2026-08-07--slice-5-landed-runtimemode-not-execmode-job-483)),
and `choose_placement` now filters candidates by the mode a launch requires, so
host work is **routable** — a host job type placed by capability rather than by
a pin
([the 2026-08-07 note on slice 6](#note-2026-08-07--slice-6-landed-host-work-is-routable-job-484)).
§1's recommendation had already shipped before P0 started; see
[the 2026-08-05 correction](#correction-2026-08-05--§1-already-shipped-p0-landed)
and its addendum on `remove` racing its own reaper.

**[§10](#10-trust-and-tenancy)'s docker-socket rule is inverted as of 2026-08-09
(job #517).** Host tasks on `gumbo-air-0` reach a working docker daemon by file
ownership, and the operator has accepted that rather than closed it; the rule's
*mechanism* clause — a node-side allow-list entry, never a job-type field the
platform honors on request — survives unweakened. See
[the 2026-08-09 amendment](#amendment--2026-08-09-job-517-the-docker-socket-rule-inverts)
and [#517](517-docker-access-for-jobs.md), which owns the decision.

**§10's tenancy list is built as of 2026-08-09 (job #525).** The same amendment
recorded `WORKER_HOST_PROJECTS` as a variable five designs named and no source
file held; it now exists, fail-closed, enforced in `container::host::HostTenancy`
at every host launch and refused at the deploy when a `host` node declares none.
The finding stands as history; see
[the 2026-08-09 note](#note-2026-08-09--10s-tenancy-list-is-built-job-525) and
[`docs/reference/runbooks/worker-host-projects.md`](../reference/runbooks/worker-host-projects.md).

**[§8](#8-secrets-on-a-shared-host)'s per-task user pool is unavailable on macOS,
and the operator has accepted the absent boundary (2026-08-09, job #526).** Host
tasks on `gumbo-air-0` run as the node's existing login user; option (b) is kept
unweakened for a Linux host node, of which there are none. With §8 unavailable,
§10's tenancy list is the only remaining bound on cross-task exposure — built by
job #525 just above, and awaiting the deploy that declares one for `gumbo-air-0`.
See
[the 2026-08-09 amendment on tenancy](#amendment--2026-08-09-job-526-8s-recommendation-is-unavailable-on-macos-and-10s-tenancy-list-is-now-the-only-bound)
and [#322](322-macos-native-runtime.md), which owns the decision.

Written against the tree at `b801b76`. Every claim about current behavior was
read out of the source and out of [docs/spec.md](../spec.md), not inferred from
the docs; where the job brief and the tree disagree, the tree wins and the
disagreement is recorded under [Corrections](#corrections-to-the-brief).

This is doc 1 of 4 extracting implementable specs from
[design #308](./308-gha-port.md). Section H of that doc (H.1–H.6) argues *why*
host-native execution is wanted — the category map, the gap ranking and the
beacon survey are there and are not restated here. This doc decides the parts
H explicitly left open: the schema syntax for a host-mode selector, the host
analogue of every `ContainerBackend` method, capability advertisement, placement
and exclusive resources, drain, resource limits, secrets, environment/state, and
tenancy.

Docs 2–4 (scheduled jobs; job parameterization; workload identity and image
builds) are separate and cite this one. Nothing here decides them.

Related: [docs/spec.md](../spec.md) §3.1 (backends, dynamic worker registration,
placement, node-local build caching), §3.5 (launch capacity queue, task
timeout), §3.6 (restart reconciliation), §14 (config/version skew), Appendix:
Deferred; [design #293](./293-worker-capacity.md) (worker capacity — overlaps
this work directly and is reconciled with throughout);
[docs/reference/design-lifecycle.md](../reference/design-lifecycle.md);
[docs/reference/style.md](../reference/style.md); [docs/reference/crates.md](../reference/crates.md).

## Current state

*The **mutable head** ([#415](415-knowledge-architecture.md) [D2](415-knowledge-architecture.md#d2-every-design-doc-opens-with-a-mutable-current-state-head)):
rewritten to current truth whenever anything below it changes. Everything after
this section is append-only — the original argument and its dated corrections,
never edited into the prose above them.*

Two phases have landed, one of them early and out of order. P0 is in the tree
(`crates/container/src/host.rs`, `WORKER_MODES`) and is **on for one node** —
`gumbo-air-0` advertises `host`, and [#490](490-agent-work-on-a-mac.md) slice 6
ran the first host tasks on it. P1 is complete: its schema half arrived ahead of P0
in job #401, driven by [#373](373-project-toolchains.md), job #478 landed
the host row's own field rules (`crates/types/src/job_type.rs` — top-level
`image` disallowed, `runtime.env` required, the evaluator-image requirement
narrowed by the [Coexistence](#coexistence-on-a-mixed-fleet) precedence rule)
and deleted the refusal, with no epoch bump, and job #479 carried the resolved
mode to the worker: `image` is `Option<String>` on both the launch config and
the wire (`WORKER_RPC_VERSION` 2), a node constructs exactly the backends its
`WORKER_MODES` names, and one naming both routes each launch by the image's
presence. **P2 is landed.** Its slice 5 shipped in job #483 now that its
gate — [#293](293-worker-capacity.md) job 3 — is in: `NodeCapabilities` is a
wire record on both
`PingOk` and `WorkerAnnounce` (`crates/types/src/worker.rs`), additive and so
still at `WORKER_RPC_VERSION` 2, a daemon derives its own from `WORKER_MODES`,
and the dispatcher ingests it inside `probe_worker` on the reply path with the
`ping`-wins precedence §4 argues for (`crates/worker/src/backend.rs`). Its
modes are `types::job_type::RuntimeMode`, not the `ExecMode` §4 sketches — see
[the 2026-08-07 note](#note-2026-08-07--slice-5-landed-runtimemode-not-execmode-job-483).
Its slice 6 shipped in job #484: `choose_placement` takes the mode the launch's
`image` selects, excludes every candidate not advertising it, and separates
"no node advertises this mode" from "every capable node is full" — see
[the 2026-08-07 note on slice 6](#note-2026-08-07--slice-6-landed-host-work-is-routable-job-484).
So a host job type is now **well-formed and routable**, on any node that
advertises the mode and with no pin. The precedence rule
[Coexistence](#coexistence-on-a-mixed-fleet) states is honoured at the launch as
well as in validation since job #507: `types::JobType`'s `level_image`,
`level_mode` and `level_runtime_env` resolve a launch from the level it is for,
so a level whose own `image` resolves it **out of the job type's mode** — a host
job type's container evaluator — inherits no `runtime.env`, while under
`mode: container` an `image` and an `env` still layer
([#373](373-project-toolchains.md) Decision 2). That is what makes "host work,
container CI, one job" actually run, and it has: [#490](490-agent-work-on-a-mac.md)
slice 6's second run is the first job to have taken a host work task and a
container `ci` evaluator to Done together. The carve-out is
[the 2026-08-08 correction](#correction-2026-08-08--the-precedence-rule-fires-only-across-a-mode-boundary-job-507).
**P3 is partly landed.** Job #524 gave
`NodeCapabilities.resources_enforced` its first reader: `choose_placement` now
takes a `LaunchRequirements { mode, resource_limits }` and admits a launch
declaring `resources.cpu`/`memory` only onto a node that enforces them **for
that launch's resolved mode**, while `HostBackend::admit` refuses — hard, naming
the field and the node — one that arrives anyway through a `placement.node` pin
([the 2026-08-09 note](#note-2026-08-09--7s-predicate-and-backstop-landed-job-524)).
Reading the advertisement per mode is what keeps a dual-mode node like
`gumbo-air-0` from claiming a bound that is false for its host launches; nothing
bounds a host task's cpu or memory on any platform, so a host job type must
declare neither, and the platform now says so at placement instead of at nothing.
The rest of P3 — per-task users ([§8](#8-secrets-on-a-shared-host)), transient
scopes as a *limits* mechanism ([§7](#7-resource-limits)) — and everything from
P4 on is unstarted. [#440](440-native-worker-daemon.md) is the design for the
native-supervision prerequisite P0 named and left unowned.

The rows below are the states of [Phasing](#phasing)'s table, which keeps each
phase's full argument; the seven-row table in
[Contracts changed](#contracts-changed-per-docsreferencestylemds-contract-first-rule)
is the same work sliced by contract, not a second plan.

| Phase | What | State |
| --- | --- | --- |
| **P0** | Backend polymorphism + a `HostBackend` on one node, routed by `placement.node`, `slots: 1` | **Landed** (job #434) — see [the 2026-08-05 correction](#correction-2026-08-05--§1-already-shipped-p0-landed) |
| **P1** | The `runtime:` selector, the epoch bump, the `min_dispatcher` requirement, the validate rule | **Landed** (job #401) for the block and the epoch, driven by #373; **Landed** (job #478) for the host row's field rules and the refusal deletion, on the same epoch; **Landed** (job #479) for §1's per-launch routing and the `WORKER_RPC_VERSION` bump it needed; **Landed** (job #507) for the launch half of [Coexistence](#coexistence-on-a-mixed-fleet)'s precedence rule — `JobType::level_image` / `level_runtime_env` resolve a launch from the **level** it is for, so a host job type's container evaluator inherits no `runtime.env` |
| **P2** | `NodeCapabilities` on ping + announce; capability-aware `choose_placement` | **Landed** (job #483) for slice 5 — the record on both transports, additive, ingested in `probe_worker` with ping authoritative and docker-endpoint nodes synthesized; **Landed** (job #484) for slice 6 — `choose_placement` takes the required mode, excludes the nodes that do not serve it, and answers "no node advertises it" differently from "every capable node is full". P2 is complete: host work is routable |
| **P3** | Per-task users; `resources_enforced`; transient scopes | **Partly landed** (job #524) — `resources_enforced` is consumed: `choose_placement` treats a launch declaring `resources.cpu`/`memory` as requiring enforcement **for its resolved mode**, and `HostBackend::admit` refuses one that reaches a node anyway ([the 2026-08-09 note](#note-2026-08-09--7s-predicate-and-backstop-landed-job-524)). Per-task users ([§8](#8-secrets-on-a-shared-host)) and transient scopes for *limits* ([§7](#7-resource-limits)) stay Proposed |
| **P4** | Device leases | Proposed — only when a host node must run a second, non-device-bound task concurrently |
| **P5** | Declared caches + GC roots + warm set | Proposed — gated on P1, independent of P2–P4 |

## Scope

This **adds** a node kind. It does not replace containers, and a mixed fleet is
already the implemented reality — `FleetBackend`
(`crates/worker/src/backend.rs`) drives docker endpoints directly and worker
nodes over NATS from one roster today. Every recommendation below is additive
and phaseable onto one node.

## Corrections to the brief

The brief is accurate on the points that carry the argument. Four claims needed
adjusting against the tree, and each moves work.

1. **"#293 is PROPOSED and not implemented" — true of the code, not of the
   spec.** #293's job 1 (the §3.1 amendment) has **landed**: `docs/spec.md` §3.1 now
   normatively describes `WorkerAnnounce { node, slots, slots_max,
   capacity_epoch, capacity_generation, version }`, `slots` on the ping reply,
   `req.worker.{node}.set_slots`, `slots_max`/`WORKER_SLOTS_MAX`, and
   `capacity_source`/`capacity_observed_at`. None of it exists in the code:
   `crates/types/src/worker.rs` still has `WorkerAnnounce { node, slots,
   version }` and a `PingOk` with no `slots`, and `grep -rn "set_slots\|slots_max
   \|capacity_epoch" crates/` returns nothing. So the reconciliation target for
   this doc is **`docs/spec.md` as written plus #293's jobs 2–7 as scheduled** — the
   normative text is already ahead of the binary here, which is exactly the
   condition §14 exists to manage.
2. **Making the daemon backend-polymorphic is bigger than swapping one field's
   type.** `crates/worker/src/daemon.rs` holds `backend: DockerBackend`, as #308
   correction 5 says. But the daemon also calls three **inherent** (non-trait)
   `DockerBackend` methods: `managed_running_total()` in the `ping` handler, and
   `with_cache_dir()` / `ping_all()` at construction
   (`crates/container/src/docker.rs`). A `dyn ContainerBackend` field alone does
   not compile. See [1](#1-backend-polymorphism).
3. **The trait's surface is 10 required methods *plus five defaulted ones*, and
   `docs/spec.md` §3.1's inline listing of it is stale.** The spec's code block shows
   eight methods and omits `logs`, `logs_tail`, and all five provided methods
   (`fleet_status`, `register_worker`, `supports_dynamic_workers`,
   `mark_worker_unschedulable`, `occupancy_unavailable_nodes`). The five
   defaulted ones are good news — they are fleet-level concerns a node-local
   backend answers with the default no-op — but the drift should be fixed
   whenever §3.1 is next edited.
4. **The brief's drain claim holds for the math and not for the control.**
   `probe_worker` computes `free: slots − ping.running` as `i64`
   (`crates/worker/src/backend.rs`) and `choose_placement` skips `free <= 0`
   (`crates/container/src/lib.rs`), so a cap below live occupancy skips the node
   and kills nothing — verified. But the only way to *change* a node's slot
   count today is to restart its daemon with a different `WORKER_SLOTS`
   (`crates/worker/src/config.rs`, `deploy/prod/build-worker.sh`). The runtime
   control is #293's jobs 2 and 4. See [6](#6-drain).

One brief claim is *stronger* than stated and deserves promotion: the trait's
container assumptions are not the only cost. `/workspace` is hardcoded in
**shared** code that container mode also uses, and that is the sharpest single
finding in this document — see [2](#2-the-traits-container-assumptions).

---

## 1. Backend polymorphism

The prerequisite. The dispatcher must never learn that a node runs host
processes: it already treats a worker node as an opaque RPC endpoint
(`NodeHandle::Worker` in `crates/worker/src/backend.rs`, ops proxied on
`req.worker.{node}.{op}` per §3.1), so the mode selection belongs entirely
inside the daemon.

**What the daemon actually needs from a backend.** It calls `launch`, `kill`,
`inspect`, `copy_file`, `logs`, `logs_tail`, `remove`, `list_managed_exited`,
`list_managed_running`, and `managed_running_total`. It **never calls `wait`** —
per §3.1 `wait` is implemented dispatcher-side as an inspect poll so worker
restarts are transparent. So a host backend's `wait` is trait-completeness only,
implementable as a poll over its own `inspect`, and never on the hot path.

### Options

**Option A — `Arc<dyn ContainerBackend>` in `WorkerState`, with
`managed_running_total` moved onto the trait as a provided method.**
The default implementation is `list_managed_running().len()`, which is what a
node-local backend can always answer; `DockerBackend` keeps its cheaper
label-filtered override. Construction moves into one
`fn local_backend(config: &WorkerConfig) -> Result<Arc<dyn ContainerBackend>>`
that owns the cache-dir and startup-probe wiring per variant.

- *For:* one seam, the one that already exists, and it is the seam
  `docs/reference/crates.md` names as `container`'s charter. Adding the third implementation
  keeps `FakeBackend` and `FleetBackend` honest about the trait rather than
  letting the daemon grow a private interface.
- *Against:* it widens a shared trait for one caller's benefit, and it makes
  `ContainerBackend` — already a name that lies about `FleetBackend` — lie
  harder.

**Option B — an enum in the worker crate:
`enum LocalBackend { Docker(DockerBackend), Host(HostBackend) }`, dispatching by
hand.**

- *For:* no trait change at all; inherent methods stay reachable per variant, so
  `managed_running_total`, `with_cache_dir` and `ping_all` need no home in the
  trait. Exhaustive matching means a new op cannot be silently missed. It is
  arguably the simpler shape, which docs/reference/style.md Tier 3 treats as a legitimate
  argument on its own.
- *Against:* it is a **second** launch seam in a codebase whose whole story is
  that there is one (`crates/container/src/lib.rs`: "the **only** launch seam").
  Ten hand-written delegations drift; the compiler enforces the trait's shape
  and does not enforce an enum's parity with it. And it makes the host backend
  unreachable from anything but the daemon, which forecloses ever driving it
  from a single-node dev deployment.

**Recommendation: Option A.** The deciding argument is that the trait is the
contract the dispatcher's behavior is written against — `NoCapacity` semantics,
`logs_tail` offset stability, `remove` idempotence — and a host backend that
satisfies the trait satisfies those by construction, where an enum arm satisfies
them only by review. The cost is one provided method with an obviously-correct
default.

**Mode selection is node-side config**, not a wire field: `WORKER_MODES`
(default `container`), parsed like `WORKER_SLOTS`/`WORKER_CACHE_DIR` in
`crates/worker/src/config.rs` — pure over its input, unit-tested without env
mutation. A daemon whose `WORKER_MODES` includes `host` constructs both backends
and routes each launch by the request's declared mode
([3](#3-the-host-mode-selector)); a daemon in container mode only is
byte-for-byte what ships today.

**The honest limit.** #308 H.1's roster — `DockerBackend`, `FleetBackend` over
NATS, `FakeBackend` (verified: `grep -rn "impl ContainerBackend for" crates/`
returns exactly those three; `crates/container/src/k8s.rs` is a stub) — proves
the seam is not welded to a local socket. It does **not** prove the seam is
container-agnostic: every implementation ultimately drives a container or
pretends to, and the trait's vocabulary (`ContainerId`,
`ContainerLaunchConfig.image`, `RunningContainer`) was derived from Docker
alone. Section 2 is where that bill comes due.

---

## 2. The trait's container assumptions, method by method

### The host execution unit

A host task is a **process group rooted in a per-task directory**. The daemon
mints a task id and creates `{WORKER_HOST_ROOT}/{task_id}/` holding:

| File | Purpose |
| --- | --- |
| `meta.json` | pgid, pid + process start-time, identity labels (`project`/`job`/`task`, read from the launch env exactly as `managed_labels` does today), declared limits, environment ref |
| `output.log` | merged stdout+stderr, append-only, opened once |
| `exit_code` | written atomically (write-temp-then-rename) when the group exits |
| `workspace/` | the clone |
| `.gc-root` | the nix indirect GC root, if any ([9](#9-environment-and-state)) |

That directory **is** the container. It is what makes `inspect`, the two
listings and `remove` implementable at all, and it is not optional.

### Per-method

| Method | Host analogue | Difficulty |
| --- | --- | --- |
| `launch` | create task dir, materialize `InjectedFile`s, spawn a process group; return `{node}/{task_id}` so the existing `{node}/{id}` routing is unchanged | **Moderate** — the path problem below lives here |
| `wait` | poll `inspect`; never called by the daemon (§3.1 polls dispatcher-side) | **Trivial** |
| `kill` | signal the process **group**, SIGTERM then SIGKILL after a grace window — the exact pattern `refresh_pgid` already implements in `crates/worker/src/daemon.rs` for the refresh script | **Trivial**, with one caveat below |
| `inspect` | `exit_code` present → `Exited`; absent → check the group is live | **Genuinely awkward** — see pid identity below |
| `copy_file` | read `{task_dir}` + the rebased path | **Trivial given the rebase rule** |
| `logs` | read `output.log`. *Better* than Docker: one fd means true stdout/stderr ordering, so the trait's "order is not preserved across streams" caveat simply does not apply | **Trivial** |
| `logs_tail` | byte offsets into an append-only file. The doc comment's "byte offsets are stable — container logs are append-only" is *definitionally* true here, and "the same offsets address the harvested `stdout.log` after exit" is true because it is the same bytes | **Trivial**, and strictly better |
| `remove` | `rm -rf {task_dir}` plus dropping the GC root; idempotent | **Trivial to write, load-bearing to get right** — see below |
| `list_managed_exited` | task dirs with an `exit_code` file | **Trivial** |
| `list_managed_running` | task dirs without one whose group is live; labels from `meta.json` | **Trivial** |
| `fleet_status`, `register_worker`, `supports_dynamic_workers`, `mark_worker_unschedulable`, `occupancy_unavailable_nodes` | the default no-ops — these are fleet concerns and the daemon runs a *single-node* backend | **Free** |

### The three that are genuinely hard

**(a) `/workspace` is hardcoded in shared code.** This is the cost #308 H.3
underprices, because it is not confined to the new backend:

- `bootstrap_cmd` (`crates/container/src/lib.rs`) emits `git clone … /workspace
  && cd /workspace`.
- `crates/dispatcher/src/launch_queue.rs` calls `copy_file(&id,
  "/workspace/eval-result.json")` — an absolute literal in the dispatcher.
- `agent::transcript_path` (`crates/agent/src/lib.rs`) returns
  `{CLAUDE_CONFIG_DIR}/projects/-workspace/{session_id}.jsonl`, where
  `-workspace` is the Claude CLI's **slugification of the cwd** — measured
  against CLI 2.1.211, per that function's own doc comment. Change the cwd and
  the transcript lands somewhere else and the harvest silently finds nothing.

Two concurrent host tasks on one node cannot both own `/workspace`. Three ways
out:

- **(i) Rebase.** Task root becomes `{task_dir}/workspace`; the host backend
  owns one rebase rule (`/workspace/*` → `{task_dir}/workspace/*`,
  `/chuggernaut/*` → `{task_dir}/chuggernaut/*`) and it is the only place that
  knows. `copy_file` rebases on the way in, so `launch_queue.rs` needs no edit.
  `bootstrap_cmd` needs a host variant. `transcript_path` must become
  *computed* from the cwd rather than a constant — turning a measured constant
  into a slugifier we now own, which is a real regression in confidence about an
  external CLI's behavior.
- **(ii) Private mount namespace per task** (Linux: `systemd-run --scope` with
  `BindPaths=`, or `unshare -m`), so `/workspace` means something different per
  task and **nothing above changes at all**. Strictly the cleanest on Linux.
  Impossible on macOS — no per-process bind mounts — and macOS is the entire
  reason #308 category F exists.
- **(iii) One host task at a time per node** (`slots: 1`), so `/workspace` is
  unambiguous. Zero code change, works everywhere, wastes the node.

**Recommendation: (iii) for the prototype, (i) as the durable answer, (ii) noted
as a Linux-only optimization.** (iii) is free and coincides exactly with H.5's
cheap interim for exclusive resources ([5](#5-placement-and-exclusive-resources))
— one 1-slot pin buys both. (i) is the only option that survives contact with
macOS, and macOS is the point.

**(b) `inspect` across a daemon restart — pid identity.** A container id is
stable and a pid is recycled. If the daemon restarts (a `worker-refresh.sh`
swap, a `nixos-rebuild switch`) while a host task runs, a bare pgid check can
match an unrelated process. Mitigation: record the process **start time**
alongside the pid (Linux: field 22 of `/proc/<pid>/stat`; macOS: `ps -o lstart`)
and treat a mismatch as *gone*. Getting this wrong reports a dead task as
running, which §3.6 classifies as "still running, re-attach" and hangs the task
until `task_timeout` — a slow, confusing failure rather than a loud one. Assert
it (docs/reference/style.md Tier 2: assert negative space).

**(c) `remove` is now a real obligation with no runtime behind it.** In
container mode a forgotten `remove` leaks an overlay and the §3.6 startup sweep
reclaims it. In host mode the same call must delete a `workspace/` holding a
5–10 GB `target/` *and* drop the nix GC root ([9](#9-environment-and-state)) —
and nothing else on the node will. The good news: `list_managed_exited` reads
the same task dirs, so §3.6 step 6's existing sweep is already the crash
backstop and **no new sweep is needed**. Keep `remove` best-effort and
idempotent as the trait documents — a failed removal must never fail a job.

**One caveat on `kill`:** a process that calls `setsid()` escapes its group and
survives. A container runtime kills the whole cgroup and has no such hole. On
Linux, launching into a transient scope (`systemd-run --scope --unit=…`)
restores the property, and that is the *same* mechanism [7](#7-resource-limits)
needs for `CPUQuota`/`MemoryMax` and [6](#6-drain) needs to survive a daemon
restart — three requirements, one mechanism, which is the strongest argument in
this document for making the transient scope non-optional on Linux. On macOS
there is no equivalent and a determined escape leaks; say so rather than imply a
boundary that is not there.

---

## 3. The host-mode selector

### Why the epoch must move

`image` is **required** for `work.type: agent | command` — the `Required {
field: "image" }` rules in `JobType::validate`
(`crates/types/src/job_type.rs`). Unknown *top-level* fields are tolerated with
a warning (§14.2; `JobType` drops `deny_unknown_fields` and captures unknowns
into `JobType::unknown`), so adding a selector is by itself N−1 safe. What is
not safe is a config where `image` is **absent**: an N−1 dispatcher rejects it
outright and, per §14.2, parks every job of that type. That is the 2026-07-22
shape §14 exists to prevent.

So: **bump `CONFIG_SCHEMA_EPOCH` in `crates/types/src/version.rs`, in the same
commit as the parser change, and gate every host-mode job type with
`min_dispatcher:` at that epoch** (§14.1, §14.3 — `.chug/tasks/ci.sh`'s
config-skew gate then fails the config's own CI against a deployed dispatcher on
the older epoch). Job #401 spent that bump: 3 → **4**, not 1 → 2, since #376 had
moved the epoch twice by then, with `RUNTIME_SCHEMA_EPOCH` frozen at 4.

`min_dispatcher` is author-declared, so leaving it to authorship guarantees
somebody forgets. `validate()` should therefore gain a rule: **`runtime.mode:
host` requires `min_dispatcher >= RUNTIME_SCHEMA_EPOCH`**, reported as an
ordinary `FieldRuleError::Required`. One line, and it makes the gate
structural. Job #401 shipped it wider — any `runtime:` beyond a bare
`mode: container` — because
[#373](373-project-toolchains.md) C7 measured that the container row leaks the
same way and that the `image` ban this section leans on cannot help: an N−1
dispatcher never runs the new field rules at all, so the declared
`min_dispatcher` is the only signal that crosses the boundary.

### Syntax options

**Option A — a discriminated `runtime:` block (recommended).**

```yaml
name: mobile-integration
runtime:
  mode: host                  # container (default) | host
  env: "nix:.#chug-mobile"    # required when mode: host, disallowed otherwise
min_dispatcher: 2
work:
  type: command
  run: ./.chug/tasks/integration.sh
```

Field rules, stated as an extension of the existing matrix rather than a
replacement:

- `runtime` absent, or `runtime.mode: container` → today's rules, unchanged.
  `image` required for agent/command, `runtime.env` disallowed.
- `runtime.mode: host` → **top-level** `image` disallowed, `runtime.env`
  required. Per-level `image` (on `wrap_up` or an evaluator) stays legal and is
  how that level opts back into container mode — see
  [Coexistence](#coexistence-on-a-mixed-fleet) for the precedence rule and the
  one existing rule it narrows.

**Option B — a flat top-level peer, presence-selected.**

```yaml
host_env: "nix:.#chug-mobile"   # mutually exclusive with image:
```

**Option C — overload `image`'s scheme** (`image: "nix:.#chug-mobile"`).

### Tradeoffs

| | A (`runtime:` block) | B (flat peer) | C (overload `image`) |
| --- | --- | --- | --- |
| Epoch bump needed | yes | yes | **no** |
| Typo protection | **yes** — nested blocks keep `deny_unknown_fields` (§14.2), so `mdoe: host` is a hard parse error | no — a typo'd `host_env` is an ignored top-level unknown, i.e. a job that silently runs in container mode | no |
| N−1 behavior | config parks pre-Work (Stalled, one park, §14.2) | same | **worse**: config parses, then every launch fails at `docker pull nix:.#…` — a per-job runtime failure, burning `work_retries` |
| Extends to a future mode | one discriminant, no new top-level field per mode | a new top-level field per mode | no |
| Expresses "these are the same axis" | yes — `mode` decides which of `image`/`env` applies | mutual exclusion, checked by hand | conflates two things in one string |

**Recommendation: Option A.** C is genuinely the cheapest — it is the only
option needing no epoch bump — and it should be rejected anyway, because it
converts a *detected, explained, non-destructive* skew condition (the §14.2
park) into an undetected one that burns retry budget job by job. Trading the
skew machinery's whole purpose for one avoided integer is a bad trade. B is
close to A but gives up `deny_unknown_fields` on the one field whose typo
silently changes where work runs.

### Coexistence on a mixed fleet

**Mode resolves per launched task**, and it needs one precedence rule stated
explicitly, because the universal case — the `ci` evaluator that
`.chug/jobs/_defaults.yaml` appends to *every* job type — depends on it.

> **The rule: an explicit `image` at a level resolves that level to container
> mode and does not inherit `runtime`. A level with no `image` of its own
> inherits the top-level `runtime`.**

"Level" means each of the three places that carry their own optional `image`
today (`crates/types/src/job_type.rs`): top-level, `wrap_up`, and each
`Evaluator`. The existing fallback (`wrap_up.image.or(job_type.image)` in
`crates/dispatcher/src/launch_queue.rs`) is the container-mode half of the same
rule, unchanged.

Consequences, spelled out so an implementer does not have to re-derive them:

- The `ci` evaluator declares an explicit `image: chuggernaut/agent-rust:prod`,
  so on a host-mode job type that appended gate **resolves to container mode and
  stays a container task** with no special-casing. Host work, container CI, one
  job — which is very likely what a real mobile job type wants.
- Under `runtime.mode: host`, the **top-level** `image` is disallowed. Per-level
  `image` is not: it is exactly how a level opts back into container mode.
- The existing "container evaluators need an image from somewhere" rule
  (`crates/types/src/job_type.rs`, `FieldRuleError::Required { field: "image" }`
  when both the evaluator's and the top-level `image` are `None`) must be
  narrowed to *evaluators whose resolved mode is container*. Under
  `runtime.mode: host` an evaluator with no `image` is a host evaluator and
  needs none — the requirement would otherwise fire on every host job type.
- **No existing config changes meaning.** The narrowing only takes effect under
  `runtime.mode: host`, which cannot appear on a job type that has not been
  rewritten (§14.1 gates it behind `min_dispatcher: 2`). There is no
  container-mode config whose evaluators silently become host evaluators, and no
  migration for evaluators that rely on the top-level fallback today.

A per-level `runtime:` override — a `mode: host` evaluator on a container-mode
job type — is deliberately **not** specified here. The precedence rule above
covers both known cases (all-host with a container gate; all-container), and the
override can be added additively later without disturbing it.

Behavior at the edges:

- **A host-mode task and no host-capable node** → `BackendError::NoCapacity`
  with a distinct message (`no node advertises host mode`), queued and retried
  via §3.5, **no retry budget consumed**, eventually escalating with
  `no_free_slots_timeout` after the 30-minute maximum queue wait. Honest cost: a
  fleet that will *never* have a host node takes 30 minutes to say so.
  Mitigation, not a new error class: when the fleet-wide set of nodes
  advertising the required mode is empty at launch, emit the platform-level
  warning immediately (`queued for a capability no node advertises`) — the same
  treatment §3.1 gives never-observed capacity.
- **A container task and a host-only node** → the inverse filter; never placed.
  An older daemon reads as container-only ([4](#4-capability-advertisement)), so
  a mid-deploy fleet has *fewer* host-capable nodes, never a mis-placement.
- **A pin (`placement.node`) onto a node that lacks the mode** → a hard
  `BackendError::Launch` naming the node and the missing mode, **not**
  `NoCapacity`. `choose_placement` today already distinguishes these: an unknown
  pin is a hard `Launch` error, a full/out-of-service pin is transient. A pin
  onto a capability-less node is a configuration error that no amount of waiting
  clears, so queueing it would be a 30-minute silence with a known answer.

---

## 4. Capability advertisement

A node must advertise what it **can do**, not just how much. Reconciling
with #293 is not optional here: that design is already editing both transports,
and #308 correction 6 is right that a capability field must land *after or with*
it.

### The shape

```rust
// crates/types/src/worker.rs
pub struct NodeCapabilities {
    /// Execution modes this daemon can serve. Absent ⇒ ["container"].
    pub modes: Vec<ExecMode>,        // Container | Host
    /// e.g. "linux/aarch64", "macos/aarch64" — informational for placement
    /// diagnostics and required for #308 category F targeting.
    pub platform: String,
    /// Whether this node can enforce `resources.cpu`/`memory` (§7).
    /// Absent ⇒ true: every node that can serve a container launch enforces
    /// them, so only a host node that says otherwise reads as false.
    pub resources_enforced: bool,
    /// Named exclusive resources this node holds (§5).
    pub leases: Vec<String>,
}
```

Carried on **both** `PingOk` and `WorkerAnnounce` as `#[serde(default,
skip_serializing_if = "Option::is_none")] Option<NodeCapabilities>`. Additive,
so it does **not** bump `WORKER_RPC_VERSION`.

**The absent-capability defaults, field by field**, because every node in the
fleet reads as absent during the whole N−1 rollout window and one of them reads
as absent forever (below). An older daemon must read correctly with no
coordination, which is the N+−1 rule (§14.1). `None` — and each absent field
within a present `NodeCapabilities` — resolves to:

| Field | Default when absent | Why this is the safe reading |
| --- | --- | --- |
| `modes` | `[Container]` | The N−1 daemon runs containers and nothing else; host capability is the new thing and only a new daemon can claim it. Fails closed. |
| `platform` | `"unknown"` | Diagnostic only; never a placement filter on its own. |
| `leases` | `[]` | A node advertises no devices unless it says so; an undeclared lease is never acquired. Fails closed. |
| `resources_enforced` | **`true`** | Every backend that can serve a container launch today enforces `cpu`/`memory` through the runtime (`nano_cpus`/`memory` on the Docker `HostConfig`, `crates/container/src/docker.rs`). The only nodes that *cannot* are the new host ones, and those announce explicitly. |

That last default is the one that matters and it is worth being blunt about
why: `resources_enforced` defaulting to `false` would break placement for
**every job type in this repo** during the rollout — `code.yaml`, `web.yaml`,
`docs.yaml`, `design.yaml`, `deploy.yaml` and `web-publish.yaml` all declare
`resources.cpu`/`memory` — queueing each for the 30-minute maximum wait and then
escalating with `no_free_slots_timeout`. Defaulting it to `true` keeps the
container fleet's behavior byte-for-byte unchanged and confines the new
predicate to nodes that opted out of enforcement.

**Docker-endpoint nodes have no wire path at all, and are synthesized instead.**
A `NodeHandle::Docker` node (`crates/worker/src/backend.rs`) is a direct socket
endpoint from the `DOCKER_NODES` seed: it is load-probed through
`DockerBackend::load_by_node`, not through `probe_worker`, and `probe_worker`
early-returns on any non-`Worker` handle. It therefore can never answer a ping
or publish an announce, and would read as absent permanently — including
`resources_enforced`, which it demonstrably *does* enforce. So the dispatcher
**synthesizes** capabilities for these nodes from the roster rather than from the
wire: `modes: [Container]`, `platform: unknown`, `leases: []`,
`resources_enforced: true`. Same values as the absent-default table, but derived
from the node kind rather than from silence — which is the honest description,
and it means a future host-capable docker endpoint (there is no such thing
today) would need its own mechanism rather than inheriting this one by accident.

A narrower alternative reaches the same guarantee and is worth naming: scope the
§7 requirement to host mode only, so a `cpu`/`memory` declaration constrains
placement solely among host-capable candidates and the container fleet is never
consulted about enforcement. It is cheaper — no default to get wrong. It is not
recommended only because it makes `resources_enforced` mean "…among host nodes",
a qualifier that has to be remembered at every call site; the defaults above
make the predicate uniform. Either is defensible; do not ship both readings.

### Ping or announce?

The brief is right that the silent-failure asymmetry is the strong argument, and
it points at ping. The verified asymmetry (#293's "What is true today", and
still true in this tree): `ping` rides the daemon's `subscribe
req.worker.{node}.>` grant and a failure marks the node out of service loudly;
`announce` is a publish whose grant was denied for weeks with only a node-side
`tracing::warn!`. The grant now exists in code —
`auth::nats::worker_permissions` publishes `_INBOX.>` **and**
`store::subjects::worker_announce()` — but the *class* of failure is
structural, not historical.

For capabilities the asymmetry is **worse than it was for capacity**, and this
is the decisive point. A capacity field that fails to arrive leaves a *stale
number* — visibly wrong, and #293's provenance chips surface it. A capability
field that fails to arrive leaves a node reading as container-only, which is
**indistinguishable from a node that legitimately cannot run host work**, and it
fails **closed**: host jobs queue on a fleet that can actually serve them, then
escalate with `no_free_slots_timeout` 30 minutes later. There is no observation
that distinguishes "denied publish" from "correct answer".

**Recommendation: capabilities ride the `ping` reply as the authoritative
source, and ride the announce too, solely so a node the dispatcher has never
pinged is not misclassified on join.**

Two supporting facts make ping-authoritative work cleanly:

- **Ping is pulled on the very attempt that needs the answer.** `place()` reads
  loads across candidates and `probe_worker` pings each one, so a placement
  attempt for a host task learns every node's capabilities as a side effect of
  the placement it is already doing. A denied announce degrades to *one extra
  probe*, not to a misclassification.
- **The capability filter must therefore be applied *after* probing**, not
  before, or there is a bootstrap deadlock (never ping a node because it reads
  container-only, so never learn it is not). Concretely: capabilities become
  part of `PlacementCandidate` and `choose_placement` gains a required-mode
  predicate — keeping the decision a pure, unit-tested function with no I/O,
  which is where docs/reference/style.md Tier 2 rule 1 wants it.

**Why the announce is still needed and cannot be dropped:** #293 states it and
it holds — the announce is how an *unknown* node joins, and the dispatcher
cannot ping a node it does not know exists.

**Why capabilities need no `(capacity_epoch, capacity_generation)` ordering
key.** #293 built that pair because capacity is *operator-mutable at runtime*
and pushed, so a stale in-flight announce could undo a fresher observation.
Capabilities are **boot-time facts**: `WORKER_MODES` changes only by recreating
the daemon, and a `nixos-rebuild switch` that adds a capability restarts the
unit anyway. Last-writer-wins on the pull path is sufficient; a stale announce
can at worst misclassify for one probe, which the next placement attempt
corrects. Not reusing the ordering machinery is a deliberate simplification, and
this paragraph is the justification for it.

### Rejected alternatives

- **Announce-only.** Cheaper — one struct, one publish path, #293 is editing it
  anyway. Rejected on the fails-closed-and-invisible argument above. This is the
  brief's question and the answer is not "default to announce".
- **Dispatcher-side static config** (`DOCKER_NODES` gains a mode: `nuc|worker|2|host`).
  Cheapest of all — zero protocol work — and it genuinely works for a two-node
  fleet. Rejected as the *durable* mechanism for the same reason #293 rejected
  its Option C: it relocates a physical fact (does this node have nix? `/dev/kvm`?
  a provisioned user pool?) into operator-typed config that goes silently wrong
  after a `nixos-rebuild`. But it is the right **bootstrap**: phase 1 below uses
  a pin and needs no capability wire at all.

### Sequencing with #293

Land capabilities **after #293 job 3** (dispatcher-side observation ingest), in
the same shape and in the same place — applied inside `probe_worker`, on the
reply path, before the startup gate reads the node. #293 §1 flags that ordering
as load-bearing and currently implicit; adding a second field to the same reply
must not be the change that breaks it. Do not race #293: a capability field
merged into `PingOk` while job 2 is in flight is a merge conflict in a wire type
during a rollout window, which is the one place this repo has already been
burned.

---

## 5. Placement and exclusive resources

### 5a. Capability-aware placement

`PlacementCandidate` (`crates/container/src/lib.rs`) gains the candidate's
advertised modes; `choose_placement` gains the required mode and skips
candidates lacking it, *before* the existing `free <= 0` and out-of-service
checks so the diagnostics stay distinguishable:

- no candidate advertises the mode → `NoCapacity("no node advertises host
  mode")`
- candidates advertise it but none has a free slot → the existing
  `NoCapacity("no free slots on any node")`

Both are transient by the §3.5 contract; only the message differs, and the
message is the diagnosis. Existing tests are unaffected if the required mode
defaults to `Container`, which is also the correct default for every job type in
the tree today.

### 5b. Exclusive resources (device leases)

Design #308 H.5's problem, restated concretely: a 2-slot host node will run two
tasks that collide on one `beacon-emu` AVD or one booted simulator, and
`placement.node` **routes but does not exclude** — §3.1 is explicit that the pin
is the only affinity control and there are "no labels, no anti-affinity".

**Declaration — inside `placement`, and the location is the argument:**

```yaml
placement:
  node: gumbo-nuc-0
  leases: [ios-sim]
```

`Placement` carries `#[serde(deny_unknown_fields)]`
(`crates/types/src/job_type.rs`), so an N−1 dispatcher **hard-rejects** a config
with `placement.leases` rather than ignoring it. That is normally the expensive
choice — and here it is the *correct* one, and it is why the field must not be a
tolerated top-level `exclusive:` instead. A silently-dropped exclusivity
constraint lets two tasks fight over one device: a correctness failure, exactly
the class §14.2 keeps out of nested blocks ("config ahead of binary" must never
become "a gate quietly disabled"). Note the consequence honestly: **this field
alone forces the epoch bump even without the host selector**, so ship them under
the same epoch or accept two bumps.

**Semantics.**

- A lease is **node-scoped**: the key is `(node, name)`. The AVD lives on one
  node, so a fleet-wide name would be wrong the moment two nodes each have a
  simulator.
- **A job type declaring `placement.leases` must also declare
  `placement.node`** — one more `validate()` rule. The reason is structural, not
  stylistic: the lease table lives in the dispatcher actor, but node selection
  happens *below* the actor, inside `FleetBackend::place`
  (`crates/worker/src/backend.rs`), which is called from `FleetBackend::launch`.
  The actor therefore does not learn the node until the launch returns a
  `ContainerId` of the form `{node}/{id}` — far too late to decide whether the
  device was free. Requiring the pin makes the node a *config* fact the actor
  already holds on the turn it decides to launch, and it matches the physical
  reality that a device is attached to one named machine.
- The dispatcher holds `held: HashMap<(Node, LeaseName), TaskId>` **in the
  actor**. Single writer, no lock, no second scheduler.

**Acquire and release points, named concretely.** One tempting shortcut must be
ruled out first: `Reservation` (`crates/worker/src/backend.rs`) looks like the
natural home, and is not one. Its own doc comment says it is "held across the
launch RPC; its drop releases the reservation once the container exists" —
`FleetBackend::launch` binds it as `_reservation` for the duration of a single
call. Hanging a lease on its drop would free the device the moment the task
*started*, which is exactly the collision the primitive exists to prevent. The
real points are:

- **Acquire** on the actor turn that decides to launch, immediately before
  `self.backend.launch(config)` — the same turn that on the failure branch
  stamps `pending_reason`/`queued_at` for the §3.5 queue
  (`crates/dispatcher/src/launch_queue.rs`). If any declared lease is held, the
  turn takes the queue branch instead and never calls `launch`.
- **Release** at the terminal transitions the actor already handles. All but one
  of them funnel into a single fan-in, `Core::on_task_exited`
  (`crates/dispatcher/src/exec.rs`): normal exit, launch failure (which
  `report_launch_failure` turns into a `Msg::TaskExited`), the §3.5 timeout kill
  (`crates/dispatcher/src/scan.rs` sends `TaskExit::code(-1)`), and the §3.6
  container-gone path (`crates/dispatcher/src/reconcile.rs`). Releasing there
  covers four of the five paths in one place.
- **Revoke is the exception and must release explicitly.** `Core::revoke_job`
  (`crates/dispatcher/src/core.rs`) kills containers, closes pending tasks and
  drops the job's exec state directly; `on_task_exited` then *deliberately
  discards* the late exits from those containers, because a revoked job has no
  exec state to transition. So a lease released only in the fan-in would leak
  forever on every revoke. Release beside `close_pending_tasks`, in the same
  loop that already walks the cascade targets.

**Lease lifetime is the task's lifetime**, which is strictly longer than a slot
reservation's. The two are unrelated mechanisms and should not be described in
terms of each other: post-launch slot accounting is the node's live count
(`ping.running`), not a reservation at all.

The rejected alternative is worth naming because it is the one that would let
leases work *without* a pin: thread the held set into `PlacementCandidate` so
`choose_placement` filters on lease availability the way [5a](#5a-capability-aware-placement)
filters on mode. That is a genuinely better end state — "any node with a free
simulator" — and it is deliberately deferred, because it means either
snapshotting the actor's table into the backend on every launch (two readers of
a fact with one writer, and a stale snapshot races) or moving the table into
`FleetBackend`, where §3.6 cannot rebuild it. The pin-required form is a strict
subset: adding the candidate predicate later relaxes the `validate()` rule
without changing any semantics decided here.

**An unavailable lease is `BackendError::NoCapacity`**, message `lease {name}
held by task {id}` — synthesized on the actor turn above, without calling
`launch` at all. That is the whole point: it inherits §3.5's behavior for free —
parked `Pending` with `pending_reason: QueuedForCapacity`, a `task-queued`
event, **no retry budget consumed**, FIFO drain when the holder exits (every
container exit already re-attempts queued launches), and the 30-minute maximum
queue wait as the wedge backstop. Reusing the capacity queue rather than
inventing a waiter is what keeps this from becoming a second scheduler.

**Release on crash and on dispatcher restart.** Leases live in dispatcher memory
— the same class as the merge queue and the launch queue, both explicitly
in-memory and both rebuilt by §3.6. So §3.6 must rebuild them: for each
`Running` task recovered in step 2, re-acquire the leases its job type declares.
This is race-free by §3.6's existing argument — reconciliation completes before
the message loop and the launch-queue drain start, so no concurrent launch can
observe a half-built lease table. Two consequences worth stating rather than
discovering: a task whose container is found *gone* releases its leases as part
of being failed, and a job type edited between launch and restart to drop a
lease simply does not re-acquire it — correct, because job-type config is read
live.

Node-side leases were considered and rejected: the dispatcher is already the
single writer and a node-held lease would need its own acquire/release protocol,
its own crash story, and its own reconciliation — three mechanisms to avoid one
`HashMap` in a process that is single-threaded by design.

**Is H.5's cheap interim worth shipping first? Yes.** Pinning a device-bound job
type to a 1-slot node needs *zero* new mechanism — `placement.node` and a slot
count both ship today — and it is the same 1-slot pin that
[2](#2-the-traits-container-assumptions) wants to dodge the `/workspace`
collision during the prototype. One configuration change buys both. It stops
being adequate at a precise, nameable moment: **when the host node must run a
second, non-device-bound task concurrently.** Until then, building the lease
primitive is speculative.

---

## 6. Drain

**Verified, and the brief is right: do not invent a drain op.**

- `probe_worker` computes `free: slots − ping.running` as `i64`
  (`crates/worker/src/backend.rs`) and `choose_placement` skips `free <= 0`
  (`crates/container/src/lib.rs`), so lowering a cap below live occupancy skips
  the node and kills nothing.
- `schedulable: AtomicBool` on `NodeHandle::Worker` already stops placement
  while `route` still reaches the node, so running containers keep being waited
  on. It is set only by `mark_worker_unschedulable` from heartbeat lapse — the
  node cannot ask to be drained — but the *mechanism* H.6 wanted exists.
- After #293, `slots: 0` **is** the operator-intent drain and it already reads
  as intent, not as unhealthy: it carries `set_by`/`set_at` in `fleet.capacity`,
  and #293 §10 gives the fleet view a distinct display for it. "Is it empty
  yet?" is answered by `occupied` in `fleet.status` (§3.1 live fleet occupancy).

So the operator-intent signal distinct from "unhealthy" is #293's, and adding a
second one here would be the fourth capacity mechanism #293 exists to retire.
**Nothing new is required for `nixos-rebuild switch` — with one exception that
is specific to host mode and is not currently covered anywhere:**

> **Host tasks must not be children of the worker daemon's own unit.** In
> container mode the daemon is a container and `docker rm -f chug-worker`
> demonstrably leaves job containers running (`deploy/prod/build-worker.sh`;
> §3.1's self-refresh depends on it). In host mode, if the daemon is a systemd
> unit and task processes are in its cgroup, `systemctl restart chug-worker` —
> which a `nixos-rebuild switch` performs — **kills every running task**. Launch
> each host task into its own transient scope (`systemd-run --scope
> --unit=chug-task-{id}`) so the daemon can be replaced under running work,
> preserving the §3.1 drain guarantee that a refresh never interrupts in-flight
> tasks.

That is the same transient scope [7](#7-resource-limits) needs for limits and
[2](#2-the-traits-container-assumptions) needs for kill-the-whole-group. It is
the single most load-bearing implementation detail in this document on Linux,
and it has no macOS equivalent — on macOS, a daemon restart under running host
tasks is a known hazard to be handled by draining first.

---

## 7. Resource limits

`Resources { cpu: Option<f64>, memory: Option<String>, task_timeout:
Option<String> }` (`crates/types/src/job_type.rs`). `cpu`/`memory` reach
`nano_cpus` and `memory` on the Docker `HostConfig`
(`crates/container/src/docker.rs`).

**`task_timeout` is unaffected and keeps working everywhere** — it is enforced
dispatcher-side by the §3.5 timeout scan, not by the runtime. That matters: the
bound that actually rescues a wedged task is mode-independent.

`cpu`/`memory` are another matter:

- **Linux + systemd:** `systemd-run --scope -p CPUQuota=<cpu×100>% -p
  MemoryMax=<bytes>` is a faithful mapping. `MemoryMax` is cgroup v2
  `memory.max` and OOM-kills like Docker's `memory`; Docker's `nano_cpus` is
  itself implemented as a `cpu.max` bandwidth quota, so `CPUQuota` is the same
  knob, not an approximation.
- **macOS:** no cgroups. `ulimit -v` bounds address space, not RSS, and is
  actively misleading for runtimes that reserve large virtual mappings. There is
  no CPU cap. Limits are **unenforceable**.

### Options

1. **Enforce where possible, advisory elsewhere.** Rejected. A `memory: 4Gi`
   that does nothing on one node and OOM-kills on another is the silent lie
   docs/reference/style.md Tier 2 rule 3 rejects ("everything is bounded … on hitting a bound
   fail fast and loud"), and it makes a job's behavior depend on invisible
   placement.
2. **Refuse the job type at validation time.** Impossible: `validate()` is
   offline and cannot know which node will run it — the same reason
   `placement.node` is only shape-checked (§3.1).
3. **Make enforceability a capability, and refuse at launch as the backstop
   (recommended).** The node advertises `resources_enforced`
   ([4](#4-capability-advertisement)); `choose_placement` treats a job type
   declaring `cpu`/`memory` as requiring it, exactly like a mode. Because the
   capability defaults to **true** when absent — including for the synthesized
   docker-endpoint nodes — this predicate is a no-op for the container fleet and
   for every job type in `.chug/jobs/` today; only a host node that declares it
   cannot enforce is ever filtered out. A launch that
   nonetheless arrives (via a pin) at a node that cannot enforce is a hard
   `BackendError::Launch` naming the field and the node — never a silent
   ignore.

**Recommendation: option 3.** It costs one boolean and reuses the [5a](#5a-capability-aware-placement)
filter, so the common case is an ordinary placement decision (queued,
transparent) rather than a failure.

The corollary is worth stating plainly because it is a real constraint on the
category F of #308: **to run host jobs on macOS at all, a job type must not
declare `resources.cpu` or `resources.memory`.** The platform says "I cannot
bound this here" instead of pretending; `task_timeout` still bounds it in time.

---

## 8. Secrets on a shared host

Today, `Core::container_env` (`crates/dispatcher/src/exec.rs`) puts into the
launch env: every declared `work.secrets` / evaluator `secrets` value
(age-decrypted immediately before injection, §8.2), the reserved
`global/agents` platform agent credentials for agent containers, project `vars`,
and a **minted per-task NATS creds file body** (`NATS_CREDS`, §7.4). In
container mode the boundary is the container. On a shared host, every one of
those is readable from `/proc/<pid>/environ` by any process of the same uid, and
by root.

### Options

- **(a) Accept and document.** Defensible only if a host node is both
  single-tenant *and* single-task; a 2-slot host node running two projects as
  one user is a genuine cross-project secret leak. Rejected as the default.
- **(b) Per-task unix user (recommended).** A **fixed pool** of pre-created
  users (`chug-task-0 … chug-task-{slots-1}`), one task at a time per user, task
  launched with `systemd-run --uid=` (or `su`/`launchd` equivalent). Then
  `/proc/<pid>/environ` requires root, and the task dir is 0700-owned by that
  user. A pool rather than `useradd` per task avoids user churn, maps one-to-one
  onto slots, and is trivially declarative on NixOS (`users.users.chug-task-*`).
  Costs: user provisioning becomes node config, and the caches in
  [9](#9-environment-and-state) must be per-user or group-shared.
- **(c) File-based injection at 0600.** Still readable by any process of the
  same uid, so it does not restore the boundary on its own — strictly weaker
  than (b) — and it requires every consumer (the agent harness, every
  `work.run` script) to change. It *does* compose usefully with (b) by keeping
  secrets out of `environ` for the task's **own** child processes, which matters
  against the [10](#10-trust-and-tenancy) threat of project-controlled code.
  Note it as a follow-up, not a substitute.

**Recommendation: (b), with a hard rule — the daemon does not advertise `host`
in its `modes` unless the user pool is provisioned.** That collapses "secrets
boundary present" into the existing capability, so an unprovisioned node is
simply not host-capable rather than silently downgrading its isolation. One
rule, no extra capability bit, and it fails closed.

Residual risk, stated rather than papered over: root on the node reads
everything; a task's own code reads its own secrets (unchanged from container
mode); and secrets injected into a host task are on the node's disk in a
process's memory rather than in a container the platform can destroy — so
`remove` scrubbing the task dir is part of the boundary, not just hygiene.

---

## 9. Environment and state

Design #308 H.6 argues the layering (system closure = machine facts, devshell =
per-project toolchains, in the project repo, never the node config). The
mechanics:

### Naming and resolution

`runtime.env` is an opaque **environment reference** interpreted by the node,
occupying the same slot `image:` occupies and carrying the same contract:
pinned, content-addressed, node-resolved. Form: `nix:<flake-ref>#<attr>`.

- **Relative** (`nix:.#chug-ci`) resolves against the **job branch checkout** —
  which is what makes a tool bump ship in the same commit as the code needing
  it, gated by the same CI (H.6's whole point).
- **Absolute** (`nix:github:owner/repo/<rev>#attr`) for environments shared
  across projects.

A relative ref inverts the container ordering and this is worth naming: an image
is pulled *before* the workspace exists, but a relative flake ref lives *inside*
the clone. So the host bootstrap is **clone → realise → exec**, not the reverse,
and the clone therefore runs outside the declared environment (it needs only
`git` and `ssh`, which are machine facts on a host node).

### GC roots

`nix build --out-link {task_dir}/.gc-root` (an indirect root under
`/nix/var/nix/gcroots/auto`) held for the task's duration. Without it a nightly
`nix-collect-garbage` deletes a running task's toolchain — a failure that would
look like a random mid-build explosion. `remove` drops the root along with the
task dir, and §3.6 step 6's existing sweep is the crash backstop
([2](#2-the-traits-container-assumptions)), so **no new sweep is needed**.

### The cold-realise cost

There is no pull phase and no concept of one: §3.5 starts the clock when
execution begins, and `task_timeout` covers everything from launch to exit. A
cold `nix develop` for a Flutter/Android toolchain is tens of minutes and lands
**inside** it, looking exactly like a slow job.

Options weighed:

- **Enlarge `task_timeout`.** Available today (per-type `resources.task_timeout`
  plus the per-job `Job.timeout` work-scoped override, §3.5) but dishonest — it
  conflates "this build is slow" with "this node is cold", so the timeout stops
  meaning anything.
- **Add a pull phase.** A new task sub-state, a second clock, and an edit to
  §3.5's timeout rule. That is a lot of platform machinery for a *first-run on a
  node* cost.
- **Move the cost out of band (recommended).** A node-side binary cache plus a
  warm set the daemon realises at startup and refreshes on a schedule. The
  common case then pays nothing; a genuinely new ref pays once per node.

**Recommendation: the third, plus an explicit statement in the job-type docs
that the first use of a new environment ref on a node is charged to
`task_timeout`.** Revisit the pull phase only if the prototype shows it hurts —
that is precisely the kind of question a prototype answers faster than an
argument.

### Declared mutable caches

`WORKER_CACHE_DIR` ships today, and it is **not** a namespaced mechanism: one
host dir, one bind, one fixed container path (`CACHE_MOUNT_PATH =
"/cache/sccache"` in `crates/container/src/docker.rs`), shared by every
container the node launches, with `RUSTC_WRAPPER`/`SCCACHE_DIR` injected
worker-side. It is safe *because* sccache's cache is content-addressed and
carries no job state — §3.1 makes exactly that the justification for it being
the one permitted exception to "no host bind-mounts".

`~/.gradle`, `~/.pub-cache` and `node_modules` satisfy neither property. So a
declared per-project cache is a **new mechanism, not a widening of the old
one**, and `WORKER_CACHE_DIR` should not be overloaded — it keeps its documented
contract for container mode unchanged.

Shape: a separate `WORKER_HOST_CACHE_ROOT`, with each declared cache mapped to
`{root}/{owner}/{project}/{purpose}` and exported as an env var into the task.
Two projects therefore cannot collide on `~/.gradle`.

Where the declaration lives is a real tension. The obvious answer — a
`runtime.caches: [gradle, pub]` field on the job type — puts a cache name on the
wire and breaks §3.1's property that the dispatcher is entirely cache-ignorant.
**Recommended instead: derive the cache set node-side from the environment ref**
(a `chug.caches` attribute on the flake output the node is already evaluating).
Nothing new rides the wire, the declaration stays repo-versioned in the project
repo per H.6, and the dispatcher stays ignorant. Fallback if reading a flake
attribute proves fiddly in practice: a wire field, accepting the lost property —
but try the flake attribute first.

**Eviction is mandatory, not optional** (docs/reference/style.md Tier 2 rule 3: everything is
bounded). Per-project caches grow without limit on a node whose disk also holds
`/nix/store` and every task's `workspace/`. Recommend LRU by directory mtime
against a node-configured ceiling, with the sweep bounded and loud on hitting
it. "No policy yet" is the answer that fills a node in a month.

---

## 10. Trust and tenancy

Resolving a project flake runs **project-controlled code on the host as the task
user** — flake evaluation, derivation builders, and then the task command
itself, which is project code in either mode. The difference is blast radius:

| | Container mode | Host mode |
| --- | --- | --- |
| Reachable | the container's overlay and env | the task user's home, every cache under it, `/nix/store` (additive — the store is root-owned and immutable, which genuinely bounds this), the node's process table |
| Persists after the task | nothing — the overlay is removed | anything under the declared caches, by design |
| Docker socket | absent | **present on the node**, because container mode needs it |

That last row is the sharp edge and it is specific to a **mixed-mode** node. A
host task that can reach `/var/run/docker.sock` is effectively root on the node
and can read every other project's containers. So: **host tasks do not get the
docker socket.** A job type that needs one (#308 category D's image-build case)
is a node-side allow-list entry, never a job-type field the platform honors on
request. Deciding that case is doc 4's, not this one's.

### Are host nodes single-tenant?

**Yes, by policy, and enforced at the node.** `WORKER_HOST_PROJECTS` lists the
`owner/project` slugs a node will run host work for; a host launch for anything
else is a hard `BackendError::Launch`, not `NoCapacity` — it can never clear
without a config change, so queueing it would be a 30-minute silence with a
known answer.

The argument for stating this rather than leaving it implicit: the per-task user
boundary ([8](#8-secrets-on-a-shared-host)) handles *accidental* cross-reading
between concurrent tasks. It does not handle a hostile or compromised flake, and
nothing short of a VM per task does — which is the isolation model host mode
exists to give up. #308 H.3 cost 4 is right that there is no third option: the
cache reuse **is** the win and **is** the contamination risk, they are the same
property. Making tenancy explicit and machine-checked is the honest version of
accepting it.

Accepted within a single-tenant host node: a project's own code persists across
its own tasks. That is the feature.

---

## Phasing

What can be prototyped on one node with nothing migrated:

| Phase | Work | Needs | Notes |
| --- | --- | --- | --- |
| **P0** | Backend polymorphism ([1](#1-backend-polymorphism)) + a `HostBackend` ([2](#2-the-traits-container-assumptions)), on one node with `WORKER_MODES=container,host`, routed by `placement.node`, `slots: 1` | nothing else | **No schema change, no epoch bump, no capability wire, no placement change.** The job type still declares `image:` and that node simply ignores it. This is deliberately a lie and must never leave the prototype node — but it answers the only question that matters: *which of the ten methods is actually hard* |
| **P1** | The `runtime:` selector, the `CONFIG_SCHEMA_EPOCH` bump, the `min_dispatcher` requirement, the validate rule ([3](#3-the-host-mode-selector)) | P0 | Still pinned; `image` stops lying. **Half-landed ahead of P0** — see the note below |
| **P2** | `NodeCapabilities` on ping + announce; capability-aware `choose_placement` ([4](#4-capability-advertisement), [5a](#5a-capability-aware-placement)) | P1, **and #293 job 3** | Unpins host work — **Landed** (jobs #483, #484) |
| **P3** | Per-task users ([8](#8-secrets-on-a-shared-host)); `resources_enforced` ([7](#7-resource-limits)); transient scopes ([6](#6-drain)) | P2 | The isolation and bounding story; the scope work is Linux-only |
| **P4** | Device leases ([5b](#5b-exclusive-resources-device-leases)) | P2 | Only when the host node must run a second, non-device-bound task concurrently |
| **P5** | Declared caches + GC roots + warm set ([9](#9-environment-and-state)) | P1 | Independent of P2–P4 |

**P1's schema half landed before P0, deliberately (job #401).**
[#373](373-project-toolchains.md) Decision 2 needed the same block for
container-mode toolchains, so the whole designed table now exists in
`crates/types/src/job_type.rs` — `runtime.mode` (`container` default | `host`)
and `runtime.env` — at `CONFIG_SCHEMA_EPOCH` **4**, not 2 (#376 spent 2 → 3
first), with `RUNTIME_SCHEMA_EPOCH` frozen at 4 and required of any `runtime:`
beyond a bare `mode: container`. **Only the container row validates**: `mode: host` parses and is
refused by `validate()` as unsupported-because-unbuilt, naming P0. So the epoch
is spent once, nothing unservable is expressible, and what P0/P1 still owe is
the backend, the placement, and the host row's own field rules — the top-level
`image` ban, the required `env`, and the evaluator-image narrowing — plus
deleting one refusal. No epoch is left to spend.

P0 is the one to start. #308's ordering already says phase 2 should be a
prototype rather than a design carried to completion, and this document's own
section 2 is why: the *number* of host analogues is knowable from the trait
today, and their *difficulty* is not — with the `/workspace` collision the
leading candidate for "harder than it looks".

**Prove P0 against a boring, cache-heavy build, not against
`flutter-integration-tests`.** #308 H.4 is right that it sits at the confluence
of too many unbuilt things.

## Contracts changed (per docs/reference/style.md's contract-first rule)

| # | Slice | Contract changed | Depends on |
| --- | --- | --- | --- |
| 1 | `code` — `ContainerBackend` gains `managed_running_total` (provided); daemon holds `Arc<dyn ContainerBackend>`; `WORKER_MODES` parsing | `ContainerBackend` trait surface; worker config | — |
| 2 | `code` — `HostBackend`: task dir, process group, the ten methods, the `/workspace` rebase rule | new backend implementation; `ContainerId` shape (`{node}/{task_id}`, unchanged) | 1 |
| 3 | `docs` — spec §3.1 amendment: the host node kind, the selector, capability advertisement, the mode filter in placement; fix the stale trait listing (correction 3) | spec §3.1, §1.1 field-rules matrix | 2 |
| 4 | `code` — `runtime:` block, field rules, the per-level mode precedence rule and the narrowing of the evaluator `image` requirement, the `CONFIG_SCHEMA_EPOCH` bump (#401 spent 3→4), the `min_dispatcher` requirement | job-type schema epoch (§14.1); `Evaluator`/`wrap_up` image resolution | 3 |
| 5 | `code` — `NodeCapabilities` on `PingOk` + `WorkerAnnounce`; ingest inside `probe_worker` | two wire records (additive, no `WORKER_RPC_VERSION` bump) | 4, **#293 job 3** |
| 6 | `code` — `choose_placement` capability predicate + the two distinct `NoCapacity` messages; the fleet-wide "no node advertises" warning — **Landed** (job #484) | `choose_placement` postcondition; §3.1 placement | 5 |
| 7 | `code` — `placement.leases` (+ the "leases require a pin" rule), the actor lease table, release in `on_task_exited` **and** on the revoke path, §3.6 rebuild | `Placement` schema (nested, breaking); `on_task_exited` postcondition; `revoke_job` postcondition; §3.6 reconciliation | 6 |

Test placement per [docs/reference/testing.md](../reference/testing.md): the selector field rules, the
`WORKER_MODES` parse, the capability defaulting (`None` ⇒ container-only), and
the extended `choose_placement` predicate are pure functions → **tier 1**, beside
the existing `choose_placement` tests in `crates/container/src/lib.rs` and the
`parse_slots` tests in `crates/worker/src/config.rs`. The host backend's
launch → inspect → logs_tail → remove round trip and the daemon's mode routing
are **tier 2**. The §3.6 lease rebuild belongs with the existing reconciliation
tests.

## What this makes wrong elsewhere

- **`docs/spec.md` §3.1's inline `ContainerBackend` listing** omits `logs`,
  `logs_tail` and all five provided methods — already stale, fixed by slice 3.
- **`docs/reference/crates.md`'s `container` row** reads "`ContainerBackend` trait + Docker and
  k8s implementations"; k8s is a stub and a host backend would make the row
  wrong in a second way.
- **`crates/container/src/lib.rs`'s module doc** ("Docker socket in dev, the
  Kubernetes Jobs API in production") describes a deployment that does not
  exist.
- **`docs/spec.md` Appendix: Deferred** — "macOS bare metal dispatchers … Execution
  model needs separate design" is the entry this document answers; it should
  point here rather than stay open-ended.

## Risks and open questions

- **The `/workspace` rebase is the schedule risk**, not the backend. It touches
  the dispatcher (`launch_queue.rs`), the agent crate (`transcript_path`, whose
  value is a *measured* property of an external CLI), and `bootstrap_cmd` —
  shared code that container mode also uses. P0 sidesteps it with a 1-slot node;
  P1+ cannot.
- **Nobody in this tree has built a non-container backend.** The count of host
  analogues is knowable; their difficulty is not. That asymmetry is why P0 is a
  prototype and why this document declines to pre-commit the ordering past P2.
- **macOS gets a materially weaker product**: no cgroup limits ([7](#7-resource-limits)),
  no mount namespaces ([2](#2-the-traits-container-assumptions)), no transient
  scopes ([6](#6-drain)), a harder per-task-user story ([8](#8-secrets-on-a-shared-host)).
  Every one of those is stated as a refusal or a capability rather than a silent
  downgrade, which is the most this design can honestly do.
- **`.chug/tasks/ci.sh` assumes a `nats-server` binary or a Docker daemon** for
  tier-2 tests (it self-skips otherwise, and its tier-summary logic exists to
  make the skip loud). A host node's environment must supply one, or *this
  repo's own* CI gate goes partial on host nodes — the failure #308 H.3 cost 6
  names, verified in the script.
- **Racing #293 is the operational risk.** Slice 5 edits `PingOk` and
  `WorkerAnnounce`, which #293 job 2 is also editing, during rollout windows the
  platform has already been burned in. The sequencing in
  [4](#4-capability-advertisement) is a hard dependency, not a preference.

---

## Correction, 2026-08-05 — §1 already shipped, P0 landed

Appended by job #434, which implemented P0. Nothing above is edited; this
section is the record of what the document got wrong and what building it
taught.

### §1's recommendation had already shipped

Option A is not a decision P0 had to make — it was already the tree. Measured on
`main` at `a978bd5`:

- `crates/worker/src/daemon.rs:205` holds `backend: Arc<dyn ContainerBackend>`,
  not `DockerBackend`. Correction 2 above ("a `dyn ContainerBackend` field alone
  does not compile") was true when written and was resolved by design #322 W1,
  which moved `with_cache_dir`/`ping_all` into one construction function and
  `managed_running_total` onto the trait.
- `managed_running_total` (`crates/container/src/lib.rs:251`) is already a
  provided method defaulting to `list_managed_running().len()`, with
  `DockerBackend` keeping the cheap label-filtered override — exactly the shape
  §1 recommends.

§1's roster is also stale. `grep -rn "impl ContainerBackend for" crates/` now
returns **four**, not the three "the honest limit" paragraph verified:
`FakeBackend` (`crates/test-utils/src/lib.rs`), `DockerBackend`, `FleetBackend`
(`crates/worker/src/backend.rs`) and `StubBackend` in `lib.rs`'s own test
module. `HostBackend` makes five.

So what P0 actually owed was smaller than §1 implies: the config seam and the
backend. The construction function §1 names `local_backend` existed as
`build_backend`; #434 renamed it to the designed name and gave it the host
branch.

### What P0 built, and what it did not

`crates/container/src/host.rs`, reachable only from a node whose `WORKER_MODES`
names `host`, off everywhere by default. No `runtime: { mode: host }` support —
job #401's refusal stays, and deleting it is P1's. No schema change, no epoch
bump, no capability wire, no placement change. The job type still declares
`image:` and a host node ignores it; the backend logs that as the lie §Phasing
says it is, at boot and at every launch.

`WORKER_HOST_ROOT` was added beside `WORKER_CACHE_DIR` (same stable-path rule,
default `/var/lib/chuggernaut/host-tasks`) because the task directory §2
describes needs a root and the operator must be able to put it on a disk that
can hold one.

### Which of the ten methods was actually hard

The document's own question. Ranked by what building them cost, against §2's
predictions:

| Method | §2 predicted | Measured | Note |
| --- | --- | --- | --- |
| `remove` | "trivial to write, load-bearing to get right" | **hardest** | §2(c) under-priced it, see below |
| `inspect` | "genuinely awkward" | **second hardest, and for a different reason** | pid identity was the *easy* half |
| `launch` | "moderate — the path problem lives here" | moderate, but the path problem was **free** under option (iii) | |
| `kill` | "trivial, with one caveat" | trivial | the caveat (a `setsid()` escape) is real and unaddressed |
| `logs`, `logs_tail` | "trivial, and strictly better" | **correct — trivial and strictly better** | one fd; the offsets are definitionally stable |
| `copy_file` | "trivial given the rebase rule" | trivial *without* a rebase rule | option (iii) makes the literal path correct |
| `list_managed_exited`, `list_managed_running` | trivial | trivial | both derive from one `status()` |
| `wait` | trivial | trivial | a poll over `inspect`; the daemon never calls it |
| `managed_running_total` | free | free | the trait's default is right for a node-local backend |

**Where the design under-priced the work:**

1. **`remove` needs an ownership record the design does not mention.** §2(c) is
   right that the call must delete a 5–10 GB `workspace/` and that nothing else
   will. What it misses is that under option (iii) the workspace is a *shared,
   fixed* path, so `remove(A)` arriving after `launch(B)` has claimed it would
   delete B's clone. The fix is a `workspace-owner` file in the host root that
   `remove` checks before deleting anything outside its own task directory — it
   must survive a daemon restart, so it is a file rather than a field. Option
   (i)'s rebase dissolves this: a per-task workspace has no shared owner.
   `remove` also has to reclaim every path the launch materialized *outside* the
   task directory (`/chuggernaut/prompt.md`, the ssh cert, the MCP config), so
   `meta.json` records them.

2. **`inspect`'s hard half is the exit code, not the pid.** The pid-identity
   rule §2(b) specifies is mechanical: record field 22 of `/proc/<pid>/stat`
   (`ps -o lstart=` off Linux) and treat a mismatch as gone. What §2 does not
   say is that the *authority* for a task's exit status cannot be the daemon.
   If the daemon reaps the child, then a `worker-refresh.sh` swap mid-task —
   which spec §3.1 guarantees does not interrupt in-flight work — loses the
   exit code, and the surviving pid rule reports a task that **succeeded** as
   `Exited { -1 }`. So the task writes its own status: the launch command is
   wrapped in `sh -c '"$@"; s=$?; … ; exit $s'` with the paths passed as
   environment. The daemon's reaper is only a backstop, and the in-memory live
   set is what keeps a just-exited task from reading as gone in the window
   before the status file lands. Three sources, in that precedence order.

3. **`/workspace` cost nothing, because option (iii) was taken.** §2(a) calls
   this "the sharpest single finding" and the Risks section calls the rebase
   "the schedule risk". Both are right about P1 and both are irrelevant to P0:
   with one task per node the literal path is correct in `bootstrap_cmd`, in
   `launch_queue.rs`'s `copy_file("/workspace/eval-result.json")`, and in
   `agent::transcript_path`'s measured `-workspace` slug. The prototype's job
   was to find out whether the seam fits a non-container runtime, and it does.
   Rebasing is still the durable answer and is still not free.

4. **The exclusion has to be enforced twice, not assumed once.** §2 and §5 both
   describe `slots: 1` as configuration. Configuration is not enforcement: an
   operator's `WORKER_SLOTS=4`, or a runtime `set_slots` raise (spec §3.1
   operator capacity control, which the design never reconciles with the 1-slot
   pin), puts two host tasks on one `/workspace`. #434 refuses the boot when
   `host` is declared with `WORKER_SLOTS`/`WORKER_SLOTS_MAX` other than 1, *and*
   refuses a second concurrent launch in the backend with `NoCapacity` — the
   transient class, so §3.5 queues and retries and no retry budget is spent.

5. **`resources.cpu`/`memory` are silently unenforced on a host node, today.**
   §7 recommends making enforceability a capability with a hard `Launch` refusal
   as the backstop, and that is P2/P3 work. In P0 a host launch *warns* and runs
   unbounded, because every job type in `.chug/jobs/` declares both and refusing
   would leave nothing to prototype against. `task_timeout` still bounds the
   task in time. This is the one place P0 knowingly ships the "silent lie"
   §7 option 1 rejects, and it is confined to a node an operator opted in.

6. **The shipped daemon is a container, and the design never says so.** This is
   the finding P0 did not expect and the one that most affects the proof. §6
   reasons about the daemon as a systemd unit ("if the daemon is a systemd unit
   and task processes are in its cgroup, `systemctl restart chug-worker` kills
   every running task"), and §2 assumes the node's own filesystem. But
   `deploy/prod/build-worker.sh` runs `docker run -d --name chug-worker` with
   only `/var/run/docker.sock` and `keys` mounted — the same shape design #372
   C3 records for `WORKER_CACHE_DIR`. A `HostBackend` inside that container
   spawns processes in the *daemon container's* namespace, with the daemon
   container's `/workspace` and its own root filesystem. That is not
   host-native execution; it is a second, worse container runtime. So P0's
   proof requires a daemon run **natively** on the node, which nothing in the
   deploy path does today. `build-worker.sh` and `worker-refresh.sh` do forward
   `WORKER_MODES` since #439 — per node, unset staying unset — so the knob is
   settable rather than unreachable; what it cannot make host-native is the
   daemon, so a declared `host` on a deployed node still buys the second
   container runtime above and not the thing P0 wants proved. Off everywhere by
   default is now the *default*, not the construction. Deciding what a
   natively-run daemon looks like (its supervision, its credentials, its
   relationship to the containerized one on a mixed-mode node) is not P0's and
   is not currently anybody's.

**Unaddressed and known:** a task that calls `setsid()` escapes the process
group and survives `kill` (§2's caveat — the Linux answer is §6's transient
scope, P3); secrets are readable from `/proc/<pid>/environ` by any process of
the same uid (§8, P3); the host task inherits the daemon's environment,
including a reachable docker socket on a mixed-mode node (§10).

The third of those was **closed by job #442**, implementing
[#440](./440-native-worker-daemon.md) slice 1: `spawn_task` clears the
environment and composes one, so a host task carries the dispatcher's launch
env, a `PATH`/`HOME` floor and the two exit-status paths, and nothing the daemon
was started with. The other two stand — §8 in particular is untouched, since
`env_clear` narrows *what* a `/proc/<pid>/environ` reader finds, never *who* may
read it.

### Test placement, as landed

Tier 1 (`crates/container/src/host.rs`, `crates/worker/src/{config,daemon}.rs`):
the pid-identity rule including same-pid-different-start-time, `/proc` field-22
parsing against a comm field holding spaces and parens, the exit-status wrapper,
id/path traversal refusal, `WORKER_MODES` and `WORKER_HOST_ROOT` parsing, the
backend-kind choice and the capacity refusal. Tier 2
(`crates/container/tests/host_backend.rs`): the launch → inspect → logs →
`logs_tail` → `copy_file` → `remove` round trip, the one-task-at-a-time
exclusion, the group kill, and a simulated daemon restart asserting a lost task
reads as `Exited`, **not** `Running`. **None of it needs Docker or NATS** — a
host task is a process group and a directory — so the whole suite runs on a
Docker-less evaluator.

**Still not verified:** that host execution works on a real node. That needs an
operator to set `WORKER_MODES=container,host`, `WORKER_SLOTS=1` and pin a job —
a later proof, in the shape of `android-proof` and `gcp-proof`, and per #308 H.4
against a boring cache-heavy build.

### Addendum, 2026-08-05 — `remove` was harder than "hardest" recorded above

The table above ranks `remove` hardest and blames the ownership record. CI then
found a second, independent way for it to fail, and this one is a *race* rather
than a missing fact.

`remove` deleted the task directory in place with `remove_dir_all`. The
daemon's order is kill-then-sweep, so at that moment the task's own reaper is
frequently still writing `exit_code` into that same directory: the walk unlinks
the entries, the reaper's `rename(exit_code.tmp → exit_code)` repopulates it,
and the final `rmdir` fails with `ENOTEMPTY`. Because §2(c) asks for loud
failure, the whole `remove` then reported **leaked disk when nothing had
leaked** — the loudness worked exactly as designed and pointed at the wrong
thing.

It is invisible unloaded and deterministic under load: 0/12 failures on an idle
machine, 12/12 with the cores oversubscribed, which is what a full
`cargo test --workspace` gate looks like and is why CI found it and the targeted
run did not.

The fix is to **detach before deleting** — rename the task tree to a
`.removing-` sibling, then delete the renamed path. The rename is atomic and
every writer addresses the old path, so the delete has no concurrent writer at
all; a late `write_exit_code` fails with `ENOENT` and cannot resurrect the tree,
because `std::fs::write` never creates a parent. The rename opens a crash window
that would leak a whole task tree, which is the §2(c) failure itself, so
construction sweeps leftover `.removing-` trees; the leading dot is what makes
`is_task_id` reject them, so a half-removed tree never reads back as a task.

Two things generalize past this backend:

1. **A container runtime hid this.** `docker rm` is one call to a daemon that
   owns the container's whole lifecycle, so "delete the task" is atomic by
   construction. On a host node the task directory is an ordinary directory with
   two independent writers, and the trait's `remove` says nothing about
   concurrency because under Docker there was none to say anything about. Every
   host analogue inherits the trait's *signature* for free and its *isolation*
   not at all — which is the P0 question, answered on a method the design had
   already flagged as the load-bearing one.

2. **The under-pricing is a pattern, not an instance.** §2 rated `remove`
   "trivial to write, load-bearing to get right" and was right twice over: this
   is the third distinct obligation found on one method (delete the workspace,
   own the workspace, delete without a racing writer), and the first two were
   found by reading while the third needed a loaded machine. P1 should assume
   the remaining §2 "trivial" verdicts are about the *writing*, not the getting
   right.

## P1 as landed, 2026-08-07 — the host row's field rules (job #478)

P1 is complete. Job #401 had already landed the block and the epoch for
[#373](373-project-toolchains.md); this job landed the half that was still open
— the host row's own field rules in `crates/types/src/job_type.rs` — and deleted
the refusal §Phasing above describes as "only the container row validates". That
sentence, and every claim in this document's body that `mode: host` is refused,
is history as of this section.

What the rules are, exactly as [§3](#3-the-host-mode-selector) specified them:

- **Top-level `image` is disallowed** under `runtime.mode: host` and required
  under container mode — one rule with the resolved mode as its discriminant
  (`JobType::validate_top_level_image`), shared by the `agent` and `command` arms.
  Job types validated the old way are untouched: absent `runtime:` resolves to
  container.
- **`runtime.env` is required** under `mode: host`. #322 argues the toolchain
  case; the plainer one is that a host task with no declared environment runs
  against whatever the node's bare `PATH` holds, which is the
  non-reproducibility [#322](322-macos-native-runtime.md) exists to remove.
- **The evaluator and `wrap_up` image requirements are narrowed** to levels
  whose resolved mode is container, per
  [Coexistence](#coexistence-on-a-mixed-fleet). Without that narrowing the
  requirement fires on *every* host job type, which is the deeper reason the
  blanket refusal was there: a host job type could not satisfy the top-level ban
  and the fallback requirement at once. `wrap_up` is narrowed alongside the
  evaluators because §3 names all three levels, not two.
- **No epoch bump.** `RUNTIME_SCHEMA_EPOCH` stays 4 — #401 spent it, and the
  `min_dispatcher` requirement it installed already covers any declared
  non-container mode. A host job type carries no top-level `image`, so an N−1
  dispatcher rejects the config and parks the type (§14.2) instead of running it
  containerized; the declared epoch is what makes that park legible rather than
  mysterious.

What did **not** change, and is still P2: nothing routes by mode. No
`NodeCapabilities` record exists, `choose_placement` cannot see a node's modes,
and no node advertises `host`. A host job type is therefore **well-formed and
unroutable** — it validates, and the platform has nowhere to put it. P2 stays
gated on [#293](293-worker-capacity.md) job 3. No job type in this repo declares
`mode: host`, deliberately: nothing serves one.

## P1 as landed, 2026-08-07 — per-launch mode routing (job #479)

**P0's node-level backend choice was a shortfall against [§1](#1-backend-polymorphism), not a
decision this document made.** §1 says a daemon whose `WORKER_MODES` includes
`host` "constructs both backends and routes each launch by the request's
declared mode", and [Coexistence](#coexistence-on-a-mixed-fleet) says "mode
resolves per launched task". What P0 shipped instead was `backend_kind`: one
`Arc<dyn ContainerBackend>` picked at boot, any mention of `host` winning. So
`WORKER_MODES=container,host` made a node **host-only** — every ordinary
`code`/`docs`/`web` job placed there would have run as a host process against
the machine's bare `PATH`, and the `ci` evaluator would have ignored its image,
which is precisely the case Coexistence works through. P0's own log line called
that "deliberately a lie that must never leave the prototype node". This job
closes it; the P0 correction's description of that behaviour, and the Phasing
table's P0 row, are history as of this section.

**The mode reaches the worker as the image's absence.** P0 could not route
because the worker could not be *told*: `ContainerLaunchConfig.image` and
`WorkerLaunchRequest.image` were both `String`, and a host launch has no image
to send. Both are now `Option<String>`, all the way from the dispatcher's
`command_launch_config` / `AgentRunConfig`. Image *presence* is the selector
rather than a separate `mode` field, because §3's precedence rule already makes
it one — a level carrying an image is a container task regardless of the job
type's mode — and a mode field beside a mandatory `image` would leave a
meaningless `""` on every host launch and let the two disagree.

**`WORKER_RPC_VERSION` moves 1 → 2**, in the same commit, per the rule its own
doc comment states: an additive op does not bump it, a breaking change to an
existing op's shape does. The break is one-directional and small — a v1 daemon
rejects an image-less launch as an unparseable payload, and a container launch
serializes byte-for-byte what v1 sent (the field is `skip_serializing_if =
"Option::is_none"`) — but it is a break, and the alternative (an additive `mode`
field) was rejected above. The fleet's three nodes deploy from one SHA, workers
before the dispatcher, so no mixed-version window can carry a host launch; none
could anyway, since placement by mode is P2.

**Each node constructs exactly what it declares.** `local_backend` builds the
docker backend iff the node serves `container` and the host backend iff it
serves `host`; a node naming both gets `worker::route::RoutedBackend` over the
two. A container-only daemon is byte-for-byte what ships today — it is handed
the `DockerBackend` directly, with no wrapper — and a host-only node never
constructs a `DockerBackend`, which is the point on a Mac that has no Docker.
Every op after the launch routes on the id the launch minted
(`container::host::names_host_task`, the `host-` prefix a docker id cannot
collide with); the two listings are the union, so the node's occupancy is the
node's.

**A wrong-mode launch is refused by name, in both directions.** `HostBackend`
refuses a launch declaring an image and `DockerBackend` refuses one carrying
none, each naming the node and the mode it serves, both as `BackendError::Launch`
— a hard failure, not the transient `NoCapacity` a placement bug would retry
forever. Neither backend falls back to the other: silently serving the wrong
mode is what P0 did.

**`enforce_host_capacity` stays node-wide, deliberately.** #309 §2 option (iii)
exists because two concurrent host tasks cannot both own `HOST_WORKSPACE`, and
the daemon has no per-mode slot accounting to express a narrower rule with. So a
dual-mode node runs **one task at a time, of either kind** — strictly more than
the host-only node P0's shortcut made of it, and a cap on concurrent *host*
tasks only is recorded here as follow-up rather than guessed at.

What did **not** change: placement. Nothing dispatcher-side routes jobs to nodes
by mode, no `NodeCapabilities` record exists, and no node advertises `host`.
That is still P2, still gated on [#293](293-worker-capacity.md) job 3. What this
job removes is the reason a node could not honestly advertise both.

---

## Note, 2026-08-07 — slice 5 landed, `RuntimeMode` not `ExecMode` (job #483)

Appended by the job that implemented P2 slice 5. Nothing above is edited; this
is the record of what the tree got that [§4](#4-capability-advertisement) did
not spell out.

**Both of the gate's premises were re-verified before anything was written.**
[#293](293-worker-capacity.md) reads `Status: IMPLEMENTED — shipped in jobs
295–301`, and its job 3 — the dispatcher-side observation ingest this slice had
to land *after* — is commit `3cfcfab` (`job/297: code`) in this history. So
the "do not race #293" hazard §4's [Sequencing](#sequencing-with-293) names is
spent, not merely believed to be.

**The mode enum is `types::job_type::RuntimeMode`, not a new `ExecMode`.** §4's
struct sketch names `ExecMode` and the tree had no such type; it had *two*
enums over exactly `{container, host}` already — `RuntimeMode` (in `types`,
serde `lowercase`, what a job type's `runtime.mode` declares) and
`WorkerMode` (worker-crate-local, what `WORKER_MODES` parses into, never on the
wire). Adding a third would have made slice 6's predicate a mapping between two
vocabularies for one concept, which is the shape docs/reference/style.md's
duplication rule exists to prevent — and the comparison slice 6 performs is
*literally* "does this node serve the mode this job type declared", so the
declaration's own type is the honest one. `WorkerMode` was left where it is:
promoting it to `types` would drag env-var parsing into a pure-data crate, and
the node maps it to `RuntimeMode` once, in `node_capabilities`
(`crates/worker/src/daemon.rs`).

**`resources_enforced` is derived, not declared.** A node reports it as "do I
serve `container`", because the Docker `HostConfig` is what enforces
`cpu`/`memory` and a host-only node has none. That means a **dual-mode** node
reports `true` while being unable to enforce limits on the host half of its
work — the honest per-mode answer needs [§7](#7-resource-limits)'s predicate,
which is P3. Recorded here rather than guessed at, because the field is
advertised now and read by nothing.

**`leases` is on the wire and always empty.** [§5b](#5b-exclusive-resources-device-leases)
is P4; the field ships with the record because the absent-defaults table
already commits to its reading (`[]`), and a node that acquires one later needs
no wire change to say so.

**What a node running the previous binary reports.** Nothing — its `PingOk` and
`WorkerAnnounce` carry no `capabilities` key at all, both fields being
`Option` + `skip_serializing_if`, so `WORKER_RPC_VERSION` stays at 2 and the
dispatcher decodes the payload unchanged. It then reads as
`NodeCapabilities::absent()`: `modes: [container]`, `platform: "unknown"`,
`resources_enforced: true`, `leases: []` — the whole fleet's reading through the
entire rollout window, and every existing placement decision unchanged because
nothing consults the record yet.

**Ping-wins is implemented as "an announce applies only while no ping has ever
answered for the node"**, in `types::ObservedCapabilities::apply`. Stronger than
last-writer-wins and deliberately so: once the pull transport has spoken, a
stale or malicious announce cannot reclassify the node, and a node that genuinely
changes its `WORKER_MODES` is corrected at the next placement probe — which is
the self-healing property §4 argues the pull path buys.

---

## Note, 2026-08-07 — slice 6 landed, host work is routable (job #484)

Appended by the job that implemented P2 slice 6, which completes P2. Nothing
above is edited beyond the status rows [§5a](#5a-capability-aware-placement)
claims; this is the record of the decisions it had to make.

**The required mode is read off `image`, not threaded as a resolved mode.**
`ContainerLaunchConfig::required_mode()` (`crates/container/src/lib.rs`) answers
`container` for a launch carrying an image and `host` for one carrying none —
the *same* selector [P1's per-launch
routing](#p1-as-landed-2026-08-07--per-launch-mode-routing-job-479) made every
backend route on. The alternative — threading `JobType::resolved_mode()` down
from the dispatcher — was rejected because it creates a second answer to one
question: a job type is `mode: host` while its `ci` evaluator level carries an
explicit `image` and is container work, so the job type's mode is *not* the
launch's mode, and a placement that believed it would route a container
evaluator onto a host-only node. `resolved_mode()` stays where it is, deciding
the field rules; the launch's own image decides where it goes. One selector,
so placement and the node's own refusal cannot disagree — which is what makes
`HostBackend`/`DockerBackend`'s wrong-mode refusals unreachable rather than
merely unlikely.

**The postcondition, which the slice table names as this slice's contract.**
`choose_placement(policy, candidates, pin, required)` is still pure — no clock,
no I/O, no logging — and now returns, for a non-empty candidate set:

- **Pinned, unknown name** → `Launch` naming the known nodes (unchanged).
- **Pinned, out of service** → `NoCapacity("no free slots on node {n}")`
  (unchanged). Checked *before* capability, deliberately: a node that has never
  answered a probe advertises nothing and reads container-only, so a hard
  failure there would condemn a host node that is merely still booting.
- **Pinned, in service, does not serve the mode** → `Launch` naming the node,
  what it serves and what was required — **whether or not it has a free slot**,
  so this is the one case the capability verdict is reached *before* the full
  check. §5b's "a pin routes but does not exclude" cuts both ways: the pin is a
  routing statement, not an exemption, and the node's own backend would refuse
  the launch by name anyway. It is `Launch` rather than `NoCapacity` for the
  reason [tenancy](#are-host-nodes-single-tenant) gives for
  `WORKER_HOST_PROJECTS`: only a config change clears it, so queueing it is a
  30-minute silence with a known answer — and a *full* incapable node has
  already given its live answer, so waiting out the slot only arrives at this
  same error later.
- **Pinned, in service, serves the mode, full** → `NoCapacity("no free slots on
  node {n}")` (unchanged).
- **Unpinned, no candidate serves the mode** → `NoCapacity("no node advertises
  {mode} mode: …")`, listing what each node does serve.
- **Unpinned, otherwise** → the busyness/headroom winner among the candidates
  that serve the mode; `NoCapacity("no free slots on any node")` when none of
  *those* is eligible. The mode filter runs before the `free <= 0` and
  out-of-service checks, so the two diagnoses never collapse into one.

An **empty** candidate set keeps the "no free slots on any node" answer rather
than the capability one: zero nodes is spec §3.1's zero-seed boot, a fleet that
has not registered yet, not one that refuses the mode.

**A container-only fleet is bit-for-bit unchanged.** Every node reads
`modes: [container]` — advertised, absent or synthesized — and every launch in
the tree carries an image, so the predicate admits every candidate it used to
and both messages are the ones that shipped. The tier-1 tests that pinned the
two policies and the ticket #60 rule now pass `RuntimeMode::Container` and
assert the same choices.

**The fleet-wide warning follows [#293](293-worker-capacity.md) §8's shape, not
a new one.** `container::ModeWarnings` is a pure `mode_warning_due` predicate
plus one cell of cadence state, bounded to one line per mode per fifteen
minutes, held by the placing backend and fired from `place()` — not from
`choose_placement`, which stays pure. It is placement-triggered rather than
scan-triggered because the dispatcher has no fleet-wide list of the modes its
job types require: it learns a mode is *wanted* only when a launch requiring it
arrives, and that is exactly the moment the finding becomes actionable.

**Nothing was flipped to host mode.** No node's `WORKER_MODES` and no job type
in `.chug/jobs/` changed, so the tree's behaviour is identical today; what
changed is that declaring either is now sufficient. End to end, a job type
declaring `mode: host` on a fleet where exactly one node advertises `host`
places every host launch on that node without a pin, queues them behind each
other at its one slot, and sends the same job type's container `ci` evaluator
wherever the policy prefers.

## Correction, 2026-08-08 — the precedence rule fires only across a mode boundary (job #507)

[Coexistence](#coexistence-on-a-mixed-fleet) states the rule unconditionally —
*"an explicit `image` at a level resolves that level to container mode and does
not inherit `runtime`"* — and the body two paragraphs on already contradicts it,
calling `wrap_up.image.or(job_type.image)` "the container-mode half of the same
rule, unchanged". Job #507, which implemented the launch half, resolved that
against the narrower reading: **a level's own `image` costs it the job type's
`runtime.env` only when that image resolves the level out of the job type's
mode.** Under `mode: host` it does, and the appended `ci` evaluator gets no
`xcode:` reference it could not reach anyway. Under `mode: container` it does
not, and an `image` and an `env` go on layering.

The unconditional reading would have been a regression rather than a stricter
rule. [#373](373-project-toolchains.md) Decision 2 has the two **layer** in
container mode — a declared `runtime.env` is a nix environment realised on the
node and bind-mounted into whatever image the level names — so dropping the
environment from every level carrying its own image would have silently taken
the toolchain away from the `ci` evaluator of every nix-layered container job
type in the fleet, none of which #507's brief was about. Nothing in this
document ever argued for that; the sentence was written with the host case in
view and generalised one step too far.

Where it lives: `JobType::level_runtime_env` (`crates/types/src/job_type.rs`)
returns the declaration when `level_mode(level)` equals the job type's resolved
mode and nothing otherwise, which is the boundary condition stated as code. The
normative form is `docs/spec.md` §1.1's `mode: host` row, which was already
scoped to that mode and needed no correction; this section exists because the
prose above it was not.

## Amendment — 2026-08-09, job #517 (the docker-socket rule inverts)

**[§10](#10-trust-and-tenancy)'s first clause is false, and the operator has
inverted the rule rather than fixed the node.** The full record — the
measurement, the argument, the cost and the revisit trigger — is
[#517](517-docker-access-for-jobs.md); this section says what changed here and
why, and does not restate it.

**What was measured.** Job #516 ran a read-only probe as a host task on
`gumbo-air-0`: `docker info` and `docker ps` both exited 0 against a colima
daemon, `DOCKER_HOST` was unset, and the active docker context named a socket
mode `0600` owned by the login user — the user host tasks run as. There is no
`/var/run/docker.sock` on that node and no `docker` group.

**Nothing granted it, and this section's own control works.**
`crates/container/src/host.rs` composes a launch environment rather than
inheriting one, and its test
`a_host_task_inherits_nothing_the_dispatcher_did_not_declare` genuinely holds —
`DOCKER_HOST` never reaches the task. The CLI does not need it: the floor
carried from the daemon includes `HOME`, the docker context resolves from
`~/.docker/config.json` under it, and the socket is reachable by **file
ownership** rather than group membership. An environment guarantee bounds what a
task is told; it says nothing about what the task's uid may open, and §10 was
written as though the first answered the second.

**What changes in this section, precisely:**

| Clause | After #517 |
| --- | --- |
| "host tasks do not get the docker socket" | **Inverted.** Jobs may use docker; #517 D1 records the reasoning, the accepted cost and the condition under which it stops holding |
| "a node-side allow-list entry" | **Kept**, and extended to container launches (#517 D3) |
| "never a job-type field the platform honors on request" | **Kept, unweakened.** A node-side entry is the node consenting to a name the project chose; a job-type field would let a merge grant node root |
| The blast-radius table's docker row | **Kept as analysis.** Its facts are unchanged and the escalation it describes is accepted, not mitigated |

**And one claim in [§10's tenancy subsection](#are-host-nodes-single-tenant) is
unbuilt.** `WORKER_HOST_PROJECTS` — "enforced at the node" — appears in this
document, in #313, #322, #355 and #367, and in no source file, deploy script or
nix module; `crates/worker/src/config.rs` parses `WORKER_MODES`,
`WORKER_HOST_ROOT`, `WORKER_KVM_PROJECTS` and `WORKER_NIX_PROJECTS` and no
host-projects list. Host single-tenancy today is `placement.node` plus the fact
that one node serves `host` at all. The policy stands as designed; the sentence
claiming enforcement was ahead of the tree, and #517's acceptance should be read
against what the tree enforces rather than what this section asserted.

## Note, 2026-08-09 — §10's tenancy list is built (job #525)

**`WORKER_HOST_PROJECTS` exists, and
[§10's](#are-host-nodes-single-tenant) "enforced at the node" is now true.**
The [2026-08-09 amendment](#amendment--2026-08-09-job-517-the-docker-socket-rule-inverts)
above recorded that it was not — that five designs reasoned from a variable no
source file, deploy script or nix module held. That record **stands**: it was
accurate when written, and this note closes it rather than deleting it. What
changed is the tree, not the finding.

What landed, exactly:

- **`container::host::HostTenancy`** is the single decision site. A host launch
  whose `JOB_PROJECT` the node's list does not name is a `BackendError::Launch`
  naming the project and the node — **never** `NoCapacity`, for the reason §10
  gives and the message carries. It matches on the stamp
  [#517 S1](517-docker-access-for-jobs.md) sealed, so project config cannot move
  the key the tenancy is read against.
- **Host launches only.** The list is read in `HostBackend::admit` and nowhere
  else, so a mixed-mode node's **container** launches are byte-identical — which
  is the distinction [#313](313-workload-identity-image-builds.md) drew and this
  slice keeps.
- **Unset runs host work for nobody**, matching `WORKER_KVM_PROJECTS` and §8's
  rule that a node does not advertise `host` unless provisioned for it. The
  alternative — unset ⇒ unrestricted — would have made the control opt-in, which
  is the shape §10 was complaining about in the first place.
- **The daemon warns; the deploy refuses.** An empty list is a working
  configuration to the daemon: it logs the warning and refuses each host launch,
  rather than refusing to boot and taking a mixed-mode node's container work down
  with it. `deploy/prod/build-worker.sh` therefore refuses a deploy declaring
  `host` with no tenancy beside it, in the shape it already refuses
  `WORKER_MODES=host` without `WORKER_SLOTS=1 && WORKER_SLOTS_MAX=1`.

**This is a real cutover, and it is the operator's.** `gumbo-air-0` serves host
work today and declares no list, so its next deploy refuses until one is
declared. Nothing in this repo declares it — no node config was edited here, by
design — and [`docs/reference/runbooks/worker-host-projects.md`](../reference/runbooks/worker-host-projects.md)
is the procedure.

**Out of scope and unchanged:** §8's per-task users (structurally blocked on
macOS by the daemon's GUI-domain requirement, [#490](490-agent-work-on-a-mac.md)),
§6's transient scopes, and the docker grant — [#517 D3](517-docker-access-for-jobs.md)
landed that, and [#517 D1](517-docker-access-for-jobs.md) accepted the escalation
this list is the containment for.

## Amendment — 2026-08-09, job #526 (§8's recommendation is unavailable on macOS, and §10's tenancy list is now the only bound)

**The operator has decided host-node tenancy on the one host node that exists,
and it is not what [§8](#8-secrets-on-a-shared-host) recommends.** The full
record — the decision, the accepted cost, each bound read out of the tree, and
the partial measurement taken against the revisit condition — is
[#322's 2026-08-09 correction](322-macos-native-runtime.md#correction--2026-08-09-job-526-host-tasks-run-as-the-login-user-the-secret-boundary-is-absent-and-what-bounds-it-is-thinner-than-5-says).
This section says what changes here and does not restate it.

**What was decided.** Host tasks on macOS run as the node's **existing login
user** (`worksalot` on `gumbo-air-0`). Per-task users are **deferred, not
rejected**.

**§8 is not reworded, and its recommendation is not superseded — it is
unavailable.** Option (b)'s mechanism is `systemd-run --uid=` against a
pre-created pool, and [#322 §5](322-macos-native-runtime.md#5-ios-specifics)
already records the three macOS collisions that leave no equivalent:
CoreSimulator is a per-user-session service, an unlocked signing keychain is a
session property, and `launchctl asuser` and a per-user launchd domain both need
a session a provisioned user does not have. [#490](490-agent-work-on-a-mac.md)'s
M5 fork adds the one that settles it on the tree: the macOS worker daemon is a
launchd agent in the **login user's GUI domain**
([#440](440-native-worker-daemon.md) D2), which is how the native conversion
works at all. So macOS runs §8's option **(a)** — accept and document — which §8
rejected as a default, and #322's correction is where the documenting happens.

| Clause | After job #526 |
| --- | --- |
| "(b) per-task unix user (recommended)" | **Kept for a Linux host node, unweakened.** Unavailable on macOS, where the node runs option (a) |
| "the daemon does not advertise `host` unless the user pool is provisioned" | **Not in force.** `gumbo-air-0` advertises `host` with no pool, because on macOS there is no pool to provision. The rule is Linux's and has never had a node to bind |
| "(a) accept and document … rejected as the default" | **Is** the macOS default now, by decision rather than by drift |
| §8's residual-risk paragraph | **Kept and widened.** `remove` scrubbing the task dir is real (`crates/container/src/host.rs`) and bounds only what is *in* the task dir |

**There is no Linux host node.** [`.chug/jobs/android-proof.yaml`](../../.chug/jobs/android-proof.yaml)
declares `image: chuggernaut/agent:prod` and runs in a **container** with
`/dev/kvm`, and [`.chug/jobs/mac-proof.yaml`](../../.chug/jobs/mac-proof.yaml)
is the only job type declaring `mode: host`. The one category that forced host
mode is the Mac, so P3's per-task-user work has, so far, no platform to land on.

**And [§10's tenancy subsection](#are-host-nodes-single-tenant) carries more
weight than it did.** §8's per-task user boundary was the thing that would have
handled *accidental* cross-reading, with §10's policy layered on top for the
hostile case. With §8 unavailable, **§10's list is the whole of it** — which is
what makes the [note above](#note-2026-08-09--10s-tenancy-list-is-built-job-525)
matter here: job #525 merged the same day this was written, so
`WORKER_HOST_PROJECTS` is now real, enforced in `container::host::HostTenancy` at
`HostBackend::admit` and fail-closed when unset. **One step of it is still the
operator's**: `gumbo-air-0` declares no list at the time of writing, so what
enforces its single-tenancy until the next deploy is `placement.node`, the fact
that one node serves `host` at all, and `enforce_host_capacity`
(`crates/worker/src/daemon.rs`) refusing to boot a host-capable node at anything
but one slot — and the declaration itself is the last bound this decision leans
on, not a tidying step.

## Note, 2026-08-09 — §7's predicate and backstop landed (job #524)

Appended by the job that implemented [§7](#7-resource-limits) option 3's
**consuming** half. `NodeCapabilities.resources_enforced` had shipped with P2's
capability slice and had **no reader** anywhere: a job type declaring
`resources.cpu`/`memory` was placed exactly as if the field did not exist. It
now has two, and nothing above is edited beyond the P3 status row.

**The predicate reuses [§5a](#5a-capability-aware-placement)'s filter rather
than adding a second path.** `choose_placement` takes a `LaunchRequirements {
mode, resource_limits }` in place of the bare `RuntimeMode`, derived in one
place — `ContainerLaunchConfig::requirements()`, beside the `required_mode()`
the same config already answered — so the placement filter and a backend's own
refusal read one launch the same way. Unpinned, an unmeetable requirement is
`NoCapacity` and therefore an ordinary §3.5 queue entry with its reason
attached; pinned, it is `Launch`, for the reason the mode pin is: a pin never
falls back, so no amount of waiting clears it.

**The dual-mode question is answered by narrowing the *reading*, not the
advertisement.** `resources_enforced: serves_container(modes)` means
`gumbo-air-0` — `WORKER_MODES=container,host` — advertises `true` while every
host task it runs is bounded by nothing, which is [option 1's silent
lie](#7-resource-limits) arrived at from the other side. The two available
fixes were to make the requirement mode-aware or to make a dual-mode node
advertise `false`; **this slice takes the first**, and the second is rejected on
measurement rather than taste: `gumbo-air-0` serves container work today, and
**ten** of the repo's twelve job types declare `cpu`/`memory`, so an
advertisement narrowed to container-only nodes would strike a live node out of
the candidate set for ten job types — a fleet change wearing a bug fix's
clothes. Enforceability is a property of **the launch's resolved mode**, the
same shape [#507](#correction-2026-08-08--the-precedence-rule-fires-only-across-a-mode-boundary-job-507)
found for `runtime.env` and [#517](517-docker-access-for-jobs.md) D4 for docker
access, and it lives in `PlacementCandidate::bounds`: the advertisement answers
for the container runtime that would enforce it, and never for the host half.

**The backstop replaced a warning that was already the rejected option.**
`HostBackend::admit` logged a `tracing::warn!` naming the declared limits and
ran the task anyway — precisely the "advisory elsewhere" §7 rejected. It is now
`BackendError::Launch`, sharing one message builder with the pinned placement
refusal so the two cannot drift, naming the field and the node, and refusing
before any task directory is created.

**`task_timeout` is untouched, and now says so in the spec.** It never reaches a
`ContainerLaunchConfig` at all — the §3.5 scan enforces it dispatcher-side — so
it is mode-independent and constrains placement not at all. `docs/spec.md`'s
`resources:` block now carries that distinction, since it is the difference
between a bound a host job type may declare and two it may not.

**What the tree measured, against §7's own claim.** §7 says the predicate is "a
no-op for the container fleet and for every job type in `.chug/jobs/` today".
True, and by a wider margin than the phrasing suggests, but the count in the
brief that commissioned this slice (five job types) is wrong: **ten** declare
`cpu`/`memory` — `android-proof`, `code`, `coverage`, `deploy`, `design`,
`docs`, `gcp-proof`, `rollback`, `web`, `web-publish` — and every one is
container work on nodes that all read `resources_enforced: true`.
`mac-proof.yaml`, the one host job type, declares `task_timeout` **only**, which
is exactly what §7's corollary asks of it. `crates/test-utils/tests/placement_guard.rs`
is that measurement as a test: every level of every job type in the tree is
placed twice — once carrying the limits it declares, once carrying none — and
the answers must match.

**One thing this slice found and did not fix.** `resources.cpu`/`memory` reach
only **command** launches: `ClaudeProvider::run` (`crates/agent/src/claude.rs`)
builds its `ContainerLaunchConfig` with `cpu_limit: None, memory_limit: None`,
so an agent work task has never been bounded by either field on any node,
whatever its job type declares. That is a pre-existing gap in the *enforcement*
of §7's fields rather than in this slice's predicate — the predicate is correct
either way, and simply never fires for an agent launch — but it means a
`code.yaml` work container has been running unbounded on the container fleet
since those fields were written. Not touched here: closing it changes what real
jobs are allowed to consume, which is a fleet-capacity decision and wants its
own ticket.

**Not in scope, and still Proposed:** [§6](#6-drain)'s transient scopes (landed
separately as [#440](440-native-worker-daemon.md) D3 for supervision, not for
limits) and [§8](#8-secrets-on-a-shared-host)'s per-task users, which is
structurally blocked on macOS by the daemon's GUI-domain requirement
([#490](490-agent-work-on-a-mac.md)). P3 is therefore *partly* landed: its
`resources_enforced` third is in, and nothing bounds a host task's cpu or memory
on any platform — a host job type must still declare neither, which the platform
now says out loud instead of implying.
