# chuggernaut Dev Journal

## 2026-03-23 — Phase 1 Implementation Complete

### What was built
- **5 Rust crates** in a workspace (now 9), all compiling on latest deps (async-nats 0.46, petgraph 0.8, reqwest 0.13, axum 0.8)
- **chuggernaut-types**: 45→~30 types with `JsonSchema` derive, NATS subject/bucket/stream constants, helpers. 12 unit tests.
- **chuggernaut-forgejo-api**: Thin reqwest client covering PRs, reviews, merge, actions, repos (~10 endpoints).
- **chuggernaut-dispatcher**: Full coordinator — NATS handlers (9 subscriptions), HTTP API (10 endpoints + SSE + `/schema`), monitor (4 scans), petgraph DAG, CAS claim lifecycle, action dispatch, restart recovery (8-step). 12 unit tests.
- **chuggernaut-worker**: Action worker binary with git helpers (clone/branch/commit/push), MCP channel bridge. SimWorker retained as test-only infrastructure.
- **chuggernaut-cli**: All-in-one `chuggernaut` binary — create, jobs, show, requeue, close, channel send/watch/status, seed.
- **Infra**: Docker Compose (NATS + Forgejo), init script, sample fixture.

### Design consistency check
The implementation covers ~70% of the full design. Verified consistent at Phase 1 completion:
- All job states and state transition table ✓
- All KV buckets and JetStream streams ✓
- All NATS subjects and message types ✓
- All dispatcher config env vars ✓
- All worker config env vars ✓
- All HTTP API endpoints from design ✓
- Dependency management with cycle detection and unblock propagation ✓
- Claim lifecycle with CAS, heartbeat, lease expiry ✓
- Monitor scan types ✓
- Restart recovery ✓

*Note: Worker assignment/preemption were part of Phase 1 but removed in Phase 2 (action-only simplification).*

### Intentionally deferred to Phase 2
- **Reviewer process** (entire crate) — merge queue, rework counts, review action dispatch, escalation polling
- **ActionWorker** — Forgejo Actions dispatch + polling (now the only worker type)
- **Graph viewer SPA** — frontend
- **CLI commands**: action, agent

### Known simplifications in Phase 1
- SSE uses 1s polling instead of KV watchers (noted in code)
- Forgejo API client has all method signatures but only SimWorker exercises create PR and find-by-head
- ~~No integration tests yet (unit tests only)~~ — resolved, see 2026-03-24 entry

---

## 2026-03-24 — Integration Tests & Phase 2 Roadmap

### What changed
- **15 dispatcher integration tests** added using testcontainers with real NATS JetStream
  - Job creation (4): on-deck, on-ice, sequential keys, dot-rejection
  - Dependencies (2): blocked-then-unblocked, diamond partial unblock
  - Worker protocol (3): yield→in-review, fail→retry, heartbeat renewal
  - Admin commands (2): close-unblocks-dependents, revoke-keeps-dependents-blocked
  - Action dispatch (2): dispatch creates claim + transitions, rework dispatch with feedback
  - Recovery (1): rebuild state from KV
- Test infra: shared NATS container via `OnceLock`, UUID-prefixed buckets for parallel isolation

*Note: Test counts below reflect the state at time of writing; final counts after Phase 2 simplification are in the latest entry.*

**Reviewer crate (`chuggernaut-reviewer`)** created with full skeleton:
- `config.rs` — all 13 env vars from spec (delay, workflow, poll intervals, rework limit, merge lock TTL)
- `error.rs` — ReviewerError with Nats/Kv/Forgejo/Dispatcher/Serde variants
- `state.rs` — ReviewerState with DashSet dedup, Forgejo client, KV handles
- `nats_setup.rs` — initializes reviewer-owned KV buckets (merge_queue, rework_counts)
- `dispatcher_client.rs` — HTTP client for `GET /jobs/{key}` and `GET /jobs?state=`
- `decision.rs` — maps Forgejo review states to ReviewOutcome, stale review filtering
- `merge.rs` — merge lock (CAS create), queue (FIFO with CAS updates), PR merge (rebase), URL parsing
- `review.rs` — full pipeline: delay → fetch job → dispatch action → poll → decide → merge/rework/escalate
- `trigger.rs` — CHUGGERNAUT-TRANSITIONS durable pull consumer, InReview filter, dedup, reconciliation
- `main.rs` — entry point with tracing, NATS connect, KV init, reconcile, trigger loop
- 13 unit tests (decision mapping, stale filtering, rework threshold, PR URL parsing)
- Escalation polling loop implemented: polls Forgejo PR for human APPROVED/REQUEST_CHANGES after escalation, handles already-merged and closed PRs
- `ChangesOutcome` enum breaks async recursion between `poll_escalation` ↔ `handle_changes_requested`

