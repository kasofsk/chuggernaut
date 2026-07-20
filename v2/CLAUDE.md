# Chuggernaut v2 — working notes

The active workspace. A NATS-backed job orchestrator: jobs form a DAG, the **dispatcher**
drives each through Ready→Work→Evaluation→Done in containers, the **api** bridges HTTP↔NATS,
and `v2/web` is the operator UI (its own `CLAUDE.md`).

## Where the knowledge already lives

Don't re-derive these — read them:

- `spec.md` — normative behavior (the data model, state machine, prompts). The source of truth.
- `design.md` — rationale; `design-lifecycle.md` — the job/release lifecycle in depth.
- `crates.md` — the crate/module map: what each crate owns and why. Read before adding a crate
  or moving responsibility between them.
- `testing.md` — the three test tiers and where a given test belongs.
- Each `crates/*/src/lib.rs` opens with a `//!` doc comment pointing at its spec section.

## Build & test

```sh
cargo build                    # from v2/
cargo test -p <crate>          # unit + integration for one crate
cargo test                     # whole workspace
```

Integration tests need **NATS** (and some need **Docker**). Run these dependencies in
**containers, not host installs** — `test-utils` provides the NATS harness and an `e2e!`
guard macro that skips when Docker/NATS are unavailable. Prefer `nats-server` via Docker
over a brew install.

## Conventions that bite if you miss them

- **The dispatcher is the single writer** of job records. State management is single-threaded
  by design — no CAS races, no multi-writer coordination, no "just add a lock". If a change
  seems to need multiple writers, it's the wrong shape; simplify instead.
- **`store` is the only crate that talks to NATS.** Everything else goes through its typed
  accessors. Don't reach for `async-nats` elsewhere.
- **`types` is pure data** — no async, no I/O. The YAML field-rules validation lives there so
  every consumer shares one implementation.
- New behavior lands with a regression test at the **lowest tier that can express it**
  (`testing.md`). `dispatcher::state` and release validation are the correctness core — keep
  their branch coverage near-total.
- Factories and job-type config are **project-owned and repo-versioned** — v2 is a
  per-consumer forge, not a shared control plane. Config travels with the project repo.
- Don't run destructive commands (deploys, restarts, data resets) without asking first.
