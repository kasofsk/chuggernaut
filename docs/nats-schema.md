# NATS Schema

Complete reference for all KV buckets, JetStream streams, and NATS subjects used by chuggernaut.

---

## KV Buckets

Each KV bucket is backed by a JetStream stream. This provides both current-state reads (KV get/put) and event replay (stream consumer/watch). All buckets use file-backed storage for durability.

### chuggernaut.jobs

The canonical job store. Every job in the system has an entry here.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}.{seq}` (e.g., `acme.payments.57`) |
| **History** | 1 (latest value only, history in transitions stream) |
| **TTL** | None |

**Value: `Job` JSON**
```json
{
  "key": "acme.payments.57",
  "repo": "acme/payments",
  "state": "on-deck",
  "title": "Add retry logic to payment service",
  "body": "Retry failed API calls with exponential backoff...",
  "priority": 80,
  "capabilities": ["rust"],
  "worker_type": null,
  "platform": null,
  "timeout_secs": 3600,
  "review": "high",
  "max_retries": 3,
  "retry_count": 0,
  "retry_after": null,
  "pr_url": null,
  "last_worker_id": null,
  "created_at": "2026-03-23T10:00:00Z",
  "updated_at": "2026-03-23T10:05:00Z"
}
```

**Priority:** 0–100, higher = more urgent, default 50. Used for assignment ordering and preemption decisions.

**Review levels:** `human`, `low`, `medium`, `high` (default). Determines review automation behavior; see [reviewer.md](reviewer.md).

**States:** `on-ice`, `blocked`, `on-deck`, `on-the-stack`, `needs-help`, `in-review`, `escalated`, `changes-requested`, `done`, `failed`, `revoked`

### chuggernaut.claims

Exclusive claim state. CAS (compare-and-swap) operations ensure at most one worker holds a claim per job.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | None (monitor handles expiry) |

**Value: `ClaimState` JSON**
```json
{
  "worker_id": "agent-rust-1",
  "claimed_at": "2026-03-23T10:05:00Z",
  "last_heartbeat": "2026-03-23T10:05:30Z",
  "lease_deadline": "2026-03-23T10:06:30Z",
  "timeout_secs": 3600,
  "lease_secs": 60
}
```

**Two-tier timeout:**
- **Lease** (short, default 60s): must be renewed by heartbeats. Expiry → requeue.
- **Job timeout** (long, default 3600s): total execution time. Expiry → fail.

### chuggernaut.deps

Dependency graph edges. Each entry has both forward (depends_on) and reverse (depended_on_by) indexes for O(1) lookups in either direction.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | None |

**Value: `DepRecord` JSON**
```json
{
  "depends_on": ["acme.payments.40", "acme.payments.41"],
  "depended_on_by": ["acme.payments.60"]
}
```

The dispatcher maintains a petgraph DAG in memory, rebuilt from this bucket on startup. All cycle detection and traversal runs against the in-memory graph. The KV is the durable persistence layer.

### chuggernaut.workers

Worker registry. Tracks all known workers and their current status.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{worker_id}` (e.g., `agent-rust-1`) |
| **History** | 1 |
| **TTL** | None (dispatcher prunes stale entries) |

**Value: `WorkerInfo` JSON**
```json
{
  "worker_id": "agent-rust-1",
  "state": "idle",
  "capabilities": ["rust"],
  "worker_type": "interactive",
  "platform": ["linux"],
  "current_job": null,
  "last_seen": "2026-03-23T10:05:00Z"
}
```

**Worker states:** `idle`, `busy`

### chuggernaut.counters

Atomic sequence numbers for job key generation. One key per repo.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}` |
| **History** | 1 |
| **TTL** | None |

**Value:** Integer as string (e.g., `"57"`). Incremented via NATS KV CAS to generate the next job sequence number.

### chuggernaut.sessions

Interactive agent session metadata. Workers write their own keys.

| Field | Value |
|-------|-------|
| **Owner** | Workers (each writes own key) |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | 60s (matches lease duration; workers refresh on each heartbeat) |

**Value: `SessionInfo` JSON**
```json
{
  "worker_id": "agent-rust-1",
  "session_url": "http://host:7681",
  "session_name": "tmux-acme.payments.57",
  "mode": "peek",
  "started_at": "2026-03-23T10:05:00Z"
}
```

### chuggernaut.activities

Job activity log. Separated from the Job record to avoid CAS contention between progress updates and state transitions.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | None (cleaned up by archival) |

**Value: `ActivityLog` JSON**
```json
{
  "entries": [
    {
      "timestamp": "2026-03-23T10:00:00Z",
      "kind": "created",
      "message": "Job created via CLI"
    },
    {
      "timestamp": "2026-03-23T10:10:00Z",
      "kind": "progress",
      "message": "Implementing exponential backoff for API retries"
    }
  ]
}
```

**Entries:** Capped at 50 per job. The dispatcher silently drops entries beyond the limit.

### chuggernaut.pending-reworks

Deferred rework assignments. When a review requests changes but the original worker is busy, the rework is queued here.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | None |

**Value: `PendingRework` JSON**
```json
{
  "worker_id": "agent-rust-1",
  "review_feedback": "Please fix the error handling in retry.rs..."
}
```

If the target worker is pruned (lease expiry, timeout), the dispatcher reassigns the rework to any capable idle worker.

### chuggernaut.abandon-blacklist

Prevents tight assign → abandon loops on the same (job, worker) pair.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}.{seq}.{worker_id}` |
| **History** | 1 |
| **TTL** | Configurable via `CHUGGERNAUT_BLACKLIST_TTL_SECS` (default 3600 / 1 hour) |

