# Chuggernaut — the docs index and structural north star

Two things live here. [**The catalogue**](#the-catalogue) is the index: every
document in this tree, one line each, and it is gated in both directions so it
cannot quietly fall behind. Everything after it is the **structural north
star** — the target factoring the codebase refactors toward.

New to the tree? Start at [`docs/overview.md`](overview.md), which orients you
across the whole system in one reading and links out to the doc that owns each
part. Then `docs/spec.md` for what the platform does,
[`docs/reference/docs.md`](reference/docs.md) for how these documents are
written and gated, and the catalogue for everything else.

## The catalogue

One row per tracked `docs/**/*.md`, including this page, grouped by directory:
the root docs, then `reference/`, then `reference/runbooks/`, then `design/` by
number. It is kept honest by `.chug/tasks/check-doc-facts.sh` check 5, which compares the two
sets **both ways**: a doc with no row and a row naming no doc are equally a
finding, and both fail the gate in the pre-stage of every job. **Adding a
document is two acts, the file and its row** — the gate is what keeps the second
from being the one everyone forgets (design
[#415](design/415-knowledge-architecture.md) D15).

| Doc | What it is |
| --- | --- |
| [`docs/README.md`](README.md) | This page: the catalogue, and the target factoring the codebase refactors toward |
| [`docs/concepts.md`](concepts.md) | The concept registry — which doc owns each term's definition, and the criterion for a row |
| [`docs/design-docs.md`](design-docs.md) | A pointer: the design-doc header contract now lives in the doc policy |
| [`docs/implementation-notes.md`](implementation-notes.md) | Per-module rationale, hoisted out of the comments the tree no longer carries |
| [`docs/overview.md`](overview.md) | The synthesis page: the shape of the whole system, entirely as glosses linking to the doc that owns each part |
| [`docs/spec.md`](spec.md) | Normative platform behaviour: the data model, the state machine, the prompts |
| [`docs/reference/contracts.md`](reference/contracts.md) | Extracting and formalizing the dispatcher's interfaces |
| [`docs/reference/crates.md`](reference/crates.md) | The crate and module map: what each crate owns, and why |
| [`docs/reference/design-lifecycle.md`](reference/design-lifecycle.md) | The job lifecycle generalization and its vocabulary |
| [`docs/reference/docs.md`](reference/docs.md) | The doc policy: the two kinds of doc, the rules each obeys, and what the gates check |
| [`docs/reference/lifecycle-model.md`](reference/lifecycle-model.md) | The concrete job/task machine — states, events, transitions, effects, invariants, authority, ports — as a reimplementer needs it |
| [`docs/reference/modules.md`](reference/modules.md) | The module registry jobs are scoped against, one contract line per module |
| [`docs/reference/structure-assessment.md`](reference/structure-assessment.md) | The 2026-07-23 audit of readiness for module-scoped work |
| [`docs/reference/style.md`](reference/style.md) | The tiered blessed practices every change is held to |
| [`docs/reference/testing.md`](reference/testing.md) | The test tiers, what each costs, and where a given test belongs |
| [`docs/reference/runbooks/adhoc-deploy.md`](reference/runbooks/adhoc-deploy.md) | Runbook: deploying out of band |
| [`docs/reference/runbooks/chug-node-adoption.md`](reference/runbooks/chug-node-adoption.md) | Runbook: adopting the node modules in a worker host's own repo |
| [`docs/reference/runbooks/macos-host-supervision-proof.md`](reference/runbooks/macos-host-supervision-proof.md) | Runbook: proving host-task supervision on macOS |
| [`docs/reference/runbooks/worker-capacity.md`](reference/runbooks/worker-capacity.md) | Runbook: reading a worker node's capacity, changing it, and where each number comes from |
| [`docs/reference/runbooks/worker-docker-grant.md`](reference/runbooks/worker-docker-grant.md) | Runbook: granting a worker node's docker socket to one `(project, job type)` |
| [`docs/reference/runbooks/worker-host-projects.md`](reference/runbooks/worker-host-projects.md) | Runbook: declaring which projects a host node runs work for |
| [`docs/reference/runbooks/worker-host-users.md`](reference/runbooks/worker-host-users.md) | Runbook: provisioning a host node's per-project unix users, and why removing one is not symmetric |
| [`docs/reference/runbooks/worker-kvm.md`](reference/runbooks/worker-kvm.md) | Runbook: turning KVM on for a worker node |
| [`docs/reference/runbooks/worker-native-daemon-nixos.md`](reference/runbooks/worker-native-daemon-nixos.md) | Runbook: converting a NixOS worker node from the containerized daemon to the native unit |
| [`docs/design/000-rationale.md`](design/000-rationale.md) | The original v2 rationale: why the platform is shaped the way it is |
| [`docs/design/169-handoff-continuity.md`](design/169-handoff-continuity.md) | Audit: what one task hands the next, and where continuity breaks |
| [`docs/design/210-ts-rewrite-plan.md`](design/210-ts-rewrite-plan.md) | The dormant TypeScript dispatcher rewrite plan, kept for its analysis |
| [`docs/design/215-refactor-plan.md`](design/215-refactor-plan.md) | The incremental Rust restructuring plan — partly executed, now dormant |
| [`docs/design/238-forge-ingest-crate-boundary.md`](design/238-forge-ingest-crate-boundary.md) | Why forge-ingest stays inside the dispatcher for now |
| [`docs/design/293-worker-capacity.md`](design/293-worker-capacity.md) | Worker capacity: one source of truth, changeable from the UI |
| [`docs/design/308-gha-port.md`](design/308-gha-port.md) | Survey: what porting a real GitHub Actions suite onto Chuggernaut would need |
| [`docs/design/309-host-native-execution.md`](design/309-host-native-execution.md) | Host-native execution: node kind, selector, capabilities, exclusive resources |
| [`docs/design/310-scheduled-jobs.md`](design/310-scheduled-jobs.md) | Time-triggered job creation |
| [`docs/design/311-job-inputs.md`](design/311-job-inputs.md) | Job inputs: parameterizing a run without rewriting it |
| [`docs/design/313-workload-identity-image-builds.md`](design/313-workload-identity-image-builds.md) | Workload identity (an OIDC issuer) and image build/push |
| [`docs/design/321-job-groups.md`](design/321-job-groups.md) | Job groups: tying a job to the thing it belongs to, derived rather than stored |
| [`docs/design/322-macos-native-runtime.md`](design/322-macos-native-runtime.md) | A native macOS execution runtime for iOS/Xcode jobs |
| [`docs/design/323-paste-a-prompt-onboarding.md`](design/323-paste-a-prompt-onboarding.md) | Paste-a-prompt onboarding: standing up an instance and importing a repo |
| [`docs/design/355-project-task-images.md`](design/355-project-task-images.md) | Project-supplied task images |
| [`docs/design/361-per-run-placement.md`](design/361-per-run-placement.md) | How a run picks its node, and why that needs no new job-record field |
| [`docs/design/362-binary-artifacts.md`](design/362-binary-artifacts.md) | Binary artifact handoff between jobs |
| [`docs/design/367-android-emulator-execution.md`](design/367-android-emulator-execution.md) | Android emulator execution: a container with KVM, not a host runtime |
| [`docs/design/372-chug-node-modules.md`](design/372-chug-node-modules.md) | NixOS and nix-darwin modules for preparing a worker host |
| [`docs/design/373-project-toolchains.md`](design/373-project-toolchains.md) | Project-supplied toolchains: nix environments in container mode |
| [`docs/design/415-knowledge-architecture.md`](design/415-knowledge-architecture.md) | Knowledge architecture: one definition per concept, and prose that cannot go quietly stale |
| [`docs/design/440-native-worker-daemon.md`](design/440-native-worker-daemon.md) | The natively-supervised worker daemon |
| [`docs/design/490-agent-work-on-a-mac.md`](design/490-agent-work-on-a-mac.md) | Agent work on a Mac: finding a transcript without predicting its path, and what one host task per node buys |
| [`docs/design/517-docker-access-for-jobs.md`](design/517-docker-access-for-jobs.md) | Docker access for jobs, accepted: what it costs, when that stops holding, and which launches get a socket |
| [`docs/design/529-secret-handling.md`](design/529-secret-handling.md) | Secret handling: which of "declare it, scope it, clean it up" is already true, and what it would take to get the platform agent token out of the task's reach |
| [`docs/design/533-molt.md`](design/533-molt.md) | The molt: shedding the doc corpus at a milestone, what survives a deletion, and why there is nowhere for sheddings to go |
| [`docs/design/537-per-project-users-macos.md`](design/537-per-project-users-macos.md) | Per-project unix users on a macOS host node: what a session-less uid driving CoreSimulator restores, and what it costs the host backend |
| [`docs/design/543-placement-granularity.md`](design/543-placement-granularity.md) | Placement granularity: what a task needs of its node, why a pin names a machine instead, and the capabilities advertised with no reader |

## The target factoring

Companion to `docs/reference/crates.md` (the map as it exists), `docs/reference/structure-assessment.md`
(the audit that motivated this), `docs/reference/contracts.md` (how the dispatcher's
interfaces get extracted and formalized on the way here), `docs/design/215-refactor-plan.md`
(the sequenced, ticketed path that executes this factoring incrementally in
Rust), `docs/reference/style.md` (the tiered blessed practices every touch is held to),
and `docs/concepts.md` (which doc owns each term's definition).
This is the **target** factoring: where the
boundaries should sit if migration cost were no object. We are not migrating
all at once — this document guides incremental refactoring as changes land, so
that every touch moves the codebase toward one shape instead of many.

Context for the shape: we want to scope work (jobs) by abstraction layer /
module. That requires boundaries that are (a) explicit, (b) enforced, and
(c) aligned with how changes actually arrive.

The headline: the macro-architecture is already close to right for this type
of project — single-writer state machine, ports for containers/agents, one
NATS integration point, services communicating only over messages. The
top-level crate count grows only as boundaries inside the two big blobs harden
enough to earn a crate (see §1); the macro-topology is otherwise as-is. What
changes is **where the boundaries sit inside the two big blobs** (dispatcher,
web) and **how boundaries are enforced**
(docs and discipline today; types and CI in the north star).

## 1. Backend: grow the pure core, shrink the interpreter

The classic factoring for an orchestrator is **decider/effects separation**
(functional core, imperative shell): the domain is a pure function
`(state, event) → (new state, [effects])`, and one thin interpreter executes
effects through ports. The hardest prerequisite — the single-writer loop —
already exists. Today, though, the "decider" and "effect execution" are
braided together: `state.rs` is a pure transition-legality guard (zero
`.await`s — the seed crystal), but the logic that decides *which* transition
to take and performs its effects is interleaved with I/O across `exec.rs`,
`eval.rs`, and `core.rs` (~150 awaits each).

Target layout:

```
dispatcher/
  domain/          # PURE — no await, no I/O, exhaustively unit-testable
    state.rs       #   transition table (already pure)
    graph.rs, queue.rs, release.rs
    decide/        #   one module per lifecycle phase: ready, work, eval,
                   #   merge_gate, wrapup, escalation, triage
                   #   each returns Vec<Effect>, never performs one
  effects.rs       # Effect enum: LaunchContainer{..}, SquashMerge{..},
                   #   PublishEvent{..}, CreateTask{..}, ...
  interpret.rs     # the ONLY place effects meet ports (container/vcs/store/agent)
  core.rs          # the mpsc loop: recv msg → domain::decide → interpret (thin!)
  handlers/        # one module per req.* subject family (as docs/reference/crates.md intended)
  contexts/        # non-lifecycle bounded contexts: factory, fleet, cd,
                   #   harvest, seed, origin, github, channel
```

Why this is the right target for *this* project specifically:

- **The correctness core becomes total-coverage-cheap.** `chuggernaut_domain::state`
  and release validation are flagged as the correctness core, but today most
  of the actual decision logic (the ~2,900-line `eval.rs`) can only be tested
  through NATS+Docker harnesses. Pure deciders test at tier 1.
- **It's incrementally reachable.** Every time a flow in `eval.rs`/`exec.rs`
  is touched, its decision half can be carved into `domain::decide::*` with
  the I/O left in the interpreter. No big bang; the boundary grows one
  transition at a time.
- **It produces the module list for scoping.** "Jobs scoped to
  `domain::decide::merge_gate`" is a real, small, reviewable surface. "Jobs
  scoped to `eval.rs`" is not.

On crate decomposition: a dispatcher boundary graduates from a `pub(crate)`
module to its **own crate** once (a) it aligns with a seam above — the pure
domain first, later the platform-ops and forge-ingest contexts — and (b) its
interface no longer needs `&mut Core`. Two things earn the split: faster
incremental compiles, and purity enforced *by construction* — a `domain` crate
that depends on neither `tokio` nor `async-nats` **cannot** drift into I/O,
where a module lint can only catch the drift after the fact. Never split
speculatively: the crate follows the de-braiding, it does not lead it — carving
one before decider extraction has removed a boundary's `&mut Core` dependency
buys the tax without the guarantee. The single-writer loop and the single
deployable are unchanged; these crates are compile/visibility boundaries only,
never new writers or processes.

Two smaller backend targets:

- **`handlers.rs` → `handlers/`** per subject family — `docs/reference/crates.md` already
  specified this and reality had regressed to one ~1,700-line file. **Landed**
  (refactor-plan C7): thirteen modules, one per `req.*` subject family, each
  with a contract header and a `docs/reference/modules.md` row.
- **Promote the grab-bag modules into named contexts.** `triage`/`origin`/
  `github` (the forge-ingest context) and `fleet`/`cd`/`harvest`/`seed` (the
  platform-ops context) are bounded contexts that grew organically. **Landed**
  (refactor-plan C8): they became `crates/dispatcher/src/forge_ingest/` and
  `platform_ops/`, each a directory whose `mod.rs` carries the charter its
  members share and a `docs/reference/modules.md` section of its own. Platform-ops has since
  graduated to its own crate (`crates/platform-ops`, refactor-plan C9), having
  met the condition that makes a crate boundary real: nothing in it needs
  `&mut Core`. Forge-ingest has not — `origin` still writes the merge gate's
  `release_holds` and `triage` still records tasks through the actor
  (`docs/design/238-forge-ingest-crate-boundary.md`) — and it is the one
  dispatcher subsystem worth considering as its own *process* if it ever needs
  independent scaling, since it is the only one not part of the single-writer
  state loop's core job.

## 2. The contract layer: generate, don't transcribe

The HTTP surface used to exist three times: Rust types in `types`, handler
wiring in `api`, and a hand-written ~600-line `web/src/api.ts`. In the north
star, `types` is the single source and the TypeScript is **generated**.

This is the highest-leverage single change on the list: it converts the
fuzziest boundary in the system (backend↔frontend, currently synchronized by
human care) into a compiler-checked one — and it is exactly the boundary that
module-scoped agent jobs will trip over most.

**Landed** (refactor-plan D1+D2) as schemars → JSON Schema → TypeScript:
`chuggernaut schema api` emits `.chug/schemas/api.schema.json`, `npm run codegen`
turns it into `web/src/api/types.gen.ts`, and both halves are drift-gated
(`committed_schemas_are_current` in cargo, `npm run codegen:check` in the web
stage of `.chug/tasks/ci.sh`). `api.ts` keeps only the fetch methods; what remains
hand-written is `api/envelopes.ts` — the replies the dispatcher builds with
`serde_json::json!`, which have no Rust type to generate from. Shrinking that
file is what "finishing" §2 means.

## 3. Web: feature modules over technical folders, one fetching layer

`pages/` + `components/` is a technical split; it says nothing about what a
change touches. The target for an app this size is **feature modules
mirroring the backend's bounded contexts**, over a thin shared substrate:

```
web/src/
  api/           # generated client + ApiError (replaces hand-written api.ts)
  data/          # THE fetching layer: one hook module per resource
                 #   (useJob, useProject, useFleet, ...) — a small cache,
                 #   invalidated by the SSE stream (useEvents becomes the
                 #   invalidation bus, not a thing pages consume raw)
  features/      # job/, project/, fleet/, library/, factory/, auth/, settings/
                 #   each owns its route pages + feature components
  ui/            # presentational only — StateBadge, Skeleton, Markdown,
                 #   RichSelect... zero imports from api/ or data/
  styles.css, theme.tsx   # unchanged — the token discipline is already right
```

The rule that creates the layering: **only `data/` fetches.** Today 22 of 35
components/pages import `api.ts` directly, which means every component is
potentially a data component and nothing is reusable-by-construction. With a
data layer, `JobDetail.tsx` stops being ~1,200 lines of interleaved
fetching/state/markup and becomes a composition of `features/job/` pieces —
which is also what makes narrow UI jobs safe to scope.

Keep: single stylesheet, design tokens, SSE isolation. Those conventions are
correct; they need the same treatment as the backend invariants — enforcement.

## 4. Enforcement: invariants as CI, module map as registry

The rule-level companion to this section is `docs/reference/style.md` — the tiered blessed
practices that workers and reviewers hold each change to.

`docs/reference/crates.md` lists "invariants worth enforcing (e.g. via CI lint)" — in the
north star they *are* enforced, because agent-driven development erodes
disciplinary boundaries faster than human development does:

- CI check on the workspace dependency graph (only `store` has `async-nats`;
  `api` never depends on `dispatcher` outside dev-deps; `types` stays sync).
  A ~20-line script over `cargo metadata` is enough.
- ESLint boundary rules on the web side (`ui/` can't import `data/`; only
  `data/` imports `api/`).
- Every module directory carries a doc header, and **one docs/reference/modules.md registry**
  lists every scoping-eligible module with its one-line contract. That
  registry is what jobs get scoped against — and CI failing when a new
  top-level module lacks a registry entry is what keeps it from drifting the
  way `docs/reference/crates.md`'s dispatcher map did.

## Priority order for the incremental path

1. **Generated TS client** — small, self-contained, kills the fuzziest
   boundary first.
2. **Web data layer + feature folders** — feature-by-feature; `JobDetail`
   last, it shrinks naturally as pieces move out.
3. **Decider extraction in the dispatcher** — opportunistically, whichever
   phase the next job touches; `merge_gate` or `escalation` is a good proving
   ground before attacking `exec`.
4. **CI boundary enforcement + docs/reference/modules.md** — cheap; do it early, since it is
   what keeps 1–3 from regressing during the migration.
