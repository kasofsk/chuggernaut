# Incremental restructuring plan — Rust, no rewrite

Companion to `NORTH-STAR.md` (target factoring), `contracts.md` (interface
extraction), and `structure-assessment.md` (current-state audit). This plan
**supersedes `ts-rewrite-plan.md`**: we are staying in Rust and refactoring the
existing code incrementally toward the north star. The rewrite plan's durable
ideas — golden decision traces, effects-as-data, contract-first change rule —
survive here as Track B; only the language migration is dropped.

Design **#208** (the Python dispatcher) was the other language-migration
proposal; it is **closed/superseded** by this decision. No dispatcher rewrite
in any language is on the table — the surviving `ts-rewrite-plan.md` is kept
only as a source of the Track-B ideas above.

Ground truth this plan is based on (2026-07-24): `state.rs` is the only pure
dispatcher module (zero awaits); `eval.rs`/`exec.rs`/`core.rs`/`handlers.rs`
carry ~640 await sites between them; no `Effect` enum, invariant checker, or
boundary lint exists yet; the web `api.ts` is a hand-written 754-line mirror
of the Rust `types` crate; schemars codegen is already a proven, drift-tested
pattern (`crates/cli/src/schema.rs` → `schemas/*.schema.json`); CI is
`tasks/ci.sh`, run as the job evaluator, which executes
`cargo test --workspace` on Rust-touching diffs.

Read that paragraph as a **dated snapshot, not a current-state claim** — the
tracks it motivates have since overtaken four of its statements. The pure core
is now its own crate (`crates/domain`, six deciders under `decide/`), so
`state.rs` is no longer the only pure module; the `Effect` enum and
`interpret.rs` landed in B2; `invariants.rs` landed in B1; and the boundary
lint is `crates/test-utils/tests/boundary_guard.rs` (A3). Its remaining
figures have drifted with the tree and are kept as history — re-measure
before scoping against any of them. Tracks F–I below, and C7's line count,
are sized against the tree as of **2026-07-26**.

## Shape of the plan

Nine tracks. A and B start immediately in parallel; C builds on B; D is
independent; E builds on D. F–I extend the same de-braiding to the half of
the dispatcher Track C never aimed at, and all four build on B2's `Effect`
vocabulary and on B1a's invariant wiring. Each ticket is sized to be a
scopeable Chuggernaut job.

```
A1, A2 ──► A3, A4              (week-one, all small)
B1 ─► B2 ─► B3 ─► C1 ─► C2…C6  (opportunistic, one per subsequent touch)
B1 ─► B1a ─► F, G, H, I        (the invariant wiring the new tracks need)
C7, C8 ─► C9                   (any time, independent; C9's forge half after H)
D1 ─► D2 ─► E1 ─► E2 ─► E3…E5
B2, B3 ─► F1 ─► F2, F3, F4     (authoring)
F1, F4 ─► G1                   (reconcile, after F opens its heal window)
B2 ─► H1, H2                   (forge & triage, into C8's `forge_ingest/`)
B2 ─► I1, I2 ─► I3             (platform-ops; I3 alone reaches outside it)
```

**Why F–I exist.** Track C targeted the *execution* lifecycle — Ready → Work
→ Evaluation → WrapUp. Roughly half the dispatcher sits outside it with no
decider, no effects vocabulary and no traces: job **authoring** (~790 lines of
`core.rs` — `create_job` through `absorb_plan`, then `revoke_job` through
`unclaim_job` — carrying eight of the twenty-six `Msg` variants, the whole
create/edit/claim/revoke half of the write API), **reconcile** (855 lines),
the **launch queue** (699), the **forge** and advisory **triage** (under
`crates/dispatcher/src/forge_ingest/`: `origin.rs` 502, `github.rs` 207,
`triage.rs` 451), and **platform-ops** (`cd.rs`, `fleet.rs`, `harvest.rs`,
`seed.rs`, since C9 in `crates/platform-ops/`). C8 gave the last two of those
a directory to live in and C9 gave platform-ops a crate, which is what is
already done here; what none of them has, housed or not, is a decider. Two consequences
shape the tracks below: the `Effect` enum covers job-lifecycle writes only, so
each track extends it before it can carve; and `invariants.rs` has five rules
(`check_ready_queue_only_ready`, `check_rdeps_inverts_deps`,
`check_active_is_executing`, `check_merge_queue_is_wrapup`,
`check_terminal_is_absorbing`), all about the execution lifecycle — none for
batch membership, draft/claim consistency, or fleet slots — so each track
lands its own.

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