**3 reviewer integration tests** with real NATS (testcontainers) + real Forgejo 14 (testcontainers):
- `review_approved_merges_and_completes` — submit APPROVED review → reviewer merges PR → job Done
- `review_changes_requested_routes_rework` — submit REQUEST_CHANGES → job ChangesRequested
- `human_review_level_escalates_then_human_approves` — Human review level → escalate → simulate human approval → merge → Done
- Test infra: shared NATS + Forgejo containers (OnceLock), admin + reviewer users (separate to avoid "approve own PR"), NATS bridge for namespace forwarding, real Forgejo API calls (create branches, files, PRs, reviews, merges)

**Updated total: 69 tests** (21 dispatcher unit + 20 dispatcher integration + 13 reviewer unit + 3 reviewer integration + 12 types unit) — all green

### Remaining coverage gaps vs `docs/testing.md`
Not yet covered:
- Job timeout (monitor scan — similar to lease expiry but `claimed_at + timeout`)
- Orphan detection (claim with unknown worker, claimless on-the-stack)
- Auto-retry with backoff (scan_retry picks up Failed jobs past retry_after)
- Retry exhaustion (retry_count >= max_retries stays Failed)
- Full E2E: create → SimWorker → PR → review action → merge → Done (requires Forgejo Actions runner)

---

## Next: Phase 2 Priorities

### ~~1. Expand dispatcher integration tests~~ ✅ Done
All 5 key tests added: ReviewDecision (approved + changes_requested), preemption, monitor lease expiry, admin requeue.

### ~~2. Reviewer crate skeleton~~ ✅ Done
Created `crates/reviewer/` with 10 modules. Full review pipeline + escalation polling loop implemented.
3 integration tests with real Forgejo 14 (testcontainers): approved→merge, changes_requested→rework, human escalation→poll→merge.

### ~~3. Merge queue + Forgejo review dispatch~~ ✅ Done
- `forgejo_retry` module: 3-attempt exponential backoff (2s/4s/8s) for all Forgejo API calls
- Already-merged PR detection in `handle_approved` (crash recovery)
- Stale merge lock cleanup in reconciliation (checks job terminal state)
- Action dispatch failures propagated as hard errors (not silently escalated)
- Integration test: two approved jobs merge sequentially through the queue
- Upgraded to Forgejo 14, noop `review-work.yml` workflow in test repos
- **Updated total: 73 tests** (21 dispatcher unit + 20 dispatcher integration + 16 reviewer unit + 4 reviewer integration + 12 types unit) — all green

### ~~4. Rework routing + escalation~~ ✅ Done (implemented as part of priorities 2-3)
Rework count tracking (`chuggernaut.rework-counts` KV), escalation threshold (count > rework_limit → human),
escalation polling loop, `ChangesOutcome` enum for recursion safety — all in `review.rs`.

### 5. E2E test harness
- Docker Compose test profile (dispatcher + reviewer + NATS + Forgejo)
- Single-job E2E: create → SimWorker → PR → review → merge → Done
- Dependency chain E2E, worker crash recovery

### ~~6. Typed NATS subject registry~~ ✅ Done
- **`chuggernaut-nats` crate** — shared typed NatsClient extracted from dispatcher, used by all crates
  - `publish_msg(&Subject<T>, &T)` — compile-time type-safe publish (wrong payload = compile error)
  - `request_msg(&RequestSubject<Req, Resp>, &Req)` — typed request-reply
  - `publish_to(&SubjectFn<T>, param, &T)` — typed dynamic subjects
  - `subscribe_subject`, `subscribe_request`, `subscribe_dynamic` — typed subscribe
  - Raw methods preserved for wildcards and NATS reply subjects
- **`subject_registry!` macro** in `chuggernaut-types` — single source of truth for all NATS subjects:
  - Defines typed constants (Subject, RequestSubject, SubjectFn) with payload types
  - Generates `subjects::registry() -> Vec<SubjectSchema>` for schema catalog
  - Impossible to add a subject without specifying its payload type
