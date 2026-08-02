# Chuggernaut v2 — Testing Strategy

Normal tests plus fixture-driven end-to-end runs. Three tiers, in increasing cost; CI runs the first two on every PR, the third nightly and on demand.

## What CI actually runs

The merge gate (`.chug/tasks/ci.sh`) runs tier 1, and tier 2 when it can. The
`test-utils` harness reaches a server two ways and only two
(`crates/test-utils/src/nats.rs`): the URL in `CHUG_TEST_NATS_URL` when a caller
exports one, else a `nats` image started through testcontainers, which needs a
Docker daemon. It never execs a `nats-server` binary. `ci.sh` provides the first
— a communal Docker NATS for the whole gate when a daemon is usable, else the
image's baked `nats-server` started by the gate itself. That second path is **on by
default** since job 382 fixed the four dispatcher tier-2 tests that went red while
the tier was dark (`CHUG_CI_LOCAL_NATS=0` opts back out), which is what makes the
tier run on an evaluator container — those get no Docker socket. It buys the
**shared-server** files only:
`NatsTestServer::spawn`/`spawn_with_config` never consult the URL, so the
private-server files self-skip on a Docker-less host and the gate names them
rather than counting them. What the gate announces is the *result* of that
attempt and never a separate probe — the two drifting apart is what job 375 found,
and `.chug/tasks/ci.test.sh` pins both the claim and its size to the mechanism. It
prints the tier state up front and a per-tier pass tally at the end (`tier-2
(NATS): N passed across M file(s)`, flagged as an upper bound when the
private-server tests self-skipped, since cargo counts a skip as a pass), so a
green gate is never silently partial.

Tier 3 (real Docker containers) stays out of the gate and self-skips. If NATS is
unavailable, `ci.sh` prints `tier-2 (NATS): SKIPPED` and, when the diff itself
adds or edits a tier-2 test file, a loud `!!!` warning — such a change then needs
a **manual verification note** in the work summary. To run the tier locally
without Docker, start the `nats-server` binary yourself and point the harness at
it: `nats-server -js & CHUG_TEST_NATS_URL=nats://127.0.0.1:4222 cargo test` — the
private-server suites above still skip, so that is not a whole-tier run.

## Tier 1: Unit

Pure-logic tests, no I/O, colocated with the code:

- `types` — job type YAML parsing and the §1.1 field-rules matrices (table-driven: every field × work/eval subtype combination), task resolution `kind` validity (§1.2), serde round-trips for every wire type
- `dispatcher::state` — the §2.1 transition table, table-driven: every (state, trigger, guard) row asserts the resulting state and effects; invalid transitions assert rejection. The rework-budget boundary (`N ≤ rework_budget`), retry exhaustion, one-shot deadline, and pre-Work escalation rules each get explicit cases
- `store` key encoding — base64url round-trips for emails and KO subject/predicate including `.`/`/`-containing values; var/secret name validation
- `agent` prompt assembly — rework context block formatting, KO dedup with narrower-scope-wins
- `auth` — permission rules table (§7.5), SSH principal formatting, JWT claim round-trips

## Tier 2: Integration (per crate, real dependencies, fake peers)

- `store` against a **real NATS server** (`test-utils` reuses the `CHUG_TEST_NATS_URL` server when one is exported, else starts a `nats:2.10-alpine` container through testcontainers; skips only when neither is available): bucket creation, watch semantics, stream replay-from-sequence, request-reply retry
- `vcs` against **temp bare repos on disk**: branch lifecycle, squash-merge (clean, no-op, conflict), conflict-context builder, diff-by-job-state including the Done-state `git log --grep` recovery
- `container` against the **local Docker socket** (skipped when unavailable): launch/wait/kill/inspect/copy_file, bootstrap wrapper, resource limits
- `dispatcher` with **real NATS + fake `ContainerBackend` + fake `AgentProvider`** (`test-utils`): full lifecycle runs entirely in-process — seed jobs, drive Ready→Work→Evaluation→Done, retries, rework, escalation, revoke cascades, restart reconciliation (kill and restart the dispatcher task mid-run, assert §3.6 behavior), factory batching/backpressure with synthetic ingest events
- `api` with **real NATS + a stub responder**: route auth matrix, SSE replay via `Last-Event-ID`, secret encryption on write, ingest token validation

The fake backend/provider are deterministic and scriptable per test ("container exits 0 after committing file X", "agent calls submit_eval with pass=false"). This tier is where most behavioral coverage lives — it is fast enough for every PR.

## Tier 3: End-to-end (fixtures)

Full stack — NATS, dispatcher, API, real containers — driven from the `fixtures/` projects. The flow mirrors real usage: start from the issues/features defined in the fixture, seed the graph, run to completion, assert outcomes.

**Fixtures:**

- `fixtures/sample.json` — minimal 4-job graph; the smoke test
- `fixtures/studybuddy/` — realistic 26-job, 5-phase Flutter project with full ticket bodies and dependencies; the load-bearing e2e fixture

The v1 fixture format (`title`/`body`/`deps`/`priority`/`capabilities`) predates v2 job types. A v2 seed step maps each fixture entry to a job instance: ticket body → work prompt file committed to the project repo, `deps` → `inputs`, `capabilities` → job type selection. The seed tool lives in `cli` (`chuggernaut seed <project> <fixture>`), so e2e tests exercise the same path users do.

