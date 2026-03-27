# Testing Strategy

Three layers: unit tests for pure logic, integration tests against real NATS, and end-to-end tests against the full stack.

## Running Tests

```bash
# Non-ignored integration tests (parallel, ~14s)
cargo test -p chuggernaut-dispatcher --test integration

# E2E tests (sequential, requires Docker + chuggernaut-runner-env image, ~80s)
cargo test -p chuggernaut-dispatcher --test integration -- --ignored --test-threads=1 --nocapture

# All tests including E2E
cargo test -p chuggernaut-dispatcher --test integration -- --include-ignored --test-threads=1

# Code coverage (requires cargo-llvm-cov)
bash scripts/coverage.sh          # HTML report → target/coverage/html/index.html
bash scripts/coverage.sh --lcov   # LCOV for CI → target/coverage/lcov.info
```

### Coverage Setup (one-time)

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov
```

---

## Integration Tests

Real NATS server via testcontainers. Each test gets a UUID-namespaced `DispatcherState` so tests run in parallel without interference. Tests that need a git provider spin up real Forgejo containers via `chuggernaut-test-utils`.

### Job Creation (4 tests)

- `create_job_on_deck` — new job with no deps → OnDeck
- `create_job_on_ice` — new job with `initial_state: on-ice` → OnIce
- `create_job_sequential_keys` — keys are `owner.repo.1`, `.2`, `.3`
- `create_job_rejects_dots_in_name` — dots in owner name → error

### Dependencies (2 tests)

- `deps_blocked_then_unblocked` — B depends on A; complete A → B unblocks
- `diamond_deps_partial_unblock` — C depends on A+B; complete A → C stays Blocked; complete B → C unblocks

### Worker Outcome Processing (2 tests)

- `worker_yield_to_in_review` — Yield outcome → InReview with PR URL stored
- `worker_fail_schedules_retry` — Fail outcome → Failed with retry_count incremented and retry_after set

### Admin Commands (3 tests)

- `admin_close_unblocks_dependents` — close A → Done, dependent B unblocks
- `admin_revoke_keeps_dependents_blocked` — revoke A → Revoked, dependent B stays Blocked
- `admin_requeue_from_failed` — Failed → OnDeck via admin requeue

### Heartbeat (1 test)

- `heartbeat_renews_lease` — heartbeat extends claim lease_deadline

### Recovery (4 tests)

- `recovery_rebuilds_state` — clear in-memory state, recover from KV, verify jobs + dep graph intact
- `recovery_cleans_stale_claims` — claim exists for non-OnTheStack job → recovery deletes it
- `recovery_fails_claimless_on_the_stack` — OnTheStack job with no claim → recovery transitions to Failed
- `recovery_repairs_reverse_deps` — corrupted reverse dep index → recovery repairs it

### Review Decisions (2 tests)

- `review_approved_completes_job_and_unblocks_deps` — Approved → Done, dependent unblocks
- `review_changes_requested_transitions_job` — ChangesRequested → ChangesRequested state

### Monitor (5 tests)

- `monitor_lease_expiry_fails_job` — 1s lease, no heartbeats → monitor detects → Failed
- `monitor_job_timeout` — 1s timeout_secs, long lease → monitor detects elapsed time → Failed
- `monitor_orphan_detection` — OnTheStack job with no claim → monitor publishes orphan event
- `monitor_retry_eligible_transitions_to_on_deck` — Failed job with retry_after in past → monitor transitions to OnDeck
- `monitor_archival_removes_done_job` — Done job past retention → monitor removes from memory

### State Machine Edges (6 tests)

- `escalated_review_transitions_job` — ReviewDecision::Escalated → Escalated state
- `escalated_then_approved_via_close` — Escalated → Done via admin close
- `escalated_then_changes_requested` — Escalated → ChangesRequested (valid transition)
- `requeue_from_failed_to_on_ice` — Failed → OnIce via admin requeue
- `thaw_from_on_ice_via_requeue` — OnIce → OnDeck via admin requeue
- `invalid_transition_rejected` — Done → OnTheStack → error

### Error Paths (4 tests)

- `action_dispatch_failure_releases_claim` — bad git provider URL → claim released, job Failed
- `dependency_cycle_rejected` — adding A→B when B→A exists → CycleDetected error
- `heartbeat_from_wrong_worker_ignored` — heartbeat with mismatched worker_id → claim unchanged
- `rework_limit_not_enforced_yet` — documents that `rework_limit` config exists but isn't enforced

### Token Usage & Concurrency (5 tests)

- `token_usage_accumulated_from_work_and_review` — work + review tokens stored as separate records
- `token_usage_across_rework_cycle` — 4 records across work→review→rework→review cycle
- `nil_token_usage_not_appended` — Yield with `token_usage: None` → no record added
- `concurrent_heartbeats_benign` — 10 simultaneous heartbeats → claim stays valid
- `duplicate_outcome_handled` — same WorkerOutcome published twice → no crash

### Action Dispatch with Git Provider (3 tests)

These start real Forgejo containers (testcontainers) and push workflow files:

- `action_dispatch_creates_claim_and_transitions` — OnDeck → try_assign_job → OnTheStack with claim
- `rework_dispatches_new_action_with_feedback` — full cycle: dispatch → yield → ChangesRequested → rework dispatched with feedback
- `yield_dispatches_review_action` — worker yield triggers review action dispatch, verified via journal entry

### Assignment & Capacity (4 tests)

- `capacity_limit_prevents_assignment` — `max_concurrent_actions=1`, slot full → `try_assign_job` returns false
- `dispatch_next_respects_priority` — 3 OnDeck jobs with different priorities → highest dispatched first (requires git provider)
- `dispatch_next_after_yield` — yield frees slot → next OnDeck job auto-dispatched (requires git provider)
- `changes_requested_in_dispatch_queue` — ChangesRequested job competes with OnDeck jobs by priority (requires git provider)

### HTTP API (12 tests)

- `http_list_jobs` — GET /jobs → correct count
- `http_list_jobs_filter_by_state` — GET /jobs?state=on-deck → filtered results (kebab-case state names)
- `http_get_job_detail` — GET /jobs/{key} → job + claim + activities
- `http_get_job_not_found` — GET /jobs/nonexistent → 404
- `http_create_job` — POST /jobs → 201 with key
- `http_requeue_job` — POST /jobs/{key}/requeue → state change
- `http_close_job` — POST /jobs/{key}/close → Done
- `http_get_journal` — GET /journal → entries present
- `http_get_job_deps` — GET /jobs/{key}/deps → dependency list
- `http_channel_send` — POST /jobs/{key}/channel/send → 200
- `http_channel_send_missing_job` — POST /nonexistent/channel/send → 404
- `http_sse_snapshot` — GET /events → first SSE event is "snapshot" with jobs array

---

## End-to-End Tests (3 tests, `#[ignore]`)