**A4. `clippy.toml` + the Tier 1 lint denies** *(code, small — sibling of A3)*
A3 landed the dependency-graph half of STYLE.md Tier 1 and left the clippy
half unfiled. `clippy::too_many_lines`, `unwrap_used` and `expect_used` are
all allow-by-default, so `tasks/ci.sh`'s `cargo clippy --workspace
--all-targets -- -D warnings` could not see them and the three rules were
reviewer-honour-system. A4 adds `clippy.toml` (`too-many-lines-threshold =
70`) plus `[workspace.lints.clippy]` denies in the root `Cargo.toml`, with
`lints.workspace = true` on every member.

It is a **ratchet, not a cleanup**: the ~280 pre-existing violations wear a
site-specific `#[allow]` with a `TODO` naming the ticket that dissolves them
(C6 for `eval.rs`/`exec.rs` — landed, so what it did not dissolve now carries
`TODO(io-split)`; C7 for `handlers.rs`, whose markers went with the split; C8
for `triage`/`origin`, whose three did not — see C8),
so the debt stays greppable while new code cannot add a violation without an
explicit, reviewable allow. Rewriting the oversized functions is Track C's
job, one decider at a time — a blanket crate-level `#![allow]` is rejected.
Test code takes STYLE.md's own "outside tests" exemption as a top-of-scope
`#![allow]` in `#[cfg(test)]` modules and `tests/` targets, for the two
panic lints only; `too_many_lines` applies to tests too. A4 also lands
`prettier` + an `npm run format:check` script in `web/`, leaving the gate
wiring to E1.

## Track B — Dispatcher contracts (`contracts.md` steps 1–3)

**B1. Invariant checker** *(code, medium)*
Harvest the "must/always/never" statements — terminal states are absorbing,
the queue holds only Ready jobs, one attempt in flight per job, rdeps is the
inverse of deps, merge queue depth-1 per project, … — into one
`check_invariants(&CoreState) -> Vec<Violation>` function. Run it after every
message in the existing integration tests (they already hold the in-process
`Core`). Pure gain, zero restructuring. **Landed** — with the wiring only
half-done, which B1a finishes.

**B1a. Finish the B1 wiring** *(code, small — depends on B1)*
`assert_invariants` is called from `lifecycle.rs` only. Fourteen integration
files hold an in-process `Core` — `batch.rs`, `claim.rs`, `draft.rs`,
`dynamic_fleet.rs`, `execution.rs`, `fleet.rs`, `fleet_e2e.rs`,
`gate_and_human.rs`, `golden_traces.rs`, `nats_submit.rs`, `origin.rs`,
`recovery.rs` and `task_output.rs` besides it — so the checker guards one of
fourteen. Lift the helper into `tests/common` and call it after every `Core`
call in all of them, which is what B1 specified. Cheap, and
a prerequisite for F–I: each of those tracks lands new invariants, and there
is no point adding rules to a checker that most tests do not run.

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

**C1. Pure-domain crate + template decider** *(code, medium — depends on B2, B3)*
*(Amended by the crate-decomposition decision — job #230: the pure domain is
its **own crate**, `crates/domain` / `chuggernaut-domain`, not a
`dispatcher/src/domain/` module directory, so purity holds by construction —
a crate with no `tokio`/`async-nats` dependency cannot drift into I/O.)*
Create `crates/domain` and move the already-pure pieces in: `state.rs`,
`graph.rs`, `queue.rs`, the `Effect` vocabulary from B2, and `release.rs`'s
pure half (the `vcs`-reading loaders stay dispatcher-side; the dispatcher
re-exports so call sites keep one surface). Extract the first decider as the
template: **escalation** — covering both the escalate/stall twins — signature
`decide(view, event) -> (Vec<Transition>, Vec<Effect>)`, executed via the
`Core::run_escalation` shim (`set_state` for transitions, `interpret.rs` for
effects; transitions first — the crash window is closed by a reconcile heal,
not write ordering), pinned by B3's traces and B1's checker. Extend A3's
checks: zero-await across all of `crates/domain`, and the crate's resolve
subtree must not reach `tokio`/`async-nats`. This ticket sets the pattern
every later one copies. **Landed.**