**Two agent modes:**

1. **Scripted agent** (default, hermetic, runs nightly): the work "agent" is a deterministic image that reads its prompt and makes predictable commits (e.g. writes a file named after the job). Asserts the *platform*: dependency ordering, branch/merge behavior (including forced merge-conflict scenarios via overlapping file edits), eval fan-out with command evaluators, escalation and task-inbox flows via the API, factory runs end-to-end (POST synthetic Sentry-style events to `/ingest/{source}`, assert triage job → created jobs → provenance and release policy).
2. **Real agent smoke** (manual/tagged, costs tokens): `sample.json` with real Claude against a scratch project — asserts provider integration, MCP tool wiring, and prompt delivery, not outcomes. Gated behind an env var with a hard token budget.

**Assertions** run against the public surfaces only — the HTTP API and git history (final graph state, one squash-merge per non-noop job with the §3.2 commit format, event stream contents) — never against KV internals, so e2e tests survive internal refactors.

## Duplication: integration tests are out of scope

The copy-paste gate (`.chug/tasks/check-duplication.sh`, STYLE.md Tier 1) runs at
`threshold: 0` over the repo, but `.jscpd.json` excludes `**/tests/**` and
`**/*.test.*` **deliberately**: integration-test setup blocks repeat by nature —
spawn NATS, seed a project, drive the same first three states — and forcing them
through shared helpers costs more in test readability than the duplication costs
in drift. A test should read top to bottom as the scenario it is.

Two consequences worth knowing:

- In-file `#[cfg(test)] mod tests` blocks are **in** scope — a glob cannot see
  inside a file. That is deliberate too: a tier-1 unit test module lives beside
  the code it pins, and a repeated `decide(...)` scaffold there is better named
  once as a local helper (`decide_ci_exit` in `domain::decide::eval`) than
  copied. Keep such helpers in the same test module, next to the fixtures.
- When a duplication genuinely belongs (a golden fixture, two tests that must
  stay independently readable), bracket it with `jscpd:ignore-start` /
  `jscpd:ignore-end`, putting the reason on the directive line itself (the
  comment gate below allows the directive, not a paragraph beside it). Never
  raise the threshold.

## Comments: tests are in scope

The comment lint (`.chug/tasks/check-comments.sh`, STYLE.md Tier 1) covers every
tracked Rust and TypeScript source, `tests/` included — no non-doc comment
anywhere in the tree, and doc comments capped at two sentences on the blocks a
change touches. A test that
needs a paragraph to explain what it pins is telling you the *test name* is
wrong: `escalates_when_eval_retries_are_exhausted` carries what a comment above
it would have said, and it carries it into the failure output.

The gate itself has a shell test rather than a Rust one — `.chug/tasks/check-comments.test.sh`,
run directly, no NATS or cargo — alongside `check-duplication.test.sh`,
`ci.test.sh` (the merge gate's own tier-2 announcement, driven against stubbed
`cargo`/`docker`/`nats-server`), `coverage.test.sh`, `doc-lint.test.sh`,
`modules-registry.test.sh` (which drives
`.chug/tasks/check-modules.sh`) and `.githooks/pre-commit.test.sh` (which drives
real `git commit`s in throwaway repos with the hook installed). Shell gates are
tested in shell: the tier-1/2/3 ladder above is about the platform's behavior,
and a gate's own behavior is not reachable from a cargo test.

## Conventions

- `test-utils` owns: the NATS harness (`CHUG_TEST_NATS_URL`, else a testcontainers-run `nats` container), temp-repo builder, fake backend/provider, fixture seeding, and `require_nats!`/`e2e!` guard macros that skip when NATS/Docker are unavailable
- Every bug fix lands with a regression test at the lowest tier that can express it
- Coverage is tracked per crate (v1 discipline carries over); `dispatcher::state` and `release` validation are held to ~100% branch coverage — they are the correctness core

## Coverage: on demand, never a gate

Numbers come from releasing a **`coverage` job** (`.chug/jobs/coverage.yaml`),
which runs `.chug/tasks/coverage.sh` in the `agent-rust` image: a pinned prebuilt
`cargo-llvm-cov` fetched per run (the platform rebuilds its images on every node
on every deploy, so occasional-use tooling stays out of them), an instrumented
`--workspace --all-features` run, and the human summary printed **last** —
stdout is the deliverable, and a worker keeps only its final 700 KiB. It carries
no commits and merges nothing (`wrap_up: type: none`).

It is deliberately not wired into `_defaults.yaml` or `ci.sh`: coverage is a
thing you ask for, not a thing that runs on every push
([#308](docs/design/308-gha-port.md) §G).

Two limits to read the number with. The run starts the image's `nats-server` and
exports `CHUG_TEST_NATS_URL`, so tier-2 executes — but tier 3, and the tier-2
suites that need a *private* server, still self-skip without Docker, so every
percentage is a lower bound and the script says so. And `coverage.lcov` plus
`coverage-html/` leave the container only through the run's output archive: the
script tars them to `/workspace/chug-output.tar.gz`, which the dispatcher
harvests into the task's `output.tar.gz` artifact
([#362](docs/design/362-binary-artifacts.md) S1, spec §3.2) — download it from
the task's artifact list. It is capped at 16 MiB and refused whole above that.
