# Dispatcher

The dispatcher is the single coordinator in chuggernaut. It owns all job state transitions, dependency management, claim lifecycle, and action dispatch.

**Invariants:**
- All KV writes use CAS (compare-and-swap) with bounded retry on conflict: up to 5 retries with exponential backoff (10ms/20ms/40ms/80ms/160ms). On exhaustion, heartbeat CAS failures are logged and dropped (benign); state transition CAS failures are logged as errors for investigation.
- The dispatcher always validates preconditions before state transitions — including admin commands. For example, `requeue` checks dependency state rather than blindly moving to OnDeck.
- Owner and repo names must not contain dots (validated at job creation time).

---

## Job State Machine

```
                         ┌─────────┐
              manual     │  OnIce  │  held; created with initial_state: on-ice
                         └────┬────┘  or admin requeue --target on-ice
                              │ admin requeue (dispatcher checks deps)
                              │
                         ┌────▼────┐
            auto-set on  │ Blocked │  has unresolved deps
            creation if  └────┬────┘
            deps present      │ all deps Done (dispatcher walks reverse deps)
                              │
                         ┌────▼────┐
                         │ OnDeck  │  dispatcher dispatches Forgejo Action
                         └────┬────┘
                              │ dispatch_action (CAS claim + workflow dispatch)
                         ┌────▼──────────┐
                         │ OnTheStack    │  action container running
                         └──┬─────────┬──┘
                            │         │
                      yield │         │ fail / lease expiry / timeout
                      + PR  │         │
                            │       ┌─▼──────┐
                            │       │ Failed │  reason + logs in activities
                            │       └────┬───┘
                            │            │
                            │            │ retry_count < max_retries
                            │            │ → OnDeck (auto-retry, new action)
                            │            │
                            │            │ retry_count >= max_retries
                            │            │ → stays Failed (manual requeue)
                    ┌───────▼┐           │
                    │InReview│           │
                    └─┬──┬──┬┘           │
                      │  │  │            │
   changes_requested  │  │  │ escalated  │
   (reviewer)         │  │  │ (reviewer) │
             ┌────────▼┐ │ ┌▼─────────┐ │
             │ Changes  │ │ │ Escalated │ │
             │Requested │ │ └──┬───┬───┘ │
             └────┬─────┘ │    │   │     │
                  │       │    │   │     │
    dispatcher    │       │ human  │ human
    dispatches    │       │ approves│ requests
    new action    │       │    │   │ changes
    (rework)      │       │    │   │    │
             OnTheStack   │  Done  │ Changes
             (is_rework)  │       Requested
                          │
                     approved (reviewer merges)
                          │
                       ┌──▼───┐
                       │ Done │
                       └──────┘

  Terminal states (reachable from Any via admin commands):
    Done     — admin close (walks reverse deps)
    Revoked  — admin close --revoke (dependents stay blocked)
    admin requeue → dispatcher checks deps → Blocked or OnDeck
    admin requeue --target on-ice → OnIce (from Failed)
```

### State Transition Rules

| From | To | Trigger | Action |
|------|----|---------|--------|
| (new) | Blocked | Creation with unresolved deps | Write job + deps KV (CAS), add to petgraph |
| (new) | OnDeck | Creation with no deps or all deps done | Write job + deps KV (CAS), dispatch action |
| (new) | OnIce | Creation with initial_state: on-ice | Write job KV (CAS) |
| OnIce | OnDeck or Blocked | Admin requeue | Check deps; if all Done → OnDeck, else → Blocked |
| Blocked | OnDeck | All deps Done | Update job KV (CAS), publish transition, dispatch action |
| OnDeck | OnTheStack | Dispatcher dispatches action | CAS claim (worker_id: action-{key}), dispatch Forgejo workflow |
| OnTheStack | InReview | Worker outcome: yield | Release claim, store pr_url, update job KV (CAS), dispatch review action |
| InReview | Done | Review decision: approved (PR already merged) | Update job KV (CAS), walk reverse deps |
| InReview | Escalated | Review decision: escalated | Update job KV (CAS) |
| InReview | ChangesRequested | Review decision: changes_requested | Update job KV (CAS) |
| Escalated | Done | Admin close or manual resolution | Update job KV (CAS), walk reverse deps |
| Escalated | ChangesRequested | Admin requeue | Update job KV (CAS) |
| ChangesRequested | OnTheStack | Dispatcher dispatches rework action | CAS claim, dispatch Forgejo workflow with review_feedback |
| OnTheStack | Failed | Lease expiry / job timeout / worker fail | Release claim, store reason in `chuggernaut.activities` |
| Failed | OnDeck | Auto-retry (retry_count < max_retries) | Increment retry_count, set `retry_after` = now + min(30s × 2^retry_count, 10min). Monitor detects eligible jobs and publishes advisory; dispatcher transitions to OnDeck and dispatches new action. |
| Failed | Failed | Retry exhausted | Stay; manual requeue required |
| Failed | OnDeck or Blocked | Admin requeue | Check deps; route accordingly |
| Failed | OnIce | Admin requeue --target on-ice | Put failed job on hold for later |
| Any | Done | Admin close | If claimed: release claim. Update job KV (CAS), walk reverse deps |
| Any | Revoked | Admin close --revoke | If claimed: release claim. Update job KV (CAS) (dependents stay blocked) |

