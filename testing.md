# Chuggernaut v2 — Testing Strategy

Two tiers exist and the merge gate runs both — tier 2 whenever it can reach a
broker. A third, fixture-driven end-to-end, is **intent, not machinery**, and
its section says so. Nothing in this repo runs nightly: the gates are the
job-type evaluators ([CLAUDE.md](CLAUDE.md), "the evaluation gates ARE the CI").

A tier, gate or fixture this document describes as existing is a **factual claim
about the tree** — see [STYLE.md](STYLE.md) Tier 2 rule 5. Anything not built is
marked here rather than described in the present tense.

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

The tests that need a **real Docker daemon** —
`crates/container/tests/docker_backend.rs`,
`crates/worker/tests/nats_backend.rs`, `crates/dispatcher/tests/fleet_e2e.rs` —
are tier-2 files that self-skip on a Docker-less host through
`test_utils::backend_suite::docker_available()`. They are not a third tier; there
is no third tier to run.

If NATS is unavailable, `ci.sh` prints `tier-2 (NATS): SKIPPED` and, when the diff itself
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

## Tier 3: End-to-end (fixtures) — PLANNED, NOT BUILT

**No part of this tier exists. Nothing below is machinery you can run, and no
test can be "covered at tier 3" today.** Measured against the tree on
2026-08-03:

- `git ls-files fixtures/` lists 67 files, every one of them under
  `fixtures/mobile/`.
- The `chuggernaut` binary has no `seed` subcommand — the eleven it does have
  are `dispatcher`, `worker`, `api`, `webhooks`, `init`, `admin`, `ssh-cert`,
  `ssh-shell`, `ssh-authz`, `schema` and `validate`
  (`crates/chuggernaut/src/main.rs`).
- No file under `crates/` references `fixtures/` at all, so no test reads one.

Earlier revisions of this page described a `sample.json` smoke graph and a
load-bearing `studybuddy/` project under `fixtures/`, in the present tense. Those
were **v1** fixtures; the v1 tree was deleted when v2 was promoted to the repo
root (`c5bec73`, 2026-07-20) and nothing in v2 ever consumed them. The seed
command was v1's too (`Seed { .. }`, "seed jobs from a fixture file", in the v1
CLI); the v2 binary has never had one. What was left here was a v1 tier
described in the present tense plus a plan for porting it, with nothing to
separate the two.

### The intent, kept as intent

The tier worth building is a full-stack run — NATS, dispatcher, API, real
containers — driven from fixture projects: start from the tickets a fixture
defines, seed the graph, run to completion, assert outcomes. Two properties are
the reason it is worth building at all, and any future design should keep them:

- **Seed through the user's path.** Whatever creates the graph should be a
  command a user also runs, not a test-only back door, so the tier exercises
  the real entry point. (No such command exists; naming one here is what made
  the old text read as reportage.)
- **Assert against public surfaces only** — the HTTP API and git history (final
  graph state, one squash-merge per non-noop job with the §3.2 commit format,
  event-stream contents), never KV internals, so the tests survive internal
  refactors.

**Two agent modes** — the part of the old design most worth preserving, because
it is what keeps such a tier affordable:

1. **Scripted agent** (hermetic, the default): the work "agent" is a
   deterministic image that reads its prompt and makes predictable commits
   (e.g. writes a file named after the job). It asserts the *platform*, not a
   model: dependency ordering, branch/merge behavior including forced conflicts
   via overlapping edits, eval fan-out with command evaluators, escalation and
   task-inbox flows through the API, factories end to end (POST synthetic
   events to `/ingest/{source}`, assert triage job → created jobs → provenance
   and release policy).
2. **Real-agent smoke** (opt-in, costs tokens): a minimal graph against a real
   provider and a scratch project, asserting provider integration, MCP tool
   wiring and prompt delivery — never outcomes. Would need an explicit opt-in
   and a hard token budget.