**Value:** `"1"`

### chuggernaut.merge-queue

Per-repo merge serialization. Prevents concurrent merges that could cause rebase conflicts.

| Field | Value |
|-------|-------|
| **Owner** | Reviewer |
| **Key** | `{owner}.{repo}.lock` or `{owner}.{repo}.queue` |
| **History** | 1 |
| **TTL** | 5 minutes (lock keys); none (queue keys) |

**Lock value:** Lock holder token (CAS-based). TTL acts as a safety net — the reviewer refreshes the lock during long merges and clears stale locks on startup.
**Queue value: `[MergeQueueEntry]` JSON (array)**
```json
[
  {
    "job_key": "acme.payments.57",
    "pr_url": "http://forgejo/acme/payments/pulls/3",
    "queued_at": "2026-03-23T11:00:00Z"
  }
]
```

### chuggernaut.rework-counts

Tracks rework cycles per job to prevent infinite review loops.

| Field | Value |
|-------|-------|
| **Owner** | Reviewer |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | None |

**Value:** Integer as string (e.g., `"2"`). Escalates to human when rework count exceeds 3.

### chuggernaut.journal

Dispatcher action log. Every significant action is recorded for observability.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{timestamp_nanos}.{seq}` |
| **History** | 1 |
| **TTL** | 7 days |

**Value: `JournalEntry` JSON**
```json
{
  "timestamp": "2026-03-23T10:05:00Z",
  "action": "assigned",
  "job_key": "acme.payments.57",
  "worker_id": "agent-rust-1",
  "details": "priority 80, capability match: rust"
}
```

---

## JetStream Streams

### CHUGGERNAUT-TRANSITIONS

Derived event stream. Published by the dispatcher whenever a job changes state.

| Field | Value |
|-------|-------|
| **Subjects** | `chuggernaut.transitions.>` |
| **Retention** | Limits (7 days, 10GB) |
| **Storage** | File |
| **Duplicates** | 5 minute window, message ID: `{job_key}.{from_state}.{to_state}` |

**Subject pattern:** `chuggernaut.transitions.{owner}.{repo}.{seq}`

**Payload: `JobTransition` JSON**
```json
{
  "job_key": "acme.payments.57",
  "from_state": "on-deck",
  "to_state": "on-the-stack",
  "timestamp": "2026-03-23T10:05:00Z",
  "trigger": "dispatcher_assigned",
  "worker_id": "agent-rust-1"
}
```

**Consumers:**
- **Reviewer**: durable pull consumer, filters for `to_state == "in-review"`
- **Dispatcher SSE**: ephemeral push consumer, relays transition events to graph viewer clients

### CHUGGERNAUT-WORKER-EVENTS

Worker lifecycle events.

| Field | Value |
|-------|-------|
| **Subjects** | `chuggernaut.worker.register`, `chuggernaut.worker.idle`, `chuggernaut.worker.outcome`, `chuggernaut.worker.unregister` |
| **Retention** | Limits (1 day) |
| **Storage** | File |

Heartbeats are excluded — they generate high volume and are already recorded in `chuggernaut.claims` KV (`last_heartbeat` field).

### CHUGGERNAUT-MONITOR

Advisory events from the monitor.

| Field | Value |
|-------|-------|
| **Subjects** | `chuggernaut.monitor.>` |
| **Retention** | Limits (1 day) |
| **Storage** | File |

Subjects: `chuggernaut.monitor.lease-expired`, `chuggernaut.monitor.timeout`, `chuggernaut.monitor.orphan`, `chuggernaut.monitor.retry`

---

## Subject Hierarchy

Complete NATS subject reference. All subjects are prefixed with `chuggernaut.`.

### Transitions (dispatcher publishes)

| Subject | Payload | Pattern |
|---------|---------|---------|
| `chuggernaut.transitions.{owner}.{repo}.{seq}` | `JobTransition` | Per-job state change events |

### Worker Events (workers publish)

| Subject | Payload | Notes |
|---------|---------|-------|
| `chuggernaut.worker.register` | `WorkerRegistration` | Request-reply. Worker waits for ack. |
| `chuggernaut.worker.idle` | `IdleEvent` | Worker ready for assignment |
| `chuggernaut.worker.heartbeat` | `WorkerHeartbeat` | Periodic lease renewal |
| `chuggernaut.worker.outcome` | `WorkerOutcome` | Job result (yield/fail/abandon) |
| `chuggernaut.worker.unregister` | `UnregisterEvent` | Graceful shutdown |

### Dispatch (dispatcher publishes to specific workers)

| Subject | Payload | Notes |
|---------|---------|-------|
| `chuggernaut.dispatch.assign.{worker_id}` | `Assignment` | Includes full Job + ClaimState |
| `chuggernaut.dispatch.preempt.{worker_id}` | `PreemptNotice` | Cancel current, take new job |

### Interaction (bidirectional)

| Subject | Publisher | Subscriber | Payload |
|---------|-----------|------------|---------|
| `chuggernaut.interact.help` | Worker | Dispatcher | `HelpRequest` |
| `chuggernaut.interact.respond.{job_key}` | CLI/Human | Dispatcher | `HelpResponse` |
| `chuggernaut.interact.deliver.{worker_id}` | Dispatcher | Worker | `HelpResponse` |
| `chuggernaut.interact.attach.{worker_id}` | CLI | Worker | (empty) |
| `chuggernaut.interact.detach.{worker_id}` | CLI | Worker | (empty) |

### Review (reviewer → dispatcher)

| Subject | Payload | Notes |
|---------|---------|-------|
| `chuggernaut.review.decision` | `ReviewDecision` | Approved / ChangesRequested / Escalated |

### Activity (fire-and-forget → dispatcher)

| Subject | Payload | Notes |
|---------|---------|-------|
| `chuggernaut.activity.append` | `ActivityAppend` | Workers/reviewer append to job activities |
| `chuggernaut.journal.append` | `JournalAppend` | External processes add journal entries |

### Monitor (advisory → dispatcher)

| Subject | Payload |
|---------|---------|
| `chuggernaut.monitor.lease-expired` | `LeaseExpiredEvent` |
| `chuggernaut.monitor.timeout` | `JobTimeoutEvent` |
| `chuggernaut.monitor.orphan` | `OrphanDetectedEvent` |
| `chuggernaut.monitor.retry` | `RetryEligibleEvent` |

### Admin (CLI → dispatcher)

| Subject | Payload | Notes |
|---------|---------|-------|
| `chuggernaut.admin.create-job` | `CreateJobRequest` | Request-reply. Returns job key. |
| `chuggernaut.admin.requeue` | `RequeueRequest` | Request-reply. Returns success/error. |
| `chuggernaut.admin.close-job` | `CloseJobRequest` | Request-reply. Done or revoke. |

---

## Message Types

### CreateJobRequest
```json
{
  "repo": "acme/payments",
  "title": "Add retry logic to payment service",
  "body": "Retry failed API calls with exponential backoff...",
  "depends_on": [40, 41],
  "priority": 80,
  "capabilities": ["rust"],
  "worker_type": null,
  "platform": null,
  "timeout_secs": 3600,
  "review": "high",
  "max_retries": 3,
  "initial_state": null
}
```
`depends_on` contains sequence numbers within the same repo. `initial_state` defaults to auto-computed (blocked if deps, on-deck if no deps). Set to `"on-ice"` to hold.

### WorkerRegistration
```json
{
  "worker_id": "agent-rust-1",
  "capabilities": ["rust"],
  "worker_type": "interactive",
  "platform": ["linux"]
}
```

### Assignment
```json
{
  "job": { "...full Job JSON..." },
  "claim": { "...ClaimState JSON..." },
  "is_rework": false,
  "review_feedback": null
}
```

### WorkerOutcome
```json
{
  "worker_id": "agent-rust-1",
  "job_key": "acme.payments.57",
  "outcome": {
    "type": "yield",
    "pr_url": "http://forgejo/acme/payments/pulls/3"
  }
}
```
Outcome types: `yield { pr_url }`, `fail { reason, logs }`, `abandon {}`

### ReviewDecision
```json
{
  "job_key": "acme.payments.57",
  "decision": "approved",
  "pr_url": "http://forgejo/acme/payments/pulls/3",
  "feedback": null
}
```
Decisions: `approved`, `changes_requested { feedback }`, `escalated { reviewer_login }`

### HelpRequest
```json
{
  "worker_id": "agent-rust-1",
  "job_key": "acme.payments.57",
  "reason": "Unsure which API version to target"
}
```

### ActivityAppend
```json
{
  "job_key": "acme.payments.57",
  "entry": {
    "timestamp": "2026-03-23T10:10:00Z",
    "kind": "progress",
    "message": "Created branch work/acme.payments.57, implementing retry logic"
  }
}
```

### JobTransition
```json
{
  "job_key": "acme.payments.57",
  "from_state": "blocked",
  "to_state": "on-deck",
  "timestamp": "2026-03-23T10:04:00Z",
  "trigger": "deps_resolved"
}
```

### PreemptNotice
```json
{
  "reason": "Higher-priority job acme.payments.99 (priority 95) needs this worker",
  "new_job_key": "acme.payments.99"
}
```

### HelpResponse
```json
{
  "job_key": "acme.payments.57",
  "message": "Use v2, the breaking changes are acceptable"
}
```

### RequeueRequest
```json
{
  "job_key": "acme.payments.57",
  "target": "on-deck"
}
```
Target: `"on-deck"` or `"on-ice"`. The dispatcher checks deps — `"on-deck"` may resolve to Blocked if deps are unresolved.

### CloseJobRequest
```json
{
  "job_key": "acme.payments.57",
  "revoke": false
}
```

### JournalAppend
```json
{
  "timestamp": "2026-03-23T10:05:00Z",
  "action": "external_event",
  "job_key": "acme.payments.57",
  "worker_id": null,
  "details": "Manually triggered by admin"
}
```

### LeaseExpiredEvent
```json
{
  "job_key": "acme.payments.57",
  "worker_id": "agent-rust-1",
  "lease_deadline": "2026-03-23T10:06:30Z",
  "detected_at": "2026-03-23T10:06:45Z"
}
```

### JobTimeoutEvent
```json
{
  "job_key": "acme.payments.57",
  "worker_id": "agent-rust-1",
  "claimed_at": "2026-03-23T10:05:00Z",
  "timeout_secs": 3600,
  "detected_at": "2026-03-23T11:05:15Z"
}
```

### OrphanDetectedEvent
```json
{
  "job_key": "acme.payments.57",
  "worker_id": "agent-rust-1",
  "kind": "claim_unknown_worker",
  "detected_at": "2026-03-23T10:06:45Z"
}
```
Kind: `"claim_unknown_worker"`, `"stale_session"`, or `"claimless_on_the_stack"`. `worker_id` is null for `claimless_on_the_stack`.

### RetryEligibleEvent
```json
{
  "job_key": "acme.payments.57",
  "retry_count": 1,
  "retry_after": "2026-03-23T10:06:00Z",
  "detected_at": "2026-03-23T10:06:10Z"
}
```

### WorkerHeartbeat
```json
{
  "worker_id": "agent-rust-1",
  "job_key": "acme.payments.57"
}
```

### IdleEvent
```json
{
  "worker_id": "agent-rust-1"
}
```

### UnregisterEvent
```json
{
  "worker_id": "agent-rust-1"
}
```

---

## Object Stores

### chuggernaut-session-recordings

Session recordings from interactive workers. Stored in a NATS JetStream object store.

| Field | Value |
|-------|-------|
| **Owner** | Workers (each writes own recordings) |
| **Key** | `sessions/{job_key}/{session_name}` |
| **TTL** | Configurable via `CHUGGERNAUT_RECORDING_TTL_SECS` (optional) |

**Value:** Raw session log (Claude Code output).

---

## Bucket Initialization

On startup, the dispatcher creates all owned KV buckets and streams if they don't exist. Configuration:

```rust
// KV buckets
jetstream.create_key_value(Config {
    bucket: "chuggernaut.jobs",
    history: 1,
    storage: StorageType::File,
    ..Default::default()
});

// Streams (dispatcher creates all three)
jetstream.create_stream(StreamConfig {
    name: "CHUGGERNAUT-TRANSITIONS",
    subjects: vec!["chuggernaut.transitions.>"],
    retention: RetentionPolicy::Limits,
    max_age: Duration::from_secs(7 * 24 * 3600),
    storage: StorageType::File,
    ..Default::default()
});

jetstream.create_stream(StreamConfig {
    name: "CHUGGERNAUT-WORKER-EVENTS",
    subjects: vec!["chuggernaut.worker.register", "chuggernaut.worker.idle", "chuggernaut.worker.outcome", "chuggernaut.worker.unregister"],
    retention: RetentionPolicy::Limits,
    max_age: Duration::from_secs(24 * 3600),
    storage: StorageType::File,
    ..Default::default()
});

jetstream.create_stream(StreamConfig {
    name: "CHUGGERNAUT-MONITOR",
    subjects: vec!["chuggernaut.monitor.>"],
    retention: RetentionPolicy::Limits,
    max_age: Duration::from_secs(24 * 3600),
    storage: StorageType::File,
    ..Default::default()
});
```

The reviewer creates its own buckets (`chuggernaut.merge-queue`, `chuggernaut.rework-counts`) on startup.
