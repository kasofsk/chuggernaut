# Incremental restructuring plan — Rust, no rewrite

Companion to `NORTH-STAR.md` (target factoring), `contracts.md` (interface
extraction), and `structure-assessment.md` (current-state audit). This plan
**supersedes `ts-rewrite-plan.md`**: we are staying in Rust and refactoring the
existing code incrementally toward the north star. The rewrite plan's durable
ideas — golden decision traces, effects-as-data, contract-first change rule —
survive here as Track B; only the language migration is dropped.

Ground truth this plan is based on (2026-07-24): `state.rs` is the only pure
dispatcher module (zero awaits); `eval.rs`/`exec.rs`/`core.rs`/`handlers.rs`
carry ~640 await sites between them; no `Effect` enum, invariant checker, or
boundary lint exists yet; the web `api.ts` is a hand-written 754-line mirror
of the Rust `types` crate; schemars codegen is already a proven, drift-tested
pattern (`crates/cli/src/schema.rs` → `schemas/*.schema.json`); CI is
`tasks/ci.sh`, run as the job evaluator, which executes
`cargo test --workspace` on Rust-touching diffs.

## Shape of the plan

Five tracks. A and B start immediately in parallel; C builds on B; D is
independent; E builds on D. Each ticket is sized to be a scopeable
Chuggernaut job.

```
A1, A2 ──► A3          (week-one, all small)
B1 ─────► B2 ─► B3 ─► C1 ─► C2…C6 (opportunistic, one per subsequent touch)
C7, C8                 (any time, independent)
D1 ─► D2 ─► E1 ─► E2 ─► E3…E5
```

Once B2 lands, the contract-first change rule (`contracts.md`) is in force:
every dispatcher job names the `Msg` pre/postcondition, Effect, invariant, or
trace it changes — and if it can't, writing that contract is the job's first
commit.

## Track A — Docs & enforcement (cheap, do first)

These keep everything else from regressing during the migration.

**A1. Reconcile the planning docs** *(docs, small)*
Close/supersede design #208 (Python dispatcher) and mark
`ts-rewrite-plan.md` superseded by this plan, recording the decision: staying
Rust, refactoring incrementally.

**A2. Re-sync `crates.md` + create `MODULES.md`** *(docs, small)*
`crates.md`'s dispatcher map documents a `handlers/` directory that doesn't
exist and misses nine real modules (`channel`, `fleet`, `github`, `harvest`,
`launch_queue`, `origin`, `run`, `seed`, `triage`). Add contract-style doc
headers (accepts / emits / guarantees / spec §) to each dispatcher module and
seed `MODULES.md` as the registry of scoping-eligible modules, one line of
contract each.

**A3. Boundary checks in CI** *(code, small — depends on A2)*
Two homes, both already-established patterns:

- A workspace test crate (or `test-utils` tests) over `cargo metadata`:
  only `store` depends on `async-nats`; `api` never depends on `dispatcher`
  outside dev-deps; `types` stays sync; zero `.await` in `state.rs` (later:
  all of `domain/`). These ride the existing `cargo test --workspace` in
  `tasks/ci.sh` — same enforcement route as the `committed_schemas_are_current`
  drift test.
- The `MODULES.md`-completeness check goes in `tasks/ci.sh` itself, **before
  the Rust early-exit** (the `config_schema_gate` precedent): a docs-only diff
  skips cargo, so a registry check living only in Rust tests would be
  silently bypassed by exactly the changes most likely to break it.

## Track B — Dispatcher contracts (`contracts.md` steps 1–3)

**B1. Invariant checker** *(code, medium)*
Harvest the "must/always/never" statements — terminal states are absorbing,
the queue holds only Ready jobs, one attempt in flight per job, rdeps is the
inverse of deps, merge queue depth-1 per project, … — into one
`check_invariants(&CoreState) -> Vec<Violation>` function. Run it after every
message in the existing integration tests (they already hold the in-process
`Core`). Pure gain, zero restructuring.

**B2. Effect catalog** *(code, medium)*
Classify the ~460 await sites in `eval.rs` (152), `exec.rs` (163), `core.rs`
(151) into a `dispatcher::effects::Effect` enum (~20–30 variants: `PutJob`,
`LaunchContainer`, `SquashMerge`, `CreateTask`, `PublishEvent`,
`IssueCredentials`, …) plus an `interpret.rs` skeleton mapping each variant
to the existing ports (`ContainerBackend`, `AgentProvider`, `store`'s typed
accessors). No decision logic moves yet — this ticket makes the vocabulary
exist and compile.

