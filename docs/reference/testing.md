# Chuggernaut v2 — Testing Strategy

Two tiers exist and the merge gate runs both — tier 2 whenever it can reach a
broker. A third, fixture-driven end-to-end, is **intent, not machinery**, and
its section says so. Nothing in this repo runs nightly: the gates are the
job-type evaluators ([CLAUDE.md](../../CLAUDE.md), "the evaluation gates ARE the CI").

A tier, gate or fixture this document describes as existing is a **factual claim
about the tree** — see [docs/reference/style.md](style.md) Tier 2 rule 5. Anything not built is
marked here rather than described in the present tense.

## What CI actually runs

The merge gate (`.chug/tasks/ci.sh`) runs tier 1, and tier 2 when it can. The
`test-utils` harness reaches a server by a **shared** route and a **private**
one, and the two do not overlap (`crates/test-utils/src/nats.rs`).

The shared route (`NatsTestServer::shared`, the `require_nats!` guard) is the
URL in `CHUG_TEST_NATS_URL` when a caller exports one, else a `nats` image
started through testcontainers, which needs a Docker daemon; it never execs a
`nats-server` binary, because the handle lives in a `static` that is never
dropped and a child process there would outlive the binary unreaped. `ci.sh`
provides the URL — a communal Docker NATS for the whole gate when a daemon is
usable, else the image's baked `nats-server` started by the gate itself. That
second path is **on by default** since job 382 fixed the four dispatcher tier-2
tests that went red while the tier was dark (`CHUG_CI_LOCAL_NATS=0` opts back
out), which is what makes the tier run on an evaluator container — those get no
Docker socket.

The private route (`NatsTestServer::spawn`/`spawn_with_config`, the
`require_nats_config!` guard) never consults the URL — its callers need
production bucket names that cannot be namespaced, or a server configuration of
their own. Since job 408 it is a private `nats-server -js` **process** per
caller when the binary is on `PATH` (an OS-chosen port via `-p -1`, a fresh temp
store dir, both reclaimed on drop), and a private container otherwise;
`CHUG_TEST_NATS_LOCAL=0` forces the container. So the private-server files —
the five job 408 measured, plus `dispatcher/tests/workload_identity.rs` (#313
S4) — now run on a Docker-less evaluator too — but `announce_tier2` still subtracts
them from the tally whenever the gate's own NATS came from a URL rather than a
daemon, which since 408 **understates** what ran. What the gate announces is the
*result* of its start attempt and never a separate probe — the two drifting
apart is what job 375 found, and `.chug/tasks/ci.test.sh` pins both the claim and
its size to the mechanism. It prints the tier state up front and a per-tier pass
tally at the end (`tier-2 (NATS): N passed across M file(s)`, flagged as an upper
bound when the private-server tests self-skipped, since cargo counts a skip as a
pass), so a green gate is never silently partial.

The tests that need a **real Docker daemon** —
`crates/container/tests/docker_backend.rs`,
`crates/worker/tests/nats_backend.rs`, `crates/dispatcher/tests/fleet_e2e.rs` —
are tier-2 files that self-skip on a Docker-less host through
`test_utils::backend_suite::docker_available()`. They are not a third tier; there
is no third tier to run. Note that this is a per-*file* list, not a per-directory
one: `crates/container/tests/host_backend.rs` sits beside `docker_backend.rs` and
needs neither a daemon nor a broker. A *private NATS server* is no longer one of those
reasons; a Docker **backend** still is, and 13 of `nats_backend.rs`'s 20 tests
skip on that guard on a Docker-less host even though all 20 now get a server.
`declared_kvm_without_the_device_refuses_to_start` is one of the 13 and job 408
added its guard: it asserts a pure *config* refusal, but `daemon::local_backend`
constructs the Docker client before it validates the `WORKER_KVM` device path,
so on a Docker-less host the refusal under test is masked by `Socket not found:
/var/run/docker.sock`. Sequencing the local check first would make the assertion
host-independent; that is `crates/worker`'s to decide, not the harness's.

