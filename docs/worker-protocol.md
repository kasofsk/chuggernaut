# Worker Protocol

Workers run inside CI action containers (Forgejo Actions or GitHub Actions). The worker binary (`chuggernaut-worker`) operates in two modes:

- **Work mode** (`--mode work`, default): dispatched when a job reaches OnDeck. Clones repo, runs Claude to do work, commits/pushes, creates PR, reports `WorkerOutcome`.
- **Review mode** (`--mode review`): dispatched when a job enters InReview. Clones repo, checks out PR branch, runs Claude to review the diff, posts PR review, attempts merge, reports `ReviewDecision`.

Both modes use the same heartbeat, NATS connection, and subprocess management infrastructure. There is no worker registration, idle loop, or assignment protocol. The dispatcher creates a claim and dispatches the action directly.

---

## Lifecycle

```
Job reaches OnDeck
  │
  ▼
┌──────────────────────────────┐
│ Dispatcher: dispatch_action  │
│  1. Create claim             │
│     worker_id: action-{key}  │
│  2. Transition → OnTheStack  │
│  3. Dispatch CI action  │
│     inputs: job_key, nats_url│
│     + review_feedback (rework)│
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ CI actions runner       │
│  starts container with       │
│  chuggernaut-worker binary   │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ Worker (inside container)    │
│                              │
│  1. Clone repo, checkout     │
│     branch work/{job_key}    │
│  2. Start heartbeat loop     │
│     (every 10s via NATS)     │
│  3. Launch Claude subprocess │
│  4. Bridge MCP ↔ NATS        │
│  5. Wait for subprocess exit │
│  6. Commit, push, find/create│
│     PR                       │
│  7. Publish WorkerOutcome    │
│     (Yield or Fail)          │
│  8. Exit                     │
└──────────────────────────────┘
```

---

## Action Dispatch

When a job reaches OnDeck, the dispatcher:

1. Creates a claim with `worker_id = "action-{job_key}"` (CAS-create for exclusivity)
2. Transitions the job to OnTheStack
3. Dispatches the `work.yml` CI actions workflow via API with inputs:
   - `job_key` — job identifier
   - `nats_url` — NATS server address
   - `review_feedback` — (optional) reviewer feedback for rework assignments
   - `is_rework` — (optional) flag indicating this is a rework cycle
4. If the dispatch fails (404/422): releases claim, transitions job to Failed

The claim enables the monitor to track the action via normal lease expiry and job timeout scans. The synthetic `worker_id` ties heartbeats and outcomes back to the correct claim.

### Rework Dispatch

When a job transitions to ChangesRequested (reviewer requests changes):

1. Dispatcher dispatches a **new** CI action with the same `job_key`
2. Passes `review_feedback` and `is_rework=true` as additional workflow inputs
3. The worker checks out the existing `work/{job_key}` branch (already has prior work)
4. The existing PR auto-updates when the worker pushes new commits

No attempt is made to route rework to a "previous worker" — each action run is independent and ephemeral.

---

## Git Workflow

1. Read repo from job metadata (passed via NATS or workflow inputs)
2. `git clone https://{git_url}/{repo}` (credential helper provides auth)
3. Check if branch `work/{job_key}` already exists on remote:
   - If yes: checkout and pull (continuation of previous attempt or rework)
   - If no: create branch (fresh start)
4. Launch Claude subprocess to do work
5. Commit, push
6. Check if a PR already exists for branch `work/{job_key}`:
   - If yes: push updates the existing PR automatically
   - If no: open PR via git provider API:
     - Title: `[acme.payments.57] Add retry logic to payment service`
     - Body: Job description + `Job: acme.payments.57`
     - Head: `work/acme.payments.57`
     - Base: `main` (or default branch)

This idempotent approach handles rework cycles and retries — the worker always picks up where the previous run left off.

### Rework

When `is_rework` is set:

1. Checkout existing branch `work/{job_key}` (already exists from first pass)
2. Pull latest from remote
3. Pass `review_feedback` to Claude as additional context
4. Address feedback, commit, push
5. The existing PR auto-updates (same branch)

---

## Heartbeat

```
Worker → chuggernaut.worker.heartbeat (every 10 seconds)

Payload: WorkerHeartbeat {
  worker_id: "action-acme.payments.57",
  job_key: "acme.payments.57"
}
```

The dispatcher CAS-updates the claim's `last_heartbeat` and `lease_deadline`.

### Failure Detection

The worker tracks consecutive NATS publish failures:
- 1-2 failures: log warning, continue
- 3+ failures: assume NATS connection lost, exit with error

The monitor detects lease expiry from the dispatcher side and transitions the job to Failed.

---

## Outcome

### Work Mode

```
Worker → chuggernaut.worker.outcome

Payload: WorkerOutcome {
  worker_id: "action-acme.payments.57",
  job_key: "acme.payments.57",
  outcome: <one of below>,
  token_usage: { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens } | null
}
```

| Type | Fields | When | Dispatcher Action |
|------|--------|------|-------------------|
| `Yield` | `pr_url` | PR opened/updated, ready for review | Release claim → InReview → dispatch review action |
| `Fail` | `reason`, `logs` | Unrecoverable error | Release claim → Failed (may auto-retry) |

If the worker cannot fully complete the task, it should still `Yield` with a PR containing partial work. The review action will evaluate it and either approve, request changes, or escalate to a human.

### Review Mode

```
Worker → chuggernaut.review.decision

Payload: ReviewDecision {
  job_key: "acme.payments.57",
  decision: <one of below>,
  pr_url: "https://git.example.com/acme/payments/pulls/3",
  token_usage: { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens } | null
}
```