- **Full migration** — all publish/subscribe/request calls across dispatcher, worker, CLI, reviewer, and channel use typed API. Manual `serde_json::to_vec` + `Bytes::from` boilerplate eliminated.
- Deleted old `crates/dispatcher/src/nats_client.rs`
- **8 crates** in workspace, **8,528 LOC**, **102 tests** — all green

### ~~7. ActionWorker E2E — MCP channel over NATS~~ ✅ Done
The action runner E2E test (`action_runner_publishes_outcome_to_nats`) was passing for heartbeat and outcome but silently skipping the MCP channel message. Two issues found and fixed:

**Issue 1: Poll loop race condition** — The test's NATS polling loop broke immediately when the outcome arrived (`got_outcome = true; break;`) before checking the channel subscription in the same iteration. Since mock-claude publishes `channel_send` and the worker reports outcome nearly simultaneously, the channel message was consistently missed.
- **Fix:** Deferred the `break` until after all subscriptions are checked, plus a 2s post-loop drain window for any channel messages in flight.
- Promoted channel message assertion from soft eprintln to hard `assert!`.

**Issue 2: No isolated MCP bridge test** — The only test path for MCP-over-NATS was through the full Forgejo Action pipeline (Docker runner + container + git clone). No way to isolate MCP issues from infrastructure issues.
- **Fix:** Added two focused bridge tests to `crates/channel/tests/integration.rs`:
  - `mcp_bridge_subprocess_to_nats` — spawns a bash MCP client subprocess, bridges stdin/stdout through `handle_message`, verifies `channel_send` → NATS outbox and `update_status` → KV.
  - `mcp_bridge_bidirectional_nats` — round-trip: external NATS inbox message → `nats_inbox_listener` → `ChannelState` → mock client calls `channel_check` → sees the message → replies via `channel_send` → verified on NATS outbox.
  - Both run in ~0.4s with just a NATS testcontainer (no Docker runner, no Forgejo).

**Networking diagnosis added** — `action_runner_publishes_outcome_to_nats` workflow now includes a "Verify networking" step that checks NATS and Forgejo reachability from inside the action container before running the worker. Fails fast with a clear error if `host.docker.internal` can't reach the services.

### ~~8. Test container cleanup~~ ✅ Done
NATS testcontainers were accumulating across test runs because `Box::leak` prevents `Drop` from firing, and Rust statics are never dropped on process exit.

- **`watchdog` feature** enabled on testcontainers — catches SIGTERM/SIGINT/SIGQUIT and removes all registered containers (handles Ctrl-C during test runs).
- **`register_container_cleanup()`** in `chuggernaut-test-utils` — registers container IDs for removal via `libc::atexit` hook on normal process exit.
- Both channel and dispatcher test suites call `register_container_cleanup(container.id())` before `Box::leak`.
- Runner config (`infra/runner/config.yaml`) updated with `--add-host=host.docker.internal:host-gateway` for job container networking.

**9 crates** in workspace, **~12,000 LOC**, **105 tests** (21 dispatcher unit + 21 dispatcher integration + 19 channel integration + 16 reviewer unit + 4 reviewer integration + 12 types unit + 2 ignored E2E) — all green.

### 9. Action-only worker simplification ✅ Done
Major refactor to remove the worker registration/idle/assignment/preemption protocol entirely. All jobs are now dispatched directly as Forgejo Actions.

**Removed from types crate:**
- 14 types/enums: `WorkerInfo`, `WorkerState`, `SessionInfo`, `PendingRework`, `WorkerRegistration`, `Assignment`, `HelpRequest`, `HelpResponse`, `PreemptNotice`, `IdleEvent`, `UnregisterEvent`, `WorkerListResponse`, `JobState::NeedsHelp`, `OutcomeType::Abandon`
- 10 NATS subjects: worker.register/idle/unregister, dispatch.assign/preempt, interact.help/respond/deliver/attach/detach
- 4 KV buckets: workers, sessions, pending-reworks, abandon-blacklist
- 1 JetStream stream: CHUGGERNAUT-WORKER-EVENTS
- `Job.last_worker_id` field

**Removed from dispatcher:**
- Deleted `workers.rs` entirely (register, idle, unregister, prune)
- Gutted `assignment.rs` from ~420 lines to ~47 lines (just calls `dispatch_action`)
- Removed 5 NATS handlers (register, idle, unregister, help, respond) — handler count 14→9
- Removed preemption logic, abandon blacklisting, pending-rework routing
- Simplified monitor (1 orphan condition instead of 3), recovery (8 steps instead of 10)
- Removed `/workers` HTTP endpoint, workers DashMap from state, `blacklist_ttl_secs` config

