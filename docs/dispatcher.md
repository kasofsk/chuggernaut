# Dispatcher

The dispatcher is the single coordinator in forge2. It owns all job state transitions, dependency management, claim lifecycle, and worker assignment.

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
                         │ OnDeck  │  claimable by dispatcher
                         └────┬────┘
                              │ dispatcher assigns (CAS on forge2.claims)
                         ┌────▼──────────┐
                         │ OnTheStack    │  exclusively held by one worker
                         └──┬────┬────┬──┘
                            │    │    │
             worker asks    │    │    │ fail / lease expiry / timeout
             for help       │    │    │
              ┌─────────────▼┐   │  ┌─▼──────┐
              │  NeedsHelp   │   │  │ Failed │  reason + logs in activities
              └──────┬───────┘   │  └────┬───┘
    claim held;      │           │       │
    heartbeats       │ human     │       │ retry_count < max_retries
    continue         │ responds  │       │ → OnDeck (auto-retry)
                     │ → back to │       │
                     │ OnTheStack│       │ retry_count >= max_retries
                     │           │       │ → stays Failed (manual requeue)
                     │     yield │       │
                     │     + PR  │       │
                    ┌▼───────────▼┐      │
                    │  InReview   │      │
                    └─┬──┬──┬────┘      │
                      │  │  │           │
   changes_requested  │  │  │ escalated │
   (reviewer)         │  │  │ (reviewer)│
             ┌────────▼┐ │ ┌▼─────────┐│
             │ Changes  │ │ │ Escalated ││
             │Requested │ │ └──┬───┬───┘│
             └────┬─────┘ │    │   │    │
                  │       │    │   │    │
    dispatcher    │       │ human  │ human
    routes to     │       │ approves│ requests
    original      │       │    │   │ changes
    worker        │       │    │   │    │
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
```

### State Transition Rules

| From | To | Trigger | Action |
|------|----|---------|--------|
| (new) | Blocked | Creation with unresolved deps | Write job + deps KV (CAS), add to petgraph |
| (new) | OnDeck | Creation with no deps or all deps done | Write job + deps KV (CAS), try assign |
| (new) | OnIce | Creation with initial_state: on-ice | Write job KV (CAS) |
| OnIce | OnDeck or Blocked | Admin requeue | Check deps; if all Done → OnDeck, else → Blocked |
| Blocked | OnDeck | All deps Done | Update job KV (CAS), publish transition, try assign |
| OnDeck | OnTheStack | Dispatcher assigns worker | CAS claim, update job + worker KV |
| OnTheStack | NeedsHelp | Worker publishes HelpRequest | Update job KV (CAS), store reason in `forge2.activities` |
| NeedsHelp | OnTheStack | Human responds via CLI | Update job KV (CAS), deliver response to worker |
| OnTheStack | InReview | Worker outcome: yield | Release claim, store pr_url, update job KV (CAS) |
| InReview | Done | Reviewer decision: approved + merge | Update job KV (CAS), walk reverse deps |
| InReview | Escalated | Reviewer decision: escalated | Update job KV (CAS), add human reviewer to PR |
| InReview | ChangesRequested | Reviewer decision: changes_requested | Update job KV (CAS), route to worker |
| Escalated | Done | Reviewer detects human approval, merges | Update job KV (CAS), walk reverse deps |
| Escalated | ChangesRequested | Reviewer detects human requests changes | Update job KV (CAS), route to worker |
| ChangesRequested | OnTheStack | Dispatcher assigns rework | CAS claim, update job + worker KV |
| OnTheStack | OnDeck | Worker outcome: abandon | Release claim, add (job, worker) to `forge2.abandon-blacklist` |
| OnTheStack | Failed | Lease expiry / job timeout / worker fail | Release claim, store reason in `forge2.activities` |
| Failed | OnDeck | Auto-retry (retry_count < max_retries) | Increment retry_count, set `retry_after` = now + min(30s × 2^retry_count, 10min). Monitor detects eligible jobs and publishes advisory; dispatcher transitions to OnDeck. |
| Failed | Failed | Retry exhausted | Stay; manual requeue required |
| Failed | OnDeck or Blocked | Admin requeue | Check deps; route accordingly |
| Any | Done | Admin close | If claimed: release claim, preempt worker. Update job KV (CAS), walk reverse deps |
| Any | Revoked | Admin close --revoke | If claimed: release claim, preempt worker. Update job KV (CAS) (dependents stay blocked) |

### Terminal and Hold States

**Done:** Job completed successfully. Triggers reverse-dep resolution — any dependent whose deps are all Done transitions from Blocked to OnDeck.

**Revoked:** Job closed without completion. Dependents stay Blocked because `all_deps_done` requires every dep to be in state Done. Revoked is terminal.

**Escalated:** PR has been assigned to a human reviewer. The reviewer polls Forgejo for the human's decision (approve or request changes) at a configurable interval (default 30s). The job stays Escalated until the human acts. The reviewer handles the merge via the merge queue — the human approves, but doesn't merge directly.

---

## Dependency Management

### Storage

Dependencies are stored in `forge2.deps` KV with both forward and reverse indexes:

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

**Rebuilt on startup:** scan all keys in `forge2.deps` KV, reconstruct adjacency.

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
1. Read X's `depended_on_by` from `forge2.deps` (or traverse petgraph reverse edges)
2. For each dependent Y:
   a. Read Y's `depends_on` list
   b. For each dep, read job state from `forge2.jobs` KV
   c. If ALL deps are Done → transition Y from Blocked to OnDeck
3. Recursion is not needed: Y transitioning to OnDeck doesn't trigger further unblocking (only Done does)

---

## Claim Lifecycle

### Acquisition

```
Dispatcher finds idle worker matching job requirements
  → CAS create on forge2.claims KV key for the job
    - If no entry exists: create succeeds (revision 0)
    - If tombstone: CAS overwrites
    - If active entry: fail (already claimed)
  → On success:
    - CAS update forge2.jobs KV: state → OnTheStack, last_worker_id → worker_id
    - CAS update forge2.workers KV: state → Busy, current_job → job_key
    - Publish Assignment to forge2.dispatch.assign.{worker_id}
    - Publish JobTransition{OnDeck → OnTheStack}
