# chuggernaut — Design Document

## Overview

NATS-first workflow orchestration for AI agents. Jobs live in NATS KV. Workers communicate via NATS pub/sub. Forgejo provides git hosting and pull requests. A graph viewer is the primary human interface. No issues, no CDC, no bidirectional sync.

---

## Why chuggernaut

The original forge system worked but had a fundamental problem: **contention between Forgejo and NATS as source of truth**. Forgejo issues were jobs, labels were state, and a CDC process polled PostgreSQL to bridge changes into NATS. This created:

- Ambiguity about what's authoritative (graph DB? labels? CDC stream?)
- Fragile parsing of raw database records, HTML comments, and label conventions
- Complex bidirectional reconciliation between the sidecar and Forgejo
- Forgejo API failures blocking state transitions

chuggernaut eliminates this by treating **NATS as the single source of truth** for all workflow state. Forgejo is used only for what it's good at: git hosting and pull requests. The single-writer principle gives each actor a clear ownership boundary. No reconciliation required.

---

## Principles

1. **NATS is the single source of truth.** All workflow state lives in NATS KV buckets backed by JetStream. No external database, no label-based state encoding.

2. **Single-writer per key.** Each NATS KV key has exactly one actor that writes to it. Most buckets are owned entirely by one actor. All KV writes use CAS (compare-and-swap) for consistency.

3. **Forgejo is for git.** Forgejo hosts repositories, pull requests, code review, and CI (Forgejo Actions). It does not store workflow state. No issues are used.

4. **The graph viewer is the human interface.** All job visibility, dependency graphs, worker status, and activity timelines are served by the dispatcher's HTTP API and rendered in the graph viewer. No issue tracker needed.

5. **Workers are ephemeral.** Each worker runs inside a Forgejo Action container. The dispatcher dispatches the action directly — there is no worker registration or assignment protocol. Workers receive their job key as a workflow input and communicate via NATS (heartbeats, outcomes, MCP channel).

6. **Failures are recoverable.** NATS KV survives restarts. The dispatcher rebuilds in-memory state (petgraph, job index) from KV on startup. Stream consumers resume from their last acknowledged position.

7. **Key format constraint.** Job keys use the format `{owner}.{repo}.{seq}` and double as NATS subject tokens. Owner and repo names **must not contain dots** — the dispatcher rejects them at job creation time.

---

## Architecture

```
CLI / API
    │
    │ chuggernaut.admin.create-job (NATS)
    ▼
┌─────────────────────────────────────────────────────┐
│ Dispatcher                                           │
│                                                      │
│  Jobs KV ──── petgraph (in-memory DAG) ──── Deps KV │
│  Claims KV    Journal KV                             │
│                                                      │
│  HTTP API (graph viewer, CLI reads)                  │
│  SSE (real-time updates)                             │
│                                                      │
│  Subscriptions:                                      │
│    chuggernaut.worker.{heartbeat,outcome}                 │
│    chuggernaut.review.decision                            │
│    chuggernaut.monitor.{lease-expired,timeout,orphan,retry} │
│    chuggernaut.admin.{create-job,requeue,close-job}       │
│    chuggernaut.activity.append, chuggernaut.journal.append     │
│                                                      │
│  Publishes:                                          │
│    chuggernaut.transitions.{job_key}                      │
│                                                      │
│  Dispatches:                                         │
│    Forgejo Actions workflows (via API)               │
└──────────────────┬──────────────┬────────────────────┘
                   │              │
        ┌──────────┘
        ▼
   Action Workers (ephemeral)
   Run inside Forgejo Action containers
   Two modes: work + review
   NATS: heartbeat, outcome/decision, channel
   Forgejo: git clone/push, PRs, review, merge
```

### Monitor

Runs as a background tokio task inside the dispatcher. Scans `chuggernaut.claims` KV for lease expiry, job timeouts, and orphaned claims. Publishes advisory events — never mutates state directly. The dispatcher re-verifies before acting.

See [docs/monitor.md](docs/monitor.md) for scan behavior, orphan detection, and open design questions.

---

## Components

### Dispatcher

The single coordination point. All state transitions flow through it. This is a deliberate SPOF — the single-writer model trades HA for simplicity. The dispatcher rebuilds all in-memory state from KV in seconds; workers and the reviewer retry automatically. If HA is needed later, the single-writer design supports active-passive with leader election.