**Updated:**
- `action_dispatch.rs` — added `review_feedback`/`is_rework` parameters, passed as workflow inputs
- `handlers.rs` — ChangesRequested now dispatches new action with review feedback (no "route to last worker")
- Worker binary — added `review_feedback`/`is_rework` env vars, removed preemption monitoring, `Abandon`→`Fail`
- CLI — removed `Interact` command
- All docs updated: DESIGN.md, worker-protocol.md (complete rewrite), nats-schema.md, dispatcher.md, monitor.md, reviewer.md, testing.md

**Job states:** 11→10 (removed NeedsHelp). **Outcome types:** 3→2 (removed Abandon). **KV buckets:** 13→9. **NATS subjects:** 30+→~15.

**9 crates**, **~10,000 LOC**, **92 tests** (12 dispatcher unit + 17 dispatcher integration + 19 channel integration + 16 reviewer unit + 4 reviewer integration + 12 types unit + 2 ignored E2E) — all green.

### 10. Rework workflow fix + docker-compose + E2E prep ✅ Done
- **Fixed `action/work.yml`** — added `review_feedback` and `is_rework` inputs + env vars. Rework dispatch was silently 422'ing at Forgejo API because inputs weren't declared.
- **Completed `docker-compose.yml`** — added dispatcher, reviewer, runner, runner-env services. Runner uses official Forgejo runner:11 with register+daemon pattern.
- **Created `Dockerfile`** — multi-stage build for dispatcher + reviewer binaries.
- **Updated `scripts/init.sh`** — creates reviewer user + token, runner registration token, writes `.env` file.
- **Added rework dispatch integration test** (`rework_dispatches_new_action_with_feedback`) — verifies full rework path with real Forgejo: create → dispatch → yield → InReview → ChangesRequested → new action with feedback → OnTheStack.
- **Updated E2E test workflows** — both `action_runner_publishes_outcome_to_nats` and `e2e_full_action_pipeline` now include `review_feedback`/`is_rework` inputs + env vars. Cleaned up duplicate workflow push in e2e test.

**93 tests** (12 dispatcher unit + 18 dispatcher integration + 19 channel integration + 16 reviewer unit + 4 reviewer integration + 12 types unit + 2 ignored E2E) — all green.

### 11. E2E tests passing ✅ Done
Both E2E tests pass with the pre-built `chuggernaut-runner-env` Docker image:

- **`action_runner_publishes_outcome_to_nats`** (24s) — Runner isolation test: Forgejo Action → runner → worker binary → NATS. Verifies heartbeat, WorkerOutcome (Yield with PR URL), channel_send message on NATS outbox, and ChannelStatus in KV.
- **`e2e_full_action_pipeline`** (27s) — Full dispatcher pipeline: create job → dispatch action → runner executes → mock-claude via MCP → git commit/push → PR created → Yield → dispatcher transitions to InReview.

Both remain `#[ignore]` — they require `docker build -t chuggernaut-runner-env -f Dockerfile.runner-env .` and Docker socket access. Run with: `cargo test -p chuggernaut-dispatcher --test integration -- --ignored --nocapture`

**93 tests** (all green) + **2 E2E tests** (passing when runner-env image is built).

### 12. Token usage tracking + reviewer elimination ✅ Done

Major architectural simplification: eliminated the standalone reviewer process. Review is now handled by the same worker binary in review mode, dispatched as a Forgejo Action.

**Token usage tracking:**
- `TokenUsage` struct: `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens` (with `AddAssign`)
- `ActionTokenRecord` struct: `action_type` ("work"/"review"), `token_usage`, `completed_at`
- `Job.token_usage: Vec<ActionTokenRecord>` — each action invocation appends a record
- `WorkerOutcome.token_usage` and `ReviewDecision.token_usage` — optional, reported by actions
- Dispatcher accumulates via `append_token_record()` in both `process_outcome` and `process_review_decision`
- Per-action records allow the UI to show work vs review tokens separately, including across rework cycles

**Reviewer → review action:**
- Dispatcher dispatches `review.yml` workflow when a job transitions to InReview
- `dispatch_review_action()` added to `action_dispatch.rs` with inputs: `job_key`, `nats_url`, `pr_url`, `review_level`
- Worker binary (`chuggernaut-worker`) now has `--mode work|review`:
  - Review mode: clones repo, checks out PR branch, gets diff, runs Claude, parses JSON decision, posts PR review to Forgejo, attempts merge with retries if approved, publishes `ReviewDecision`