```

### Heartbeat

Workers publish `WorkerHeartbeat` every 10 seconds. The dispatcher:
1. Reads current claim from `forge2.claims` KV
2. Verifies `worker_id` matches
3. CAS update: `last_heartbeat = now`, `lease_deadline = now + lease_secs`
4. CAS collision → treat as success (claim still valid, another heartbeat won the race)

### Release

On worker outcome (yield/fail/abandon):
1. Delete claim from `forge2.claims` KV (tombstone)
2. Process outcome-specific logic (state transition, retry, etc.)
3. Worker registry is NOT updated here — the worker remains marked as Busy until it publishes its next `IdleEvent`, at which point the dispatcher sets state → Idle, current_job → null

### Lease Expiry

Monitor detects `now > claim.lease_deadline`:
1. Publishes `LeaseExpiredEvent` (advisory)
2. Dispatcher receives, re-reads claim from KV
3. If still expired: release claim, delete worker from `forge2.workers` KV (prune), transition job to Failed (auto-retry may then move to OnDeck if retry_count < max_retries)
4. If renewed since: ignore (heartbeat arrived between monitor scan and dispatcher handling)

The worker entry is pruned rather than set to idle — the worker is absent or misbehaving. If the process is still alive (e.g., network partition), it will re-register within 15s and get a fresh entry.

### Job Timeout

Monitor detects `now - claim.claimed_at > claim.timeout_secs`:
1. Publishes `JobTimeoutEvent` (advisory)
2. Dispatcher receives, re-reads claim from KV
3. Release claim, delete worker from `forge2.workers` KV (prune), transition to Failed with reason "job timeout"

---

## Worker Assignment

### Algorithm

When a job reaches OnDeck (from creation, unblock, requeue, or auto-retry):

1. Get all idle workers from `forge2.workers` KV (or in-memory registry)
2. Filter by job requirements:
   - `capabilities`: worker must have all required capabilities
   - `worker_type`: if set on job, worker must match
   - `platform`: if set on job, worker must match
3. Filter by abandon blacklist: skip workers in `forge2.abandon-blacklist` for this job (blacklist is not checked for rework routing — see Rework Routing)
4. Sort by: longest idle time (fairness)
5. Attempt CAS claim with first matching worker
6. If claim fails (race): try next worker
7. If no workers available: job stays OnDeck (will be assigned when a worker idles)

### Preemption

When a high-priority job reaches OnDeck and no idle workers are available:

1. Find workers busy with lower-priority jobs
2. Select the worker with the lowest-priority current job
3. Publish `PreemptNotice` to `forge2.dispatch.preempt.{worker_id}`
4. Worker cancels execution, reports `Outcome::Abandon`
5. Dispatcher requeues abandoned job to OnDeck
6. Dispatcher assigns the high-priority job to the now-idle worker

### Rework Routing

When a job transitions to ChangesRequested:

1. Read `job.last_worker_id` (set when the job was assigned to OnTheStack)
2. Check if worker exists and is idle in `forge2.workers` KV
3. If idle: assign immediately with `is_rework: true` + review feedback (bypasses abandon blacklist)
4. If busy: write to `forge2.pending-reworks` KV (includes worker_id + review feedback)
5. If worker is gone (pruned): fall back to normal assignment with `is_rework: true` to any capable idle worker
6. When the worker next publishes `IdleEvent`:
   - Dispatcher checks `forge2.pending-reworks` before normal assignment
   - If pending rework found: assign with `is_rework: true` + stored feedback

**On worker prune:** When the dispatcher prunes a worker (lease expiry, job timeout), it scans `forge2.pending-reworks` for entries pointing to that worker. Any found are reassigned to a capable idle worker, or the job stays in ChangesRequested until one becomes available.

---

## Restart Recovery

On startup:

1. **Create/verify KV buckets and streams** (idempotent)
2. **Rebuild job index:** scan all keys in `forge2.jobs` KV, build in-memory `HashMap<String, Job>`
3. **Rebuild petgraph:** scan all keys in `forge2.deps` KV, reconstruct DAG
4. **Repair dep indexes:** for every forward dep A → B, verify B's `depended_on_by` contains A. If not, CAS-update B's dep entry to add A. This repairs partial writes from a crash during dep creation.
5. **Rebuild worker registry:** scan `forge2.workers` KV (partially stale — workers re-register within 15s)
6. **Resume stream consumers:** durable consumers resume from last ack position
7. **Reconcile claims against jobs:** scan all keys in `forge2.claims` KV. For each active claim:
   - If the job is in `on-the-stack` or `needs-help`: valid, keep
   - If the job is in any other state: stale claim from a mid-transition crash — delete it
   - If the job doesn't exist: delete the claim
8. **Reconcile jobs against claims:** scan jobs in `on-the-stack` or `needs-help` state. For each:
   - If a matching claim exists: valid
   - If no claim exists: transition to Failed (claim was lost — mid-transition crash)
9. **Scan pending-reworks:** for each entry, verify the target worker exists in `forge2.workers`. If not, clear the entry so the rework can be reassigned to another worker.
10. **Monitor scan:** immediately scan `forge2.claims` for any stale claims that accumulated during downtime

Workers that were running during the restart:
- Continue heartbeating → dispatcher sees heartbeats and recognizes the claim
- If dispatcher was down long enough for lease expiry → monitor detects, prunes worker, fails job
- Workers re-register every 15s → registry rebuilds quickly

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

### Worker Endpoints

```
GET /workers
  Returns: { "workers": [WorkerInfo] }
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

