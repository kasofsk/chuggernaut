# Chuggernaut v2 — Platform Specification

## Philosophy

The job graph is the source of truth, not issues or PRs. Version control is one output of the platform, not its center of gravity. Agents do virtually all implementation work; the operator's primary task is planning and reviewing the graph. Every design decision optimizes for consistency and simplicity over performance, since execution is dominated by long-running agent containers.

---

## Core Concepts

### Job Node

The fundamental primitive. A job is a node in a DAG with:
- Named dependency declarations (DAG edges; data transfer is implicit through VCS)
- Eval criteria attached at definition time
- A strict state machine (see below)
- A resource envelope and timeout

### Job Type

Declarative YAML, one file per job type, lives under `jobs/` in the repo and is version controlled. Declares only the contract — image, platform, resources, input names, eval criteria, retry limits, secrets, vars. No instance-specific wiring, no steps, no imperative logic.

```yaml
# jobs/implement-endpoint.yaml
name: implement-endpoint
image: registry.acme.com/agents/impl:latest
platform: linux        # linux (default) | macos | windows
work:
  type: agent           # agent | command | human
  prompt: prompts/work/implement-endpoint.md
resources:
  cpu: 2
  memory: 4Gi
  timeout: 2h
max_retries: 3      # work task execution retries before escalation
rework_limit: 2     # eval→work cycles before escalation
inputs:
  - name: spec         # named dependency slots; declare DAG edges, not data paths
  - name: codebase
eval:
  - type: command
    run: cargo test --no-fail-fast
    weight: 0.4
    required: true
  - type: agent
    prompt: prompts/eval/security-review.md
    weight: 0.3
    required: true
  - type: agent
    prompt: prompts/eval/architecture-review.md
    weight: 0.3
  - type: human
    required: true
secrets: [GITHUB_TOKEN]
vars: [RUST_EDITION]
```

### Job Instance

Created at planning time, stored in NATS KV. Wires specific upstream instance IDs to the type's named dependency slots. Not a file — ephemeral data in the graph.

```json
{
  "id": 42,
  "project": "acme/api",
  "type": "implement-endpoint",
  "inputs": {
    "spec": 11,
    "codebase": 22
  },
  "state": "Frozen",
  "branch": "job/42",
  "created_at": "2026-04-05T10:00:00Z"
}
```

