# Testing Strategy

Three layers: unit tests for pure logic, integration tests against real NATS, and end-to-end tests against the full stack.

---

## Unit Tests

Pure logic, no infrastructure dependencies. Fast, deterministic.

### Dispatcher

- **State machine transitions:** given (current_state, trigger) → assert (new_state, side_effects). Cover every row in the state transition table, including edge cases (admin close while OnTheStack, requeue from OnIce with unresolved deps).
- **Cycle detection:** build petgraph, attempt edge additions that would create cycles, verify rejection. Test diamond deps, self-loops, transitive chains.
- **Unblock propagation:** given a dep graph and a job transitioning to Done, verify correct set of dependents are unblocked. Cover: partial deps done, all deps done, diamond deps, no dependents.
- **Message serialization/deserialization:** round-trip all message types through serde. Verify backward compatibility if fields are added.

### Worker

- **Outcome selection:** given execution results, verify correct outcome type (yield/fail).
- **Heartbeat failure counting:** verify 1-2 failures → continue, 3+ → exit with error.

### Worker (Review Mode)

- **Review output parsing:** verify JSON decision parsing from subprocess output (approved, changes_requested, escalate).
- **PR index parsing:** extract PR number from URL.
- **Merge retry:** verify retry logic on merge failure.

---

## Integration Tests

Real NATS server (via testcontainers). Forgejo API interactions stubbed at the client boundary. Each test gets a fresh NATS server or isolated subject prefix.

### Job Lifecycle

- **Happy path:** create job → verify OnDeck → verify action dispatched → worker heartbeats → worker yields with pr_url → verify InReview → inject ReviewDecision{Approved} → verify Done.
- **With dependencies:** create job A (no deps) → create job B (depends on A) → verify B is Blocked → complete A → verify B transitions to OnDeck.
- **Diamond deps:** A depends on B and C. Complete B → A stays Blocked. Complete C → A transitions to OnDeck.
- **On-ice flow:** create job with initial_state: on-ice → verify OnIce → admin requeue → verify OnDeck (or Blocked if deps unresolved).

### Failure and Recovery

- **Lease expiry:** create job, dispatch action, stop heartbeats, wait for monitor scan → verify job transitions to Failed.
- **Job timeout:** create job with short timeout_secs, dispatch action, heartbeat continuously → verify Failed after timeout.
- **Auto-retry with backoff:** fail a job with max_retries > 0, verify it stays Failed until retry_after, then transitions to OnDeck and new action dispatched after monitor scan.
- **Retry exhaustion:** fail a job max_retries times → verify it stays Failed, no further retries.

### Restart Recovery

- Populate NATS KV with known state (jobs, claims, deps).
- Start dispatcher, let it rebuild.
- Verify: in-memory job index matches KV, petgraph matches deps, stale claims are cleaned, claimless OnTheStack jobs transition to Failed, partial dep indexes are repaired.

### Rework Dispatch

- Worker yields → InReview → inject ReviewDecision{ChangesRequested} → verify job in ChangesRequested, new action dispatched with review_feedback and is_rework=true.

### Review Dispatch

- Worker yields → InReview → verify review action dispatched with pr_url and review_level.

### Monitor Scans

- Orphan detection: set job to OnTheStack with no claim → verify OrphanDetectedEvent.

### Admin Commands

- Admin close while OnTheStack → verify claim released, job transitions to Done, reverse deps walked.
- Admin close --revoke → verify Revoked state, dependents stay Blocked.
- Admin requeue from Failed → verify dep check, route to OnDeck or Blocked.

---

## End-to-End Tests

Full stack: NATS, Forgejo, runner (via testcontainers). Requires pre-built `chuggernaut-runner-env` Docker image (`docker build -t chuggernaut-runner-env -f Dockerfile.runner-env .`) and Docker socket access.

Two E2E tests exist (marked `#[ignore]`, run with `cargo test -- --ignored --nocapture`):

- **`action_runner_publishes_outcome_to_nats`** — Runner isolation: Forgejo Action → runner → worker binary → NATS. Verifies heartbeat, WorkerOutcome (Yield with PR URL), channel_send message, and ChannelStatus KV.
- **`e2e_full_action_pipeline`** — Full dispatcher pipeline: create job → dispatch action → runner → mock-claude via MCP → git commit/push → PR created → Yield → InReview.

