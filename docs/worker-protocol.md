# Worker Protocol

Workers are stateless executors that communicate with the dispatcher exclusively via NATS. They receive everything they need in the assignment payload and interact with Forgejo only for git operations and pull requests.

---

## Lifecycle

```
┌─────────┐
│  Start  │
└────┬────┘
     │
     ▼
┌─────────────────────┐
│ Register             │ forge2.worker.register (request-reply)
│ Wait for ack         │ Includes: worker_id, capabilities, worker_type, platform
└────┬────────────────┘
     │
     ▼
┌─────────────────────┐◄──────────────────────────────────┐
│ Idle                 │ forge2.worker.idle                 │
│ Wait for assignment  │ Re-register every 15s             │
└────┬────────────────┘                                    │
     │                                                     │
     │ forge2.dispatch.assign.{worker_id}                  │
     ▼                                                     │
┌─────────────────────┐                                    │
│ Execute             │ Content ops via Forgejo git/API     │
│ Heartbeat every 10s │ forge2.worker.heartbeat            │
│ May request help    │ forge2.interact.help               │
└────┬────────────────┘                                    │
     │                                                     │
     │ forge2.worker.outcome                               │
     ▼                                                     │
┌─────────────────────┐                                    │
│ Report outcome      │ Yield / Fail / Abandon             │
└────┬────────────────┘                                    │
     │                                                     │
     └─────────────────────────────────────────────────────┘

On SIGTERM/CTRL-C:
  → forge2.worker.unregister
  → exit
```

---

## Registration

```
Worker → forge2.worker.register (request-reply)

Payload: WorkerRegistration {
  worker_id: "agent-rust-1",
  capabilities: ["rust"],
  worker_type: "interactive",
  platform: ["linux"]
}

Dispatcher → reply with empty payload (ack)
```

The dispatcher CAS-creates the `forge2.workers` KV entry (revision 0). If the worker_id already exists (another process is using it), the create fails and the dispatcher replies with an error — the worker must exit or use a different ID.

The worker blocks until the ack arrives. If no ack within 5 seconds, retry with backoff.

### Re-announcement

Every 15 seconds while idle, the worker re-registers via request-reply. Re-announcements CAS-update the existing entry (not create), so they succeed for the rightful owner. This ensures recovery after dispatcher restarts without manual intervention. If the request-reply times out (dispatcher down), the worker continues retrying.

---

## Idle

```
Worker → forge2.worker.idle

Payload: IdleEvent {
  worker_id: "agent-rust-1"
}
```

Published after registration and after each job completion. The dispatcher may immediately respond with an assignment or may wait until a suitable job appears.

### Pending Rework Check

When the dispatcher receives an `IdleEvent`, it first checks `forge2.pending-reworks` KV for a deferred rework for this worker. If found, the rework takes priority over normal assignment.

---

## Assignment

```
Dispatcher → forge2.dispatch.assign.{worker_id}

Payload: Assignment {
  job: Job,           // Full job from forge2.jobs KV
  claim: ClaimState,  // The claim just acquired
  is_rework: false,   // true if re-assigned after review feedback
  review_feedback: null  // present if is_rework, contains reviewer's feedback
}
```

The assignment includes the **full Job** — title, body, repo, priority, everything. The worker needs no other data source to understand what to do.

### Local Validation

The worker may reject an assignment by immediately reporting `Outcome::Abandon`. This is used for local checks that the dispatcher can't evaluate (e.g., disk space, network access to the repo).

### Assignment While Busy

If a worker receives an assignment while already executing a job, it ignores the assignment and logs a warning. This indicates a dispatcher-side race condition; the unserviced claim will expire via normal lease expiry.

---

## Execution

### Git Workflow

1. Read repo from `assignment.job.repo` (e.g., `"acme/payments"`)
2. `git clone http://forgejo/{repo}` (credential helper provides auth)
3. Check if branch `work/{job_key}` already exists on remote:
   - If yes: checkout and pull (continuation of previous attempt)
   - If no: create branch (fresh start)