### What running the five private-server files costs (job 408)

Before 408 they cost ~15ms **because they did nothing**; the honest comparison is
against what running them buys. Measured per binary on one host with
`RUST_MIN_STACK=16777216`, cargo's own `finished in`, before → after:

| file | before | after | what runs now |
| --- | --- | --- | --- |
| `auth/tests/nats_live.rs` | 0.00s | **12.12s** | 1 test, operator-mode server |
| `cli/tests/init_admin.rs` | 0.01s | 0.81s | 3 tests |
| `dispatcher/tests/fleet_e2e.rs` | 0.00s | 0.17s | 2 tests |
| `worker/tests/nats_backend.rs` | 0.00s | 0.12s | 20 spawns, 7 tests past the Docker guard |
| `chuggernaut-channel/tests/stdio.rs` | 0.00s | 0.07s | 1 test |
| `test-utils/tests/local_nats.rs` (new) | — | 0.10s | 2 tests, 3 servers |

Cargo runs test binaries one at a time, so those deltas add: ~+13.3s on job 407's
23.44s whole-workspace baseline, ~36.7s in total. That is arithmetic on the table
above, not a sixth measurement.

**Starting the server is not the cost.** A private `nats-server` is up and
serving in ~34ms, so all ~26 of them across the five binaries are well under a
second in total. `nats_live.rs` is 12.1s because it asserts **denials**, and a
denial is only observable as a wait: four of its assertions wait 3s each for a
request the server must never answer. That is inherent to what it proves. It was
19.1s until 408 bounded the fifth one, which passed a 200ms *backoff* to
`request_with_retry(attempts = 1)` — where the backoff is never reached — and so
waited async-nats' 10s default request timeout instead (docs/reference/style.md Tier-2 rule 3:
every wait a timeout).

### A skip costs nothing (job 407)

A test that *cannot* run must not be allowed to cost time, because cargo counts a
self-skip as a pass and the cost is therefore invisible. Until job 407 it was not
free: `NatsTestServer::start` answered an unreachable Docker daemon with a
five-attempt retry loop whose backoff sleeps total **5s**, paid once per
`shared()` binary and once per `spawn()` **call** — so the five private-server
files burnt 30.1s of a 54.2s whole-workspace run (55%) proving Docker was still
absent. A client-init failure is now classified as permanent, recorded
process-wide, and answered instantly for every later caller; only a *transient*
container failure is still retried. Measured on one 12-core host against a local
`nats-server`, fresh JetStream store, per-binary: **54.19s → 23.44s**, with
`worker/tests/nats_backend.rs` 10.03s → 5ms and `auth/tests/nats_live.rs`,
`chuggernaut-channel/tests/stdio.rs`, `cli/tests/init_admin.rs`,
`dispatcher/tests/fleet_e2e.rs` each ~5.02s → 3–5ms. With no broker reachable at
all — a work container, where every NATS binary took the retry path — the same
run went **139.40s → 4.15s**. Every binary's pass count is unchanged.
`crates/test-utils/src/nats.rs`'s own unit test pins the property by pointing
`DOCKER_HOST` at a socket that cannot exist and asserting 20 spawns finish inside
2s.

Two measurement traps, both found the hard way here. **Reuse of a JetStream store
across runs skews everything after it** — every test namespaces its own buckets
and nothing deletes them, so a second run against the same `-sd` directory
measured `execution.rs` at 7.38s and a third at 18.57s against 3.59s on a fresh
one; start a clean server per measurement or the numbers are fiction. And
**`RUST_MIN_STACK=16777216` is not optional**: without it a dozen dispatcher
binaries abort with `has overflowed its stack` before asserting anything (`ci.sh`
exports it whenever the tier runs), and an abort looks like a fast binary.