### Setup

- testcontainers starts Forgejo + NATS; `start_runner` launches Forgejo runner container.
- Action workers run inside Forgejo Action containers (using `mock-claude` for testing).
- `chuggernaut seed` loads test fixture DAGs for manual/future tests.

### Scenarios

- **Single job:** create → work action dispatched → mock-claude runs → PR opened in Forgejo → review action dispatched → review mock-claude → PR merged → job Done.
- **Dependency chain:** seed A → B → C. Verify jobs execute in order. C completes last.
- **Parallel fan-out:** A has no deps. B, C, D all depend on A. Complete A → verify B, C, D all transition to OnDeck and get dispatched concurrently.
- **Rework cycle:** create job with review: high. Worker yields a PR that the review action flags → verify ChangesRequested → new action dispatched with feedback → re-review → eventually approved and merged.
- **Escalation:** create job with review: human → verify PR gets human reviewer assigned, job in Escalated state.
- **Action crash:** kill action container mid-execution → verify lease expiry → job Failed → auto-retry → new action dispatched → completes.
- **Dispatcher restart:** mid-pipeline, restart dispatcher container → verify all in-flight jobs recover correctly, pipeline completes.

### Assertions

- Job states verified via `chuggernaut show {key}` or dispatcher HTTP API.
- PR existence and merge status verified via Forgejo API.
- Dependency resolution verified by observing downstream jobs unblock.
- Timing assertions use generous margins (not flaky wall-clock checks).

---

## Test Infrastructure

### NATS Isolation

Integration tests use one of:
- **testcontainers:** ephemeral NATS server per test suite, torn down after.
- **Subject prefix:** shared NATS server with a unique prefix per test (e.g., `test_{uuid}.chuggernaut.*`), allowing parallel test execution.

### Container Lifecycle

Test containers are shared per process via `OnceLock` + `Box::leak` (Rust statics are never dropped). Two mechanisms ensure cleanup:

- **`watchdog` feature** (testcontainers): catches SIGTERM/SIGINT/SIGQUIT signals and removes all registered containers. Handles Ctrl-C during test runs.
- **`register_container_cleanup()`** (`chuggernaut-test-utils`): registers container IDs for removal via `libc::atexit`. Handles normal process exit.

All test suites call `register_container_cleanup(container.id())` before leaking the container handle.

### MCP Bridge Tests

The `chuggernaut-channel` integration tests include two subprocess bridge tests that exercise the full MCP-over-NATS path without Docker runners or Forgejo:

- **`mcp_bridge_subprocess_to_nats`**: spawns a bash MCP client, bridges stdin/stdout through `handle_message`, verifies `channel_send` → NATS outbox and `update_status` → KV.
- **`mcp_bridge_bidirectional_nats`**: round-trip test — publishes a message to the NATS inbox, mock client calls `channel_check` to receive it, replies via `channel_send`, verified on NATS outbox.

These use inline bash scripts as MCP clients (no git operations, no Forgejo) and run in ~0.4s.

### Forgejo Stub

A lightweight HTTP server that implements the subset of Forgejo API used by the system:
- `POST /api/v1/repos/{owner}/{repo}/pulls` — record PR creation
- `GET /api/v1/repos/{owner}/{repo}/pulls/{id}/reviews` — return configured review state
- `POST /api/v1/repos/{owner}/{repo}/pulls/{id}/merge` — record merge, return success/failure
- `POST /api/v1/repos/{owner}/{repo}/actions/workflows/{file}/dispatches` — record dispatch

The stub is configurable per test: inject specific review decisions, merge failures, or API errors to exercise failure paths.

### Fixtures

`chuggernaut seed` accepts JSON fixture files describing job DAGs:

```json
{
  "jobs": [
    { "title": "Setup database schema", "deps": [] },
    { "title": "Add user model", "deps": [1] },
    { "title": "Add auth endpoints", "deps": [1, 2] }
  ]
}
```

Used by both integration and end-to-end tests for repeatable scenarios.
