# Testing Strategy

Three layers: unit tests for pure logic, integration tests against real NATS, and end-to-end tests against the full stack.

---

## Unit Tests

Pure logic, no infrastructure dependencies. Fast, deterministic.

### Dispatcher

- **State machine transitions:** given (current_state, trigger) → assert (new_state, side_effects). Cover every row in the state transition table, including edge cases (admin close while OnTheStack, requeue from OnIce with unresolved deps).
- **Cycle detection:** build petgraph, attempt edge additions that would create cycles, verify rejection. Test diamond deps, self-loops, transitive chains.
- **Assignment algorithm:** given a set of idle workers with capabilities and a job with requirements, verify correct selection. Cover: capability filtering, platform filtering, blacklist filtering, longest-idle-first ordering, no-match returns none.
- **Preemption selection:** given busy workers with job priorities, verify the lowest-priority worker is selected for preemption. Cover: ties, single worker, all workers higher priority than incoming job.
- **Unblock propagation:** given a dep graph and a job transitioning to Done, verify correct set of dependents are unblocked. Cover: partial deps done, all deps done, diamond deps, no dependents.
- **Message serialization/deserialization:** round-trip all message types through serde. Verify backward compatibility if fields are added.

### Worker

- **Outcome selection:** given execution results, verify correct outcome type (yield/fail/abandon).
- **Heartbeat failure counting:** verify 1-2 failures → continue, 3+ → abandon.
- **Assignment-while-busy rejection:** verify ignored with warning.

### Reviewer

- **Review state mapping:** APPROVED → approve, REQUEST_CHANGES → changes_requested, COMMENT → escalate.
- **Rework count threshold:** verify escalation when count > 3.
- **Stale review filtering:** given reviews with timestamps, verify only post-action reviews are considered.

---

## Integration Tests

Real NATS server (via testcontainers). Forgejo API interactions stubbed at the client boundary. Each test gets a fresh NATS server or isolated subject prefix.

### Job Lifecycle

- **Happy path:** create job → verify OnDeck → idle worker registers → verify assignment sent → worker heartbeats → worker yields with pr_url → verify InReview → inject ReviewDecision{Approved} → verify Done.
- **With dependencies:** create job A (no deps) → create job B (depends on A) → verify B is Blocked → complete A → verify B transitions to OnDeck.
- **Diamond deps:** A depends on B and C. Complete B → A stays Blocked. Complete C → A transitions to OnDeck.
- **On-ice flow:** create job with initial_state: on-ice → verify OnIce → admin requeue → verify OnDeck (or Blocked if deps unresolved).

### Failure and Recovery

- **Lease expiry:** create job, assign, stop heartbeats, wait for monitor scan → verify job transitions to Failed.
- **Job timeout:** create job with short timeout_secs, assign, heartbeat continuously → verify Failed after timeout.
- **Abandon and blacklist:** assign job to worker, worker abandons → verify job back to OnDeck, worker blacklisted → re-assign skips blacklisted worker.
- **Auto-retry with backoff:** fail a job with max_retries > 0, verify it stays Failed until retry_after, then transitions to OnDeck after monitor scan.
- **Retry exhaustion:** fail a job max_retries times → verify it stays Failed, no further retries.

### Restart Recovery

- Populate NATS KV with known state (jobs, claims, deps, workers).
- Start dispatcher, let it rebuild.
- Verify: in-memory job index matches KV, petgraph matches deps, stale claims are cleaned, claimless OnTheStack jobs transition to Failed, partial dep indexes are repaired.

### Preemption

- Two workers, both busy. High-priority job arrives → verify PreemptNotice sent to worker with lowest-priority job → simulate abandon + idle → verify high-priority job assigned.

### Rework Routing

- Worker yields → InReview → inject ReviewDecision{ChangesRequested} → verify job in ChangesRequested, original worker gets rework assignment with feedback.
- Same flow but original worker is busy → verify pending-rework written → simulate worker idle → verify rework assigned.
- Same flow but original worker is gone (pruned) → verify rework assigned to another capable worker.

### Merge Queue

- Two jobs approved concurrently for same repo → verify one gets lock, other queued → first merges, second processed from queue → both Done.

### Monitor Scans

- Orphan detection: create claim with unknown worker_id → verify OrphanDetectedEvent.
- Stale session: create session entry for a Done job → verify detected.
- Claimless on-the-stack: set job to OnTheStack with no claim → verify detected.

### Admin Commands

- Admin close while OnTheStack → verify claim released, preempt sent, job transitions to Done, reverse deps walked.
- Admin close --revoke → verify Revoked state, dependents stay Blocked.
- Admin requeue from Failed → verify dep check, route to OnDeck or Blocked.

---

## End-to-End Tests

Full Docker Compose stack: NATS, Forgejo, dispatcher, reviewer, runner, SimWorkers.

### Setup

- Docker Compose brings up all services.
- `chuggernaut seed` loads test fixture DAGs into the system.
- SimWorkers register with configurable delays.

### Scenarios

- **Single job:** create → SimWorker picks up → PR opened in Forgejo → review action runs → PR merged → job Done.
- **Dependency chain:** seed A → B → C. Verify jobs execute in order. C completes last.
- **Parallel fan-out:** A has no deps. B, C, D all depend on A. Complete A → verify B, C, D all transition to OnDeck and get assigned to separate workers concurrently.
- **Rework cycle:** create job with review: high. SimWorker yields a PR that the review action flags → verify ChangesRequested → SimWorker reworks → re-review → eventually approved and merged.
- **Escalation:** create job with review: human → verify PR gets human reviewer assigned, job in Escalated state.
- **Worker crash:** kill a SimWorker mid-execution → verify lease expiry → job Failed → auto-retry → new worker picks up → completes.
- **Dispatcher restart:** mid-pipeline, restart dispatcher container → verify all in-flight jobs recover correctly, workers re-register, pipeline completes.

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