### Terminal and Hold States

**Done:** Job completed successfully. Triggers reverse-dep resolution — any dependent whose deps are all Done transitions from Blocked to OnDeck.

**Revoked:** Job closed without completion. Dependents stay Blocked because `all_deps_done` requires every dep to be in state Done. Revoked is terminal.

**Escalated:** The review action could not make a decision (subprocess failure, unclear output). The job stays Escalated until manually resolved via admin commands (requeue or close).

---

## Dependency Management

### Storage

Dependencies are stored in `chuggernaut.deps` KV with both forward and reverse indexes:

```json
// Key: acme.payments.57
{
  "depends_on": ["acme.payments.40", "acme.payments.41"],
  "depended_on_by": ["acme.payments.60"]
}
```

When creating a job with deps `[40, 41]`:
1. Write forward deps for job 57 (CAS create)
2. CAS-update job 40's dep entry: read, add 57 to `depended_on_by`, write back (retry on CAS conflict)
3. CAS-update job 41's dep entry: read, add 57 to `depended_on_by`, write back (retry on CAS conflict)
4. Add edges to petgraph
5. Run cycle detection

### petgraph In-Memory DAG

The dispatcher maintains a `petgraph::graph::DiGraph` in memory:
- **Nodes:** job keys (indexed by a `HashMap<String, NodeIndex>`)
- **Edges:** `depends_on` relationships (A → B means "A depends on B")

**Rebuilt on startup:** scan all keys in `chuggernaut.deps` KV, reconstruct adjacency.

**Used for:**
- Cycle detection (DFS before adding any edge)
- Topological queries
- Reverse-dep walking on Done transitions
- Full graph serialization for the graph viewer

### Cycle Detection

Before adding a new dependency edge A → B:
1. Check if B can reach A via DFS in the current graph
2. If yes, reject the edge (return error to CLI)
3. If no, add edge to petgraph and persist to KV

### Unblock Propagation

When job X transitions to Done:
1. Read X's `depended_on_by` from `chuggernaut.deps` (or traverse petgraph reverse edges)
2. For each dependent Y:
   a. Read Y's `depends_on` list
   b. For each dep, read job state from `chuggernaut.jobs` KV
   c. If ALL deps are Done → transition Y from Blocked to OnDeck
3. Recursion is not needed: Y transitioning to OnDeck doesn't trigger further unblocking (only Done does)

---

## Claim Lifecycle

### Acquisition

```
Job reaches OnDeck → dispatcher dispatches action:
  → CAS create on chuggernaut.claims KV key for the job
    - worker_id: "action-{job_key}"
    - If no entry exists: create succeeds (revision 0)
    - If tombstone: CAS overwrites
    - If active entry: fail (already claimed)
  → On success:
    - CAS update chuggernaut.jobs KV: state → OnTheStack
    - Dispatch Forgejo Action workflow (job_key, nats_url, review_feedback)
    - Publish JobTransition{OnDeck → OnTheStack}
  → On dispatch failure:
    - Release claim, transition to Failed
```

### Heartbeat