**B3. Golden decision traces** *(code, medium — depends on B2)*
Instrument the dispatcher in tests: log every `Msg` in, every `set_state`,
every effect out. Capture YAML trace fixtures for the lifecycle scenarios
(start with `lifecycle.rs` and `gate_and_human.rs` — smallest high-value
files). Traces are ordinary `cargo test` fixtures, so the evaluator gates
them with zero extra plumbing. They pin behavior during Track C.

## Track C — Decider extraction (grow the pure core)

**C1. Module skeleton + template decider** *(code, medium — depends on B2, B3)*
Create `dispatcher/src/domain/` and move the already-pure pieces in
(`state.rs`, `graph.rs`, `queue.rs`, `release.rs`). Extract the first decider
as the template: **escalation** (`escalation.rs` is 46 lines — the cheapest
proving ground), signature
`decide_escalation(view, event) -> (transitions, Vec<Effect>)`, executed via
`interpret.rs`, pinned by B3's traces and B1's checker. Extend A3's
zero-await test to cover `domain/`. This ticket sets the pattern every later
one copies.

**C2–C6. One ticket per remaining phase** *(code, medium each — after C1)*
In rough order of risk: `merge_gate` (carved from `eval.rs`), `wrapup`,
`ready`, `eval` fan-out/reduce, `work`/`exec`. The two monsters (`eval.rs`
2,858 lines, `exec.rs` 2,494) go last, when the template is proven and their
traces are richest. Per NORTH-STAR, these are **opportunistic** — file C1 as
scheduled work, then extract whichever phase the next real job touches;
C2–C6 are backlog stubs plus a standing rule, not a scheduled series.

**C7. `handlers.rs` → `handlers/`** *(code, small, independent)*
Mechanical split of the 1,727-line file along its existing seams
(`spawn_container_handlers`, `spawn_api_handlers`, `spawn_tasks_handler`,
`spawn_read_handlers`, `spawn_worker_announce_handler`) into one module per
subject family — what `crates.md` already specifies.

**C8. Named contexts** *(code, small, independent)*
Group `fleet`/`cd`/`harvest`/`seed` (platform-ops) and
`factory`/`triage`/`origin`/`github` (forge/ingest) into two context
directories with doc headers. Mostly `git mv`; clean up the `factory.rs` /
`launch.rs` 2-line stubs here.

## Track D — Generated TS contract (NORTH-STAR priority #1, independent)

**D1. schemars over the HTTP surface** *(code, medium)*
Extend the existing gated pattern (`schema` feature, currently only on
`job_type.rs`) to the ~40 types the API serializes (`Job`, `Task`,
`QueueSnapshot`, `FleetStatus`, …) plus `crates/api`'s few request-body
structs. Emit via a new `chuggernaut schema api` subcommand into `schemas/`,
guarded by the same committed-schema drift test.

**D2. TS codegen + swap** *(code, medium — depends on D1)*
JSON Schema → TS types generated into `web/src/api/types.gen.ts`, replacing
the ~580 hand-mirrored interface lines in `api.ts`; the ~41 one-liner method
bodies stay hand-written. Exit gate: `tsc -b` green on generated types plus a
round-trip test (serialize from Rust, parse in TS).

## Track E — Web layering (after D)

**E1. ESLint + boundary rules** *(code, small)*
No ESLint exists today; add it with the import-boundary rules from day one
(`ui/` can't import `data/`; only `data/` imports `api/`) — as `warn` while
files still violate them, flipped to `error` per path as migration proceeds.

**E2. `data/` fetching layer** *(code, medium — depends on D2)*
One hook module per resource (`useJob`, `useProject`, `useFleet` — the
existing `useFleet.ts` is the seed), with `useEvents.ts` becoming the
invalidation bus rather than a thing pages consume raw. Rule: **only `data/`
fetches** (today 25 files import `api.ts` directly).

**E3–E5. Feature folders** *(code, one job per feature — after E2)*
Migrate `fleet`/`library`/`settings` first (small, self-contained), then
`project`, then **`JobDetail.tsx` last** — at 1,251 lines it shrinks
naturally as `features/job/` pieces move out. Each job flips the ESLint
boundary rule to `error` for its migrated paths.

## What we are explicitly not doing

- No language rewrite, no `ts/` workspace, no shadow mode or cutover.
- No dispatcher crate split — modules with `pub(crate)` discipline, not new
  crates (NORTH-STAR: crates are compile/deploy boundaries; the dispatcher is
  one deployable with one writer).
- No multi-writer anything; every refactor preserves the single-writer loop.
- No big-bang: `eval.rs`/`exec.rs` are dismantled one decider at a time
  behind traces, never rewritten wholesale.