- `action/review.yml` created (parallel to `action/work.yml`)
- Merge is attempted inline by the review action with 3 retries at 5s/10s/15s intervals (no separate merge queue process)
- On review failure or subprocess error: escalates to human reviewer

**Dispatcher config additions:**
- `CHUGGERNAUT_REVIEW_WORKFLOW` (default `review.yml`)
- `CHUGGERNAUT_REVIEW_RUNNER_LABEL` (default `ubuntu-latest`)
- `CHUGGERNAUT_REWORK_LIMIT` (default 3)
- `CHUGGERNAUT_HUMAN_LOGIN` (default `you`)

**Removed:**
- `crates/reviewer/` — entire crate (10 modules, 16 unit tests, 4 integration tests)
- Reviewer service from `docker-compose.yml` and `Dockerfile`
- No longer need: `CHUGGERNAUT-TRANSITIONS` durable consumer for reviewer, merge queue KV lock protocol, separate reviewer Forgejo account
- Reviewer-specific config env vars

**Additional cleanups in this pass:**
- Removed `worker_type` field from `Job`, `CreateJobRequest`, and all tests/CLI/docs (was always `None` or `"action"` — vestigial from when multiple worker types existed)
- Added `nats_worker_url` config (`CHUGGERNAUT_NATS_WORKER_URL`) — separate NATS URL for action workers in Docker (`host.docker.internal`). E2E tests now use automatic dispatcher dispatch instead of manual workarounds.
- Static SPA serving: removed `include_str!`, always reads from disk. `CHUGGERNAUT_STATIC_DIR` env var + fail-fast `check_static_dir()` at startup. Docker Compose mounts `./static:/static:ro`.
- Both Dockerfiles use `cargo-chef` for cached dependency builds.
- Added `mock-review.sh` to runner-env image for review action E2E testing.
- 3 new integration tests: `yield_dispatches_review_action` (Forgejo-backed), `token_usage_accumulated_from_work_and_review`, `token_usage_across_rework_cycle`
- New E2E test: `e2e_full_review_cycle` — work → review (changes_requested) → rework → review (approved) → Done, with token usage verification

**8 crates** in workspace, all green.

### 13. API manifest + self-describing HTTP API + fixture templating ✅ Done

**`GET /api.json` — self-describing API manifest:**
- Single JSON document containing the complete route table, SSE event definitions, and full JSON Schema for all types
- Route metadata defined once in `api_specs()` (`http.rs`) — the authoritative source for both the manifest response and test assertions
- Type names in route specs use `schemars::JsonSchema::schema_name()` — renaming a type in the types crate automatically updates the manifest (compile-time guarantee)
- SSE event schemas document all four event types: `snapshot`, `job_update`, `claim_update`, `channel_update` with their payload shapes
- `GET /schema` retained for raw JSON Schema access

**Two tests enforce consistency:**
- `api_specs_match_router` — asserts every (method, path) pair in `api_specs()` has a corresponding route in the axum router and vice versa. Catches routes added/removed without updating the manifest.
- `api_spec_types_exist_in_schema` — asserts every type name referenced in route specs (request/response) exists in the generated schema `$defs`. Catches stale type references.

**CLI `seed --var`:**
- `chuggernaut seed {repo} {fixture.json} --var KEY=VALUE,...` — interpolates `{{KEY}}` placeholders in the fixture JSON before parsing
- Enables fixtures to reference environment-specific values (e.g. `--var DISPATCHER_URL=https://dispatcher.example.com`)

**Frontend fixture (`fixtures/frontend-wasm.json`):**
- 11-job DAG describing a standalone Rust+WASM frontend (Leptos) that is fully decoupled from the chuggernaut workspace
- Task 2 fetches `{{DISPATCHER_URL}}/api.json` in `build.rs` and generates typed Rust structs + API client from the schema — no compile-time dependency on chuggernaut crates
- Graph: scaffold → codegen → SSE client → store → 3 parallel UI components → detail view → routing → styling → integration test

### 14. Comprehensive test coverage + cargo-llvm-cov ✅ Done

Added 36 new integration tests covering all previously-untested dispatcher code paths, plus code coverage tooling.