**C2–C6. One ticket per remaining phase** *(code, medium each — after C1)*
In rough order of risk: `merge_gate` (carved from `eval.rs`), `wrapup`,
`ready`, `eval` fan-out/reduce, `work`/`exec`. The two monsters (`eval.rs`
2,858 lines, `exec.rs` 2,494) go last, when the template is proven and their
traces are richest. Per NORTH-STAR, these are **opportunistic** — file C1 as
scheduled work, then extract whichever phase the next real job touches;
C2–C6 are backlog stubs plus a standing rule, not a scheduled series.
**The series is complete**: C2 (`merge_gate`), C3 (`wrapup`), C4 (`ready`),
C5 (`eval`) and C6 (`work`) have all landed. Every lifecycle phase's decisions
are pure functions in `crates/domain/src/decide/`, and `eval.rs`/`exec.rs` are
the launch/monitor halves plus the folds that drive them. What remains in the
two files is I/O assembly — prompt, credential and env building — tagged
`TODO(io-split)` where it still trips a Tier-1 lint; splitting it is mechanical
work with no contract to name, so it is not a Track C ticket. `io-split` is a
standing grep label, **not** a ticket id — there is no C-numbered stub behind
it, and a reader who greps for one should stop here.

**C7. `handlers.rs` → `handlers/`** *(code, small, independent)* — **landed.**
The 1,824-line file split along its existing seams into thirteen modules, one
per `req.*` subject family (`container`, `worker`, `status`, `projects`,
`origin`, `access`, `jobs` + `jobs_reply`, `graph`, `tasks`, `jobtypes`,
`repo`) plus the shared §6.5 `reply` envelope; the directory's `mod.rs` is wiring
only. Each module carries a contract header and a `MODULES.md` row, and the
registry gate in `tasks/ci.sh` now walks nested dispatcher modules the way it
already walked the domain crate's.

**C8. Named contexts** *(code, small, independent)*
Group `fleet`/`cd`/`harvest`/`seed` (platform-ops) and
`factory`/`triage`/`origin`/`github` (forge/ingest) into two context
directories with doc headers. Mostly `git mv`; clean up the `factory.rs` /
`launch.rs` 2-line stubs here.
**C8 has landed** as `platform_ops/` and `forge_ingest/`, each with a charter
`mod.rs` and its own `MODULES.md` section; the `factory.rs`/`launch.rs` stubs
were already deleted by job/225, so nothing was left to absorb. The A3
registry gate now walks the dispatcher tree recursively (`<dir>/mod.rs`
registers as `<dir>`), the same rule it already applied to `domain`.

C8 moved the files and stopped there, which is the whole of what a naming
change can do: the three `TODO(track-C8)` markers A4's allow-ratchet left
behind — two in `crates/dispatcher/src/forge_ingest/origin.rs`, one in
`crates/dispatcher/src/forge_ingest/triage.rs` —
are still in the tree, because the oversized functions they sit on shrink
only when their decisions leave. **They now resolve to H1 and H2.** Read C8
as having built the two houses that H and I move into: neither track has a
directory to deliver, and both carve their deciders out into `crates/domain`,
leaving the I/O tail where C8 put it.

**C9. Context crates** *(code, medium — depends on C8)* — **half landed.**
Graduate each named context from a directory to its own crate, under the same
rule the domain crate graduated by: only once the context's interface no
longer needs `&mut Core`.

