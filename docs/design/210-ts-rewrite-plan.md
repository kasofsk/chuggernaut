# TypeScript dispatcher rewrite — plan

Status: SUPERSEDED and DORMANT — **nobody is executing this plan, and nobody is
expected to.** [#215](215-refactor-plan.md) reversed the decision it argues
(stay in Rust, refactor incrementally); the last substantive edit was 2026-07-24
(job #224) and no phase below is scheduled work. The phases are retained for the
ideas that outlive the reversal — golden decision traces, effects-as-data, the
wire-level conformance suite, the contract-first change rule — which #215
carries as its Track B and where they are actually tracked. There is no slice
table: a dormant plan's phases are not slices, and giving them landed states
would imply a queue that does not exist. The blockquote below is the original
supersession note, left as written.

> **Superseded by [`docs/design/215-refactor-plan.md`](215-refactor-plan.md).** The decision is to
> stay in Rust and refactor the existing dispatcher incrementally toward the
> north star — not to rewrite it in another language. Design #208 (the Python
> dispatcher, the language-migration alternative this plan argued against) is
> closed in the same move. This page is **retained for the ideas that outlive
> the reversal** — golden decision traces, effects-as-data, the wire-level
> conformance suite, and the contract-first change rule — which
> `docs/design/215-refactor-plan.md` carries forward as its Track B. Read it for those; do not
> action the language migration, the `ts/` workspace, or the shadow/cutover
> phases below.

Companion to `docs/README.md` (target factoring), `docs/reference/contracts.md` (interface
extraction), and `docs/reference/structure-assessment.md` (current-state audit). This is the
execution plan for rewriting the **dispatcher** in TypeScript, guided by those
documents. Everything else stays: `api`, `store`-defined bucket schemas, the
static musl MCP binaries, and the web app are unaffected (the web app later
*benefits* via the shared types package).

**Decision: TypeScript** (over Python). Rationale in brief: the contract
vocabulary (`Msg`, `Effect`, `JobState`) is ADT-shaped and TS checks
discriminated-union exhaustiveness at zero runtime cost — recovering the most
valuable thing the rewrite loses from Rust; one language with `web/` collapses
the contract layer to a single shared package; the single-writer event loop is
Node's native execution model; and the risky adapter surface (nkeys, NATS JWT,
age) has first-party or author-maintained TS libraries (`nats.js`, `nkeys.js`,
`typage`). Python's edge (Pydantic ergonomics) is covered by the zod-first
rule below. Iteration speed is equal: run via `tsx`/Node type-stripping, no
build step; type-checking lives in the editor and CI.

**A prior draft exists**: design #208 ("Python dispatcher behind a wire-level
conformance suite"). This plan supersedes its language choice; its conformance
-suite framing is independently re-derived here. Reconcile/close #208 as part
of phase 0.

## Ground rules

1. **The Rust dispatcher stays in production for the entire rewrite.** It is
   also the behavioral oracle: the conformance suite must pass against it
   before the TS implementation is measured against anything.
2. **The rewrite is executed through Chuggernaut itself** — work arrives as
   scoped jobs on the platform, with dependencies expressed as the job DAG.
   This is deliberate dogfooding: the module registry and contract-first
   change rule (`docs/reference/contracts.md`) exist precisely to make these jobs safely
   scopable. Human-supervised pieces go through the claim flow.
3. **Contract-first during the overlap.** Any behavior change to the Rust
   dispatcher while the rewrite is in flight must land in the conformance
   suite / golden traces first. The suite is the single definition of "what
   the dispatcher does"; both implementations track it.
4. **Write TS in north-star shape from day one.** No porting of today's file
   layout — `eval.rs`'s braided decisions-and-I/O is exactly what we are not
   reproducing. The TS tree is `domain/decide/` + `effects` + `interpret` +
   `adapters` from the first commit.
5. **zod-first wire types.** TS types are erased at runtime; every NATS
   message and KV read is validated with zod schemas, and the static types
   are derived from the schemas (`z.infer`), never written twice. Schemas for
   shared shapes are *generated* from the Rust `types` crate (phase 2), not
   hand-written.

## Repo layout

```
ts/                      # new top-level workspace (Node LTS, tsx, vitest)
  packages/
    types/               # generated: JSON Schema (from Rust `types` via
                         #   schemars) → zod schemas + inferred TS types;
                         #   field-rules matrices as consumed data
    conformance/         # the wire-level suite + golden decision traces
                         #   (runs against EITHER implementation via env)
    dispatcher/
      src/
        domain/          # PURE — no I/O, no Date.now in logic paths
          state.ts       #   transition table (port of state.rs, table-tested)
          graph.ts  queue.ts  release.ts
          decide/        #   ready / work / eval / merge_gate / wrapup /
                         #   escalation / triage — (view, event) → {transitions, effects[]}
          invariants.ts  #   check(state): violation[] — run after every msg in tests
        effects.ts       # the Effect union (discriminated, exhaustively switched)
        interpret.ts     # the only place effects meet adapters
        core.ts          # single-writer loop: recv → decide → interpret
        adapters/        # store (nats.js KV/streams), container (docker),
                         #   vcs (git shell-out), agent, auth (nkeys/ssh-keygen/typage)
        handlers/        # req.* subject families → Msg
```