4. Do work (varies by worker type)
5. Commit, push to Forgejo
6. Check if a PR already exists for branch `work/{job_key}`:
   - If yes: push updates the existing PR automatically
   - If no: open PR via Forgejo API:
     - Title: `[acme.payments.57] Add retry logic to payment service`
     - Body: Job description + `Job: acme.payments.57`
     - Head: `work/acme.payments.57`
     - Base: `main` (or default branch)

This idempotent approach handles reassignment after abandon, preemption, or worker crash — the new worker picks up where the previous one left off.

### Rework

When `assignment.is_rework == true`:

1. Checkout existing branch `work/{job_key}` (already exists from first pass)
2. Pull latest from remote (reviewer may have left review comments on specific lines)
3. Read `assignment.review_feedback` for guidance
4. Address feedback, commit, push
5. The existing PR auto-updates (same branch)

### Activity Reporting

Workers can publish progress updates:

```
Worker → forge2.activity.append

Payload: ActivityAppend {
  job_key: "acme.payments.57",
  entry: {
    timestamp: "...",
    kind: "progress",
    message: "Implementing exponential backoff for API retries"
  }
}
```

These are fire-and-forget. The dispatcher appends to `forge2.activities` KV (separate from the job record to avoid CAS contention with state transitions). Capped at 50 entries per job — messages beyond the limit are silently dropped.

---

## Heartbeat

```
Worker → forge2.worker.heartbeat (every 10 seconds)

Payload: WorkerHeartbeat {
  worker_id: "agent-rust-1",
  job_key: "acme.payments.57"
}
```

The dispatcher CAS-updates the claim's `last_heartbeat` and `lease_deadline`.

### Failure Detection

The worker tracks consecutive NATS publish failures:
- 1-2 failures: log warning, continue
- 3+ failures: assume NATS connection lost, cancel execution, report `Outcome::Abandon`

This is a local safeguard. The monitor also detects lease expiry from the dispatcher side.

---

## Outcome

```
Worker → forge2.worker.outcome

Payload: WorkerOutcome {
  worker_id: "agent-rust-1",
  job_key: "acme.payments.57",
  outcome: <one of below>
}
```

### Outcome Types

| Type | Fields | When | Dispatcher Action |
|------|--------|------|-------------------|
| `Yield` | `pr_url` | PR opened, ready for review | Release claim → InReview |
| `Fail` | `reason`, `logs` | Unrecoverable error | Release claim → Failed (may auto-retry) |
| `Abandon` | — | Preempted, shutdown, or local rejection | Release claim → OnDeck + blacklist entry |

The `pr_url` in Yield is stored in the job's `pr_url` field so the reviewer knows which PR to review.

---

## Preemption

```
Dispatcher → forge2.dispatch.preempt.{worker_id}

Payload: PreemptNotice {
  reason: "Higher-priority job acme.payments.99 (priority 95) needs this worker",
  new_job_key: "acme.payments.99"
}
```

The worker:
1. Cancels current execution (via `CancellationToken`)
2. Reports `Outcome::Abandon` for the current job
3. The dispatcher requeues the abandoned job to OnDeck
4. Publishes `IdleEvent` → dispatcher assigns the new job

Preemption is handled as a secondary subscription in the worker's select loop — it fires alongside the heartbeat task and execution future.

---

## NeedsHelp

Interactive workers can pause for human assistance.

### Request

```
Worker → forge2.interact.help

Payload: HelpRequest {
  worker_id: "agent-rust-1",
  job_key: "acme.payments.57",
  reason: "Unsure which API version to target — v1 is deprecated but v2 has breaking changes"
}
```

Dispatcher:
1. Transitions job to NeedsHelp
2. Stores reason in job activities
3. Claim is NOT released — worker keeps heartbeating

### Response

Human responds via CLI:
```
$ forge2 interact respond acme.payments.57 "Use v2, the breaking changes are acceptable"
```

CLI publishes to `forge2.interact.respond.{job_key}` (e.g., `forge2.interact.respond.acme.payments.57`)

Dispatcher:
1. Publishes `HelpResponse` to `forge2.interact.deliver.{worker_id}`
2. Transitions job back to OnTheStack

