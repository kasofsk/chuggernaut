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
- `testing.md` — the test tiers and where a given test belongs (two are built;
  tier 3 is marked as intent, not machinery).
- `STYLE.md` — the tiered blessed practices (machine-checkable invariants,
  reviewer-checked rules, principles). Hold every change to it; reviewers reject
  by rule name.
- `NORTH-STAR.md` — the entry point for the structural-direction cluster: target
  factoring, and the reading order into `structure-assessment.md` (current-state
  audit), `contracts.md` (extracting the dispatcher's interfaces), and
  `ts-rewrite-plan.md` (the TypeScript dispatcher rewrite). Read before
  module-scoped restructuring work.
- `docs/implementation-notes.md` — per-module rationale, hoisted out of the comments
  job #342 deleted. Notes, not norms: `spec.md` and the design docs still win.
- Each `crates/*/src/lib.rs` opens with a `//!` doc comment pointing at its spec section.

## Build & test

```sh
cargo build                    # from repo root
cargo test -p <crate>          # unit + integration for one crate
cargo test                     # whole workspace
```

Integration tests need **NATS** (and some need **Docker**). The `test-utils` harness
reaches a broker **two ways and only two** (`crates/test-utils/src/nats.rs`): the URL in
`CHUG_TEST_NATS_URL`, else a `nats` image via testcontainers, which needs Docker — it
never execs a local `nats-server` itself, so a bare binary on `PATH` buys nothing unless
you start it and export the URL (`nats-server -js & CHUG_TEST_NATS_URL=nats://127.0.0.1:4222
cargo test`). That URL buys the **shared-server** suites only: `NatsTestServer::spawn` /
`spawn_with_config` (the `require_nats_config` guard) never read it, so the
private-server files still need Docker and self-skip without it. Prefer containers over
host installs. The skip guards are the `require_nats!` / `require_nats_config!`
macros and `test_utils::backend_suite::docker_available()`; there is no `e2e!`
macro, and no tier-3 suite for one to guard (`testing.md`).

## CI — the evaluation gates ARE the CI

There is no `.github/` workflow here, and that does **not** mean "no CI is
wired." This project dogfoods itself: every change merges through a Chuggernaut
job, and the job's **evaluation criteria are the CI**. "Enforced in CI" in this
repo means "enforced by an evaluator" — never conclude the repo is ungated from
the absence of a workflow file.

- `.chug/jobs/_defaults.yaml` appends the `ci` **command evaluator** to *every* job
  type. It runs `.chug/tasks/ci.sh` (stage 1) against the job branch before any merge:
  `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace --no-fail-fast`. Tier-2 executes only when
  the gate can hand the harness a broker — a communal Docker NATS, or the image's
  baked `nats-server`, which since #382 is the default on a Docker-less host
  (`CHUG_CI_LOCAL_NATS=0` opts out) — and says which of the
  two happened, naming the private-server files the URL-only path leaves dark;
  otherwise it announces the skip. It is diff-aware, with two independent
  stages: a diff touching `web/` runs `npm ci && npm run build` (tsc + vite), a
  diff touching Rust paths runs the cargo gate, and a doc/config-only diff runs
  neither and gates in seconds.
- `.chug/tasks/ci.sh` also runs five pure-shell gates **before** those diff-aware
  stages, so a web-only or docs-only change is still gated: the `.chug/jobs/*.yaml`
  version-skew check against the deployed dispatcher (spec §14), the
  `MODULES.md` registry check (`.chug/tasks/check-modules.sh`),
  `.chug/tasks/check-duplication.sh` — copy-paste
  detection via a pinned `jscpd@5.0.5` at `threshold: 0` (STYLE.md Tier 1;
  ~30ms for the whole repo, so it is unconditional) — `.chug/tasks/check-comments.sh`,
  the comment lint, and since #385 **the repo's 18 `*.test.sh` shell suites**.
  Any clone fails the gate.
- **The shell suites are the tests of the gates themselves, and CI runs them all.**
  Discovery is `git ls-files '*.test.sh'` — add a suite and it is picked up, with
  no list to update and nothing to register; a glob matching nothing fails rather
  than passing quietly. Bounded at 60s per suite and 120s total
  (`CHUG_CI_SUITE_TIMEOUT_SECS` / `CHUG_CI_SUITES_BUDGET_SECS`), the total checked
  *between* suites so it stops at the bound and names what it never ran; measured
  36.8s for the 17 that existed on 2026-08-02, 27.1s of that
  `deploy/prod/update-refresh.test.sh`, plus ~9s for `android-proof.test.sh`. The
  per-suite cap needs a working `timeout`, probed before the stage announces it —
  a host without one fails the stage rather than running it uncapped and quiet.
  Each suite is handed
  `CHUG_CI_SHELL_SUITES=0` so the real `ci.sh` that `ci.test.sh` drives cannot
  recurse. **The gate's Debian container is the authority** — these suites assume
  GNU tooling, so hand-running them on macOS produces false reds (`testing.md`).
  Deliberately *not* in the pre-commit hook, which is ~2s by design.
- **Comments are banned; docs are not.** `.chug/tasks/check-comments.sh` rejects
  every non-doc comment in any Rust or TypeScript source and caps doc comments at
  two sentences (module headers exempt) — STYLE.md Tier 1. The tree holds **zero**
  non-doc comments since job #342, so rule 1 is enforced over every tracked source
  rather than only the lines a diff adds; only the two-sentence cap is still a
  ratchet. The knowledge a comment would have carried goes in a doc and the
  rationale in the commit message; `.chug/tasks/docs-update.md` is the work
  task that keeps the docs in step, and `.chug/tasks/review-docs-updated.md` is
  its (currently inert) evaluator. Its scanner runs under `LC_ALL=C`, so the
  verdict is the same on every host and every awk (macOS's BWK awk aborts on the
  tree's astral-plane characters in a UTF-8 locale); a file the scanner cannot
  finish exits **2** as a `LINTER ERROR`, never as a comment violation.
- **The fast half of that gate also runs at the commit.** `.githooks/pre-commit`
  formats staged Rust/web files with `rustfmt`/`prettier` and re-stages them,
  then runs the comment lint (`--staged` mode), the registry check and the
  duplication check over the staged diff — ~2s, so an agent learns about
  a stray `//` before it exits instead of a rework cycle later. `prettier` runs
  from `web/` so `web/.prettierignore` applies: the Rust-emitted
  `web/src/api/wire-samples.json`, whose exact bytes a cargo test asserts, is
  never rewritten. It rejects only what `.chug/tasks/ci.sh` runs unconditionally
  (so it never blocks a commit CI would accept); `doc-lint` is advisory, and a
  gate that cannot run — missing tooling, an unreachable registry, a `LINTER
  ERROR` — degrades to a loud skip. `git commit --no-verify` bypasses it —
  legitimate when the alternative
  is leaving work uncommitted. Work containers get it from
  `container::bootstrap_cmd`; **a local checkout needs `git config
  core.hooksPath .githooks` once.** Its test is `.githooks/pre-commit.test.sh`.
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
  `jobs/`, `prompts/`, `tasks/`, `tags/`, `schedules/` (plus this repo's `schemas/`) — lives under `.chug/`,
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
