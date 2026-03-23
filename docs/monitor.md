# Monitor

The monitor scans for stale claims, timed-out jobs, and orphaned resources. It publishes advisory events — it never mutates state directly. The dispatcher re-verifies every advisory before acting.

---

## Deployment

Runs as a background tokio task inside the dispatcher process. Not a separate process — it's a lightweight scan loop and shares the dispatcher's NATS connection and configuration.

---

## Scan Loop

The monitor runs on a fixed tick, configurable via `FORGE2_MONITOR_SCAN_INTERVAL_SECS` (default 10). Each tick runs all scans sequentially:

1. Lease expiry scan
2. Job timeout scan
3. Orphan detection scan
4. Retry scan
5. Job archival scan

On dispatcher startup, the first scan runs immediately to catch stale claims that accumulated during downtime. This may produce a burst of advisories — the dispatcher processes them sequentially and the volume is bounded by the number of workers.

---

## Scans

### Lease Expiry

Scans `forge2.claims` KV for entries where `now > lease_deadline`.

Publishes `LeaseExpiredEvent` to `forge2.monitor.lease-expired`.

The dispatcher receives the event, re-reads the claim from KV (the heartbeat may have renewed it since the scan), and acts only if still expired.

### Job Timeout

Scans `forge2.claims` KV for entries where `now - claimed_at > timeout_secs`.

Publishes `JobTimeoutEvent` to `forge2.monitor.timeout`.

The dispatcher transitions the job to Failed with reason "job timeout".

### Orphan Detection

Three orphan conditions are detected:

**1. Claim with unknown worker:** A `forge2.claims` entry whose `worker_id` does not exist in `forge2.workers` KV. This overlaps with lease expiry (if the worker is gone, heartbeats stop), but catches the edge case where a worker deregistered without its claim being released.

**2. Stale session:** A `forge2.sessions` entry for a job that is no longer OnTheStack or NeedsHelp. Session entries have a TTL (matching the lease duration, default 60s) and workers refresh them on each heartbeat cycle, so stale sessions expire naturally. This scan detects any that slip through.

**3. Claimless on-the-stack job:** A job in OnTheStack state with no corresponding entry in `forge2.claims`. This indicates a dispatcher bug (claim was released but job state wasn't updated).

All three conditions publish `OrphanDetectedEvent` to `forge2.monitor.orphan`. The dispatcher handles condition 1 by releasing the claim and transitioning the job to Failed. For conditions 2 and 3, the dispatcher logs a warning — these indicate bugs rather than normal failure modes.

### Retry

Scans `forge2.jobs` KV for entries where `state == "failed"` and `retry_count < max_retries` and `now > retry_after`.

Publishes `RetryEligibleEvent` to `forge2.monitor.retry`.

The dispatcher receives the event, re-reads the job from KV (state may have changed since the scan), and if still eligible transitions to OnDeck. This survives restarts — `retry_after` is persisted in the job record, so pending retries are picked up on the next scan.

---

## Session Cleanup

Session entries in `forge2.sessions` KV use a TTL matching the lease duration (default 60s). Workers refresh the entry alongside their heartbeat (every 10s). If a worker dies, the session entry expires on its own without any actor needing to delete it. This avoids violating the ownership model (workers write their own session keys).

The orphan detection scan (condition 2) serves as a secondary check for any session entries that survive past their TTL due to timing edge cases.

---

## Job Archival

A fifth scan runs after the detection and retry scans. It reclaims resources from completed jobs that are no longer needed.

### Candidates

Jobs in terminal states (Done, Revoked, Failed with retry_count >= max_retries) become archival candidates after `FORGE2_JOB_RETENTION_SECS` (default 86400 / 24 hours), measured from `updated_at`.

A candidate is only archived if every job in its `depended_on_by` list is also in a terminal state. This ensures no active job references the archived job for unblock checks.

### Archival Actions

For each archivable job, the dispatcher:

1. Remove from in-memory petgraph (node + all edges) and job index
2. Delete `forge2.deps` entry
3. Delete `forge2.activities` entry
4. Delete `forge2.rework-counts` entry (if present)
5. Delete `forge2.pending-reworks` entry (if present, defensive)
6. Set a TTL on the `forge2.jobs` KV entry (default: same as retention period) — the job remains queryable via the HTTP API until NATS evicts it

### API Impact

- `GET /jobs` returns only non-archived (in-memory) jobs by default
- `GET /jobs?include=archived` also returns jobs still in KV but evicted from memory
- `GET /jobs/{key}` reads directly from KV, so it works for archived jobs until TTL expiry
- SSE snapshots include only non-archived jobs

---

## Configuration

| Env Var | Default | Notes |
|---------|---------|-------|
| `FORGE2_MONITOR_SCAN_INTERVAL_SECS` | `10` | Fixed tick interval for all scans |
| `FORGE2_JOB_RETENTION_SECS` | `86400` | Time after terminal state before archival |

No other configuration — the monitor inherits NATS connection, lease durations, and timeout values from the dispatcher.
