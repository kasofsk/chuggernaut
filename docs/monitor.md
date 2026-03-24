# Monitor

The monitor scans for stale claims, timed-out jobs, and orphaned resources. It publishes advisory events — it never mutates state directly. The dispatcher re-verifies every advisory before acting.

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
5. Job archival scan

On dispatcher startup, the first scan runs immediately to catch stale claims that accumulated during downtime. This may produce a burst of advisories — the dispatcher processes them sequentially and the volume is bounded by the number of active action jobs.

---

## Scans

### Lease Expiry

Scans `chuggernaut.claims` KV for entries where `now > lease_deadline`.

Publishes `LeaseExpiredEvent` to `chuggernaut.monitor.lease-expired`.

The dispatcher receives the event, re-reads the claim from KV (the heartbeat may have renewed it since the scan), and acts only if still expired.

### Job Timeout

Scans `chuggernaut.claims` KV for entries where `now - claimed_at > timeout_secs`.

Publishes `JobTimeoutEvent` to `chuggernaut.monitor.timeout`.

The dispatcher transitions the job to Failed with reason "job timeout".

### Orphan Detection

One orphan condition is detected:

**Claimless on-the-stack job:** A job in OnTheStack state with no corresponding entry in `chuggernaut.claims`. This indicates a dispatcher bug (claim was released but job state wasn't updated).

Publishes `OrphanDetectedEvent` to `chuggernaut.monitor.orphan`. The dispatcher logs a warning — this indicates a bug rather than a normal failure mode.

### Retry

Scans `chuggernaut.jobs` KV for entries where `state == "failed"` and `retry_count < max_retries` and `now > retry_after`.

Publishes `RetryEligibleEvent` to `chuggernaut.monitor.retry`.

The dispatcher receives the event, re-reads the job from KV (state may have changed since the scan), and if still eligible transitions to OnDeck. This survives restarts — `retry_after` is persisted in the job record, so pending retries are picked up on the next scan.

---

## Job Archival

A fifth scan runs after the detection and retry scans. It reclaims resources from completed jobs that are no longer needed.

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
