# Structure assessment — readiness for module-scoped work (2026-07-23)

A point-in-time audit of how well the codebase is structured for scoping work
(jobs) by abstraction layer / module. Companion to `NORTH-STAR.md`, which
describes the target factoring this assessment motivated. `crates.md` remains
the living map; this file is a snapshot and is expected to go stale.

The bar: module-scoped work needs boundaries that are (a) explicit,
(b) enforced, and (c) aligned with how changes actually arrive.

## Verdict

**The Rust workspace is in good shape for layer/module scoping — with one big
caveat: nearly all the mass is inside one crate.** The web app is the
structurally weakest area: it has conventions but essentially no internal
layering, and its two biggest files are where most UI changes land.

## What was verified (not just read)

- **`crates.md` exists and is mostly honest.** The documented invariants hold
  in production code: `async-nats` appears only in `store`'s Cargo.toml; `api`
  does not depend on `dispatcher` (dev-dependency only, for integration
  tests); `types` is pure data. Every crate's `lib.rs` opens with a doc
  comment tying it to its spec section. This is exactly the scaffolding
  module-scoped work needs.
- **But `crates.md`'s dispatcher module map has drifted.** It documents a
  `handlers/` directory with one module per subject family — reality is a
  single 1,727-line `handlers.rs`. Nine current modules aren't documented at
  all: `channel`, `fleet`, `github`, `harvest`, `launch_queue`, `origin`,
  `run`, `seed`, `triage`. *(Since closed: `crates.md` now documents all nine,
  refactor-plan C7 split `handlers.rs` into `handlers/`, and the A3
  `MODULES.md` registry gate in `tasks/ci.sh` fails the build on a module
  without a row — this class of drift can no longer accumulate silently.)*
- **The dispatcher is ~14,700 lines — over half the workspace's library
  code** (next largest: `types` at ~4,000). Scoping "by crate" gives almost no
  discrimination for orchestrator work; the real modules are the dispatcher's
  internal files, and three are themselves monoliths: `eval.rs` (2,856),
  `exec.rs` (2,474), `core.rs` (2,417).
- **`state.rs` is genuinely pure** (zero `.await`s) — but it only checks that
  a transition is *legal*. The logic that decides *which* transition to take
  and performs its effects is interleaved with I/O in `eval.rs`, `exec.rs`,
  and `core.rs` (~150 awaits each). The pure domain core is tiny.
- **The web app has boundary *conventions* but no *layers*.** The good:
  exactly one ad-hoc `fetch` outside `api.ts` (a static manifest in
  `TrainHeader`), SSE isolated in `useEvents.ts`, one stylesheet with design
  tokens, almost no inline styles. The bad: 22 of 35 components/pages import
  `api.ts` directly — components fetch for themselves, so "components" vs
  "pages" is a folder split, not an abstraction layer. `JobDetail.tsx` is
  1,187 lines (over twice the next page) and is the gravity well for most UI
  changes. There is no `web/` equivalent of `crates.md`.

## Layers and modules as they exist today

### Backend (layered by the dependency graph — acyclic, enforced by Cargo)

| Layer | Modules | Role |
|---|---|---|
| L0 — domain | `types` | Pure data + YAML field-rules validation; no async, no I/O |
| L1 — infrastructure adapters | `store` (sole NATS integration), `vcs` (git shell-out), `container` (Docker/k8s backends) | Each wraps exactly one external system |
| L2 — capabilities | `auth` (JWT/SSH-CA/permissions), `agent` (provider trait + prompt assembly), `worker` (fleet node daemon) | Compose L0/L1 |
| L3 — services | `dispatcher` (orchestration core), `api` (HTTP↔NATS bridge), `webhooks` (event pusher, ~stub), `cli` (init/admin) | Talk to each other only over NATS |
| L4 — binaries | `chuggernaut` (fat bin, wiring only), `chuggernaut-channel`, `chuggernaut-ko`, `chuggernaut-harness` (static musl, injected into agent containers) | No logic |
| cross-cutting | `test-utils` | Embedded NATS harness, fakes, fixtures |

### Dispatcher internal modules (the de-facto module list for most backend work)

`core` (single-writer event loop), `state` (transition table), `graph`,
`queue`, `launch_queue`, `release`, `exec`, `eval`, `escalation`, `launch`,
`scan`, `cd`, `factory`, `triage`, `reconcile`, `handlers`, `channel`,
`fleet`, `origin`, `github`, `harvest`, `seed`, `run`, `config`.

The problem: the last ~9 of these exist only in code, not in `crates.md`.

### Web (folder conventions, not layers)

- `api.ts` — the single typed HTTP client (the one real boundary)
- `useEvents.ts` / `useFleet.ts` / `useTypewriter.ts` — hooks, ad hoc rather
  than a layer
- `jobFilters.ts`, `format.ts` — stray domain/presentation logic with no home
- `pages/` — 13 route components; `JobDetail` and `Project` carry most of the
  app
- `components/` — 22 components, most of which fetch data themselves
- `styles.css` — tokens + all styling (3,937 lines), `theme.tsx`

## Gaps to close for module-scoped work

1. **Re-sync `crates.md`'s dispatcher map to reality** — it is the document a
   scoping scheme would key off, and it is stale exactly where it matters
   most.
2. **Decide the module granularity for the dispatcher** — crate-level scoping
   is too coarse there; file-level works only if `eval`/`exec`/`core`/
   `handlers` are either split or accepted as "wide" modules.
3. **Give `web/` a real module map** — either promote the implicit layers
   (client → hooks/data → components → pages) into documented rules, or accept
   "the web app is one module." Splitting `JobDetail.tsx` is the first
   structural move either way.

`NORTH-STAR.md` turns these gaps into a target factoring and an incremental
migration order.