**Coverage tooling:**
- `scripts/coverage.sh` — wraps `cargo-llvm-cov` with multi-run support
  - `bash scripts/coverage.sh` — fast: non-ignored tests only
  - `bash scripts/coverage.sh --full` — all tests including E2E (sequential)
  - `bash scripts/coverage.sh --lcov` — LCOV output for CI
- Uses `--no-report` + `report` pattern to merge coverage from parallel + sequential runs

**New integration tests (36 tests, 7 groups):**

*State machine edges (6):* escalation flow (3 tests), Failed→OnIce requeue, OnIce→OnDeck thaw, invalid transition rejection.

*Error paths (4):* dispatch failure releases claim + fails job, dependency cycle detection, wrong-worker heartbeat ignored, rework_limit not enforced (documents current behavior).

*Token usage + concurrency (3):* nil token_usage not appended, 10 concurrent heartbeats benign, duplicate outcome idempotent.

*Recovery (3):* stale claim cleanup, claimless OnTheStack→Failed, reverse dep index repair.

*Monitor (4):* job timeout (distinct from lease expiry), orphan detection event, retry-eligible→OnDeck, archival removes Done job from memory.

*HTTP API (12):* list jobs, filter by state, job detail, 404, create, requeue, close, journal, deps, channel send, channel 404, SSE snapshot.

*Assignment & capacity (4):* capacity limit blocks assignment, priority ordering, dispatch-next-after-yield, ChangesRequested competes in dispatch queue. (All 4 use real Forgejo containers.)

**Test helpers added:**
- `setup_with_config(|c| ...)` — setup with Config overrides
- `start_http(state)` — binds axum router on random port for HTTP tests

**Code change:** Added `(Failed, OnIce)` to `validate_transition` in `jobs.rs` so admins can put failed jobs on ice.

**E2E fix:** `e2e_full_review_cycle` had a brittle 1.5s sleep that raced with the review decision handler. Replaced with proper polling that waits for the job to leave InReview before checking rework dispatch.

**Coverage results (full run, 60 tests):**

| File | Line Coverage |
|------|-------------|
| nats_init.rs | 100% |
| action_dispatch.rs | 95% |
| recovery.rs | 93% |
| claims.rs | 90% |
| jobs.rs | 90% |
| assignment.rs | 88% |
| monitor.rs | 84% |
| state.rs | 79% |
| handlers.rs | 77% |
| deps.rs | 75% |
| http.rs | 44% |

### 15. Model/flag passthrough for workers (PR #4) ✅ Done

Worker command args are now configurable per job. `CHUGGERNAUT_COMMAND_ARGS` supports passing additional flags to the Claude subprocess (e.g., model selection, think mode). Validation rejects dangerous flags before spawning.

### 16. Token governance: budget warnings + rate-limit scheduling (PR #5) ✅ Done

- **Per-graph token budget** (`CHUGGERNAUT_TOKEN_BUDGET`): optional cap. When total usage across all jobs exceeds budget, dispatcher logs overage warnings on every outcome.
- **Per-job overage detection**: fires regardless of whether a per-graph budget is set.
- **Rate-limit-aware scheduling**: `TokenTracker` tracks token consumption rate. When the rate exceeds a configurable threshold, dispatch is deferred to avoid hitting API rate limits.
- `TokenTracker` struct with sliding-window rate calculation.

### 17. Handle partial work + CI failures (PR #7) ✅ Done

Workers now yield partial work as PRs even when they can't fully complete the task. The review action evaluates the partial PR and either approves, requests changes, or escalates. CI failure detection added — if the Forgejo Action fails (container crash, OOM, etc.), the dispatcher detects it via the action status API and transitions the job to Failed with the CI failure reason.

### Remaining Phase 2 work
- **Token capture from Claude** — worker currently reports `token_usage: None`; need to parse Claude Code's output for actual usage
- **Human escalation polling** — currently just transitions to Escalated; needs a monitor task to poll Forgejo for human action
- **Dispatcher trait abstraction** — dispatch strategy behind traits for swappability
- **Graph viewer SPA** — frontend (fixture ready at `fixtures/frontend-wasm.json`)
- **Terraform** — Forgejo provisioning for test (`terraform/test/`) and deploy (`terraform/deploy/`)
- **GitHub compatibility** (issue #8) — support GitHub as an alternative git provider to Forgejo
- **Coverage gaps** — `http.rs` SSE streaming logic (44%), `deps.rs` edge cases (75%), `handlers.rs` error branches (77%)
