# Design #355 — Project-supplied task images (#308 gap 12)

Status: PROPOSED — nothing built, unowned; only its epoch sequencing rule is live.

Nothing this document proposes exists in the tree: no
`PROJECT_IMAGE_SCHEMA_EPOCH` in `crates/types/src/version.rs`, no `build_image`
worker op, no project-supplied image anywhere. It concludes **O2** — the worker
daemon builds a project's task image as it already builds its own — with O1
(registry plus a launch-time pull) as the named successor under a stated
trigger ([The options](#the-options)). It waits on
[#309](./309-host-native-execution.md) §4's `NodeCapabilities` for the placement
filter, and on an epoch it deliberately does not number. No slice table: this
sequences an epoch, not slices.

Live already: [§3](#3-the-epoch-and-the-sequencing-rule)'s sequencing rule — the
epoch is a counter, not a reservation, and each feature freezes its own constant
— adopted from [#313](./313-workload-identity-image-builds.md) and followed by
every epoch bump since.

The body was verified against the tree at `fce9e33`, and its disagreements with
the brief are in [Corrections](#corrections-verified-against-the-tree). Two
qualifications carry forward: the per-level `image` fallback it counts in four
dispatcher call sites is one accessor today, `JobType::level_image` (job #507),
though the three `pub image: Option<String>` declaration sites its argument
rests on are unchanged; and its claims about the beacon repository rest on the
survey in #308, since beacon is not checked into this tree.

---

## Problem

A job type declares the container image its work and eval containers run in
([`docs/spec.md`](../spec.md) §1.1: `image` is **required** for `work.type: agent |
command`). A project cannot supply that image. It can only name one of the three
the platform builds for itself, so the only way to get a new toolchain into a
job is to add it to chuggernaut's own Dockerfiles — which is exactly the shape
[`CLAUDE.md`](../../CLAUDE.md) rejects: *"factories and job-type config are
project-owned and repo-versioned — a per-consumer forge, not a shared control
plane."*

The operator requirement that makes this urgent, and the acceptance test for
every answer below:

> **A project must be able to update its own task image without a full
> chuggernaut redeploy.** Image freshness runs on the project repo's clock, not
> the platform's.

Today the two clocks are welded together: the *only* thing that rebuilds a task
image is `deploy/prod/worker-refresh.sh` running at the platform's deployed SHA,
driven by a deploy job.

## What is true today (verified)

1. **Every job type in this repo names one of two platform images.**
   `.chug/jobs/code.yaml`, `design.yaml`, `docs.yaml`, `web.yaml` and
   `web-publish.yaml` declare `chuggernaut/agent-rust:prod`; `deploy.yaml` and
   `rollback.yaml` declare `chuggernaut/agent:prod`; `.chug/jobs/_defaults.yaml`
   pins the appended `ci` evaluator to `chuggernaut/agent-rust:prod`.
   `.chug/jobs/manual.yaml` declares no top-level image (human work), as §1.1
   requires.

2. **Exactly three images exist and they are hardcoded in a shell script.**
   `deploy/prod/worker-refresh.sh` builds `chuggernaut/worker`,
   `chuggernaut/agent` and `chuggernaut/agent-rust` **on each node**, tagged
   locally and consumed locally. The names are literals in the build, verify,
   retag-swap and cleanup steps; there is no list to extend.

3. **There is no pull path.** `grep -rn "create_image\|docker pull\|CreateImageOptions\|RegistryAuth" crates/`
   returns exactly one hit, `crates/test-utils/src/backend_suite.rs` — a test
   fixture. `DockerBackend::launch` (`crates/container/src/docker.rs`) calls
   `create_container` directly with `image: Some(config.image.clone())` and no
   preceding image fetch, so a missing image surfaces as the create call's error
   mapped to `BackendError::Launch`.

4. **A `Launch` error fails the task; only `NoCapacity` queues.**
   `crates/dispatcher/src/launch_queue.rs` states the rule in its own module
   header: a `NoCapacity` launch is *"queued rather than failed"* and parked
   `Pending` with *"no retry budget consumed"*, while
   *"genuinely-unreachable-node and other launch errors keep today's
   fail-the-task semantics."*
   `MAX_QUEUE_WAIT` bounds a deferred launch at 30 minutes before
   `QUEUE_TIMEOUT_REASON = "no_free_slots_timeout"`. So "the node does not have
   the image" is currently a `work_retries`-burning task failure whose only
   diagnosis is the container log.

5. **`ContainerLaunchConfig` assumes the image is already there.**
   `crates/container/src/lib.rs` defines it as `{ image, cmd, env, files,
   cpu_limit, memory_limit, node }` — no registry, no auth, no pull policy.
   ([#313](./313-workload-identity-image-builds.md) correction 4 found the same
   thing for a different reason.)

6. **The daemon already builds images, and that is the precedent.**
   `docs/spec.md` §3.1 "worker self-refresh" specifies a `refresh { sha, tag }` op on
   `req.worker.{node}.>` (`RefreshRequest` in `crates/types/src/worker.rs`,
   dispatched in `crates/worker/src/daemon.rs`). The daemon fetches its own
   build context over the ssh front and runs three `docker build`s locally. No
   job container ever holds a docker socket.

7. **That fetch can only take an advertised ref.** `worker-refresh.sh` fetches
   `HEAD` and then asserts `FETCH_HEAD == $SHA`, because the bare repos enable
   only `uploadpack.allowFilter` (`crates/vcs/src/lib.rs`, `ensure_upload_filter`)
   and **not** `allowAnySHA1InWant` — a `want <raw sha>` fetch is refused. Spec
   §3.1 records the same constraint.

8. **The disk floor is already the binding constraint on node builds.**
   `worker-refresh.sh` refuses a build below `DISK_FREE_GB_MIN` (default 30GB),
   with a derivation history spanning deploys #248, #347, #351 and #352 — the
   last of which deleted the `agent-rust` warm-target seed specifically to bring
   the peak down.

9. **`CONFIG_SCHEMA_EPOCH` was `2`** when this was written
   (`crates/types/src/version.rs` is the authority today), bumped for
   job `inputs:` (#311), with `INPUTS_SCHEMA_EPOCH = 2` frozen beside it.

10. **The spec's own examples already assume registry-qualified images.**
    `docs/spec.md` §1.1 shows `image: registry.acme.com/agents/impl:latest` and
    `image: registry.acme.com/runners/deploy:latest`. Nothing in the code makes
    those work. The Appendix: Infrastructure Summary leaves the row open:
    `| Image registry | Harbor or Zot | ECR, GCR, GHCR |`.

11. **Config directories are already generic.** `dispatcher::project_config` has
    a `pub(crate) fn entries(tree, dir, suffix)` — "every blob sitting directly
    in config directory `dir` with extension `suffix`, at either location" — and
    `types::config_paths::config_path_candidates` resolves `.chug/<dir>/` with a
    repo-root fallback. A new config directory costs no new resolution code.

## Corrections (verified against the tree)

**1. [#308](./308-gha-port.md)'s phase 1 is not free, and this gap is why.**
The #308 ordering section says *"Phase 1 is the highest value for zero new
capability — it uses only mechanisms that already ship."* Phase 1 is category B,
"CI as evaluators", listed there as `rust-ci`, `flutter-ci`, `creator-tests`,
`code-duplication`, `terraform-validate`, `actions-validate`. A command evaluator
runs in an image, and the image it would run in is `chuggernaut/agent-rust:prod`,
which bakes `rust:1.96-bookworm`, `nodejs`, `git`, `openssh-client`, `ripgrep`,
`jq`, `curl`, `sccache`, a `nats-server` and `@anthropic-ai/claude-code`
(`deploy/prod/Dockerfile.agent-rust`). It bakes no flutter, no gradle, no
terraform. So the "zero new capability" claim holds for `code-duplication` and
`actions-validate` and fails for the rest: the cheapest phase in #308's plan has
an **unbudgeted dependency on a capability that does not exist**. It is absent
from #308's ranked gap table; it is new, and it is **gap 12**. Amending #308 is a
separate `docs` job — this document states the correction, it does not apply it.

**2. Which toolchains beacon's workflows need is asserted, not verified.** The
beacon repository is not in this tree. `grep -rn "beacon-repo\|us-central1-docker\|APP_GCP_PROJECT"`
over every `.md`, `.rs`, `.sh` and `.yaml` here returns nothing. The brief's
claims that beacon already pushes to
`us-central1-docker.pkg.dev/${APP_GCP_PROJECT}/beacon-repo` and `beacon-web-repo`
are therefore **taken on trust from #308's survey**, not confirmed. Correction 1
above does not depend on them: it depends only on what `agent-rust` bakes, which
is verifiable here, plus #308's own list of category-B workflow names.

**3. #313 says `CONFIG_SCHEMA_EPOCH` is "currently `1`". It is `2`.**
[#313](./313-workload-identity-image-builds.md)'s "Skew: this field costs an
epoch bump" parenthesizes *"currently `1`, `crates/types/src/version.rs`"*;
[#322](./322-macos-native-runtime.md) correction 1 already caught the same
staleness in its own brief. The rule #313 states around that stale number is
nonetheless the right one and this document adopts and extends it — see
[The epoch, and the sequencing rule](#3-the-epoch-and-the-sequencing-rule).

**4. #313's half B is about a different kind of image than this document.**
Design #313's half B is about images a project **produces and ships** — beacon's app
containers, pushed to a registry, deployed to Cloud Run. Gap 12 is about the
image a job **runs inside**. They share a build mechanism question and share
nothing else: a product image is an artifact of the work, a task image is a
precondition of the work, and only the second one has to exist *before* the
dispatcher can launch anything. Conflating them is how "just use #313 half B"
looks like an answer when it is not one — see
[Decision 0](#decision-0-task-images-are-not-product-images).

---

## Decision 0: task images are not product images

| | Task image (this doc) | Product image (#313 half B) |
| --- | --- | --- |
| Who consumes it | the **dispatcher**, at launch | a deploy target (Cloud Run, k8s) |
| When it must exist | before the first task of the job runs | after the build job succeeds |
| Failure if absent | no job of that type can run at all | one job fails, loudly, in its own log |
| Needs a registry | only if distribution is by pull | **yes, by definition** — that is what shipping means |
| Push credential | none — nothing is published | yes (#313 B2) |

Two consequences that shape everything below.

- **A task image sits on the platform's critical path.** If fetching it can
  fail, *launching* can fail — and by fact 4 a launch failure is a task failure
  that burns `work_retries`. That is a much higher bar than "a build job failed."
- **A task image needs no push credential and no publish step.** #313 half B's
  hard prerequisite — half A's workload identity, so a build can push keylessly
  — is not a prerequisite here at all *unless* we choose to distribute by
  registry. That is a large, avoidable dependency, and it is the single biggest
  input to the recommendation.

---

## The options

### O1 — build once, push to a registry, pull at launch

A `build-image` job type builds the project's task image and pushes it; nodes
pull it when a launch names it.

*For.* One build for the whole fleet. Distribution scales to any number of nodes.
It is the shape `docs/spec.md` §1.1's own examples already assume (fact 10), and the
shape every other CI system uses. It composes with #313 half B: one build
mechanism serves product and task images alike.

*Against — four costs, each verified.*

- **A pull path does not exist** (fact 3) and is not one line: it needs a
  `create_image` call in `crates/container/src/docker.rs` *and* in the worker
  daemon's launch path, a pull timeout, a retry policy, and a decision about
  whether a pull failure is `Launch` (fails the task) or `NoCapacity` (queues).
- **A standing pull credential on every node.** For Artifact Registry that is a
  service account with the **roles/artifactregistry.reader** role on the repository, present
  on the node, refreshed, and rotated. **This is a different IAM binding from the
  push one**, which #313 B2 scopes to `attribute.workload` = a specific (project,
  job type, container) and grants **roles/artifactregistry.writer**. The push
  binding is per-workload and keyless; the pull binding is per-*node* and
  standing, because the puller is the daemon and the daemon has no workload
  token. Half A does not remove this credential; it removes the *push* one. Any
  plan that says "keyless" about O1 is talking about the wrong half.
- **It puts a network dependency on every launch.** Today a node with the images
  can run jobs with the registry, the internet and the cloud provider all down.
  Under O1 a registry outage is a fleet-wide launch outage. That is a real
  reliability regression against a platform whose §3.1 self-refresh design exists
  precisely so *"each node rebuilds its own copies rather than pulling them"*
  (#313 correction 2).
- **The registry does not exist.** #308 gap 11 and the spec's Infrastructure
  Summary agree: it is operator infrastructure nobody has provisioned. Per
  correction 2 above, the brief's argument that this "collapses to an IAM
  binding" for beacon rests on beacon facts this tree cannot check — and even
  granting them, it collapses to *an IAM binding plus a pull binding plus a pull
  path plus a launch-time network dependency*.

### O2 — the worker daemon builds project images, as it already builds its own

Extend the daemon with a `build_image` op alongside `refresh`: the dispatcher
tells a node *"build image `<name>` for project `<owner>/<project>` from
Dockerfile `<path>` at ref `<ref>`"*; the daemon fetches the context over the ssh
front exactly as `worker-refresh.sh` does and runs `docker build` locally.

*For.*

- **The precedent is in the tree and is load-bearing.** #313 correction 3 states
  it as a rule: *"image building is already a worker-daemon operation, not a
  container capability … the daemon builds; job containers never do."* Every
  hard part — fetching a context without shipping bytes over NATS, staging to a
  temp tag, verifying a SHA label before the retag-swap, cleaning up on failure
  and on cancel, a disk pre-flight, phase markers streamed back through `ping` —
  is already written, tested (`deploy/prod/worker-refresh.test.sh`) and operated.
- **No registry, no pull path, no standing credential, no new network dependency
  on the launch path.** The three costs that dominate O1 all go to zero.
- **It satisfies the operator requirement directly.** A project changes its
  Dockerfile, merges to its default branch, and the reconciler rebuilds — with no
  chuggernaut deploy anywhere in the loop.
- **The blast radius is the one we already accept.** The build runs on the
  daemon's socket, but the *build* does not get the socket; a `RUN` step is
  arbitrary project code with network access, which is what every job container
  already is. See [Trust](#8-trust-what-a-project-dockerfile-actually-reaches).

*Against — priced honestly.*

- **N× build cost.** Every node builds every project image it must serve. On a
  three-node fleet with one project that is three builds; it does not stay small.
- **N× disk, against a floor that already bites.** Fact 8: the platform's own
  three images already make `DISK_FREE_GB_MIN = 30` a real gate that *refuses*
  builds, and #352 shipped specifically to shrink the peak. Adding a multi-GB
  flutter/android image per node is the same pressure again, per project.
- **A reconciliation story that does not exist yet** — which node holds which
  project image at which SHA, who notices drift, and what a launch does
  meanwhile. This is genuine new platform surface and [§5](#5-reconciliation-what-triggers-a-rebuild-and-who-owns-it)
  and [§6](#6-a-launch-that-lands-on-a-node-without-the-image) are most of this
  document because of it.
- **The ref constraint of fact 7.** The daemon cannot fetch an arbitrary SHA, so
  a project image is built from an *advertised ref* — in practice the project's
  default branch tip — never from an in-flight job branch. This is a real
  limitation and, as [§5](#5-reconciliation-what-triggers-a-rebuild-and-who-owns-it)
  argues, also the correct policy.

### O3 — hybrid: build once on a builder node, distribute by registry

Or: build per-node, lazily, on first launch.

*For.* The first variant is where a large fleet ends up: one build, N pulls.

*Against.* The first variant is **O1 with an extra step** — it still needs the
pull path, the per-node pull credential and the registry, and adds a designated
builder node whose failure blocks the fleet. It buys only the N× build cost,
which is the *cheapest* of O2's three costs (build is CPU on otherwise-idle
nodes; disk and reconciliation are the ones that hurt).

The second variant — build lazily at launch — is **rejected outright**. A build
that runs inside a launch either blocks `ContainerBackend::launch` for ten
minutes (the dispatcher's actor turn is single-threaded by design; blocking it on
a node build is the wrong shape) or runs inside the task and eats
`resources.task_timeout`. `.chug/jobs/code.yaml` sets `task_timeout: 1h` to cover
a cold cargo build; a flutter image build inside that budget would make the
timeout mean two unrelated things at once.

### Recommendation: **O2**, with O1 named as the successor and a stated trigger

Take O2. The argument is not that O2 is better in the limit — it is not; O1 wins
on a large fleet — but that **O2's costs are node-local and O1's are
architectural**. O2's three costs (build CPU, disk, reconciliation) are all
things the node and the dispatcher already reason about. O1's costs (a registry,
a standing per-node cloud credential, a network dependency on the launch path)
are all things that do not exist and one of which is a reliability regression.

**And the reconciliation work O2 forces is not wasted under O1.** Desired-state
computation, per-node image inventory, capability-filtered placement, staleness
detection and GC are needed identically whether the node *builds* the image or
*pulls* it. Only the verb changes. So O2 is not a detour toward O1; it is the
first two thirds of O1 with the third that requires new infrastructure deferred.

**The trigger to revisit.** Switch the distribution verb to pull when any of:
(a) the fleet exceeds roughly six nodes, where N× build stops being amortisable;
(b) a project image exceeds the node disk budget of
[§7](#7-disk-accounting-and-gc) on a node that must also serve platform images;
or (c) #313 half A ships and a registry is provisioned for half B anyway, at
which point the marginal cost of the pull path is the pull path alone. The
declaration in [§1](#1-the-declaration) is deliberately silent about *how* the
image reaches the node, so that switch is a daemon-and-dispatcher change with no
project-repo churn.

---

## 1. The declaration

**Where: `.chug/images/<name>.yaml`, one file per image**, in the project repo,
mirroring `.chug/jobs/<name>.yaml` and `.chug/tags/{tag}.md`. Spec §1.1 makes `.chug/`
the config root; `dispatcher::project_config::entries` already lists any flat
config directory at either layout (fact 11), so the resolution code is reuse, not
new.

```yaml
# .chug/images/ci.yaml
name: ci                       # must equal the file stem
dockerfile: .chug/images/Dockerfile.ci
context: .                     # repo-relative build context root; default "."
ref: main                      # advertised ref to build from; default: the project's default branch
build_args:                    # optional, static; never a job input
  FLUTTER_VERSION: "3.32.0"
```

`ref` defaults to the project's default branch as the `HEAD` symref resolves it
(spec §12.2: *"the default branch name is stored in the git repository's `HEAD`
symref"*), which for a **linked-origin** project is `integration`, not origin
main (§5.3). A linked project that writes `ref: main` names a ref its local bare
repo does not have; the default is what it wants, and this is the argument for
having a default at all.

Three alternatives were weighed for the location:

| | A: `.chug/images/<name>.yaml` | B: one `.chug/images.yaml` map | C: inline in the job type  <!-- absent --> |
| --- | --- | --- | --- |
| Matches existing layout | **yes** (`jobs/`, `prompts/`, `tasks/`, `tags/`) | no | n/a |
| Per-file `deny_unknown_fields` | **yes** | yes | yes |
| Blast radius of one bad file | one image | every image | one job type |
| N−1 dispatcher behavior | ignores an unknown directory | ignores an unknown file | **hard parse error** |

**C is rejected on the last row and it is not close.** `JobType::image` is
`Option<String>` (`crates/types/src/job_type.rs`). Turning it into a
string-or-map union means an N−1 dispatcher hits a *type* error, not an unknown
field — and §14.2's tolerance rule covers unknown top-level *fields*, not
mistyped known ones. A single job type would refuse to load, which for
`_defaults.yaml`-style shared config means the project's whole job-type set goes
down. B is fine and A is better only because it matches four existing precedents
and confines a parse failure to one image.

**`build_args` are static and may never come from a job input.** This is not a
new rule; it is the existing one. `crates/types/src/job_type.rs` states it in the
`inputs` doc comment: *"nothing here can select an image, an evaluator, a secret
or a `run:` string"* (#311 Decision 1). An input-parameterised image would make
config resolution depend on a job's inputs, which a tier-1 property test already
forbids (#308's gap 10 records the same collision from the placement side).

**No `platform:`/architecture field.** `worker-refresh.sh` builds natively on
each node and spec §3.1 calls that out ("native arch preserved"). Under O2 the
image is built where it runs, so an architecture declaration would be either
redundant or a lie. It becomes necessary the day distribution becomes pull —
another reason the switch is a real decision, not a detail.

## 2. Referencing a project image, and the namespace question

**A job type references it with a scheme on the existing `image:` field:**

```yaml
# .chug/jobs/flutter-ci.yaml
image: project/ci
min_dispatcher: <the epoch this lands at>   # required — see §3
```

### There are three `image:` fields, and the scheme is accepted at all of them

`crates/types/src/job_type.rs` has three `pub image: Option<String>` sites, not
one: `JobType::image` (the top-level, line 40), `WrapUpSpec::image` (line 201,
*"Image for the `run` container; falls back to the job's top-level image (like
an evaluator, §1.1)"*) and `Evaluator::image` (line 388, *"command/agent:
optional, falls back to top-level image"*). The fallback is real code in four
places — `eval_image` (`crates/dispatcher/src/exec.rs`) and
`crates/dispatcher/src/eval.rs` for the two live paths, mirrored in
`crates/dispatcher/src/launch_queue.rs` for the requeued ones (one evaluator
arm, one `TaskPhase::WrapUp` arm) — each of them the same
`…image.clone().or_else(|| job_type.image.clone())`.

**All three accept `project/<name>`, and the `validate` rule scans all three.**
The alternative — restrict the scheme to the top level and let evaluators reach
it only by the fallback — was weighed and rejected: `.chug/jobs/_defaults.yaml`
shows an evaluator carrying its own `image: chuggernaut/agent-rust:prod` rather
than inheriting, and #308 category B is *command evaluators* (`rust-ci`,
`flutter-ci`, `terraform-validate`) whose whole point is that different
evaluators want different toolchains. A rule that only scanned the top level
would let an author write `eval[].image: project/flutter-ci` with no
`min_dispatcher`, pass field-rule validation, and hand an N−1 dispatcher a
literal `project/flutter-ci` to `create_container` — the exact failure #322 §3
rejects the scheme for, reintroduced through the back door.

**The fallback is nonetheless the shape most projects want.** A job type with
top-level `image: project/ci` and evaluators that declare no image of their own
gets the project image in work, every evaluator and `wrap_up.run` from one
declaration and one `min_dispatcher`. Beacon's CI-as-evaluators port is
probably exactly that: one `project/ci` image per repo, several command
evaluators inheriting it.

**A project can never name a docker tag.** The tag the daemon builds and the
dispatcher launches is **derived**, never supplied:

```text
chug-proj/{owner}/{project}/{name}:{sha}
```

`{sha}` is the resolved commit of the declared `ref`, and the same value is
stamped as an image label — the `chug.git.sha` pattern `worker-refresh.sh`
already uses and already *verifies before the retag-swap*. Deriving the tag makes
the collision question answer itself:

- A project **cannot** shadow `chuggernaut/agent-rust:prod` or any other platform
  image, because it never writes a tag string.
- A project **cannot** reach another project's images, because the derived prefix
  contains its own slug and the dispatcher — not the project — supplies that slug
  to the daemon.
- The two namespaces are **syntactically disjoint** at both ends: `project/…` in
  config (a scheme the parser recognises), `chug-proj/…` on the node (a prefix no
  platform image uses).

An unresolvable `project/<name>` — no such file in `.chug/images/` — is a <!-- intent -->
**config error at load**, in the same class as any other malformed job type, not
a launch-time surprise.

### Why the scheme is safe here when [#322](./322-macos-native-runtime.md) rejected one

Design #322 §3 weighs exactly this shape as its option C ("overload `image` with a
sentinel/scheme") and rejects it, with a correct argument: an N−1 dispatcher
parses the config fine, then passes the sentinel to `create_container`, and every
launch fails on a bogus image — burning `work_retries` per job instead of parking
once. Fact 4 confirms the mechanism.

That objection is fatal to a scheme **alone**. It is answered by a scheme
**plus a mandatory `min_dispatcher` field rule**, which is precisely the
mechanism `inputs:` already uses. Spec §14.2:

> Some schema features require that declaration rather than leaving it to the
> author: a non-empty `inputs:` (§1.1) is a field rule error unless
> `min_dispatcher` is at least the epoch inputs landed in. The rule exists
> because `min_dispatcher` is the one field an N−1 dispatcher **does** parse.

So: `JobType::validate` gains a rule — *any of `image`, `wrap_up.image` or
`eval[].image` beginning `project/` requires `min_dispatcher >=
PROJECT_IMAGE_SCHEMA_EPOCH`* — reported as an ordinary
`FieldRuleError::Required`, mirroring `validate_inputs`'s
`min_dispatcher.unwrap_or(0) < INPUTS_SCHEMA_EPOCH` check
(`crates/types/src/job_type.rs`). One rule over three fields, not three rules:
the scan is a helper over the three `Option<String>`s and the error names which
field carried the scheme, the way `wrap_up.image`'s existing `Required` error
already does. An N−1 dispatcher
reads `min_dispatcher`, finds it ahead of its own epoch, and **parks the job
pre-Work (Stalled, one park, §14.2)** with a diagnostic naming the file and the
needed version. `.chug/tasks/ci.sh`'s config-skew gate fails the config's own CI
before it can merge against an older deployed dispatcher (§14.3).

The honest residue: an author who writes `image: project/ci` and *omits*
`min_dispatcher` gets a field-rule error, not a working job — which is the
intended outcome and is one more thing to get right in the validator than a
separate `image_ref:` field would be. A separate field was weighed and rejected
because it makes `image:` optional-but-required-unless, and an N−1 dispatcher
would then fail the `Required { field: "image" }` rule and refuse the whole file
rather than parking the job.

## 3. The epoch, and the sequencing rule

**Yes, this costs a `CONFIG_SCHEMA_EPOCH` bump.** The `validate` rule above is
only meaningful if there is an epoch to declare, and a project-image reference an
N−1 dispatcher silently ran as a literal docker tag is exactly the failure mode
which #322 §3 rejects.

**Do not write a number in this document, and do not write one in #322 or #313
either.** Three independent tracks want "the next epoch":

| Track | Field | Where it says so |
| --- | --- | --- |
| [#322](./322-macos-native-runtime.md) N2 | `runtime: { mode, env }` | states `2 → 3` literally |
| [#313](./313-workload-identity-image-builds.md) S3 | `workload_identities:` on `work`/`eval[]`/`wrap_up` | says "the new epoch", and correctly refuses to number it |
| this doc | `image: project/<name>` | — |

Design #313 already states the rule and this document adopts it verbatim rather than
inventing a second one: *"whichever of the three lands last re-derives it, and
§14.3's gate reads the deployed epoch live, so a stale declaration fails CI
rather than shipping. If two of them ship in one deploy generation, one bump
covers both."* Extended with the two clauses that make it mechanical:

1. **The epoch is a counter, not a reservation.** No design owns a number. The
   first change to **merge** takes `CONFIG_SCHEMA_EPOCH + 1`; every other track
   rebases onto whatever it finds at merge time. #322's literal `2 → 3` must be
   read as "the next epoch", not as a claim on `3`.
2. **Every feature freezes its own constant at the value it shipped at.**
   `INPUTS_SCHEMA_EPOCH = 2` is the precedent, and `crates/types/src/version.rs`
   explains why: *"it is frozen at the epoch the feature shipped, so a later bump
   for an unrelated feature does not retroactively raise what an existing
   `inputs:` config has to declare."* So `PROJECT_IMAGE_SCHEMA_EPOCH` is its own
   constant, and the `feature_epochs_are_understood_by_this_binary` test in
   `version.rs` gets one more row. A collision is then a merge-order problem
   only — never a semantics problem, and never a reason for two tracks to
   coordinate a deploy.

That is the answer to the escalation-storm concern: the 2026-07-22 incident was a
config field a running dispatcher's strict parser rejected with no declared
epoch at all (`version.rs`'s module header records it). Three tracks each
declaring a frozen constant and rebasing `min_dispatcher` at merge cannot
reproduce it, because the failure mode requires an *undeclared* skew.

## 4. The build mechanism

**A new `build_image` op on `req.worker.{node}.>`**, beside `refresh`,
`refresh_cancel` and the rest of `crates/worker/src/daemon.rs`'s dispatch switch.

```rust
// crates/types/src/worker.rs — sketch
pub struct BuildImageRequest {
    /// `owner/project` — supplied by the DISPATCHER, never by the project.
    pub project: String,
    /// Image name (the `.chug/images/` file stem).
    pub name: String,
    /// Advertised ref to fetch (fact 7 — never a raw SHA).
    pub git_ref: String,
    /// The commit that ref must resolve to; a mismatch aborts the build.
    pub sha: String,
    pub dockerfile: String,
    pub context: String,
    pub build_args: BTreeMap<String, String>,
}
```

Four properties are contracts, not implementation detail:

- **The repo URL is derived node-side from `project`, never carried.** The node
  holds `WORKER_GIT_KEY` (`/data/keys/worker_git`) and
  `WORKER_REFRESH_GIT_URL` points it at one repo today; a project build composes
  the ssh front's host with the dispatcher-supplied slug. A project-supplied URL
  in this message would let a project's config aim the node's credential at any
  repo the ssh front serves. It is not in the message, and that is the reason.
- **The tag is derived node-side** from `(project, name, sha)`, per
  [§2](#2-referencing-a-project-image-and-the-namespace-question).
- **The op carries no secrets and no job credentials.** Spec §3.1 puts per-job
  credentials inline on the launch request; a build request is not a launch and
  gets none. A Dockerfile needing a private dependency is an open question, named
  in [§11](#11-what-this-document-does-not-decide).
- **`docker build` runs with fixed flags.** `DOCKER_BUILDKIT=1` and no
  `--insecure-entitlement`, so `security.insecure` and `network.host` stay off
  (#313 B-IV's reasoning, applied to a build the daemon runs itself rather than
  one a proxied container asks for).

**The build reuses `refresh`'s whole lifecycle, not a parallel one.** Concretely,
the same six mechanisms: validate-first before any docker mutation; the disk
pre-flight; a temp tag with a retag-swap only on complete success; the
`chug.git.sha` label verified *before* the swap; cleanup and prune on the failure
path as well as the success path; and phase markers on stdout that the daemon
reports through `ping`. The last one matters more than it looks — spec §3.1 is
explicit that a multi-minute build must never be a silent wait, and a project
image build is exactly as long and exactly as opaque.

**It is additive, so it does not bump `WORKER_RPC_VERSION`.** `version.rs`:
*"The daemon logs-and-fallbacks on an unknown op rather than crashing, so an
additive op does not bump this."* An N−1 daemon replies
`WorkerError::Other { message: "unknown op \"build_image\" on …" }`
(`crates/worker/src/daemon.rs`). The reconciler must read that reply as **"this
node cannot serve project images"** and stop placing project-image work there —
fail closed, exactly as #309 §4's `modes` default does, not "retry forever".

**Why a `build_image` op and not #313's B-IV proxy.** #313 recommends a narrowed
docker-API proxy bound into an allow-listed job type's containers, and explicitly
prefers it over B-III ("a build op on the worker daemon") because B-III needs *"a
second execution lifecycle inside the daemon (a build is not a task, so its
timeout, log tail, cancellation and crash recovery are all new code paths that
duplicate what the task machinery already does)"*.
That argument is right for #313's problem and wrong for this one, for one reason:
**the second lifecycle already exists.** `refresh` is it. Timeout, cancellation
(`refresh_cancel`, ticket #254), progress relay (`refresh_progress`, #253) and
durable outcome (`RefreshOutcome`, #187) are all written and operated. #313's
B-III cost is "build the refresh lifecycle again"; here the cost is "generalise
the refresh lifecycle over an image list", which is a much smaller thing.

The two also want different isolation. B-IV's proxy hands a *job container* the
ability to ask the daemon to build — because a **product** build is work a job
does. A task image must exist before any job of its type can run, so there is no
job to hand a socket to. The B-IV shape is not available here even if it were
preferable.

## 5. Reconciliation: what triggers a rebuild, and who owns it

**The dispatcher owns desired state; the node owns observed state; a reconciler
closes the gap.** This is the same shape as the fleet-capacity loop the platform
already runs (§3.1: nodes advertise, the dispatcher merges, placement reads the
merged view).

**Desired state** is computed from the project's default branch, which the
dispatcher already reads live for job types (spec §14's "config is read *live*
from the default branch"). For each `.chug/images/<name>.yaml`, desired is
`(project, name, resolved_sha)` where `resolved_sha` is the declared `ref`
resolved against the project's bare repo.

**Observed state** is what each node reports: the `(project, name, sha)` triples
it currently holds. This rides `NodeCapabilities` — see
[§6](#6-a-launch-that-lands-on-a-node-without-the-image).

### Where the reconciler runs and what wakes it

**It runs on the periodic scan tick, inside the single-writer loop.**
`crates/dispatcher/src/core.rs` defines `SCAN_INTERVAL` = 30 seconds and the
ticker sends `Msg::Scan` on it; `crates/dispatcher/src/scan.rs` handles it and
its module header already lists what it does beyond the §3.5 timeout scans —
*"also drives the launch-queue drain and config republish."* Image
reconciliation is one more of those, for three reasons:

- **It is the same class of work the scan already does.** The scan already
  reaches git per project inside the tick: the one-shot deadline scan calls
  `release::load_job_type(&self.repos, …)` for jobs it has no cached job type
  for. A ref resolve against a local bare repo is cheaper than that.
- **It preserves the single-writer property** (`CLAUDE.md`; docs/reference/style.md Tier 3).
  Desired-vs-observed lives in the actor, the `build_image` op is emitted from
  the actor, and no second writer of image state exists — which is the same
  reason `scan.rs`'s header says *"both scans run inside the single-writer loop
  like any other message."*
- **A level trigger is one code path; an edge trigger is four.** See below.

**Level-triggered, deliberately, over the edge-triggered alternative.** The
obvious edge trigger has a precedent in the tree: spec §13.3 says factory
definitions are read from the default branch HEAD and *"the dispatcher reloads
them on startup and after every squash-merge to the default branch."* Copying
that here would be wrong, and not only for linked projects:

- For a **self-hosted** project the config-carrying ref moves on squash-merge —
  one event, easy to hook.
- For a **linked-origin** project the config-carrying ref is the local
  `integration` branch, **not** origin main. Spec §5.3 is explicit — *"the local
  bare repo's `HEAD` symref points at a chuggernaut-owned `integration` branch"*,
  and "default branch" **is** integration. So the desired-state read above is
  well-defined for beacon's shape. But `integration` moves through *several*
  dispatcher paths: a squash-merge, `origin.sync`'s post-merge hard reset onto
  the new origin main, and `sync`'s no-open-release fast-forward that pulls
  external commits in. And `sync` is not itself periodic — §5.3 has it running
  on `req.origin.sync` and opportunistically from `origin.status`, i.e. when
  someone asks. Hooking each of those is four hooks today and a silent
  staleness bug the day a fifth path lands.

A tick that recomputes desired state from whatever the ref resolves to now is
correct under all of them and needs no maintenance when a new one appears.

**The steady-state cost is one ref resolve, not a tree read.** The tick resolves
each project's declared image refs against its bare repo and compares to the
last-seen SHA; only on a change does it read the `.chug/images/*.yaml` blobs and
recompute the desired set. So a fleet with nothing changing pays N ref resolves
per 30 seconds, and the expensive path runs exactly when something moved.

**The latency this buys, stated as a number.** Detection is bounded by one
`SCAN_INTERVAL` — **at most 30 seconds** from the config-carrying ref moving to
the `build_image` op being emitted — plus the build itself. That is the
operator requirement's acceptance test answered concretely: merge a Dockerfile
change, and within 30 seconds the fleet is building it, with no chuggernaut
deploy anywhere in the loop.

**The trigger is a change to the resolved SHA of a declared image's ref**, not
every commit. Two candidate triggers were weighed:

| | Rebuild on every default-branch commit | Rebuild when the declared ref's SHA changes |
| --- | --- | --- |
| Correct | yes | yes |
| Cost | a build per merge; unusable | a build per image-ref move |
| Detection | none needed | label compare against `chug.git.sha` |

The second is chosen and the SHA label makes it a **string comparison, not a
heuristic** — which is the same reason `worker-refresh.sh` stamps and verifies
the label rather than trusting an exit code. It over-builds when the ref moves
for an unrelated reason (a docs commit rebuilds the image); pinning `ref:` to a
dedicated tag or branch is the project's lever if it cares, and that is why `ref`
is declarable rather than hardcoded to the default branch.

**The ref constraint of fact 7 is not just tolerable, it is the right policy.**
A task image built from an in-flight job branch would let any job change the
environment its own evaluators run in — the environment that is supposed to judge
it. Building only from an advertised ref (default: the default branch) means a
task image changes only through the project's ordinary merge gate. It also means
**a project cannot test an image change in the job that makes it**, which is a
real cost: the sequence is merge the Dockerfile, let the reconciler rebuild, then
run the job that needs it. Naming a second image (`ci-next`) and a second job type
is the available workaround; a nicer one is an open question.

**This is what satisfies the operator requirement.** The loop contains: a project
repo commit, the next scan tick's ref resolve against the project's default
branch, a `build_image` op, and a node build. It contains no chuggernaut deploy,
no `worker-refresh.sh` run, and no platform SHA. The two clocks are separated
because the *trigger* is the project's ref, not the platform's deployed SHA.

**Bounds** (docs/reference/style.md Tier 2 #3 — everything is bounded):

- **One project build at a time per node.** BuildKit does not honour per-build
  CPU/memory limits, so concurrency-of-one plus a timeout is the only honest
  bound available. A build also does **not** occupy a launch slot, so this bound
  is the only thing standing between a project Dockerfile and the node's whole
  CPU.
- **A build timeout**, after which the build is signalled the way
  `refresh_cancel` signals: the process **group**, SIGTERM then SIGKILL after a
  grace window, so the cleanup trap runs and no partial generation is stranded.
- **A bounded retry with backoff per (node, image, sha)**, and a failure recorded
  durably the way `RefreshOutcome` is — a node that cannot build an image must be
  a queryable fact, not a log line. A repeatedly-failing build must stop
  retrying and surface, not loop.

## 6. A launch that lands on a node without the image

Three options, and the answer is different for the interim and the durable case.

| | Hard fail | Build on demand | Filter at placement |
| --- | --- | --- | --- |
| Behavior today | this is it (fact 3, 4) | — | — |
| Cost | burns `work_retries`; diagnosis only from the container log | blocks the launch path or eats `task_timeout` | needs advertised inventory |
| Verdict | unacceptable as the end state | rejected (see [O3](#o3--hybrid-build-once-on-a-builder-node-distribute-by-registry)) | **the durable answer** |

**Filter at placement, and this should ride [#309](./309-host-native-execution.md)
§4 rather than precede it.** #309 §4 defines `NodeCapabilities` carried on **both**
`PingOk` and `WorkerAnnounce` as an optional additive field, with per-field
absent-defaults chosen so an N−1 daemon reads correctly with no coordination.
Project-image inventory is one more field in that struct:

```rust
/// Project task images this node holds, as `{owner}/{project}/{name}:{sha}`.
/// Absent ⇒ `[]` — a node advertises no project images unless it says so.
/// Fails closed, exactly like `leases`.
pub images: Vec<String>,
```

Adding a *second*, independent capability field to both transports would be the
mistake #309 §4 opens by warning about — #293 is already editing both, and #308
correction 6 is quoted there as right that a capability field must land *after or
with* it. So the dependency is real and it is on #309 §4 landing, **for the
placement filter**. Two things soften it:

- **The interim answer ships today: `placement.node`** (spec §3.1,
  `JobType::placement`). Pin the project's job types to the node that builds
  their image. This is the same interim #313 B-IV names for its builder node, and
  it is honest about being a pin rather than a policy.
- **`choose_placement` needs no new shape.** `crates/container/src/lib.rs` already
  takes `&[PlacementCandidate]` and already skips ineligible candidates; an image
  predicate is another skip condition, not a new signature.

**The error class matters and must be chosen deliberately.**

- *No node holds the image yet, but it is declared* → **`NoCapacity`**. Fact 4:
  `NoCapacity` defers and the launch queue retries as slots free — and "a node is
  building this image" is exactly as transient as "a node is busy". The message
  should say so verbatim, e.g. `no node holds image {owner}/{project}/{name}:{sha}`.
- *The image is not declared at all* → **hard `Launch`**. It can never clear
  without a config change, so queueing it would be a 30-minute silence with a
  known answer — the same reasoning #309 §10 uses for an out-of-tenancy host
  launch.

**The honest cost of the `NoCapacity` choice, in the two numbers that decide
it.** `MAX_QUEUE_WAIT` is 30 minutes. Reconciler detection is bounded by
`SCAN_INTERVAL` = 30 seconds ([§5](#5-reconciliation-what-triggers-a-rebuild-and-who-owns-it)),
so the build starts ~30s after the declaration lands, and a job created *at the
same time* gets ~29½ of the 30 minutes as usable build budget. That is enough
for most images and **not** enough for a first cold build of a large toolchain:
the platform's own `agent-rust` leg ran 673s on air *before* #352, and a
flutter/android image is larger — a 40-minute cold build queues, waits 30
minutes, and escalates with `no_free_slots_timeout` even though nothing is
wrong.

So the 30s tick makes the window *good*, not *sufficient*. The residual rough
edge is narrow and precisely bounded: a job created within one build-duration of
its image's first declaration, where that build exceeds 30 minutes. It is
retryable, the second attempt finds the image built, and raising
`MAX_QUEUE_WAIT` to cover it would degrade the wedged-fleet diagnostic the bound
was sized for. The right fix if it bites in practice is a distinct queue reason
for "a node is building this image" with its own longer bound, which is a
one-variant change and is deliberately not proposed until a real build is
measured.

## 7. Disk accounting and GC

Fact 8 is the whole problem: the platform's own three images already make a 30GB
floor a gate that refuses builds, re-derived four times across real incidents.

**Three rules, in priority order.**

1. **The platform refresh must win.** A project build checks a **higher** free-disk
   floor than `worker-refresh.sh` does. Under pressure the node then sheds
   project-image builds first and a deploy still lands. Inverting this — letting a
   project image build a node out of headroom for its own refresh — reproduces the
   #248 failure loop (a failed refresh strands a partial generation, making the
   next attempt more likely to fail) with a new cause the operator cannot fix by
   deploying.
2. **A per-node byte budget for project images**, checked before a build and
   reported in the node's advertised state. Refuse and say so loudly with the
   numbers, exactly as `refresh_disk_preflight` does — *"in SECONDS and with the
   numbers that explain it, instead of burning ten minutes of cargo into a doomed
   build."*
3. **GC is a desired-state diff, and it needs `docker rmi`, not `prune`.** This is
   the gotcha worth stating explicitly: `worker-refresh.sh`'s cleanup is `docker
   image prune -f`, which reclaims **dangling** images only — and it is correct
   there, because a retag-swap onto a fixed tag (`:prod`) is what makes the
   previous generation dangling. Project images are tagged **per SHA**, so a
   superseded generation keeps its tag and is *never* dangling. It is never
   reclaimed by any prune the node runs today. So GC must explicitly `docker rmi`
   the tags the desired-state diff says are unwanted.

**What "unwanted" means**, and the one deliberate exception: keep the current SHA
**and the immediately previous one** — a rollback handle, the same discipline
that #313 B4 insists on for product images — and remove everything older, plus every
image of an undeclared name, plus every image of a project the node no longer
serves. A never-GC'd generation is how a node fills up silently; a
zero-generation-retained policy is how a bad merge takes a project's jobs down
with no way back.

**Cross-project BuildKit cache poisoning is real and is not solved here.**
Design #313 B3 names it and its mitigation — per-project cache `id`s in project
Dockerfiles, as `deploy/prod/Dockerfile.worker` and
`deploy/prod/Dockerfile.agent-rust` already demonstrate — and is honest that
*"Do not pretend the shared cache is a boundary."* That remains true here. It is a documented
property of a shared builder, not something a task-image design can fix.

## 8. Trust: what a project Dockerfile actually reaches

**The starting point is #313 correction 5**, which is the sharpest statement in
the sibling docs: a raw docker socket on a chuggernaut node yields
`docker inspect chug-worker`, the host path of its `:ro` key mount, and from
there `worker.creds` (which subscribes `req.worker.{node}.>`) and `worker_git`.
Since §3.1 puts per-job credentials **inline** on those subjects, the socket
grants the ability to receive other jobs' minted credentials. That is the bar
anything on a node must clear.

**Does a project build widen it? Not by that mechanism.** The daemon holds the
socket and runs `docker build`; a `RUN` step gets a build container, not the
socket. With `security.insecure` and `network.host` entitlements off, a `RUN`
step is arbitrary project code with network access and no host access — which is
**the same class of thing every job container already is**. A project already
runs arbitrary code on these nodes; that is what a job is.

**What it does widen, stated plainly — four things, each with its bound.**

1. **Resource consumption outside slot accounting.** A job container carries
   `resources.cpu`/`memory` enforced through `nano_cpus`/`memory` on the Docker
   `HostConfig`. A build carries nothing, and BuildKit does not honour per-build
   limits. *Bound:* concurrency-of-one per node, a build timeout, the disk floor
   of [§7](#7-disk-accounting-and-gc). This is the honest weak point of the whole
   design — a hostile Dockerfile can degrade a node inside those bounds.
2. **Persistence.** A job container's overlay is removed
   (`ContainerBackend::remove`); a built image stays and is launched again later.
   *Bound:* the derived tag namespace of [§2](#2-referencing-a-project-image-and-the-namespace-question).
   A poisoned image can affect later jobs **of the same project only**, which is
   a boundary the platform already draws everywhere else.
3. **Build-time network egress with no job context.** A `RUN curl … | sh` is
   ordinary Dockerfile practice and is also an exfiltration path — but with
   nothing to exfiltrate, since the build request carries no secrets and no
   credentials ([§4](#4-the-build-mechanism)). *Bound:* keep it that way. The
   moment a build needs a private-registry or private-dependency credential, this
   property is gone and the decision must be retaken, which is why
   [§11](#11-what-this-document-does-not-decide) leaves it open rather than
   quietly allowing it.
4. **The node's git credential.** `worker_git` can fetch any repo the ssh front
   serves. *Bound:* the build request carries a project **slug** supplied by the
   dispatcher, and the daemon composes the URL. No project-controlled URL, ever.

**Not in scope, and worth saying:** a hostile project. #309 §10 is right that
nothing short of a VM per task bounds a genuinely hostile tenant, and this design
does not claim otherwise. It claims that a project build is the same trust class
as a project job, and enumerates the four places that equivalence needed work to
be true.

## 9. Interaction with host-native mode

[#309](./309-host-native-execution.md) §3 and [#322](./322-macos-native-runtime.md)
§3 replace `image:` with `runtime: { mode: host, env: … }` — a nix flake ref or
`xcode:<version>` — resolved by the node. #322 §3 records that `image` is
required for `agent`/`command` work and that a host runtime has no image.

**A host node does not participate in project task images, and that is
consistent rather than a gap.** The two are the *same slot in two modes*: the
project-declared, repo-versioned, node-resolved reference to a pinned
environment. `image: project/ci` + `.chug/images/` is to container mode what <!-- intent -->
`runtime.env: nix:…#ci` is to host mode. Both are declared under `.chug/`, both
are resolved by the node, both are opaque to the dispatcher, and both are
mutually exclusive with the other by the field rules #322 §3 sketches (`env`
required when `mode: host`, disallowed otherwise).

Two consequences:

- **A job type is container-mode or host-mode, never both**, so "does a host node
  have the project image" is never asked. `NodeCapabilities.modes` (#309 §4)
  already filters host work off container nodes and vice versa; the `images`
  predicate of [§6](#6-a-launch-that-lands-on-a-node-without-the-image) applies
  only to container-mode placement.
- **The epoch bumps could genuinely be one bump.** #322 N2's `runtime:` and this
  document's `project/` scheme are both "the project declares its environment"
  and both hang off `min_dispatcher`. If they ship in one deploy generation one
  bump covers both, exactly as [§3](#3-the-epoch-and-the-sequencing-rule)'s rule
  (inherited from #313) already says.

## 10. Version skew: three distinct kinds, only one of which is §14

| Skew | Mechanism | Costs an epoch? |
| --- | --- | --- |
| Config declares `project/…`, dispatcher is N−1 | `min_dispatcher` field rule → §14.2 park pre-Work; §14.3 CI gate at merge | **yes** — `CONFIG_SCHEMA_EPOCH`, per [§3](#3-the-epoch-and-the-sequencing-rule) |
| Dispatcher sends `build_image`, daemon is N−1 | additive op; N−1 replies `unknown op`; reconciler reads it as "cannot serve project images" and fails closed | **no** — `version.rs`'s additive-op rule |
| Job type names an image no node has built yet | `NoCapacity` + launch queue + reconciler; a platform event so it is visible | **no** — see below |

**The third row is the one to get right, because it looks like §14 and is not.**
Both sides understand the schema perfectly; the disagreement is about *inventory*,
which is fleet state, not a wire surface. Modelling it as an epoch would be a
category error with a concrete cost: an epoch parks a job until a **deploy**, and
inventory converges without one — which is the entire point of the operator
requirement. It belongs in the same class as "no free slots": a transient,
self-healing placement condition with a bound and a loud timeout.

**One skew the sibling docs' framing does not cover.** A node that swaps onto a
new platform generation via `refresh` keeps its project images — they are
separate tags, and the retag-swap touches only `chuggernaut/*`. That is correct
and is worth asserting in a test, because the alternative (a refresh that
implicitly invalidates project images) would re-weld the two clocks the whole
design exists to separate.

## 11. What this document does not decide

- **Private build dependencies.** A Dockerfile needing a credential (a private
  npm registry, a licensed SDK) breaks the "the build carries no secrets"
  property of [§4](#4-the-build-mechanism) and [§8](#8-trust-what-a-project-dockerfile-actually-reaches).
  It is a real requirement for real toolchains and it wants its own decision,
  most plausibly against #313 half A. Not allowed by default here.
- **Age-based rebuild.** `FROM debian:bookworm` + `apt-get install` is not
  reproducible: the same SHA yields different bytes months apart. A declared
  max-staleness (`refresh: 30d`) is the obvious answer and is deliberately not
  proposed — it adds schema for a problem no consumer has hit yet, and
  [§5](#5-reconciliation-what-triggers-a-rebuild-and-who-owns-it)'s SHA trigger
  is bounded without it.
- **Testing an image change in the job that makes it.** [§5](#5-reconciliation-what-triggers-a-rebuild-and-who-owns-it)
  names the two-image workaround and stops there.
- **The exact free-disk floor and byte budget** of [§7](#7-disk-accounting-and-gc).
  `worker-refresh.sh`'s own header is emphatic that these are derived from
  measurement and that lowering one on a projection is how deploy #347 got burned.
  Derive them from a real build, not from this document.
- **Whether to build project images eagerly on every node or on a declared
  subset.** Eager is assumed above because it keeps placement unconstrained; a
  per-project node subset (the analogue of #309 §10's `WORKER_HOST_PROJECTS`) is
  the obvious lever if disk becomes the binding constraint, and composes with the
  `images` predicate unchanged.
- **Amending [#308](./308-gha-port.md).** Correction 1 states the correction;
  applying it to #308's gap table, its phase-1 claim and its ordering table is a
  separate `docs` job.

## Contracts this changes

Per docs/reference/style.md's contract-first rule, the interfaces a `code` job implementing this
would change:

| Contract | Change |
| --- | --- |
| `JobType` (`crates/types/src/job_type.rs`) | all three image fields — `image`, `wrap_up.image`, `eval[].image` — accept a `project/<name>` scheme; new `validate` rule scanning all three and requiring `min_dispatcher >= PROJECT_IMAGE_SCHEMA_EPOCH` if any carries it |
| `types::version` | `CONFIG_SCHEMA_EPOCH` + 1 and a frozen `PROJECT_IMAGE_SCHEMA_EPOCH`, in the same commit; one more row in `feature_epochs_are_understood_by_this_binary` |
| New: `ImageSpec` in `types` | `.chug/images/<name>.yaml`, `deny_unknown_fields`, field rules; pure data, no I/O |
| `types::worker` | new `BuildImageRequest`/`BuildImageOk`; **additive**, no `WORKER_RPC_VERSION` bump |
| `crates/dispatcher/src/scan.rs` | the reconciler runs on the existing scan tick, beside the launch-queue drain and config republish |
| `crates/worker/src/daemon.rs` | `build_image` arm in the dispatch switch; the `refresh` lifecycle generalised over an image list |
| `NodeCapabilities` (#309 §4) | one more field, `images: Vec<String>`, absent ⇒ `[]` |
| `container::choose_placement` | one more skip predicate; `NoCapacity("no node holds image …")` |
| `deploy/prod/worker-refresh.sh` | the three hardcoded image names become the platform entry of a list |
| `docs/spec.md` | §1.1 gains `.chug/images/` and the `project/` scheme; §3.1 gains the `build_image` op and the reconciler; the Appendix's "Image registry" row gains a note that task images do not need one under this design  <!-- intent --> |

## Related

- [`docs/spec.md`](../spec.md) §1.1 (`.chug/` config root, `image` field rules),
  §3.1 (worker self-refresh, placement, node-local cache), §14 (config/version
  skew), Appendix: Infrastructure Summary.
- [`CLAUDE.md`](../../CLAUDE.md) — the per-consumer forge constraint this design
  exists to satisfy.
- [#308](./308-gha-port.md) — gap 11 (registry), phase 1 (corrected above), §H.2
  (the three clocks).
- [#313](./313-workload-identity-image-builds.md) — corrections 2/3/5, B1's
  four-option build survey, B2 (registry auth), B3 (build cache), B4 (tagging).
- [#309](./309-host-native-execution.md) — §4 `NodeCapabilities`, §10 trust and
  tenancy.
- [#322](./322-macos-native-runtime.md) — §3 the `runtime:` selector and the
  scheme-in-`image` rejection this document answers.
- [`docs/design-docs.md`](../design-docs.md) — the header contract above.
