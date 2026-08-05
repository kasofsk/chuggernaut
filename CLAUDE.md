# Chuggernaut — working notes

A NATS-backed job orchestrator: jobs form a DAG, the **dispatcher**
drives each through Ready→Work→Evaluation→WrapUp→Done in containers, the **api** bridges HTTP↔NATS,
and `web` is the operator UI (its own `CLAUDE.md`).

## Where the knowledge already lives

Don't re-derive these — read them:

- `docs/spec.md` — normative behavior (the data model, state machine, prompts). The source of truth.
- `docs/design/000-rationale.md` — rationale; `docs/reference/design-lifecycle.md` — the job/release lifecycle in depth.
- `docs/reference/crates.md` — the crate/module map: what each crate owns and why. Read before adding a crate
  or moving responsibility between them.
- `docs/reference/testing.md` — the test tiers and where a given test belongs (two are built;
  tier 3 is marked as intent, not machinery).
- `docs/reference/style.md` — the tiered blessed practices (machine-checkable invariants,
  reviewer-checked rules, principles). Hold every change to it; reviewers reject
  by rule name.
- `docs/README.md` — the entry point for the structural-direction cluster: target
  factoring, and the reading order into `docs/reference/structure-assessment.md` (current-state
  audit), `docs/reference/contracts.md` (extracting the dispatcher's interfaces), and
  `docs/design/210-ts-rewrite-plan.md` (the TypeScript dispatcher rewrite). Read before
  module-scoped restructuring work.
- `docs/implementation-notes.md` — per-module rationale, hoisted out of the comments
  job #342 deleted. Notes, not norms: `docs/spec.md` and the design docs still win.
- Each `crates/*/src/lib.rs` opens with a `//!` doc comment pointing at its spec section.

## Build & test

```sh
cargo build                    # from repo root
cargo test -p <crate>          # unit + integration for one crate
cargo test                     # whole workspace
```

Integration tests need **NATS** (and some need **Docker**). The `test-utils` harness
has a **shared** route and a **private** one (`crates/test-utils/src/nats.rs`), and they
do not overlap. Shared (`require_nats!`): the URL in `CHUG_TEST_NATS_URL`, else a `nats`
image via testcontainers, which needs Docker — it never execs a `nats-server` itself,
because its handle lives in a never-dropped `static`, so start one yourself and export
the URL (`nats-server -js & CHUG_TEST_NATS_URL=nats://127.0.0.1:4222 cargo test`).
Private (`NatsTestServer::spawn` / `spawn_with_config` / `require_nats_config`): never
reads that URL, and since #408 is a **local `nats-server -js` process per caller** when
the binary is on `PATH` — OS-chosen port, fresh temp store dir, both reclaimed on drop —
falling back to a private container otherwise (`CHUG_TEST_NATS_LOCAL=0` forces the
container). So the private-server files run on a Docker-less evaluator; only the
files needing a Docker **backend** still self-skip there. The skip guards are the
`require_nats!` / `require_nats_config!`
macros and `test_utils::backend_suite::docker_available()`; there is no `e2e!`
macro, and no tier-3 suite for one to guard (`docs/reference/testing.md`). **A skip is free and
must stay free**: since #407 an unreachable Docker daemon is a permanent,
process-wide verdict answered instantly, not a 5s retry backoff per call — that
was 55% of the suite's wall time. Measure on a **fresh** JetStream store dir
with `RUST_MIN_STACK=16777216`, or the numbers lie (`docs/reference/testing.md`). And a tier-2
binary that costs ~30s while still reporting `ok` is almost always a wait rescued
by the core's 30s scan tick, not a slow broker — `test_utils::wait::DEFAULT_TIMEOUT`
is **20s** so that now fails loudly instead of hiding (`docs/reference/testing.md`).

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
  two happened, naming the private-server files as dark on the URL-only path;
  since #408 they serve themselves from the same baked binary, so that
  subtraction now **understates** what ran. Otherwise it announces the skip. It is diff-aware, with two independent
  stages: a diff touching `web/` runs `npm ci && npm run build` (tsc + vite), a
  diff touching Rust paths runs the cargo gate, and a doc/config-only diff runs
  neither and gates in seconds.
- `.chug/tasks/ci.sh` also runs seven pure-shell gates **before** those diff-aware
  stages, so a web-only or docs-only change is still gated: the `.chug/jobs/*.yaml`
  version-skew check (spec §14.3 — **advisory and early**, see below), the
  `docs/reference/modules.md` registry check (`.chug/tasks/check-modules.sh`),
  `.chug/tasks/check-duplication.sh` — copy-paste
  detection via a pinned `jscpd@5.0.5` at `threshold: 0` (docs/reference/style.md Tier 1;
  ~30ms for the whole repo, so it is unconditional) — `.chug/tasks/check-comments.sh`,
  the comment lint, `.chug/tasks/check-doc-facts.sh`, the doc-fact gate,
  `.chug/tasks/doc-staleness.sh`, the staleness ledger, and
  since #385 **the repo's 21 `*.test.sh` shell suites**.
  Any clone fails the gate.
- **A doc's claims about the tree are gated on every job, whole-tree, as an
  error.** `.chug/tasks/check-doc-facts.sh` resolves every backticked path claim
  in every tracked `*.md` against `git ls-files`, and every backticked constant
  asserted with a value against the `pub const` in the tree — checks 1 and 2 of
  design [#415](docs/design/415-knowledge-architecture.md) D6, which lived in
  `doc-lint.sh` as warnings until S1b moved them here (~0.8s whole-tree).
  Whole-tree and every job because the claims are made by every job type: a
  `code` job orphaned ten tag-file references (#416) that a `docs`-scoped
  gate never saw. A claim that is correctly unresolvable is marked on its line —
  `<!-- intent -->`, `<!-- runtime -->`, `<!-- absent -->` (docs/reference/style.md's doc-claim
  rule). An unparseable token is skipped silently, and a check that cannot run
  exits **2** as a `LINTER ERROR`, never as a clean tree. `doc-lint.sh` keeps
  markdown well-formedness, relative links and the design-filename shape.
- **A doc whose subject moved after it did is *suspect*, and suspect is not
  wrong.** `.chug/tasks/doc-staleness.sh` (#415 D7, job #446) is the git-derived
  ledger: for each doc, the tree **files** it names, and whether any of them has
  a commit newer than the doc. Nothing is declared and nothing is maintained —
  no `last-verified:` front matter, no dates in prose. It reads check 1's path
  set through `check-doc-facts.sh --emit-paths` rather than answering "what paths
  does this doc name" a second time, and it is **advisory**: the whole-tree
  counts print on every job, the reading list itself is `.chug/tasks/doc-staleness.sh`,
  and the pre-commit hook only reports. The one blocking case is `--gate` on a doc
  **this diff edits** that is still suspect — which needs the branch to have
  edited the doc and *then* changed a file it names. It blocks nowhere else on
  purpose: failing a build for history nobody in the commit caused is how a
  ledger gets disabled, and at the commit no edit could clear it anyway. Only
  file claims are judged — a directory is newer than every doc the moment
  anything under it changes, so it is a constant, not a signal.
- **A slice table cannot claim a job that never merged.** Check 3 (#415 S5a,
  job #444) resolves `**Landed** (job #N)` in a `docs/design/*.md` table row
  against a `job/N: {type}` squash-merge commit, and refuses a head saying
  `Status: IMPLEMENTED` over a row still `Proposed`. It reads git, never the
  platform API, so a revoked job and one that never existed are the same
  finding. The job doing the landing is exempt — #415 D10 has it write the row
  in the same commit, so `job/N` cannot exist yet; its number comes from
  `$JOB_ID` or a `job/N` branch name. Everything else is skipped in silence: a
  doc with no slice table, a row it cannot parse, markdown outside
  `docs/design/`, and the whole check when the history holds no `job/N:` commit.
- **Version skew is gated twice, and only the dispatcher's half is authoritative.**
  The **dispatcher** refuses to merge a branch whose `.chug/jobs/*.yaml` or
  `.chug/schedules/*.yaml` declares a `min_dispatcher` above the running
  binary's `CONFIG_SCHEMA_EPOCH`, escalating with `merge_config_skew` naming the
  file and both epochs (spec §3.3 step 0, §14.3). It performs the merge and
  knows its own epoch, so it needs no API call, no credential and no env var,
  and cannot degrade to a pass. `config_schema_gate()` in `.chug/tasks/ci.sh` is
  the **advisory, early** signal: it asks `$CHUG_API_URL/api/v1/platform/config`
  only when that variable is set — which no task container sets — and otherwise
  compares against the checkout's own epoch, which catches a config declaring an
  epoch newer than the code beside it and nothing about any deployed
  dispatcher. A green CI skew gate is not evidence a dispatcher was consulted;
  #421 is the job that fixed reading it that way.
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
  GNU tooling, so hand-running them on macOS produces false reds (`docs/reference/testing.md`).
  Deliberately *not* in the pre-commit hook, which is ~2s by design.
- **Comments are banned; docs are not.** `.chug/tasks/check-comments.sh` rejects
  every non-doc comment in any Rust or TypeScript source and caps doc comments at
  two sentences (module headers exempt) — docs/reference/style.md Tier 1. The tree holds **zero**
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
  then runs the comment lint (`--staged` mode), the registry check, the
  duplication check, the doc-fact check (`--staged`, +0.16s) and the staleness
  ledger (`--staged`, advisory) over the staged
  diff — ~2s, so an agent learns about
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
  (`docs/reference/testing.md`). `chuggernaut_domain::state` (`crates/domain/src/state.rs`) and release
  validation are the correctness core — keep their branch coverage near-total. The
  dispatcher re-exports it as `dispatcher::state`, so both names resolve; the code lives
  in `crates/domain`.
- Factories and job-type config are **project-owned and repo-versioned** — v2 is a
  per-consumer forge, not a shared control plane. Config travels with the project repo.
- **`infra/` holds terraform roots, and they are versioned.** Config travels with the
  project repo, so `infra/gcp-proof/` (chuggernaut's own GCP project and workload-identity
  pool) is tracked; `.gitignore` excludes only the secret and derived half —
  `terraform.tfvars`, `.tokens/`, `.terraform/`, state, and the fetched `jwks.json`. It
  used to exclude **`infra/` entirely**, which silently swallowed every `git add` of a real
  root; if a `git add infra/...` appears to do nothing, that is the shape of the bug.
  **The operator applies. No job and no gate ever runs `terraform apply`** —
  `infra/README.md` is the runbook, including the trap that an invalid uploaded JWK set
  surfaces as `Error connecting to the given credential's issuer` and blames the issuer.
- **A commit here is a publication.** The GitHub mirror `kasofsk/chuggernaut` is **public**
  (verified 2026-08-04) and `deploy/prod` force-pushes `main` to it every five minutes, so
  anything that merges is on the public internet minutes later with no review step in
  between. That makes the `infra/` ignore rules a disclosure boundary rather than tidiness,
  and a new terraform root adds its own **before** its first `git add` — `infra/README.md`
  names what is excluded and why.
- **`gcp-proof` is the only job type that may declare `workload_identities:`.** Half A of
  design #313 is being *proven*, not adopted: `.chug/jobs/gcp-proof.yaml` climbs a
  six-rung ladder against chuggernaut's own project, and its stage-0 `no-identity`
  evaluator asserts rung 5b by declaring **no** identity — that absence is the assertion,
  so don't add one there or anywhere else. Rungs 3–5 speak **REST over `curl` + `jq`**,
  never `gcloud`: no job type here pulls a public image and neither agent image carries
  the SDK, so a curl rung proves the STS accepts our token but leaves #313 A3's
  "every Google client library already reads this shape" claim unverified — A3 says so.
  Rung 5b's **ambient** check is curl too, straight at the GCE metadata server and
  bounded, and its line distinguishes "no metadata server reachable" (what an on-prem
  worker reports, and it tested nothing) from "answered and minted nothing".
- Don't run destructive commands (deploys, restarts, data resets) without asking first.
