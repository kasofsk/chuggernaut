# Design #373 — Project-supplied toolchains: nix environments in container mode (clock 3)

Status: FINDING, amended 2026-08-02 — clock 3 in container mode on dedicated nodes; image+env layer; P1 shipped, 45s cap (C6).

Written against the tree at `5aeb439` (this branch adds only this document).
Every claim about this repository was read out of the source or out of
[`spec.md`](../../spec.md). Every claim about node behavior was **measured on
`gumbo-nuc-0` on 2026-08-02** and the commands are given, so a reader can re-run
them rather than trust them. Where a sibling design and a measurement disagree,
the measurement wins and the disagreement is recorded in
[Corrections](#corrections). This revision answers review findings on the first
draft; the substantive changes are
[Decision 3](#decision-3--resolution-and-the-realise-site) (new),
[Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism)
(rewritten) and [Weighed against #355](#weighed-against-355--when-a-project-picks-which)
(new).

This document was **claimed and written by the operator**, not delegated: the
decisions below were worked through directly.

**Amended after P1 merged** (job #387). Job #384 shipped [P1](#sequencing) at `2bd4bf3`, and
implementing it settled three things this document had guessed and one it had
priced wrong. The bound in
[3c](#3c-the-realise-is-bounded-and-it-is-outside-task_timeout) is not a free
parameter — it has a **45-second ceiling**, which turns
[Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism)'s
warming job from an optimization into a precondition
([C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization)).
The three smaller divergences are recorded in
[P1 as shipped](#p1-as-shipped-job-384). Everything in that section is read out
of the merged tree; where it and the argument above it disagree, the merged tree
wins.

## The question

[#308](308-gha-port.md) §H.6 sorts environments onto **three clocks**, by who
owns a thing and what ritual changing it costs:

| Clock | Owner | Holds | Verb |
| --- | --- | --- | --- |
| 1. System closure | operator | machine facts: docker daemon, nix daemon and caches, `/dev/kvm`, users, the worker unit | `nixos-rebuild` + a drain |
| 2. Worker daemon | platform | the `chug-worker` binary | `worker-refresh.sh` |
| **3. Task environment** | **project repo** | **the toolchain a job runs against** | **`git push` — no deploy** |
| 4. Declared mutable caches | — | `~/.gradle`, sccache, buildx, AVDs | no clock |

§H.6 is emphatic that per-project toolchains belong to clock 3: putting flutter's
version in a node's NixOS config "rebuilds the image problem with worse
ergonomics, and makes the node a **central control plane** — which CLAUDE.md
rejects outright". The property it is protecting is stated in the same
paragraph: "a tool bump ships in the same commit as the code that needs it,
gated by the same CI."

[#309](309-host-native-execution.md) §9 gives clock 3 exactly one mechanism —
`runtime.env: "nix:<flake-ref>#<attr>"` — and makes it reachable **only in host
mode**, because `runtime.mode: host` is what the selector switches on.

So: **can clock 3 be served in container mode?** If it can, host-native
execution's motivation narrows sharply, and a project gets a repo-versioned
toolchain without any of #309's ten trait analogues.

## The measurement that reopened it

On `gumbo-nuc-0`, against the real prod agent image:

```sh
docker run --rm --device /dev/kvm -v /nix/store:/nix/store:ro \
  -e ANDROID_SDK_ROOT=/nix/store/3zr1pgw…-androidsdk/libexec/android-sdk \
  chuggernaut/agent:prod  …
```

- `emulator -accel-check` → exit 0, "KVM (version 12) is installed and usable."
- An AVD was created and booted headless: `adb devices` → `emulator-5554 device`,
  `Boot completed in 30421 ms`, ~40s wall clock.
- The image is **Debian 12, entirely unmodified**. `java` is not on its `PATH`,
  yet `avdmanager` ran — the nix wrappers resolve their own JDK out of the store.

A container consumed a nix environment. #309 §9's assumption that a nix
environment implies host mode is false.

**And the run was doctrinally wrong even though it worked.** The SDK came from
`gumbo-nuc-0`'s `configuration.nix` — a *different project's* repo — so clock 1
supplied what clock 3 should own. A Flutter bump would need a `nixos-rebuild`
and a fleet drain, and would change the environment for every tenant of that
box. That is the central control plane #308 §H.6 rejects, arrived at by
accident. The doctrine-conformant version is a **project-repo flake realised on
the node**; `androidenv.composeAndroidPackages` is an ordinary nixpkgs function
usable from any flake, so nothing blocks it in principle.

### The store-mount premise is settled in this tree

The measurement mounts `/nix/store` whole, and so — as of `5aeb439` — does the
corpus. [#367](367-android-emulator-execution.md)'s correction 9 struck its own
**T2** (a named `WORKER_ANDROID_SDK_DIR` at `/opt/android-sdk`) as dead on the
grounds this document would otherwise have had to re-derive: a nix-provisioned
SDK "is not a directory — it is a *view into a store*", and binding the view
without the store yields dangling symlinks and a missing interpreter. The
surviving recommended form in §3.2 is "**`/nix/store` bind-mounted read-only at
`/nix/store`**", §3.4 argues its exposure, and the brief's amendment is
therefore **merged, not pending**.

That settles this document's premise instead of conditioning it. A container
seeing `/nix/store` is the corpus's recommended shape today, which is what lets
[Decision 4](#decision-4--gc-roots-an-explicit-indirect-root-per-task-and-no-assertion)
treat an unrooted store path held by a running task as a live hazard rather than
a hypothetical one. What #367 does **not** settle is whose clock fills that
store: it fills it from clock 1, by `nixos-rebuild`. That is the whole of this
document's remaining problem.

## Decision 1 — tenancy: dedicated nodes only

**Clock 3 is available in container mode on nodes dedicated to the project.
Multi-tenant nodes keep image-only task environments.**

The constraint is not about the container; it is about **evaluation**. Realising
a project's flake runs project-controlled code on the node, in the platform's
own trust domain, *before any container the task will run in exists* — see
[Decision 3](#decision-3--resolution-and-the-realise-site) for exactly where.
Container mode does not fix that: the task is contained, the evaluation is not.
[#309](309-host-native-execution.md) §10 already made host mode single-tenant by
policy (`WORKER_HOST_PROJECTS`) for this reason, and the reason transfers
unchanged.

This is a smaller claim than "nix works in containers", and it is the honest
one. It is also close to what already exists: `gumbo-nuc-0` is effectively
dedicated hardware today.

**It also closes the brief's question about serving many projects.** The brief
asks whether a read-only whole-store mount — which hands a container every
package on the node — gets worse when many projects are served. Taking
[#367](367-android-emulator-execution.md)'s amendment as input rather than
re-deriving it: the answer is that this design never creates that situation. A
node carrying clock-3 environments has **one** tenant, so "every package on the
node" is that tenant's own toolchain plus the operator's machine facts. The
exposure that would matter — project A reading project B's realised closure — is
excluded by construction, not by a mount flag. Relaxing Decision 1 reopens it,
which is why it is stated as a rule and not a deployment habit.

## Decision 2 — schema: `container + image + env`, one cell

**In host mode `image` and a flake ref occupy the same slot. In container mode
they layer.**

- **`image`** is the base userland the task runs *in* — libc, shell, coreutils,
  and the `git` that `container::bootstrap_cmd` clones with.
- **`env`** is the toolchain the task runs *with*.

The measurement is the argument: Debian supplied the userland, nix supplied the
emulator, NDK and JDK. They did not compete.

Resulting field rules:

| `runtime.mode` | `image` | `runtime.env` |
| --- | --- | --- |
| `container` (default) | **required** (unchanged) | **optional** — the only change |
| `host` | disallowed | required |

`image` stays required in container mode: a container always needs a root
filesystem, and `container + env + no image` is not coherent. So **every
existing job type parses identically**, and this costs **no epoch bump beyond
the one [#309](309-host-native-execution.md) slice 4 already spends** — it makes
that bump buy three modes instead of two. `CONFIG_SCHEMA_EPOCH` is 2 today
(`crates/types/src/version.rs`); `image` is `Required` for `WorkType::Agent` and
`WorkType::Command` in `JobType::validate` (`crates/types/src/job_type.rs`), and
neither rule moves.

Four rules fall out:

1. **Scheme validity depends on mode.** `nix:<flake-ref>#<attr>` is legal in
   both. **`xcode:<version>` requires `mode: host`** — Xcode cannot be
   containerized at all — so this is a `validate()` rule, not a runtime failure.
   That keeps [#322](322-macos-native-runtime.md) §3's exception explicit rather
   than implicit.
2. **The bootstrap uses the image; the task uses the env.** `bootstrap_cmd`
   (`crates/container/src/lib.rs`) clones inside the container, before the
   task command runs, using the image's `git`. It never needs the env, and the
   env is never realised from the checkout — see
   [Decision 3](#decision-3--resolution-and-the-realise-site), which is where
   that ordering is actually resolved.
3. **Container-mode `env` is gated node-side, not by job type** — a
   `WORKER_NIX_PROJECTS`-shaped allow-list, exactly as #309 §10 requires for the
   docker socket: "a node-side allow-list entry, never a job-type field the
   platform honors on request." A job type asks for an *environment*; it never
   asks for a *privilege*.
4. **N−1 fails safe for free.** An N−1 dispatcher does not know `runtime` at
   all, and `deny_unknown_fields` on the nested block parks the config Stalled
   per §14.2 — one park, detected and explained, rather than retries burned per
   job. That is the property #309 §3 chose the nested block for, and it covers
   this addition at no extra cost.

Honest residual: nix closures carry their own glibc — which is why a nix wrapper
ran on Debian — so the env is largely self-contained and image/env skew is
mostly a non-issue. A project *can* still write a task script that assumes the
image's tooling and gets the env's. That is the same class of confusion as a
wrong `image` today, it is the project's to get right, and it fails loudly.

## Decision 3 — resolution and the realise site

This is the decision the first draft asserted rather than made, and both halves
of it turn out to bite.

### 3a. A relative ref has no checkout to resolve against

[#309](309-host-native-execution.md) §9 defines the relative form `nix:.#chug-ci`
as resolving against the **job branch checkout**, and gives host mode the
ordering **clone → realise → exec**, all host-side. In container mode that
checkout does not exist on the host at any point: `bootstrap_cmd`
(`crates/container/src/lib.rs`) runs `git clone … /workspace` *inside* the
container, at task start. So the worker has nothing to realise against.

This is load-bearing, not a detail. The relative ref is #308 §H.6's whole point
— a tool bump shipping in the same commit as the code that needs it. A design
that can only carry absolute refs still serves clock 3, but loses the property
the three-clock model was drawn to protect.

**R1 — absolute refs only in container mode.** A `validate()` rule rejects
`nix:.#…` under `mode: container`. Cheapest by a wide margin and honest about
what it gives up: a toolchain bump becomes a two-commit dance — merge the flake
change, then bump the pinned ref in the job type — which is the image problem
with a different noun. Rejected on that ground alone.

**R2 — a host-side clone before launch.** The worker clones the job branch to a
scratch dir so it can realise `.#attr`, then launches. It contradicts "the
bootstrap uses the image", adds a second clone of every job, and puts project
code on the host earlier than anything else does. Rejected: it buys nothing R3
does not, at strictly higher cost.

**R3 — the worker rewrites the relative ref into a remote flake ref
(recommended).** nix's git fetcher takes a repository URL and a ref directly; it
does not need a working tree. The worker already holds both parts, because they
are what it injects into the container for the clone: `REPO_URL` and
`JOB_BRANCH` are both built in `Core::container_env`
(`crates/dispatcher/src/exec.rs`). So `nix:.#chug-ci` resolves to
`git+{REPO_URL}?ref={JOB_BRANCH}#chug-ci` — the flake comes from the job branch,
the same-commit property survives, and no checkout is required on either side.

**R3's honest gap, and the contract that closes it.** `?ref=` names a branch,
not a commit. The launch env carries no commit sha (`Core::container_env` has
`JOB_BRANCH` and no `JOB_SHA`), so a push landing between the worker's realise
and the container's clone would give a task a toolchain realised from the
previous tip. The window is seconds and the failure is a stale toolchain rather
than a wrong one, but it is a real race and the fix is small: **carry the job
branch's commit on the launch spec** and use `?rev=`. Per STYLE.md's
contract-first rule that is a named contract addition, sequenced with
[P2](#sequencing), not something to leave implicit.

### 3b. Where the realise runs

Nothing on this node runs `nix` today where it would need to. `chug-worker` is
itself a container — `deploy/prod/build-worker.sh`'s `docker run` gives it the
docker socket and a `:ro` key mount and nothing else — so "the worker
realises the flake" is not free.

**W1 — the worker container invokes the node's nix daemon (recommended).** Give
the worker container `/nix/store:ro` and the nix daemon socket, and take the
client binary *out of the mounted store* by absolute path — the same trick the
measurement proved for `avdmanager`. The worker image gains nothing; the two
mounts are the whole change, and they are the shape `with_cache_dir`
(`crates/container/src/docker.rs`) already establishes for adding a bind. Version
skew is impossible because the client is the daemon's own binary.

> **As shipped, W1 costs four mounts — plus a fifth when the node attaches a
> device — and the client comes from the node's
> profiles, not from a store path** — two mounts and an absolute store path were
> both wrong, for reasons that are now written down in
> [P1 as shipped](#p1-as-shipped-job-384). The conclusion holds; the mechanics
> in the paragraph above do not.

**W2 — the worker goes native.** #308 §H.6 already says a mixed-mode node forces
this for host mode. Rejected here: this design's entire claim is that container
mode does not need host-native execution, and paying host-native's largest cost
for the daemon would undercut it for no gain over W1.

**W3 — a disposable realise container.** The worker launches a short-lived
container that clones the job branch, realises, and exits; only it sees the
daemon socket. Better blast radius than W1 — project flake evaluation would run
in a throwaway, not beside `docker.sock` — at the cost of a second launch and a
second clone per task. **Named as W1's successor with a stated trigger**, in the
shape [#355](355-project-task-images.md) uses for O2→O1: take W3 the moment
[Decision 1](#decision-1--tenancy-dedicated-nodes-only) is relaxed, or the first
time a node runs a flake the operator has not read.

**The trust cost of W1, stated plainly.** Flake *evaluation* is client-side and
unsandboxed, so under W1 it runs inside the worker container — the process that
holds `docker.sock`, the NATS creds and the git key. That is a worse place for
project code than a task container. It is tolerable only because Decision 1
makes the node single-tenant: the boundary evaluation crosses is
platform-vs-project, not project-vs-project, and the project already runs
arbitrary code in its own tasks on that node. The nix daemon socket itself is
**not** `docker.sock`'s equal — derivation builders run sandboxed as `nixbld`
users and cannot mount the host — but it is not nothing either: it can consume
unbounded build CPU and disk, which is why
[Decision 4](#decision-4--gc-roots-an-explicit-indirect-root-per-task-and-no-assertion)'s
budget and the bound below both exist.

### 3c. The realise is bounded, and it is outside `task_timeout`

A pre-launch realise happens before execution begins, and #309 §3.5 starts the
task clock at execution. So the realise is covered by **no existing timeout** —
a cold flake would be an unbounded hang in the launch path, which STYLE.md Tier 2
rule 3 forbids outright.

**The worker bounds it** with a node-side realise timeout and fails the launch
loudly on expiry — `BackendError::Launch`, not `NoCapacity`, because a cold
realise that exceeded the bound will not get faster by being requeued. That is
the same posture [#367](367-android-emulator-execution.md) §3.3 gives a missing
SDK mount: "fail the launch loudly … never fall through to an ambient SDK."

**The bound is not a free parameter: its ceiling is 45 seconds.** This section
first read as though the operator sized the timeout to the toolchain. They
cannot. The realise runs *inside* the `launch` RPC handler
(`crates/worker/src/daemon.rs`, `realise_for_launch` before `backend.launch`),
and the dispatcher-side call is bounded at `store::worker::OP_TIMEOUT` = 60s, so
a bound past that never produces the named failure this paragraph promises — it
produces a transport failure and an orphaned container. P1 therefore ships
`NIX_REALISE_TIMEOUT_SECS_MAX` = `OP_TIMEOUT` − 15s reserve = **45**, default
**30** (`crates/worker/src/config.rs`), refused at parse time and again at
deploy. The whole derivation, and what it does to
[Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism),
is [C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization).

This has a consequence for [Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism)
that the first draft got wrong: **a project cannot absorb a cold realise by
enlarging `resources.task_timeout`**, because the realise does not happen inside
it. Nor, per C6, by enlarging the realise bound past 45s.

## Decision 4 — GC roots: an explicit indirect root per task, and no assertion

**The worker creates an indirect GC root over the realised closure as part of
the realise, and removes it when the task exits. The `chug-node` modules
(job #372) assert nothing about `nix.gc`.** They do not provision the directory
either — this section guessed that they would, and
[P1 as shipped](#p1-as-shipped-job-384) records where it actually comes from.

Measured, in order:

- **Today's mount is already safe, for a reason that expires.** The Android SDK
  is reachable from `/run/current-system` because it is in
  `environment.systemPackages`; `nix.gc` with `--delete-older-than 14d` drops
  only *old generations*, never the current closure. So
  [#367](367-android-emulator-execution.md)'s mount needs no protection —
  **precisely because clock 1 owns the SDK**, which is what this document
  removes. The safety and the doctrine violation are the same fact.
- **A project flake is in no profile.** The moment clock 3 works as intended the
  realised environment is garbage by definition. `nix-gc.timer` runs weekly on
  this node and would collect it between tasks — or collect the un-opened parts
  of a closure mid-task.
- **`/proc` scanning does cross the container boundary.** Starting a container
  holding a store file open made a new **runtime root** appear for exactly that
  path, for exactly the container's lifetime — nix finds these by scanning
  `/proc/*/maps`, `/proc/*/cwd` and friends, and `nix-store --gc --print-roots`
  prints the literal token `{censored}` in place of the `/proc` path when the
  caller is untrusted. (`{censored}` is nix's own output, not a redaction in
  this document; there is no nix client in the work container, so the exact
  line is quoted from the node measurement rather than re-run here.) So
  containerized tasks get
  incidental protection — **but only over what a process has open or mapped at
  that instant**, not the closure, and not what the task will need ten minutes
  later. It is a backstop, not a guarantee, and must be documented as one.
- **The mechanism works unprivileged**, as the worker's user:

  ```sh
  nix-store --add-root /path/task-N --indirect --realise /nix/store/…
  ```

  It registers under `/nix/var/nix/gcroots/auto`, shows in
  `nix-store --query --roots`, and clears when the platform's own symlink is
  removed. `nix build --out-link {gcroots}/{task_id}` — #309 §9's form — creates
  the same indirect root, which is why the realise and the root are one action
  rather than two.

**Why not an assertion.** Job #265's assertion pattern is right for
`virtualisation.docker.autoPrune` because there the platform *cannot* protect
itself — nothing about an agent image tells docker to spare it, so the boundary
must be compile-time. Here the opposite holds: nix ships the right primitive and
the platform can use it. An assertion would mean fighting the host owner over a
legitimate setting on their own machine, and it would constrain a *schedule*
rather than an *invariant* — the weaker kind of rule. `chug-node`'s stated
philosophy is to declare conditions and stay out of the lifecycle; a GC root is
lifecycle.

**The split:**

- **Clock 1 (the node)** provides the gcroots directory, owned by the worker's
  user — the same treatment `WORKER_CACHE_DIR` already gets. No GC assertion.
  *As shipped this is `deploy/prod/build-worker.sh`, not a `chug-node`
  `systemd.tmpfiles` line: [#372](372-chug-node-modules.md) §5 A5 declines to own
  GC roots, so the deploy provisions it and refuses if it cannot
  ([P1 as shipped](#p1-as-shipped-job-384)).*
- **Clock 2 (worker daemon)** creates the root over the **whole closure** as the
  realise's out-link and removes it at exit — the lifecycle `platform-ops`'s
  `dispose` already occupies, which exists because container overlays leaked
  disk.
- **Roots are named by task id**, so a crashed worker leaves a *greppable* stale
  root rather than an invisible pin — the posture `chuggernaut.managed`
  containers and the §3.6 reconcile sweep already use. That implies a bounded
  stale-root reaper, best-effort like `dispose`: a failed cleanup leaks disk but
  must never fail a job.

**The reaper is a divergence from #309 §9, and it is deliberate.** §9 concludes
"`remove` drops the root along with the task dir, and §3.6 step 6's existing
sweep is the crash backstop, so **no new sweep is needed**." That reasoning is
sound in host mode and does not transfer: container mode has **no task dir** for
the root to die with. The root lives in a worker-owned directory whose lifetime
is the node's, and §3.6's sweep reconciles *containers*, not store roots — a
container removed by the sweep leaves its root behind. Hence the reaper. See
[C5](#c5-309-9s-no-new-sweep-does-not-transfer-to-container-mode).

Honest cost: a pinned closure is disk that outlives the task — the same trade
`WORKER_CACHE_DIR` makes, and #308 §H.6's unclocked fourth thing. The reaper
bounds it; nothing eliminates it.

## Decision 5 — warming is a scheduled job, not a platform mechanism

[#309](309-host-native-execution.md) §9 prices a cold `nix develop` for a
Flutter/Android toolchain at tens of minutes. Per
[Decision 3c](#3c-the-realise-is-bounded-and-it-is-outside-task_timeout) that
cost now lands in a bounded pre-launch phase, so the failure mode is a loud
refused launch rather than a job that looks slow — better, but it makes warming
*more* necessary, not less: an unwarmed environment does not run late, it does
not run.

**This section carries two claims, and they are different arguments.** The
heading's claim is about **ownership**: warming is the project's to do, not the
platform's. [C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization)
adds a claim about **necessity**: the realise bound cannot exceed 45s, and tens
of minutes does not fit in 45 seconds, so **a toolchain that is not already
substituted on the node cannot be realised in the launch path at all.** For any
toolchain over the ceiling the warming job is a **precondition** of running the
job type there — not a wise optimization a project may skip and pay for in
latency. A node whose store lacks the closure refuses every admitted launch
against it. The ownership argument below decides *who* warms; the necessity
claim decides *whether the job type works at all*, and it holds even for a
project that would happily accept slow first runs.

**Warming is clock 3's problem.** The environment belongs to the project, so its
readiness does too. A platform-owned pre-warm would put the operator back in the
loop for a project's toolchain, and clock 3 would leak into clock 1 in a new
shape.

The first draft offered three shapes without saying which side of the boundary
each runs on. Only one of them is a project mechanism:

- **A scheduled job carrying the same `runtime.env` — this is the answer.**
  [#310](310-scheduled-jobs.md) shipped. A job type that declares the same env
  and does nothing (`run: true`) is warmed **by the worker's own pre-launch
  realise** — the realise site is unchanged, the store is populated as a side
  effect, and the platform ships nothing new. It is repo-versioned project
  config on the project's clock, which is exactly clock 3.
- ~~**A warm step inside the job**~~ — **withdrawn.** It presumed the task
  could realise its own env, and a task container has `/nix/store` **read-only**
  and no daemon socket, by
  [Decision 3b](#3b-where-the-realise-runs). Even given one, Decision 3c puts
  the realise before the task exists, so `resources.task_timeout` cannot cover
  it.
- **A binary cache is a clock-1 node fact, not a project choice.** Substituters
  are read from the node's `nix.conf` or passed by whoever invokes nix — here
  the worker. A flake's `nixConfig.extra-substituters` is honoured only for
  substituters the node already trusts (`trusted-substituters`, or a trusted
  invoking user), which is nix's design and not something this platform can
  route around. So a cache is the operator's to provision, per
  [C4](#c4-309-9s-daemon-warm-set-is-withdrawn-the-substituter-half-survives),
  and calling it "the project's config" in the first draft was wrong.

**The residual cost, stated:** relative refs are per-commit, so a scheduled warm
on the default branch warms what is already merged. **The commit that bumps the
toolchain pays the cold realise**, once, on its own job branch. That is the
right place for it — the change that costs the time is the change that pays —
but it means a toolchain bump can exceed the realise bound and be refused, and
under C6's ceiling the project's answer is **only** to land the flake change
ahead of the code change (or to substitute it from a cache first): raising the
node's bound buys at most 45 seconds and cannot buy a cold Flutter realise. Say
it in the job-type docs, as #309 §9 says of its own first-use cost.

So the platform's obligations are exactly three, and all are decided above: a
**bounded realise** ([3c](#3c-the-realise-is-bounded-and-it-is-outside-task_timeout)),
**do not garbage-collect a realised environment**
([Decision 4](#decision-4--gc-roots-an-explicit-indirect-root-per-task-and-no-assertion)),
and **make the store visible to the container**
([Decision 2](#decision-2--schema-container--image--env-one-cell)). Beyond that
it ships nothing for warming.

**Still worth measuring — by whoever adopts it, not as a platform gate.** No
cold realise has been timed in this tree. What a measurement now decides is
narrower than the first draft thought: the bound is picked from a 1..45s range
whose top P1 fixes ([C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization)),
so the number to measure is a *warm* realise — the substitution or the no-op —
and what an adopter learns from a cold one is how far ahead of the code change
the flake change has to land.

## Weighed against #355 — when a project picks which

[#355](355-project-task-images.md) project-supplied task images is the direct
competitor, and the first draft cited it without weighing it. It solves the same
problem statement, quotes the same CLAUDE.md sentence, and answers the same
acceptance test — "a project must be able to update its own task image without a
full chuggernaut redeploy."

| Axis | `image:` — #355 project image | `env:` — nix in container |
| --- | --- | --- |
| **Tenancy** | any node; nothing project-controlled evaluates outside a build sandbox | **dedicated node only** ([Decision 1](#decision-1--tenancy-dedicated-nodes-only)) |
| **Node-side project code** | a `docker build` of the project's Dockerfile (#355 §8) | flake evaluation in the worker's trust domain ([3b](#3b-where-the-realise-runs)) |
| **Freshness clock** | the project repo's **default branch** — #355 §1's `ref:` defaults to it, and #355 §5's reconciler builds from it | the **job branch's commit** ([3a](#3a-a-relative-ref-has-no-checkout-to-resolve-against)) |
| **Platform machinery** | a build path, a desired-state reconciler, per-node inventory, `docker rmi` GC (#355 §5, §7) | a realise step and a GC root ([Decision 4](#decision-4--gc-roots-an-explicit-indirect-root-per-task-and-no-assertion)) |
| **Disk** | per-SHA tags, never dangling, explicit GC, a per-node byte budget (#355 §7) | content-addressed store, deduped across job types and projects, roots reaped |
| **Bulk** | #367 §3.1's two real grounds: node disk, and rebuild time on refresh | the store already holds one copy however many job types name it |
| **Cold cost** | a build, on the project's clock, out of band | **must be paid out of band too.** The launch-path realise is capped at 45s ([C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization)), which fits a substitution and not a cold build, so a warming job is a precondition rather than a tuning knob ([Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism)) |
| **Can express** | anything a Dockerfile can, including non-nix ecosystems | anything nixpkgs can; **not** Xcode ([#322](322-macos-native-runtime.md) §3) |

**#355 wins the column that matters most here, and it should be said plainly:**
it needs no dedicated node. Decision 1's tenancy restriction is the largest
thing this design gives up, and for most projects #355 is simply the better
answer.

**The one thing #355 structurally cannot do** is the property #308 §H.6 drew the
three-clock model to protect. Its image is built from a declared `ref:` by a
reconciler, so a toolchain bump on a job branch has no image until it merges and
the reconciler catches up. R3's ref rewriting gives the flake the *job branch's*
tip. So:

> **#355 delivers a project-scoped clock. A flake ref delivers a commit-scoped
> one.**

The rule of thumb, in #367 §3.3's complement-not-rival shape:

- **Default to `image:`.** It is the multi-tenant answer and it costs the
  project a Dockerfile it probably already has.
- **Reach for `env:`** when the toolchain is many GB and content-addressed
  sharing pays, when it is already expressed as a flake, or when the project
  needs a toolchain change and the code change that needs it to be **the same
  commit**.
- **They layer** ([Decision 2](#decision-2--schema-container--image--env-one-cell)):
  a slim project image plus a realised env is a legal and sensible combination,
  and it is the same three-layer split #367 §3.3 already drew — image, node-side
  bulk, container overlay — with the middle layer's clock moved from the
  operator to the project.

## P1 as shipped (job #384)

[P1](#sequencing) merged at `2bd4bf3`. Three places where the implementation
diverged from the text above, **and the implementation was right** — recorded as
fact rather than as options, so the next reader is not misled by the paragraph
that guessed. The fourth divergence is large enough to be a Correction:
[C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization).

- **The nix client does not come "out of the mounted store by absolute path"**
  ([3b](#3b-where-the-realise-runs)'s W1). It cannot be, twice over. That path is
  content-addressed (`/nix/store/<hash>-nix-<version>/bin/nix`), and
  [#367](367-android-emulator-execution.md) §3.5 forbids a store hash in any
  chug-side config; and `chug-worker` is long-lived, so a client resolved at
  bind-create would pin the generation current at the last swap — exactly what
  `--delete-older-than` collects. It is reached through the node's **profiles**
  instead
  (`NIX_CLIENT_DEFAULT` = `/nix/var/nix/profiles/system/sw/bin/nix-store`,
  `crates/worker/src/config.rs`), a tree that is itself GC-rooted, mounted
  read-only beside the store and resolved inside the container at each use
  (`deploy/prod/build-worker.sh`). W1's *conclusion* survives — the client is
  still the daemon's own binary, so version skew is still impossible — but by
  the profiles, not by an absolute store path.
- **The realise target is bound as its PARENT directory, never the leaf.** The
  node's stable toolchain path is a *symlink into the store*, and `mount(2)`
  resolves a bind source host-side — the same fact #367 relies on for the *task*
  container. Applied to the leaf it destroys the property: `chug-worker` gets the
  store path's content at a non-store path, and `nix-store --realise`, which
  canonicalizes client-side before the daemon hears anything, refuses it. So the
  toolchain path's `dirname` is bound as a **fifth** mount, added only when the
  node attaches the KVM device (`deploy/prod/build-worker.sh`) — that attachment
  is exactly the condition under which a launch is admitted and therefore
  realised, so `realise_for_launch` (`crates/worker/src/daemon.rs`) returns
  `Ok(None)` without it. `store_target()` (`crates/worker/src/nix.rs`) canonicalizes in the daemon's
  own view and refuses a target that does not land under the store — making this
  a **boot refusal** rather than a per-launch failure. The deploy refuses ahead
  of that when the path is not a direct absolute symlink into the store under a
  real parent, naming the `systemd.tmpfiles` `L+` remedy.
- **The gcroots directory is not `chug-node`'s.** The [Sequencing](#sequencing)
  row gated P1 on "#372 providing the gcroots dir and the daemon socket". Neither
  was #372's to give: the daemon socket already exists at
  `/nix/var/nix/daemon-socket/socket` on a stock node with nothing to provision
  (the deploy checks for it and refuses if it is absent, and never creates it),
  `/nix/var/nix/gcroots/auto` — where an indirect root registers — already
  exists, and [#372](372-chug-node-modules.md) §5 A5 declines **both** an
  assertion and a `chug.node.gcRoots` option, so job #383's merged modules
  contain no `gcroots` at all. P1 provisions the roots directory from
  `deploy/prod/build-worker.sh` instead, in the shape job #380 gives
  `WORKER_CACHE_DIR`, and a failure to provision refuses the deploy with the live
  daemon untouched.

**One root cause underlies all three**, and it is what cost #384 all three of its
rework cycles: `chug-worker` is itself a container, so every host fact has to be
re-derived inside its namespace. The first two bullets are that mistake directly
— the client's path and the realise target are both resolved in the daemon's own
view, not the host's. The third is its consequence for provisioning: a directory
the daemon creates lands in the daemon's writable layer and never on the node, so
the deploy has to create it. The invariant is stated **once**, in
[STYLE.md](../../STYLE.md) Tier 2 rule 7 — where a worker reads it before writing
code rather than after failing — and this document does not restate it.

## Corrections

### C1. #308 §H.6's "same slot" claim is mode-dependent

§H.6 states: "**`image:` and a flake ref occupy the same slot.** Both are
pinned, content-addressed environment references, which is why H.3's schema
change is one selector field rather than a new execution model in the config."
(§H.2 makes the shorter version — "a nix flake replaces `image:` … that
equivalence is why the schema change stays small".)

**True in host mode, false in container mode.** With no userland to declare the
two collapse into one slot; with a container they layer, and the measurement
shows a Debian image and a nix closure serving different halves of one task.
The conclusion both sections draw from it — that the schema change stays small —
happens to survive, for a different reason: `env` becomes an *optional* peer
rather than a replacement, which is a one-cell change.

### C2. #309 §9's mechanism is not host-mode-only

§9 defines `runtime.env` as reachable only under `runtime.mode: host`. The
measurement shows a container consuming a nix environment with no host execution
anywhere. #309's own P5 ("declared caches") is already marked *"Independent of
P2–P4"*; this finding extends that independence to the environment mechanism
itself.

### C3. The `kvm` group requirement in #367 A0 is moot

`/dev/kvm` on `gumbo-nuc-0` is mode `0666`; `worksalot` is not in the `kvm`
group and the container opened the device fine. (The host's *GHA runner*
systemd units do need `SupplementaryGroups` plus `PrivateDevices = false` and
`DevicePolicy = "auto"`, because nixpkgs' systemd defaults hide `/dev/kvm` —
that is a unit concern and does not apply to the containerized `chug-worker`.)
Recorded here because it was measured in the same session;
[#367](367-android-emulator-execution.md)'s amendment owns the fix.

### C4. #309 §9's daemon warm set is withdrawn; the substituter half survives

§9 recommends, and #309 P5 sequences, "a node-side binary cache plus a warm set
**the daemon realises at startup and refreshes on a schedule**."

**The warm set is withdrawn, in both modes.** A daemon-held warm set is a
platform-owned list of project environments — clock 3 config living on clock 2,
refreshed on the platform's schedule. Since #309 was written,
[#310](310-scheduled-jobs.md) shipped, and a scheduled job declaring the env is
the same mechanism with the project owning the list and the cadence
([Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism)).
Host mode gains nothing from keeping the daemon version once container mode has
the scheduled-job version, so this withdrawal is offered for both rather than
scoped to container mode; #309's own amendment is the place to accept or reject
that.

**The binary cache half stands, and it is clock 1** — a substituter in the
node's `nix.conf`, not a project field. #309 §9 does not say which clock it
belongs to; this document does.

### C5. #309 §9's "no new sweep" does not transfer to container mode

§9: "`remove` drops the root along with the task dir, and §3.6 step 6's existing
sweep is the crash backstop, so **no new sweep is needed**." That holds in host
mode, where the root lives *in* the task dir. Container mode has no task dir and
§3.6 reconciles containers rather than store roots, so a crash leaks a pin with
nothing to notice it. The bounded reaper in
[Decision 4](#decision-4--gc-roots-an-explicit-indirect-root-per-task-and-no-assertion)
is the container-mode replacement.

### C6. The realise bound has a 45-second ceiling, so warming is a precondition, not an optimization

[3c](#3c-the-realise-is-bounded-and-it-is-outside-task_timeout) treats the realise
bound as a free parameter: the realise is covered by no existing timeout, so "the
worker bounds it" and the operator sizes that bound to the toolchain. **It is not
free.** Job #384 established where the realise actually runs and what contains it.

**The ceiling, and where it comes from.** The realise happens inside the `launch`
RPC handler — `realise_for_launch` runs before `backend.launch`
(`crates/worker/src/daemon.rs`) — and the dispatcher-side call is bounded at
`store::worker::OP_TIMEOUT` = **60s** (`crates/store/src/worker.rs`). A realise
that outlives that budget does **not** produce the loud named
`BackendError::Launch` 3c describes. The dispatcher's call fails as
`WorkerRpcError::Transport`; a launch has no container id yet, so `rpc_err`
(`crates/worker/src/backend.rs`) maps it to `BackendError::Unavailable("worker
transport: …")`, which is not `NoCapacity` and so is not requeued — the task fails
with a reason naming **worker transport**, which says nothing about a toolchain.

**And the node keeps going.** The worker never learns the caller left: it finishes
the realise and launches the container. The dispatcher only records
`task.container_id` on the `Ok` path (`crates/dispatcher/src/launch_queue.rs`), so
that container is running for a task already failed, under an id nobody holds,
and its GC root is recovered only by the stale-root reaper's one-hour grace
(`REAP_AGE_MIN`, `crates/worker/src/nix.rs`). A bound the RPC cannot contain
converts a loud refusal into a silent orphan.

**So P1 makes the ceiling a rule rather than a matter of operator judgement.**
`NIX_REALISE_TIMEOUT_SECS_MAX` = `OP_TIMEOUT` − `NIX_REALISE_RESERVE_SECS` (15s
for the container create and the reply's trip home) = **45**, with
`NIX_REALISE_TIMEOUT_SECS_DEFAULT` = **30** (`crates/worker/src/config.rs`). A
larger value is a hard config error at parse time, and `deploy/prod/build-worker.sh`
mirrors the same 1..45 refusal so the deploy fails fast instead of handing
`--restart=always` a daemon that cannot boot.

**The consequence 3c did not draw.** [Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism)
prices a cold Flutter/Android realise, after [#309](309-host-native-execution.md)
§9, at *tens of minutes*. Tens of minutes does not fit in 45 seconds, and no
operator setting makes it fit. Therefore **a project toolchain that is not
already substituted on the node cannot be realised in the launch path at all**,
and Decision 5's warming job stops being an optimization: it is a
**precondition** for running any job type whose toolchain exceeds the ceiling.
Decision 5's heading argues *ownership* — warming is the project's, not the
platform's. This is a separate claim about *necessity*, and it binds even a
project that would happily accept slow first runs.

**The named successor: accept fast, realise in the background.** #384 raised the
one shape that could lift the ceiling — the daemon acknowledging the launch
immediately and doing the slow work outside the caller's budget. It is not
hypothetical here: `refresh` already works that way, and `REFRESH_TIMEOUT`'s doc
comment (`crates/store/src/worker.rs`) says so — "the daemon accepts fast and
builds/swaps in the background … this only covers the accept round-trip, not the
build." Recorded as the successor in the shape [3b](#3b-where-the-realise-runs)
uses for W1→W3 and [#355](355-project-task-images.md) uses for O2→O1, **not
designed here**. Its trigger: the first project toolchain that cannot be warmed
ahead of the commit that needs it — i.e. the first time
[What would refute this](#what-would-refute-this)'s first bullet actually
happens. Until then a 45s bound plus a warming job is the cheaper answer, and it
costs the platform nothing new.

## What this makes wrong elsewhere

- **[#309](309-host-native-execution.md) §9** — three ways: the mechanism is not
  host-only ([C2](#c2-309-9s-mechanism-is-not-host-mode-only)), the daemon warm
  set is withdrawn ([C4](#c4-309-9s-daemon-warm-set-is-withdrawn-the-substituter-half-survives)),
  and "no new sweep is needed" does not transfer
  ([C5](#c5-309-9s-no-new-sweep-does-not-transfer-to-container-mode)). §9's
  relative-ref resolution rule ("resolves against the job branch checkout") also
  needs the container-mode clause from
  [Decision 3a](#3a-a-relative-ref-has-no-checkout-to-resolve-against). And §9's
  closing instruction — "an explicit statement in the job-type docs that the
  first use of a new environment ref on a node is charged to `task_timeout`" —
  is host-mode-only for the same reason C5 is: container mode realises
  *before* execution starts, so the cost falls outside `task_timeout` entirely
  and is bounded by the worker instead
  ([Decision 3c](#3c-the-realise-is-bounded-and-it-is-outside-task_timeout)).
  No separate Correction: §9 is right about its own mode.
- **[#308](308-gha-port.md) §H.6** (and §H.2's shorter form) — see
  [C1](#c1-308-h6s-same-slot-claim-is-mode-dependent).
- **[#355](355-project-task-images.md) §9** — it states that `image:` and
  `runtime.env` are "**mutually exclusive** with the other by the field rules"
  and that "a job type is container-mode or host-mode, never both". The first
  clause is falsified by
  [Decision 2](#decision-2--schema-container--image--env-one-cell): in container
  mode they layer. The second clause survives — mode is still exclusive; it is
  `image`/`env` exclusivity that goes. #355 §9's larger point, that a host node
  does not participate in project task images, is untouched.
- **Host-native's motivation narrows.** For everything except Xcode and
  genuinely host-state-dependent work, container mode plus a store mount serves
  clock 3. [#322](322-macos-native-runtime.md) stays fully motivated — Xcode is
  not expressible as a flake output and `xcrun simctl` cannot be containerized,
  and that is where the boundary sits: `xcode:` schemes stay host-only by
  `validate()` rule ([Decision 2](#decision-2--schema-container--image--env-one-cell)
  rule 1), with #322 W4's node-side discovery unchanged.
  [#309](309-host-native-execution.md) stays motivated for the general host node
  kind, but its urgency for *toolchain delivery specifically* drops.

## What would refute this

- **A cold realise no scheduled-job shape can absorb** — too slow to warm ahead
  of the commit that needs it. The second half of this bullet ("too slow for any
  defensible realise bound") is no longer a judgement call: the bound tops out at
  45s ([C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization)),
  so almost every cold realise is already past it. That would force the platform
  back into warming, which
  [Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism)
  declines, or into C6's named successor — accept-fast, realise in the
  background — and would put clock 3 back in the operator's hands.
- **A project whose toolchain genuinely needs host state** — a device, a
  daemon, a persistent VM — rather than just files. That is #309/#322's
  territory and this document does not take it.
- **Multi-tenant demand.** If a node must serve two projects' flakes, Decision 1
  fails; W3 in [3b](#3b-where-the-realise-runs) is the first move, and the
  evaluation-side tenancy problem then has to be solved properly rather than
  side-stepped.
- **A nix client that cannot be run from a mounted store.** W1 rests on the same
  property the measurement demonstrated for `avdmanager`. If the daemon socket
  turns out to need something the worker container cannot have, W2 or W3 is
  forced and this design gets more expensive.

## Sequencing

| Slice | Kind | Work | Depends on |
| --- | --- | --- | --- |
| **P1** | `code` | **Shipped (job #384, `2bd4bf3`)** — the realise step in the worker: `/nix/store:ro`, the profiles tree and the nix daemon socket mounted into `chug-worker`, client resolved through the profiles (W1, [as shipped](#p1-as-shipped-job-384)), realise bounded at 30s default / **45s ceiling** with a loud `BackendError::Launch` ([C6](#c6-the-realise-bound-has-a-45-second-ceiling-so-warming-is-a-precondition-not-an-optimization)), out-link GC root named by task id, removal at exit, bounded stale-root reaper. The shipped realise fires **only for a KVM-admitted launch** (`realise_for_launch`), so what P1 realises today is the node's declared Android SDK path, not yet an arbitrary project toolchain — that is P2 | **Nothing from #372.** The daemon socket and `/nix/var/nix/gcroots/auto` already exist on a stock node, and #372 §5 A5 declines to own GC roots, so `deploy/prod/build-worker.sh` provisions the roots dir in #380's shape ([as shipped](#p1-as-shipped-job-384)) |
| **P2** | `code` | `runtime.env` accepted in container mode: the one field rule, the `xcode:`-is-host-only validate rule, R3's relative-ref rewriting, the commit sha on the launch spec, the store mount and env injection into the task container, `WORKER_NIX_PROJECTS` | [#309](309-host-native-execution.md) slice 4 (the `runtime:` block + epoch); P1 |
| — | — | **Warming is not a platform slice.** Per [Decision 5](#decision-5--warming-is-a-scheduled-job-not-a-platform-mechanism) it is a scheduled job declaring the same env, warmed by P1's own realise; the substituter is clock 1 | — |

**P1 first, and it stands alone.** At `5aeb439`
[#367](367-android-emulator-execution.md) §3.2 already recommends mounting
`/nix/store` read-only into the task container, so a task holding a store path
with no GC root against a weekly `nix-gc.timer` is a hazard on the fleet's
current recommended shape — whether or not P2 ever lands. P1 fixes that, and is
a prerequisite for P2 besides. Its standing argument is otherwise the ordinary
one: no schema surface, no epoch cost, no wire change.

Note what is **not** here: no measurement gate, and no pre-warm slice. Decision 5
moved both out of the platform.

## Related

[#308](308-gha-port.md) §H.6 (three clocks, the "same slot" bullet) and §H.2
("the schema change stays small"); [#309](309-host-native-execution.md) §3, §9,
§10, P5; [#322](322-macos-native-runtime.md) §3, W4;
[#367](367-android-emulator-execution.md) §3.1, §3.3 and its amendment;
[#355](355-project-task-images.md) §1, §5, §7, §8, §9;
[#310](310-scheduled-jobs.md); job #265 (worker-node co-tenancy, the assertion
pattern); [#372](372-chug-node-modules.md) §5 A5 (`chug-node` modules — machine
facts, and why GC roots are not theirs); jobs #384 (P1 as merged), #383 (the
modules), #380/#379 (the cache-dir provisioning shape) and #374 (the KVM
mounts); `crates/types/src/job_type.rs` (`JobType::validate`, `image`
rules); `crates/types/src/version.rs` (`CONFIG_SCHEMA_EPOCH`);
`crates/container/src/lib.rs` (`bootstrap_cmd`, `ContainerLaunchConfig`);
`crates/container/src/docker.rs` (`with_cache_dir`, `CACHE_MOUNT_PATH`);
`crates/dispatcher/src/exec.rs` (`REPO_URL`/`JOB_BRANCH` injection);
`crates/dispatcher/src/launch_queue.rs` (what a non-`NoCapacity` launch error
does); `crates/store/src/worker.rs` (`OP_TIMEOUT`, `REFRESH_TIMEOUT`);
`crates/worker/src/config.rs` (`NIX_REALISE_TIMEOUT_SECS_MAX`, `NIX_CLIENT_DEFAULT`);
`crates/worker/src/nix.rs` (`store_target`, `REAP_AGE_MIN`);
`crates/worker/src/daemon.rs` (`realise_for_launch`);
`crates/worker/src/backend.rs` (`rpc_err`); `deploy/prod/build-worker.sh` (the
worker's own container, and the roots dir it provisions);
[STYLE.md](../../STYLE.md) Tier 2 rule 7 (the container-namespace invariant);
`spec.md` §1.1, §3.1, §14.2; CLAUDE.md (factories and job-type
config are project-owned and repo-versioned).