**Owns (writes to):**
- `chuggernaut.jobs` — full job records (states: `on-ice`, `blocked`, `on-deck`, `on-the-stack`, `in-review`, `escalated`, `changes-requested`, `done`, `failed`, `revoked`)
- `chuggernaut.claims` — exclusive claim state (CAS)
- `chuggernaut.deps` — dependency edges (forward + reverse indexes)
- `chuggernaut.counters` — per-repo job sequence numbers
- `chuggernaut.activities` — job activity logs (separate from job records to avoid CAS contention)
- `chuggernaut.journal` — dispatcher action log
- `CHUGGERNAUT-TRANSITIONS` — job state change event stream

**In-memory state:**
- petgraph DAG — rebuilt from `chuggernaut.deps` KV on startup, maintained incrementally

**HTTP API:** Serves the graph viewer and CLI read operations. SSE endpoint watches KV changes for real-time updates. `GET /api.json` serves a self-describing manifest (routes + SSE events + JSON Schema) for client codegen. `GET /schema` serves the raw JSON Schema.

See [docs/dispatcher.md](docs/dispatcher.md) for state machine, dep management, and claim lifecycle.

### Action Workers

Ephemeral executors that run inside Forgejo Action containers. The dispatcher dispatches a workflow directly — there is no worker registration or assignment protocol.

**How it works:**
1. Job reaches OnDeck → dispatcher creates claim (`worker_id = "action-{job_key}"`) and dispatches Forgejo Action
2. Worker binary starts inside the action container
3. Clones repo, checks out `work/{job_key}` branch, launches Claude as subprocess
4. Bridges Claude's stdin/stdout via MCP channel (NATS ↔ JSON-RPC)
5. Heartbeats every 10s to keep the claim alive
6. On completion: commits, pushes, finds/creates PR, reports `Yield` or `Fail`
7. Container exits

**Publishes to:**
- `chuggernaut.worker.heartbeat` — lease renewal
- `chuggernaut.worker.outcome` — Yield (pr_url) or Fail (reason, logs)
- `chuggernaut.activity.append` — progress updates (fire-and-forget)
- `chuggernaut.channel.{job_key}.outbox` — MCP channel messages

**Rework:** When a reviewer requests changes, the dispatcher dispatches a new action with the same `job_key` plus `review_feedback`. The worker checks out the existing branch and PR, addresses the feedback, and yields again.

**Idempotent execution:** Workers check for an existing remote branch and PR for the job key and continue from where the previous attempt left off.

See [docs/worker-protocol.md](docs/worker-protocol.md) for the full protocol.

### Review (via Action Workers)

Review is handled by the same worker binary in review mode (`--mode review`), dispatched as a Forgejo Action just like work. When a job transitions to InReview, the dispatcher dispatches a `review.yml` workflow. The review action:

1. Clones repo, checks out the PR branch
2. Gets the diff and runs Claude to review it
3. Posts a PR review to Forgejo (APPROVED or REQUEST_CHANGES)
4. If approved, attempts to merge the PR (with retries for contention)
5. Publishes `ReviewDecision` to NATS

There is no separate reviewer process. The dispatcher handles rework counting, escalation transitions, and merge queue coordination is done inline by the review action with Forgejo API retries.

See [docs/reviewer.md](docs/reviewer.md) for the review lifecycle.

### Graph Viewer

Single-page web app. Primary human interface.

- Real-time DAG of all jobs with dependency edges
- Job detail: state, priority, activities, PR link, channel status
- Active claims (running actions)
- Dispatcher journal
Reads from the dispatcher's HTTP API. SSE-driven — full snapshot on connect, incremental updates thereafter.

**Decoupled frontend:** The graph viewer (or any frontend) does not depend on chuggernaut Rust crates. The dispatcher exposes `GET /api.json` — a self-describing API manifest containing the route table, SSE event definitions, and the full JSON Schema for all types. A frontend can fetch this at build time to generate typed API clients and models in any language. See the `api_specs()` function in `http.rs` for the authoritative route list.

### CLI

Command-line interface for job creation, listing, admin, and agent interaction.

```
chuggernaut create --repo owner/repo --title "..." --body "..." [--deps 1,2,3] [--priority N]
chuggernaut jobs [--state on-deck]
chuggernaut show {job_key}
chuggernaut requeue {job_key} [--target on-deck|on-ice]
chuggernaut close {job_key} [--revoke]
chuggernaut channel send {job_key} "message"
chuggernaut channel watch {job_key}
chuggernaut channel status {job_key}
chuggernaut seed {repo} {fixture.json} [--var KEY=VALUE,...]
```

