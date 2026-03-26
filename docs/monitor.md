# Monitor

The monitor scans for stale claims, timed-out jobs, and orphaned resources. It routes events directly to the dispatcher's assignment task channel via `publish_and_dispatch`, which both publishes to NATS (for JetStream observability) and sends a `DispatchRequest` to the sequential processing channel. Events no longer round-trip through a NATS handler subscription. The dispatcher re-verifies every event before acting.

---

## Deployment

Runs as a background tokio task inside the dispatcher process. Not a separate process — it's a lightweight scan loop and shares the dispatcher's NATS connection and configuration.

---

## Scan Loop

The monitor runs on a fixed tick, configurable via `CHUGGERNAUT_MONITOR_SCAN_INTERVAL_SECS` (default 10). Each tick runs all scans sequentially:

1. Lease expiry scan
2. Job timeout scan
3. Orphan detection scan
4. Retry scan
5. CI status scan
6. Pending reviews scan
7. Job archival scan

On dispatcher startup, the first scan runs immediately to catch stale claims that accumulated during downtime. This may produce a burst of advisories — the dispatcher processes them sequentially and the volume is bounded by the number of active action jobs.

---

## Scans

### Lease Expiry

Iterates in-memory jobs in active states (OnTheStack, Reviewing) and checks each job's individual claim from `chuggernaut.claims` KV. Detects entries where `now > lease_deadline`. This avoids streaming `kv.claims.keys()`, which could hang on tombstone-only KV buckets.

Dispatches `LeaseExpiredEvent` via `publish_and_dispatch` (publishes to `chuggernaut.monitor.lease-expired` and sends directly to the assignment task channel).

The dispatcher re-reads the claim from KV (the heartbeat may have renewed it since the scan), and acts only if still expired.

### Job Timeout

Iterates in-memory jobs in active states (OnTheStack, Reviewing) and checks each job's individual claim from `chuggernaut.claims` KV. Detects entries where `now - claimed_at > timeout_secs`. Like lease expiry, this avoids streaming KV keys.

Dispatches `JobTimeoutEvent` via `publish_and_dispatch` (publishes to `chuggernaut.monitor.timeout` and sends directly to the assignment task channel).

The dispatcher transitions the job to Failed with reason "job timeout".

### Orphan Detection

One orphan condition is detected:

**Claimless on-the-stack job:** A job in OnTheStack state with no corresponding entry in `chuggernaut.claims`. This indicates a dispatcher bug (claim was released but job state wasn't updated).

Dispatches `OrphanDetectedEvent` via `publish_and_dispatch` (publishes to `chuggernaut.monitor.orphan` and sends directly to the assignment task channel). The dispatcher transitions the orphaned job to Failed and schedules auto-retry (if retry_count < max_retries). Previously this only logged a warning.

### Retry

Scans in-memory jobs for entries where `state == "failed"` and `retry_count < max_retries` and `now > retry_after`.

Dispatches `RetryEligibleEvent` via `publish_and_dispatch` (publishes to `chuggernaut.monitor.retry` and sends directly to the assignment task channel).

The dispatcher re-reads the job from KV (state may have changed since the scan), and if still eligible transitions to OnDeck. This survives restarts — `retry_after` is persisted in the job record, so pending retries are picked up on the next scan.

### CI Status

Scans in-memory jobs in OnTheStack state and checks the CI status of their associated Forgejo Action runs. Detects runs that have completed (success or failure) but whose outcome has not yet been processed by the dispatcher (e.g., because the NATS message was lost).

Dispatches events via `publish_and_dispatch`. On CI failure, if `rework_count >= rework_limit`, the dispatcher escalates to a human reviewer instead of retrying.

### Pending Reviews

Scans in-memory jobs in InReview state and checks whether a review action is still running. Detects cases where the review action completed or failed without the dispatcher receiving the review decision (e.g., lost NATS message, action crash).

Dispatches events via `publish_and_dispatch` so the dispatcher can re-dispatch the review action or handle the stale review state.

---

## Job Archival

The final scan runs after the detection and retry scans. It reclaims resources from completed jobs that are no longer needed.

### Candidates

Jobs in terminal states (Done, Revoked, Failed with retry_count >= max_retries) become archival candidates after `CHUGGERNAUT_JOB_RETENTION_SECS` (default 86400 / 24 hours), measured from `updated_at`.

A candidate is only archived if every job in its `depended_on_by` list is also in a terminal state. This ensures no active job references the archived job for unblock checks.

### Archival Actions

For each archivable job, the dispatcher:

1. Remove from in-memory petgraph (node + all edges) and job index
2. Delete `chuggernaut.deps` entry
3. Delete `chuggernaut.activities` entry
4. Delete `chuggernaut.rework-counts` entry (if present, managed by dispatcher)
5. Set a TTL on the `chuggernaut.jobs` KV entry (default: same as retention period) — the job remains queryable via the HTTP API until NATS evicts it

### API Impact

- `GET /jobs` returns only non-archived (in-memory) jobs by default
- `GET /jobs?include=archived` also returns jobs still in KV but evicted from memory
- `GET /jobs/{key}` reads directly from KV, so it works for archived jobs until TTL expiry
- SSE snapshots include only non-archived jobs

---

## Configuration

| Env Var | Default | Notes |
|---------|---------|-------|
| `CHUGGERNAUT_MONITOR_SCAN_INTERVAL_SECS` | `10` | Fixed tick interval for all scans |
| `CHUGGERNAUT_JOB_RETENTION_SECS` | `86400` | Time after terminal state before archival |

No other configuration — the monitor inherits NATS connection, lease durations, and timeout values from the dispatcher.