The SSE handler watches the backing streams of `forge2.jobs`, `forge2.workers`, `forge2.claims`, and `forge2.deps` KV buckets, plus the `FORGE2-TRANSITIONS` stream for state change events. Any mutation triggers an SSE event to all connected clients.

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
```

These HTTP endpoints publish the corresponding NATS admin messages internally. Provided for convenience (graph viewer forms, simple scripts) alongside the CLI.

---

## Configuration

| Env Var | Default | Notes |
|---------|---------|-------|
| `FORGE2_NATS_URL` | `nats://localhost:4222` | NATS connection |
| `FORGE2_HTTP_LISTEN` | `0.0.0.0:8080` | HTTP API + SSE listen address |
| `FORGE2_LEASE_SECS` | `60` | Default claim lease duration |
| `FORGE2_DEFAULT_TIMEOUT_SECS` | `3600` | Default job timeout if not set on job |
| `FORGE2_CAS_MAX_RETRIES` | `5` | CAS retry attempts before giving up |
| `FORGE2_MONITOR_SCAN_INTERVAL_SECS` | `10` | Monitor scan tick interval |
| `FORGE2_JOB_RETENTION_SECS` | `86400` | Time after terminal state before archival |
| `FORGE2_ACTIVITY_LIMIT` | `50` | Max activity entries per job |
| `FORGE2_BLACKLIST_TTL_SECS` | `3600` | Abandon blacklist duration per (job, worker) pair |