| Decision | When | Dispatcher Action |
|----------|------|-------------------|
| `approved` | PR merged successfully | → Done, unblock dependents |
| `changes_requested { feedback }` | Review found issues or merge conflict | → ChangesRequested, dispatch rework |
| `escalated { reviewer_login }` | Subprocess failed or decision unclear | → Escalated |

### Token Usage

Both work and review outcomes include an optional `token_usage` field. The dispatcher stores each action's usage as an `ActionTokenRecord` on the Job, allowing per-action breakdown of work vs review tokens across rework cycles.

If the container is killed or crashes without reporting an outcome, the monitor detects lease expiry and transitions the job to Failed.

---

## Activity Reporting

Workers can publish progress updates:

```
Worker → chuggernaut.activity.append

Payload: ActivityAppend {
  job_key: "acme.payments.57",
  entry: {
    timestamp: "...",
    kind: "progress",
    message: "Implementing exponential backoff for API retries"
  }
}
```

These are fire-and-forget. The dispatcher appends to `chuggernaut.activities` KV (separate from the job record to avoid CAS contention with state transitions). Capped at 50 entries per job.

---

## MCP Channel

The worker acts as an MCP server, bridging between Claude's stdin/stdout and NATS:

1. Spawns the command (`claude` or `mock-claude`) as a subprocess with piped stdin/stdout
2. Reads JSON-RPC requests from the child's stdout
3. Dispatches through `chuggernaut_channel::handle_message` (initialize, tools/list, tools/call)
4. Writes JSON-RPC responses back to the child's stdin
5. Forwards NATS inbox notifications to the child (in channel mode)

### MCP Tools Exposed to Claude

- `channel_check` — drain pending inbox messages (poll mode)
- `channel_send` — publish a `ChannelMessage` to the NATS outbox
- `reply` — reply to a specific message (channel mode, push notifications)
- `update_status` — write `ChannelStatus` to the `chuggernaut_channels` KV bucket

### Channel Subjects

```
chuggernaut.channel.{job_key}.inbox   — messages TO the worker (from CLI/orchestrator)
chuggernaut.channel.{job_key}.outbox  — messages FROM the worker (to CLI/orchestrator)
```

---

## Runner Environment

The worker binary is distributed via the runner environment Docker image (`chuggernaut-runner-env`):

- Built from `Dockerfile.runner-env`
- Contains: `chuggernaut-worker` binary, `mock-claude` (for testing), git, standard tools
- Used by the CI actions runner as the container image for job execution

### Workflow Definition (`action/work.yml`)

```yaml
name: chuggernaut-work
on:
  workflow_dispatch:
    inputs:
      job_key: { required: true, type: string }
      nats_url: { required: true, type: string }
      review_feedback: { required: false, type: string }
      is_rework: { required: false, type: string }
jobs:
  work:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: chuggernaut-worker --job-key "${{ inputs.job_key }}"
               --nats-url "${{ inputs.nats_url }}"
               --git-url "${{ github.server_url }}"
        env:
          CHUGGERNAUT_GIT_TOKEN: ${{ secrets.CHUGGERNAUT_WORKER_TOKEN }}
          CHUGGERNAUT_COMMAND: claude
          CHUGGERNAUT_REVIEW_FEEDBACK: ${{ inputs.review_feedback }}
          CHUGGERNAUT_IS_REWORK: ${{ inputs.is_rework }}
```

---

## Review Mode Workflow Definition (`action/review.yml`)

```yaml
name: chuggernaut-review
on:
  workflow_dispatch:
    inputs:
      job_key: { required: true, type: string }
      nats_url: { required: true, type: string }
      pr_url: { required: true, type: string }
      review_level: { required: false, type: string, default: "high" }
jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: chuggernaut-worker --mode review --job-key "${{ inputs.job_key }}"
               --nats-url "${{ inputs.nats_url }}" --git-url "${{ github.server_url }}"
               --pr-url "${{ inputs.pr_url }}" --review-level "${{ inputs.review_level }}"
        env:
          CHUGGERNAUT_GIT_TOKEN: ${{ secrets.CHUGGERNAUT_REVIEWER_TOKEN }}
          CHUGGERNAUT_COMMAND: claude
```

---

## Configuration

| Env Var | Default | Notes |
|---------|---------|-------|
| `CHUGGERNAUT_JOB_KEY` | (required) | Job identifier (passed as workflow input) |
| `CHUGGERNAUT_NATS_URL` | `nats://localhost:4222` | NATS connection |
| `CHUGGERNAUT_GIT_URL` | (required) | Git provider base URL (falls back to `CHUGGERNAUT_FORGEJO_URL`) |
| `CHUGGERNAUT_GIT_TOKEN` | (required) | Git provider API token (falls back to `CHUGGERNAUT_FORGEJO_TOKEN`) |
| `CHUGGERNAUT_COMMAND` | `claude` | Subprocess command to run |
| `CHUGGERNAUT_COMMAND_ARGS` | (none) | Additional args for subprocess |
| `CHUGGERNAUT_HEARTBEAT_INTERVAL_SECS` | `10` | Heartbeat period |
| `CHUGGERNAUT_CHANNEL_MODE` | `false` | Enable push notifications vs poll mode |
| `CHUGGERNAUT_WORKDIR` | `/tmp/chuggernaut-work` | Working directory for git clone |
| `CHUGGERNAUT_REVIEW_FEEDBACK` | (none) | Reviewer feedback for rework cycles |
| `CHUGGERNAUT_IS_REWORK` | (none) | Flag indicating rework cycle |
| `CHUGGERNAUT_MODE` | `work` | Operating mode: `work` or `review` |
| `CHUGGERNAUT_PR_URL` | (none) | PR URL to review (review mode only) |
| `CHUGGERNAUT_REVIEW_LEVEL` | `high` | Review level: low/medium/high (review mode only) |