The `seed` command supports `--var` for template interpolation — `{{KEY}}` placeholders in fixture job bodies are replaced at seed time. This allows fixtures to reference environment-specific values (e.g. `--var DISPATCHER_URL=https://example.com`).

---

## Known Limitations (v1)

- **No authentication or authorization.** The HTTP API, NATS subjects, and CLI have no auth. NATS supports account-based auth and subject-level permissions natively; the HTTP API can add bearer tokens. Out of scope for v1.
- **Basic token usage metrics.** Each action (work and review) reports `TokenUsage` (input, output, cache read/write tokens) in its outcome. The dispatcher stores per-action `ActionTokenRecord` entries on the Job, so you can see token usage per action invocation and broken down by work vs review. No Prometheus endpoint or OpenTelemetry yet — out of scope for v1.
- **No cross-repo dependencies.** `depends_on` uses sequence numbers within the same repo. Cross-repo workflows can be coordinated externally but are not modeled in the dep graph.
- **Single-node NATS.** No clustering or replication. NATS supports clustered JetStream natively; out of scope for v1.

---

## Infrastructure

| Service | Purpose |
|---------|---------|
| **NATS with JetStream** | All state persistence (KV) + messaging (streams/pub-sub). File-backed. |
| **Forgejo** | Git hosting, pull requests, code review, Forgejo Actions (CI). |

That's it. No PostgreSQL polling, no RocksDB, no external graph database.

**Shared env vars:** `CHUGGERNAUT_NATS_URL` (default `nats://localhost:4222`) and `CHUGGERNAUT_FORGEJO_URL` (required) are used by all components. Per-component configuration is documented in each companion doc.

**Docker Compose:**
```yaml
services:
  nats:          # JetStream, file-backed storage
  forgejo:       # git + PRs + Actions
  dispatcher:    # coordinator + HTTP API
  runner:        # shared Forgejo Actions runner
```

Workers run inside Forgejo Action containers — no separate worker processes to manage.

---

## Crate Structure

```
chuggernaut/
  Cargo.toml                      # workspace
  crates/
    types/                        # chuggernaut-types: Job, JobState, ClaimState, messages
    dispatcher/                   # chuggernaut-dispatcher: coordinator binary
    worker/                       # chuggernaut-worker: action worker binary (work + review modes)
    cli/                          # chuggernaut-cli: CLI binary
    channel/                      # chuggernaut-channel: MCP channel bridge
    nats/                         # chuggernaut-nats: typed NATS client
    forgejo-api/                  # Forgejo REST client
    test-utils/                   # chuggernaut-test-utils: testcontainers + cleanup
  static/                         # graph viewer SPA
  docs/                           # component specs
  scripts/                        # init.sh
  docker-compose.yml
```

---

## Key Differences from forge v1

| Aspect | v1 | v2 |
|--------|----|----|
| Source of truth | Ambiguous (Forgejo + IndraDB + NATS KV) | NATS KV only |
| Forgejo | Issues = jobs, labels = state, CDC polls PostgreSQL | Git + PRs only, no issues |
| Job creation | Forgejo issue → CDC → sidecar | CLI/API → dispatcher → NATS KV |
| PR linking | `Closes #N` auto-closes Forgejo issue | Job key in PR title, branch convention |
| Dependencies | HTML comments in issue body | NATS KV + petgraph in-memory |
| State sync | Bidirectional (CDC ↔ sidecar ↔ Forgejo) | None needed (single source) |
| Done detection | CDC detects merged PR from PostgreSQL | Review action merges and reports directly |
| Human interface | Forgejo issues + graph viewer | Graph viewer only |
| Persistence | RocksDB + NATS KV + PostgreSQL | NATS JetStream only |
| Processes | CDC + Sidecar + Reviewer | Dispatcher only (review via actions) |
| Graph queries | IndraDB/RocksDB | petgraph in-memory + NATS KV persistence |

---

## Companion Documents

- [NATS Schema](docs/nats-schema.md) — KV buckets, streams, subjects, payload types
- [Dispatcher](docs/dispatcher.md) — state machine, deps, claims, assignment, restart recovery
- [Worker Protocol](docs/worker-protocol.md) — action dispatch, heartbeat, outcomes, MCP channel
- [Reviewer](docs/reviewer.md) — review lifecycle, merge queue, rework, done detection
- [Monitor](docs/monitor.md) — lease scanning, orphan detection, job archival
- [Testing](docs/testing.md) — unit, integration, and end-to-end testing strategy