## Phases

Each phase lists its exit gate and how it lands as platform jobs.

### Phase 0 — foundations (small, do immediately)

- Scaffold `ts/` workspace: strict tsconfig, tsx, vitest, zod, lint rules
  including the boundary rules (domain imports nothing from adapters/interpret;
  only interpret touches adapters).
- In Rust: add the **invariant checker** and per-module contract headers
  (`docs/reference/contracts.md` step 1) — pure gain now, and it hardens the oracle.
- Reconcile design #208.
- *Jobs:* 2–3 independent `code` jobs, no deps.

### Phase 1 — the oracle in testable form

- **Wire-level conformance suite**: re-express the dispatcher integration
  tests (12.3k lines, currently in-process against `Core`) as wire-driven
  scenarios — drive `req.*` subjects, assert on KV state and event streams.
  Green against the **Rust** dispatcher is the exit gate.
- **Golden decision traces**: instrument the Rust dispatcher (log every `Msg`
  in, every `set_state`, every effect out during test runs) and capture
  traces for the lifecycle scenarios. Traces are YAML fixtures in
  `ts/packages/conformance/` — language-neutral.
- This is the largest phase; budget it as real rewrite cost, not plumbing.
- *Jobs:* one per existing test file (lifecycle, execution, claim, batch,
  gate_and_human, fleet, origin, …), parallelizable after a first
  harness-establishing job that the rest depend on.

### Phase 2 — shared types, generated

- `schemars` derivation on the Rust `types` crate → JSON Schema artifact in
  CI → zod codegen into `ts/packages/types/`.
- Lift the YAML field-rules matrices into a data file; Rust and TS both load
  it (single validation source).
- Exit gate: round-trip property test — serialize from Rust, parse with zod,
  re-serialize, byte-compare.
- *Jobs:* 2 sequential (`schemars`+CI artifact, then codegen+round-trip).

### Phase 3 — de-risk the auth adapter (spike, parallel with 1–2)

- Prototype in TS: per-job NATS JWT minting (`nkeys.js`), SSH cert issuance
  (shell out to `ssh-keygen`, as Rust does), age decryption (`typage`).
- Exit gate: credentials minted by the TS spike are accepted by the live
  NATS/sshd/secret paths in a test environment.
- This is the likeliest place to find a blocker — surface it before the
  domain work is deep.
- *Jobs:* 3 independent spike jobs.

### Phase 4 — pure domain in TS

- `state.ts` transition table + `graph`/`queue`/`release` + the phase
  deciders, effects-as-data, invariants runner.
- Built against: spec §2.1/§2.2/§3, the golden traces (phase 1), and table
  tests ported from the Rust `state`/release unit tests.
- Start with `merge_gate` or `escalation` as the template decider; `exec` and
  `eval` decision logic last (they are the largest and the traces for them
  the most valuable).
- Exit gate: every golden trace replayed through the TS deciders yields the
  recorded transitions and effects; invariant checker clean across all
  property-test sequences.
- *Jobs:* one per decider module, DAG-ordered after the template lands.

### Phase 5 — interpreter and adapters

- `interpret.ts` + adapters: store (nats.js), container (Docker fleet
  placement, put-archive injection, worker-node proxying — the moderate-risk
  one), vcs (git shell-out), agent (prompt assembly, provider config).
- Exit gate: the full conformance suite (phase 1) green against the **TS**
  dispatcher with fake backend/provider, then with real Docker.
- *Jobs:* one per adapter, then a suite-green integration job.

### Phase 6 — shadow, then cutover

- **Shadow mode**: TS deciders consume the production event stream
  read-only, emit `[Effect]`, diffed against what Rust actually did.
  Effects-as-data is what makes this possible. Run until the diff is quiet
  over a representative period (including factory ingest and fleet churn).
- **Cutover** is a deployment swap — state lives in NATS, both
  implementations read the same buckets (why phase 2's fidelity gate is
  strict). **Rollback = redeploy the Rust binary.** Single-writer means the
  swap is atomic per deployment: never both live.
- Keep the Rust dispatcher deployable (CI-built) for a generous window after
  cutover.
- *Jobs:* shadow-harness job, then human-claimed cutover/deploy jobs (deploys
  require asking — existing ops policy).

## What "done" means

- Conformance suite + golden traces green against the TS dispatcher.
- Shadow diff quiet in production.
- `docs/reference/crates.md` updated: dispatcher marked superseded; `ts/` documented.
- docs/reference/modules.md registry covering the TS module set — the rewrite's output is
  also the scoping-by-module structure we set out to get.

## What we explicitly are not doing

- Not rewriting `api`, `store`'s schema ownership, `auth`'s server-side
  helpers used by sshd, the worker daemon, or the MCP binaries.
- Not running two writers, ever — no gradual per-project cutover.
- Not porting the Rust file layout; the north-star shape is the spec.
- Not letting the Rust dispatcher's behavior drift undocumented during the
  overlap (ground rule 3).