**platform-ops has landed** as `crates/platform-ops`
(`chuggernaut-platform-ops`). What it needed from the core turned out to be
two ports it is handed, a `FleetView` (roster, launch-queue depth, and a
sync read-only `JobLookup` over the graphs) and a borrowed `ConfigSnapshot`;
`crates/dispatcher/src/platform_ops.rs` is what remains — the adapter that
gathers those off `Core`'s fields and decides nothing. The base-snapshot
mapping went the other way, to `config.rs`, since it reads dispatcher config
and this crate's `CHUG_GIT_SHA`. `boundary_guard.rs` pins the context's
allowed edges (the port crates, never `dispatcher`, dev-deps included) and
asserts the reverse edge so the arrow stays one-way; the `tasks/ci.sh`
registry gate now walks any context crate's `src/`.

**forge-ingest did not, deliberately** — `docs/design/238-forge-ingest-crate-boundary.md`
records the finding. `origin.rs` writes `release_holds` (a merge-gate input
`eval.rs` reads) and calls `pump_merges` from both `origin_sync` arms;
`triage.rs` creates task records and launches containers through the actor's
own machinery. Carving either today would mean a second writer of job state or
an "interface" wider than the code it replaces. It is a follow-on ticket
**after H1 and H2**, whose whole job is to empty those functions of decisions.

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
round-trip test (serialize from Rust, parse in TS). **Landed** — via
`json-schema-to-typescript` behind `npm run codegen`, with `codegen:check` in
the web stage of `tasks/ci.sh` (which `schemas/**` now triggers) and the round
trip over `chuggernaut schema api-samples` payloads. `api.ts` is 352 lines; the
envelopes with no Rust type moved to `web/src/api/envelopes.ts`, which is the
remaining hand-mirrored surface.

## Track E — Web layering (after D)

