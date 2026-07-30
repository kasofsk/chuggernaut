# Design #308 — Porting beacon's GitHub Actions onto Chuggernaut

Status: PROPOSED. Written against the tree at `0346a80`. Every claim about
*Chuggernaut's* current behavior was read out of `spec.md` and the source in
this repo, not inferred from the docs; the corrections in
[What the brief got wrong](#what-the-brief-got-wrong) are the ones that survey
found.

Related: [spec.md](../../spec.md) §1.1 (job-type config), §3.1 (dispatcher
backends, fleet, node-local build cache), §3.3 (staged evaluation, merge gate),
§13 (factories and ingest), §14 (config/version skew), Appendix: Deferred;
[design-lifecycle.md](../../design-lifecycle.md) (rework, abort);
[CLAUDE.md](../../CLAUDE.md) ("the evaluation gates ARE the CI");
[deploy/prod/README.md](../../deploy/prod/README.md) §3 (GitHub Actions CD is
already gone for one workflow), §5a/§5b (tailnet vs public exposure);
[docs/design/293-worker-capacity.md](293-worker-capacity.md) (in-flight changes
to the same announce payload this doc wants to extend).

## Provenance

Two halves, with different evidentiary weight, and the doc marks which is which:

- **The beacon half** — 41 workflows and 14 composite actions in `beacon`'s
  `.github/`, plus the quilbert/scrybert agent protocol — comes from the survey
  in the job brief. That repo is not checked out in this workspace, so nothing
  here re-verifies it. Where a decision hangs on a beacon detail, the doc says
  so, and that detail should be re-read before the corresponding phase starts.
- **The Chuggernaut half** is verified against this tree. Paths and spec
  sections are citations, not illustrations.

## Problem

beacon's CI/CD is 41 GitHub Actions workflows. Chuggernaut is the platform we
now run everything else through, and one of those workflows — deploy — has
already been ported (`.chug/jobs/deploy.yaml`; `deploy/prod/README.md` §3 opens
with "**GitHub Actions CD is gone.**"). The question is whether the remaining 40
can follow, what it costs, and in what order.

The answer is not uniform, and the interesting part is not "can Chuggernaut run
a shell script" — it obviously can. It is that **Chuggernaut's execution
substrate is stronger than GHA exactly where the two overlap, and absent where
they don't.** Overlapping and stronger: staged gates (§3.3), rework loops
(§1.1 `rework_budget`, §4.5 inline review), per-container secret scoping
(§1.1, §8.2), resource limits (§1.1 `resources`), a merge gate that guarantees
no commit lands without every required command evaluator passing against the
exact tree that lands (§3.3 Merge Gate), and single-writer concurrency (§3.1).
Absent: triggers, parameterization, keyed caching, binary artifacts, and
machine-level execution.

### The one-sentence version

**GHA's isolation unit is a machine; Chuggernaut's is a container.** GHA never
solved docker-in-docker — it sidesteps it by running jobs *on* the host, which
is why beacon's `gumbo` runner is a persistent NixOS box with a host docker
daemon and a `/var/lib/github-runner/.buildx-cache-*` directory. Every hard
problem below (image builds, keyed caches, emulators, Xcode) is a restatement of
that one difference. Section [H](#h-host-native-workers) is the proposal that
addresses it directly; everything before H is what can be ported without it.

## What is true today (verified)

| Capability | Where | State |
| --- | --- | --- |
| Staged evaluators, stage 0 gates stage 1 | `spec.md` §3.3; `.chug/jobs/_defaults.yaml` | Shipped |
| Project-wide CI as an appended command evaluator | `.chug/jobs/_defaults.yaml` → `.chug/tasks/ci.sh` | Shipped |
| Diff-aware multi-leg CI script | `.chug/tasks/ci.sh` (549 lines) | Shipped |
| Per-container secret scoping | `spec.md` §1.1/§8.2; `.chug/jobs/deploy.yaml` | Shipped |
| Command work + `wrap_up: none` for external effects | `spec.md` §1.1; `.chug/jobs/deploy.yaml` | Shipped |
| Node pinning (`placement.node`) | `spec.md` §3.1; `crates/types/src/job_type.rs` | Shipped |
| Mixed fleet (docker endpoints + NATS worker nodes) | `crates/worker/src/backend.rs` `NodeHandle` | Shipped |
| `ContainerBackend` implementations | `crates/container/src/docker.rs:378`, `crates/worker/src/backend.rs:539`, `crates/test-utils/src/lib.rs:343` | Three — one Docker, one NATS proxy, one fake; `crates/container/src/k8s.rs` is a stub |
| Node-local build cache (sccache) | `spec.md` §3.1; `crates/worker/src/daemon.rs`, `WORKER_CACHE_DIR` | Shipped |
| Three review outcomes (pass / fail / fail+abort) | `spec.md` §1.2 Abort verdict, §3.3 Reduce | Shipped |
| Version-skew machinery | `crates/types/src/version.rs`, `spec.md` §14 | Shipped |
| Ingest → triage → jobs (GitHub origin) | `crates/dispatcher/src/forge_ingest/` | Shipped |
| Generic factories (`factories/{name}.yaml`) | `spec.md` §13.3 | Specced, not in the tree |
| Runtime capacity control / richer announce | `spec.md` §3.1; [design #293](293-worker-capacity.md) | Specced + designed, not in the tree |
| Outbound webhooks | `crates/webhooks/src/lib.rs` | Two-line `TODO` stub |
| Binary artifact store | `spec.md` Appendix: Deferred | Deferred |
| Cron / schedules | — | Absent and unspecced |
| Job inputs / matrix / parameters | — | Absent and unspecced |
| OIDC issuance / JWKS endpoint | — | Absent (RS256 keypair exists, §12.1) |

## What the brief got wrong

The brief is a snapshot analysis and seven of its claims do not survive contact
with the tree. They matter because three of them make the port *cheaper* than the
brief assumes and four make it *more expensive* or re-sequence it.

1. **"Chuggernaut evaluators are pass/fail, so scrybert's forced three-way
   verdict is lost."** They are not. `spec.md` §1.2 defines the **abort
   verdict**: `abort: true` on `submit_eval` means "not satisfiable by rework",
   implies fail, skips the remaining rework budget and escalates immediately
   (§3.3 Reduce, §3.2 step 13). scrybert's `approve` / `request-changes` /
   `escalate` maps exactly onto `pass:true` / `pass:false` / `pass:false,
   abort:true`. There is nothing to build; there is a *prompt* to write that
   forces the choice, which is where the property actually lived in scrybert
   too.
2. **"A human always merges; Chuggernaut auto-merges — a policy divergence with
   no mechanism."** The mechanism exists: a `human` evaluator (§1.1, §3.3) at
   the highest `stage` runs after the agent reviewer and CI have passed and
   before the squash lands. The divergence is a *default*, not a capability gap.
   That reframes decision [D2](#open-decisions) from "build something" to
   "choose a default", which is a much smaller thing to be blocked on.
3. **"No keyed cache at all; the only mitigation is baking toolchains into the
   image."** A node-local build cache already ships (`spec.md` §3.1 "Node-local
   build caching"): a worker sets `WORKER_CACHE_DIR`, its daemon bind-mounts one
   host directory into every container it launches and injects
   `RUSTC_WRAPPER`/`SCCACHE_DIR` (`crates/worker/src/daemon.rs`). Critically,
   the **dispatcher is cache-ignorant** — no launch field, no wire field, no
   schema change — because the cache is declared to carry no job state and be
   safe when cold. That is precisely the property a `~/.gradle` or
   `node_modules` cache also has, so extending this mechanism is a
   worker-daemon-local change, not a platform change. The gap is real but it is
   "one sccache-shaped hole widened", not "no mechanism".
4. **"`image` is required and every nested block is `deny_unknown_fields`, so a
   host-node selector must bump the epoch."** Right conclusion, wrong reason,
   and the right reason is sharper. Unknown **top-level** fields are *tolerated*
   with a warning (§14.2) — so adding a top-level `env:`/`host:` selector is by
   itself N−1 safe. What forces the bump is that `image` is **required for
   agent/command work** (`crates/types/src/job_type.rs`, the `Required { field:
   "image" }` rules): an old dispatcher reading a host-mode job type sees a
   missing required field and rejects the config outright. So the epoch bump +
   `min_dispatcher` is still mandatory — but because of a *required* field, not
   an unknown one.
5. **"The worker daemon can select a host backend locally, keeping dispatcher
   changes near zero."** Directionally right, one step short. The daemon does
   not hold a `dyn ContainerBackend`; `crates/worker/src/daemon.rs` holds a
   concrete `backend: DockerBackend` and every op calls it directly. Making the
   daemon backend-polymorphic is a small, contained refactor — but it is a real
   prerequisite, not a free consequence of the existing abstraction.
6. **"Add `modes`/`platform` to the announce payload as an additive field."**
   Fine in principle, but `WorkerAnnounce` in `crates/types/src/worker.rs` is
   currently `{ node, slots, version }` while `spec.md` §3.1 already describes
   `{ node, slots, slots_max, capacity_epoch, capacity_generation, version }` —
   the fields [design #293](293-worker-capacity.md) adds and the code does not
   yet have. Any capability field must land *after* or *with* #293, not race it.
7. **"`ContainerBackend` already has two implementations (`docker.rs`,
   `k8s.rs`), so a host backend is a third."** The count is right and the roster
   is wrong, in a way that moves risk. `crates/container/src/k8s.rs` is a
   six-line stub — `pub struct K8sBackend;` and a `TODO: implement with kube +
   k8s-openapi` — and does not implement the trait. `grep -rn "impl
   ContainerBackend for" crates/` returns exactly three: `DockerBackend`
   (`crates/container/src/docker.rs:378`), `FleetBackend`
   (`crates/worker/src/backend.rs:539`), and `FakeBackend`
   (`crates/test-utils/src/lib.rs:343`). See H.1 — the real roster is *better*
   evidence for the seam than the brief's, but it also means no non-container
   backend has ever been built, which is where H.3's cost 1 concentrates.

Three of these (1, 2, 3) delete or shrink work the brief scheduled. Four
(4, 5, 6, 7) add work or re-sequence it.

## Category map: the 41 workflows

### A. Subsumed — do not port (5 workflows)

quilbert, scrybert, and the quilbert-slots scheduler. These are an agent
work/review loop implemented on top of GitHub events, and Chuggernaut *is* that
loop:

| beacon mechanism | Chuggernaut equivalent |
| --- | --- |
| quilbert (implementer) | `work.type: agent` (§1.1) |
| scrybert (reviewer) | agent evaluator (§3.3) |
| iteration cap 5 | `work.review.iterations`, default 5 (§4.5 inline review) — the *inner* author↔reviewer loop. The *outer* loop across eval cycles is `rework_budget` (§1.1) |
| `autobert` label | release / revoke (§2.1, §2.2) |
| failure ping | `Escalated` + a Human escalation task (§3.4) |
| per-issue concurrency group | one job, one `job/{seq}` branch, single writer (§3.1, §5.1) |

The brief maps the iteration cap onto `rework_budget`; both loops exist and the
cap maps more precisely onto `review.iterations` — an author↔reviewer exchange
inside one work task, which is what quilbert's cap actually bounded. Say which
one you mean when porting, because they have different costs (`review.iterations`
burns tokens inside a task; `rework_budget` burns whole eval fan-outs).

**The two-identity PAT scheme dies with the transport.** beacon runs two real
GitHub accounts with manually-rotated PATs (~30 days) purely so the two agents
can act as distinct GitHub actors and see each other's events. With a dispatcher
there is no event transport to impersonate — the loop is in-process. This is
pure deletion, and it is the single largest operational saving in category A.

**Preserve deliberately** (these are beacon properties worth keeping, not
accidents):

- **The forced three-way verdict**, with comment-only reviews banned so the
  reviewer cannot hedge. As established above, the mechanism exists (abort); the
  forcing lives in the evaluator prompt. `.chug/tasks/review-code.md` is the
  precedent to follow.
- **Guards in YAML, not prompts.** Anything a reviewer *must* do belongs in
  `.chug/jobs/*.yaml` (`required`, `stage`) where config validation enforces it,
  not in prose an agent may drift from. This repo already holds that line —
  §14.2 keeps `deny_unknown_fields` on evaluator blocks precisely so a typo
  cannot silently disable a gate.
- **A human in the merge path** — see [D2](#open-decisions).

**One item in category A has no home.** `sentry-resolve` is an *outbound*
webhook (Chuggernaut → Sentry), and `crates/webhooks/src/lib.rs` is a two-line
`TODO` stub. Nothing in the ordering below unblocks it; it stays on GHA or waits
for §6.4 to be implemented.

### B. CI as evaluators (6 workflows)

`rust-ci`, `flutter-ci`, `creator-tests`, `code-duplication`,
`terraform-validate`, `actions-validate`.

The pattern is already the house pattern: `.chug/jobs/_defaults.yaml` appends a
`ci` command evaluator that runs `.chug/tasks/ci.sh` against the job branch
before any merge. beacon's equivalent is one diff-aware script with rust,
flutter, creator, terraform and jscpd legs — structurally the same shape as
`.chug/tasks/ci.sh`, which already has independent diff-aware stages (web vs
Rust vs docs-only) and three unconditional pure-shell gates. Both repos already
run the same pinned `jscpd@5.0.5`, so `code-duplication` ports as a copy.

Mechanical translations:

- **Matrix** → a shell loop inside one evaluator, or several evaluators sharing
  a `stage` (they fan out in parallel within a stage, §3.3). Prefer separate
  evaluators when the legs fail for genuinely different reasons: the failing
  evaluator's *name* is then the diagnosis, and the staged merge gate uses stage
  boundaries to classify compile-class vs test-class failures (§3.3 gate-fix
  fast path).
- **`concurrency` / `cancel-in-progress`** → drops out entirely. One job owns one
  branch; the dispatcher is the single writer.

**Gain:** a stage-0 agent reviewer runs *before* the slow build, so the
expensive gate is spent only on changes a reviewer accepts — plus a rework loop
that GHA has no concept of.

**Loss:** `actions/cache` — gradle, pub, `node_modules`, buildx. Per correction
3, the mitigation is better than the brief suggests (the `WORKER_CACHE_DIR`
mechanism generalizes without a schema change), but it is still node-scoped and
still uncovered for anything that is not sccache today. The spec's own
mitigation — bake toolchains into the image (Appendix: Deferred) — is proven for
Rust (`deploy/prod/Dockerfile.agent-rust` bakes sccache, a `nats-server`, and a
warm dep graph) and **untested for flutter/gradle/npm**. This is the biggest
container-path regression in the whole port, and it is the gap section H
dissolves rather than narrows.

### C. Deploys (16 workflows)

deploy/rollback × {web, worker, bot} × {dev, prod}, plus creator, homepage, and
nats. Every one is `workflow_dispatch`, and `workflow_dispatch` is exactly
"create a job and release it".

`.chug/jobs/deploy.yaml` is the finished template and needs no invention:
`work.type: command`, secrets scoped to the work container only, a health
command evaluator at `stage: 0` that alone decides whether the release is
healthy, and `wrap_up: type: none` so an eval pass goes straight to Done with
nothing merged.

**Two wins that are not cosmetic:**

1. **Concurrency.** Per the brief's survey, beacon has *no* concurrency guard on
   any deploy workflow — two dispatches can race the same VM image-tag metadata.
   The single-writer dispatcher (§3.1) removes the race for free, with no
   configuration.
2. **Secret blast radius.** beacon sets `CF_API_TOKEN` as a workflow-level
   `env`, which exposes it to every step including `npm ci` — i.e. to every
   transitive package's install script. Chuggernaut's secrets are scoped to the
   container that needs them (§1.1: "injected into the work container only …
   evaluators declare their own"), which is what `.chug/jobs/deploy.yaml`
   already does for `MINI_DEPLOY_KEY` and `DEPLOY_HEALTH_API_TOKEN`.

**Was blocked, now unblocked:** rollback needs a per-run target (an `image_tag`
there, a git SHA here) and there was no per-job input of any kind. See gap 1.
**Landed** as [#311](./311-job-inputs.md) slice A: `.chug/jobs/rollback.yaml`
is `deploy.yaml`'s shape plus one required `sha` input, and
`.chug/tasks/rollback.sh` reads it as `$CHUG_INPUT_SHA`. So this category ports
whole — forward-only was never the ceiling, only the ordering.

### D. Image build and push (5 workflows) — open

Job containers have no docker socket, no docker-in-docker, and no registry
auth. Two options, presented without a pick because the choice depends on
whether phase 2 lands (see [Open decisions](#open-decisions)):

**D1 — a scoped docker socket on a pinned builder node.** The gumbo analogue:
mount the host socket into containers of one pinned job type only
(`placement.node`, §3.1). *For:* it is the same trust beacon already extends to
gumbo, narrowed from "every workflow" to "one job type", and it reuses buildx's
local cache unchanged. *Against:* a container with the host docker socket is
effectively root on that node — and unlike gumbo, that node also runs other
projects' jobs. It also punches a hole in §3.1's "no host bind-mounts" rule,
whose one existing exception (the build cache) is explicitly justified by
carrying *no job state*; a docker socket carries the whole node.

**D2 — rootless buildkit or kaniko.** *For:* strictly better isolation than GHA
offers today, and no rule to break. *Against:* the local buildx cache must
become a registry cache, which changes cache economics and adds a registry
round-trip to every build; and it is new operational surface nobody here has
run.

**Do not port two of these as-is.** Per the survey, two push `:latest` with no
SHA tag — so there is no rollback handle, which is also why category C's
rollback workflows are fragile. Fix at port time (tag by SHA, move `:latest` to
an alias) rather than faithfully reproducing the defect.

**Both options dissolve if host-native nodes land** (section H): a host node has
a real docker daemon the way gumbo does, and the question stops being
interesting.

### E. Cron (1 live, 2 dormant)

`flutter-integration-tests` runs nightly; the `sentry-sync` and `quilbert-slots`
crons are commented out. There is no cron anywhere in Chuggernaut and — unlike
ingest, which is specced in §13 — no spec for one.

`crates/dispatcher/src/scan.rs` is the obvious hook: it already runs inside the
single-writer loop on the 30-second tick (`SCAN_INTERVAL` in
`crates/dispatcher/src/core.rs`) and already handles time-driven transitions
(task timeouts, job deadlines, heartbeat lapse). A schedule scan is one more
`scan_*` call in `run_scans`, and it inherits the single-writer property for
free — which is what stops a restart from double-firing a schedule, provided
last-fired is persisted rather than held in memory.

Shape it as **repo-versioned config**, following §13.3's precedent for
factories: `.chug/schedules/{name}.yaml`, read from the default branch HEAD,
resolved through `crates/types/src/config_paths.rs` like every other config
directory. That keeps the per-consumer-forge principle (CLAUDE.md: "factories
and job-type config are project-owned and repo-versioned") — a schedule change
ships in the same commit as the job type it fires, gated by the same CI.

Two design points a schedule spec must answer, both of which have obvious
answers in this codebase and neither of which should be left implicit:

- **Missed ticks.** A dispatcher down for six hours: fire once on recovery, or
  not at all? The §13.4 factory answer ("at most one triage job per factory in
  flight") is the right shape — at most one in-flight job per schedule, and
  catch-up collapses to a single fire.
- **Overlap.** A nightly job still running at the next nightly. Same answer:
  skip, and emit an event, rather than queueing.

### F. Mobile (2.5 workflows)

iOS and Android fastlane, plus the iOS half of the integration-test job.
Impossible under containers — not hard, impossible: Xcode does not run in a
Linux container, and `xcrun simctl` needs a macOS host. The spec already
concedes this in Appendix: Deferred — "**macOS bare metal dispatchers**:
required for Xcode builds. Execution model needs separate design."

**Section H is that design.** It is worth being explicit that this is the only
category where no amount of container cleverness helps.

beacon's emulator workarounds — a writable `integration_test` plugin copy under
`RUNNER_TOOL_CACHE`, a `jq`-patched `.flutter-plugins-dependencies`, `-gpu
swangle` for a documented SIGSEGV — are adaptations to *GHA's* runner
environment, not portable facts. Expect to re-derive them against a NixOS/macOS
host, not copy them.

### G. Delete (3 workflows)

`gcp-iam-test` (stale registry reference), `test-precommit-hook` (a diagnostic
that outlived its bug), `rust-coverage` (becomes an ordinary job — coverage is a
thing you ask for, not a thing that runs on every push).

## H. Host-native workers

A third node kind that runs tasks as **host processes** in a NixOS or macOS
environment instead of inside a container. This **adds** a node kind; it does
not replace containers. A mixed fleet is already the implemented reality, so
this can be prototyped on one node without migrating anything.

### H.1 Why the existing abstraction supports it

- `ContainerBackend` (`crates/container/src/lib.rs`) is the **only** launch
  seam. Per correction 7, it has exactly three implementations, and their shape
  is the argument:
  - **`DockerBackend`** (`crates/container/src/docker.rs:378`) — the one real
    execution backend, and the one the trait's shape was derived from.
  - **`FleetBackend`** (`crates/worker/src/backend.rs:539`) — the load-bearing
    precedent. It is *not a local docker daemon at all*: for its
    `NodeHandle::Worker` variant it satisfies the same trait over **NATS
    request-reply** to a remote daemon, converting `ContainerLaunchConfig` to a
    `WorkerLaunchRequest` on the wire and polling `wait` rather than blocking.
    That the trait already spans "syscall to a local socket" and "RPC to another
    machine" is the strongest available evidence that it generalizes past a
    socket.
  - **`FakeBackend`** (`crates/test-utils/src/lib.rs:343`) — an implementation
    with no containers behind it at all. Weaker evidence (it answers to tests,
    not to reality), but it does show the trait is satisfiable without a runtime.
  - `crates/container/src/k8s.rs` is a **stub**, not a fourth implementation —
    a reserved slot. Do not count it.
- `FleetBackend` also already drives a **mixed** fleet through its `NodeHandle`
  enum — docker endpoints driven directly, worker nodes proxied over NATS. A
  host node is a third variant, or (better) no new variant at all: the *worker
  daemon* selects a host backend locally and the dispatcher never learns the
  difference.
- `placement.node` pinning already exists (§3.1), so routing work to the one
  host node needs no new mechanism on day one.
- The Mini already runs the dispatcher and api natively
  (`deploy/prod/install-launchd.sh`), so "a platform process outside a
  container" is not a new operational category.

**Correction 5 above applies here:** the daemon holds a concrete
`DockerBackend` (`crates/worker/src/daemon.rs:196`), so "the daemon selects a
backend locally" requires making that field polymorphic first. Small, contained,
and worth doing regardless — it is the change that keeps the dispatcher out of
this entirely.

**And the roster cuts both ways.** It is good evidence that the seam is not
welded to a local socket. It is *not* evidence that the seam is agnostic to
containers — every implementation above ultimately drives one, or pretends to.
That is a real limit on how much H.1 can carry, and H.3 prices it as cost 1
rather than burying it.

### H.2 What it buys

- **Unblocks mobile entirely** (category F). Nothing else does.
- **Dissolves the cache gap** (category B's biggest loss): persistent gradle,
  pub, `node_modules`, sccache, buildx caches — which is exactly how gumbo
  works today.
- **Dissolves the image-build question** (category D): a host node has a real
  docker daemon, the same way gumbo does.
- **`/dev/kvm`** for the Android emulator.
- **A nix flake replaces `image:`** with comparable reproducibility — both are
  pinned, content-addressed environment references. That equivalence is why the
  schema change stays small (H.3, cost 2).

### H.3 What it costs — honestly

None of these are blockers. All of them are real. They are ordered by how much
is *unknown*, not by how much work each is — cost 1 is first because it is the
one nobody in this tree has priced.

1. **The trait's ten required methods assume container semantics — and this is
   where the uncertainty concentrates.** `copy_file`, `remove` (documented as
   overlay reclaim — "a cargo-building job leaves 5–10 GB per task in its
   overlay"), `list_managed_exited` / `list_managed_running`, and the
   **byte-offset `logs_tail` contract** all need host analogues: a pid/cgroup
   instead of a container id, a per-task workdir, a state file the listings can
   read, and a log file — where, notably, byte offsets get *simpler*, since the
   doc comment's "byte offsets are stable — container logs are append-only" is
   trivially true of a file.

   Rank this cost first, not last. H.1's roster shows the seam generalizes past
   a *local socket* — `FleetBackend` proves that over NATS — but every
   implementation in the tree still bottoms out in a **container runtime**, and
   the trait's vocabulary (`ContainerId`, `ContainerLaunchConfig.image`,
   `RunningContainer`) was derived from Docker alone. Nobody has yet built a
   backend whose unit of execution is not a container, so the honest position is
   that the *number* of host analogues is knowable from the trait definition and
   their *difficulty* is not. That asymmetry is the whole reason the Ordering
   section makes phase 2 a **prototype on one node** rather than a design
   carried to completion: the cheapest way to price this cost is to pay a bit of
   it.
2. **The schema change is the risky part.** `image` is *required* for
   command/agent work; a host node needs a different selector. Per correction 4,
   the danger is not an unknown field (tolerated, §14.2) but a **missing
   required** one: an N−1 dispatcher rejects the config outright. This **must**
   bump `CONFIG_SCHEMA_EPOCH` (`crates/types/src/version.rs`) and gate with
   `min_dispatcher` **in the same commit** (§14.1), or it reproduces the
   2026-07-22 `wrap_up` escalation storm that §14 exists to prevent. The runtime
   park (§14.2 — Stalled, not Escalated) is the fallback, not the plan.
3. **Resource limits weaken.** `systemd-run` recovers most of `resources:` on
   NixOS. macOS has no cgroups, so limits become advisory — meaning `resources:`
   would **silently lie** on a macOS node. Silent lying is the failure mode
   STYLE.md's "everything is bounded / fail fast and loud" rejects; prefer
   rejecting `resources:` on a platform that cannot honor it over accepting it
   and ignoring it.
4. **Isolation traded for cache persistence.** The reuse *is* the win and *is*
   the contamination risk; they are the same property. Hosted runners buy the
   isolation back by destroying the VM; gumbo simply accepts the risk. There is
   no third option, and the trade should be made explicitly rather than
   discovered.
5. **Env-injected secrets on a shared host** are visible to other processes of
   the same user. §10.2 keeps plaintext narrow today by scoping secrets to one
   container; a host node widens that to "one unix user".
6. **`.chug/tasks/ci.sh` assumes a docker socket or a baked `nats-server`** —
   the agent-rust image provides the latter (`deploy/prod/Dockerfile.agent-rust`).
   A host node has neither by default; the nix profile must supply them, or
   tier-2 tests self-skip and the gate goes silently partial — which `ci.sh`'s
   own tier-summary logic was written to prevent.

### H.4 Worked example: `flutter-integration-tests`

This one workflow needs **both** cron and host execution, which is why it is the
best illustration and the worst first target.

- The iOS leg boots a simulator via `xcrun simctl`. Not hard in a container —
  impossible.
- The Android leg checks whether an emulator is **already running** via `adb
  devices` and reuses it, talking to a host-level adb server. That is
  definitionally host-state-dependent: there is no container-shaped version of
  "reuse the thing that is already there".
- The AVD `beacon-emu` **persists across runs** and is created only if absent.
  In a container that check fails every night, so it re-downloads the system
  image forever.
- `-gpu swangle` and `-no-snapshot` are empirical fixes for a documented
  emulator SIGSEGV about 11 minutes in — evidence that this environment is tuned,
  not declarative.

### H.5 The new primitive this surfaces: exclusive resources

The workflow uses one concurrency group with `cancel-in-progress: false`,
because two runs fight over the same simulator and AVD.

Chuggernaut places by **slots per node** (§3.1: free slots = `slots − running`).
A 2-slot host node will happily run two tasks that collide on `beacon-emu`.
`placement.node` **routes but does not make exclusive** — §3.1 is explicit that
the pin is the *only* affinity control and that there are "no labels, no
anti-affinity".

So host mode surfaces a genuinely new scheduling concept: a **device lease /
exclusive resource** — a named token a job type declares, of which the fleet
grants one holder at a time. Note that the existing capacity queue is the right
place to express it: an unavailable lease should behave like
`BackendError::NoCapacity` (transient, queued, retried, no retry budget
consumed) rather than a launch failure. That reuse is what keeps this from
being a second scheduler.

A cheap interim: pin such job types to a **1-slot node**. It is not a general
answer, and it wastes the node, but it unblocks a prototype without inventing a
primitive.

### H.6 NixOS layering: where tooling lives

A node running both modes needs an answer to "where does flutter's version
live", and the tempting answer is wrong.

**Resist putting per-project tooling in the node's NixOS configuration.** Nix
offers two mechanisms and they map cleanly onto the split:

| Mechanism | Owned by | Changes via | Holds |
| --- | --- | --- | --- |
| **System closure** (`nixos-rebuild`) | operator, root | a drain + rebuild | machine facts: docker daemon, nix daemon and caches, `/dev/kvm`, users, the worker unit |
| **Flake devshell** (`nix develop`) | the project repo | `git push` | per-project toolchains: flutter, gradle, the Rust toolchain |

Putting flutter's version in the node config rebuilds the image problem with
worse ergonomics, and makes the node a **central control plane** — which
CLAUDE.md rejects outright ("factories and job-type config are project-owned and
repo-versioned — a per-consumer forge"). Toolchains *are* job-type config. Put
`flake.nix` in the project repo and a tool bump ships in the same commit as the
code that needs it, gated by the same CI.

**Three clocks, plus a fourth thing that has no clock:**

1. **System closure** — operator, needs a drain.
2. **Worker daemon** — platform, `deploy/prod/worker-refresh.sh`.
3. **Task environment** — project repo, `git push`, **no deploy at all**.
4. **Declared mutable caches** — not versioned by any of the above; see the GC
   friction below.

Consequences:

- **`image:` and a flake ref occupy the same slot.** Both are pinned,
  content-addressed environment references, which is why H.3's schema change is
  one selector field rather than a new execution model in the config.
- **The worker daemon must go native on mixed nodes.** It is containerized today
  — `deploy/prod/build-worker.sh` runs `docker run -d --restart=always --name
  chug-worker -v /var/run/docker.sock:/var/run/docker.sock …` — and a
  containerized worker cannot spawn host processes on its parent. So
  `deploy/prod/worker-refresh.sh` gains a second deployment shape. Note this
  interacts with §3.1's self-refresh design, which is written around a daemon
  that replaces its own container via a detached sibling; a native daemon needs
  the systemd-unit analogue of that dance.
- **The announce must advertise capability.** `WorkerAnnounce`
  (`crates/types/src/worker.rs`) carries `slots` and nothing about what the node
  *can do*. It needs `modes: [container, host]` plus a platform, as an
  **additive optional** field so an omitting daemon reads container-only — the
  N±1 rule (§14.1). That gives capability-based placement nearly free. Per
  correction 6, sequence this with [design #293](293-worker-capacity.md), which
  is already changing this struct.
- **Drain already has a hook.** `schedulable` in `crates/worker/src/backend.rs`
  stops placement while keeping the node routable — exactly what
  `nixos-rebuild` needs. Wire an **explicit drain op** rather than leaning on
  heartbeat lapse, which is semantically wrong: a lapse means UNHEALTHY, a drain
  means DRAINING, and an operator staring at the fleet view needs to tell them
  apart. (`slots: 0` per §3.1 is the specced full drain, but the operator
  capacity API that sets it is design #293, not yet in the tree.)

**Frictions worth naming now:**

- **GC versus in-flight tasks.** Every (project × lockfile) accumulates in
  `/nix/store`. This needs a GC policy *and* a GC root held for a task's
  duration, or a nightly `nix-collect-garbage` will delete a running task's
  toolchain.
- **No pull phase.** A cold `nix develop` is an image pull, but there is no
  concept of one — so it lands **inside** `task_timeout` and looks like a slow
  job. Wants a binary cache and a pre-warm step.
- **Evaluating a project flake runs project-controlled code on the host** as the
  worker user. Host mode and multi-tenancy pull against each other; on a
  single-tenant node this is acceptable, on a shared node it is not.
- **Nix manages tools, not state** — and the state (AVDs, `~/.gradle`, buildx,
  sccache) is precisely why host mode is wanted. Declare it per (project,
  purpose) and namespace it in the worker, the way `WORKER_CACHE_DIR` is
  namespaced today.

## Gaps, ranked

| # | Gap | Why it ranks here |
| --- | --- | --- |
| 1 | **Job inputs / parameterization** | ~~Blocks rollback~~ — inputs landed ([#311](./311-job-inputs.md) slice A; `.chug/jobs/rollback.yaml` is the first consumer), so the deploy-file collapse is available too. Matrix / fan-out stays excluded, with reasons (#311 Decision 7). Still ranked first for what it unlocked: not in the spec at all, most structural, most underestimated |
| 2 | **Cron** | One live workflow, but the trigger class the whole category-E port depends on |
| 3 | **Host-native execution** | New backend + daemon polymorphism + schema epoch bump |
| 4 | **Keyed caching** | Narrower than the brief assumed (correction 3), still real for gradle/pub/npm/buildx |
| 5 | **Artifacts** | `crates/store/src/artifacts.rs` holds transcripts, stdout and attachments; there is no inter-job binary handoff (Appendix: Deferred, "Binary artifact store") |
| 6 | **OIDC issuer prerequisite** | Needs a *publicly reachable* JWKS endpoint; infra, not just code |
| 7 | **Outbound webhooks** | `crates/webhooks/src/lib.rs` is a stub; blocks `sentry-resolve` |
| 8 | **Auto-merge vs human-merge default** | A policy choice, not a mechanism gap (correction 2) |
| 9 | **Node-level exclusive resources** | Only bites once host nodes run device-bound work (H.5) |

Gap 1 deserves its rank. Twelve deploy workflows differ by two strings. Every
one of them becomes a job-type file that differs by two strings, because a job
type is a static file and a job carries no parameters.

The mechanism one would reach for first — a job-level override — is already
ruled out, and for a good reason:
[design-lifecycle.md](../../design-lifecycle.md) ("eval criteria are a floor,
additive per job") rejects full per-job overrides because one "would let a job
creator silently drop the type's merge-gate protections", and permits only
*additive* evaluators. It also draws exactly the distinction a parameterization
design needs: "`workflow_dispatch` parameterizes a run, it does not rewrite the
steps."

That is the frame to start from. A job **input** — a typed value substituted
into `work.run`, declared and constrained by the job type — parameterizes a run
without rewriting it, and cannot weaken a gate. That is a much narrower thing
than a matrix, and it is the thing rollback actually needs. Designing it is out
of scope here; starting it anywhere other than that constraint would be
re-litigating a settled decision.

**Landed, with one correction to the sentence above.** [#311](./311-job-inputs.md)
took the frame and tightened it: an input is *never* substituted into `work.run`
or into any other job-type field — no substitution engine exists — it is
delivered to the running container as `$CHUG_INPUT_{NAME}`, and the
parameterization happens inside the work script where it always belonged
(#311 Decision 1). Same guarantee, reached structurally rather than by rule.

## Decisions

### Taken

**GCP auth: Chuggernaut becomes an OIDC issuer,** with a second
`workload_identity_pool_provider` on the GCP side. This keeps the current
keyless posture rather than regressing to long-lived service-account keys.
beacon's existing WIF binding is to `token.actions.githubusercontent.com` with a
repo claim, so it **cannot** be reused as-is — the port needs a new provider
regardless, which is why "issue our own" costs little more than "reconfigure
theirs".

The platform already signs RS256 JWTs and holds the keypair (§7.1, §12.1), so
the missing pieces are a **public JWKS endpoint** and a stable issuer URL.
Reachability is the real constraint: gumbo-mini-0 is tailnet-only by default —
but `deploy/prod/README.md` §5b already documents a `cloudflared` path for
exposure beyond the tailnet, so this is a documented configuration change rather
than new infrastructure. Scope the exposure to the JWKS path only.

**Triggers: cron only.** Not ingest, not factories, not push/PR. Ingest (§13.2)
and factories (§13.3) are an *events → triage agent → jobs* pipeline; a nightly
test run is not a triage problem, and routing it through an agent adds tokens,
latency and nondeterminism to a thing that should be a timer. Push/PR triggers
are explicitly not wanted: the whole point of the merge gate (§3.3) is that
validation happens on the way *in*, not after the fact.

### Open decisions

Both are presented without a recommendation, deliberately.

**D1 — image builds: scoped socket vs rootless builder.** See category D. The
tiebreaker is phase 2: if host-native nodes land, the question dissolves, and
choosing now risks building the wrong thing twice. Recommend deciding *after*
the phase-2 prototype reports, not before.

**D2 — auto-merge vs human-merge default.** Chuggernaut auto-squash-merges on
eval pass; beacon requires a human to merge. Per correction 2 this is a default,
not a capability: a `human` evaluator at the highest stage expresses beacon's
policy today. The real question is which default a *project* gets, and it is
genuinely a values question — throughput versus a human checkpoint — not a
technical one. It should be decided by whoever owns the risk, per job type,
rather than argued here.

## Ordering

Not a commitment — a dependency reading.

| Phase | Work | Depends on |
| --- | --- | --- |
| 0 | Land this doc | — |
| 1 | CI as evaluators (category B) | — |
| 2 | Host-exec backend prototype on one node (H) | — |
| 3 | Cron (category E) | — |
| 4 | OIDC issuer + JWKS + WIF provider | infra exposure |
| 5 | Deploys, forward-only (category C) | 4 |
| 6 | Image build and push (category D) | 2 or 4 |
| 7 | Node-level exclusive resources (H.5) | 2 |
| 8 | Mobile and simulator jobs on host nodes (F) | 2, 3, 7 |
| 9 | Job inputs → unblocks rollback (**landed**, [#311](./311-job-inputs.md) slice A) | — |

Phases 1, 2, 3 and 9 are mutually independent.

**Phase 1 is the highest value for zero new capability** — it uses only
mechanisms that already ship.

**Phase 2 is the highest leverage**: it feeds 6, 7 and 8, and retires gap 4
outright. Start it early, and start it as a *prototype on one node* rather than
a design carried to completion. H.3's cost 1 is exactly the kind that resolves
faster by contact than by argument: the ten host analogues are enumerable from
the trait today, but nothing in the tree tells you which of them is hard.

**`flutter-integration-tests` is a good north star and a bad first target.** It
sits at the confluence of 2, 3, 7 and 8, which makes it a useful thing to
sequence *toward* and a terrible thing to attempt first. Prove phase 2 against a
boring, cache-heavy build instead.

## What this doc does not decide

- The schema *syntax* for a host-mode selector (H.3 fixes only that it needs an
  epoch bump and a `min_dispatcher` gate in the same commit).
- The schedule file format (E fixes only the location and the two semantics
  questions).
- Anything in gap 1. Job parameterization is the largest gap here and it
  deserves its own design doc, starting from `design-lifecycle.md`'s existing
  constraints on per-job overrides rather than from this one.
- Whether beacon's non-CI GHA usage (release notes, label automation, anything
  the survey classified into A) is worth reproducing at all, versus dropping.