Neither mode has a schedule to inherit — nothing here runs nightly, and this repo
has no `.chug/schedules/` at all. A built tier 3 would be released like the
on-demand [`coverage` job type](#coverage-on-demand-never-a-gate), or scheduled
under [spec §1.1](spec.md) ([#310](docs/design/310-scheduled-jobs.md)) if someone
writes the schedule.

**Building it is a separate job, and a design question first** — what seeds a
graph, what a fixture format is in v2, and what it costs per run are not settled
here. Do not treat this section as a work item already scoped.

### What `fixtures/` actually holds

One tree, and it is not an e2e fixture:

- [`fixtures/mobile/`](fixtures/mobile/README.md) — a stock Flutter app
  skeleton, a *build target* for the mobile-execution proofs
  ([#367](docs/design/367-android-emulator-execution.md) A2,
  [#322](docs/design/322-macos-native-runtime.md)). Nothing seeds it, no cargo
  test reads it, and it carries no job graph; what builds it is the on-demand
  `android-proof` job (`.chug/jobs/android-proof.yaml`), on the one node with
  `/dev/kvm`. Its README says what it is for.

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
run directly, no NATS or cargo. Shell gates are tested in shell: the tier-1/2/3
ladder above is about the platform's behavior, and a gate's own behavior is not
reachable from a cargo test.

## The shell suites: `*.test.sh`

Every gate script, hook and deploy script is pinned by a `*.test.sh` beside it —
18 of them, driving the real script inside a throwaway repo against stubbed
`cargo`, `npm`, `docker`, `nats-server`, `curl`, `ssh`, `flutter`, `adb` and
`emulator`. No NATS, no Docker, no network. Run one directly: `sh .chug/tasks/check-comments.test.sh`.

**`.chug/tasks/ci.sh` runs all of them, unconditionally, as its last pure-shell
stage** (job #385; before that nothing executed a single one, and
`coverage.test.sh` had been red for a day). Unconditional because a diff touching
only `deploy/`, `.githooks/`, `nix/` or a `.chug/tasks/*.sh` other than `ci.sh`
triggers neither the cargo stage nor the web one — those diffs are exactly what
these suites cover.

- **Discovery is `git ls-files '*.test.sh'`** — a new suite is picked up with no
  list to update, and tracked-files-only keeps `node_modules/` and `target/` out
  by construction. A glob that matches nothing **fails** the gate.
- **Bounded**: 60s per suite (`CHUG_CI_SUITE_TIMEOUT_SECS`), 120s total
  (`CHUG_CI_SUITES_BUDGET_SECS`); over either is a loud failure, because an
  unconditional stage's cost is every job's cost. Measured 2026-08-02 on the
  `agent-rust` container: **36.8s for the 17 that existed then**, of which
  `deploy/prod/update-refresh.test.sh` alone is 27.1s (stub polling sleeps);
  `android-proof.test.sh` (#367 A2) adds ~9s, most of it three deliberately short
  emulator bounds it waits out.
  The total is checked **between** suites, not after the loop — otherwise the
  real ceiling would be suite-count × per-suite cap — and the failure names the
  suites it therefore never ran. The per-suite cap is applied with `timeout`,
  which is *probed* before the stage announces the cap: no working `timeout`
  fails the stage rather than silently running the suites unbounded, so the
  announcement can never claim a bound that is not in force.
- Each suite runs with `CHUG_CI_SHELL_SUITES=0`, so the real `ci.sh` that
  `ci.test.sh` drives does not recurse into the suites again. Setting it to 0 by
  hand opts the stage out, announced.
- **The gate's Debian container is the authoritative environment.** These suites
  assume GNU tooling and Linux path semantics, so a hand-run on macOS reds some
  of them spuriously (BSD `sed` rejecting GNU label syntax in
  `agent-rust-image.test.sh`; `/var` → `/private/var` in `pre-commit.test.sh`).
  Trust the gate over a laptop.
- They are **not** in `.githooks/pre-commit`: the hook is ~2s by design and these
  are 36.8s.

## Conventions

- `test-utils` owns: the NATS harness (`CHUG_TEST_NATS_URL`, else a testcontainers-run `nats` container), temp-repo builder, fake backend/provider, record-fixture builders (`fixture::job` — one blank `types::Job` a test edits into its case, not a project fixture), and the skip guards: the `require_nats!`/`require_nats_config!` macros for NATS and `backend_suite::docker_available()` for Docker. There is no `e2e!` macro
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
exports `CHUG_TEST_NATS_URL`, so tier-2 executes — but the tier-2 suites that
need a *private* server, and the ones that need a Docker daemon, still self-skip,
so every percentage is a lower bound and the script says so. (Its wording calls
those Docker-needing suites "Tier-3"; they are the tier-2 files named above, and
tier 3 is not built.) And `coverage.lcov` plus
`coverage-html/` leave the container only through the run's output archive: the
script tars them to `/workspace/chug-output.tar.gz`, which the dispatcher
harvests into the task's `output.tar.gz` artifact
([#362](docs/design/362-binary-artifacts.md) S1, spec §3.2) — download it from
the task's artifact list. It is capped at 16 MiB and refused whole above that.
