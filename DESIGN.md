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

2. **Single-writer per key.** Each NATS KV key has exactly one actor that writes to it. Most buckets are owned entirely by one actor. Where multiple actors write to the same bucket (e.g., `chuggernaut.sessions`), each writes only the key for its current job with no cross-key contention. All KV writes use CAS (compare-and-swap) for consistency.

3. **Forgejo is for git.** Forgejo hosts repositories, pull requests, code review, and CI (Forgejo Actions). It does not store workflow state. No issues are used.

4. **The graph viewer is the human interface.** All job visibility, dependency graphs, worker status, and activity timelines are served by the dispatcher's HTTP API and rendered in the graph viewer. No issue tracker needed.

5. **Workers are stateless executors.** They receive everything they need in the assignment payload (full job content from NATS KV). Their only external dependency during execution is Forgejo for git operations.

6. **Failures are recoverable.** NATS KV survives restarts. The dispatcher rebuilds in-memory state (petgraph, worker registry) from KV on startup. Stream consumers resume from their last acknowledged position.

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
│  Claims KV    Workers KV    Journal KV               │
│                                                      │
│  HTTP API (graph viewer, CLI reads)                  │
│  SSE (real-time updates)                             │
│                                                      │
│  Subscriptions:                                      │
│    chuggernaut.worker.{register,idle,heartbeat,outcome,unregister} │
│    chuggernaut.review.decision                            │
│    chuggernaut.interact.help, chuggernaut.interact.respond.{job_key} │
│    chuggernaut.monitor.{lease-expired,timeout,orphan,retry} │
│    chuggernaut.admin.{create-job,requeue,close-job}       │
│    chuggernaut.activity.append, chuggernaut.journal.append     │
│                                                      │
│  Publishes:                                          │
│    chuggernaut.transitions.{job_key}                      │
│    chuggernaut.dispatch.assign.{worker_id}                │
│    chuggernaut.dispatch.preempt.{worker_id}               │
│    chuggernaut.interact.deliver.{worker_id}               │
└──────────────────┬──────────────┬────────────────────┘
                   │              │
        ┌──────────┘              └──────────┐
        ▼                                    ▼
   Workers (N)                          Reviewer
   NATS: events, outcomes               NATS: transitions, decisions
   Forgejo: git clone/push, PRs         Forgejo: PR review, merge