IDs are sequential integers scoped per project (e.g. `acme/api` job #42), maintained via a CAS counter in NATS KV. Human-friendly for communication; uniqueness is guaranteed within the project namespace. `retry_count` and `rework_count` are not stored on the job record — they are derived from the task log (`attempt` on work tasks and `cycle` on tasks respectively).

Each job works on a dedicated branch `job/{seq}` created from the default branch at launch time. The DAG guarantees that all upstream dependencies are Done (and their branches squash-merged to default) before a dependent job starts. Data transfer between jobs is implicit through VCS — downstream jobs pull the default branch and upstream work is already there.

Named inputs are dependency declarations only — they establish DAG edges and ordering, not data routing. Graph validation at plan time checks that every named input references a valid upstream job instance before any job leaves Frozen.

### State Machine

```
Frozen → Ready | Blocked → Work → Evaluation → Done | Revoked
                              ↑         |
                              └─────────┘  (rework: eval failed, under rework limit)
Work → Escalated       (work task retries exhausted)
Evaluation → Escalated (rework limit exhausted)
Escalated → Work | Done | Revoked
Done, Revoked — terminal
```

- **Frozen** — created but awaiting operator approval; no execution begins until explicitly released
- **Blocked** — waiting on upstream dependencies; auto-transitions to Ready when all deps reach Done
- **Ready** — queued for execution
- **Work** — one or more work tasks executing; job stays here across retries within the current cycle
- **Evaluation** — evaluation tasks running (commands, agents, human); job stays here until all tasks resolve; rework loops job back to Work
- **Escalated** — human intervention required; triggered by work retries exhausted or rework limit exceeded; operator resolves via task inbox
- **Revoked** — terminal; downstream jobs that depend on this job cannot proceed

Job state is the authoritative phase marker. Fine-grained execution detail lives in the task log (see Tasks).

### Graph Modes

Two primary modes:

1. **New feature / project**: graph is statically known and operator-approved before any work begins. No execution until review is complete.
2. **Ongoing development**: jobs are added only. Instead of removing job nodes, they are revoked. Large new features are planned and reviewed statically before launching.

Any non-trivial new job (i.e. a job with dependencies) requires human review before it enters the graph. Trivial/dependency-less jobs may be added automatically by agents.

### Tasks

Tasks are the unit of execution within a job's Work and Evaluation phases. They are a chronological log — created by the job state machine as it transitions, never referencing each other. No task graph; no task dependencies.

Each rework loop is a **cycle**. Cycle 1 = first Work execution + first Evaluation. Cycle 2 = rework Work + second Evaluation. Tasks carry a cycle number tying them to the rework loop.

```rust
pub struct Task {
    pub id: u64,                          // sequential within job
    pub job_seq: u64,
    pub project: String,
    pub phase: TaskPhase,
    pub cycle: u32,                       // increments on each rework
    pub kind: TaskKind,
    pub state: TaskState,
    pub attempt: u32,                     // retry count within this cycle/phase
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub enum TaskPhase { Work, Evaluation }

pub enum TaskKind {
    Command { run: String },
    Agent   { provider: String, model: Option<String>, prompt: String },
    Human   { prompt: String },           // surfaces to operator task inbox
}

pub enum TaskState { Pending, Running, Done, Failed, Cancelled }

pub enum TaskResult {
    Work    { summary: String },
    Command { pass: bool, exit_code: i32, output: String },
    Agent   { pass: bool, score: f32, findings: Vec<Finding>, notes: Option<String> },
    Human   { pass: bool, notes: Option<String> },
}
```

**Task creation rules:**

| Trigger | Tasks created |
|---|---|
| Job enters Work (new cycle) | One task per work definition in job type |
| Work task fails, retries available | New attempt on same task (attempt++) |
| Work retries exhausted | Human task: `{ prompt: "work failed N times — decide" }` → job → Escalated |
| Job enters Evaluation | One task per evaluator declared in job type |
| Eval task fails, under rework limit | All eval tasks cancelled; job re-enters Work (cycle++) |
| Rework limit exhausted | Human task: `{ prompt: "eval failed N cycles — decide" }` → job → Escalated |

**Human tasks** surface in the operator task inbox regardless of which phase or cycle they belong to. A human evaluator in the job type creates a `Human` task in the Evaluation phase — the job stays in `Evaluation` state, no special state needed. The operator's decision (pass/fail + notes) is the task result. Human task rejections in the Evaluation phase do not increment `rework_count` unless they are the only failing evaluator; the normal eval reduce logic applies.

**Example task log:**

```
cycle=1  Work        Agent    attempt=1  Done
cycle=1  Evaluation  Command  attempt=1  Done   pass=true
cycle=1  Evaluation  Agent    attempt=1  Done   pass=false  ← required, triggers rework
cycle=2  Work        Agent    attempt=1  Done              ← findings from cycle 1 injected
cycle=2  Evaluation  Command  attempt=1  Done   pass=true
cycle=2  Evaluation  Agent    attempt=1  Done   pass=true
cycle=2  Evaluation  Human    attempt=1  Done   pass=true   ← operator approved
```

NATS KV: `tasks.{owner}.{project}.{job_seq}.{task_id}`

---

## Orchestration

### NATS Everywhere

NATS JetStream unifies all communication, state, and dispatch.

```
KV:     jobs.{owner}.{project}.{seq}                    job instance record + status
KV:     rdeps.{owner}.{project}.{seq}                   inverse dependency index
KV:     counters.{owner}.{project}                      per-project sequential ID counter
KV:     tasks.{owner}.{project}.{job_seq}.{task_id}     task log entries
KV:     channels.{owner}.{project}.{seq}                agent status / progress
KV:     secrets.{owner}.{project}.{name}                age-encrypted secret values
KV:     users.{email}                                   user accounts
KV:     knowledge.platform                              platform-level agent context
KV:     knowledge.{owner}                               team-level agent context
Stream: job.events.{owner}.{project}.{seq}              event bus / audit log
Stream: job.ready.{owner}.{project}.{platform}          work queue, competing consumers
```

### Orchestrator

Thin stateless service. Watches KV for status changes. When a job completes:
1. Reads the rdeps index for the completed job
2. For each dependent job, checks whether all its dependencies are now Done
3. CAS-transitions newly-unblocked jobs from Blocked → Ready
4. Publishes to the work queue

CAS prevents double-unblocking under concurrent completions. On restart, the orchestrator runs a reconciliation pass: any job with all dependencies Done but state still Blocked is transitioned to Ready.

### Failure Recovery

No distributed leases. Recovery is handled by two mechanisms:

1. **NATS AckWait**: if a runner pulls a job from the queue and dies before acking, NATS redelivers to the next available runner automatically.
2. **Timeout scan**: orchestrator periodically scans for tasks in `Running` state where `now - started_at > job.spec.timeout`. The task is marked Failed and the runner's retry logic applies. No heartbeat lease needed — the job timeout bounds long-running agent tasks.

---

## Runner

Sits between NATS and the container backend. Responsibilities:

1. Pull jobs from `job.ready.{owner}.{project}.{platform}` as a competing consumer
2. CAS-transition job from Ready → Work (drop if CAS fails — another runner got it)
3. Create work task(s) for the current cycle; write to `tasks` KV
4. Inject secrets (decrypted from NATS KV `secrets.*`) and vars (from NATS KV) as env vars; launch container
5. On task failure: increment attempt; if under retry limit re-launch; if retries exhausted create Human task, transition job to Escalated
6. On work task success: transition job to Evaluation; create one task per evaluator in job type
7. Fan out evaluation tasks in parallel (commands and agent processes); collect results
8. Apply reduce across eval task results; on pass: squash-merge `job/{seq}` to default branch; transition job to Done
9. On eval fail: if under rework limit cancel remaining eval tasks, increment cycle, re-queue job to Work; if rework limit exhausted create Human task, transition job to Escalated

### Environment Variables Injected at Launch

```
JOB_ID          42
JOB_PROJECT     acme/api
JOB_BRANCH      job/42
BASE_BRANCH     main
REPO_URL        https://git.acme.com/acme/api.git
NATS_URL        nats://...
NATS_TOKEN      <scoped per-job credential>
# secrets (from Vault, named as declared in job type)
GITHUB_TOKEN    ...
# vars (from NATS KV, named as declared in job type)
RUST_EDITION    2021
```

Containers are dumb — they read env vars, clone the repo, branch, do work, commit, and exit. No NATS awareness required beyond optional progress streaming.

### Evaluation Tasks

After a work task succeeds the runner creates evaluation tasks for the current cycle (see Evaluation section). Command and agent tasks receive the same env var set as the work container — repo URL, job branch ref — so they can inspect the diff. Results are written to `tasks.*` KV as `TaskResult` variants. The runner watches for all evaluation tasks to complete, then applies the reduce.

### Runner Backends

Interface with two implementations:

- **Docker socket** — local dev and single-node self-hosted
- **k3s** — production, multi-node, resource-constrained runner pools

Runners declare their platform (`linux`, `macos`, `windows`) and subscribe only to the matching work queue subject. The platform itself runs on k3s, making it self-hosting.

macOS bare metal runner support (required for Xcode builds) is deferred — the execution model for non-containerised macOS runners needs separate design.

---

## Evaluation

When Work completes the runner creates one task per evaluator declared in the job type and fans them out in parallel. Three evaluator types:

**`command`** — runner executes a CLI command inside a container on the job branch. Captures exit code and stdout. Writes a `TaskResult::Command` to `tasks.*` KV directly.

**`agent`** — runner invokes the configured `AgentProvider` with the eval prompt (path resolved from the repo). Agent inspects the diff, reasons about the criteria, calls `submit_eval` via the channel MCP. Writes a `TaskResult::Agent` to `tasks.*` KV.

**`human`** — runner creates a Human task in `Pending` state and transitions the job to await operator action. No process launched. Operator submits `TaskResult::Human` via the task inbox.

All eval task results land in `tasks.{owner}.{project}.{seq}.{task_id}`. Runner watches for all N evaluation tasks to reach `Done` or `Failed`, then applies the reduce.

### Reduce

Applies only to Evaluation phase tasks. Extracts `pass` and `score` from `TaskResult::Command`, `TaskResult::Agent`, and `TaskResult::Human` variants:

- Weighted score: `sum(score * weight) / sum(weight)` — `Human` results use score 1.0 (pass) or 0.0 (fail)
- Any `required: true` evaluator failing → overall fail regardless of weighted score
- Pass threshold: 0.7 (configurable per job type)

On overall fail: cycle++; findings from all `Agent` and `Command` results injected into the next work task context. If cycle exceeds `rework_limit`: create Human escalation task, transition job to Escalated.

`Finding`, `Severity` types are shared with `TaskResult::Agent`:

```rust
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    pub location: Option<String>,  // "src/foo.rs:42" if applicable
}

pub enum Severity { Error, Warning, Info }
```

---

## Artifact Passing

All work product lives in the version control system. Each job works on a dedicated branch (`job/{seq}`); on evaluation pass the runner squash-merges to the default branch. Downstream jobs start from the default branch — upstream work is already there by the time they launch, guaranteed by the DAG dependency ordering.

No separate artifact store for v1. Binary artifact storage (S3/Minio) is deferred.

---

## Channel MCP Server

An MCP server (`chuggernaut-channel`) bridges Claude processes to NATS. Injected into any `claude -p` invocation launched by the runner — both work containers and agent evaluators. Built on the v1 `crates/channel/` foundation.

### Tools

| Tool | Used by | Purpose |
|---|---|---|
| `update_status` | work + eval agents | Write progress to `channels.{owner}.{project}.{seq}` KV |
| `channel_check` | work + eval agents | Poll inbox for messages from operator |
| `reply` | work + eval agents | Send message to operator via NATS outbox |
| `submit_result` | work agents | Signal work completion with a summary; runner reads and transitions to Evaluation |
| `submit_eval` | eval agents | Write `TaskResult::Agent` to `tasks.{owner}.{project}.{seq}.{task_id}` KV |
| `propose_job` | work agents | Write a Frozen job instance to NATS KV for operator review |

`submit_pr` and `submit_review` from v1 are replaced by `submit_result` and `submit_eval` respectively — same pattern, language cleaned up.

### Supports Two Modes

- **Push notifications** (`claude/channel` experimental capability) — orchestrator can interrupt Claude mid-run with a message delivered as a `<channel>` tag
- **Polling** — Claude calls `channel_check` periodically; suitable for clients that don't support push

---

## Agents

Agents are first-class participants in the state machine, not scripts bolted onto a human workflow.

### Provider Abstraction

The runner invokes agents through a `AgentProvider` trait with per-provider implementations. Provider and model are configured at platform, team, or project level, with job type able to override.

```rust
#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn run(&self, config: AgentRunConfig) -> Result<AgentOutput, AgentError>;
    fn supports_push_notifications(&self) -> bool;
}

pub struct AgentRunConfig {
    pub prompt: String,
    pub working_dir: PathBuf,
    pub model: Option<String>,
    pub system_prompt: Option<String>,    // prepended to prompt for providers without native support
    pub mcp_servers: Vec<McpServerConfig>,
    pub output_schema: serde_json::Value, // JSON Schema; both providers support this
    pub env: HashMap<String, String>,
    pub timeout: Duration,
}

pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

pub struct AgentOutput {
    pub structured: serde_json::Value,   // validated against output_schema
    pub exit_code: i32,
}
```

**`ClaudeProvider`** — `claude -p {prompt} --model {model} --system-prompt {system_prompt} --mcp-config {json} --output-format json --json-schema {schema}`

**`CodexProvider`** — writes `.codex/config.toml` to `working_dir` for MCP config; prepends system prompt to task prompt (no native flag); runs `codex exec {prompt} --model {model} --output-schema {schema_file} --json`

Push notifications (`claude/channel` experimental capability) are Claude-only. Codex agents fall back to polling mode (`channel_check`) automatically — `supports_push_notifications()` governs which mode the channel MCP server starts in.

### Provider Configuration

Declared under `work:` in the job type; falls back to project → team → platform default:

```yaml
work:
  type: agent
  prompt: prompts/work/implement-endpoint.md
  provider: claude           # claude | codex
  model: claude-sonnet-4-6   # optional override
```

### Prompt / Knowledge Libraries

Available at three levels — platform, team, project. Lower levels inherit and extend. Used to inject domain context, coding standards, and architectural conventions into the agent's system prompt at launch time.

- **Project level**: `CLAUDE.md` / `.claude/CLAUDE.md` in the repo — picked up automatically by `claude -p`; injected into system prompt for Codex
- **Team level**: stored in NATS KV under `knowledge.{owner}`, fetched by runner at launch
- **Platform level**: stored in NATS KV under `knowledge.platform`, fetched by runner at launch

Runner composes: platform → team → project (each level can append or override sections). Final composed context is passed via `--append-system-prompt` for Claude, prepended to prompt for Codex.

### Agent Proposals

Agents may propose new job nodes and graph extensions via the `propose_job` channel MCP tool. A proposal writes a Frozen job instance to NATS KV. The operator reviews and either approves (Frozen → Ready / Blocked) or rejects (Frozen → Revoked). Any job with dependencies requires human approval before leaving Frozen.

---

## Version Control

Git is a storage layer, not the center of gravity. The platform manages all branches and commits; users rarely interact with git directly.

- **Repos on disk**: stored on a persistent volume, one bare repo per project
- **Runner operations**: all git operations (branch, commit, squash-merge, push within cluster) are performed by shelling out to the `git` CLI. gitoxide is not used — push and merge are incomplete in that library.
- **Diff API**: the platform's axum API serves diffs on demand (`git diff`, `git log`) — no separate web UI needed
- **Direct user access**: rare; served via standard git-over-SSH against the repo volume using `git-shell`. No git hosting service (Forgejo, GitHub) required.
- **Branch protection**: enforced by the permissions layer — the SSH layer checks the `AuthProvider` before forwarding to git-shell; only the runner identity may push to protected refs

---

## API Layer

The API layer is a bridge, not a service. Two responsibilities only:

1. **HTTP → NATS request-reply proxy**: translate authenticated HTTP requests into NATS requests, return responses. Zero business logic in this layer.
2. **NATS stream → SSE bridge**: subscribe to `job.events.*` streams, forward events to connected HTTP clients.

All platform-internal communication is via NATS. Runners, the orchestrator, and all other services speak NATS directly — the API layer is the only component that speaks HTTP, and only because clients require it.

Implementation: axum. Versioning: `/api/v1/` URL prefix.

### NATS Request-Reply Subjects

The API layer publishes to these subjects and awaits a reply. Services subscribe and handle — the API has no knowledge of what's behind each subject.

```
req.jobs.get.{owner}.{project}.{seq}
req.jobs.list.{owner}.{project}
req.graph.get.{owner}.{project}
req.graph.validate.{owner}.{project}
req.vcs.diff.{owner}.{project}.{seq}
req.vcs.tree.{owner}.{project}.{ref}
req.vcs.blob.{owner}.{project}.{ref}
req.vcs.log.{owner}.{project}
req.vars.get.{owner}.{project}
req.vars.set.{owner}.{project}.{name}
req.vars.delete.{owner}.{project}.{name}
req.secrets.list.{owner}.{project}
req.secrets.set.{owner}.{project}.{name}
req.secrets.delete.{owner}.{project}.{name}
req.knowledge.get.platform
req.knowledge.set.platform
req.knowledge.get.{owner}
req.knowledge.set.{owner}
req.channel.send.{owner}.{project}.{seq}
req.channel.status.{owner}.{project}.{seq}
req.tasks.list.pending.{owner}.{project}
req.tasks.list.{owner}.{project}.{job_seq}
req.tasks.complete.{owner}.{project}.{job_seq}.{task_id}
req.tasks.fail.{owner}.{project}.{job_seq}.{task_id}
```

### HTTP Surface

The sole mutation surface exposed to the web client is escalation handling — the operator's primary task. All other endpoints are read-only. Further mutation surfaces are deferred pending operator scenario exploration.

```
# Auth
POST   /auth/login                                                  → sets httpOnly JWT cookie
POST   /auth/logout                                                 → clears cookie
GET    /auth/me                                                     → current identity

# Operator task inbox (sole mutation surface for v1 web client)
# Surfaces all pending Human tasks across all jobs — escalations and planned human reviews

GET    /api/v1/projects/{owner}/{project}/tasks/pending
       returns all Human tasks in Pending state across all jobs

POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/complete
       body: { "notes": "optional" }   → task Done; job proceeds

POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/fail
       body: { "notes": "what needs to change" }   → task Failed; job re-enters Work or closes

# Per-job task log (read)
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks

# Jobs (read)
GET    /api/v1/projects/{owner}/{project}/jobs
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}

# Graph (read)
GET    /api/v1/projects/{owner}/{project}/graph

# Event streams (SSE — NATS stream bridged to HTTP)
GET    /api/v1/projects/{owner}/{project}/events               project-wide stream
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/events    per-job stream

# VCS
GET    /api/v1/projects/{owner}/{project}/diff/{seq}
GET    /api/v1/projects/{owner}/{project}/tree/{ref}
GET    /api/v1/projects/{owner}/{project}/blob/{ref}/{path}
GET    /api/v1/projects/{owner}/{project}/log

# Operator → agent channel
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/messages
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/status

# Variables (hierarchical: platform → team → project)
GET    /api/v1/projects/{owner}/{project}/vars
PUT    /api/v1/projects/{owner}/{project}/vars/{name}
DELETE /api/v1/projects/{owner}/{project}/vars/{name}

# Secrets (names only; values proxied to Vault, never returned)
GET    /api/v1/projects/{owner}/{project}/secrets
PUT    /api/v1/projects/{owner}/{project}/secrets/{name}
DELETE /api/v1/projects/{owner}/{project}/secrets/{name}

# Knowledge libraries
GET    /api/v1/knowledge/platform
PUT    /api/v1/knowledge/platform
GET    /api/v1/knowledge/{owner}
PUT    /api/v1/knowledge/{owner}
```

### SSE Event Stream

The project-wide stream bridges `job.events.{owner}.{project}.*` from NATS JetStream to the connected HTTP client. Each event is a newline-delimited JSON object. The per-job stream filters to `job.events.{owner}.{project}.{seq}.*`. Clients reconnect using the `Last-Event-ID` header; the API replays from that sequence position in the NATS stream.

### Webhooks

A separate webhook service consumes NATS streams directly and pushes to external endpoints. Not part of the API layer — no coupling between the two.

---

## Identity and Access Control

Principle: deny by default, minimum necessary scope, short-lived credentials. No external auth service for v1 — the entire IAM footprint is a JWT keypair and a NATS KV bucket.

### AuthProvider Trait

A thin interface in the axum layer. Implementations are swappable — replace with Zitadel, Keycloak, or Ory later without touching business logic.

```rust
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, req: &Request) -> Result<Identity, AuthError>;
    async fn authorize(&self, identity: &Identity, action: &Action) -> Result<(), AuthError>;
}

pub struct Identity {
    pub sub: String,
    pub kind: IdentityKind,
    pub project_roles: HashMap<String, ProjectRole>,  // "owner/project" → role
    pub platform_admin: bool,
}

pub enum IdentityKind { User, Runner, Orchestrator }
pub enum ProjectRole { Admin, Member, Viewer }
```

### User Storage

Users stored in NATS KV — no separate database. Keyed by email (natural unique identifier).

```
KV: users.{email}    →  User record
```

```rust
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub project_roles: HashMap<String, ProjectRole>,
    pub platform_admin: bool,
    pub created_at: DateTime<Utc>,
}
```

### Token Issuance

JWT (RS256). On login, validate credentials against the `users` KV bucket, sign a token:

```json
{ "sub": "user_id", "kind": "user", "projects": { "acme/api": "member" }, "exp": ... }
```

API middleware validates the JWT signature and extracts the identity on every request — no external service call per request.

Machine identities (runner, orchestrator) hold long-lived JWTs with `kind: runner` / `kind: orchestrator`, issued at deploy time and rotated via k8s secret.

### Permission Rules

| Action | Required |
|---|---|
| Read any project endpoint | Viewer+ on that project |
| Complete / fail a task | Member+ on that project |
| Manage vars, secrets, knowledge | Admin on that project |
| Platform-level config | `platform_admin` |
| Push to default branch | Runner identity only |

### Per-Job Machine Credentials

At job launch, the runner issues two short-lived credentials scoped to the specific job (duration = job timeout):

**NATS JWT** — subject allow list for work containers:
```
KV read:    jobs.{owner}.{project}.{seq}
KV read:    tasks.{owner}.{project}.{seq}.*
KV write:   channels.{owner}.{project}.{seq}
Publish:    job.events.{owner}.{project}.{seq}.*
Sub/Pub:    channel.{inbox|outbox}.{owner}.{project}.{seq}
```

**NATS JWT** — subject allow list for eval agent containers (more restricted):
```
KV read:    tasks.{owner}.{project}.{seq}.{task_id}
KV write:   tasks.{owner}.{project}.{seq}.{task_id}   ← submit_eval only
KV write:   channels.{owner}.{project}.{seq}
Sub/Pub:    channel.{inbox|outbox}.{owner}.{project}.{seq}
```

**Git SSH certificate** — work containers:
```
push: refs/heads/job/{seq}    only their own branch
pull: any
```

**Git SSH certificate** — eval containers:
```
pull: any    read-only, no push
```

### Branch Protection

Enforced in the git-over-SSH layer before forwarding to git-shell. Protected refs (default branch) reject pushes from any identity except the runner service account. Implemented as a ref permission check — no external policy service needed.

### Secrets

Secret values are stored in NATS KV (`secrets.{owner}.{project}.{name}`) encrypted with [age](https://github.com/FiloSottile/age) (X25519).

The platform generates an age keypair at init time:

- **Public key** — available to the API layer; used to encrypt values on write (`PUT /secrets/{name}`)
- **Private key** — stored as a Kubernetes Secret; mounted into the runner only

The runner decrypts values at job launch and injects them as env vars. Containers never see the key or the KV bucket.

### SecretStore Trait

A thin interface with a single NATS-backed implementation. Swappable if an external secrets manager is needed later.

```rust
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> Result<()>;
    async fn get(&self, owner: &str, project: &str, name: &str) -> Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> Result<()>;
    async fn list(&self, owner: &str, project: &str) -> Result<Vec<String>>;  // names only
}
```

`list` returns names only — values are never returned to callers outside the runner. The runner calls `get` for each name declared in the job type's `secrets` field at launch time.

Key rotation requires re-encrypting all values in the bucket with the new public key — a one-time admin operation exposed as a platform CLI command.

---

## Supporting Infrastructure

All modular, all swappable at the interface boundary:

| Component | Default (self-hosted) | Cloud alternative |
|---|---|---|
| Orchestration / state / events | NATS JetStream | NATS JetStream (managed) |
| Container execution | k3s / Docker socket | EKS, GKE, AKS |
| Artifact store | _(deferred)_ | S3, GCS, Azure Blob |
| Secrets | age-encrypted NATS KV | swap `SecretStore` impl for external manager |
| Variables | NATS KV | — |
| Image registry | Harbor or Zot | ECR, GCR, GHCR |
| Identity / access | JWT (RS256) + NATS KV | Zitadel, Keycloak, Ory (swap AuthProvider impl) |
| Version control | git CLI + bare repos on disk | — |
| API | axum | — |

All infrastructure is Terraformable via Kubernetes providers. Vault is not a dependency.

---

## Schema Registry

The platform does not own a schema registry. The platform's internal type contracts are Rust structs and enums — enforced by the compiler, not at runtime.

A schema registry is available as a platform service for applications built on the platform to use if they choose. It is not a platform primitive.

---

## Security

### Private by Default

All projects deny access unless explicitly granted. Covered by the IAM layer — no additional mechanism needed.

### Container Isolation

Job containers run agent code the platform didn't write. Constraints enforced by the runner at launch:

- No privileged mode
- No host network
- No host volume mounts
- Resource limits from job spec (`cpu`, `memory`, `timeout`) enforced as container runtime constraints
- Ephemeral filesystem — wiped on exit
- **Egress**: internet access permitted (agents need to pull dependencies, call external APIs); cluster-internal CIDRs blocked via network policy. Containers reach NATS only through the injected `NATS_URL` with their scoped token — not via free cluster routing.

Image signing deferred.

### Secrets Discipline

- Secrets exist in two places only: NATS KV (age-encrypted at rest) and container env vars (ephemeral, process-scoped)
- Plaintext values never written to git, task records, logs, or event streams
- Job definitions declare secret names only — never values
- The age private key is runner-only; containers never see it
- `SecretStore.list` returns names only; `get` is called only by the runner at launch
- Eval prompts should instruct agents not to include secret values in findings or notes — platform cannot enforce this mechanically

### Audit Trail

Three layers, all already in the design:

- **NATS event stream** — every job and task state transition published to `job.events.*`; append-only; the primary audit log for all execution activity
- **Task results** — every human task completion records operator identity, timestamp, and notes; who approved what and when is in the task log
- **Git history** — signed squash-merges to default branch; commit message references job seq so any commit is traceable to its originating job

Continuous security audit and inter-service mTLS deferred.

### Frontend Security

PWA served from the same axum origin as the API. Authentication via `httpOnly; Secure; SameSite=Strict` cookie containing the JWT — set on `POST /auth/login`, sent automatically with every subsequent request. XSS cannot read the token; CSRF is prevented by `SameSite=Strict`. TLS enforced everywhere via cert-manager + Let's Encrypt on the k8s ingress.

---

## Mobile

PWA — single codebase, installable on mobile home screen, served from the axum API server. Designed mobile-first; works on desktop as the primary operator interface.

The SSE event stream is the data backbone for the UI — the client connects once per project and receives all state changes in real time. No polling.

**Core screens:**

- **Task inbox** — pending human tasks across all jobs; primary operator interaction surface
- **Graph view** — DAG visualization, job states, live updates via SSE
- **Job detail** — state, task log, agent status/progress, diff for the job branch
- **Escalation flow** — read findings, provide context, complete or fail the task

Frontend framework TBD. Push notifications via Web Push API for task inbox alerts (new human task assigned).

---

## Deferred

- **Dependency invalidation**: no automatic invalidation. Terminal jobs are immutable. Any fix must be appended as a new job. Operator decides whether downstream jobs need re-running.
- **Graph replay**: the NATS event stream is the version history. Graph state at any point in time is reconstructable by replaying `job.events.*` up to that timestamp. No explicit snapshot mechanism needed.
- **Multi-region runner pools**: routing by region is a future extension — add a region dimension to the work queue subject (`job.ready.{owner}.{project}.{platform}.{region}`). Not specced for v1.
- **MFA / OAuth2 login**: deferred until user growth warrants it.
- **Image signing**: cosign verification at runner launch time. Deferred.
- **Continuous security audit**: standard security evaluator prompts shipped with the platform. Deferred.
- **Inter-service mTLS**: deferred; NATS scoped credentials are the primary security boundary within the cluster.
- **Binary artifact store**: S3/Minio for non-git artifacts. Deferred; all work product in VCS for v1.
- **macOS bare metal runners**: required for Xcode builds. Execution model needs separate design.