`test_utils::wait::DEFAULT_TIMEOUT` is **20s**, sized from that run rather than
from a guess: all 244 waits in a whole-workspace run finished under 0.94s (p50
54ms, p99 552ms), and the worst wait seen with all 53 binaries forced to run at
once on 12 cores — far past anything CI's sequential binaries produce — was
4.31s. A wait that exceeds 20s is a hang, not a slow host, and should be read as
one.

### A wait that lands on a multiple of 30s is the scan tick, not a slow host

`dispatcher::core`'s scan ticker fires every **30s**, so a test whose assertion
one `trigger_scan` failed to satisfy is not lost — the next tick rescues it, and
the only evidence is a binary that takes 30s instead of 0.3s while still
reporting `ok`. That is what the 30.25s attributed to
`dispatcher/tests/dynamic_fleet.rs` was:
`heartbeat_loss_stops_placement_but_preserves_running` set a **1ms**
`worker_heartbeat_timeout` and then raced it, because `announce_worker` and
`trigger_scan` are two messages to one actor and the scan can read the heartbeat
it just recorded less than a millisecond old. Measured here at 4 of 15 runs
taking 31.1–31.4s against 2.5–3.5s, all 15 green under the old 60s ceiling; the
fix is `Duration::ZERO`, which makes the lapse a property of the scan rather
than of elapsed wall clock, and 40 runs then stayed under 4.2s with none failing.
The general rule: **a threshold a test asks the dispatcher to cross must not be
one the test has to out-run**, and a tier-2 wait quantised to 30s (or 60s, or
90s) is a scan-tick rescue rather than a slow broker.

If NATS is unavailable, `ci.sh` prints `tier-2 (NATS): SKIPPED` and, when the diff itself
adds or edits a tier-2 test file, a loud `!!!` warning — such a change then needs
a **manual verification note** in the work summary. To run the tier locally
without Docker, start the `nats-server` binary yourself and point the harness at
it: `nats-server -js & CHUG_TEST_NATS_URL=nats://127.0.0.1:4222 cargo test`. That
URL serves the shared suites; the private-server suites serve themselves from the
same binary on `PATH` (job 408), so with `nats-server` installed this *is* a
whole-tier run except for the files that need a real Docker daemon.

## Tier 1: Unit

Pure-logic tests, no I/O, colocated with the code:

- `types` — job type YAML parsing and the §1.1 field-rules matrices (table-driven: every field × work/eval subtype combination), task resolution `kind` validity (§1.2), serde round-trips for every wire type
- `chuggernaut_domain::state` (`crates/domain/src/state.rs`, re-exported as `dispatcher::state`) — the §2.1 transition table, table-driven: every (state, trigger, guard) row asserts the resulting state and effects; invalid transitions assert rejection. The rework-budget boundary (`N ≤ rework_budget`), retry exhaustion, one-shot deadline, and pre-Work escalation rules each get explicit cases
- `store` key encoding — base64url round-trips for emails and KO subject/predicate including `.`/`/`-containing values; var/secret name validation
- `agent` prompt assembly — rework context block formatting, KO dedup with narrower-scope-wins
- `auth` — permission rules table (§7.5), SSH principal formatting, JWT claim round-trips

## Tier 2: Integration (per crate, real dependencies, fake peers)

