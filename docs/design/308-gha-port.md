# Design #308 — Porting beacon's GitHub Actions onto Chuggernaut

Status: PROPOSED, amended 2026-07-30 (job #320), corrected 2026-08-09 (job #520).

A survey whose job was to spawn children; all four were written and **all four**
have shipped code. The port itself has not begun. See
[Current state](#current-state). The head was rewritten on 2026-08-09 by job #513,
and again by job #520 for the operator decision that beacon is imported as a
**platform-owned** project — which inverts phase 0b's role and is recorded as
[A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts).

The original was written against the tree at `0346a80`; the amendment against
`00dd0dc`. **Four corrections to this doc's own claims** — one retraction, one
overstatement, one finding it was missing entirely, one made stale by shipped
code — **plus one added phase**: see
[What #308 got wrong](#what-308-got-wrong), which is where a downstream reader
should look first. A **fifth** correction was appended on 2026-08-09 and is the
one that moves the ordering:
[A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts).

Every claim about *Chuggernaut's* current behavior, at all three
dates, was read out of `docs/spec.md` and the source in this repo, not inferred from
the docs; the corrections in
[What the brief got wrong](#what-the-brief-got-wrong) are the ones the original
survey found.

Children (the implementable specs extracted from this survey):
[#309 host-native execution](309-host-native-execution.md),
[#310 scheduled jobs](310-scheduled-jobs.md),
[#311 job inputs](311-job-inputs.md),
[#313 workload identity and image builds](313-workload-identity-image-builds.md).
Where a claim of this doc now lives in a child, the child is cited rather than
restated.

Related: [docs/spec.md](../spec.md) §1.1 (job-type config), §3.1 (dispatcher
backends, fleet, node-local build cache), §3.3 (staged evaluation, merge gate),
§13 (factories and ingest), §14 (config/version skew), Appendix: Deferred;
[docs/reference/design-lifecycle.md](../reference/design-lifecycle.md) (rework, abort);
[CLAUDE.md](../../CLAUDE.md) ("the evaluation gates ARE the CI");
[deploy/prod/README.md](../../deploy/prod/README.md) §3 (GitHub Actions CD is
already gone for one workflow), §5a/§5b (tailnet vs public exposure);
[docs/design/293-worker-capacity.md](293-worker-capacity.md) (in-flight changes
to the same announce payload this doc wants to extend).

## Current state

*The **mutable head** ([#415](415-knowledge-architecture.md) [D2](415-knowledge-architecture.md#d2-every-design-doc-opens-with-a-mutable-current-state-head)):
rewritten to current truth whenever anything below it changes. Everything after
this section is append-only — the original survey, its 2026-07-30 amendment and
its corrections, never edited into the prose above them.*

**This is a survey, and its job was to spawn children.** All four children were
written and all four have shipped code — [#309](309-host-native-execution.md) P0
(job #434), [#310](310-scheduled-jobs.md) (jobs #359/#360),
[#311](311-job-inputs.md) slice A (job #314) and
[#313](313-workload-identity-image-builds.md) half A (job #413). Shipped is not
the same as in use, and the two halves of that sentence have moved apart: #309
P0 is now **on for one node** — `gumbo-air-0` advertises `host` and has run
agent host tasks ([#490](490-agent-work-on-a-mac.md) slice 6) — while #313 half
A's deploy and provider registration are still open, both as the rows below
say. The port itself — beacon's workflows actually running here — has not
begun, and **cannot be judged from this tree**: `~/beacon` is not checked out,
so every phase whose work lives in that repo is reported below as unknown
rather than guessed.

The rows below are the states of [Ordering](#ordering)'s table, which is a
dependency reading rather than a commitment, and which keeps each phase's
argument. **Phase numbers are never reassigned** — the children cite them.

| Phase | Work | State |
| --- | --- | --- |
| **0** | Land this doc | **Landed** (job #308), amended by job #320 |
| **0b** | **Import beacon as a platform-owned project** — a cutover, not a prerequisite ([A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts)) | Not started. The kind is decided (operator, 2026-08-09): platform-owned, with beacon's `.github/workflows/` disabled at import |
| **1** | CI as evaluators (category B) | Unknown here — same reason |
| **2** | Host-exec backend prototype on one node | **Landed** (job #434) as [#309](309-host-native-execution.md) P0, and now **on for one node**: `gumbo-air-0` advertises `host` and host tasks have run on it |
| **3** | Cron (category E) | **Landed** (job #359), with (job #360) for the dispatcher half — [#310](310-scheduled-jobs.md)'s minimum useful version |
| **4** | OIDC issuer + JWKS + WIF provider | **Landed** (job #413) in part — [#313](313-workload-identity-image-builds.md) half A's code; the deploy and the provider registration are open |
| **5** | Deploys, forward-only (category C) | Not started |
| **6** | Image build and push (category D) | Not started — #313 half B is still a design |
| **7** | Node-level exclusive resources | Not started — #309 P4 |
| **8** | Mobile and simulator jobs on host nodes (category F) | Started on both legs, and the macOS leg is **proven on the air by `mac-proof`** — `gumbo-air-0` is host-capable, `.chug/jobs/mac-proof.yaml` is the one job type declaring `mode: host`, and two of its runs (jobs #506 and #509) drove the node's Xcode and a booted iOS simulator ([#490](490-agent-work-on-a-mac.md), IMPLEMENTED). A proof merges nothing and no beacon workflow has run; see below. The Android leg is where [#367](367-android-emulator-execution.md) A1/A2 left it (jobs #374, #395), still pinned to the one node with `/dev/kvm` |
| **9** | Job inputs → unblocks rollback | **Landed** (job #314) — [#311](311-job-inputs.md) slice A, with jobs #315–#317 and #319 |
| **10** | Per-run placement | Answered, not scheduled — [#361](361-per-run-placement.md) found gap 10 needs no new field |

### Phase 0b, after the platform-owned decision

Row 0b said "Onboard beacon as a **linked-origin** project" until 2026-08-09, and
[A5](#a5-the-missing-phase-onboarding-beacon-as-a-project) argued that kind was a
requirement. The operator has decided otherwise: beacon is imported as a
**platform-owned** project — the kind this repo is, where the bare repo owns
`main` and GitHub is a force-pushed read-only mirror — with beacon's
`.github/workflows/` **disabled** at import. A5's analysis of the linked-origin
mode stays standing as the rejected option; what changed is which mode beacon
gets.

**The consequence is an inversion, and it is the part to read.** Linked-origin
would have been additive and reversible, so 0b came *first* and phases 1, 5, 6
and 8 hung off it. A platform-owned import is the **cutover**: after it the
platform owns `main` and there is no incremental-porting window on the far side,
so those phases are now work to be proven **before** 0b rather than work it
unblocks. That is not a deadlock — proof job types against in-repo fixtures
(`.chug/jobs/android-proof.yaml`, `.chug/jobs/mac-proof.yaml`,
`.chug/jobs/gcp-proof.yaml`) already prove capability with no beacon anywhere.
The full argument, the reason the workflows are disabled, and what it does to
[D2](#open-decisions) are in
[A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts).

### Phase 8's macOS leg, precisely

Row 8 said "**Linux-proven only**, with no macOS node in the fleet" until
2026-08-09. Both halves are now false, and the replacement is narrower than
"category F is done" — so what the two `mac-proof` runs on `gumbo-air-0` do and
do not license is worth stating in full. The record is
[#490](490-agent-work-on-a-mac.md)'s job #510 correction, and the machinery is
`.chug/jobs/mac-proof.yaml` (`runtime: {mode: host, env: "xcode:26.5"}`,
`placement: {node: air}`).

**What the runs demonstrated:**

- an **authenticated** agent CLI running as a native macOS process rather than
  in a container, with its `session.jsonl` harvested end to end at 462,085
  bytes (job #506);
- the Mach-O `chuggernaut-channel` the node installs carrying `update_status`
  and `submit_result` on the first call, on both runs;
- a real **iOS simulator** exercised against the runtime the node's
  `xcode:26.5` toolchain carries — `simctl boot`, then
  `launch com.apple.Preferences` inside the booted device;
- **host work and container CI in one job** (job #509), which is
  [#309](309-host-native-execution.md) §1's worked case running for the first
  time.

**What it does not license.** The claim is "proven on the air by `mac-proof`",
not "category F ports". It is one node, one job type, and a *proof*: `mac-proof`
declares `wrap_up: type: none`, so it merges nothing and gates nothing, and no
beacon workflow — fastlane, `flutter-integration-tests`, or any other — has run
here. The Android leg of this phase is untouched by that work and stands where
[#367](367-android-emulator-execution.md) A1/A2 left it.

**Two findings from the proof bear on the port — one open, one withdrawn:**

- **M7 has two samples and no verdict.** Simulator state one task leaves for
  the next made the second run *cheaper*, not disturbed; two observations of
  "did not disturb" are not "cannot disturb", so
  [#490](490-agent-work-on-a-mac.md) D4's one host task per node stays and
  [#322](322-macos-native-runtime.md) §5's per-task device set stays deferred.
- **`xcrun simctl spawn <udid>` is not broken, and the session was the wrong
  culprit.** The proof runs' `LaunchdSimError` 111 and `NSPOSIXErrorDomain` 2
  were recorded here as a property of the session a host task gets; both
  reproduce over an ordinary SSH session and separate by **argument**, so that
  attribution is withdrawn ([#490](490-agent-work-on-a-mac.md)'s job #527
  correction). What is left is the ordinary iOS constraint: `spawn` runs the
  named program inside the simulator's own filesystem, so a ported workflow
  shelling out to it hits nothing host-task-specific unless it names a binary
  the runtime does not carry.

## Provenance

Two halves, with different evidentiary weight, and the doc marks which is which:

- **The beacon half** — 41 workflows and 14 composite actions in `beacon`'s
  `.github/`, plus the quilbert/scrybert agent protocol — came from the survey
  in the job brief. That repo is still not checked out in this workspace, so
  nothing written here re-derives it. Where a decision hangs on a beacon detail,
  the doc says so, and that detail should be re-read before the corresponding
  phase starts.

  **It has now been verified once.** On **2026-07-30** the operator inspected
  `~/beacon/.github` directly and re-checked the claims this amendment turns on.
  The two findings recorded in [A2](#a2-the-keyed-caching-gap-was-overstated) and
  [A3](#a3-beacon-already-parameterizes-placement-per-run) — that exactly one
  workflow uses `actions/cache`, and that thirteen jobs select their runner from
  a dispatch input — are **verified fact with that provenance and date**, not
  survey inference, and are stated as such below. Beacon claims *not* re-read on
  that pass (the 41/14 counts, the quilbert/scrybert protocol, the emulator
  workarounds, the two `:latest`-only pushes) keep their original weight; and
  anything that changed in `.github/` after 2026-07-30 is unverified again.
- **The Chuggernaut half** is verified against this tree, at both dates. Paths
  and spec sections are citations, not illustrations.

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
daemon and a `/var/lib/github-runner/.buildx-cache-*` directory. Most of the hard
problems below (persistent build state, emulators, Xcode) are a restatement of
that one difference. Section [H](#h-host-native-workers) is the proposal that
addresses it directly; everything before H is what can be ported without it.

**Amended:** image builds are *not* a restatement of it. That was this doc's
[§D](#d-image-build-and-push-5-workflows--open) claim and it is retracted — see
[A1](#a1-image-builds-do-not-dissolve-into-host-mode). And "keyed caches" was
the wrong name for the cache problem: what gumbo has is implicit host state, not
a configured cache ([A2](#a2-the-keyed-caching-gap-was-overstated)).

## What is true today (verified)

| Capability | Where | State |
| --- | --- | --- |
| Staged evaluators, stage 0 gates stage 1 | `docs/spec.md` §3.3; `.chug/jobs/_defaults.yaml` | Shipped |
| Project-wide CI as an appended command evaluator | `.chug/jobs/_defaults.yaml` → `.chug/tasks/ci.sh` | Shipped |
| Diff-aware multi-leg CI script | `.chug/tasks/ci.sh` (549 lines) | Shipped |
| Per-container secret scoping | `docs/spec.md` §1.1/§8.2; `.chug/jobs/deploy.yaml` | Shipped |
| Command work + `wrap_up: none` for external effects | `docs/spec.md` §1.1; `.chug/jobs/deploy.yaml` | Shipped |
| Node pinning (`placement.node`) | `docs/spec.md` §3.1; `crates/types/src/job_type.rs` | Shipped |
| Mixed fleet (docker endpoints + NATS worker nodes) | `crates/worker/src/backend.rs` `NodeHandle` | Shipped |
| `ContainerBackend` implementations | `crates/container/src/docker.rs:378`, `crates/worker/src/backend.rs:539`, `crates/test-utils/src/lib.rs:343` | Three — one Docker, one NATS proxy, one fake; `crates/container/src/k8s.rs` is a stub |
| Node-local build cache (sccache) | `docs/spec.md` §3.1; `crates/worker/src/daemon.rs`, `WORKER_CACHE_DIR` | Shipped |
| Three review outcomes (pass / fail / fail+abort) | `docs/spec.md` §1.2 Abort verdict, §3.3 Reduce | Shipped |
| Version-skew machinery | `crates/types/src/version.rs`, `docs/spec.md` §14 | Shipped |
| Ingest → triage → jobs (GitHub origin) | `crates/dispatcher/src/forge_ingest/` | Shipped |
| Generic factories (`factories/{name}.yaml`) | `docs/spec.md` §13.3 | Specced, not in the tree |
| Runtime capacity control / richer announce | `docs/spec.md` §3.1; [design #293](293-worker-capacity.md) | Specced + designed, not in the tree |
| Outbound webhooks | `crates/webhooks/src/lib.rs` | Two-line `TODO` stub |
| Binary artifact store | `docs/spec.md` Appendix: Deferred | Deferred |
| Cron / schedules | — | Absent from the tree and from `docs/spec.md`; designed as [#310](./310-scheduled-jobs.md) |
| Job inputs (`CHUG_INPUT_*`) | `docs/spec.md` §1.1, §6.3, §14.2; `crates/types/src/inputs.rs`, `crates/domain/src/inputs.rs` | **Shipped and deployed** (amendment [A4](#a4-job-inputs-shipped-so-gap-1-is-retired)) — `CONFIG_SCHEMA_EPOCH` was 2 when A4 landed (`crates/types/src/version.rs` is the authority today); first consumer `.chug/jobs/rollback.yaml` |
| Matrix / fan-out over inputs | — | Absent **by decision** ([#311](./311-job-inputs.md) Decision 7), not by omission |
| Per-run placement (a runner chosen at launch) | — | Absent, and forbidden in the obvious shape — see [A3](#a3-beacon-already-parameterizes-placement-per-run) |
| Linked-origin projects (external host owns `main`) | `docs/spec.md` §5.3; `crates/dispatcher/src/handlers/origin.rs`, `crates/dispatcher/tests/origin.rs` | Shipped — ~~the mode beacon needs~~ **not** the mode beacon gets: the operator chose a platform-owned import ([A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts)) |
| OIDC issuance / JWKS endpoint | — | Absent (RS256 keypair exists, §12.1); designed as [#313](./313-workload-identity-image-builds.md) half A |

## What the brief got wrong

The brief is a snapshot analysis and seven of its claims do not survive contact
with the tree. They matter because three of them make the port *cheaper* than the
brief assumes and four make it *more expensive* or re-sequence it.

1. **"Chuggernaut evaluators are pass/fail, so scrybert's forced three-way
   verdict is lost."** They are not. `docs/spec.md` §1.2 defines the **abort
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
   image."** A node-local build cache already ships (`docs/spec.md` §3.1 "Node-local
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
   currently `{ node, slots, version }` while `docs/spec.md` §3.1 already describes
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

## What #308 got wrong

The same treatment, applied to this doc. **Five corrections and one addition** —
the fifth appended on 2026-08-09 and described at the end of this preamble. The
original four: one claim retracted outright ([A1](#a1-image-builds-do-not-dissolve-into-host-mode)), one
materially overstated ([A2](#a2-the-keyed-caching-gap-was-overstated)), one
significant finding missing entirely
([A3](#a3-beacon-already-parameterizes-placement-per-run)), and one made stale by
shipped code ([A4](#a4-job-inputs-shipped-so-gap-1-is-retired)). Plus **one
addition** ([A5](#a5-the-missing-phase-onboarding-beacon-as-a-project)): a phase
the ordering never had, which every category-B and category-C port assumes.

The fifth is
[A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts): beacon
is imported as a **platform-owned** project, which falsifies A5's central
requirement and re-derives the ordering table's `Depends on` column. It belongs
to this list, but it is written as a top-level section at the **end** of the doc
— the body is append-only, and a correction that arrived after the sections
below cannot be spliced in among them.

Corrections are recorded here rather than by rewriting the sections they touch,
so a reader who cited a section can see what moved; each affected section carries
a pointer into this list.

**Numbers are stable identifiers.** Sibling docs cite "Gap 1 of #308"
([#311](./311-job-inputs.md)) and "#308's ordering … phase 6"
([#313](./313-workload-identity-image-builds.md)), so gap and phase numbers are
never reassigned. Where a ranking changed, the row *order* changed and the number
did not; where something new was added it took the next free number, wherever it
sorts.

### A1. Image builds do not dissolve into host mode

§D closed with "**Both options dissolve if host-native nodes land** (section H):
a host node has a real docker daemon the way gumbo does, and the question stops
being interesting." **Retracted.**
[#313](./313-workload-identity-image-builds.md) Decision 0 retracts it on three
counts — the gumbo analogy does not transfer, "may I push" is an authentication
question host mode does not touch, and there is nothing to push to without a
registry. The reasoning lives there and is not restated here; §D now points at
it, and #313 B1 is the mechanism that replaced both of §D's options.

**Consequence for the [ordering](#ordering).** Phase 6 (image build and push)
read "depends on 2 or 4". It depends on **phase 4 plus an
operator-provisioned registry**, and **not on phase 2 at all** — #313's build
service runs on a pinned builder node in container mode, independent of
whether #309 ever lands. The registry is now [gap 11](#gaps-ranked).

### A2. The keyed-caching gap was overstated

Category B called the loss of `actions/cache` "the biggest container-path
regression in the whole port" and named gradle, pub, `node_modules` and buildx.
Gap 4 was ranked on that basis. Both were wrong about beacon.

**Verified by direct inspection of `~/beacon/.github` on 2026-07-30:** exactly
**one** of the 41 workflows uses `actions/cache` — `creator-tests.yml`, caching
one path (`creator/node_modules`), keyed on
`hashFiles('creator/package-lock.json')`. **Nothing caches gradle, pub or buildx
by key anywhere in the 41 workflows.**

So the sentence named three things beacon never configured. What makes those
builds fast on `gumbo` is not a cache *feature* at all: it is **implicit host
state on a persistent NixOS box** — a `~/.gradle`, a `~/.pub-cache`, a
`.buildx-cache-*` directory that is simply still there from last night, with no
key, no restore step and no declaration.

**This sharpens the case for [#309](./309-host-native-execution.md) rather than
weakening it.** The thing to build is **host execution with declared persistent
state** (#309 §9 / P5), not a cache-keying subsystem. Nobody is asking the
platform to compute a cache key; they are asking it to let a directory survive.
That is a different, smaller, and much better-specified piece of work.

Two consequences, both reflected in the tables below:

- **Gap 4 largely collapses into gap 3** (host-native execution), which is why
  gap 3 now heads the ranking and gap 4 sits at the bottom as a remnant.
- **The one real keyed cache is the small case.** A single `node_modules`
  directory is the node-local shape [correction 3](#what-the-brief-got-wrong)
  already identified — worker-daemon-local, no launch field, no wire field, no
  schema change. Read #309 §9 for the refinement that correction needs:
  `node_modules` (like `~/.gradle` and `~/.pub-cache`) is neither
  content-addressed nor free of job state, which are the two properties §3.1
  uses to justify `WORKER_CACHE_DIR` as the one permitted host bind. So it is a
  **second, namespaced mechanism** (#309's `WORKER_HOST_CACHE_ROOT`), not a
  widening of the sccache hole. Still worker-local; still not a platform change.

### A3. Beacon already parameterizes placement per run

Missing from #308 entirely.

**Verified by direct inspection of `~/beacon/.github` on 2026-07-30:** eleven
jobs choose their runner from a dispatch input —

```yaml
runs-on: ${{ inputs.runner == 'cloud' && 'ubuntu-latest' || fromJSON('["self-hosted", "linux", "x64", "gumbo"]') }}
```

— and two more do the same for macOS. **Thirteen jobs in total.** This is not
cosmetic: "run this one on the cloud runner today, gumbo is busy" is a per-run
operational lever, and the category-B and category-C ports inherit the habit.

It collides head-on with something the platform has since **shipped**, and the
collision is verifiable in this tree:

- `placement` is a field on the **job type**
  (`crates/types/src/job_type.rs`: `pub placement: Option<Placement>`, read at
  launch through `placement_node()`), and a job type is a repo-versioned file
  resolved at `base_ref`.
- [#311](./311-job-inputs.md) Decision 1 classifies `placement.node` as
  **never** selectable by an input: "placement is a fleet fact; an input naming a
  node lets a job creator pick which host runs project code."
- That is not merely a documented intention. It is an **enforced property**: the
  tier-1 test `resolved_job_type_is_equal_for_any_two_input_maps`
  (`crates/domain/src/release.rs`) asserts that for any job type, any job, and
  any two input maps, the `JobType` the release path resolves is *equal* — with
  cases deliberately containing the input names a substitution engine would
  notice. Threading `Job.inputs` into config resolution fails that test — but its
  reach stops at config resolution; see the correction directly below.

**Correction (2026-08-02, from [#361](361-per-run-placement.md)):** that last
sentence is true and is not the whole story. The test guards **job-type
resolution**, and the natural shape of an input-driven placement hack never goes
through it. `ContainerLaunchConfig.node` is composed at launch, at three sites
that each read `job_type.placement_node()` (`crates/dispatcher/src/exec.rs`,
`crates/dispatcher/src/eval.rs`, `crates/dispatcher/src/launch_queue.rs`), so a
change of the form

```rust
node: job.inputs.get("runner").cloned().or_else(|| job_type.placement_node().map(String::from)),
```

leaves the resolved `JobType` byte-identical and **passes** the property test. It
would arguably not even violate #311 Decision 1's literal wording: the job type
was resolved without reading inputs; the *launch config* was overridden
afterwards. **The invariant's text is broader than its enforcement**, and anyone
citing this test as the reason gap 10 is blocked is citing it one step too far.
Nothing the test *does* guard is diminished — no input reaches
`with_job_evaluators` or the merge below it, which is what keeps
[docs/reference/design-lifecycle.md](../reference/design-lifecycle.md)'s eval floor intact. The launch
path is covered instead by a stated, reviewer-enforced contract:
[docs/reference/contracts.md](../reference/contracts.md) §2 — "`ContainerLaunchConfig.node` is a pure
function of the resolved job type."

**So "make the runner an input" is not available under the current contract**,
and this amendment does not invent a way around it. The likely shape is that
per-run placement is resolved **at launch from a mechanism other than inputs**.
The nearest precedent in the tree is `Job.timeout` / `Job.model`
(`crates/types/src/job.rs`; §1.1) — per-job fields that override a type field,
scoped so that evaluators keep the type's resolution — which is the door #311
Decision 1 itself points at for its "never (by inputs)" rows.
[#309](./309-host-native-execution.md) §5a's capability-aware `choose_placement`
is a different thing again: placement by *requirement* ("this needs host mode"),
not placement by *choice* ("this run, on the cloud").

**Recorded as [gap 10](#gaps-ranked) and left OPEN.** Deciding it here would be
the per-job-override re-litigation the gap-1 discussion below warns against, and
it is a contract change (`Job` grows a field; §3.1's "the pin is the only
affinity control") that deserves its own doc.

That doc is [#361](361-per-run-placement.md), and it **closes gap 10 without any
`Job` change**: the per-run lever decomposes into a capability requirement
([#309](./309-host-native-execution.md) §5a, designed), load shedding and node
drain (both shipped), and a cost axis this fleet does not have. `placement.node`
stays the reviewed escape hatch, #311 Decision 1 stands unamended, and the only
work gap 10 generated is the contract recorded above.

### A4. Job inputs shipped, so gap 1 is retired

Job inputs are in the tree and deployed. `CONFIG_SCHEMA_EPOCH` was **2** when
this amendment landed (`crates/types/src/version.rs` is the authority today,
with `INPUTS_SCHEMA_EPOCH` as its own constant);
the declaration is a typed schema on the job type (`docs/spec.md` §1.1 `inputs:`); the
effective set lives on the job record and is immutable once `base_ref` is pinned
(§1.1 `Job.inputs`); delivery is one reserved env namespace, `CHUG_INPUT_*`
(§6.3); and `.chug/jobs/rollback.yaml` (`min_dispatcher: 2`) is the first
consumer, with `.chug/tasks/rollback.sh` reading `$CHUG_INPUT_SHA`. Jobs #314–#317
landed the platform half and #319 landed the UI (the create form and Draft editor
render declared inputs, so `rollback` no longer needs the API).

Job #317 already corrected this file: category C's "**Blocked:** rollback does
not port" became "was blocked, now unblocked", the gap-1 row and ordering row 9
were updated, and the gap-1 prose gained #311 Decision 1's tightening (nothing is
substituted anywhere — the value reaches the container as `$CHUG_INPUT_*`). That
work is not redone here.

What this amendment finishes: the **ranking**. Gap 1 is retired and its row moves
to the bottom of the table (keeping its number, per the note above), and the
forecast of twelve near-identical deploy job types is corrected in place — one
`deploy.sh` reading `$CHUG_INPUT_SERVICE` is the shape now. Anything left in this
doc that reads as "inputs are absent" is stale, not a live constraint.

### A5. The missing phase: onboarding beacon as a project

> **Superseded in part, 2026-08-09** by
> [A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts): the
> phase is real and stays, but beacon is imported **platform-owned**, not
> linked-origin. Everything below about the linked-origin mode is accurate and
> is left standing as the rejected option; what is false is that beacon "must be
> the other kind", and — per A6 — that 0b comes first.

No phase in the [ordering](#ordering) covers the prerequisite every category-B
and category-C port silently assumes: **beacon has to be a Chuggernaut project
first.** It is not one yet (per the operator, 2026-07-30 — the platform's project
list is not a fact this tree records), and it cannot be the same *kind* of
project this repo is.

`kasofsk/chuggernaut` is **platform-owned**: the bare repo on the Mini owns
`main` and GitHub is a read-only mirror, force-pushed every five minutes by a
launchd agent — which is why `deploy/prod/README.md` §3 records the linked-origin
flow as no longer applying to this project and warns that direct pushes to GitHub
`main` are overwritten.

beacon must be the other kind: a **linked-origin** project (`docs/spec.md` §5.3;
implemented in `crates/dispatcher/src/handlers/origin.rs`, tier-2 coverage in
`crates/dispatcher/tests/origin.rs`). GitHub keeps owning `main` and chuggernaut
never pushes it; the local bare repo's `HEAD` points at a chuggernaut-owned
`integration` branch, so the entire §3.2/§3.3 merge machinery — job branches,
merge queue, merge gate, SSH branch protection — operates unchanged with
`integration` as "the default branch"; and shipping is an explicit
`req.origin.release` that pushes `integration` to the origin as
`chug/release-{n}` and opens a PR into `main`, holding the merge queue until that
PR is merged or closed.

That is a real phase with real prerequisites, none of which are invention:

- The `CHUG_ORIGIN_DEPLOY_KEY` (write deploy key) and `CHUG_ORIGIN_PAT` (PR API)
  project secrets, set with `admin secret set` **before** linking; the `CHUG_`
  prefix is reserved, so neither can be declared in a job type or reach a
  container (§5.3).
- `chuggernaut admin project link --owner … --name … --origin-url …`
  (`crates/cli/src/admin.rs`), which fetches the origin, creates `integration` at
  origin main, installs the pre-receive hook, and seeds the `.chug/` config
  subset **skip-existing** — reaching GitHub through the first release PR.
- beacon's own `.chug/jobs/`, `.chug/prompts/` and `.chug/tasks/`, authored as
  project-owned config (CLAUDE.md: "factories and job-type config are
  project-owned and repo-versioned — a per-consumer forge"). Category B's CI
  script and category C's deploy types *are* this work; the phase is where it has
  somewhere to live.
- Fleet capacity for a second project on the existing worker nodes, which is the
  first time the fleet is genuinely multi-tenant — and, per
  [#309](./309-host-native-execution.md) §10, the reason host work is
  single-tenant by node policy.

It lands as **phase 0b** — before phase 1, and a precondition of phases 1, 5, 6
and 8. It is 0b rather than a renumbering because the existing numbers are cited
elsewhere.

**It also bears on [D2](#open-decisions)** (auto-merge vs human-merge), and the
bearing is substantial. For a linked-origin project the merge that reaches
GitHub's `main` **is a PR a human merges**, and `req.origin.release` is
explicit-trigger-only — so the release step is already an operator-controlled
checkpoint between a squash-merged job branch and the repo of record. What
auto-merge decides for beacon is therefore whether a human gates each job onto
`integration`, not whether a human gates code into GitHub.

That defuses D2; it does not close it, and this doc does not close it
unilaterally. The checkpoint is per *release*, not per *job*: a project that
wants each change reviewed before it joins `integration` still needs the `human`
evaluator at the highest stage (correction 2), and a release that batches twenty
squashes into one PR is a different review than twenty gates.

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

**Loss — restated by [A2](#a2-the-keyed-caching-gap-was-overstated); the
original claim here was overstated.** What this paragraph said was: "`actions/cache`
— gradle, pub, `node_modules`, buildx … the biggest container-path regression in
the whole port." Verified on 2026-07-30, beacon keys **one** cache
(`creator/node_modules` in `creator-tests.yml`) and keys nothing for gradle, pub
or buildx. The real loss is therefore not a cache feature but **implicit host
state on a persistent runner**: gradle/pub/buildx directories that survive
between runs because the box does.

That splits the loss in two, and both halves are smaller than one big one:

- **The persistent-state half** is [gap 3](#gaps-ranked) — host execution with
  *declared* persistent state ([#309](./309-host-native-execution.md) §9 / P5),
  not a cache-keying subsystem.
- **The one keyed cache** is the node-local shape of correction 3, i.e. a
  worker-daemon-local change with no schema or wire impact — though per #309 §9 a
  second, namespaced mechanism rather than a widening of `WORKER_CACHE_DIR`.

The spec's own mitigation — bake toolchains into the image (Appendix: Deferred) —
is proven for Rust (`deploy/prod/Dockerfile.agent-rust` bakes sccache, a
`nats-server`, and a warm dep graph) and remains **untested for
flutter/gradle/npm**, which is the part of this that is still genuinely unknown.

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

~~**Both options dissolve if host-native nodes land** (section H): a host node
has a real docker daemon the way gumbo does, and the question stops being
interesting.~~

**RETRACTED** — see [A1](#a1-image-builds-do-not-dissolve-into-host-mode) and
[#313](./313-workload-identity-image-builds.md) Decision 0, which refutes the
premise on three counts and supersedes both D1 and D2 above with a third shape:
a **node-provided build service** on a pinned builder node (#313 B1), reached
through a narrowed daemon API, available in container mode today and independent
of whether #309 lands. Read the options above as history, and #313 for the
decision. The retraction also re-sequences phase 6 in the
[ordering](#ordering): it depends on phase 4 plus a registry, not on phase 2.

**Amended 2026-08-09 (job #517): D1 is the adopted shape after all.** A host
task on `gumbo-air-0` was measured reaching a working docker daemon by file
ownership, and the operator has accepted docker access for jobs; #313 B-IV's
proxy is superseded and the real socket — allow-listed node-side, escalation
accepted — is what half B now builds. So the paragraph above is false of D1
where it says #313 supersedes "both D1 and D2 … with a third shape". The
struck-through sentence at the head of this note was right about the
**capability** and wrong about the consequence, and #313 Decision 0 was right
that the capability must be deliberate. D1's stated cost ("effectively root on
that node") is unchanged and is now accepted rather than avoided. See
[#517](./517-docker-access-for-jobs.md), which owns the decision.

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

**Landed** (job #375) for `rust-coverage`: `.chug/jobs/coverage.yaml` is that
ordinary job, released by hand and wired into no default. Its lcov file and HTML
tree became durable with [#362](362-binary-artifacts.md) S1 (job #381), which
named this job type as the consumer that triggers it: the run tars them to
`/workspace/chug-output.tar.gz` and the dispatcher harvests that into the task's
`output.tar.gz` artifact.

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
- **Answers the persistent-state gap**: gradle, pub, `node_modules`, sccache and
  buildx directories that survive between runs — which is exactly how gumbo works
  today, and per [A2](#a2-the-keyed-caching-gap-was-overstated) is *all* gumbo is
  doing (no keys, no restore steps). The state must be **declared** rather than
  merely ambient — #309 §9 — which is the difference between porting the property
  and inheriting the mess.
- ~~**Dissolves the image-build question** (category D)~~ — **retracted**, see
  [A1](#a1-image-builds-do-not-dissolve-into-host-mode). Host mode changes where
  a build may run, not who may push, and #309 §10 forbids the docker socket to
  host tasks precisely because the daemon it would reach is the node's container
  fleet's.
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
   docs/reference/style.md's "everything is bounded / fail fast and loud" rejects; prefer
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

Re-ranked in the 2026-07-30 amendment. **Row order is the ranking; the `#`
column is a stable identifier and is never reassigned** (siblings cite these
numbers — see [What #308 got wrong](#what-308-got-wrong)). Two rows are new (10,
11); one is retired (1) and one is mostly absorbed (4).

| # | Gap | Why it ranks here |
| --- | --- | --- |
| 3 | **Host-native execution** | Now first. It carries mobile (category F, which nothing else unblocks) *and* the persistent-build-state story gap 4 was standing in for ([A2](#a2-the-keyed-caching-gap-was-overstated)). New backend + daemon polymorphism + schema epoch bump; specced as [#309](./309-host-native-execution.md), P0 (prototype) first |
| 2 | **Cron** | One live workflow, but the trigger class the whole category-E port depends on; specced as [#310](./310-scheduled-jobs.md) |
| 10 | **Per-run placement** *(new, open)* | Thirteen beacon jobs pick their runner from a dispatch input, and the obvious port is forbidden: inputs may never influence config resolution, enforced by a tier-1 property test. No mechanism, no design — [A3](#a3-beacon-already-parameterizes-placement-per-run) |
| 11 | **No image registry** *(new)* | Surfaced by the [A1](#a1-image-builds-do-not-dissolve-into-host-mode) retraction: a build with nowhere to push produces an image that exists on one machine ([#313](./313-workload-identity-image-builds.md) correction 2). Operator infrastructure, not code — and phase 6's second dependency |
| 6 | **OIDC issuer prerequisite** | Needs a *publicly reachable* JWKS endpoint; infra, not just code. Specced as #313 half A, which also prices the fallback |
| 5 | **Artifacts** | `crates/store/src/artifacts.rs` holds transcripts, stdout and attachments; there is no inter-job binary handoff (Appendix: Deferred, "Binary artifact store") |
| 7 | **Outbound webhooks** | `crates/webhooks/src/lib.rs` is a stub; blocks `sentry-resolve` |
| 9 | **Node-level exclusive resources** | Only bites once host nodes run device-bound work (H.5); specced as #309 §5b / P4, where `placement.leases` is shown to force the epoch bump on its own |
| 8 | **Auto-merge vs human-merge default** | A policy choice, not a mechanism gap (correction 2) — ~~and narrower again for a linked-origin project, whose release PR is already a human checkpoint ([A5](#a5-the-missing-phase-onboarding-beacon-as-a-project))~~ **that narrowing lapsed**: beacon is imported platform-owned and has no release PR ([A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts)) |
| 4 | **Keyed caching** | **Mostly folded into gap 3.** Exactly one beacon workflow keys a cache (`creator/node_modules`); gradle/pub/buildx warmth is implicit host state, not a configured cache ([A2](#a2-the-keyed-caching-gap-was-overstated)). The remnant is one namespaced persistent directory in the worker — #309 §9, no platform change |
| 1 | **Job inputs / parameterization** | **Retired — landed and deployed** ([A4](#a4-job-inputs-shipped-so-gap-1-is-retired)): epoch 2, `.chug/jobs/rollback.yaml` is the first consumer, the UI renders declared inputs. Kept at its number because siblings cite "Gap 1 of #308"; matrix / fan-out stays excluded by decision (#311 Decision 7) |

**Gap 1 deserved its rank — and it is now closed, so this reads as history.**
Twelve deploy workflows differ by two strings, and before inputs every one of
them would have become a job-type file differing by two strings, because a job
type is a static file and a job carried no parameters. Per
[A4](#a4-job-inputs-shipped-so-gap-1-is-retired) that collapse is available
today: one `.chug/tasks/deploy.sh` reading `$CHUG_INPUT_SERVICE`, one job type.

The mechanism one would reach for first — a job-level override — is already
ruled out, and for a good reason:
[docs/reference/design-lifecycle.md](../reference/design-lifecycle.md) ("eval criteria are a floor,
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

**D1 — image builds: scoped socket vs rootless builder.** ~~The tiebreaker is
phase 2: if host-native nodes land, the question dissolves.~~ **CLOSED, and on
different grounds than this doc expected.** Per
[A1](#a1-image-builds-do-not-dissolve-into-host-mode) the tiebreaker was a
mistake — the question does not dissolve — and
[#313](./313-workload-identity-image-builds.md) B1 rejects *both* of D1's
options in favour of a node-provided build service. Nothing here is waiting on
the phase-2 prototype.

**D2 — auto-merge vs human-merge default.** Still open. Chuggernaut
auto-squash-merges on eval pass; beacon requires a human to merge. Per correction
2 this is a default, not a capability: a `human` evaluator at the highest stage
expresses beacon's policy today. The real question is which default a *project*
gets, and it is genuinely a values question — throughput versus a human
checkpoint — not a technical one. It should be decided by whoever owns the risk,
per job type, rather than argued here.

**Narrower after [A5](#a5-the-missing-phase-onboarding-beacon-as-a-project),
though not closed.** As a linked-origin project beacon gets an operator-controlled
checkpoint for free: `req.origin.release` is explicit-trigger-only and lands on
GitHub as a PR into `main` that a human merges (§5.3). So for beacon, D2 is about
gating each job onto `integration`, not about gating code into the repo of record
— a smaller decision than the original framing, and one that can be taken per job
type after phase 0b rather than before it.

**That narrowing lapsed on 2026-08-09.** It rested entirely on the release PR a
linked-origin project gets, and a platform-owned beacon has none: a squash-merge
lands on the platform's `main` and the mirror publishes it. D2 is back at its
original width for beacon — see
[A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts), which
also applies to gap 8's row in [Gaps, ranked](#gaps-ranked).

## Ordering

Not a commitment — a dependency reading. Amended 2026-07-30; **phase numbers are
not reassigned** (children cite them), so the new project-onboarding phase is
**0b** and the new open question is 10. The `Depends on` column was re-derived on
2026-08-09 for the platform-owned import decision — every edge into 0b reversed —
per [A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts).

| Phase | Work | Depends on |
| --- | --- | --- |
| 0 | Land this doc | — |
| 0b | ~~Onboard beacon as a linked-origin project (§5.3)~~ → **Import beacon as a platform-owned project** — `chug-install.sh project-import`, a per-project mirror agent, `.github/workflows/` disabled ([A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts)) | ~~—~~ **1, 5, 6, 8 proven first**: it is a cutover, and nothing ports incrementally after it (A6) |
| 1 | CI as evaluators (category B) | ~~0b~~ — nothing; it **precedes** 0b (A6) |
| 2 | Host-exec backend prototype on one node (H; [#309](./309-host-native-execution.md) P0) | — |
| 3 | Cron (category E; [#310](./310-scheduled-jobs.md)) | — |
| 4 | OIDC issuer + JWKS + WIF provider ([#313](./313-workload-identity-image-builds.md) half A) | infra exposure |
| 5 | Deploys, forward-only (category C) | ~~0b,~~ 4 — and it **precedes** 0b (A6) |
| 6 | Image build and push (category D; #313 half B) | ~~0b,~~ 4, **plus an operator-provisioned registry** — **not 2**, per [A1](#a1-image-builds-do-not-dissolve-into-host-mode); and it **precedes** 0b (A6) |
| 7 | Node-level exclusive resources (H.5; #309 §5b / P4) | 2 |
| 8 | Mobile and simulator jobs on host nodes (F) | ~~0b,~~ 2, 3, 7 — and it **precedes** 0b (A6) |
| 9 | Job inputs → unblocks rollback (**landed**, [#311](./311-job-inputs.md) slice A; jobs #314–#317, #319) | — |
| 10 | Per-run placement — **design first**, an open question, not scheduled work ([A3](#a3-beacon-already-parameterizes-placement-per-run)) | — |

~~Phases 0b, 2 and 3 are mutually independent, and 9 is done. Phase 1 and every
category-B/C port now hang off 0b, which is the one thing in this table that
cannot be prototyped around: without a project there is nowhere for beacon's
`.chug/` config to live.~~

~~**Phase 0b is the cheapest and the most blocking** — it invents nothing (§5.3
ships) and unblocks the two largest categories.~~

**Both paragraphs are superseded**
([A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts)). 2, 3
and 9 are unaffected; 0b is neither independent nor first. It is the cutover, so
it is the *last* of 1/5/6/8's chain rather than their precondition, and it is the
most expensive phase in the table rather than the cheapest — the analysis above
priced the linked-origin mode, which is not the mode beacon gets.

**Phase 1 is the highest value for zero new capability** — it uses only
mechanisms that already ship.

**Phase 2 is the highest leverage**: it feeds 7 and 8, and it absorbs gap 4 —
per [A2](#a2-the-keyed-caching-gap-was-overstated) the caching problem is a
persistent-state problem, which is what host mode is for. It **no longer feeds
6** ([A1](#a1-image-builds-do-not-dissolve-into-host-mode)), so the leverage claim
is one dependant smaller than originally written. Start it early, and start it as
a *prototype on one node* rather than a design carried to completion. H.3's cost 1
is exactly the kind that resolves faster by contact than by argument: the ten host
analogues are enumerable from the trait today, but nothing in the tree tells you
which of them is hard.

**`flutter-integration-tests` is a good north star and a bad first target.** It
sits at the confluence of 2, 3, 7 and 8, which makes it a useful thing to
sequence *toward* and a terrible thing to attempt first. Prove phase 2 against a
boring, cache-heavy build instead.

## What this doc does not decide

- The schema *syntax* for a host-mode selector (H.3 fixes only that it needs an
  epoch bump and a `min_dispatcher` gate in the same commit) — **decided since**,
  in [#309](./309-host-native-execution.md) §3.
- The schedule file format (E fixes only the location and the two semantics
  questions) — **decided since**, in [#310](./310-scheduled-jobs.md).
- ~~Anything in gap 1~~ — **decided since and shipped**:
  [#311](./311-job-inputs.md) took the frame, and
  [A4](#a4-job-inputs-shipped-so-gap-1-is-retired) records what landed.
- **How a run picks its node** ([gap 10](#gaps-ranked) /
  [A3](#a3-beacon-already-parameterizes-placement-per-run)). This amendment
  states the collision with #311 Decision 1 and the shipped property test, and
  deliberately stops there: it is a `Job`-record contract change and wants its own
  doc.
- The concrete phase-0b sequence for beacon (which job types first, what beacon's
  `.chug/tasks/ci.sh` legs are, ~~what `origin/release` cadence the project
  wants~~ — the cadence question goes with the project kind, since a
  platform-owned project has no `origin/release` at all).
  [A5](#a5-the-missing-phase-onboarding-beacon-as-a-project) fixes only ~~that the
  project must be linked-origin and~~ that the phase exists; the kind is
  **platform-owned**, and 0b is the cutover rather than the prerequisite
  ([A6](#a6-beacon-imports-as-a-platform-owned-project-and-phase-0b-inverts)).
- Whether beacon's non-CI GHA usage (release notes, label automation, anything
  the survey classified into A) is worth reproducing at all, versus dropping.

## A6. Beacon imports as a platform-owned project, and phase 0b inverts

*Correction — 2026-08-09, job #520. Falsifies
[A5](#a5-the-missing-phase-onboarding-beacon-as-a-project)'s central requirement
and reverses every dependency edge into phase 0b. The Chuggernaut half is
verified against this tree at this date; the decision itself is the operator's,
recorded here because it overturns a premise this doc states as a requirement.*

### The decision

Taken by the operator on 2026-08-09, in two parts:

1. **beacon is imported as a platform-owned (chug-managed) project** — the same
   kind as `kasofsk/chuggernaut`, where the platform's bare repo owns `main` and
   GitHub is a read-only mirror force-pushed by a launchd agent
   (`deploy/prod/README.md` §3). **Not** a linked-origin project.
2. **beacon's GitHub Actions workflows are disabled at import** — disabled, not
   deleted.

### What it falsifies

[A5](#a5-the-missing-phase-onboarding-beacon-as-a-project) states the opposite as
a requirement — "beacon must be the other kind: a **linked-origin** project …
GitHub keeps owning `main` and chuggernaut never pushes it" — and the
[ordering](#ordering)'s phase 0b read "Onboard beacon as a linked-origin
project", with phases 1, 5, 6 and 8 depending on it and the prose calling 0b "the
cheapest and the most blocking".

**The mechanism A5 describes is not what is wrong with it.** Linked-origin ships
and A5's account of it is accurate: the `CHUG_ORIGIN_DEPLOY_KEY` /
`CHUG_ORIGIN_PAT` secrets, `chuggernaut admin project link`, the chuggernaut-owned
`integration` branch, the `chug/release-{n}` PR (`docs/spec.md` §5.3;
`crates/dispatcher/src/handlers/origin.rs`). That analysis is left standing as the
**rejected option**. What is false is that beacon *must* be that kind, and
therefore everything the ordering derived from it.

The prerequisites change with the kind. A platform-owned import needs none of
A5's origin secrets — `deploy/prod/README.md` §3 already records them as dead for
this project — and the push credential is an SSH **deploy key** installed out of
band by the operator, not a project secret
(`deploy/prod/chug-mirror-install.sh`: "no secret is stored here"). A5's last
bullet is unaffected: fleet capacity for a second project is the same problem
whichever kind beacon is.

### The consequence: 0b's role inverts

This is the substance of the correction, and it is a sequencing claim, not a
vocabulary one.

**Linked-origin would have been cheap, additive and reversible.** GitHub keeps
owning `main`, beacon keeps building on Actions, and the platform holds an
`integration` branch beside them. beacon's `.chug/` config could land there and
workflows could be ported **one at a time against real code**, with the still-live
Actions run as the control and an unported workflow costing nothing. Undoing it is
deleting a project.

**A platform-owned import is the cutover.** After it the platform owns `main`,
GitHub is a mirror the platform force-pushes over, and — per the decision's second
half — beacon's Actions are off. There is no dual-running window on the far side:
the day of the import, everything beacon's CI/CD does either runs on Chuggernaut
or does not run.

So the dependency reading flips. Phases 1 (category B), 5 (category C), 6
(category D) and 8 (category F) are no longer work that 0b unblocks; they are work
that must be **proven before** 0b, because there is no incremental-porting window
afterwards. 0b stops being the cheapest and most blocking phase and becomes the
most expensive one — the point of no return, which the table now reads as "1, 5,
6, 8 proven first".

### Why that is not a deadlock

A reader who sees only "0b inverted" will conclude the ports are blocked on a
project that is blocked on the ports. They are not, and the reason is already
routine in this repo: **a job type is proven against fixtures and proof job types
here, with no beacon anywhere.**

- `.chug/jobs/android-proof.yaml` boots the node's Android emulator under KVM and
  runs a real `flutter build apk --debug` of `fixtures/mobile` — phase 8's Android
  leg aimed at a stock in-repo fixture rather than at beacon
  ([#367](367-android-emulator-execution.md) A1/A2, jobs #374 and #395; phase 8's
  row in [Current state](#current-state) is where that leg stands).
- `.chug/jobs/mac-proof.yaml` did the same for the macOS leg on `gumbo-air-0`,
  driving the node's Xcode and a booted iOS simulator (jobs #506 and #509,
  [#490](490-agent-work-on-a-mac.md)).
- `.chug/jobs/gcp-proof.yaml` climbs [#313](313-workload-identity-image-builds.md)
  half A's ladder against chuggernaut's own GCP project — phase 4's and phase 6's
  auth half, with no consumer repo involved.

All three declare `wrap_up: type: none`, so a proof merges nothing and gates
nothing: the cost of being wrong is one report. That is the shape the pre-cutover
work takes for categories B, C, D and F — prove the *capability* and the shape of
the port here, then import.

**What a proof cannot cover, stated plainly.** A fixture proves the platform can
do the thing; it does not prove beacon's own `.chug/` config is right, because a
job type in beacon's repo does nothing until beacon is a project. Under
linked-origin that gap closed incrementally after 0b; under a platform-owned
import the first exercise of beacon's config happens after `main` has moved. That
residual risk is the price of the decision, and how to rehearse the cutover — a
throwaway platform-owned import of a beacon copy is the obvious candidate — is an
operator question this doc does not decide.

### Why the workflows are disabled

Not tidiness — a collision.

**A force-push still fires `push`-triggered workflows.** The mirror is a launchd
agent running `git push <remote> main:main --force-with-lease` on an interval
(300s by default — `deploy/prod/chug-mirror-install.sh`; every five minutes for
this repo, `deploy/prod/README.md` §3). GitHub delivers a `push` event for a
forced update like any other, so every mirrored push would trigger whatever
beacon's workflows trigger on. Left alone they would keep running against the
mirror, duplicating or racing whatever Chuggernaut just did.

**Deploys are the sharp case, and this doc already documented why.**
[Category C](#c-deploys-16-workflows) records that beacon has **no concurrency
guard on any deploy workflow**, so two dispatches can race the same VM image-tag
metadata; the single-writer dispatcher (`docs/spec.md` §3.1) removes that race for
free — *within* Chuggernaut. Two systems deploying is the same hazard with a
second driver the dispatcher cannot see, and no amount of single-writer discipline
on this side helps.

**Disabled, not deleted.** The workflow files stay in beacon's tree. They are the
specification each port is checked against — this doc's entire category map is a
reading of them, and phases 1 and 5 are re-implementations that need something to
be diffed against. Deleting them destroys the reference in the middle of the port,
and re-enabling one is how a partial rollback would work.

**How "disabled" is achieved is an open operator step.** Nothing in this tree does
it. `deploy/prod/chug-install.sh project-import` creates the project, pushes the
history in as `main` and installs the per-project mirror agent
(`deploy/prod/chug-mirror-install.sh`), and neither script touches beacon's
workflow directory; `.claude/skills/chug-install/SKILL.md` — the documented path
for importing an existing GitHub repo as a platform-owned project, which is worth
reading before 0b — covers the deploy-key step and says nothing about workflows
either. So this is an out-of-band action on the GitHub side, to be named in the
import runbook rather than invented here.

### What else moves

- **[D2](#open-decisions) re-widens.** A5 narrowed it on the strength of the
  release PR: for a linked-origin project the merge reaching GitHub `main` is a PR
  a human merges, so an operator checkpoint came for free. A platform-owned beacon
  has no release PR — a squash-merge lands on the platform's `main` and the mirror
  publishes it on the next tick. So beacon's D2 is back at its original width:
  whether a human gates each job, answerable only by a `human` evaluator at the
  highest stage (correction 2). The [gap 8](#gaps-ranked) row's "narrower again for
  a linked-origin project" lapses with it.
- **A commit becomes a publication.** `CLAUDE.md` records that for this repo,
  whose mirror is public and force-pushed every five minutes. Whether beacon's
  GitHub repo is public is a beacon fact this tree cannot check — but the shape is
  the same, so beacon's ignore rules and secret hygiene become a disclosure
  boundary at import, not afterwards.
- **Nothing else in the ordering changes.** Phases 2, 3 and 4 never depended on
  0b; 7 depends on 2; 9 is landed; 10 is unscheduled by decision, and
  [#361](361-per-run-placement.md) already answered "is this needed before the
  beacon import?" with no — an answer that does not turn on the project kind.