**E1. ESLint + boundary rules** *(code, small)*
No ESLint exists today; add it with the import-boundary rules from day one
(`ui/` can't import `data/`; only `data/` imports `api/`) — as `warn` while
files still violate them, flipped to `error` per path as migration proceeds.
E1 also owns the **web section of `tasks/ci.sh`**: the `npm run format:check`
that A4 wired into `web/package.json` has no caller yet, and turning it green
means a one-time `prettier --write` over the 55 files that currently fail it.

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

## Track F — Job authoring (the write API outside the execution lifecycle)

The operator-facing writes that Track C never reached: create, update,
draft, finalize, membership edits, claim, unclaim, revoke — eight of the
twenty-six `Msg` variants. Most of it is the record's Draft / Frozen /
Batched life, and the two that are not are deliberate: `ClaimJob` reserves
the *next* work attempt and so rejects Draft and Batched outright (F3), and
`RevokeJob` cascades from any state, which is why F4 is its own ticket.
This is where every field rule and membership rule in the system lives, and
it has no golden trace today. `ReleaseJob` is the ninth operator write and is
*not* in scope: C4's `ready` decider already owns the release decision; F
owns only what happens before the hand-off.

**F1. `decide::authoring` — create** *(code, medium — depends on B2, B3)*
Create (plain and batch), plus the batch-composition primitives
(`plan_batch`, `validate_member`, `batch_auto_description`) that
`finalize_job`, `edit_members` and `release_job` all share. Extracting the
primitives alongside create is what makes F2–F4 cheap; extracting create
alone strands them in `core.rs`.

*Signature.* This is the first decider that can **reject**, and that is the
point: elsewhere the shim pre-validates, here validation *is* the decision.
So a rejection is a variant of the outcome, not an `Err` — "these members are
stale, commit nothing" is a decision the decider reached, not a failure it
suffered:

```rust
pub fn decide(view: &AuthoringView<'_>, event: AuthoringEvent) -> AuthoringOutcome;

pub enum AuthoringOutcome {
    Committed { transitions: Vec<Transition>, effects: Vec<Effect>, step: AuthoringStep },
    Rejected(Rejection),
}

pub enum Rejection {
    Invalid(Vec<ValidationError>),  // 422 — field-rule / membership violations
    Conflict(String),               // 409 — well-formed, but the state forbids it
}
```

The shim matches once: `Committed` runs the steps below, `Rejected` converts
to the matching `CoreError`, so HTTP status codes are unchanged. The variant
(rather than `Err`) buys two things — the type says the refusal was *decided*,
so it is traceable (F1c), and `?` can never discard one silently. The cost is
that `decide/` now advertises two decider shapes; F1's doc header names the
rule that tells them apart: **a decider returns an outcome enum exactly when
refusal is one of its decisions.**

*View.* `AuthoringView { graph: Option<&JobGraph>, next_seq, owner, project,
now }`. Borrowing `&JobGraph` is a deliberate widening past `ReadyView`'s
pre-read `deps_done: bool` and belongs in the doc header as such: `ready` asks
the graph one question, `authoring` asks it N unpredictable ones, so a
projected snapshot would just be a hand-rolled `JobGraph` with the same reach.
`JobGraph` already lives in `crates/domain`, so the borrow costs no new
dependency. `next_seq` is pre-read from `counters.next()` — reads feed the
view, they are not effects (the `escalation` `next_task_id` precedent).
`CreateSpec` (today `CreateJobRequest` minus the owner/project the view
carries) moves to `types` beside `Job`: `domain` cannot see
`dispatcher::core`, and `types` is the crate both ends already depend on.
Note what that move *enables* rather than what it preserves — `api` types no
create body today, it forwards an opaque `Json<serde_json::Value>` straight
onto the `jobs.create` subject (`crates/api/src/routes.rs`), and
`CreateJobRequest` is referenced only inside `crates/dispatcher`. A
`types`-side spec is the prerequisite for `api` ever typing that body; it is
not a call site F1 has to keep compiling.

*Zero new `Effect` variants* — the strongest available evidence that B2 got
the vocabulary right. Plain create decomposes into `PutJob` (and
`interpret.rs` already dual-writes KV *and* the in-memory graph, exactly what
`create_job` hand-rolls today), `AppendRdep` × deps, and
`PublishEvent(job-created)`. A non-draft batch adds `Transition { member, to:
Batched }` × N plus `PublishEvent(job-batched)` × N — subsuming
`absorb_batch`, today duplicated across create, finalize and release.

*The shim grows a step.* Create is a **birth, not a transition** —
`assert_transition` has no entry edge, correctly. But a non-draft batch's
decision *contains* transitions, each carrying `batch_id: Some(next_seq)`.
Run transitions first and a crash before `PutJob(batch)` leaves `Batched`
members pointing at a batch that does not exist in KV — strictly worse than
today's window, where the batch exists and reconcile can re-drive the
absorption. So authoring's shim is five steps, not four:

```
0. seed              — AuthoringStep::Seed { record }, create-only, before any transition
1. gather            — next_seq, the clock, the project graph into the view
2. decide            — the pure call
3. apply transitions — each member Frozen→Batched through the set_state funnel
4. run effects       — PutJob, AppendRdep, PublishEvent via interpret
```

This is the only decider whose shim deviates from the fixed four-step shape,
and the deviation is forced by the create/transition distinction rather than
chosen for convenience. Pin it: add `batched_member_has_live_batch` to
`invariants.rs` so the ordering cannot silently regress.

*Split into three jobs.* **F1a** — move the two primitives that are still
dispatcher-side, `CreateJobRequest` (`core.rs:80`, → `types` as `CreateSpec`
per the view note above) and `BatchComposition` (`core.rs:138`), and relocate
`plan_batch`/`validate_member`/`batch_auto_description` into
`domain::decide::authoring` as free functions over `&JobGraph`; `core.rs`
keeps its methods as one-line delegates. Nothing to move for
`ValidationError` — it is already `domain`'s (`crates/domain/src/release.rs`),
which is why `Rejection::Invalid` can carry it directly; what stays behind is
the `From<Vec<ValidationError>> for CoreError` conversion (`core.rs:72`), now
reached from the shim's `Rejected` arm rather than an implicit `?`. No
behavior change, no trace change, reviewable on its own. **F1b** — the
decider, the `Seed` step, the shim
rewrite, the new invariant; mechanical once F1a lands. **F1c** — golden
traces `create_plain`, `create_batch_atomic`, `create_batch_draft`,
`create_batch_stale_member`, plus the trace-schema extension below.

*Trace schema extension (F1c).* Both trace funnels record **writes**, so a
rejected create performs none and would serialize as a step with an event
name and nothing else — indistinguishable from a step that decided to do
nothing, which is precisely the confusion the rejection rules must not fall
into. `TraceStep` gains one optional field:

```rust
/// The decision was to write nothing, and why.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub rejected: Option<String>,
```

Because it is `skip_serializing_if`, the existing eleven fixtures serialize
byte-identically — no re-baseline. The recording point is the shim's
`Rejected` arm, the one place the outcome is matched, which is why the
variant-not-`Err` choice matters: an `Err` propagated by `?` would have had
no single place to tap.

**F2. Update / draft / finalize / edit_members** *(code, medium — after F1)*
The Draft-editing family, all four of which call F1a's primitives.
`finalize_job` and `draft_job` also carry the `absorb_batch` /
`release_batch_members` twins that F1 already moved into the decider, so this
ticket is mostly deleting the duplicates. Lands the
`draft_batch_absorbs_nothing` invariant.

**F3. Claim / unclaim** *(code, small — after F1)*
Pure guard rules (terminal, Draft, Batched, attempt-in-flight) over a pre-read
"is a work task in flight" boolean — the one input that is I/O
(`tasks.list_for_job`) and therefore belongs in the view, not the decider.

**F4. Revoke** *(code, medium — after F1)*
A graph cascade with container kills and merge-gate eviction, so a different
shape from F1–F3 and worth its own ticket: `cascade_targets` is already pure
(`domain::graph`), but the per-target sequence (kill containers, close tasks,
drop from the ready queue and merge gate, delete branches, un-absorb members)
needs `KillContainer`/`RemoveContainer`/`DeleteBranch` effects that already
exist plus a `CloseTask` variant that does not.

## Track G — Reconcile *(after F)*

**G1. `decide::reconcile`** *(code, large — depends on F1, F4)*
"Given observed reality, what state should each job be in" is textbook
decider shape, and the whole C-track safety argument leans on it — *the crash
window is closed by a reconcile heal, not write ordering* — yet the heal that
justifies transitions-before-effects has no pure core and no trace. Carve
`decide(observed, view) -> (Vec<Transition>, Vec<Effect>)` and add restart
traces: a scenario driven to a mid-decision crash point, re-entered through
`spawn`, with the healing transitions recorded. Sequenced after F because
reconcile heals authoring state (a batch seeded but not absorbed — F1's new
window) as well as execution state.

## Track H — Forge & triage *(after B2 and B1a)*

The two request-driven surfaces left outside the job lifecycle. C8 already
seated both in `crates/dispatcher/src/forge_ingest/`, so H inherits the
directory rather than delivering it; each ticket moves its module's
classification into `crates/domain/src/decide/` and leaves the git, REST and
launch tail behind in `forge_ingest/`. They ride one track because they share
that shape — a small classification over a pre-read snapshot, braided with a
large I/O tail — not because their decisions are related.

Note what is *not* here, because the directory's name promises it: there is
**no issue-ingest path in this tree**. `crates/webhooks` is a
two-line TODO stub, and `crates/dispatcher/src/forge_ingest/github.rs` is a
minimal REST client (`create_pr`, `get_pr`, behind the `PullRequestApi`
trait) and nothing else.
Nothing in the dispatcher turns an issue into a job, so no ingest-shaped
effect variant belongs in the catalog.

**H1. `decide::origin`** *(code, medium — depends on B2)*
`crates/dispatcher/src/forge_ingest/origin.rs` (502 lines) runs the
linked-origin state machine inside the actor: link, release, status, sync. Its
decisions are already a clean classification
over a pre-read snapshot — most legibly `origin_sync`, which maps (release
status, PR merged/closed, whether origin main moved off `base_main_sha`) onto
one of Merged / Closed / leave-open, and, with no open release, decides
whether `integration` may fast-forward by asking `has_commits_beyond`.
Everything around that fork is git and GitHub REST.

Settle the signature first, because origin is the first decider whose subject
is **not a job**: its outcome is a new `ReleaseState` plus effects, and it
emits no `Transition` at all. Either the shared shape widens or origin returns
its own outcome type — decide that in the ticket rather than bending
`Vec<Transition>` to hold project state. The `Effect` enum likewise has no
forge variants, so this ticket adds them (push a `chug/release-{n}` snapshot,
open a PR, reset `integration` onto origin main) alongside the `PutProject`
that already exists. Watch the coupling to C2: clearing a release hold calls
`pump_merges` (`origin_sync`, both the merged and closed arms), so the hold is
a merge-gate input and the two deciders meet at `release_holds` — name that in
the doc header. `github.rs` stays put beside it, already a trait-behind port
so tests can script PR state; H1 dissolves the two `TODO(track-C8)` allows on
`link_project` and `origin_release` by emptying those functions of decisions.

**H2. `decide::triage`** *(code, small–medium — depends on B2)*
`crates/dispatcher/src/forge_ingest/triage.rs` (451 lines) is
**operator-dispatched advisory triage** (spec
§1.2), not ingest: an agent run over an *existing* Escalated or Stalled job
that records a `TaskPhase::Triage` task holding a written assessment. It
never creates a job and never touches an issue. It decides three things and
performs none of them: **admissibility** (Escalated or Stalled else 409;
`TRIAGE_IMAGE` configured else 422; repeatable, so deliberately no
once-only guard), the **shape of the task** it records (phase `Triage`, cycle
pinned to the job's latest so it sorts with the run it is triaging, id = next),
and **how a finished run is recorded** (`on_triage_exited`: an assessment →
Done + `task-completed`; none → Failed + a placeholder assessment +
`task-failed`, so a dead container is visible rather than a silent no-op).

Two things make it worth its own ticket. It refuses, with exactly F1's
`Validation` / `Conflict` pair, so F1's rule makes it an outcome-enum decider
too — the first evidence that the rule generalises past authoring, and
whichever of F1 and H2 lands second inherits the shape instead of inventing
one. And its defining guarantee is *negative*: triage never calls `set_state`,
so `triage_job` is deliberately not routed through `assert_transition`. That
cannot be stated as an `invariants.rs` rule — the checker reads state, not the
absence of an edge — so pin it with a golden trace instead: a triage scenario
whose recorded steps contain zero transitions. Effects: one `LaunchTriage`
variant beside `LaunchEvaluator` (the same platform-image, `Review`-profile,
no-channel-MCP launch), over the `PutTask` and `PublishEvent` that already
exist.

## Track I — Platform-ops *(after B2 and B1a)*

**I1. Launch-queue admission** *(code, medium — depends on B2)*
C1 moved the launch queue's *arithmetic* into `domain::queue` — the
drain-priority class (`launch_priority`) and the max-wait budget
(`waited`/`is_expired`) — which is exactly the scope `MODULES.md` claims for
it, and no more. Every *decision* stayed behind: at 699 lines
`launch_queue.rs` still owns whether a `NoCapacity` launch is deferred or
failed (`defer_launch`, `on_launch_deferred` — the two shapes differ because
the agent path's provider erases the error), which queued launch drains next
and whether it is still eligible when a slot frees (`drain_launch_queue`),
and when a launch has outwaited the queue and escalates
(`scan_launch_queue_timeouts`). None of it has a decider or a trace, and the
timeout backstop in particular is a branch that only fires on a wedged fleet —
precisely the kind that a golden trace can exercise and a live system cannot.
Carve those four, and widen the `MODULES.md` `queue` row to match once it
lands.

**I2. Occupancy + CD skew** *(code, medium — depends on B2)*
The two snapshot builders in `crates/platform-ops/` (C9 moved them there,
which does not carve them — a crate boundary is not a decider), both of which
compute a `platform`-bucket value and write it back only when the bytes
changed. Its `fleet.rs` is occupancy *reporting*, as its header says:
`fleet::compute` rebuilds which slot on which node is busy from the live
container list
(`ContainerBackend::list_managed_running`) rather than from in-memory
bookkeeping, and that reconstruction — container id → node → occupant, plus
the unlisted-node-is-out-of-service rule its one unit test pins — is the
decision worth carving. `cd.rs` is the version-skew half: the deployed SHA
against the self-repo tip, `commits_behind`, and the seed-vs-announced slot
merge in `merge_live_fleet`. Both funnel through the `WriteKv` effect that
already carries those snapshots, so I2 adds **no new effect variants** — it
converts two `async fn`s that interleave probing and computing into a probe
step and a pure `decide`.

*Not carved here.* Worker **placement** is not in platform-ops at all:
`container::choose_placement` (`crates/container/src/lib.rs`) is already a
sync `pub fn` over `&[PlacementCandidate]`, already carries the full /
out-of-service / unknown-pin rules (#60), and already has its own unit tests.
It is pure by construction in another crate, so I2 inherits it rather than
extracting it; the only thing this plan asks of it is the A4 ratchet marker it
already wears — an `expect_used` allow that comes off whenever the function is
next touched.

**I3. Fleet membership** *(code, medium — depends on B2, I2)*
Split out of I2 the way F4 was split out of F1, because it is a different
shape and — uniquely in Track I — it reaches **outside platform-ops**. Who
is in the fleet is decided in two places: `Core::on_worker_announce`
(`core.rs:1085`) gates on `supports_dynamic_workers`, then merges the announce
into the seed-vs-announced roster, and `Core::scan_worker_heartbeats`
(`scan.rs:54`) gates a node whose announce heartbeat has lapsed. Both are
double writes today — `backend.register_worker` /
`backend.mark_worker_unschedulable` *and* `self.fleet_roster` — which is the
second-writer smell an effect variant exists to remove, so this is where
`RegisterWorker` and `DrainWorker` earn their place in `effects.rs`. Note that
"drain" here means fleet gating only: running containers keep running, and the
word means the §3.6 actor shutdown everywhere else in the tree.

C8 gave the context its directory and its charter — "no member drives a job
transition" — but a charter is a claim, not a guarantee; I3 is what makes it
checkable, pinning the roster with a `fleet_slots_conserved` invariant. That
rule has nothing to read today: `invariants::CoreState` (`invariants.rs:52`)
carries only `graphs`, `queue`, `active` and `merge_gates`, so I3 widens it
and `Core::state` with the roster fields (`fleet_roster`,
`announced_workers`) — the first invariant input that is not job-shaped.

## What we are explicitly not doing

- No language rewrite, no `ts/` workspace, no shadow mode or cutover.
- **No _speculative_ crate splits.** Decomposing the dispatcher into small
  crates *is* now in scope (reversing this plan's earlier "no crate split"
  stance), but only as a consequence of the de-braiding, never ahead of it: a
  boundary graduates from a `pub(crate)` module to its own crate once (a) it
  aligns with a north-star seam — the pure domain first, later the platform-ops
  and forge-ingest contexts — and (b) its interface no longer needs `&mut
  Core`. The payoff is faster incremental compiles and purity enforced *by
  construction* — a `domain` crate with no `tokio`/`async-nats` dependency
  cannot drift into I/O, which a module lint can only catch after the fact. The
  single-writer loop and the single deployable are unchanged; these crates are
  compile/visibility boundaries only, never new writers or processes. Carving a
  crate before its decider extraction has dropped the `&mut Core` dependency
  stays out of scope. (C1 carved the first one — `chuggernaut-domain` — *in
  the same change* as the escalation decider extraction that dropped its
  `&mut Core` dependency, satisfying both conditions at once; later
  graduations still follow the rule, one boundary at a time.)
- No multi-writer anything; every refactor preserves the single-writer loop.
- No big-bang: `eval.rs`/`exec.rs` are dismantled one decider at a time
  behind traces, never rewritten wholesale.
