# Chuggernaut v2 — Design Document

## Introduction

Chuggernaut is an AI-native software delivery platform. Instead of developers writing code directly, they define a **job graph** — a DAG of work units — and AI agents execute each job: implementing features, running evaluations, and iterating until the work passes. Humans stay in the loop at planning time and at review gates, but agents do the heavy lifting in between.

Jobs are declarative: you describe what needs to happen (image, resources, inputs, eval criteria) and the platform handles scheduling, retries, and state transitions. The job graph drives everything — it determines what runs, in what order, and whether the output is acceptable.

---

## Philosophy

The job graph is the source of truth, not issues or PRs. Version control is one output of the platform, not its center of gravity. Agents do virtually all implementation work; the operator's primary task is planning and reviewing the graph. Every design decision optimizes for consistency and simplicity over performance, since execution is dominated by long-running agent containers.

---

## Architecture Overview

Five components interact to execute a job graph:

- **Dispatcher** — the sole orchestrator. A single process that owns all job and task state, launches containers, drives state transitions, and manages git operations. Nothing else writes job or task records.
- **API layer** — a thin HTTP↔NATS bridge. Translates authenticated HTTP requests into NATS request-reply calls, bridges NATS event streams to SSE, and encrypts secrets on write. No orchestration logic.
- **NATS JetStream** — the nervous system. All state (jobs, tasks, users, secrets, vars, knowledge), all messaging (request-reply, events), and all inter-service communication flows through NATS.
- **Agent containers** — the workers. Each job launches a container that includes the agent CLI (Claude or Codex) and the full development environment. The container clones the repo, does work, and exits. MCP servers injected at launch give agents access to job lifecycle operations and knowledge.
- **Git repos** — the artifact layer. One bare repo per project on a persistent volume. The dispatcher manages all branches and merges. Users rarely touch git directly.

**Data flow:**

```
Operator plans job graph → creates Frozen jobs via POST /jobs
Operator releases graph  → dispatcher validates wiring, transitions to Blocked | Ready
Dispatcher pulls Ready   → creates branch, launches container
Container does work      → commits to job/{seq}, calls submit_result, exits 0
Dispatcher runs eval     → fans out eval tasks (command / agent / human)
Eval passes              → dispatcher squash-merges job/{seq} → default branch → Done
Downstream job unblocks  → Blocked → Ready → repeat
```

---

## Key Design Decisions

### Job graph as source of truth, not issues or PRs

The plan lives in the job graph, not in a ticketing system or a PR queue. This means the platform can enforce ordering, track state, and drive execution without reconciling against external systems. Operators interact with the graph directly at planning time.

### Single-writer dispatcher

All job and task state is written by exactly one process — the dispatcher — in sequence. This eliminates the class of bugs that come from competing writers: no CAS races, no optimistic locking, no distributed consensus. The tradeoff is that the dispatcher is a single point of failure, which is acceptable because execution is already dominated by long-running agent containers, not dispatcher throughput.

### NATS JetStream for everything

One system handles state (KV), messaging (request-reply), events (streams), and pub/sub. This reduces operational complexity: no separate message queue, no separate database, no separate event bus. NATS KV provides the append-only log semantics needed for the task record and the job event stream.

### Declarative job types, no imperative logic in the spec

Job types are YAML files in the repo — they declare the contract (image, inputs, eval criteria, resources, secrets, vars) but contain no steps or procedural logic. The agent container provides all execution logic. This means job types are version-controlled alongside the code they operate on, and changing a job's behavior means changing a YAML file and the agent's prompt, not a CI pipeline definition.

### VCS as output, not center of gravity

Git is a storage layer the dispatcher operates directly. There is no forge (Forgejo, GitHub) in v1. The platform manages all branches and merges; users rarely interact with git. Squash-merges to the default branch carry all job output; downstream jobs pick it up implicitly — no explicit artifact routing needed.

### No separate artifact store in v1

All work product lives in VCS. Binary artifact storage (S3/Minio) is deferred. This keeps the data model simple: every job produces commits (or nothing), and commits flow through the default branch by the DAG ordering.

### Short-lived scoped credentials per job

Each container launch issues a NATS JWT and an SSH certificate valid for `task_timeout` only. The JWT allow list is scoped to exactly what that container needs — work containers cannot write task records; eval containers cannot publish job events. This is the primary security boundary within the cluster.

### Agents via container image

The declared `image` provides the full development environment — language toolchain, agent CLI binary, runtime dependencies. The platform does not manage agent runtimes or install anything at launch time. Operators version their images independently and reference them in job types.

### Three-pass validation

Jobs are validated three times, each at the right moment:
1. **Release-time** — input wiring and static file existence give the operator early feedback before any execution is committed.
2. **Ready-transition** — static files are re-verified at the exact `base_ref` locked when the job enters Ready, eliminating TOCTOU drift between approval and execution.
3. **Launch-time** — secrets and vars are checked immediately before injection, catching anything deleted between release and launch.

### Merge conflicts resolved by agents, not humans

When a squash-merge conflicts (another job landed first), the platform re-enters Work with an updated `base_ref` rather than escalating to a human. The agent redoes its work on the new base. This is consistent with the principle that agents handle implementation work — a rebase is implementation work.

### Human tasks are time-unbounded by design

Human evaluators and human work tasks have no timeout and are never automatically abandoned. A job waiting on a human task remains blocked indefinitely. Operators are responsible for their task inbox. `job_deadline` exists for wall-clock bounding when needed, but automatic abandonment of human review gates is not a platform behavior.

---

## Technology Choices

| Component | Default (self-hosted) | Cloud alternative |
|---|---|---|
| Orchestration / state / events | NATS JetStream | NATS JetStream (managed) |
| Container execution | k3s / Docker socket | EKS, GKE, AKS |
| Artifact store | _(deferred)_ | S3, GCS, Azure Blob |
| Secrets | age-encrypted NATS KV | swap `SecretStore` impl for external manager |
| Variables | NATS KV | — |
| Image registry | Harbor or Zot | ECR, GCR, GHCR |
| Identity / access | JWT (RS256) + SSH CA + NATS KV | Zitadel, Keycloak, Ory (swap `AuthProvider` impl) |
| Version control | git CLI + bare repos on disk | — |
| API | axum | — |

Platform init generates: JWT RS256 keypair, SSH CA keypair, age keypair, VAPID keypair. All private keys are mounted into services at runtime via the deployment's secret mechanism — never stored in NATS KV.

All infrastructure is Terraformable via Kubernetes providers.

---

## Deferred

- **Dependency invalidation**: no automatic invalidation. Terminal jobs are immutable. Any fix must be appended as a new job. Operator decides whether downstream jobs need re-running.
- **Graph replay**: the NATS event stream is the version history. Graph state at any point in time is reconstructable by replaying `job.events.*` up to that timestamp. No explicit snapshot mechanism needed.
- **Multi-region dispatcher pools**: routing by region is a future extension — add a region dimension to the work queue subject. Not specced for v1.
- **MFA / OAuth2 login**: deferred until user growth warrants it.
- **Image signing**: cosign verification at dispatcher launch time. Deferred.
- **Continuous security audit**: standard security evaluator prompts shipped with the platform. Deferred.
- **Inter-service mTLS**: deferred; NATS scoped credentials are the primary security boundary within the cluster.
- **Binary artifact store**: S3/Minio for non-git artifacts. Deferred; all work product in VCS for v1.
- **macOS bare metal dispatchers**: required for Xcode builds. Execution model needs separate design.
- **Commit signing**: GPG-signed squash-merges. Deferred.