Worker receives response and continues execution.

---

## Interactive Sessions

Interactive workers (Claude Code in tmux/ttyd) support peek/attach/detach.

### Session Registration

When an interactive session starts, the worker writes to `forge2.sessions` KV:

```json
{
  "worker_id": "agent-rust-1",
  "session_url": "http://agent-host:7681",
  "session_name": "tmux-acme.payments.57",
  "mode": "peek",
  "started_at": "2026-03-23T10:05:00Z"
}
```

### Attach / Detach

```
CLI → forge2.interact.attach.{worker_id}   (switch to interactive mode)
CLI → forge2.interact.detach.{worker_id}   (switch to peek/headless mode)
```

The worker handles mode transitions:
- **Peek (headless):** Claude runs in background, output logged, ttyd is read-only
- **Interactive:** tmux session is live, ttyd allows input, human can type

### Cleanup

Session entries have a TTL matching the lease duration (default 60s). Workers refresh the entry alongside their heartbeat. When the job ends and heartbeats stop, the entry expires naturally — no actor needs to explicitly delete it.

---

## Worker Types

### SimWorker

For testing. Creates a branch, commits a placeholder file, opens a PR, sleeps for a configurable delay, yields.

```
clone → branch work/{key} → commit "sim: {title}" → push → open PR → sleep → yield
```

### ActionWorker

Dispatches a Forgejo Actions workflow on a dedicated branch.

```
clone → branch work/{key} → dispatch workflow → poll until complete → yield
```

Passes `job_key` and `runner_label` as workflow inputs. The action run is tied to the branch for traceability.

### InteractiveWorker

Claude Code agent in a tmux session fronted by ttyd.

```
clone → branch work/{key} → write helper scripts → launch Claude Code → monitor for result
```

Helper scripts in the work directory:
- `workflow-yield` → writes "yield" to result.txt
- `workflow-fail "reason"` → writes "fail: reason" to result.txt
- `workflow-request-help "reason"` → writes help_request.txt, polls for response

These scripts are the interface between the Claude Code subprocess (running in tmux) and the worker process. The worker process monitors the filesystem and translates file events into NATS messages — e.g., detecting help_request.txt triggers a `HelpRequest` publish to `forge2.interact.help`. This is a pragmatic bridge: Claude Code doesn't speak NATS directly.

The worker monitors for result.txt (job done), help_request.txt (pause), and attach/detach signals.

### Session Recording

After job completion, if a Claude output log exists:
- Upload to NATS JetStream object store (`forge2-session-recordings`)
- Key: `sessions/{job_key}/{session_name}`
- Optional TTL via `FORGE2_RECORDING_TTL_SECS`

---

## Graceful Shutdown

On SIGTERM or CTRL-C:

1. If executing: cancel execution, report `Outcome::Abandon`
2. Publish `forge2.worker.unregister` (UnregisterEvent with worker_id)
3. Exit

The dispatcher:
1. Releases any held claim
2. Requeues the job to OnDeck
3. Removes worker from registry

---

## Configuration

| Env Var | Default | Notes |
|---------|---------|-------|
| `FORGE2_WORKER_ID` | (required) | Unique worker identifier |
| `FORGE2_NATS_URL` | `nats://localhost:4222` | NATS connection |
| `FORGE2_FORGEJO_URL` | (required) | Forgejo base URL |
| `FORGE2_FORGEJO_TOKEN` | (required) | Forgejo API token for content ops |
| `FORGE2_HEARTBEAT_INTERVAL_SECS` | `10` | Heartbeat period |
| `FORGE2_REREGISTER_INTERVAL_SECS` | `15` | Idle re-registration period |
| `FORGE2_CAPABILITIES` | (none) | Comma-separated capability tags |
| `FORGE2_WORKER_TYPE` | `"unknown"` | Worker type classification |
| `FORGE2_PLATFORM` | (auto-detect) | Platform tags |
| `FORGE2_RECORDING_TTL_SECS` | (none) | TTL for session recordings in object store |
