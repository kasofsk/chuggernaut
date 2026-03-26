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
  "platform": null,
  "timeout_secs": 3600,
  "review": "high",
  "max_retries": 3,
  "retry_count": 0,
  "retry_after": null,
  "pr_url": null,
  "token_usage": [
    { "action_type": "work", "token_usage": { "input_tokens": 25000, "output_tokens": 8000, "cache_read_tokens": 0, "cache_write_tokens": 0 }, "completed_at": "2026-03-23T10:30:00Z" },
    { "action_type": "review", "token_usage": { "input_tokens": 15000, "output_tokens": 3500, "cache_read_tokens": 0, "cache_write_tokens": 0 }, "completed_at": "2026-03-23T10:35:00Z" }
  ],
  "created_at": "2026-03-23T10:00:00Z",
  "updated_at": "2026-03-23T10:35:00Z"
}
```

**Priority:** 0–100, higher = more urgent, default 50. Used for dispatch ordering.

**Review levels:** `human`, `low`, `medium`, `high` (default). Determines review automation behavior; see [reviewer.md](reviewer.md).

**States:** `on-ice`, `blocked`, `on-deck`, `on-the-stack`, `in-review`, `escalated`, `changes-requested`, `done`, `failed`, `revoked`

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
  "worker_id": "action-acme.payments.57",
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

### chuggernaut.counters

Atomic sequence numbers for job key generation. One key per repo.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}` |
| **History** | 1 |
| **TTL** | None |

**Value:** Integer as string (e.g., `"57"`). Incremented via NATS KV CAS to generate the next job sequence number.

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

### chuggernaut.merge-queue

Per-repo merge serialization. Prevents concurrent merges that could cause rebase conflicts.

| Field | Value |
|-------|-------|
| **Owner** | Dispatcher (initialized on startup; used by review actions) |
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
| **Owner** | Dispatcher |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | None |

**Value:** Integer as string (e.g., `"2"`). Escalates to human when rework count exceeds 3.

### chuggernaut_channels

Worker channel status. The MCP channel's `update_status` tool writes progress updates here.

| Field | Value |
|-------|-------|
| **Owner** | Workers (via MCP channel) |
| **Key** | `{owner}.{repo}.{seq}` |
| **History** | 1 |
| **TTL** | None |

**Value: `ChannelStatus` JSON**
```json
{
  "job_key": "acme.payments.57",
  "status": "implementing feature",
  "progress": 0.45,
  "updated_at": "2026-03-24T10:05:00Z"
}
```

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
  "worker_id": "action-acme.payments.57",
  "details": "priority 80, dispatched action"
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
  "worker_id": "action-acme.payments.57"
}
```

**Consumers:**
- **Dispatcher SSE**: ephemeral push consumer, relays transition events to graph viewer clients

### CHUGGERNAUT-WORKER-EVENTS

Worker outcome events.

| Field | Value |
|-------|-------|
| **Subjects** | `chuggernaut.worker.outcome` |
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

### Worker Events (action workers publish)

| Subject | Payload | Notes |
|---------|---------|-------|
| `chuggernaut.worker.heartbeat` | `WorkerHeartbeat` | Periodic lease renewal (every 10s) |
| `chuggernaut.worker.outcome` | `WorkerOutcome` | Job result (yield/fail) |

### Review (review action → dispatcher)

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

### Channel (worker MCP ↔ orchestrator)

| Subject | Publisher | Subscriber | Payload |
|---------|-----------|------------|---------|
| `chuggernaut.channel.{job_key}.inbox` | Orchestrator/CLI | Worker (MCP channel) | `ChannelMessage` |
| `chuggernaut.channel.{job_key}.outbox` | Worker (MCP channel) | Orchestrator/CLI | `ChannelMessage` |

The channel provides bidirectional messaging between a running worker and external callers. The MCP server inside the worker bridges these NATS subjects to Claude's stdin/stdout via JSON-RPC tools (`channel_check`, `channel_send`, `reply`, `update_status`).

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
  "platform": null,
  "timeout_secs": 3600,
  "review": "high",
  "max_retries": 3,
  "initial_state": null
}
```
`depends_on` contains sequence numbers within the same repo. `initial_state` defaults to auto-computed (blocked if deps, on-deck if no deps). Set to `"on-ice"` to hold.

### WorkerOutcome
```json
{
  "worker_id": "action-acme.payments.57",
  "job_key": "acme.payments.57",
  "outcome": {
    "type": "yield",
    "pr_url": "http://forgejo/acme/payments/pulls/3"
  },
  "token_usage": {
    "input_tokens": 25000,
    "output_tokens": 8000,
    "cache_read_tokens": 12000,
    "cache_write_tokens": 3000
  }
}
```
Outcome types: `yield { pr_url }`, `fail { reason, logs }`. `token_usage` is optional (null if not captured).

### ReviewDecision
```json
{
  "job_key": "acme.payments.57",
  "decision": "approved",
  "pr_url": "http://forgejo/acme/payments/pulls/3",
  "token_usage": {
    "input_tokens": 15000,
    "output_tokens": 3500,
    "cache_read_tokens": 8000,
    "cache_write_tokens": 2000
  }
}
```
Decisions: `approved`, `changes_requested { feedback }`, `escalated { reviewer_login }`. `token_usage` is optional. Published by review actions (worker in review mode).

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
  "worker_id": "action-acme.payments.57",
  "lease_deadline": "2026-03-23T10:06:30Z",
  "detected_at": "2026-03-23T10:06:45Z"
}
```

### JobTimeoutEvent
```json
{
  "job_key": "acme.payments.57",
  "worker_id": "action-acme.payments.57",
  "claimed_at": "2026-03-23T10:05:00Z",
  "timeout_secs": 3600,
  "detected_at": "2026-03-23T11:05:15Z"
}
```

### OrphanDetectedEvent
```json
{
  "job_key": "acme.payments.57",
  "worker_id": "action-acme.payments.57",
  "kind": "claimless_on_the_stack",
  "detected_at": "2026-03-23T10:06:45Z"
}
```
Kind: `"claimless_on_the_stack"`. Indicates a dispatcher bug (claim was released but job state wasn't updated). `worker_id` is null for this kind.

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
  "worker_id": "action-acme.payments.57",
  "job_key": "acme.payments.57"
}
```

### ChannelMessage
```json
{
  "sender": "cli:david",
  "body": "what is your status?",
  "timestamp": "2026-03-24T10:05:00Z",
  "message_id": "a89d3169-ec98-4553-aad8-39cd1ed82cc8",
  "in_reply_to": null
}
```
Used on both `channel.{job_key}.inbox` and `channel.{job_key}.outbox` subjects. The `sender` field is prefixed with the source (e.g., `cli:david`, `claude:acme.payments.57`). The `in_reply_to` field references the `message_id` of a previous message when using the `reply` tool.

### ChannelStatus
```json
{
  "job_key": "acme.payments.57",
  "status": "implementing feature",
  "progress": 0.45,
  "updated_at": "2026-03-24T10:05:00Z"
}
```
Written to the `chuggernaut_channels` KV bucket by the `update_status` MCP tool. Progress is a float 0.0–1.0 (input is 0–100, divided by 100).

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
    subjects: vec!["chuggernaut.worker.outcome"],
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

The dispatcher also creates the `chuggernaut.merge-queue` and `chuggernaut.rework-counts` buckets on startup (previously owned by the standalone reviewer process, now part of the dispatcher's initialization).
