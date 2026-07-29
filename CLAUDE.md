# Chuggernaut — working notes

A NATS-backed job orchestrator: jobs form a DAG, the **dispatcher**
drives each through Ready→Work→Evaluation→WrapUp→Done in containers, the **api** bridges HTTP↔NATS,
and `web` is the operator UI (its own `CLAUDE.md`).

## Where the knowledge already lives

Don't re-derive these — read them:

- `spec.md` — normative behavior (the data model, state machine, prompts). The source of truth.
- `design.md` — rationale; `design-lifecycle.md` — the job/release lifecycle in depth.
- `crates.md` — the crate/module map: what each crate owns and why. Read before adding a crate
  or moving responsibility between them.
- `testing.md` — the three test tiers and where a given test belongs.
- `STYLE.md` — the tiered blessed practices (machine-checkable invariants,
  reviewer-checked rules, principles). Hold every change to it; reviewers reject
  by rule name.
- `NORTH-STAR.md` — the entry point for the structural-direction cluster: target
  factoring, and the reading order into `structure-assessment.md` (current-state
  audit), `contracts.md` (extracting the dispatcher's interfaces), and
  `ts-rewrite-plan.md` (the TypeScript dispatcher rewrite). Read before
  module-scoped restructuring work.
- Each `crates/*/src/lib.rs` opens with a `//!` doc comment pointing at its spec section.

## Build & test

```sh
cargo build                    # from repo root
cargo test -p <crate>          # unit + integration for one crate
cargo test                     # whole workspace
```

Integration tests need **NATS** (and some need **Docker**). Run these dependencies in
**containers, not host installs** — `test-utils` provides the NATS harness and an `e2e!`
guard macro that skips when Docker/NATS are unavailable. Prefer `nats-server` via Docker
over a brew install.

## CI — the evaluation gates ARE the CI

There is no `.github/` workflow here, and that does **not** mean "no CI is
wired." This project dogfoods itself: every change merges through a Chuggernaut
job, and the job's **evaluation criteria are the CI**. "Enforced in CI" in this
repo means "enforced by an evaluator" — never conclude the repo is ungated from
the absence of a workflow file.

- `.chug/jobs/_defaults.yaml` appends the `ci` **command evaluator** to *every* job
  type. It runs `.chug/tasks/ci.sh` (stage 1) against the job branch before any merge:
  `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace --no-fail-fast` with tier-2 tests
  executing against a real `nats-server`. It is diff-aware, with two independent
  stages: a diff touching `web/` runs `npm ci && npm run build` (tsc + vite), a
  diff touching Rust paths runs the cargo gate, and a doc/config-only diff runs
  neither and gates in seconds.
- `.chug/tasks/ci.sh` also runs three pure-shell gates **before** those diff-aware
  stages, so a web-only or docs-only change is still gated: the `.chug/jobs/*.yaml`
  version-skew check against the deployed dispatcher (spec §14), the
  `MODULES.md` registry check, and `.chug/tasks/check-duplication.sh` — copy-paste
  detection via a pinned `jscpd@5.0.5` at `threshold: 0` (STYLE.md Tier 1;
  ~30ms for the whole repo, so it is unconditional). Any clone fails the gate.
- Per-type **stage-0 agent reviewers** run first (`.chug/tasks/review-*.md`), so the
  slow gate is spent only on changes the reviewer accepts; `docs`/`design`
  jobs additionally gate on `.chug/tasks/doc-lint.sh` at stage 1.
- **Reviewers read; they do not run.** Agent evaluators launch under the
  read-only `Review` permission profile (spec §4.3) — no `cargo`, no `npm`.
  Executing is CI's job, so don't add "build it and check" to a
  `.chug/tasks/review-*.md`; add it to `.chug/tasks/ci.sh`.
- The wiring lives in `.chug/jobs/*.yaml` (job types) and `.chug/prompts/` (work/review
  prompts); the gates themselves are `.chug/tasks/*.sh` and `.chug/tasks/review-*.md`.

## Conventions that bite if you miss them

- **`.chug/` is the config root.** Everything the platform reads out of a project repo —
  `jobs/`, `prompts/`, `tasks/`, `tags/` (plus this repo's `schemas/`) — lives under `.chug/`,
  not the repo root (spec §1.1). Reads fall back to the bare repo-root path for refs and
  projects that predate the move, so both layouts resolve; only `.chug/` is ever written.
  Resolution lives in `types::config_paths` and `dispatcher::project_config` — don't hand-roll
  a second copy. The root `tasks/ci.sh` is a labelled migration bridge, not a second gate.
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