Full stack: NATS + Forgejo + action runner (via testcontainers). Requires pre-built `chuggernaut-runner-env` Docker image and Docker socket access.

```bash
# Build runner-env image first
docker build -t chuggernaut-runner-env -f Dockerfile.runner-env .

# Run E2E tests (must use --test-threads=1)
cargo test -p chuggernaut-dispatcher --test integration -- --ignored --test-threads=1 --nocapture
```

- **`action_runner_publishes_outcome_to_nats`** — Runner isolation: CI action → runner → worker binary → NATS. Verifies heartbeat, WorkerOutcome (Yield with PR URL), channel_send message, and ChannelStatus KV.
- **`e2e_full_action_pipeline`** — Full dispatcher pipeline: create job → dispatch action → runner → mock-claude via MCP → git commit/push → PR created → Yield → InReview.
- **`e2e_full_review_cycle`** — Complete lifecycle: create → work action → InReview → ChangesRequested → rework action → InReview → Approved → Done. Verifies token usage records are accumulated across the rework cycle.

---

## Test Infrastructure

### NATS Isolation

A single NATS JetStream container is started once per test process via `OnceLock`, on a dedicated OS thread to avoid tokio runtime conflicts. It's `Box::leak`-ed so it lives for the entire test run.

Each test gets its own UUID-namespaced `DispatcherState` — all KV buckets and NATS subjects are prefixed so tests run in parallel without interference.

### Container Lifecycle

Two mechanisms ensure cleanup:

- **`watchdog` feature** (testcontainers): catches SIGTERM/SIGINT/SIGQUIT signals and removes all registered containers.
- **`register_container_cleanup()`** (`chuggernaut-test-utils`): registers container IDs for removal via `libc::atexit`.

### Test Helpers

- **`setup()`** — creates a namespaced DispatcherState with default Config
- **`setup_with_config(|c| ...)`** — same but with Config overrides (custom lease, timeout, capacity, etc.)
- **`start_http(state)`** — binds the HTTP router on a random port, returns base URL
- **`wait_for_state(state, key, target, timeout_secs)`** — polls until job reaches target state

### MCP Bridge Tests

The `chuggernaut-channel` integration tests include subprocess bridge tests that exercise the full MCP-over-NATS path without Docker runners or a git provider:

- **`mcp_bridge_subprocess_to_nats`**: spawns a bash MCP client, bridges stdin/stdout through `handle_message`, verifies `channel_send` → NATS outbox and `update_status` → KV.
- **`mcp_bridge_bidirectional_nats`**: round-trip test — publishes a message to the NATS inbox, mock client calls `channel_check` to receive it, replies via `channel_send`, verified on NATS outbox.

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
