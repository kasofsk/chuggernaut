# Design — a native (macOS) execution runtime for iOS/Xcode jobs

Status: PROPOSED.

Written against the tree at `61b721d` (2026-07-30). Every claim about current
behavior was read out of the source or out of [spec.md](../../spec.md); where
this document and the job brief disagree, the tree wins and the disagreement is
recorded under
[Corrections](#corrections-to-the-brief-and-to-309).

**Relationship to the existing docs — read this first.** Host-native execution
already has a design: [#309](./309-host-native-execution.md), which decided the
generic shape (backend polymorphism, the `runtime:` selector, capability
advertisement, placement, leases, drain, limits, secrets, tenancy) and named
macOS as its weakest platform without resolving the macOS-specific parts.
[#308 §H](./308-gha-port.md) argues *why* host execution is wanted and §F says
the mobile category is the one place no container cleverness helps.

This document is **not** a second general design. It decides the macOS
instantiation and the three things #309 left as open risk on that platform:

1. the durable per-task registry that replaces what the Docker daemon
   remembered for us, including recovery across a **launchd** restart and a
   reboot — the failure mode #309 named but did not design;
2. the `/workspace` rebase, whose *totality* and whose **credential teardown
   guarantee** #309 leaves at "part of the boundary, not just hygiene";
3. `runtime.env` — #309 requires it and defines exactly one scheme,
   `nix:<flake-ref>#<attr>`. **Xcode is not expressible as a nix flake output.**
   That is a real gap in #309 for the only category that motivated it, and
   §[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode)
   closes it.

It also does the comparison the brief asks for and #309 does not do at all:
native execution versus *SSHing into a Mac from an ordinary container*, versus
doing nothing. #309 assumes host mode is wanted and argues its shape; this
document argues the decision.

Related: [spec.md](../../spec.md) §1.1 (job-type schema and the field-rules
matrix), §3.1 (backends, placement, worker RPC, self-refresh, node-local
caching), §3.5 (launch capacity queue, task timeout), §3.6 (restart
reconciliation and the two sweeps), §4.1 (workspace bootstrap), §14
(config/version skew), Appendix: Deferred ("macOS bare metal dispatchers");
[design.md](../../design.md) (single-writer discipline);
[#293](./293-worker-capacity.md) (worker capacity — landed in the wire types
since #309 was written); [STYLE.md](../../STYLE.md);
[testing.md](../../testing.md); [crates.md](../../crates.md).

## Corrections to the brief and to #309

The brief's findings are accurate on every point that carries its argument. Four
things have moved or need adjusting, and each one moves work.

1. **`CONFIG_SCHEMA_EPOCH` is already `2`, not `1`.** `crates/types/src/version.rs`
   holds `pub const CONFIG_SCHEMA_EPOCH: u32 = 2` — bumped for job `inputs:`
   (#311), with a frozen `INPUTS_SCHEMA_EPOCH = 2` beside it so a later
   unrelated bump does not retroactively raise what an existing `inputs:`
   config must declare. So #309's "bump 1 → 2" is stale: the host selector is a
   **2 → 3** bump, and it should follow the `INPUTS_SCHEMA_EPOCH` precedent with
   its own frozen constant rather than reading the moving one. See
   §[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode).
2. **#293's wire fields have landed.** #309 correction 1 recorded that
   `spec.md` §3.1 described `slots_max` / `capacity_epoch` /
   `capacity_generation` / `set_slots` while the code had none of it. The code
   has them now: `WorkerAnnounce` and `PingOk` both carry
   `slots_max`/`capacity_epoch`/`capacity_generation` as `Option`s with
   `#[serde(default)]` (`crates/types/src/worker.rs`), and the trait gained a
   sixth provided method, `set_node_slots` (`crates/container/src/lib.rs`). Two
   consequences: **a node's slot count is now operator-settable at runtime**, so
   moving a Mac from 1 slot to 2 needs no redeploy; and #309's "do not race
   #293" sequencing risk is largely retired.
3. **The trait is 10 required + 6 provided methods.** #309 says five provided;
   `set_node_slots` is the sixth. `crates/container/src/k8s.rs` is still a bare
   `pub struct K8sBackend;` with a TODO, and `grep -rn "impl ContainerBackend
   for" crates/` still returns exactly three implementations (`DockerBackend`,
   `FakeBackend`, `FleetBackend`). The brief's caveat about the trait never
   having been pressure-tested by a second real implementation stands.
4. **job/181's lesson is narrower and sharper than the brief states, and the
   existing mitigation does not cover a host backend.** The outage was a worker
   answering `ping` while `list_running` failed, leaving fleet occupancy blind
   and the node "falsely idle". That has a fix in the tree:
   `FleetNode::list_failed` is set when the listing RPC errors and
   `FleetBackend::occupancy_unavailable_nodes` names those nodes so the snapshot
   shows out-of-service instead of `occupied: 0`
   (`crates/worker/src/backend.rs`). **`list_failed` is set only when the RPC
   returns an error.** A host backend whose registry scan hits an unreadable
   directory and returns `Ok(vec![])` is *not* an RPC error — it reproduces
   job/181 exactly, past its own fix. That is why
   §[1](#1-the-durable-task-registry) makes fail-loud listing a contract rather
   than a code-review note.

5. **`bootstrap_cmd` cannot simply "gain a workspace parameter", because its
   callers are above the backend.** The brief and
   [#309 §2](./309-host-native-execution.md#2-the-traits-container-assumptions)
   both describe the rebase as a parameter on `bootstrap_cmd`
   (`crates/container/src/lib.rs`). But it is called from
   `crates/dispatcher/src/launch_queue.rs` and `crates/agent/src/claude.rs` —
   *dispatcher*-side, at a point that does not know and must not know the
   node's task directory. A parameter there could only ever be the constant
   `/workspace`. §[2](#2-workspace-as-a-virtual-wire-path) resolves this with
   env indirection instead, which keeps the change worker-local and is what
   lets the rebase land in the prototype phase rather than after it.

One claim in the brief is stronger than stated: `/workspace` is not the only
hardcoded wire path. Injected credentials land under `/chuggernaut/` —
`SSH_ID_PATH = "/chuggernaut/ssh/id"` at mode 0600 and `SSH_CERT_PATH =
"/chuggernaut/ssh/id-cert.pub"` (`crates/dispatcher/src/exec.rs`) — so the
rebase rule needs **two** prefixes and must be total over both. It is stronger
again than *that*: those constants are also **interpolated into env values** —
`GIT_SSH_COMMAND` is built as `ssh -i {SSH_ID_PATH} -o
CertificateFile={SSH_CERT_PATH} …` in the same file — so a rebase that moves
the files without moving the strings that point at them produces a task that
clones nothing and fails on push.

## The problem

Some work cannot run in a container on any host we own:

- `xcodebuild`, the Xcode toolchain, and `xcrun simctl` are macOS-userspace-only.
- CoreSimulator is a per-user-session service (`com.apple.CoreSimulator.CoreSimulatorService`
  in the user's launchd domain) with device state under
  `~/Library/Developer/CoreSimulator`.
- Code signing reads identities out of a keychain belonging to a logged-in user.

Containers on macOS are Linux guests inside a VM — colima (which is what the
Mini runs today, per `deploy/prod/README.md`), Docker Desktop, and Apple's
`container` framework are all Linux VMs. There is no macOS kernel to share and
nothing to pass through. `spec.md` Appendix: Deferred already concedes the
point: "**macOS bare metal dispatchers**: required for Xcode builds. Execution
model needs separate design."

So the question is not "how do we containerize Xcode" — it is "what does the
platform do with a class of work that must run as a host process on a Mac".

## The options

### A — native execution as a worker-local runtime (recommended)

A `chuggernaut worker` daemon on a Mac gains a second local backend that runs
the task as a **host process group in a per-task directory** instead of
launching a container. From the dispatcher the node stays an ordinary
`{name}|worker|{slots}` roster entry (`nuc|worker|4` is the documented seed
form, `crates/worker/src/backend.rs`) reached over NATS request-reply.

The brief's claim that the dispatcher side needs no changes is **verified in
substance**, with one qualification. Placement, the `reserved` in-flight counter
and its `Reservation` drop, `NoCapacity` → the §3.5 launch queue,
announce/heartbeat, occupancy, `set_node_slots`, and the §3.6 sweeps are all
generic over what is on the far end of the RPC: `NodeHandle::Worker` proxies ops
on `req.worker.{node}.{op}` and never learns what executes them. The
qualification is that **schema-level** dispatcher changes are unavoidable —
`image` is required today (§[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode))
— so "no dispatcher changes" is true of the *runtime* path and false of the
*config* path. That distinction is the whole shape of the phasing below.

There is direct precedent for putting a runtime decision entirely worker-side.
`cache_dir` is documented in `crates/worker/src/config.rs` as "a node property,
provisioned entirely worker-side: it never rides the wire or the dispatcher's
launch config (spec §3.1)", and the daemon injects sccache env itself in
`inject_cache_env` (`crates/worker/src/daemon.rs`). #309's `WORKER_MODES` sits
in exactly that slot. (The brief calls it `CHUG_RUNTIME=host`; adopt #309's
name and its **list** form — a Mac that also runs colima can serve both modes,
and the list is what expresses that.)

**For:** one scheduler, one credential shape, one log path, one artifact path,
one fleet view. Concurrency is counted by the mechanism that already counts it.
An operator sees the Mac in `fleet.status` with a version, a heartbeat, and an
occupancy number; a wedged Mac is visible as a wedged node. `task_timeout` kills
a real process group the platform owns. Secrets stay per-task and short-lived.

**Against:** it is the expensive option. It needs backend polymorphism in the
daemon, a genuinely new durable-state component
(§[1](#1-the-durable-task-registry)), a rebase rule in shared code
(§[2](#2-workspace-as-a-virtual-wire-path)), a normative `spec.md` §1.1/§4.1
change plus an epoch bump (§[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode)),
and a node-provisioning runbook the platform does not have
(§[6](#6-node-provisioning-and-the-self-refresh-collision)). It also **gives up
the isolation boundary** — that is inherent to the category and is discussed in
§[7](#7-what-the-recommendation-gives-up), not hidden here.

### B — an ordinary container that SSHes into a Mac

A normal container job with an SSH key secret runs `ssh mac 'xcodebuild …'`.
Zero platform change; buildable in an afternoon.

**This option is real and its appeal is not fake.** It works today, it needs no
epoch bump, no new backend, and no spec edit. For a single one-off release build
it is the right amount of machinery. The costs, honestly:

- **It introduces a second, invisible scheduler.** Fleet placement, slots, and
  the §3.5 no-capacity queue all count **container** slots. Two containers
  SSHing into the same Mac contend over one CoreSimulator session, one shared
  `~/Library/Developer/Xcode/DerivedData`, and one `xcode-select` setting, with
  nothing counting them. That is precisely the untracked-concurrency shape
  [design.md](../../design.md)'s single-writer discipline exists to avoid.
  *Concession:* the concurrency **can** be bounded with existing mechanism —
  pin the iOS job type to a dedicated 1-slot node and only one such container
  runs at a time. At that point you are paying for a whole node to serialize
  access to another whole machine, and the bound is a side effect of a
  placement pin rather than a stated invariant, so it breaks silently the day
  anything else is pinned there.
- **Killing the container does not kill the build.** §3.5's timeout scan kills
  the container; the container's death kills the **ssh client**. Without a tty
  the remote `xcodebuild` is not reliably signalled, so a timed-out task can
  leave an orphan build on the Mac holding a booted simulator and a DerivedData
  lock — invisible to the platform, and poisoning the next task. The platform's
  strongest bound (`task_timeout` — the one bound that works in every mode)
  stops being a bound.
- **The isolation boundary is gone while its cost is still paid.** The container
  pulls an image, clones the branch, and holds an overlay — and then hands
  project-controlled code a shell on a real Mac. Every argument against host
  mode's blast radius applies in full, plus the container's cost.
- **The credential shape regresses.** The platform mints per-task,
  TTL-bounded SSH certificates today (`ssh_credential_files`, TTL = the task's
  resolved timeout, §7.4). B replaces that with a **long-lived private key to a
  Mac** stored as a job secret and injected into every task of the type. A
  credential whose lifetime is bounded by rotation discipline instead of by the
  task is a strictly worse credential.
- **The Mac is not in the fleet.** No heartbeat, no version, no occupancy, no
  `list_running`. An operator reads a green fleet while the Mac is wedged. That
  is job/181's failure class, reintroduced by construction — and this time with
  no `list_failed` bit to surface it, because there is no node.
- **Two checkouts and a round trip.** The branch is cloned in the container by
  the §4.1 bootstrap and again on the Mac; logs and `eval-result.json` come back
  by hand. Each hop is the job's problem to get right, and each hop fails
  silently in a way the platform cannot classify.

### C — do nothing: `work.type: human`

`.chug/jobs/manual.yaml` already declares `work.type: human` with no top-level
`image`: the operator pushes commits to the job branch and resolves the Work
task Pass, launching no container at all (§1.1 — `image` is *disallowed* for
human work). An operator can run `xcodebuild` on their own Mac and drive the job
by hand today.

This is a **legitimate stopgap** and should be named as one rather than
dismissed. It costs nothing, it is already implemented, and for a handful of
releases it is fine.

Its ceiling is exact: the iOS build is **not gated**, it is *asserted*. The
appended `ci` evaluator runs in a Linux container
(`.chug/jobs/_defaults.yaml`, `image: chuggernaut/agent-rust:prod`), so the
iOS half of any gate cannot execute; `work_retries` is disallowed for human work
and human tasks are excluded from the timeout scan (§1.1), so there is no
retry, no timeout, and no captured evidence. It does not scale past the
attention of one person.

### The rejected fourth option, for completeness

A **rented Mac** (MacStadium, EC2 mac instances) reached the same way is B with
a bigger bill: identical scheduler, orphan, credential, and visibility
problems. If B is chosen, the location of the Mac is not the interesting
variable.

### Recommendation

**A**, with **C as the explicit interim** while A's phase 1 is built, and **B
rejected** — including as a stepping stone.

The deciding argument is not that B is hacky. It is that B's central cost is
**untracked concurrency against a shared, stateful, single-session resource**,
and every later step of A begins by unwinding it: a lease primitive
([#309 §5b](./309-host-native-execution.md#5b-exclusive-resources-device-leases))
cannot count holders it cannot see, and the §3.6 sweeps cannot reap orphans on a
machine that is not a node. C has the same ceiling but does not build anything
that must later be removed — which is what makes it the honest interim and B a
detour.

Concede the one case where B wins outright: if the requirement is exactly one
signed release build per month, forever, then B's total cost is lower than A's
first phase and this recommendation is wrong. The recommendation assumes iOS
work becomes ordinary work — jobs a DAG depends on, gated by evaluators — which
is the premise of #308 category F.

## 1. The durable task registry

This is the only net-new component rather than a reuse, and it is worst-first
for a reason: `list_managed_exited` and `list_managed_running` back the two
§3.6 sweeps and the ping running-count, and the whole `chuggernaut.managed`
label scheme assumes an external daemon that remembers containers across
dispatcher **and** worker restarts. Host processes do not remember anything:
pids are recycled, exit statuses are reaped by whoever waited, and a reboot
erases the process table.

On a Mac this is not a rare edge. The Mini's own services run under **launchd**
with `KeepAlive` (`deploy/prod/install-launchd.sh`, the plist templates in
`deploy/prod/launchd/`), and launchd restarts a crashed agent immediately. A
design that only works while the daemon stays up is not a design here.

### The task directory is the container

`{WORKER_HOST_ROOT}/{task_id}/`, created 0700 **before** anything is written
into it. `WORKER_HOST_ROOT` is #309 §2's node-side setting, provisioned exactly
like `cache_dir` — worker-side, off the wire, never in the dispatcher's launch
config. On macOS it **must be a path the task user owns**, under that user's
home or a data volume (`/Users/chug/tasks` in the runbook,
§[6](#6-node-provisioning-and-the-self-refresh-collision)); it must not be a
root-level path, for the reason in §[2](#2-workspace-as-a-virtual-wire-path).
The daemon refuses to advertise `host` mode if the root is missing or not
writable — the same "refuse to advertise unless provisioned" rule #309 §8
applies to the user pool.

| Entry | Contents |
| --- | --- |
| `meta.json` | schema version, task id, identity labels (`project`/`job`/`task`, read from the launch env exactly as the Docker path's `managed_labels` does), pid, pgid, the process **start time**, the host **boot generation**, resolved env ref, created-at |
| `output.log` | merged stdout+stderr, append-only, opened once |
| `exit_code` | the terminal verdict, written **atomically** (temp + rename) |
| `workspace/` | the clone (§[2](#2-workspace-as-a-virtual-wire-path)) |
| `chuggernaut/` | the mapped `/chuggernaut` prefix — injected credential files and node-local artifacts, deleted at process exit (§[2](#2-workspace-as-a-virtual-wire-path)) |
| `devices/` | the task's private CoreSimulator device set, if any (§[5](#5-ios-specifics)) |

### The exit code is written by the task's own wrapper, not by the daemon

This is the load-bearing decision, and it is what makes everything else
trivial. Each task is spawned as a small generated **wrapper** — the same
`process_group(0)` + piped-stdout shape `run_script` already uses in
`crates/worker/src/daemon.rs` — whose only job is:

```sh
# {task_dir}/run.sh — generated per task
"$@" ; code=$?
rm -rf "$TASK_DIR/chuggernaut"
printf '%s' "$code" > "$TASK_DIR/.exit_code.tmp"
mv "$TASK_DIR/.exit_code.tmp" "$TASK_DIR/exit_code"
```

Because the verdict is recorded by a process that is *inside* the task, the
daemon **never needs to be alive at the moment a task exits**. There is nothing
to re-attach to and no `wait` to lose: `inspect` becomes a pure function of the
directory, `logs`/`logs_tail` read a file, and a daemon restart mid-task is a
non-event. Compare the container path, where the same property is provided by
the Docker daemon remembering the exit status for us — this is the host
analogue of that memory, and it is cheaper than it looks.

The trait's `wait` stays poll-over-`inspect` and is never on the hot path: per
§3.1 the dispatcher implements waiting as an inspect poll precisely so worker
restarts are transparent, and the daemon's `handle()` never calls `wait`.

### Liveness: pid + start time + boot generation

For a task dir with no `exit_code`, "is it still running?" is answered in this
order, cheapest and most decisive first:

1. **Boot generation mismatch ⇒ definitively dead.** Record
   `sysctl -n kern.boottime` (Linux: the boot id) at launch. Different boot,
   no `exit_code` ⇒ the process cannot exist. This closes the reboot hole
   *exactly*, with no pid reasoning at all.
2. **Same boot: pid + start time must both match.** A bare pgid check can match
   an unrelated recycled process; the (pid, start-time) pair cannot. macOS:
   `ps -p <pid> -o lstart=`; Linux: field 22 of `/proc/<pid>/stat`. A mismatch
   is *gone*.
3. **Neither ⇒ synthesize a terminal verdict, once, durably.** The daemon writes
   `exit_code` (`-1`) plus an `exit_reason` (`host_rebooted` /
   `process_vanished`) using the same atomic write. After that the directory is
   self-describing and the answer is stable across every future call.

Step 3 is the important one. Reporting such a task as **Running** is the bad
failure: §3.6 classifies it as "still running, re-attach" and the task hangs
until `task_timeout` — a slow, confusing failure. Synthesizing a nonzero exit
makes it a loud task failure that consumes a retry and says why. Assert the
negative space (STYLE.md Tier 2 rule 2): *no task dir ever transitions out of
having an `exit_code`*, and the daemon is the only writer of a task dir.

### Restart recovery, concretely

On daemon start, before serving any op:

1. Enumerate `{WORKER_HOST_ROOT}/*/`, **bounded** at
   `slots_max × HOST_TASK_DIRS_PER_SLOT_MAX` entries. Exceeding the bound is a
   loud startup error, not a truncated scan — an unbounded registry on a laptop
   disk is exactly what STYLE.md Tier 2 rule 3 forbids.
2. For each dir with no `exit_code`, run the liveness ladder above. Live tasks
   are **adopted with no work** (nothing to re-attach); dead ones get a
   synthesized verdict.
3. Sweep **orphan device sets**: for any dir now terminal, tear down
   `{task_dir}/devices` (§[5](#5-ios-specifics)) before it is removable. A
   booted simulator surviving its task holds RAM and poisons the next one.
4. Delete task dirs whose `exit_code` is older than `HOST_TASK_RETENTION` *and*
   which the dispatcher has stopped asking about. §3.6's own
   `sweep_exited_containers` (`crates/dispatcher/src/reconcile.rs`) is the
   authoritative reclaimer — it reads `list_managed_exited` and removes what no
   live task owns — so this local sweep is only the backstop for a dispatcher
   that never comes back. **No new sweep is added to the dispatcher.**

### Fail loud, never empty — the job/181 rule as a contract

Per correction 4, `list_failed` only fires on an RPC error, so a host backend
that swallows a scan failure defeats the existing mitigation. Therefore:

- A registry scan that cannot read the root, or cannot read *any* task dir,
  returns `Err(BackendError::Unavailable)`. It **never** returns a partial
  `Ok`. The RPC error is what sets `list_failed` and keeps the node from
  reading falsely idle.
- A task dir whose `meta.json` is unparseable counts as **running-unknown** —
  it holds a slot and is reported by `list_managed_running` with its labels
  absent. Failing closed toward "occupied" wastes a slot; failing open reaps a
  live build. (§3.6's sweep already refuses to reap a container carrying the
  marker but no identity labels, so this classification lands in behavior the
  spec already describes.)
- `managed_running_total` is derived from **the same scan**, so ping's count and
  the listing can never disagree. It is #309's provided-method-on-the-trait, and
  the default implementation (`list_managed_running().len()`) is the right one
  here — the cheap label-filtered override is Docker's alone.

### The residual failure mode, stated

A leaked task dir costs disk (a Rust or Xcode build tree is gigabytes); a
wrongly-reaped live build costs a job. The design above prefers leaking:
retention is time-based and bounded, reaping requires a *positive* liveness
answer, and the unparseable case is treated as live. The bound on leaks is the
startup scan cap plus retention, and hitting either is loud.

## 2. `/workspace` as a virtual wire path

`bootstrap_cmd` (`crates/container/src/lib.rs`) emits

```text
git clone --single-branch --filter=blob:none --branch "$JOB_BRANCH" "$REPO_URL" /workspace && cd /workspace && exec {cmd}
```

and the **dispatcher** reaches back for `/workspace/eval-result.json` through
`copy_file` (`crates/dispatcher/src/launch_queue.rs`, and documented in
`spec.md` §4.1/§1.1 for command evaluators). Injected credentials use the second
fixed prefix, `/chuggernaut/` (`crates/dispatcher/src/exec.rs`).

One fixed absolute path on a shared Mac means concurrent tasks collide, and a
daemon that took those paths literally would write into the **host root**.

**On macOS it is worse than a collision: it does not run at all.** Since
Catalina the root filesystem is a sealed, read-only system volume, so
`git clone … /workspace` fails at `mkdir` — creating a root-level directory
requires an `/etc/synthetic.conf` entry and a reboot. That single fact settles
the sequencing question below: the rebase is not hardening that a prototype can
defer, it is the thing that makes the **first** host launch possible. The
alternative — telling every Mac operator to add a synthetic firmlink so that
project code can clone into a machine-global path outside the 0700 task
directory — trades a one-file worker change for a permanent provisioning step
whose whole purpose is to reintroduce the hazard this section exists to remove.

**Evaluated and adopted: keep `/workspace` and `/chuggernaut` as *virtual wire
paths* that the host backend maps to the task dir.** This is #309's option (i)
(rebase), recommended there as the durable answer, and nothing above the backend
changes. Per correction 5 the mechanism is **not** a parameter on
`bootstrap_cmd` — its callers are dispatcher-side. It is env indirection:

```text
WS="${CHUG_WORKSPACE:-/workspace}"; git clone --single-branch --filter=blob:none \
  --branch "$JOB_BRANCH" "$REPO_URL" "$WS" && cd "$WS" && exec {cmd}
```

`CHUG_WORKSPACE` unset is today's behavior byte-for-byte, so every container
task is unaffected and the change is testable at tier 1. The host backend sets
it to `{task_dir}/workspace`. Three additions this document makes:

- **The rebase has four surfaces, not one.** The clone destination (above);
  `InjectedFile.container_path` (`crates/container/src/lib.rs`), which is where
  the credential files land; the `copy_file` argument the dispatcher uses to
  fetch `/workspace/eval-result.json`; and — the one easy to miss —
  **env *values* that embed a wire prefix**, because `GIT_SSH_COMMAND` names
  `SSH_ID_PATH` inline (corrections, above). Env values are rebased by the same
  prefix substitution as paths. Substitution over env strings is the ugly part
  of this design and is worth naming as such: it is textual, and it is
  defensible only because the two prefixes are constants owned by this repo and
  are asserted at rebase time to appear nowhere in a task's env except as a
  path. If a third consumer ever interpolates a wire path into a *value*, this
  rule is what catches it — the assertion fires rather than the credential
  silently pointing at nothing.
- **The mapping must be total, and unmapped is an error.** `/workspace/*` →
  `{task_dir}/workspace/*`, `/chuggernaut/*` → `{task_dir}/chuggernaut/*`, and
  **anything else is a hard `BackendError::Launch` / `NotFound`** naming the
  path. There is no fall-through to the host filesystem, and the rebase asserts
  that its output is under the task dir (STYLE.md Tier 2 rule 2 — assert the
  negative space). A permissive mapping is how a `copy_file` bug becomes a write
  to `/`.
- **Symlink escape is part of totality.** The rebase resolves and re-checks
  containment after normalization, so `/workspace/../../etc` cannot address the
  host. This is the one piece that is genuinely hardening rather than
  enablement, and it is the only part of the rebase the phasing defers.

The alternatives are recorded in
[#309 §2](./309-host-native-execution.md#2-the-traits-container-assumptions)
and the conclusion is unchanged on macOS, with one becoming *worse*: option (ii)
(per-task mount namespaces) is **impossible on macOS** — no per-process bind
mounts — and macOS is the entire reason this category exists. Option (iii)
(one host task at a time, `slots: 1`) is still recommended for phase 1, but on
the strength of CoreSimulator's global device state (§[5](#5-ios-specifics)),
**not** as a substitute for the rebase: serializing tasks removes the collision
between two tasks and leaves the collision with the host filesystem exactly
where it was.

### The transcript-path trap, and the scoping decision it forces

`agent::transcript_path` (`crates/agent/src/lib.rs`) returns
`{CLAUDE_CONFIG_DIR}/projects/-workspace/{session_id}.jsonl`, where
`-workspace` is the agent CLI's **slugification of the cwd** — a measured
property of an external CLI, per that function's own doc comment. Change the
cwd to `{task_dir}/workspace` and the transcript lands elsewhere; the harvest
silently finds nothing.

**Decision: phase 1 supports `work.type: command` only on a Mac node.** An
`agent` host task additionally needs a node-provisioned agent CLI, a
`CLAUDE_CONFIG_DIR` per task, and a *computed* slugifier replacing a measured
constant. `xcodebuild`, `fastlane` and simulator test runs are all command work,
and the `ci` evaluator is a command evaluator, so nothing in the motivating
category is blocked. What is given up: no agent-driven iOS work until a later
phase.

**This restriction is enforced, not documented.** A job-type doc note would be
the exact silent-ignore failure this document refuses everywhere else — and an
agent host task is not self-correcting, because §[6](#6-node-provisioning-and-the-self-refresh-collision)
provisions a node-local `chuggernaut-channel` copy: the CLI is present, the run
looks healthy, and the harvest finds nothing. Two enforcement points, because
they become available at different times:

1. **A field rule, with the others (N2).** `runtime.mode: host` requires
   `work.type: command`, reported as an ordinary `FieldRuleError::Invalid` on
   `work.type`. This is expressible where the other rules live: `validate()`
   already matches on `self.work.r#type` (`crates/types/src/job_type.rs`), so
   the rule is a branch in the arm that already exists, and it fails the
   author's own CI rather than a task at runtime — the same shape as the
   `min_dispatcher >= RUNTIME_SCHEMA_EPOCH` rule below.
2. **A launch-time refusal at the node, always.** The daemon refuses a host
   launch carrying agent shape — concretely, one whose env sets
   `CLAUDE_CONFIG_DIR` (`crates/agent/src/lib.rs`) — with a hard
   `BackendError::Launch` naming the reason. This backstop is not redundant:
   during W2 the `runtime:` block does not exist yet, so **it is the only
   enforcement there is**, and it stays afterwards to cover a job type reaching
   a host node by a `placement.node` pin the schema never saw.

Lifting the restriction is P2 work, where the computed slugifier lands.

### Teardown, and what actually bounds credential lifetime

In a container, injected credentials vanish with the overlay. On a host they are
files on a disk, so the guarantee has to be stated:

1. **Injection.** The task dir is created 0700 before any write; credential
   files keep their declared modes (0600 for `SSH_ID_PATH`, 0644 for the
   certificate) and land under the mapped `/chuggernaut` prefix —
   `{task_dir}/chuggernaut/ssh/id` — never in `workspace/` (which the task's own
   tooling walks and which a stray `git add` could reach). **This is a
   consequence of the rebase, not an addition to it**, and it is the second
   reason the rebase cannot be deferred past the first host launch: without it
   the same files land at the fixed host path `/chuggernaut/ssh/id`, outside the
   0700 task directory, where nothing in this list is true of them.
2. **At process exit, credentials are removed — earlier than `remove()`.** The
   wrapper deletes `chuggernaut/` as its first act after the command returns
   (see the snippet in §[1](#1-the-durable-task-registry)), and the daemon
   re-deletes it when it observes the terminal state. This is deliberately
   *not* deferred to `remove()`: `logs` and `copy_file` must still work after
   exit, so the directory has to outlive the process — but the secrets do not.
   Env-carried secrets die with the process image, as they do today.
3. **`remove()` deletes the whole task dir**, idempotently and best-effort per
   the trait's contract — a failed removal must never fail a job — with
   §3.6's exited sweep plus the local retention sweep as the crash backstops.
4. **The durable bound is the credential's TTL, not the file's lifetime.** This
   is the honest answer and it is already true in the tree: `ssh_credential_files`
   issues a certificate whose TTL is the task's resolved timeout
   (`creds_ttl`, §7.4), and `NATS_CREDS` is minted per task. A leaked file is
   therefore a *bounded-lifetime* credential, not a standing one. Guaranteed
   directory removal is defence in depth; short TTLs are the boundary.

Residual risk, unpapered: on a single-user Mac (§[5](#5-ios-specifics)) any
process of the task user can read `{task_dir}/chuggernaut` and
`/proc`-equivalent env while the task runs. #309 §8's per-task unix-user pool is
the fix on Linux and **conflicts with CoreSimulator on macOS** — see
§[5](#5-ios-specifics). The mitigation here is single-tenancy, not isolation.

## 3. `image`, `resources`, and what `runtime.env` means when the toolchain is Xcode

This is where the real cost sits, because it is the part that must go through
the normative docs rather than just code.

**What is true today.** `ContainerLaunchConfig.image: String` and
`WorkerLaunchRequest.image: String` are both non-optional
(`crates/container/src/lib.rs`, `crates/types/src/worker.rs`). `spec.md` §1.1
makes `image` **required** for `work.type: agent | command` and **disallowed**
for `human`, and `JobType::validate` enforces it with `Required { field:
"image" }` rules (`crates/types/src/job_type.rs`). A host runtime has no image.
`Resources { cpu, memory, task_timeout }` reach `nano_cpus`/`memory` on the
Docker `HostConfig` (`crates/container/src/docker.rs`); macOS has no cgroups, so
`cpu`/`memory` would be silently ignored.

### The selector: options and the one to take

| | A: `runtime:` block | B: flat `host_env:` peer | C: overload `image` with a sentinel/scheme |
| --- | --- | --- | --- |
| Epoch bump | yes (2 → 3) | yes | **no** |
| Typo protection | **yes** — nested blocks keep `deny_unknown_fields` (§14.2), so `mdoe: host` is a hard parse error | no — a typo'd top-level field is an ignored unknown, i.e. a job that silently runs as a container | no |
| N−1 behavior | config **parks** pre-Work (Stalled, one park, §14.2) | same | **worse** — config parses, then every launch fails pulling a bogus image, burning `work_retries` per job |
| Extends to a further mode | one discriminant | a new top-level field per mode | no |

**Adopt A**, as [#309 §3](./309-host-native-execution.md#3-the-host-mode-selector)
recommends, and for the reason #309 gives: C is genuinely the cheapest and
should still be rejected, because it converts a *detected, explained,
non-destructive* skew condition (the §14.2 park) into an undetected one that
burns retry budget job by job. Trading the skew machinery's entire purpose for
one avoided integer is a bad trade.

```yaml
name: ios-integration
runtime:
  mode: host            # container (default) | host
  env: "xcode:16.4"     # required when mode: host, disallowed otherwise
min_dispatcher: 3
work:
  type: command
  run: ./.chug/tasks/ios-tests.sh
resources:
  task_timeout: 90m     # cpu/memory deliberately absent — see below
```

### The gap this document closes: `runtime.env` for Xcode

Design #309 requires `runtime.env` under `mode: host` and defines one form,
`nix:<flake-ref>#<attr>`, resolved against the job-branch checkout. **Xcode is
not a nix flake output.** It is a signed 15 GB app bundle installed on the
machine, with per-version simulator runtimes and a license that must be
accepted once with root. Requiring a flake ref on a Mac would force authors to
write a fiction.

Options weighed:

1. **Make `runtime.env` optional; absent means "the node's ambient
   environment."** Cheapest. Rejected: it deletes the one property that makes
   the field worth having. The toolchain becomes an unpinned node fact, so the
   same commit builds differently on two Macs and nothing in the job says which
   Xcode ran.
2. **Add a `none` sentinel.** Same as 1 with extra syntax, and it reads as
   deliberate rather than as a gap.
3. **Keep `runtime.env` required and make its *scheme* node-interpreted:
   `xcode:<version>` (recommended).** The field keeps its exact #309 contract —
   opaque, declared in the project repo, resolved by the node, one required
   field — and only the scheme registry grows. The node **discovers** its
   installed Xcodes at startup (scan `/Applications/Xcode*.app`, read
   `version.plist`) and maps `xcode:16.4` to a `DEVELOPER_DIR` exported into the
   task. An unknown version is a hard `BackendError::Launch` naming what *is*
   installed — never a silent build against whatever `xcode-select` points at.

**Take 3.** Three properties fall out of it that are worth naming, because they
are the argument:

- **`DEVELOPER_DIR` per task, never `xcode-select -s`.** `xcode-select` mutates
  a machine-global symlink; two concurrent tasks on different Xcodes would
  fight over it, and a crashed task would leave the machine changed.
  `DEVELOPER_DIR` is per-process and needs no cleanup. Toolchain selection stops
  being global mutable state — which is the same reason the platform prefers a
  per-task device set in §[5](#5-ios-specifics).
- **Discovery, not operator-typed config.** #309 rejects dispatcher-side static
  node config because it relocates a physical fact into config that goes
  silently wrong after a rebuild. Reading the installed bundles keeps the fact
  where the fact is.
- **It is already the capability list.** The discovered set (`envs:
  ["xcode:16.4", "xcode:26.0"]`) is exactly what a node should advertise in
  #309's `NodeCapabilities`, so capability-filtered placement later needs no new
  field — until then, a `placement.node` pin routes the work.

The honest cost of 3: the mapping is only as good as the version string, two
Macs with "the same" Xcode can still differ in simulator runtimes and CLT, and
**an unpinnable environment must therefore be recorded**. Require the phase-1
task script to log `sw_vers`, `xcodebuild -version`, and `xcrun simctl
runtime list` into the task's stdout, which the platform already captures and
encrypts. A build that cannot be pinned should at least be **auditable after the
fact**; that is the most this design can honestly offer, and it is more than
option 1 offers.

### `resources.cpu` / `resources.memory`

Adopt #309 §7's answer — **enforceability is a capability, refusal is the
backstop** — with the macOS consequence stated flatly: `resources_enforced:
false` on a Mac node, `choose_placement` treats a job type declaring
`cpu`/`memory` as requiring enforcement, and a launch that arrives anyway (via
a pin) is a hard `BackendError::Launch` naming the field and the node. **Never a
silent ignore** — that is the lie STYLE.md Tier 2 rule 3 rejects.

So: **a host-mode job type on macOS must not declare `resources.cpu` or
`resources.memory`.** `resources.task_timeout` is unaffected and still works
everywhere — it is enforced dispatcher-side by the §3.5 timeout scan, not by the
runtime, which is exactly why it remains the bound that actually rescues a
wedged task.

Two macOS-specific notes on top of #309:

- The default when the capability is **absent must stay `true`**. Every job type
  in `.chug/jobs/` except `manual` declares both `resources.cpu` and
  `resources.memory`; a `false` default would filter the entire
  container fleet during a rollout and escalate everything with
  `no_free_slots_timeout` 30 minutes later.
- A Mac node may still **protect itself** without the platform pretending to:
  wrapping the task in `nice`/`taskpolicy -c utility` is a node property in
  exactly the sense `cache_dir` is — worker-side, off the wire, invisible to the
  dispatcher. It is not a limit, it is not advertised as one, and it matters most
  if a Mac ever co-hosts the dispatcher (§[6](#6-node-provisioning-and-the-self-refresh-collision)).

### Migration and skew, spelled out

1. **Bump `CONFIG_SCHEMA_EPOCH` 2 → 3 in the same commit as the parser
   change**, and add a frozen `RUNTIME_SCHEMA_EPOCH = 3` beside
   `INPUTS_SCHEMA_EPOCH` (`crates/types/src/version.rs`), so a later unrelated
   bump does not retroactively raise what an existing `runtime:` config must
   declare. This is the precedent `inputs:` set, and it exists because
   `min_dispatcher` is author-declared.
2. **`validate()` gains two rules.** `runtime.mode: host` requires
   `min_dispatcher >= RUNTIME_SCHEMA_EPOCH`, reported as an ordinary
   `FieldRuleError::Required` — one line, and it makes the gate structural
   instead of a thing an author remembers (`inputs:` is enforced this way
   already). And, per §[2](#2-workspace-as-a-virtual-wire-path),
   `runtime.mode: host` requires `work.type: command`, as a
   `FieldRuleError::Invalid` on `work.type` until the agent transcript path is
   computed rather than measured.
3. **The version-skew gate then does the rest.** `.chug/tasks/ci.sh` runs
   `chuggernaut validate --deployed-epoch` against the *deployed* dispatcher
   (§14.1/§14.3), so a `runtime:` config merged before the dispatcher that
   understands it fails **its own CI** with "requires dispatcher >= 3; deploy
   first or gate it" rather than merging a time bomb. Order of operations:
   land + deploy the dispatcher at epoch 3 first, then merge the job type.
4. **Existing configs are untouched.** `runtime` absent, or `mode: container`,
   is today's rule set byte-for-byte: `image` required for agent/command,
   disallowed for human.
5. **The field-rules matrix in `spec.md` §1.1 gains a host column**, §4.1 gains
   the sentence that `/workspace` is a *logical* path the backend may map, and
   §3.1 gains the host node kind. Also fix the stale inline `ContainerBackend`
   listing in §3.1 while there (it omits `logs`, `logs_tail`, and all six
   provided methods).
6. **Per-level `image` still resolves that level to container mode**, per
   [#309 §3's precedence rule](./309-host-native-execution.md#coexistence-on-a-mixed-fleet).
   This is what lets the appended `ci` evaluator — which declares `image:
   chuggernaut/agent-rust:prod` in `.chug/jobs/_defaults.yaml` — stay a
   container task on a host-mode job type with no special-casing. **A
   consequence worth naming for iOS: a project with host-mode job types still
   needs a container node in its fleet**, or that appended gate has nowhere to
   run. A Mac-only fleet is not a supported shape.

## 4. Is `ContainerBackend` the right seam?

**Yes — and the trait should not be reshaped first.** The blunt version: it is
the wrong *name* and the right *seam*.

The evidence for the seam: `handle()` in `crates/worker/src/daemon.rs` already
routes launch / kill / inspect / copy_file / logs / logs_tail / remove /
list_exited / list_running through `state.backend` with no container-specific
logic in the routing, and the trait's own contracts — `NoCapacity` as transient,
`logs_tail` offset monotonicity, `remove` idempotence — are what the
dispatcher's behavior is written against. A host backend that satisfies the
trait satisfies those *by construction*; an enum arm satisfies them by review.

The evidence for the name being wrong, which the brief collects accurately:
`remove()` is documented purely in Docker terms ("reclaiming its writable
overlay layer"), `logs()` carries Docker's cross-stream-ordering caveat,
`ContainerId` is an opaque string `FleetBackend` formats as `{node}/{docker_id}`,
and `k8s.rs` is a stub — so there has never been a second real implementation.

Where a host backend lands on each awkward method:

| Method | Host reading | Verdict |
| --- | --- | --- |
| `remove` | delete the task dir; the doc comment's *rationale* is Docker-specific, its *contract* (idempotent, best-effort, reclaims bulk storage) is not | reword the comment, keep the method |
| `logs` | one merged fd, so ordering is **better** than Docker's — the caveat becomes vacuous, not violated | strictly better |
| `logs_tail` | byte offsets into an append-only file; `LogTail::slice` (`crates/container/src/lib.rs`) is a pure function over a byte buffer and is reusable **verbatim** against the captured file | strictly better |
| `ContainerId` | `{node}/{task_id}` keeps the existing prefix routing unchanged | free |
| `image` | the one field with no host meaning — §[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode) | the real cost |

So the required changes are: `managed_running_total` lifted onto the trait as a
provided method (it is an **inherent** `DockerBackend` method today, called from
the `ping` handler, so a `dyn` field alone does not compile);
`WorkerState.backend` becomes `Arc<dyn ContainerBackend>`; and construction moves
into one function that owns the per-variant wiring, because `with_cache_dir` and
`ping_all` are inherent too. That is #309 §1's Option A, and it is the smaller
half of this work.

**Do not rename the trait as part of this job.** A rename touches every
implementation and every test in the same commits that introduce a new backend,
and the trait's *contracts* are what matter for correctness. Reshaping it before
there is a second real implementation would be designing against a guess; the
prototype is what tells us which parts of the vocabulary actually hurt. Revisit
the naming once a host backend exists and has run real work — that is a
follow-on `docs` job, not a prerequisite.

## 5. iOS specifics

### Slots: **1**, then 2 only after per-task device sets are proven

Recommend `WORKER_SLOTS=1` on a Mac node. Three reasons converge on the same
number, which is what makes it cheap:

- CoreSimulator device state is effectively global per user session; parallel
  runs contend over it.
- The per-task device set (below) is a hypothesis with a test attached, not a
  measured isolation guarantee — one slot is what makes being wrong about it
  cheap.
- Xcode builds are memory-hungry and the platform cannot bound them on macOS
  (§[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode)).

Note what is *not* on that list: the `/workspace` rebase, which
§[2](#2-workspace-as-a-virtual-wire-path) lands in the same phase as the backend
because macOS gives it no choice. Slots are about device and memory contention
only.

Two is defensible once per-task device sets are proven, and — because
design #293 has landed in the wire types — moving 1 → 2 is now a runtime
operator action (`set_slots`, §3.1), not a redeploy. Beyond 2, the honest
answer is
[#309 §5b](./309-host-native-execution.md#5b-exclusive-resources-device-leases)'s
device lease, which is explicitly deferred until a Mac must run a second,
non-device-bound task concurrently.

### Per-task device strategy: a private device set, not a shared named device

The tempting shape — a long-lived `chug-sim` device the job boots and reuses —
is what #308 H.4 describes GHA doing, and it is durable node state nobody owns.
Recommended instead:

```sh
# in the project's own task script, not in platform code
DEVSET="$CHUG_TASK_DIR/devices"
UDID=$(xcrun simctl --set "$DEVSET" create "chug-$CHUG_TASK" \
        "iPhone 16" "com.apple.CoreSimulator.SimRuntime.iOS-18-4")
xcrun simctl --set "$DEVSET" boot "$UDID"
xcodebuild test -scheme App -derivedDataPath "$CHUG_TASK_DIR/dd" \
        -destination "id=$UDID"
```

Four points, in descending order of how much trouble they save:

- **`simctl --set <path>` gives the task its own device set**, so two tasks
  cannot see each other's devices at all. This is the closest macOS analogue of
  a per-task namespace available without a VM.
- **A uniquely-named, per-task device, deleted on exit.** No cross-run device
  reuse means no "created only if absent" logic that silently re-downloads a
  system image forever in one environment and not another.
- **`-derivedDataPath` into the task dir.** The default
  `~/Library/Developer/Xcode/DerivedData` is shared global state *and* a lock;
  per-task is correct even at 1 slot. The legitimate desire to *cache* across
  tasks is a declared-cache question
  ([#309 §9](./309-host-native-execution.md#9-environment-and-state)), not a
  reason to share DerivedData.
- **`simctl … delete all` is only ever safe scoped to the task's own `--set`.**
  A global `shutdown all` / `delete all` — a common CI incantation — would
  destroy a concurrent task's device. Make this a rule in the job-type docs,
  because it is the single most likely way a second slot breaks.

**Platform-side, the guarantee is teardown, not simulator logic.** `simctl`
calls belong in the project's script for the same reason toolchain versions do
(#308 H.6: toolchains *are* job-type config, project-owned and repo-versioned).
What the daemon owes is a **bounded** teardown on `kill`/`remove` and on startup
recovery: signal the process group (SIGTERM, then SIGKILL after a grace window —
the exact pattern `kill_process_group` already implements for the refresh
script in `crates/worker/src/daemon.rs`), then, if `{task_dir}/devices` exists,
`xcrun simctl --set {task_dir}/devices shutdown all` and `delete all` before
removing the dir. Deleting the directory alone can leave `CoreSimulatorService`
holding stale registrations. **Flagged as unverified:** that last point is
domain knowledge, not a measurement — the prototype must confirm it on a real
machine, and if `simctl` teardown proves unnecessary, drop it rather than keep
a superstition.

Two macOS holes to state rather than imply a boundary that is not there: a task
that calls `setsid()` escapes its process group and survives (Linux recovers
this with a transient systemd scope; macOS has no equivalent), and CoreSimulator
processes are children of the per-user `CoreSimulatorService`, not of the task —
so a killed task can leave a running simulator that only the `--set`-scoped
teardown reclaims.

### Secrets and users: where #309 §8 does not port

Design #309 §8 recommends a **fixed pool of per-task unix users**, and makes the
daemon refuse to advertise `host` unless the pool is provisioned. On macOS that
recommendation collides with the runtime:

- CoreSimulator is per-user-session; each task user gets its own
  `CoreSimulatorService` and device set — good for isolation, but it needs a
  real, active session per user.
- Signing identities live in a specific user's keychain, and an unlocked
  keychain is a session property.
- macOS has no `systemd-run --uid=`; the equivalents (`launchctl asuser`, a
  per-user launchd domain) require that session to exist.

**Recommendation for macOS: one dedicated task user with a login session, and
the node declared single-tenant** — #309 §10's `WORKER_HOST_PROJECTS`, with a
host launch for any other `owner/project` a hard `BackendError::Launch` (it can
never clear without a config change, so queueing it would be a 30-minute
silence with a known answer). The cross-task secret boundary is therefore
**absent** on a Mac; what bounds exposure is single-tenancy, exit-time secret
deletion, and short credential TTLs
(§[2](#2-workspace-as-a-virtual-wire-path)). Per-task users on macOS are a
later question, and one that should be answered by measuring whether
CoreSimulator works in a non-Aqua session at all.

Also inherited from #309 §10 and worth restating because a Mac is likely to be
running colima: **host tasks do not get the docker socket.** A host task that
can reach it is effectively root on the node.

### Signing: out of scope for phase 1, and deliberately

Simulator builds need no signing identity — simulator binaries are ad-hoc
signed and `CODE_SIGNING_ALLOWED=NO` covers the rest. Device builds, archives,
notarization, and App Store upload all need a keychain, a provisioning profile,
and an Apple ID, each of which is per-user session state with its own secret
shape. Scoping phase 1 to **simulator work only** removes the hardest
provisioning problem from the critical path while still delivering the category
that motivated the design (tests that cannot run on Linux at all). Signing gets
its own design once host mode has run real work; guessing its shape now would be
the deferred debt STYLE.md Tier 3 rejects.

## 6. Node provisioning and the self-refresh collision

**What the platform does not manage today, and will not manage in phase 1:**
the macOS version, installed Xcode(s) and their simulator runtimes, Command Line
Tools, the accepted Xcode license (`xcodebuild -runFirstLaunch` needs root
once), Homebrew tools, the task user and its login session, signing identities,
`WORKER_HOST_ROOT` (a task-user-owned directory such as `/Users/chug/tasks` —
**never** a root-level path, §[2](#2-workspace-as-a-virtual-wire-path); nothing
here needs an `/etc/synthetic.conf` entry, and needing one would be the signal
that the rebase regressed), and the `chuggernaut worker` launchd agent plus its
node-local `chuggernaut-channel` copy. That is a runbook, in the shape of
`deploy/prod/README.md`'s existing node sections — not platform machinery.

The platform's contribution is to make the unmanaged parts **legible and
fail-loud**: the node discovers and advertises its Xcodes as env refs
(§[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode)),
a launch for an env the node lacks fails hard naming what is installed, and the
task records its resolved toolchain versions in its own captured stdout.

### The self-refresh collision, which is the real finding here

`spec.md` §3.1's self-refresh is written around a **containerized** daemon: on
`refresh { sha, tag }` the node builds three images locally and a **detached
sibling** does `docker rm -f chug-worker` + `docker run` of the new image
(`deploy/prod/worker-refresh.sh`). The §3.1 **drain guarantee** depends on a
specific fact — `docker rm -f` hits only the daemon container, so *in-flight job
containers survive*, and the dispatcher's poll-based `wait` re-attaches over the
new daemon.

On a native Mac node that fact does not hold:

- There are no job images to build. The node-local artifacts are the
  `chuggernaut` binary and `chuggernaut-channel`, built from source.
- The daemon is a **launchd** agent, and `launchctl bootout` / `kickstart -k`
  takes down the job's processes. **A refresh would kill every running host
  task** — the exact opposite of the drain guarantee.

Two mitigations, and the phasing between them matters:

1. **Phase 1 (recommended, zero new mechanism): refuse to refresh while a host
   task is running.** The daemon already has a drain gate for in-flight
   *launches* (`DRAIN_TIMEOUT` in `crates/worker/src/daemon.rs`), but that gate
   is about the launch handshake, not about running work. Extend the refresh
   precondition: if the registry reports any running host task, the daemon
   **declines the refresh with that reason** rather than swapping. Deploys then
   drain the Mac first — set `slots: 0` via `set_slots` (which per §3.1 is
   already operator intent, not "unhealthy", and kills nothing: `probe_worker`
   computes `free = slots − running` and `choose_placement` skips `free <= 0`),
   wait for `occupied` to reach zero in `fleet.status`, then refresh. Failing
   fast and loud beats silently killing a 40-minute build, and it needs one
   precondition check.
2. **Later: each host task in its own launchd job**, bootstrapped into the task
   user's domain so the daemon can be replaced under running work. That is the
   macOS analogue of #309 §6's transient systemd scope and it restores the §3.1
   guarantee properly. It is more machinery than phase 1 deserves.

The reusable half of the deployment story already exists and should be used
rather than reinvented: the Mini runs the **dispatcher and api natively under
launchd** today via `deploy/prod/install-launchd.sh` and the templates in
`deploy/prod/launchd/`. A native worker is a fourth plist of the same shape.

### Where the Mac should be

Prefer a **dedicated Mac**, joined as a worker node. If the Mini itself must
serve host work, note what §6 of `deploy/prod/README.md` records: job containers
were moved *off* the Mini precisely so heavy builds could not starve the control
plane, and its colima node sits at 0 slots. An unbounded `xcodebuild` co-tenant
with the dispatcher and api reintroduces that starvation with no cgroup to stop
it — mitigable only by `nice`/`taskpolicy` and a slot count of 1. That is a
risk to accept knowingly, not a default.

## 7. What the recommendation gives up

Stated plainly, because a design that only lists what it buys is not honest:

- **The isolation boundary.** Project code runs on a real Mac as a real user
  with a real home directory. Nothing short of a VM per task changes this, and a
  VM per task is what makes the toolchain unavailable in the first place. The
  cache reuse *is* the win and *is* the contamination risk — the same property,
  as #308 H.3 cost 4 says.
- **Enforceable `cpu`/`memory`.** Refused rather than ignored, which is the best
  available answer and still a worse product than a container node.
- **Cross-task secret isolation on macOS**, per §[5](#5-ios-specifics).
- **A pinned, content-addressed environment.** `xcode:16.4` is a node-resolved
  version string, not a hash. Recorded, not pinned.
- **Agent work on the Mac in phase 1**, per §[2](#2-workspace-as-a-virtual-wire-path).
- **The drain guarantee during a refresh**, until per-task launchd jobs land;
  traded for a loud refusal in the meantime (§[6](#6-node-provisioning-and-the-self-refresh-collision)).
- **A new durable-state component to get wrong.** §[1](#1-the-durable-task-registry)
  is the piece with no precedent in this tree, and its bad failure — reaping a
  live build, or reporting a dead one as running until `task_timeout` — is worse
  than anything the container path can do.

## Phased implementation sketch

Worker-local work and normative-doc work are separated deliberately: phases W
touch only the worker/container crates and one prototype node, phases N change
`spec.md` and the config schema. **W1–W3 need no spec change, no epoch bump, and
no dispatcher behavior change** — which is what makes the prototype cheap. The
one edit they make to shared code is `bootstrap_cmd`'s `${CHUG_WORKSPACE:-…}`
default, which is a no-op for every container task and is why folding the rebase
into W2 does not cost the prototype its cheapness.

| Phase | Kind | Work | Depends on |
| --- | --- | --- | --- |
| **W1** | `code` | Backend polymorphism: `managed_running_total` lifted onto `ContainerBackend` as a provided method; `WorkerState.backend` becomes `Arc<dyn ContainerBackend>`; one construction function owning the `with_cache_dir`/`ping_all` wiring; `WORKER_MODES` parsing in `crates/worker/src/config.rs` beside `parse_slots`/`parse_cache_dir` | — |
| **W2** | `code` | The host backend, **including the rebase** — it is a precondition of the first launch, not hardening (§[2](#2-workspace-as-a-virtual-wire-path)): task dir under `WORKER_HOST_ROOT`, wrapper-written `exit_code`, the liveness ladder, restart recovery with the bounded startup scan, fail-loud listings (§[1](#1-the-durable-task-registry)); `CHUG_WORKSPACE` indirection in `bootstrap_cmd`, total `/workspace` + `/chuggernaut` mapping over all four surfaces with a hard error on anything unmapped, exit-time deletion of the mapped `chuggernaut/` credential tree, and the agent-shaped-launch refusal. One Mac at `slots: 1`, routed by `placement.node`, `WORKER_MODES=container,host`. The job type still declares `image:` and the node ignores it — **a deliberate lie that must never leave the prototype node**, and the one prototype-only lie in this phase; N1/N2 exist to remove it | W1 |
| **W3** | `code` | macOS hardening, once W2 runs a real build: symlink-resolution containment and its rejection cases; `simctl`-scoped teardown on kill/remove/recovery, including orphan device sets found by the startup scan (§[5](#5-ios-specifics)); the local retention sweep; optional `nice`/`taskpolicy` self-protection | W2 |
| **N1** | `docs` | `spec.md`: §1.1 host column in the field-rules matrix + the `runtime:` block, §3.1 host node kind and the stale trait listing, §4.1 `/workspace` as a logical path, Appendix: Deferred points here instead of staying open. `crates.md` container row | W3 |
| **N2** | `code` | The schema: `runtime: { mode, env }` with nested `deny_unknown_fields`, the field rules and the per-level precedence rule, `CONFIG_SCHEMA_EPOCH` 2 → 3 + frozen `RUNTIME_SCHEMA_EPOCH`, and both validate rules — `min_dispatcher >= 3`, and `mode: host` requires `work.type: command` (§[2](#2-workspace-as-a-virtual-wire-path), §[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode)) | N1 |
| **W4** | `code` | Node-side env-ref resolution: Xcode discovery, `xcode:<version>` → `DEVELOPER_DIR`, hard failure naming what is installed | N2 |
| **W5** | `code` | Refresh precondition: decline a refresh while any host task runs, with the reason (§[6](#6-node-provisioning-and-the-self-refresh-collision)) | W2 |
| **N3** | `docs` | The macOS node runbook in `deploy/prod/README.md`: Xcode, task user + session, launchd plist, the `simctl --set` and no-global-`delete all` rules, the drain-before-refresh procedure | W5 |
| **P1** | `code` | `NodeCapabilities` on `PingOk`/`WorkerAnnounce` (`modes`, `platform`, `envs`, `resources_enforced`) and the `choose_placement` predicate — [#309 §4](./309-host-native-execution.md#4-capability-advertisement)/§5a verbatim; unpins host work and lets `resources` be refused rather than pinned around | N2 |
| **P2** | later | Per-task launchd jobs (restores the refresh drain guarantee); agent work on a Mac (computed transcript path, node-provisioned CLI); device leases ([#309 §5b](./309-host-native-execution.md#5b-exclusive-resources-device-leases)); signing | P1 |

**W2 is the phase to start**, and it should be proven against a **boring
`xcodebuild test` on a simulator** — not against a full fastlane release. #308
H.4 is right that the flutter/fastlane workflows sit at the confluence of too
many unbuilt things; what W2 needs to answer is which of the ten trait methods
is actually hard.

Test placement per [testing.md](../../testing.md): the `WORKER_MODES` parse, the
rebase rule (including the rejection cases), the liveness ladder as a pure
function over a synthetic task dir, the `runtime:` field rules, and the
`choose_placement` predicate are all pure → **tier 1**, beside the existing
`parse_slots` tests in `crates/worker/src/config.rs` and the `choose_placement`
tests in `crates/container/src/lib.rs`. The host backend's launch → inspect →
`logs_tail` → remove round trip, the restart-recovery scan, and the daemon's
mode routing are **tier 2** (no Docker needed, which makes them cheap). The
synthesized-verdict-on-reboot path deserves a named regression test: it is the
one whose failure mode is a hung task.

## Contracts changed

Per STYLE.md's contract-first rule, each slice names the contract it changes:

| Slice | Contract |
| --- | --- |
| W1 | `ContainerBackend` trait surface (`managed_running_total` becomes provided); `WorkerConfig` gains `modes` |
| W2 | New backend implementation; `ContainerId` shape unchanged (`{node}/{task_id}`); **`list_managed_exited`/`list_managed_running` postcondition strengthened: never a partial `Ok`**; new invariant — a task dir never transitions out of having an `exit_code`, and the daemon is its only writer; `bootstrap_cmd` output (clone destination becomes `${CHUG_WORKSPACE:-/workspace}` — unchanged when unset); a new **total** wire-path mapping contract with no fall-through, over paths *and* env values; credential lifetime — secrets deleted at process exit, not at `remove`; host launches refuse agent shape |
| W3 | Mapping containment strengthened to post-normalization (symlink escape); `kill`/`remove` postcondition gains bounded device-set teardown |
| N1 | `spec.md` §1.1 field-rules matrix, §3.1 node kinds and trait listing, §4.1 workspace bootstrap |
| N2 | Job-type schema epoch (§14.1): `CONFIG_SCHEMA_EPOCH` 2 → 3, `RUNTIME_SCHEMA_EPOCH` = 3, `JobType::validate` rules, per-level `image`/`runtime` precedence |
| W4 | `runtime.env` scheme registry (`xcode:<version>`); launch fails hard on an unresolvable ref |
| W5 | `refresh` precondition (declines while host tasks run) — a narrowing of §3.1's refresh contract |
| P1 | Two wire records, additively (`PingOk`, `WorkerAnnounce`); `choose_placement` postcondition |

## What this makes wrong elsewhere

- **`spec.md` Appendix: Deferred** — "macOS bare metal dispatchers … Execution
  model needs separate design" should point at this document and
  [#309](./309-host-native-execution.md) instead of staying open-ended. (Note
  the entry says *dispatchers*; what this design adds is a macOS **worker**, and
  the dispatcher stays where it is.)
- **`crates.md`'s `container` row** reads "Docker and k8s implementations"; k8s
  is a stub and a host backend makes the row wrong a second way.
- **`crates/container/src/lib.rs`'s module doc** ("Docker socket in dev, the
  Kubernetes Jobs API in production") describes a deployment that does not exist.
- **`crates/container/src/lib.rs`'s `remove` doc comment** explains itself in
  terms of a writable overlay layer; the contract survives, the rationale needs a
  second sentence.
- **[#309](./309-host-native-execution.md) itself** is stale on two points now
  fixed here: the epoch claim (it read 2 when this was written; job #401 landed
  the `runtime:` block at 4, so there is no bump left to spend), and
  `runtime.env`'s nix-only scheme does not cover the category that motivated the
  design — §3's `xcode:<version>` is a `validate()` rule in the tree today,
  requiring `mode: host`.

## Risks and open questions

- **`simctl` teardown semantics are asserted, not measured.** Whether deleting a
  `--set` directory leaves `CoreSimulatorService` holding stale registrations —
  and whether `simctl --set` isolates as completely as claimed — must be
  confirmed on a real Mac in W2. Treat §[5](#5-ios-specifics) as a hypothesis
  with a test attached.
- **Whether CoreSimulator works at all in a non-Aqua session** decides whether
  per-task users are ever available on macOS, and therefore whether the secret
  boundary in §[5](#5-ios-specifics) can be recovered. Unknown; measure before
  designing.
- **Nobody in this tree has built a non-container backend.** The *count* of host
  analogues is knowable from the trait; their *difficulty* is not. That
  asymmetry is why W2 is a prototype and why this document declines to
  pre-commit past P1.
- **The wrapper-writes-`exit_code` design has one hole:** a `SIGKILL` to the
  wrapper itself (OOM, `kill -9`, power loss) leaves no `exit_code`, which is
  exactly what the liveness ladder's step 3 exists to resolve. The ladder must
  be correct or this whole section is theatre — hence the named regression test.
- **`.chug/tasks/ci.sh` needs a `nats-server` or a Docker daemon** for tier-2
  tests and self-skips otherwise (its tier-summary logic exists to make the skip
  loud). Any Mac node that runs *this repo's* CI must supply one, or the gate
  goes partial. Not a phase-1 problem — this repo's job types stay container
  mode — but it is a trap for the first consumer that tries to run its own Rust
  CI on a Mac.
- **A host-mode project still needs a container node** for the appended `ci`
  evaluator (§[3](#3-image-resources-and-what-runtimeenv-means-when-the-toolchain-is-xcode)).
  A Mac-only fleet is not a supported shape, and nothing currently says so out
  loud.