```

### Monitor

Runs as a background tokio task inside the dispatcher. Scans `chuggernaut.claims` KV for lease expiry, job timeouts, and orphaned claims. Publishes advisory events — never mutates state directly. The dispatcher re-verifies before acting.

See [docs/monitor.md](docs/monitor.md) for scan behavior, orphan detection, and open design questions.

---

## Components

### Dispatcher

The single coordination point. All state transitions flow through it. This is a deliberate SPOF — the single-writer model trades HA for simplicity. The dispatcher rebuilds all in-memory state from KV in seconds; workers and the reviewer retry automatically. If HA is needed later, the single-writer design supports active-passive with leader election.

**Owns (writes to):**
- `chuggernaut.jobs` — full job records (states: `on-ice`, `blocked`, `on-deck`, `on-the-stack`, `needs-help`, `in-review`, `escalated`, `changes-requested`, `done`, `failed`, `revoked`)
- `chuggernaut.claims` — exclusive claim state (CAS)
- `chuggernaut.deps` — dependency edges (forward + reverse indexes)
- `chuggernaut.workers` — worker registry
- `chuggernaut.counters` — per-repo job sequence numbers
- `chuggernaut.activities` — job activity logs (separate from job records to avoid CAS contention)
- `chuggernaut.journal` — dispatcher action log
- `chuggernaut.pending-reworks` — deferred rework routing
- `chuggernaut.abandon-blacklist` — prevent assign-abandon loops
- `CHUGGERNAUT-TRANSITIONS` — job state change event stream

**In-memory state:**
- petgraph DAG — rebuilt from `chuggernaut.deps` KV on startup, maintained incrementally
- Worker registry — rebuilt from `chuggernaut.workers` KV + re-announcements

**HTTP API:** Serves the graph viewer and CLI read operations. SSE endpoint watches KV changes for real-time updates.

See [docs/dispatcher.md](docs/dispatcher.md) for state machine, dep management, and claim lifecycle.

### Workers

Stateless executors. Receive assignments via NATS, do work via Forgejo git/API, report outcomes via NATS.

**Writes to (KV):**
- `chuggernaut.sessions` — interactive session metadata (own keys only)

**Publishes to:**
- `chuggernaut.activity.append` — progress updates (fire-and-forget)
- Worker event subjects — register, idle, heartbeat, outcome, unregister

**Three modes:**
- **Sim** — creates branch, commits file, opens PR, yields. For testing.
- **Action** — dispatches a Forgejo Actions workflow, polls until complete, yields.
- **Interactive** — Claude Code agent in tmux/ttyd. Supports peek/attach/detach and help requests.

All worker types handle idempotent execution: on assignment, the worker checks for an existing remote branch and PR for the job key and continues from where the previous attempt left off.

See [docs/worker-protocol.md](docs/worker-protocol.md) for the full NATS protocol.

### Reviewer

Automated PR review. Watches for InReview transitions, dispatches a review action, reads the decision, and merges/escalates/requests changes. For escalated PRs, the reviewer polls Forgejo for human decisions and handles the merge via the merge queue.

**Owns (writes to):**
- `chuggernaut.merge-queue` — per-repo merge serialization (lock has 5-minute TTL, refreshed during long merges)
- `chuggernaut.rework-counts` — limit rework cycles
- `chuggernaut.review.decision` subject — review outcomes

See [docs/reviewer.md](docs/reviewer.md) for the review lifecycle.

### Graph Viewer

Single-page web app. Primary human interface.

- Real-time DAG of all jobs with dependency edges
- Job detail: state, priority, activities, PR link, session link
- Worker roster: idle / busy
- Dispatcher journal
- Peek/attach/detach for interactive agents

Reads from the dispatcher's HTTP API. SSE-driven — full snapshot on connect, incremental updates thereafter.

### CLI

Command-line interface for job creation, listing, admin, and agent interaction.

```
chuggernaut create --repo owner/repo --title "..." --body "..." [--deps 1,2,3] [--priority N]
chuggernaut jobs [--state on-deck]
chuggernaut show {job_key}
chuggernaut requeue {job_key} [--target on-deck|on-ice]
chuggernaut close {job_key} [--revoke]
chuggernaut interact respond {job_key} "message"
chuggernaut interact attach {worker_id}
chuggernaut interact detach {worker_id}
chuggernaut sim [--delay N]
chuggernaut action [--workflow FILE --runner LABEL]
chuggernaut agent [--capability CAP]
chuggernaut seed {repo} {fixture.json}
```

---

## Known Limitations (v1)

- **No authentication or authorization.** The HTTP API, NATS subjects, and CLI have no auth. Workers self-assign IDs without verification. NATS supports account-based auth and subject-level permissions natively; the HTTP API can add bearer tokens. Out of scope for v1.
- **No metrics or tracing.** No Prometheus endpoint, no OpenTelemetry, no structured tracing across NATS message chains. The journal provides an action log but not operational metrics. Out of scope for v1.
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
  reviewer:      # PR review automation
  runner:        # shared Forgejo Actions runner
```

Workers launched via `scripts/workers.sh`.

---

## Crate Structure

```
chuggernaut/
  Cargo.toml                      # workspace
  crates/
    types/                        # chuggernaut-types: Job, JobState, ClaimState, messages
    dispatcher/                   # chuggernaut-dispatcher: coordinator binary
    reviewer/                     # chuggernaut-reviewer: PR review binary
    worker/                       # chuggernaut-worker: SDK library
    cli/                          # chuggernaut-cli: CLI binary
    forgejo-api/                  # generated Forgejo REST client
  static/                         # graph viewer SPA
  docs/                           # component specs
  scripts/                        # init.sh, workers.sh
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
| Done detection | CDC detects merged PR from PostgreSQL | Reviewer reports merge directly |
| Human interface | Forgejo issues + graph viewer | Graph viewer only |
| Persistence | RocksDB + NATS KV + PostgreSQL | NATS JetStream only |
| Processes | CDC + Sidecar + Reviewer | Dispatcher + Reviewer |
| Graph queries | IndraDB/RocksDB | petgraph in-memory + NATS KV persistence |

---

## Companion Documents

- [NATS Schema](docs/nats-schema.md) — KV buckets, streams, subjects, payload types
- [Dispatcher](docs/dispatcher.md) — state machine, deps, claims, assignment, restart recovery
- [Worker Protocol](docs/worker-protocol.md) — NATS message flows, heartbeat, outcomes, sessions
- [Reviewer](docs/reviewer.md) — review lifecycle, merge queue, rework, done detection
- [Monitor](docs/monitor.md) — lease scanning, orphan detection, job archival
- [Testing](docs/testing.md) — unit, integration, and end-to-end testing strategy
