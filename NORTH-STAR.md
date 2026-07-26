# Chuggernaut — structural north star

Companion to `crates.md` (the map as it exists), `structure-assessment.md`
(the audit that motivated this), `contracts.md` (how the dispatcher's
interfaces get extracted and formalized on the way here), `refactor-plan.md`
(the sequenced, ticketed path that executes this factoring incrementally in
Rust), and `STYLE.md` (the tiered blessed practices every touch is held to).
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
  handlers/        # one module per req.* subject family (as crates.md intended)
  contexts/        # non-lifecycle bounded contexts: factory, fleet, cd,
                   #   harvest, seed, origin, github, channel
```

Why this is the right target for *this* project specifically:

- **The correctness core becomes total-coverage-cheap.** `dispatcher::state`
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

- **`handlers.rs` → `handlers/`** per subject family — `crates.md` already
  specified this and reality had regressed to one ~1,700-line file. **Landed**
  (refactor-plan C7): thirteen modules, one per `req.*` subject family, each
  with a contract header and a `MODULES.md` row.
- **Promote the grab-bag modules into named contexts.** `triage`/`origin`/
  `github` (the forge-ingest context) and `fleet`/`cd`/`harvest`/`seed` (the
  platform-ops context) are bounded contexts that grew organically. **Landed**
  (refactor-plan C8): they became `crates/dispatcher/src/forge_ingest/` and
  `platform_ops/`, each a directory whose `mod.rs` carries the charter its
  members share and a `MODULES.md` section of its own. Platform-ops has since
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
`chuggernaut schema api` emits `schemas/api.schema.json`, `npm run codegen`
turns it into `web/src/api/types.gen.ts`, and both halves are drift-gated
(`committed_schemas_are_current` in cargo, `npm run codegen:check` in the web
stage of `tasks/ci.sh`). `api.ts` keeps only the fetch methods; what remains
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

The rule-level companion to this section is `STYLE.md` — the tiered blessed
practices that workers and reviewers hold each change to.

`crates.md` lists "invariants worth enforcing (e.g. via CI lint)" — in the
north star they *are* enforced, because agent-driven development erodes
disciplinary boundaries faster than human development does:

- CI check on the workspace dependency graph (only `store` has `async-nats`;
  `api` never depends on `dispatcher` outside dev-deps; `types` stays sync).
  A ~20-line script over `cargo metadata` is enough.
- ESLint boundary rules on the web side (`ui/` can't import `data/`; only
  `data/` imports `api/`).
- Every module directory carries a doc header, and **one MODULES.md registry**
  lists every scoping-eligible module with its one-line contract. That
  registry is what jobs get scoped against — and CI failing when a new
  top-level module lacks a registry entry is what keeps it from drifting the
  way `crates.md`'s dispatcher map did.

## Priority order for the incremental path

1. **Generated TS client** — small, self-contained, kills the fuzziest
   boundary first.
2. **Web data layer + feature folders** — feature-by-feature; `JobDetail`
   last, it shrinks naturally as pieces move out.
3. **Decider extraction in the dispatcher** — opportunistically, whichever
   phase the next job touches; `merge_gate` or `escalation` is a good proving
   ground before attacking `exec`.
4. **CI boundary enforcement + MODULES.md** — cheap; do it early, since it is
   what keeps 1–3 from regressing during the migration.