The action worker publishes `WorkerHeartbeat` every 10 seconds. The dispatcher:
1. Reads current claim from `chuggernaut.claims` KV
2. Verifies `worker_id` matches
3. CAS update: `last_heartbeat = now`, `lease_deadline = now + lease_secs`
4. CAS collision → treat as success (claim still valid, another heartbeat won the race)

### Release

On worker outcome (yield/fail):
1. Delete claim from `chuggernaut.claims` KV (tombstone)
2. Process outcome-specific logic (state transition, retry, etc.)

### Lease Expiry

Monitor detects `now > claim.lease_deadline`:
1. Publishes `LeaseExpiredEvent` (advisory)
2. Dispatcher receives, re-reads claim from KV
3. If still expired: release claim, transition job to Failed (auto-retry may then move to OnDeck if retry_count < max_retries)
4. If renewed since: ignore (heartbeat arrived between monitor scan and dispatcher handling)

### Job Timeout

Monitor detects `now - claim.claimed_at > claim.timeout_secs`:
1. Publishes `JobTimeoutEvent` (advisory)
2. Dispatcher receives, re-reads claim from KV
3. Release claim, transition to Failed with reason "job timeout"

---

## Action Dispatch

When a job reaches OnDeck (from creation, unblock, requeue, or auto-retry), the dispatcher dispatches a Forgejo Action immediately:

1. Create claim with `worker_id = "action-{job_key}"` (CAS-create)
2. Transition job to OnTheStack
3. Dispatch `work.yml` Forgejo Actions workflow with inputs: `job_key`, `nats_url`
4. If dispatch fails (API error): release claim, transition to Failed
5. The action container starts, runs the worker binary, communicates via NATS

There is no worker registration, capability matching, or idle worker selection. All jobs are dispatched as Forgejo Actions.

### Rework Dispatch

When a job transitions to ChangesRequested:

1. Dispatcher dispatches a **new** Forgejo Action with the same `job_key`
2. Passes `review_feedback` and `is_rework=true` as additional workflow inputs
3. Creates a new claim (same `worker_id = "action-{job_key}"`)
4. The worker checks out the existing `work/{job_key}` branch and addresses the feedback
5. The existing PR auto-updates when new commits are pushed

No attempt is made to route rework to a "previous worker" — each action run is independent and ephemeral.

### Review Dispatch

When a worker yields (transition to InReview), the dispatcher also dispatches a review action:

1. Read the job's `review` level
2. Dispatch `review.yml` Forgejo Actions workflow with inputs: `job_key`, `nats_url`, `pr_url`, `review_level`
3. The review action (worker in review mode) reviews the PR, posts a review, and attempts merge if approved
4. Publishes `ReviewDecision` to NATS — dispatcher handles the decision

There is no separate reviewer process. Review is just another action type.

### Token Usage Tracking

Both work and review actions report `TokenUsage` in their outcomes. The dispatcher stores per-action records on the Job:

```json
{
  "token_usage": [
    { "action_type": "work", "token_usage": { "input_tokens": 25000, ... }, "completed_at": "..." },
    { "action_type": "review", "token_usage": { "input_tokens": 15000, ... }, "completed_at": "..." },
    { "action_type": "work", "token_usage": { "input_tokens": 10000, ... }, "completed_at": "..." }
  ]
}
```

This allows the UI to show per-action usage broken down by type, including across rework cycles.

---

## Restart Recovery

On startup:

1. **Create/verify KV buckets and streams** (idempotent)
2. **Rebuild job index:** scan all keys in `chuggernaut.jobs` KV, build in-memory `HashMap<String, Job>`
3. **Rebuild petgraph:** scan all keys in `chuggernaut.deps` KV, reconstruct DAG
4. **Repair dep indexes:** for every forward dep A → B, verify B's `depended_on_by` contains A. If not, CAS-update B's dep entry to add A. This repairs partial writes from a crash during dep creation.
5. **Resume stream consumers:** durable consumers resume from last ack position
6. **Reconcile claims against jobs:** scan all keys in `chuggernaut.claims` KV. For each active claim:
   - If the job is in `on-the-stack`: valid, keep
   - If the job is in any other state: stale claim from a mid-transition crash — delete it
   - If the job doesn't exist: delete the claim