- `store` against a **real NATS server** (`test-utils` reuses the `CHUG_TEST_NATS_URL` server when one is exported, else starts a `nats:2.10-alpine` container through testcontainers; skips only when neither is available): bucket creation, watch semantics, stream replay-from-sequence, request-reply retry
- `vcs` against **temp bare repos on disk**: branch lifecycle, squash-merge (clean, no-op, conflict), conflict-context builder, diff-by-job-state including the Done-state `git log --grep` recovery
- `container` against the **local Docker socket** (skipped when unavailable): launch/wait/kill/inspect/copy_file, bootstrap wrapper, resource limits
- `container`'s `HostBackend` against **real processes on the test machine** (`crates/container/tests/host_backend.rs`, design #309 P0): the launch → inspect → logs → `logs_tail` → `copy_file` → `remove` round trip, the one-task-per-node exclusion, the refusal of a launch that declares an `image` (design #309 §1: the image's absence is what selects this backend), the group kill, and a simulated daemon restart. A host task is a process group and a directory, so this file needs **neither Docker nor NATS**. Five tests are the exception. Three are design #440 D3's supervision-unit assertions (a task's own scope, a task surviving the teardown of the launching unit, and D8's `setsid()` escapee reached through the scope); the fourth is job #459's `a_setsid_escapee_is_staged_under_a_scope_as_well`, which asserts no part of D3 and exists to measure whether an escapee stages under a scope at all, and the fifth is job #462's `a_scoped_task_is_handed_the_dollars_its_command_was_written_with`, which asserts that the scope's client does not rewrite the task's command on its way through. All five need a systemd that can create a transient scope **and** a cgroup-v2 hierarchy to read the result back from, and self-skip through `scope_or_skip` — printing the reason and "is NOT covered by this run" — on a machine with neither. The evaluator has neither (no `systemd-run`, pid 1 is `sh`), so they run only against a systemd host. They take the manager `probe_supervision` chose rather than naming one, so an unprivileged run gets the `--user` scope polkit grants it (job #451) and signals and tears down that same manager's units; a run as root gets the system scope. A `--user` run needs `XDG_RUNTIME_DIR` (or `DBUS_SESSION_BUS_ADDRESS`) in the environment `cargo test` itself carries — the probe hands them to its `systemd-run` client and refuses when it holds neither (job #453) — so an interactive login has them and an `env -i` invocation does not. Run them on a prospective host-mode node with `cargo test -p container --test host_backend -- --nocapture` and read the output for "skipping". Every read of a launched task's cgroup **taken at launch** goes through `supervised_cgroup`, which polls for the pid to enter the unit under a 10s bound rather than reading once: `systemd-run --scope` execs the task only after the manager's start job completes, so a read taken the instant `launch` returns sees the **launcher's** cgroup and fails an assertion the mechanism would have satisfied. Once membership is established there is nothing left to race, so a single read is the right assertion — which is what the teardown test's last one is, re-reading the task's cgroup after the teardown to show it was not reparented. That test depends on the polled check for both its stand-in daemon's unit and its task's, and on the task's cgroup not being inside the daemon's, before it tears anything down — an assertion that a task outlived a teardown says nothing unless the task was supervised at all. The two that assert D3 pass on a systemd node — measured on `gumbo-nuc-0` on 2026-08-06, which is what proves D3's Linux half through the shipped path ([#440's Linux execution](../design/440-native-worker-daemon.md#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456)); D3's third, D8's escapee, has never reached its assertion — it failed in its own staging on every run so far, and job #462 **diagnoses** that failure — from systemd's own documentation and source rather than from a run, none of the mechanism having been executed — as `systemd-run --scope` expanding the argv itself, collapsing the fixture's `"$$"` to `"$"` so the pidfile never held a number ([#440's client rewriting the command](../design/440-native-worker-daemon.md#correction-2026-08-06--the-scopes-client-was-rewriting-the-tasks-own-command-job-462)); `scope_args` now passes `--expand-environment=no`, and whether that clears the staging is one operator run away. Its staging — a `setsid()` child that leaves the task's process group — needs no systemd and is asserted on every machine by `a_setsid_escapee_is_staged_outside_the_task_process_group`, which also asserts that a task's `kill` does *not* reach the escapee, so D8's premise is measured where the scope's half cannot be. That passing test is also what rules the cwd, the log fds and the composed environment out as the staging failure's cause **on their own**: it carries all three through the same `HostBackend::launch` with the same `escapee_script`, differing only in `Supervision`, and stages the escapee anyway — so what remains is an interaction between the scope and one of them, which is where a fourth attempt would have to look ([#440's third attempt](../design/440-native-worker-daemon.md#correction-2026-08-06--the-escapees-own-trace-and-three-differences-ruled-out-job-458)). `a_setsid_escapee_is_staged_under_a_scope_as_well` is where that fourth attempt looks: the same script, helper and staging assertion with **only** the `Supervision` changed, so a red says the defect reproduces in the simplest fixture and D8's test is only its first victim, and a green says the cause is something the D8 test does and this one does not. It ran on `gumbo-nuc-0` at tree `e0b6570` and was red, which is what put the failure in the shipped launch path rather than in D8's fixture ([#440's one variable](../design/440-native-worker-daemon.md#correction-2026-08-06--the-one-variable-four-attempts-never-changed-job-459)). `a_scoped_task_is_handed_the_dollars_its_command_was_written_with` is that cause with no `setsid` in the way: a scoped task whose whole command is `printf %s "$$" > …`, asserting the file holds a pid, so a client that rewrites the dispatcher's command is red there and names itself instead of surfacing as a staging timeout three tests away. Since job #458 the escapee redirects its own stderr to an `escapee.trace` path of its own before it writes anything, so a failed pidfile write is told apart from a child that never ran — a discrimination `a_failing_escapee_write_reports_into_the_escapees_own_trace` asserts on every machine by making the pidfile path a directory. Every test that stages an escapee resolves `setsid` through `container::host::task_path()`, the `PATH` a launch composes, rather than through the environment `cargo test` carries — the same string today, since the floor copies the daemon's `PATH` and in tier 2 the daemon *is* the test process, so this is the guard following the floor rather than the caller and not a bug that fired. The macOS half of D3 has no test at all and is an operator procedure: `docs/reference/runbooks/macos-host-supervision-proof.md`
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
has no `.chug/schedules/` at all. A built tier 3 would be released like the <!-- absent -->
on-demand [`coverage` job type](#coverage-on-demand-never-a-gate), or scheduled
under [spec §1.1](../spec.md) ([#310](../design/310-scheduled-jobs.md)) if someone
writes the schedule.

**Building it is a separate job, and a design question first** — what seeds a
graph, what a fixture format is in v2, and what it costs per run are not settled
here. Do not treat this section as a work item already scoped.

### What `fixtures/` actually holds

One tree, and it is not an e2e fixture:

- [`fixtures/mobile/`](../../fixtures/mobile/README.md) — a stock Flutter app
  skeleton, a *build target* for the mobile-execution proofs
  ([#367](../design/367-android-emulator-execution.md) A2,
  [#322](../design/322-macos-native-runtime.md)). Nothing seeds it, no cargo
  test reads it, and it carries no job graph; what builds it is the on-demand
  `android-proof` job (`.chug/jobs/android-proof.yaml`), on the one node with
  `/dev/kvm`. Its README says what it is for.

## Duplication: integration tests are out of scope

The copy-paste gate (`.chug/tasks/check-duplication.sh`, docs/reference/style.md Tier 1) runs at
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

The comment lint (`.chug/tasks/check-comments.sh`, docs/reference/style.md Tier 1) covers every
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
27 of them (`git ls-files '*.test.sh' | wc -l`), driving the real script inside
a throwaway repo against stubbed
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
- **A suite beside `nix/chug-node/` still evaluates no nix.**
  `chug-worker-unit.test.sh` (#440 slice 7) compares the worker daemon's unit
  template against the unit `deploy/prod/build-worker.sh` renders, and the
  module's option defaults against that script's — text over text, in ~0.1s.
  Nothing in CI runs `nixos-rebuild` (#372 §2.3), so a green run says the two
  renderings agree and says nothing about whether the module evaluates.
- **Bounded**: 60s per suite (`CHUG_CI_SUITE_TIMEOUT_SECS`), 120s total
  (`CHUG_CI_SUITES_BUDGET_SECS`); over either is a loud failure, because an
  unconditional stage's cost is every job's cost. Measured 2026-08-02 on the
  `agent-rust` container: **36.8s for the 17 that existed then**, of which
  `deploy/prod/update-refresh.test.sh` alone is 27.1s (stub polling sleeps);
  `android-proof.test.sh` (#367 A2) adds ~9s, most of it three deliberately short
  emulator bounds it waits out. `check-doc-facts.test.sh` (#415 S1b, S4, S5a, S6)
  adds 0.65s (re-measured 2026-08-05 on the same container, after check 4's cases
  landed) — it stubs nothing, because all four checks it pins read a
  throwaway `git init` fixture rather than the tree: check 3 gets its own, whose
  two `job/N:` commits *are* the history it resolves against, and check 4 a third
  holding the registry, the doc that owns its one registered term, and a second
  doc to write about it from.
  `doc-staleness.test.sh` (#415 S6, job #454) adds ~0.2s, and its fixture is a
  *history* rather than a tree — three commits written with an explicit
  `GIT_COMMITTER_DATE`, because "the file moved after the doc did" cannot be
  expressed in a repo that committed everything at once. The same three commits
  carry a pair of docs that name **each other**, one of them reworked in the
  last commit: that is the ordering `--gate` used to block on and no rework
  commit could clear, and it is pinned beside the non-doc ordering that still
  blocks.
  `docker-proof.test.sh` (design #517, job #538) adds ~2.5s, and its stub answers
  the **Engine API** rather than a URL: it keeps the image and the container the
  earlier rungs created, so rung 6's re-listing is answered by what the ladder
  actually did rather than by a constant, and it reads the run's marker back out
  of the build context it was posted. It binds a real `AF_UNIX` file with
  `python3` (falling back to `perl`), because `[ -S ]` is rung 1's whole question
  and a regular file standing in for the socket would answer it wrong.
  `check-molt.test.sh` (design #533 S2, job #548) adds ~2s (measured 2026-08-10)
  and stubs nothing —
  every check it pins is a **before-and-after**, so each case needs a
  fixture with real history: a base commit holding the pre-molt corpus, a second
  commit doing the shedding, and `BASE_BRANCH` pointing at the first. The molt
  commit must also sit on its **own branch**, which is where the cost goes and
  where the first draft went wrong — on `main` the merge-base with `BASE_BRANCH`
  is HEAD itself, so the diff is empty, every deletion check goes quiet, and five
  cases passed while proving nothing.
  The total is checked **between** suites, not after the loop — otherwise the
  real ceiling would be suite-count × per-suite cap — and the failure names the
  suites it therefore never ran. The per-suite cap is applied with `timeout`,
  which is *probed* before the stage announces the cap: no working `timeout`
  fails the stage rather than silently running the suites unbounded, so the
  announcement can never claim a bound that is not in force.
- **How many cases a suite holds is not written down here — run it.** How the
  answer arrives is a property of the suite, not of the tree: many end by
  printing their own tally (`check-doc-facts.test.sh` prints `passed N, failed
  N`), while older ones only announce `all cases pass` or `ALL PASS` and so
  answer nothing at all. Either way the runtime is the cheaper oracle. Two
  reviewers deriving this suite's total by reading its invocation sites got two
  different answers on the same file, and the standing number they jointly
  condemned turned out to be exactly what the suite printed at the base it
  described — wrong only because the suite had grown since
  ([#415](../design/415-knowledge-architecture.md)'s
  [job #450 finding](../design/415-knowledge-architecture.md#finding--2026-08-05-job-450-a-case-count-two-reviewers-derived-differently));
  a total that is cheap to ask the runtime for does not belong transcribed in
  prose. Timings and budgets are different — they are measurements of a host and
  a date, so they are stated with both.
- Each suite runs with `CHUG_CI_SHELL_SUITES=0`, so the real `ci.sh` that
  `ci.test.sh` drives does not recurse into the suites again. Setting it to 0 by
  hand opts the stage out, announced.
- **The stage drives every suite as `sh "$suite"`, and one case names its shell
  anyway.** A suite that invokes its subject as `sh "$SUT"` measures the subject
  under dash here and says nothing about the shell that actually runs it in
  production — macOS `/bin/sh` is bash. `build-worker.test.sh` case 2c2c-shells
  runs the host-mode capacity shape under `sh` *and* `bash` explicitly for that
  reason: the mis-parse it pins (job #501) is one dash gets right, so under the
  stage's own shell the failing input stayed green. Name a shell only when the
  divergence is the thing under test, and say so where the case is written; the
  class itself is gated ahead of this stage by
  `.chug/tasks/check-shell-quoting.sh`, whose own suite adds 0.16s
  (measured 2026-08-08) and whose fixtures name bash for the same reason.
- **The gate's Debian container is the authoritative environment.** These suites
  assume GNU tooling and Linux path semantics, so a hand-run on macOS reds some
  of them spuriously (BSD `sed` rejecting GNU label syntax in
  `agent-rust-image.test.sh`; `/var` → `/private/var` in `pre-commit.test.sh`).
  Trust the gate over a laptop.
- They are **not** in `.githooks/pre-commit`: the hook is ~2s by design and these
  are 36.8s.

## Conventions

- `test-utils` owns: the NATS harness (shared: `CHUG_TEST_NATS_URL`, else a testcontainers-run `nats` container; private: a local `nats-server -js` process per caller, else a private container), temp-repo builder, fake backend/provider, record-fixture builders (`fixture::job` — one blank `types::Job` a test edits into its case, not a project fixture), and the skip guards: the `require_nats!`/`require_nats_config!` macros for NATS and `backend_suite::docker_available()` for Docker. There is no `e2e!` macro
- Every bug fix lands with a regression test at the lowest tier that can express it
- **Assert the invariant, not the feature's absence.** A golden that says "this
  field is off for every shipped job type" expires silently the day a type adopts
  it, and reads as passing until someone notices. Two such tests in
  `crates/types/src/job_type.rs` had been false for two epochs — `mac-proof`
  declares `runtime.mode: host` and `gcp-proof` declares `workload_identities:` —
  and neither was caught, because the `REPO_JOB_TYPES` array they judged had
  drifted to ten of twelve and those were the two missing files. The durable form
  is the pair the feature actually promises: a type that declares the field carries
  its epoch, and one that does not leaves the wire clean. Where a golden judges a
  population read from the tree, assert the population is complete too, or the
  goldens quietly judge a subset
- Coverage is tracked per crate (v1 discipline carries over); `chuggernaut_domain::state` and `release` validation are held to ~100% branch coverage — they are the correctness core

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
([#308](../design/308-gha-port.md) §G).

Two limits to read the number with. The run starts the image's `nats-server` and
exports `CHUG_TEST_NATS_URL`, so tier-2 executes — and since job 408 the
*private*-server suites are measured too, because they serve themselves from the
same binary `start_nats` puts on `PATH`. What still self-skips is only what needs
a Docker **backend**: 7 of `docker_backend.rs`'s 8 tests, 13 of
`nats_backend.rs`'s 20, and 1 of `fleet_e2e.rs`'s 2, all at their
`docker_available()` guard. So every percentage is still a lower bound, and the
script says so. And `coverage.lcov` plus
`coverage-html/` leave the container only through the run's output archive: the
script tars them to `/workspace/chug-output.tar.gz`, which the dispatcher
harvests into the task's `output.tar.gz` artifact
([#362](../design/362-binary-artifacts.md) S1, spec §3.2) — download it from
the task's artifact list. It is capped at 16 MiB and refused whole above that.
