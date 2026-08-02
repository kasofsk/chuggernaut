# Design #367 — Android emulator execution: a container with `/dev/kvm`, not a host runtime

Status: PROPOSED.

Written against the tree at `1e567e3`. Every claim about this repository was
read out of the source or out of [`spec.md`](../../spec.md), not inferred from a
sibling design; where the brief and the tree disagree, the tree wins and the
disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree). The four external claims
the brief supplies were fetched and are quoted where used. One class of claim —
anything about the **beacon** repository — is **not verifiable from this tree**:
`~/beacon` is not present in this container, and every such claim below is
marked *(secondhand)*.

**What this document is for.** [#308](./308-gha-port.md) category F treats
"mobile" as one thing that needs host-native execution, and
[#309](./309-host-native-execution.md) §H and
[#322](./322-macos-native-runtime.md) build that route. #322 is explicitly
macOS-only. **No document in this tree designs the Android leg at all**, and
`/dev/kvm` appears in the whole corpus exactly three times — twice in #308
(lines 690 and 801) as a passing machine-fact, once in #309 (line 540) as an
example of a physical fact. This document fills that hole and argues that the
two legs of category F are not symmetric and should stop being sequenced as if
they were.

**Scope.** This does **not** redesign iOS — [#322](./322-macos-native-runtime.md)
owns the macOS instantiation, and where a primitive is shared this document says
so and defers to it. It does **not** re-litigate whether host-native execution
should exist; #309 and #322 decided that, and the recommendation below leaves
both intact. It changes nothing about `runtime:`, `CONFIG_SCHEMA_EPOCH`, or the
host backend.

Related: [`spec.md`](../../spec.md) §3.1 (backends, placement, worker RPC,
node-local build caching, "no host bind-mounts"), §3.5 (launch capacity queue),
§14.1 (config/version skew); [#308](./308-gha-port.md) §F, §G, H.2, H.4, H.5,
H.6 and the gap table; [#309](./309-host-native-execution.md) §4, §5a, §5b, §9,
§10, Phasing; [#322](./322-macos-native-runtime.md);
[#313](./313-workload-identity-image-builds.md) B-IV;
[#355](./355-project-task-images.md); [#361](./361-per-run-placement.md);
[#362](./362-binary-artifacts.md); [`STYLE.md`](../../STYLE.md);
[`crates.md`](../../crates.md); [`testing.md`](../../testing.md).

---

## Corrections (verified against the tree)

The brief is right about the thing that carries its argument — Android is a
device-passthrough problem, not a host-execution problem. Seven claims needed
adjusting, and five of them move work.

1. **#322 W1 has landed. The brief understates its own case.** It says the
   Android route needs neither `HostBackend` nor the `runtime:` selector nor the
   epoch bump, and implies #309 P0 / #322 W1 is still ahead. It is not:
   `crates/worker/src/daemon.rs` already holds
   `Arc<dyn ContainerBackend>` behind a single `build_backend` construction
   site that owns the `with_cache_dir` / `ping_all` wiring;
   `managed_running_total` is a **provided** trait method
   (`crates/container/src/lib.rs`); `WORKER_MODES` parses in
   `crates/worker/src/config.rs` (`parse_modes`, `WorkerMode::{Container,
   Host}`); and a node declaring a mode this build cannot serve refuses to start
   by name (`crates/worker/tests/nats_backend.rs`,
   `declared_mode_without_a_backend_refuses_to_start`). What remains on the host
   line is **W2 and beyond** — the host backend, the durable task registry, the
   `/workspace` rebase. Android needs none of it.

2. **`NodeCapabilities` does not exist anywhere in the code.** `grep -rn
   "NodeCapabilities" crates/` returns nothing; the only `capabilities` hit in
   the whole crate tree is an MCP protocol field in
   `crates/chuggernaut-channel/src/server.rs`. It is entirely a design-doc
   construct (#309 §4, cited forward by #322 P1, #355 §6 and #361). **This makes
   the `platform` contradiction cheap to settle**: it is a disagreement between
   two paragraphs of one unimplemented design, not between a design and a
   shipped wire field. Resolving it is a docs edit with no migration.

3. **The launch path already models devices; only the plumbing is missing.**
   `ContainerLaunchConfig` (`crates/container/src/lib.rs`) has exactly `image,
   cmd, env, files, cpu_limit, memory_limit, node` — the brief is right that
   there is no device field. But `bollard` 0.19's `HostConfig` already carries
   `devices`, `device_cgroup_rules` and `device_requests`, with `DeviceMapping {
   path_on_host, path_in_container, cgroup_permissions }` (read out of the
   prebuilt `libbollard-*.rmeta` under `/opt/chug-prebuilt-target`; the crate
   source is not vendored in this container, so the field *names* are verified
   and their exact optionality is not). So the Docker-side change is populating
   one existing struct field in `build_host_config`
   (`crates/container/src/docker.rs`), not a new API surface.

4. **`WorkerLaunchRequest` carries six fields, not seven** — `image, cmd, env,
   files, cpu_limit, memory_limit` (`crates/types/src/worker.rs`). There is no
   `node`: the node is the NATS subject (`req.worker.{node}.{op}`). Worth
   stating because it is the reason a node-side mechanism costs the wire
   *nothing*: the worker already knows which node it is.

5. **The job-type name never reaches the worker, and this constrains the
   allow-list shape.** The launch env is exactly `JOB_ID`, `JOB_PROJECT`,
   `JOB_BRANCH`, `BASE_BRANCH`, `REPO_URL`, `NATS_URL`, `CHANNEL_ROLE`,
   `JOB_TASK_ID`, `CHUG_TASK_ID`, `CHUG_PHASE`, `CHUG_EVALUATOR` plus secrets
   and git/NATS wiring (`Core::container_env`, `crates/dispatcher/src/exec.rs`).
   [#313](./313-workload-identity-image-builds.md) B-IV proposes a node-side
   allow-list of an **"(project, job type)"** pair; the job type half is not
   observable node-side today. A node-side allow-list can be keyed on
   `JOB_PROJECT` and on the requested `image`, and nothing finer, without a wire
   change. This is a finding for #313 as much as for this document.

6. **"nuc is `slots: 2` today" is no longer a redeploy-shaped fact.** #293 has
   landed in the wire types and the API: `set_node_slots` is the sixth provided
   trait method (`crates/container/src/lib.rs`), `SetSlotsRequest`/`SetSlotsOk`
   are on the wire (`crates/types/src/worker.rs`), and
   `platform_fleet_capacity_set` in `crates/api/src/routes.rs` gates it behind
   `platform_admin`. The live slot count of any particular node is runtime state
   this repo does not record, so *(secondhand)* stands for the number — but the
   consequence is verified and it matters: **"pin the device-bound type to a
   1-slot node" is now one authenticated API call**, not a `WORKER_SLOTS`
   redeploy. That materially strengthens the cheap interim #309 §5b endorses,
   and #309's own correction 4 ("the only way to change a node's slot count
   today is to restart its daemon") is stale.

7. **#308 H.4's AVD claim is wrong, and this document records the correction
   without being able to verify it** *(secondhand)*. #308 says the AVD
   "persists across runs and is created only if absent, so in a container that
   check fails every night, so it re-downloads the system image forever." The
   operator's inspection of beacon's flutter-integration-tests workflow
   (2026-08-01) reports the opposite in a comment — `ANDROID_USER_HOME` is
   per-workspace, so each run starts with no AVD configured and creates one on
   the fly with `avdmanager create avd --force`, which is fast and downloads
   nothing because the *system-image SDK package* is already installed on the
   runner. What persists is the **installed SDK**, not the AVD; the `adb
   devices` reuse check is an optimization for a shared host, not a requirement.
   This removes the principal "Android is host-state-dependent" argument, and
   §[3](#3-part-two-the-toolchain-bulk) is built on it. **If it is wrong, the
   toolchain half of this design is wrong and the device half is not** — they
   are independent, which is the point of separating them.

One more thing the brief flags that is worth pinning: `crates/worker/src/config.rs`
line 375 contains the string `"kvm"`. It is a *rejected `WORKER_MODES` value* in
a parse test, not a device reference. A future `grep -rn kvm` will hit it; it
means nothing.

---

## 1. The question, and the hypothesis under test

> **Do Android emulator jobs need host-native execution, or are they an ordinary
> Linux container with `/dev/kvm` passed through?**

The hypothesis the brief asks to test first:

> Android is (a) `/dev/kvm` device passthrough into an ordinary Linux container
> and (b) a node-local Android SDK, mounted the way sccache already is. Neither
> needs the host backend, the `runtime:` selector, or the epoch bump.

**Verdict: confirmed, and more strongly than stated.** Containerization does not
merely avoid the host backend — it *dissolves* the exclusivity primitive #308
H.5 introduced on Android's behalf (§[4](#4-exclusivity-and-why-android-does-not-need-a-lease)).
The one thing Android does share with iOS is capability-aware placement, and
only once the fleet has a second KVM node (§[5](#5-the-platform-contradiction-and-how-a-node-advertises-kvm)).

The claim is narrow and worth stating in its negative form too. This does not
say host-native execution is unnecessary — iOS genuinely needs it, for the
reason #308 §F gives and no container cleverness touches: `xcrun simctl` needs a
macOS host. It says **the two legs of category F have different shapes and the
Android one is cheaper by roughly an order of magnitude**, and the corpus
currently prices them as one.

### What #308 got wrong, precisely

Design #308 H.2 lists **"`/dev/kvm` for the Android emulator"** as one of the
things *host mode buys*. That is the error, and it is a single-bullet error with a
large consequence: `/dev/kvm` is available to an ordinary container. Google's own
`android-emulator-container-scripts` documents the launch as

```
docker run -e ADBKEY="$(cat ~/.android/adbkey)" --device /dev/kvm \
  --publish 8554:8554/tcp --publish 5555:5555/tcp <image>
```

— `--device /dev/kvm`, **not** `--privileged` (fetched 2026-08-01; the docs
state "KVM must be enabled on your host … bare-metal Linux or inside cloud
Virtual Machines with nested virtualization enabled" and that "Docker Desktop on
macOS and Windows is not supported for KVM acceleration"). That distinction is
load-bearing for a fleet that is deliberately bind-mount-free: a single named
device node is a far narrower grant than `--privileged`, and it is
representable in the launch path the platform already has.

The industry moved the same way. GitHub's 2024-04-02 changelog announces
hardware-accelerated Android virtualization on **Linux** runners (extending it
to 2-vCPU runners; Linux is the only OS named), and
`ReactiveCircus/android-emulator-runner` now reads "It is now recommended to use
the **Ubuntu** (`ubuntu-latest`) runners which are 2-3 times faster than the
**macOS** ones which are also a lot more expensive." Both fetched 2026-08-01.
*(secondhand)* beacon's own Android leg reportedly already runs
`runs-on: ["self-hosted","linux","x64","gumbo"]` with `ubuntu-latest` as the
cloud fallback; only the iOS leg is macOS.

So the honest reading of #308 §F is: **"impossible under containers — not hard,
impossible"** is true of Xcode and false of the Android emulator. #308's own
sentence names Xcode and `xcrun simctl` and then generalizes to "mobile". The
generalization is the bug.

---

## 2. Part one: the device

Design #308 conflates two problems that have nothing to do with each other, and
the whole design follows from separating them:

| | Problem | Nature | Settled by |
| --- | --- | --- | --- |
| **1** | **Device access** — the container needs `/dev/kvm` | Narrow, named, one struct field | Evidence above; §[2](#2-part-one-the-device) |
| **2** | **Toolchain bulk** — SDK + NDK + system images, many GB | Storage and provisioning; nothing to do with devices | §[3](#3-part-two-the-toolchain-bulk) |

### 2.1 Is `/dev/kvm` the same class as the docker socket?

[#309 §10](./309-host-native-execution.md#10-trust-and-tenancy) sets the rule the
brief asks about: a job type that needs a docker socket "is a node-side
allow-list entry, never a job-type field the platform honors on request", and
[#313](./313-workload-identity-image-builds.md) B-IV's recommendation is
explicitly praised for "resolv[ing] the contradiction by *complying* with the
rule rather than carving it out."

**Same mechanism, different rule.** The two are worth distinguishing carefully,
because the distinction decides the *end state*, not the first phase.

| | Docker socket | `/dev/kvm` |
| --- | --- | --- |
| What it grants | `POST /containers/create` with a bind ⇒ **root on the node**, and read access to every other project's containers (#313 correction 5) | `KVM_CREATE_VM` and friends ⇒ a guest VM the caller already could have run in software, slowly |
| Blast radius on success | the node's whole container fleet, i.e. the platform's own execution substrate | the container's own process tree, plus whatever a guest→host KVM escape reaches |
| Residual risk | structural — the capability *is* root-equivalence | a real kernel CVE class (guest escapes exist), but not a capability grant |
| Should it ever be a job-type field? | **No, permanently.** #309 §10 is right and it is not a phasing statement | **Yes, eventually** — it is a legitimate declarable requirement once more than one node has it |

So `/dev/kvm` is **not** in the docker socket's class, and this document should
not pretend it is in order to borrow the rule's authority. It is in the class of
`resources.cpu`: a physical capacity a job legitimately requires, that the node
legitimately asserts, and that the scheduler legitimately matches on.

The reason to nonetheless start node-side is different and purely economic: with
**one** KVM node in the fleet, `placement.node` already routes the work, a
job-type field buys nothing, and adding one to `Placement` — which carries
`#[serde(deny_unknown_fields)]` (`crates/types/src/job_type.rs`) — forces a
`CONFIG_SCHEMA_EPOCH` bump on its own, exactly as #309 §5b shows for
`placement.leases`. That is the whole cost argument and it is a phasing
argument, not a principle.

The honest cost of that "physical capacity" framing must be stated too:
`/dev/kvm` is not free of security consequence. Guest-to-host escapes through
KVM are a recurring CVE class, and a container holding `/dev/kvm` on a shared
node is a materially larger kernel attack surface than one without it. The
mitigation is the one the fleet already uses for the docker socket — **grant it
narrowly** (§2.3), and keep the KVM node's kernel current as a machine fact
under #308 H.6's "system closure" row.

### 2.2 Options for the device primitive

**D1 — node-side, unconditional: every container on a KVM-enabled node gets the
device.** Exactly the `WORKER_CACHE_DIR` shape: a `WORKER_KVM_DEVICE` (or a
bare boolean) parsed beside `parse_cache_dir`, threaded into `DockerBackend` by
a `with_devices`-style builder beside `with_cache_dir`, and populated in
`build_host_config`.

*For:* Zero wire change, zero schema change, zero dispatcher change, zero epoch
bump. It is the mechanism `spec.md` §3.1 already blesses ("a node property added
worker-side, not a launch input"), and #313 B-IV's "costs the dispatcher
nothing" applies verbatim. One unit test on the produced `HostConfig`, beside
`host_config_with_cache_adds_one_bind`.

*Against:* It widens the kernel attack surface for **every** job on that node,
including ordinary `code` jobs that will never touch an emulator. That is a real
cost and it is the reason not to recommend this one unmodified.

**D2 — node-side, allow-listed (recommended).** As D1, but the device is added
only for launches whose `JOB_PROJECT` (and, optionally, requested `image`)
appears in a node-side allow-list — `WORKER_KVM_PROJECTS`, mirroring #309 §10's
`WORKER_HOST_PROJECTS` and #313 B-IV's per-`(project, …)` proxy binding. Both
keys are already observable node-side (correction 5), and neither costs the wire
anything.

*For:* Same zero-cost profile as D1. Narrows the grant to the project that asked
for it, so an unrelated `code` job on the same node runs exactly as it does
today. Fails closed: an unset allow-list means no container gets the device, so
enabling this on a node is an explicit act.

*Against:* An allow-list is operator-typed config about a project, which is the
shape #309 §4 rejects for *physical facts*. The distinction holds — "does this
node have KVM" is physical and belongs on the node; "may this project use it" is
policy and belongs in operator config — but the two live in the same env var and
a reader should not confuse them. Keep them as **two** settings for that reason:
one that says the device exists, one that says who may have it.

**D3 — a job-type field, `placement.devices: [kvm]` (or `resources.devices`),
carried on `ContainerLaunchConfig` and `WorkerLaunchRequest`.**

*For:* It is the honest end state. The requirement lives in the project repo
where #308 H.6 and `CLAUDE.md`'s per-consumer-forge principle want it; it is
reviewed through the merge gate like every other job-type change; and it
degrades correctly when a second KVM node appears, because it becomes a
placement predicate rather than a pin.

*Against, and this is decisive for phase one:* it costs a
`CONFIG_SCHEMA_EPOCH` bump (`deny_unknown_fields` on the nested block hard-rejects
an N−1 dispatcher, and the §14.2 park is the *correct* behavior for a
constraint whose silent loss is a wrong placement — the same argument #309 §5b
makes for `leases`). It costs two wire records. And with one KVM node it changes
no placement decision that `placement.node` does not already make. Every one of
those costs is real today; every one of the benefits arrives with the second
node.

**D4 — `--privileged`.** Named only to reject it. It is not what Google
documents, it is not what the GHA runners do, and it hands the container the
node. No.

### 2.3 Recommendation

**Take D2 now, with D3 named as the successor and a stated trigger.**

- The node declares the physical fact: `WORKER_KVM=1` (or a device path,
  defaulting to `/dev/kvm`), parsed in `crates/worker/src/config.rs` beside
  `parse_cache_dir`, and **the daemon refuses to start if the setting is on and
  the device node is absent** — the same fail-loud shape `build_backend` already
  uses for an unserviceable `WORKER_MODES` entry. A node advertising a
  capability it cannot serve is the failure #322 W1's test exists to prevent.
- The operator declares the policy: `WORKER_KVM_PROJECTS=owner/project,…`,
  empty ⇒ nobody, checked against the launch's `JOB_PROJECT`.
- `DockerBackend` gains a device list exactly as it has `cache_dir` — a node
  property, `None` on the dispatcher's construction, never on the wire or in
  `ContainerLaunchConfig`. `build_host_config` populates `HostConfig.devices`
  with one `DeviceMapping { path_on_host: "/dev/kvm", path_in_container:
  "/dev/kvm", cgroup_permissions: "rwm" }`.
- **The switch to D3 fires when a second node has KVM.** At that point the pin
  stops expressing the requirement and starts constraining it, and the epoch
  bump buys something. Until then it buys an epoch.

Explicitly *not* recommended: adding a device field to `ContainerLaunchConfig`
without the schema field. A launch-config field with no way for a job type to
set it is a field the dispatcher would populate from something — and the only
somethings available are a node name (which is the pin, already there) or an
input (forbidden by #311 Decision 1, re-affirmed at length by
[#361](./361-per-run-placement.md)).

---

## 3. Part two: the toolchain bulk

The operator's constraint, and it is a requirement rather than a preference:

> **Do not bake the Android SDK / NDK / system images into a task image.**

### 3.1 One premise of that constraint does not survive contact with the tree

The stated reason is "many GB, **pulled per task**." That is not how this fleet
works, and saying so is the honest thing to do before designing around it:

- **There is no pull path at all.** [#355](./355-project-task-images.md) fact 3
  verified it and it still holds: `grep -rn "create_image\|docker pull"
  crates/` finds only a test fixture, and `DockerBackend::launch`
  (`crates/container/src/docker.rs`) calls `create_container` directly with
  `image: Some(config.image.clone())` and no preceding fetch. Images are built
  **on each node** by `deploy/prod/worker-refresh.sh` and consumed locally.
- A container start does not copy the image. It stacks a writable overlay over
  shared read-only layers. A 20 GB image costs 20 GB of node disk **once**, not
  per task.

So the "pulled per task" framing is wrong for this platform. **The constraint
still stands, on two grounds that are real:**

1. **Disk.** `deploy/prod/worker-refresh.sh` already refuses to build below a
   free-disk floor and says so loudly; #355 §7 calls that floor "re-derived four
   times across real incidents." An Android SDK image on the same partition as
   `/nix/store`-free but image-heavy node storage is a live risk, not a
   theoretical one.
2. **Rebuild time on every refresh.** The platform's three images rebuild **on
   every node on every deploy** (`build-worker.sh`, `worker-refresh.sh`). A
   multi-GB Android layer would be paid by every deploy on every node, and
   `agent-rust`'s leg already ran 673s on one node before #352.

Both arguments point the same way as the operator's, so the recommendation is
unchanged — but a design that repeats a false premise is a design a future
reader will over-apply. The correct statement is: *do not put the Android SDK
in an image the platform rebuilds on every deploy.*

### 3.2 Options

**T1 — bake it into a platform image.** Rejected on §3.1's two real grounds.

**T2 — a second named node-local mount, read-only (recommended).**
`WORKER_ANDROID_SDK_DIR` on the node, bind-mounted **read-only** at a fixed
container path (`/opt/android-sdk`), with `ANDROID_SDK_ROOT`/`ANDROID_HOME`
injected worker-side exactly as `inject_cache_env` injects `SCCACHE_DIR`
(`crates/worker/src/daemon.rs`). Provisioned by the operator out of band — an
`sdkmanager` run, or a NixOS closure — which is precisely #308 H.6's "system
closure = machine facts" row.

**T3 — generalize `WORKER_CACHE_DIR` into a list of named node mounts.**
`WORKER_MOUNTS=sccache:/cache/sccache:rw,android-sdk:/opt/android-sdk:ro`, one
mechanism, still node-side.

**T4 — #309 P5 declared caches.** The flake-attribute-derived per-project cache
set. It is the right general answer and it is host-mode machinery: #309 §9
scopes it to `WORKER_HOST_CACHE_ROOT` and derives the set from `runtime.env`,
which container mode does not have.

**T5 — a project task image under [#355](./355-project-task-images.md) with the
SDK baked in.** Under #355's recommended O2 the node *builds* the image locally
and never pulls it, and it is rebuilt on the **project's** clock, not on every
platform deploy — which neutralizes §3.1's second ground entirely.

### 3.3 Recommendation: T2 now, and T5 as the complement rather than the rival

**Take T2**, and note T3 as the refactor to do when a *third* mount appears —
not before. Two mounts do not justify a list; STYLE.md's simplicity-over-
generality principle applies, and #309 §9 is explicit that `WORKER_CACHE_DIR`
"should not be overloaded — it keeps its documented contract for container mode
unchanged."

**Read-only is the load-bearing property, and it is what makes the whole design
work.** `spec.md` §3.1 permits the one cache bind because it "carries **no job
state** — it is a build accelerator only, safe to be empty/cold", and
concurrency is safe because *sccache locks*. An Android SDK satisfies neither
of those the way sccache does — so it needs a different justification, and
read-only is it:

- **Concurrency safety by construction.** Two emulator tasks on one node cannot
  corrupt a mount neither can write. sccache's justification (it locks) does not
  transfer; this one does not need it.
- **It forces the mutable state into the container's own writable layer**, which
  is where correction 7 says beacon already puts it: `ANDROID_USER_HOME` and
  `ANDROID_AVD_HOME` point into the container, the AVD is created per run with
  `avdmanager create avd --force`, and it dies with the overlay. No shared AVD,
  no shared adb server, no reuse check.
- **It is not a cache and should not be called one.** It is a read-only
  toolchain volume. Calling it a cache invites someone to make it writable.

**Cold-start is the honest cost, and it is charged to the operator, not to
`task_timeout`.** #309 §9's "cold-realise cost" analysis applies here and
reaches the same answer: the first-run cost must be moved out of band. A
read-only mount cannot be filled by the first task, which is a feature — it
means the failure mode is "the node does not have the SDK" (loud, at launch)
rather than "the first task of the day takes forty minutes and looks slow"
(#309 §9's exact complaint about a cold `nix develop`). The node must therefore
**fail the launch loudly** when a KVM-and-SDK job lands and the mount is absent,
never fall through to an ambient SDK.

**T5 is complementary, not alternative.** The right split is:

| Layer | Holds | Size | Clock | Mechanism |
| --- | --- | --- | --- | --- |
| Task image | JDK, emulator runtime deps, `git`/`ssh`, the agent CLI | hundreds of MB | project repo (#355) or platform | `image:` |
| Node mount | SDK packages, system images, NDK | many GB | operator, out of band | `WORKER_ANDROID_SDK_DIR` |
| Container overlay | the AVD, `ANDROID_USER_HOME`, gradle output | per task | the task | nothing — it is the overlay |

A slim project image plus a mounted SDK is the right split, and it is the one
design #355 §9 already implies without saying so: `image:` and the node-resolved
environment reference are "the same slot in two modes", and a node mount is
neither — it is a third thing, the machine fact.

---

## 4. Exclusivity, and why Android does not need a lease

[#308 H.5](./308-gha-port.md) introduces the device-lease primitive on Android's
behalf: "a 2-slot host node will happily run two tasks that collide on
`beacon-emu`", and the workflow's `concurrency` group with
`cancel-in-progress: false` exists "because two runs fight over the same
simulator and AVD." #309 §5b designs the lease; #361 verifies it and amends its
reasoning.

**In container mode the collision does not exist.** Walk the shared resources:

| Shared thing | Container mode | Needs a lease? |
| --- | --- | --- |
| The AVD (`beacon-emu`) | per-container `ANDROID_USER_HOME`, created per run, dies with the overlay (correction 7) | **No** |
| The adb server / ports 5554-5555 | container-local: `build_host_config` sets `nano_cpus`, `memory`, `binds` and nothing else, so Docker's default bridge network namespace applies and each container has its own | **No** |
| The SDK / system images | read-only mount (§3.3) | **No** |
| `/dev/kvm` | KVM multiplexes VMs by design; a host runs many guests | **No** |
| CPU and RAM | `resources.cpu` / `resources.memory`, enforced today via `nano_cpus`/`memory` | **No — this is the existing mechanism** |

So the emulator's real constraint is **memory**, and memory is a declared
resource the platform already enforces. The 1-slot pin #308 H.5 offers as a
"cheap interim" is not needed as an exclusivity device at all; where it is still
wanted it is a *capacity* decision, and per correction 6 it is now one
`platform_admin` API call rather than a redeploy.

**This is a finding for #309 §5b and #308 H.5, and it should be recorded there.**
Not that the lease primitive is wrong — [#322 §5](./322-macos-native-runtime.md)
gives it a genuine motivating case (`xcrun simctl` mutates machine-global state,
`xcode-select` more so, and #322 recommends `slots: 1` on a Mac until per-task
device sets are proven) — but that **half its stated motivation evaporates.**
Design #309 §5b's own trigger condition ("when the host node must run a second,
non-device-bound task concurrently") is a host-mode condition, and Android in a
container never reaches it. The gap table's row 9, *"Node-level exclusive
resources — only bites once host nodes run device-bound work (H.5)"*, is right
in its qualifier and wrong in citing H.5's Android example.

**The one thing that could force a lease after all, stated so it is not a
surprise.** If the emulator turns out to need write access into the SDK
directory — license acceptance files, `sdkmanager` temp state, a
an adb key the SDK tree expects — then the mount cannot be read-only,
the concurrency argument above loses its by-construction property, and an SDK
tree satisfies neither of `spec.md` §3.1's justifications for the sccache bind.
The answer then is *not* a lease; it is a per-container copy-up of the small
mutable subset, with the bulk staying read-only. A lease would be the third
answer and the worst one. This is the first thing to measure in phase A1.

Two more empirical risks, from #308 H.4 and worth carrying forward rather than
re-discovering: `-gpu swangle` and `-no-snapshot` are fixes for a documented
emulator SIGSEGV about eleven minutes in, and #308 §F is right that these are
"adaptations to *GHA's* runner environment, not portable facts." Expect to
re-derive them against a container on the fleet's own kernel. If two concurrent
emulators prove unstable for reasons that are not memory, the answer is still
the capacity dial, not a new primitive.

---

## 5. The `platform` contradiction, and how a node advertises KVM

The brief is right that #309 §4 says two incompatible things about
`NodeCapabilities.platform`:

- the struct's doc comment: *"informational for placement diagnostics and
  **required for #308 category F targeting**"*;
- the absent-defaults table: *"Diagnostic only; **never a placement filter on its
  own**."*

They can be *narrowly* reconciled — "on its own" could mean "not as the sole
predicate" — but that reading makes the field useless for targeting, which is
what the doc comment claims it is required for. Two paragraphs of one document
disagreeing is not a contract; it must be settled.

### 5.1 Resolution: the defaults table is right, the doc comment is wrong

**Recommendation, as a finding for [#309 §4](./309-host-native-execution.md#4-capability-advertisement)
rather than a parallel mechanism:** keep `platform` as the diagnostic string the
defaults table describes, delete the "required for category F targeting" clause,
and target **both** legs of category F on specific, fail-closed capability
names.

The argument is the defaults table's own: `platform`'s absent-default is
`"unknown"`, and a filter whose default value matches nothing **fails open into
a wrong placement or fails closed into a fleet-wide stall**, depending on which
way the predicate is written — and neither is recoverable, because there is no
observation that distinguishes "N−1 daemon" from "genuinely unknown platform".
Design #309 §4 makes exactly this argument for `modes` and then does not apply
it to `platform`. Applying it is the fix.

More importantly, **neither leg of category F actually wants an OS string**:

- **iOS.** #322 §3 already made the *discovered Xcode set* the capability
  (`envs: ["xcode:16.4", "xcode:26.0"]`, read from the installed Xcode bundles
  at daemon startup) and says so in as many words: *"It is already the
  capability list."* A job needing Xcode 16.4 matches on `xcode:16.4`, not on
  `macos/aarch64`. `platform` adds nothing.
- **Android.** A job needs KVM and an SDK mount. `linux/x86_64` is neither
  necessary (a Linux node without KVM is useless to it) nor sufficient.

So the resolution is not a compromise — it is that `platform` was never the
right currency, and [#361](./361-per-run-placement.md) argues the general form of
this at length ("the currency of routine selection should be capability").

### 5.2 The field, and the shape smell worth naming

The proposal: **`features: Vec<String>`** on `NodeCapabilities`, absent ⇒ `[]`
(fails closed, exactly like `leases`), carried on both `PingOk` and
`WorkerAnnounce` as the same additive `Option<NodeCapabilities>` #309 §4
specifies — so no `WORKER_RPC_VERSION` bump. A KVM-enabled node with a
provisioned SDK advertises `["kvm", "android-sdk"]`; the daemon derives both
from its own config and its own filesystem, never from operator-typed strings
about what it *should* have.

The corresponding job-type side is `placement.features` and it is **phase A4**,
not now — §[2.3](#23-recommendation)'s trigger.

**The smell, stated honestly, because a reader will see it anyway.** Four
designs now each add a `Vec<String>` to `NodeCapabilities`: `modes` and `leases`
(#309 §4), `envs` (#322 P1), `images` (#355 §6), and now `features`. All five
are "names of things this node has, absent ⇒ empty, fails closed", and a single
namespaced set (`kvm`, `xcode:16.4`, `image:owner/project/name:sha`) would
express all of them with one predicate.

This document **does not propose that collapse**, for two reasons and it is
worth being explicit about both. First, it is out of scope — three of the four
fields belong to other designs and none of them is implemented, so collapsing
them is an edit to three unshipped documents by a fourth. Second, the fields
have genuinely different churn: `images` changes on every project image build
(#355 §5's reconciler), while `modes`, `envs` and `features` are boot-time facts
that design explicitly declines to give an ordering key *because* they are
boot-time facts. Merging a high-churn field into that set would drag the ordering question
back in. The finding to record is: **whoever implements #309 §4 first should
decide the whole set at once**, not accumulate it one design at a time.

### 5.3 The placement predicate

`choose_placement` (`crates/container/src/lib.rs`) needs no new shape — #355 §6
already says this and it is right. It is a pure function over
`&[PlacementCandidate]` with an optional pin, and it already skips ineligible
candidates. A required-features predicate is another skip, applied **after**
probing for #309 §4's bootstrap-deadlock reason, with its own distinct
`NoCapacity` message so the diagnosis survives:

- no candidate advertises a required feature → `NoCapacity("no node advertises
  kvm")`
- candidates advertise it but none is free → the existing `NoCapacity("no free
  slots on any node")`

Both transient by the §3.5 contract, both queued, neither consuming retry
budget. Tier 1 per [`testing.md`](../../testing.md), beside the existing
`choose_placement` tests.

**Interaction with #322, named as the brief asks.** The device grant and the
features predicate are shared primitives, and the *predicate* is the shared half:
`choose_placement` gains one required-capability argument once, and both
`xcode:16.4` and `kvm` are values in it. The *device* half is Android-only — a
Mac has no `/dev/kvm` and #322's simulator story is `simctl`-scoped teardown,
not device passthrough. So A3 below and #322 P1 are **the same slice**, and
whichever leg reaches it first should build it for both.

---

## 6. Alternatives

### 6.1 Redroid — containerized Android with no KVM at all

Runs Android as a normal Linux process tree on the host kernel; near-native
speed; no `/dev/kvm`. Verified against the cited article (fetched 2026-08-01):
it requires the host kernel modules `binder_linux` (`modprobe binder_linux
devices="binder,hwbinder,vndbinder"`) and `ashmem_linux`, unless the kernel
builds `CONFIG_ANDROID_BINDERFS` and ashmem in; it ships **no Google Play
Store** by default; and the article's own recommendation is Redroid "for
headless CI, device farms, ARM cloud" and the official emulator where
"Play-Store-dependent flows" matter.

*For:* No KVM, so it works on a node that is itself a VM without nested
virtualization — which is the one scenario that kills the recommendation
outright (§[8](#8-risks-and-open-questions)). Faster.

*Against, and it is decisive:* it trades a **narrower** node commitment for a
**wider** one. `--device /dev/kvm` needs a kernel that already has KVM, which
every bare-metal Linux node has. Redroid needs two out-of-tree kernel modules
loaded on the host — a `nixos-rebuild` with a custom kernel, i.e. exactly the
"machine facts" row of #308 H.6's table but a far heavier entry, and one that
must be re-derived on every kernel bump. And it changes **what is under test**:
Redroid is a container-Android, not the AVD system image the application ships
against, so a green Redroid run is weaker evidence than a green emulator run.
For a platform whose evaluation gates *are* the CI, weakening what a gate proves
is the wrong trade.

**Rejected, and named as the fallback** if the Linux node turns out to be a VM
without nested virtualization and cannot be replaced.

### 6.2 Host-native Android on a pinned Linux node

The #309/#322 route applied to Android: `WORKER_MODES=container,host`, a
`HostBackend`, `runtime: { mode: host, env: "nix:.#android" }`.

*For:* One mechanism for both legs of category F. Reuses whatever #322 W2 builds.
Gives ambient SDK state for free.

*Against:* It costs #322 W2 in full — the durable task registry, the liveness
ladder, restart recovery, the total `/workspace` + `/chuggernaut` wire-path
mapping, credential teardown at process exit, the agent-shaped-launch refusal —
plus N2's epoch bump and W4's env-ref resolution, for a leg that does not need
any of it. It re-acquires every problem containerization dissolves: ambient
mutable SDK state (#309 §9 calls per-project caches "a new mechanism, not a
widening of the old one" and mandates eviction), a shared adb server, and the
exclusivity primitive §[4](#4-exclusivity-and-why-android-does-not-need-a-lease)
shows is unnecessary. And it inherits #309's own stated schedule risk: "nobody
in this tree has built a non-container backend."

**Rejected.** It is the more general answer and it is strictly more expensive for
this leg.

### 6.3 Do nothing — keep Android on GitHub Actions

*For:* Zero platform work. It is also what happens by default if this ships late,
so it deserves to be priced rather than dismissed.

*Against:* Category F stays unported, which is the gap #308 ranks first. The
Android leg stays outside the platform's gating, so the property CLAUDE.md
describes — "every change merges through a Chuggernaut job, and the job's
evaluation criteria are the CI" — does not hold for the largest test suite the
consumer project has. And it leaves the corpus's claim that mobile requires
host-native execution unchallenged, which is the error that produced this
sequencing in the first place.

**Rejected as the end state, accepted as the honest status quo** until phase A1
lands. It is a legitimate answer to "what if the KVM precondition fails."

### 6.4 An ordinary container that SSHes into a Linux box

Design #322's option B, transposed. Named only to dismiss: the thing you would SSH to
is a Linux host that can run the container directly. #322's version of this
option exists because a Mac cannot run the container at all; that asymmetry is
the whole reason the option is interesting there and pointless here.

---

## 7. Sequencing: what ships first, and what it unblocks

**Yes — the Android leg can precede #322 W2, and by a wide margin.** Phases A0
and A1 touch no dispatcher code, no wire record, no schema, and no shared
`/workspace` handling. They are strictly node-side, which is the same property
that made #313 B-IV and `WORKER_CACHE_DIR` cheap.

| Phase | Kind | Work | Depends on |
| --- | --- | --- | --- |
| **A0** | operator | Confirm KVM on the target Linux node (`/dev/kvm` present, the daemon's user in the `kvm` group). Provision the SDK + system images at a host path. Zero code — and **this is the precondition the whole design rests on** | — |
| **A1** | `code` | `WORKER_KVM` + `WORKER_KVM_PROJECTS` + `WORKER_ANDROID_SDK_DIR` in `crates/worker/src/config.rs` beside `parse_cache_dir`; `DockerBackend` device + read-only-mount properties beside `cache_dir`; `build_host_config` populates `HostConfig.devices` and the second bind; `ANDROID_SDK_ROOT`/`ANDROID_HOME`/`ANDROID_USER_HOME` injected beside `inject_cache_env`; fail-loud when the setting is on and the device or mount is absent. A slim Android task image. The job type pinned with `placement.node`. **No wire change, no dispatcher change, no epoch bump** | A0 |
| **A2** | `code` | Prove it against a **boring `./gradlew connectedAndroidTest` on one module**, not against the full flutter integration suite — #308 H.4 is right that it sits at the confluence of too many unbuilt things, and #309 and #322 both say the same about their own first targets. Measure: does the emulator need to write into the SDK tree (§[4](#4-exclusivity-and-why-android-does-not-need-a-lease)); do two concurrent emulators fit in the node's memory; does the SIGSEGV reproduce | A1 |
| **A3** | `code` | `NodeCapabilities` on `PingOk`/`WorkerAnnounce` with `features`, and the `choose_placement` predicate — **this is [#309 §4](./309-host-native-execution.md#4-capability-advertisement)/§5a and [#322](./322-macos-native-runtime.md) P1, one slice serving both legs**. Unpins Android work. Needed only when a second KVM node exists | A2; §[5.2](#52-the-field-and-the-shape-smell-worth-naming)'s "decide the whole field set at once" |
| **A4** | `code` | `placement.features` as a job-type field + `CONFIG_SCHEMA_EPOCH` bump. Only when the pin stops expressing the requirement | A3 |
| **never** | — | Device leases for Android (§[4](#4-exclusivity-and-why-android-does-not-need-a-lease)). #309 §5b/P4 stays, motivated by iOS | — |

**A1 is the phase to start**, and its whole cost is one config parse, one
builder method, one `HostConfig` field, one bind and four env vars — the same
diff shape as the sccache work that already shipped.

**What it unblocks:** the Android half of #308 category F, without waiting on any
of #322's W2, W3, N1, N2, W4 or W5. Restated as a schedule fact: the corpus currently
sequences Android behind six phases of macOS work it does not need.

Test placement per [`testing.md`](../../testing.md): the config parses
(`WORKER_KVM`, `WORKER_KVM_PROJECTS`, `WORKER_ANDROID_SDK_DIR`), the produced
`HostConfig` (device present only for an allow-listed project; absent otherwise;
the read-only flag on the SDK bind), the env injection, and the
`choose_placement` features predicate are all pure → **tier 1**, beside
`host_config_with_cache_adds_one_bind` and `parse_cache_dir`'s tests. The
daemon's refusal to start with `WORKER_KVM` set and no device node is **tier 2**
beside `declared_mode_without_a_backend_refuses_to_start`. An emulator boot is
**tier 3 / out of tree** and belongs to the consumer project's own job.

---

## Contracts changed (per STYLE.md's contract-first rule)

| Phase | Contract changed |
| --- | --- |
| A1 | `WorkerConfig` gains device/mount/allow-list fields; `DockerBackend` gains a node-property builder beside `with_cache_dir`; **`build_host_config`'s postcondition changes** — "whether a cache bind-mount is present" becomes "which node properties are present", and the dispatcher-side `binds: None` invariant must survive as `devices: None, binds: None`; a new node-side invariant — a daemon never advertises or serves a device it does not hold |
| A3 | Two wire records additively (`PingOk`, `WorkerAnnounce` gain `Option<NodeCapabilities>`), no `WORKER_RPC_VERSION` bump; `choose_placement` postcondition gains the features predicate and a second distinct `NoCapacity` message |
| A4 | Job-type schema epoch (§14.1): `placement.features`, `CONFIG_SCHEMA_EPOCH` 2 → 3 (or later) with a frozen feature constant per `INPUTS_SCHEMA_EPOCH`'s precedent (`crates/types/src/version.rs`), plus the `min_dispatcher` rule |

---

## What this makes wrong elsewhere

- **[#308](./308-gha-port.md) H.2** lists "`/dev/kvm` for the Android emulator"
  among the things *host mode buys*. It is not a host-mode benefit; it is
  available in container mode with one flag.
- **#308 §F** — "Impossible under containers — not hard, impossible" is true of
  Xcode and false of the Android emulator. The sentence's own evidence (Xcode,
  `xcrun simctl`) supports the narrower claim only.
- **#308 H.4** — the AVD-persistence claim, per correction 7 *(secondhand)*.
- **#308's gap table, row 3** — "Host-native execution … carries mobile
  (category F, which nothing else unblocks)" carries **half** of category F.
- **#308 H.5 and [#309 §5b](./309-host-native-execution.md#5b-exclusive-resources-device-leases)**
  — the `beacon-emu` example is not a shared device in container mode. The
  primitive stands on #322 §5's simulator case; its Android motivation does not.
- **[#309 §4](./309-host-native-execution.md#4-capability-advertisement)** — the
  `NodeCapabilities.platform` doc comment contradicts the defaults table.
  Recommendation in §[5.1](#51-resolution-the-defaults-table-is-right-the-doc-comment-is-wrong).
- **#309 correction 4** — "the only way to change a node's slot count today is
  to restart its daemon" is now stale (correction 6).
- **[#313](./313-workload-identity-image-builds.md) B-IV** — its allow-list is
  described as per-`(project, job type)`; the job type is not observable
  node-side (correction 5).
- **`spec.md` §3.1** — "no host bind-mounts … The one permitted exception is a
  **worker-provisioned node-local build cache**" becomes a small closed class,
  and the section gains a device-passthrough sentence. This is a `docs` job, not
  part of A1.
- **`crates/container/src/docker.rs`** — `build_host_config`'s doc comment names
  the cache bind specifically.
- **`crates.md`'s `container` row** — already noted as wrong by #309; a device
  property does not make it wronger, but the same edit should catch it.

---

## 8. Risks and open questions

- **The KVM precondition is the whole design, and it is unverified.** This
  container has no `/dev/kvm` (`ls -l /dev/kvm` → no such file), which proves
  nothing about the fleet's nodes but does prove the check is not free. If the
  intended Linux node is itself a VM without nested virtualization, `--device
  /dev/kvm` fails and the fallbacks are Redroid (§6.1) or a different machine.
  **A0 exists to answer this before any code is written.**
- **A read-only SDK mount may not survive contact with `sdkmanager`.** The one
  measurement that could change the shape; §[4](#4-exclusivity-and-why-android-does-not-need-a-lease)
  states the answer if it fails (copy-up the mutable subset, not a lease).
- **The emulator's empirical tuning does not port.** `-gpu swangle`,
  `-no-snapshot` and the eleven-minute SIGSEGV are GHA-runner adaptations; #308
  §F says expect to re-derive them and it is right.
- **Kernel attack surface.** `/dev/kvm` in a container is a real CVE class
  (§2.1). The allow-list narrows *who*, not *what*; keeping the node's kernel
  current is a machine fact under #308 H.6's system-closure row and belongs in
  the node runbook.
- **Disk.** The SDK mount shares the partition with the node's images and the
  `worker-refresh.sh` free-disk floor. #355 §7's "the platform refresh must win"
  rule applies to a hand-provisioned mount as much as to a built image, and
  nothing enforces it for a directory the operator created.
- **Everything about beacon in this document is secondhand.** The AVD
  correction, the `runs-on` labels and the emulator flags all come from operator
  inspection, not from a tree this document could read. The device half of the
  design is independent of all of it; the toolchain half is not.
- **A3's field set should be decided once.** Five `Vec<String>` capability fields
  are proposed across four unimplemented designs
  (§[5.2](#52-the-field-and-the-shape-smell-worth-naming)). Whoever lands
  `NodeCapabilities` first inherits that decision whether or not they want it.