7. **Reconcile jobs against claims:** scan jobs in `on-the-stack` state. For each:
   - If a matching claim exists: valid (action container is still running and heartbeating)
   - If no claim exists: transition to Failed (claim was lost — mid-transition crash)
8. **Monitor scan:** immediately scan `chuggernaut.claims` for any stale claims that accumulated during downtime

Action workers that were running during the restart:
- Continue heartbeating → dispatcher sees heartbeats and recognizes the claim
- If dispatcher was down long enough for lease expiry → monitor detects and fails job

---

## HTTP API

### Job Endpoints

```
GET /jobs
  Query: ?state=on-deck (optional filter)
  Returns: { "jobs": [Job] }

GET /jobs/{key}
  Returns: { "job": Job, "claim": ClaimState | null, "activities": [ActivityEntry] }

GET /jobs/{key}/deps
  Returns: { "dependencies": [Job], "all_done": bool }
```

### Journal Endpoint

```
GET /journal
  Query: ?limit=100 (optional)
  Returns: { "entries": [JournalEntry] }
```

### SSE Endpoint

```
GET /events
  Returns: SSE stream
  On connect: full snapshot (all jobs, workers, deps)
  Then: incremental updates on any KV change
```

The SSE handler watches the backing streams of `chuggernaut.jobs`, `chuggernaut.claims`, and `chuggernaut.deps` KV buckets, plus the `CHUGGERNAUT-TRANSITIONS` stream for state change events. Any mutation triggers an SSE event to all connected clients.

**Lag recovery:** If a client falls behind, the server detects via SSE delivery failure and sends a full snapshot on the next successful delivery.

### Admin Endpoints (convenience wrappers around NATS)

```
POST /jobs
  Body: CreateJobRequest
  Returns: { "key": "acme.payments.57" }

POST /jobs/{key}/requeue
  Body: { "target": "on-deck" | "on-ice" }
  Returns: 200

POST /jobs/{key}/close
  Body: { "revoke": false }
  Returns: 200

POST /jobs/{key}/channel/send
  Body: { "message": "...", "sender": "ui" }
  Returns: 200 (publishes to NATS channel inbox)
  404 if job not found
```

These HTTP endpoints call dispatcher logic directly (not via NATS round-trip). Provided for convenience (graph viewer forms, simple scripts) alongside the CLI.

---

## Configuration

| Env Var | Default | Notes |
|---------|---------|-------|
| `CHUGGERNAUT_NATS_URL` | `nats://localhost:4222` | NATS connection (dispatcher) |
| `CHUGGERNAUT_NATS_WORKER_URL` | (same as NATS_URL) | NATS URL passed to action workers. Set to `nats://host.docker.internal:4222` when workers run in Docker. |
| `CHUGGERNAUT_HTTP_LISTEN` | `0.0.0.0:8080` | HTTP API + SSE listen address |
| `CHUGGERNAUT_STATIC_DIR` | (workspace `static/`) | Directory containing `index.html`. Dispatcher panics at startup if not found. In Docker, mount as volume. |
| `CHUGGERNAUT_LEASE_SECS` | `60` | Default claim lease duration |
| `CHUGGERNAUT_DEFAULT_TIMEOUT_SECS` | `3600` | Default job timeout if not set on job |
| `CHUGGERNAUT_CAS_MAX_RETRIES` | `5` | CAS retry attempts before giving up |
| `CHUGGERNAUT_MONITOR_SCAN_INTERVAL_SECS` | `10` | Monitor scan tick interval |
| `CHUGGERNAUT_JOB_RETENTION_SECS` | `86400` | Time after terminal state before archival |
| `CHUGGERNAUT_ACTIVITY_LIMIT` | `50` | Max activity entries per job |
| `CHUGGERNAUT_REVIEW_WORKFLOW` | `review.yml` | Review action workflow file |
| `CHUGGERNAUT_REVIEW_RUNNER_LABEL` | `ubuntu-latest` | Runner label for review actions |
| `CHUGGERNAUT_REWORK_LIMIT` | `3` | Max rework cycles before escalation |
| `CHUGGERNAUT_HUMAN_LOGIN` | `you` | Human reviewer login for escalation |
