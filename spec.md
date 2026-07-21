# Chuggernaut v2 — Platform Specification

---

## Part 1: Data Model

### 1.1 Job

The fundamental primitive. A **job** is a node in a DAG stored in NATS KV. One Rust struct is the canonical record; the dispatcher is its sole writer.

```rust
pub struct Job {
    pub id: u64,                           // sequential per project; maintained via counter in NATS KV
    pub project: String,                   // "{owner}/{repo}" slug
    pub r#type: String,                    // job type name; references jobs/{type}.yaml at base_ref
    pub title: String,                     // ticket-style instance identity: what this run is for (may be empty). The type carries the *how*; title/description carry the *what*.
    pub description: String,               // the ticket body; injected into work and eval prompts as the §4.3 job brief
    pub deps: Vec<u64>,                    // upstream job ids this job depends on (ordering edges: upstreams must be Done first; their work is in this job's base, their structured results available to it). Plain ids, no named roles — picked at creation, validated at release.
    pub state: JobState,
    pub branch: String,                    // "job/{id}"; set at creation; actual git branch created when job enters Work
    pub base_ref: Option<String>,          // exact HEAD of default branch; set/updated at every Ready-transition (Frozen→Ready and Blocked→Ready) and on squash-merge conflict; None until job first enters Ready
    pub knowledge_tags: Vec<String>,       // union of job type defaults and operator-supplied tags at creation
    pub eval: Vec<Evaluator>,              // additive per-job evaluators, layered on top of the type's eval list at execution; the type's evaluators are a floor — creation can add criteria, never remove or override them; name collisions are a release-time error (see design-lifecycle.md)
    pub timeout: Option<String>,           // optional per-job work-task timeout override (duration string, e.g. "45m"), layering over the type's resources.task_timeout exactly like `eval` layers over the type's evaluators — but Work-phase tasks only; evaluators keep the type default. Any valid duration. Parseability validated at release. None → the type default applies
    pub factory: Option<String>,           // factory name when created by a factory triage agent (see §13); None for operator-created jobs
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,   // set once (immutably) when job first enters Ready; anchor for job_deadline; None until then
}

pub enum JobState { Frozen, Blocked, Ready, Work, Evaluation, WrapUp, Escalated, Stalled, Done, Revoked }
```

`retry_count` and `rework_count` are not stored on the job record — they are derived from the task log (`attempt` on work tasks and `cycle` on tasks respectively). `ready_at` is set once, immutably, when the job first transitions to Ready (Frozen→Ready or Blocked→Ready); it anchors `job_deadline` enforcement.

**NATS KV key:** `jobs.{owner}.{project}.{seq}`

Example record:

```json
{
  "id": 42,
  "project": "acme/api",
  "type": "implement-endpoint",
  "title": "Stripe webhook endpoint",
  "description": "Add /api/v1/stripe/webhook with idempotency-key handling; see the payments runbook for retry semantics.",
  "deps": [11, 22],
  "state": "Frozen",
  "branch": "job/42",
  "base_ref": null,
  "knowledge_tags": ["rust", "rest-api", "payments/stripe-integration"],
  "eval": [],
  "timeout": null,
  "factory": null,
  "created_at": "2026-04-05T10:00:00Z",
  "ready_at": null
}
```

**Naming conventions:** `project` in JSON records is always `"{owner}/{repo}"` (e.g. `"acme/api"`). In NATS KV keys and subjects it is split into `{owner}` and `{project}` components (e.g. `jobs.acme.api.42`), where `{project}` is the bare repo name. HTTP routes follow the same split (`/api/v1/projects/{owner}/{project}/...`). These are the same project — three representations of one identifier.

IDs are sequential integers scoped per project, maintained via a counter at `counters.{owner}.{project}`.

**Branch lifecycle:** each job works on a dedicated branch `job/{seq}`. The branch name is deterministic and stored at creation; the actual git branch is created from the default branch when the job first enters Work. Upstream dependencies are guaranteed Done (and their branches squash-merged) before a dependent job starts. Data flows between jobs implicitly through VCS — no separate artifact routing.

#### Job Type

Declarative YAML, one file per job type, lives under `jobs/` in the repo and is version-controlled. Declares only the contract — image, resources, eval criteria, retry limits, secrets, vars. Not an instance; no imperative logic. (Dependencies are per-instance, chosen at job creation — the type does not declare them.)

**Canonical schema:**

```yaml
# ── Top-level ────────────────────────────────────────────────────────────────
name: string                   # required; unique within the repo (the file stem — the wire identifier)
display_name: string           # optional; human-facing name for the library and the create-form type picker; falls back to name
description: string            # optional; one-line summary shown alongside the display name in the type picker
image: string                  # required for agent/command work; disallowed at top level for human work (container evaluators must declare their own image; see eval.image)

work:                          # required
  type: agent | command | human  # required

  # type: agent only
  prompt: string               # required; path to prompt file in repo (resolved from base_ref)
  provider: claude | codex     # optional; falls back to platform default (see §12.4); project/team defaults deferred
  model: string                # optional; falls back to provider default
  review:                      # optional; inline review loop (see §4.5); omit = no inline loop
    prompt: string             # required; path to reviewer prompt file in repo (resolved from base_ref)
    provider: claude           # optional; defaults to work provider; v1 supports claude only (release-time validation)
    model: string              # optional; falls back to provider default
    iterations: int            # optional; default 5; max author↔reviewer rounds before submitting anyway

  # type: command only
  run: string                  # required; shell command executed inside the container

  # type: agent or command
  secrets: [string]            # optional; injected into the work container only — scoped here because that is the only container top-level-declared secrets ever reached; evaluators declare their own. Disallowed for human work (no container).

  # type: human only
  prompt: string               # required; shown to operator in task inbox

resources:                     # optional; disallowed for work.type: human
  cpu: number
  memory: string               # positive integer, optionally suffixed with a binary unit (Ki|Mi|Gi), or plain bytes: "512Mi", "4Gi", "1048576". No other suffixes ("5g", "4GB" are rejected). Format validated at parse time (release + `chuggernaut validate`), not deferred to container launch
  task_timeout: duration       # per-container execution limit; default 1h

wrap_up:                       # optional; the job's third step (work → evaluation → wrap-up); see design-lifecycle.md
  type: merge | none           # default merge: squash-merge the job branch through the merge queue/gate. none: eval-pass goes straight to Done — for jobs whose effect is external (deploys, reports); the job branch is scratch and is deleted unmerged

job_deadline: duration         # optional; wall-clock limit on entire job (all retries + rework); clock starts when job first enters Ready; applies to all work types

work_retries: int              # optional; default 0; disallowed for work.type: human
eval_retries: int              # optional; default 1; per-agent-evaluator infra retry budget; no-op for command/human evaluators
rework_budget: int             # optional; default 0; disallowed for command work

eval:                          # optional; omit or leave empty for auto-pass
  - name: string
    type: command | agent | human

    # type: command only
    run: string

    # type: agent or human
    prompt: string             # path to prompt file in repo (resolved from base_ref)

    # type: command or agent
    image: string              # optional; falls back to top-level image; one of the two is required
    secrets: [string]          # evaluator-specific secrets; not inherited from top-level

    # type: agent only
    provider: claude | codex
    model: string

    required: bool             # optional; default true; false = advisory
    stage: int                 # optional; default 0; staged-evaluation ordering (§3.3). Non-negative. Evaluators run in ascending stage order; within a stage they fan out in parallel. A later stage's tasks are created only after every required evaluator in the prior stage passes

knowledge: [string]            # default knowledge tags for KO injection at launch
vars: [string]                 # injected into work container and all eval containers
```

**Field rules by work subtype:**

| Field | `agent` | `command` | `human` |
|---|---|---|---|
| `image` | required | required | disallowed |
| `resources` | optional | optional | disallowed¹ |
| `work_retries` | optional | optional | disallowed |
| `eval_retries` | optional | optional | optional |
| `rework_budget` | optional | disallowed | optional |
| `wrap_up` | optional | optional | optional |
| `eval` | optional | optional | optional |
| `prompt` | required | disallowed | required |
| `provider` | optional | disallowed | disallowed |
| `model` | optional | disallowed | disallowed |
| `review` | optional | disallowed | disallowed |
| `run` | disallowed | required | disallowed |

¹ `resources` is disallowed for `human` work because no container is launched. `job_deadline` is top-level and applies to all work types including `human`.

**Field rules by evaluator subtype:**

| Field | `command` | `agent` | `human` |
|---|---|---|---|
| `name` | required | required | required |
| `image` | optional² | optional² | disallowed |
| `run` | required | disallowed | disallowed |
| `prompt` | disallowed | required | required |
| `provider` | disallowed | optional | disallowed |
| `model` | disallowed | optional | disallowed |
| `secrets` | optional | optional | disallowed |
| `required` | optional | optional | optional |
| `stage` | optional | optional | optional |

² Falls back to the job's top-level `image`. Required per-evaluator when the job declares no top-level image (`work.type: human`).

**`work.type: human`** — no container is launched. The dispatcher creates a `Human` task in `Pending` state in the Work phase; it surfaces in the operator task inbox. The operator performs the work manually, then resolves via `POST .../tasks/{task_id}/resolve`:
- `TaskResolution::Pass` — work complete; proceeds to Evaluation (or Done if no evaluators).
- `TaskResolution::Fail` — operator cannot/will not complete; job → Escalated with a Human escalation task.

`work_retries` is disallowed for human work — there is no container to retry. Human work tasks are excluded from the timeout scan; use `job_deadline` to bound wall-clock time. If eval fails and rework budget remains, a new Human task is created for the next cycle with all eval findings injected. Command/agent evaluators on human-work jobs run in their per-evaluator `image` (required, since there is no top-level image to fall back to).

**Command work jobs must be idempotent.** `work_retries` will re-run the command on failure; the command is responsible for safe handling of any partial side effects. Set `work_retries: 0` if the operation cannot be made safe to retry.

**Example job type files:**

```yaml
# jobs/implement-endpoint.yaml
name: implement-endpoint
image: registry.acme.com/agents/impl:latest
work:
  type: agent
  prompt: prompts/work/implement-endpoint.md
  provider: claude
  model: claude-sonnet-4-6
  review:
    prompt: prompts/review/implement-endpoint.md
    model: claude-sonnet-4-6
    iterations: 5
resources:
  cpu: 2
  memory: 4Gi
  task_timeout: 2h
job_deadline: 24h
work_retries: 3
eval_retries: 1
rework_budget: 2
eval:
  - name: unit-tests
    type: command
    run: cargo test --no-fail-fast
  - name: security-review
    type: agent
    prompt: prompts/eval/security-review.md
    provider: claude
    model: claude-opus-4-6
    secrets: [GITHUB_TOKEN]
  - name: architecture-review
    type: agent
    prompt: prompts/eval/architecture-review.md
    required: false
  - name: human-approval
    type: human
    prompt: prompts/eval/human-approval.md
knowledge:
  - rust
  - rest-api
secrets: [GITHUB_TOKEN]
vars: [RUST_EDITION]
```

```yaml
# jobs/deploy-staging.yaml
name: deploy-staging
image: registry.acme.com/runners/deploy:latest
work:
  type: command
  run: scripts/deploy.sh staging
wrap_up:
  type: none                   # the deploy's effect is external; nothing to merge
resources:
  task_timeout: 30m
work_retries: 2
eval:
  - name: smoke
    type: command
    run: scripts/smoke.sh staging
```

#### Authoring Support

The YAML schema above is machine-derived from the platform's own parse types
(single source of truth — it cannot drift from the code):

- `chuggernaut schema job-type | defaults` emits the JSON Schema
  (draft 2020-12). Canonical copies live in `schemas/` (platform repo,
  regenerated under test). In a project repo, commit it next to the files it
  describes and add a yaml-language-server modeline for in-editor validation,
  autocomplete, and hover docs:

  ```sh
  chuggernaut schema job-type > jobs/.job-type.schema.json
  ```
  ```yaml
  # yaml-language-server: $schema=.job-type.schema.json
  name: implement-endpoint
  ...
  ```

- `chuggernaut validate jobs/*.yaml` runs the static slice offline (parse +
  the field-rules matrices below, with a sibling `_defaults.yaml` merged) —
  for contributors and CI. Repo-dependent checks (prompt files exist,
  secrets/vars set) still run at release (§2.2).

#### Project Default Evaluators

An optional file `jobs/_defaults.yaml` declares evaluators appended to **every** job type's `eval` list. This is how a project gates all changes on an evergreen test suite without each job type author remembering to declare it:

```yaml
# jobs/_defaults.yaml
eval:
  - name: ci
    type: command
    run: ./scripts/ci.sh
    image: registry.acme.com/runners/ci:latest   # optional; falls back to the job's top-level image
```

Semantics:
- Resolved from `base_ref` like job type files; the same evaluator field rules apply.
- Default evaluators are appended after the job type's own evaluators. An evaluator name collision between `_defaults.yaml` and a job type is a release-time validation error.
- A default evaluator with no `image` falls back to the job's top-level `image`; for `work.type: human` jobs (no top-level image) the default evaluator's `image` is required — validated at release.
- Required `command` default evaluators participate in the merge gate (see §3.3) like any other required command evaluator.

---

### 1.2 Task

Tasks are the unit of execution within a job's Work and Evaluation phases. They form a chronological log — created by the dispatcher as it drives state transitions, never referencing each other. No task graph; no task dependencies.

Each rework loop is a **cycle**. Cycle 1 = first Work + first Evaluation. Cycle 2 = rework Work + second Evaluation. Tasks carry a `cycle` number.

`rework_budget` is the maximum number of additional rework cycles permitted after the initial one. With `rework_budget: 2`, cycles 1, 2, and 3 are permitted; eval failure at the end of cycle 3 triggers escalation. Precisely: when the eval reduce fails in cycle N, the job re-enters Work as cycle N+1 iff `N ≤ rework_budget`; otherwise it escalates.

```rust
pub struct Task {
    pub id: u64,                          // sequential within job, 1-indexed
    pub job_seq: u64,
    pub project: String,
    pub phase: TaskPhase,
    pub cycle: u32,
    pub kind: TaskKind,
    pub state: TaskState,
    pub attempt: u32,                     // 1-indexed; each retry is a new task record with attempt+1
    pub evaluator: Option<String>,        // evaluator name for Evaluation/MergeGate tasks; None for work and escalation tasks
    pub container_id: Option<String>,     // backend-assigned container ID (Docker or k8s); None for Human tasks
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub enum TaskPhase { Work, Evaluation, MergeGate, Triage }

pub enum TaskKind {
    Command { run: String },
    Agent   { provider: String, model: Option<String>, prompt: String },
    Human   { prompt: String },
}

pub enum TaskState { Pending, Running, Done, Failed }

pub enum TaskResult {
    Work    { summary: Option<String>, structured: Option<serde_json::Value>, token_usage: Option<TokenUsage> },
    Command { pass: bool, exit_code: i32, output: String, structured: Option<serde_json::Value> },
    Agent   { pass: bool, abort: bool, structured: Option<serde_json::Value>, token_usage: Option<TokenUsage> },
    Human   { pass: bool, abort: bool, structured: Option<serde_json::Value>, action: Option<EscalationAction>, operator: String, resolved_at: DateTime<Utc> },
    Triage  { assessment: String, token_usage: Option<TokenUsage> },  // operator-dispatched advisory triage (see below); assessment is the agent's written recommendation, captured from the CLI result text (no submit_result — triage runs without the channel MCP)
}

pub enum EscalationAction { Retry, Resolve, Revoke }

pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

pub enum TaskResolution {
    Pass       { structured: Option<serde_json::Value> },
    Fail       { structured: serde_json::Value, abort: bool },  // structured required on fail; abort defaults false
    Escalation { action: EscalationAction, structured: Option<serde_json::Value> },  // only valid on escalation Human tasks
}
```

**Abort verdict** (design-lifecycle.md): `abort: true` on an evaluator verdict (`submit_eval` for agent evaluators, `TaskResolution::Fail` for human evaluators) declares the work *not satisfiable by rework* — wrong premise, impossible requirement. It implies fail (a contradictory `pass: true, abort: true` submission normalizes to abort). At the eval reduce, an abort from any **required** evaluator skips the remaining rework budget and escalates immediately, carrying the aborting evaluators' structured findings in the escalation prompt. Advisory (`required: false`) aborts are recorded as plain advisory fails. Command evaluators have no abort — an exit code cannot judge fixability. `abort` on a human *work* task resolution is ignored (a declined work task escalates regardless).

**JSON serialization** (adjacent tagging — `kind` field discriminates):

```json
{ "kind": "Pass", "structured": null }
{ "kind": "Fail", "structured": { "notes": "auth check failed" } }
{ "kind": "Escalation", "action": "Retry", "structured": null }
{ "kind": "Escalation", "action": "Resolve", "structured": { "notes": "fixed manually" } }
{ "kind": "Escalation", "action": "Revoke", "structured": null }
```

**Valid `kind` values by task context:**

| Task context | Valid `kind` values | Invalid → |
|---|---|---|
| Human work task (Work phase, `work.type: human`) | `Pass`, `Fail` | 400 if `Escalation` submitted |
| Human evaluator task (Evaluation phase) | `Pass`, `Fail` | 400 if `Escalation` submitted |
| Post-work escalation task (Escalated state) | `Escalation` (`Retry`/`Resolve`/`Revoke`) | 400 if `Pass` or `Fail` submitted |
| Pre-work escalation task (Stalled state) | `Escalation` (`Retry`/`Revoke`) | 400 if `Pass`/`Fail` submitted, or `Escalation` with `action: Resolve` |

`action` is only meaningful on escalation tasks. `structured` is required (non-null) on `Fail`; optional on all others.

**Token usage:** agent work tasks (`TaskResult::Work`) and agent eval tasks (`TaskResult::Agent`) carry optional `token_usage` populated at submit time. `token_usage` is `None` when a container crashes before submitting — this is expected and does not affect retry logic. Command and human tasks never carry token usage.

**NATS KV key:** `tasks.{owner}.{project}.{job_seq}.{task_id}`

**Task creation rules:**

| Trigger | Tasks created |
|---|---|
| Job enters Work (new cycle), `work.type: agent\|command` | One work task (attempt=1) |
| Job enters Work (new cycle), `work.type: human` | One `Human` work task (attempt=1); no container launched |
| Work task fails, `work_retries` available | New task record (same cycle, attempt++); `job/{seq}` hard-reset to `base_ref` before re-launch |
| Work retries exhausted | Human escalation task → job → Escalated |
| Human work task: operator resolves `Fail` | Human escalation task → job → Escalated |
| Operator grants `Retry` on escalation | New work task (same cycle, attempt++); `work_retries` budget NOT reset |
| Eval reduce passes, squash-merge conflict | Update `base_ref` to current default HEAD; cycle++ (rework_budget NOT consumed); re-enter Work with conflict context injected |
| Job enters Evaluation | One task (attempt=1) per evaluator in the lowest `stage` (§3.3); later stages' tasks are created only as each prior stage passes |
| Agent eval task: container exits with no prior `submit_eval`, `eval_retries` available | Infra error; new task record (same cycle, attempt++) |
| Agent eval task: `eval_retries` exhausted | Final task marked Failed (infra error); reduce proceeds |
| Eval reduce fails (`work.type: agent \| human`), under rework budget | Job re-enters Work (cycle++); all eval results feed into next work task |
| Eval reduce fails (`work.type: agent \| human`), rework budget exhausted | Human escalation task → job → Escalated |
| Eval reduce fails (`work.type: command`) | Human escalation task → job → Escalated (rework_budget disallowed for command) |
| Eval reduce passes, default HEAD moved past `base_ref`, candidate squash-merge clean | One `MergeGate` task (attempt=1) per required command evaluator, run against the candidate merge commit (see §3.3 Merge Gate) |
| Any merge-gate task fails | Update `base_ref` to current default HEAD; cycle++ (rework_budget NOT consumed); re-enter Work with gate findings + conflict-style context injected |

`work_retries` counts container failures within a single Work phase. `eval_retries` counts the same for individual agent eval containers. `rework_budget` counts evaluation-driven rework cycles and is entirely separate from both.

**Human tasks** surface in the operator task inbox regardless of phase or cycle. The job stays in its current state (`Evaluation` for human evaluators in the eval phase) while the human task is pending.

**Escalation resolution** — when the operator completes an escalation Human task, `action` drives the next transition:
- `Retry` — re-enters Work in the same cycle (no cycle increment; no rework budget consumed). Branch is used as-is — not reset to `base_ref`, since the operator may have modified it.
- `Resolve` — re-enters Evaluation with the current branch. The operator has done the work and is submitting it for evaluation.
- `Revoke` — terminates the job.

Escalation never bypasses evaluation — `Done` is only reachable via an evaluation pass.

**Pre-Work escalations** — escalations raised before any work task exists put the job in the **`Stalled`** state (not `Escalated`): Blocked→Stalled on re-validation failure (see §2.1); job-deadline escalation from Ready (Ready→Stalled, see §3.5). They accept only `Retry` and `Revoke`; `Resolve` is rejected with 400. For these, `Retry` re-attempts the failed step — re-runs Ready-transition re-validation for the former, re-enqueues the job for execution for the latter — rather than creating a work task. The distinction is carried by the state itself: `Stalled` has no transition into `Work` or `Evaluation`, so a mis-routed `Resolve` is impossible by construction rather than guarded at resolve time.

**Manual triage (advisory)** — when a job is `Escalated` or `Stalled`, the operator may **dispatch a triage task** (`POST .../jobs/{seq}/triage`, §6.2) to help understand *why* it failed. The dispatcher runs an agent over the whole job state — the job brief, the escalation reason, every task that ran with its result, and the per-task captured Stdout logs (decrypted via the `age_artifacts` identity) — and records its written assessment + recommendation as a `TaskPhase::Triage` task carrying `TaskResult::Triage`. Triage is **purely advisory**: it never changes job state and creates no job transition (there is no `Triage` row in the §2.1 table). The operator still decides Retry / Resolve / Revoke. It runs in a platform-level image (`TRIAGE_IMAGE`, §12.4) with provider/model from the platform agent defaults, so it works uniformly on any job type (agent/command/human). The run is self-contained — the prompt embeds the job state in plaintext and there is no channel MCP, so the assessment is read back from the agent CLI's own JSON result text rather than a `submit_result` call. Session transcripts are omitted from the prompt by design (opaque, large, low value). Triage may be dispatched repeatedly; a revoke's container cleanup kills an in-flight triage.

**Example task log:**

```
cycle=1  Work        Agent    attempt=1  Failed            ← infra error
cycle=1  Work        Agent    attempt=2  Done
cycle=1  Evaluation  Command  attempt=1  Done   pass=true
cycle=1  Evaluation  Agent    attempt=1  Done   pass=false  ← required; triggers rework
cycle=2  Work        Agent    attempt=1  Done              ← findings from cycle 1 injected
cycle=2  Evaluation  Command  attempt=1  Done   pass=true
cycle=2  Evaluation  Agent    attempt=1  Done   pass=true
cycle=2  Evaluation  Human    attempt=1  Done   pass=true
cycle=2  MergeGate   Command  attempt=1  Done   pass=true   ← only present if default HEAD moved (see §3.3)
```

**Steps** — work tasks running under the inline review harness (§4.5) additionally carry a step log: the author↔reviewer iterations inside the single work container. Steps are sub-task granularity; they exist for observability (the tracker UI renders the live ping-pong) and never drive dispatcher state transitions — the work task's outcome is still its exit code plus the final `submit_result`.

```rust
pub struct StepRecord {
    pub step: u32,                            // 1-indexed within the task
    pub kind: StepKind,
    pub iteration: u32,                       // ping-pong round, 1-indexed
    pub status: StepStatus,
    pub pass: Option<bool>,                   // inline review verdict; None for author steps and running steps
    pub findings: Option<serde_json::Value>,  // inline review findings; None for author steps
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub enum StepKind { AuthorIteration, InlineReview }
pub enum StepStatus { Running, Done, Failed }
```

**NATS KV key:** `steps.{owner}.{project}.{job_seq}.{task_id}` — one key per work task holding a JSON array of `StepRecord`s. The dispatcher is the sole writer; it appends records as the harness reports step transitions via `req.step.report.*` (see §4.5). Tasks without an inline review loop have no `steps.*` entry.

---

### 1.3 User

Users are stored in NATS KV — no separate database.

**NATS KV key:** `users.{b64url(email)}` (emails contain characters outside the NATS key alphabet; see §1.4 key encoding)

```rust
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub project_roles: HashMap<String, ProjectRole>,  // "owner/project" → role
    pub platform_admin: bool,
    pub created_at: DateTime<Utc>,
}

pub struct Identity {
    pub sub: String,
    pub kind: IdentityKind,
    pub project_roles: HashMap<String, ProjectRole>,
    pub platform_admin: bool,
}

pub enum IdentityKind { User, Dispatcher }
pub enum ProjectRole { Admin, Member, Viewer }
```

User and project management (create user, assign roles, create project) is CLI-only via `chuggernaut admin ...` — no HTTP surface for admin operations.

---

### 1.4 NATS Schema

All NATS KV keys and stream subjects used by the platform:

```
# KV Buckets
jobs.{owner}.{project}.{seq}                    job instance record
rdeps.{owner}.{project}.{seq}                   inverse dependency index (dispatcher-maintained cache)
counters.{owner}.{project}                      per-project sequential ID counter
tasks.{owner}.{project}.{job_seq}.{task_id}     task log entries
steps.{owner}.{project}.{job_seq}.{task_id}     inline review step log (see §1.2, §4.5)
channels.{owner}.{project}.jobs.{seq}           latest ChannelUpdate + latest AgentReply
vars.{owner}.{project}.{name}                   plaintext variable values
secrets.{owner}.{project}.{name}                age-encrypted secret values
users.{email}                                   user accounts
knowledge.global                                global knowledge objects
knowledge.{owner}                               team-level knowledge objects
knowledge.{owner}.{project}                     project-level knowledge objects
platform.vapid.public                           Web Push VAPID public key
push.{user_id}.{subscription_id}                Web Push subscription records
ingest-tokens.{owner}.{project}.{source}        hashed per-source ingest tokens (see §13.2)

# Streams
job-events      subjects: job.events.{owner}.{project}.{seq}.{event_type}
channel-inbox   subjects: channel.inbox.{owner}.{project}.{seq}
ingest          subjects: ingest.{owner}.{project}.{source}
```

Subject components are limited to values that cannot contain `.`. Values that may contain `.` — file paths, git refs, knowledge subjects and predicates — are passed in the request payload, not embedded in the subject.

**Bucket model:** each top-level prefix above (`jobs`, `rdeps`, `counters`, `tasks`, `steps`, `channels`, `vars`, `secrets`, `users`, `knowledge`, `platform`, `push`, `ingest-tokens`) is one fixed NATS KV bucket created at platform init (see §12.1); the dotted remainder is the key within that bucket. No buckets are created dynamically.

**Key encoding:** key segments that may contain characters outside the NATS key alphabet are base64url-encoded: user emails (`users.{b64url(email)}`) and knowledge subjects/predicates (`knowledge` bucket keys: `global.{b64url(subject)}.{b64url(predicate)}`, `{owner}.{b64url(subject)}.{b64url(predicate)}`, `{owner}.{project}.{b64url(subject)}.{b64url(predicate)}`). The owner name `global` is reserved to keep scope prefixes unambiguous. Secret and var `{name}`s are validated to `[A-Za-z0-9_]+` at write time (they become env var names) and stored unencoded.

**`rdeps` index format:** `rdeps.{owner}.{project}.{seq}` stores a JSON array of job IDs (unsigned integers) that directly declare the indexed job as an input:

```json
[43, 77, 103]
```

This is a dispatcher-maintained cache derived from `deps` on each `Job` record. **Write lifecycle:** written only on job creation — when job N is created with `deps: [M]`, the dispatcher appends N to `rdeps[M]`. It is never updated on revocation or completion (those transitions read `rdeps`, not write it). If the write fails on creation, it is non-fatal and does not roll back the job write; the index is always rebuilt from scratch on startup, guaranteeing eventual consistency. **Read lifecycle:** consulted on job Done (to find newly-unblocked dependents) and on job Revoked (to cascade). Each dependent's current state is checked before acting — stale entries pointing to already-terminal jobs are harmless.

**`channels.*` KV entry format:** `channels.{owner}.{project}.jobs.{seq}` stores a `ChannelEntry`. `update_status` overwrites the `update` field; `reply` overwrites the `last_reply` field — both are read-modify-write operations. The `GET .../status` endpoint reads this entry and adds `job_seq` from the URL.

```rust
pub struct ChannelEntry {
    pub update: Option<ChannelUpdate>,
    pub last_reply: Option<AgentReply>,
}
```

**`push.*` KV entry format:** `push.{user_id}.{subscription_id}` stores the W3C `PushSubscription` JSON exactly as received from the client (endpoint, keys). The `subscription_id` is a server-generated UUID assigned at registration.

---

### 1.5 NATS Configuration

**KV buckets** — all buckets use file storage. Replicas: 1 (dev/single-node), 3 (production).

| Bucket prefix | TTL | Notes |
|---|---|---|
| `jobs.*` | none | Job records are permanent |
| `rdeps.*` | none | Derived cache; rebuilt on restart |
| `counters.*` | none | Monotonic counters; permanent |
| `tasks.*` | none | Task log is permanent |
| `steps.*` | none | Inline review step log; permanent alongside tasks |
| `channels.*` | 7d | Agent progress/status; short-lived |
| `vars.*` | none | Plaintext config; permanent |
| `secrets.*` | none | age-encrypted; permanent |
| `users.*` | none | User accounts; permanent |
| `knowledge.*` | none | KO store; permanent |
| `platform.*` | none | Platform config (VAPID public key etc.); permanent |
| `push.*` | none | Web Push subscriptions; permanent until unsubscribed |
| `ingest-tokens.*` | none | Hashed per-source ingest tokens; permanent until rotated |

**Streams** — file storage, deny-delete policy, at-least-once delivery.

| Stream | Subjects filter | Retention | Notes |
|---|---|---|---|
| `job-events` | `job.events.>` | limits: max-age 90d | Primary audit log; append-only |
| `channel-inbox` | `channel.inbox.>` | limits: max-age 7d | Operator→agent messages |
| `ingest` | `ingest.>` | limits: max-age 30d | External event sources (see §13); factory consumers are durable |

---

## Part 2: Job Lifecycle

### 2.1 State Machine

The authoritative definition of all valid job state transitions. No transition exists outside this table.

| From | To | Trigger | Guard | Effect |
|---|---|---|---|---|
| _(creation)_ | `Frozen` | Dispatcher handles `req.jobs.create.*` | — | Write job record to KV; publish `job-created`; update `rdeps` index |
| `Frozen` | `Ready` | `POST .../release` accepted | All deps Done | Record `base_ref` = current default HEAD; set `ready_at`; publish `job-released` |
| `Frozen` | `Blocked` | `POST .../release` accepted | At least one dep not Done | Publish `job-released` |
| `Blocked` | `Ready` | Last upstream dep reaches Done | All deps Done; re-validation of static config at `base_ref` passes | Record `base_ref` = current default HEAD; set `ready_at`; publish `job-unblocked` |
| `Blocked` | `Stalled` | Last upstream dep reaches Done | Re-validation of static config at `base_ref` fails (file deleted or renamed since release) | Create Human task describing the missing file; publish `job-stalled` |
| `Ready` | `Work` | Dispatcher picks up job | — | Create work task (cycle=1, attempt=1); create branch `job/{seq}` from `base_ref`; launch container or surface Human task; publish `job-started` |
| `Ready` | `Stalled` | `job_deadline` elapsed before work started (§3.5) | — | Create Human task noting the deadline; publish `job-stalled` |
| `Work` | `Work` | Work task fails, retries remain | `attempt ≤ work_retries` | Hard-reset `job/{seq}` to `base_ref`; create new task (same cycle, attempt++) |
| `Work` | `Escalated` | Work task fails, no retries left | `attempt > work_retries` | Create Human escalation task; publish `job-escalated` |
| `Work` | `Escalated` | Human work task resolved `Fail` | — | Create Human escalation task; publish `job-escalated` |
| `Work` | `Escalated` | Launch-time validation fails (declared secret or var missing from KV) | — | Create Human escalation task; publish `job-escalated` |
| `Work` | `Evaluation` | Work task succeeds | — | Create one task per evaluator in the lowest `stage` (§3.3 staged evaluation; attempt=1); publish `job-evaluation-started` |
| `Evaluation` | `Evaluation` | A stage completes with every required evaluator passing and a later stage remains | — | Create the next stage's evaluator tasks (attempt=1); a short-circuited stage creates none |
| `Evaluation` | `Evaluation` | Agent eval container exits without `submit_eval`, retries remain | `attempt ≤ eval_retries` | Create new eval task (same cycle, attempt++) |
| `Evaluation` | `Escalated` | Any required eval task exhausts `eval_retries` (infra error) | — | Create Human escalation task; publish `job-escalated` |
| `Evaluation` | `Work` | Eval reduce: product failure, under rework budget | evaluated cycle N ≤ `rework_budget` | cycle++; collect eval findings into rework context; reset `job/{seq}` to `base_ref`; publish `job-rework-started` (reason: `eval_failure`) |
| `Evaluation` | `Escalated` | Eval reduce: product failure, rework budget exhausted | evaluated cycle N > `rework_budget` | Create Human escalation task; publish `job-escalated` |
| `Evaluation` | `Escalated` | Eval reduce: `work.type: command` and any required evaluator failed | — | Create Human escalation task; publish `job-escalated` (rework_budget disallowed for command) |
| `Evaluation` | `WrapUp` | Eval reduce passes; `wrap_up: merge` | — | Enter wrap-up: enqueue on the per-project merge queue; publish `job-wrapup-started` |
| `Evaluation` | `Done` | Eval reduce passes; `wrap_up: none` | — | Delete the scratch branch `job/{seq}`; publish `job-done` |
| `WrapUp` | `WrapUp` | Default HEAD moved past `base_ref`; candidate squash-merge clean; merge gate required (see §3.3) | — | Create one `MergeGate` task per required command evaluator against the candidate merge commit; job remains in WrapUp |
| `WrapUp` | `Work` | Squash-merge conflict (any cycle; rework budget NOT consumed) | — | Update `base_ref` to current default HEAD; cycle++; build conflict context (see §4.3); reset `job/{seq}` to new `base_ref`; publish `job-rework-started` (reason: `merge_conflict`) |
| `WrapUp` | `Work` | Merge-gate task fails (any cycle; rework budget NOT consumed) | — | Update `base_ref` to current default HEAD; cycle++; inject gate output as findings plus conflict-style context (see §3.3); reset `job/{seq}` to new `base_ref`; publish `job-rework-started` (reason: `merge_gate_failure`) |
| `WrapUp` | `Done` | All required evaluators passed; merge gate passed or skipped (see §3.3); squash-merge clean or no-op | — | Squash-merge `job/{seq}` to default branch (no-op if no commits); delete branch `job/{seq}`; publish `job-done` |
| `WrapUp` | `Escalated` | Unexpected hard wrap-up failure — git plumbing / repo IO, not a conflict (design-lifecycle.md) | — | Create Human escalation task; the merge queue advances past the job rather than wedging; publish `job-escalated` |
| `Escalated` | `Work` | Operator resolves escalation Human task with `action: Retry` | — | Create new work task (same cycle, attempt++); publish `job-escalation-resolved` |
| `Escalated` | `Evaluation` | Operator resolves escalation Human task with `action: Resolve` | — | Re-enter Evaluation; publish `job-escalation-resolved` |
| `Escalated` | `Revoked` | Operator resolves escalation Human task with `action: Revoke` | — | See Revoked transition below; publish `job-escalation-resolved` then `job-revoked` |
| `Stalled` | `Ready` | Operator resolves a pre-Work escalation with `action: Retry`; the failed step succeeds (re-validation passes / re-enqueue) — see §1.2 pre-Work escalations | — | Record `base_ref` = current default HEAD; set `ready_at` if unset; enqueue; publish `job-escalation-resolved` then `job-unblocked` |
| `Stalled` | `Stalled` | Operator resolves a pre-Work escalation with `action: Retry`; the failed step still fails | — | Create a new Human task describing the failure; job remains Stalled |
| `Stalled` | `Revoked` | Operator resolves a pre-Work escalation with `action: Revoke` | — | See Revoked transition below; publish `job-escalation-resolved` then `job-revoked` |
| any non-terminal | `Revoked` | `POST .../revoke` | Job not already Done or Revoked | Kill Running tasks; cancel Pending tasks; cascade Revoked to Frozen/Blocked/Ready dependents (transitively); dependents in Work/Evaluation/WrapUp/Escalated/Stalled left in current state; delete branch `job/{seq}` if it exists; publish `job-revoked` |

**State descriptions:**
- **Frozen** — created, awaiting operator approval; no execution begins
- **Blocked** — waiting on upstream dependencies
- **Ready** — queued for execution; `base_ref` is set
- **Work** — work task executing; job stays here across retries within the current cycle
- **Evaluation** — evaluation tasks running; job stays here until all tasks resolve
- **WrapUp** — evaluation passed; the job is landing (`wrap_up: merge` only): merge queue, merge gate, squash. `wrap_up: none` jobs skip this state (Evaluation→Done). See §3.3
- **Escalated** — post-work human intervention: work executed but automation ran out. Operator resolves Retry / Resolve / Revoke via the task inbox
- **Stalled** — pre-work human intervention: the job could not start or become ready (config re-validation failed, or `job_deadline` elapsed while still Ready). No work task exists. Operator resolves Retry / Revoke only — `Resolve` is rejected (§1.2)
- **Done** — terminal; evaluation passed and squash-merge completed
- **Revoked** — terminal; reachable from any non-terminal state

The dispatcher is the sole writer of `jobs.*` KV — including creation. All transitions flow through the dispatcher; no other service writes job records.

---

### 2.2 Release Validation

Validation runs in three passes, each at the right moment:

1. **Release-time** — fast feedback before any execution is committed; checked against current HEAD. Not an execution guarantee.
2. **Ready-transition** — static file existence re-checked at `base_ref` (the exact HEAD locked when the job enters Ready). Eliminates TOCTOU drift.
3. **Launch-time** — secrets and vars re-checked immediately before injection. Catches anything deleted between release and launch.

**Release-time checks (applied on every `release` request, per-job and graph-level):**

Graph wiring rules (all five must hold):
- Every dependency references an existing, non-Revoked upstream job in the same project
- No job references itself (no self-edges)
- No job references a downstream job (no cycles; full topological sort required)
- No duplicate dependencies

Static configuration (fail-fast check against current HEAD of default branch):
- The job type file (`jobs/{type}.yaml`) exists
- For `work.type: agent` jobs: `work.prompt` path exists
- For `work.type: agent` jobs with a `review` block: `work.review.prompt` path exists; the resolved review provider (`work.review.provider`, defaulting to the resolved work provider) is `claude` — inline review is not supported for other providers in v1
- If `jobs/_defaults.yaml` exists: it parses against the evaluator schema, and no default evaluator name collides with an evaluator declared in any job type being validated
- For each agent or human evaluator (including project defaults): the evaluator's `prompt` path exists
- Every secret named in `secrets:` (`work.secrets` and per-evaluator) has an entry in the `secrets.*` KV bucket
- Every var named in `vars:` has an entry in the `vars.*` KV bucket

**Ready-transition re-validation** (Blocked→Ready only; Frozen→Ready already had the check at release):

The dispatcher re-validates static configuration (job type file and all prompt paths for agent and human evaluators) at `base_ref`. If re-validation fails, the job transitions to Escalated with a Human task describing the missing file. Secrets and vars are not re-checked at this point — their values are live KV entries, not pinned to `base_ref`.

**Launch-time validation:**

Secrets and vars declared in the job type are checked immediately before injection. If any are missing, the job transitions to Escalated.

Any release request that fails validation is rejected with a list of offending job instances and the specific rule violated. A job with no dependencies skips wiring rules.

All subsequent execution uses `base_ref` exclusively — the moving default branch is never consulted again after Ready-transition.

---

### 2.3 Graph Operations

Two primary modes:
1. **New feature / project**: graph is statically known and operator-approved before any work begins.
2. **Ongoing development**: jobs are added only; to remove work, revoke jobs. Large new features are planned and reviewed statically before launching.

Operators submit job creation requests via `POST /jobs`; the dispatcher creates the job record in KV and publishes `job-created`. All jobs start in Frozen state.

**Graph-level operations:**
- `POST .../graph/validate` — validate all Frozen jobs in the project; returns all wiring and static config errors across all jobs
- `POST .../graph/release` — atomic: validates all Frozen jobs first; if any fail, rejects with all errors and releases nothing; if all pass, releases each Frozen job (→ Ready or Blocked) in dependency order; jobs already in non-Frozen states are unaffected

**`rdeps` index:** the dispatcher maintains `rdeps.{owner}.{project}.{seq}` as an inverse dependency index (which jobs depend on this job). It is rebuilt from scratch on startup by scanning all `jobs.*` KV records — it is a derived cache, not the source of truth. A write failure during `rdeps` update is non-fatal and does not roll back the job write.

---

## Part 3: Dispatcher

### 3.1 Overview

The dispatcher is the sole writer of all job and task state. A single process drives all orchestration, task execution, and state transitions sequentially — state transitions are processed one at a time, with no concurrent writes to `jobs.*` or `tasks.*` KV. Container monitoring (waiting for running containers to exit) happens concurrently via the async runtime; multiple containers may be running simultaneously. No competing writers for state. All jobs run on Linux containers.

Responsibilities:
1. On `req.jobs.create.*`: write job record to `jobs.*` KV; publish `job-created`; update `rdeps` index
2. On job `Done`: read `rdeps` index; for each newly-unblocked job, record `base_ref` = current HEAD of default branch; transition Blocked → Ready; enqueue each newly-Ready job for execution
3. On job `Revoked`: kill Running tasks; cancel Pending tasks; cascade Revoked to Frozen/Blocked/Ready dependents (transitively); leave Work/Evaluation/Escalated dependents in current state
4. On `req.jobs.release.*`: run release validation (see §2.2); if valid and all deps Done, record `base_ref` and transition Frozen → Ready and enqueue for execution; else transition Frozen → Blocked
5. Execute Ready jobs from the work queue (see §3.2). The dispatcher maintains an in-memory FIFO queue of Ready job IDs. A job is enqueued when it enters Ready state (via steps 2 or 4 above, or after restart reconciliation). The execution loop dequeues one job at a time and drives it through §3.2; container monitoring runs concurrently so multiple containers may run simultaneously, but state transitions remain sequential.
6. On `req.vcs.*`: serve VCS queries by shelling out to `git` against the bare repo on disk
7. On restart: reconciliation pass (see §3.6)

**Dispatcher backends** — interface with two implementations:
- **Docker fleet** — the v1 production default: one or more Docker daemons. The single-node form (local socket; also the dev backend) and the multi-node form are the same backend — the node list just has one entry or several. Platform services (dispatcher, API, NATS, sshd, TLS proxy) run under docker compose on one node; every node runs a Docker daemon reachable over TCP+mTLS (or an SSH tunnel) and executes agent containers.
- **k8s (k3s self-hosted, or EKS/GKE/AKS)** — scale-out option beyond what a small Docker fleet covers; built when needed, not before.

**Docker fleet semantics.** The dispatcher is already the scheduler — single writer, work queue, retries — so the fleet needs no cluster orchestration, only dumb endpoints:
- **Config**: a node list, each entry `{ name, endpoint, slots }` where `slots` caps concurrent containers on that node. A single entry pointing at the local socket is the single-node deployment. Endpoints take three forms: `unix://` (local socket), `tcp://` (a Docker daemon on a private network or tunnel), and the literal **`worker`** — a NATS-proxied `chuggernaut worker` daemon on the node (below). Node names are subject-safe tokens (`[A-Za-z0-9_-]+`), validated at config parse.
- **Placement**: at launch, pick the node with the most free slots (ties broken by name). No affinity, no preemption. Worker-node free slots come from the ping reply (the worker counts its own running `chuggernaut.managed` containers).
- **Container IDs**: `ContainerId` is already opaque; the fleet backend encodes placement as `{node}/{docker_id}` and routes `wait`/`kill`/`inspect`/`copy_file` to the owning daemon.
- **Node failure**: a node dying makes its containers report not-found — exactly the condition §3.5/§3.6 already classify as task failure; retries land on healthy nodes. All configured **docker-endpoint** nodes must be reachable at dispatcher startup (consistent with the backend-reachability rule in §3.6); **worker** nodes soft-fail — an unreachable worker is logged and marked *out of service* (skipped by placement, re-probed on each placement attempt), and its daemon connecting later brings it back without a dispatcher restart.
- **Addressing discipline**: `REPO_URL` and `NATS_URL` injected into containers must be reachable from every node — never `localhost`.
- Container-internal files (MCP binaries, prompt, events batch) are **injected via the put-archive API** into the created container before start (see §4.2, §4.3) — no host bind-mounts, so nothing needs to exist on remote node filesystems (worker nodes substitute their node-local static artifacts, below).

**Worker nodes (`chuggernaut worker`).** A worker node runs a daemon that connects OUT to NATS and executes container operations against its local Docker socket — no Docker endpoint is exposed on any network and the node has no listening port. The dispatcher's fleet backend proxies each `ContainerBackend` op as JSON request-reply on `req.worker.{node}.{op}` (`launch`, `kill`, `inspect`, `copy_file`, `logs`, `ping`); `wait` is implemented dispatcher-side as an inspect poll, so worker restarts are transparent (containers keep running; the poll re-attaches). The daemon authenticates with scoped credentials minted by `chuggernaut admin worker-creds --node {name}` (subscribe `req.worker.{name}.>` + inbox reply publish — nothing else).

**Small-message discipline**: every worker request fits NATS's default 1MB max_payload. The launch request carries metadata plus small dynamic files inline (prompt, per-job credentials, harness config — KBs); **static artifacts are node-local**, provisioned at deploy time with the worker itself (the agent images, and the channel MCP binary baked into the worker's image at the same git SHA). A launch references them by name (`"channel"`) and the daemon substitutes its local copy; an unknown name fails the launch. The client side enforces a payload-size guard — a launch that exceeds it is a wiring bug (bulk bytes leaking back into the launch path), not a transport problem. `logs` replies are tailed to the most recent ~700KB with a truncation marker; the ping reply carries the worker's build version and artifact hashes, and the dispatcher warns (never refuses) on version drift.

The k8s implementation drives the Kubernetes Jobs API directly — create Job, watch pod status, stream logs. No CI or workflow engine (Argo, Tekton, Temporal) sits in between: those bundle their own DAG, state store, and retry semantics — the layer the dispatcher owns — and would reintroduce a second writer to reconcile against.

**`ContainerBackend` trait:**

```rust
#[async_trait]
pub trait ContainerBackend: Send + Sync {
    /// Launch a container; returns an opaque ID used for subsequent calls.
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError>;
    /// Block until the container exits; returns its exit code.
    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError>;
    /// Kill a running container (SIGKILL).
    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError>;
    /// Query current container status; None if container is not found.
    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError>;
    /// Copy a single file out of the container filesystem; None if not found.
    async fn copy_file(&self, id: &ContainerId, path: &str) -> Result<Option<Vec<u8>>, BackendError>;
}

pub struct ContainerLaunchConfig {
    pub image: String,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    pub files: Vec<InjectedFile>,       // written into the created container before start
    pub cpu_limit: Option<f64>,         // fractional CPUs
    pub memory_limit: Option<String>,   // e.g. "4Gi"
}

/// Injected via the backend's file API (Docker put-archive / k8s equivalent)
/// after create, before start. No host bind-mounts — works identically on
/// remote fleet nodes.
pub struct InjectedFile {
    pub container_path: String,
    pub contents: Vec<u8>,
    pub mode: u32,                      // e.g. 0o755 for the MCP binaries
}

pub enum ContainerStatus { Running, Exited { exit_code: i32 } }
```

`copy_file` is used by the dispatcher to extract `/workspace/eval-result.json` after a command eval container exits.

---

### 3.2 Work Execution

For each Ready job, the dispatcher executes the following sequence:

1. Transition job Ready → Work; create work task (cycle=1, attempt=1)
2. **For `work.type: agent | command`**: create branch `job/{seq}` from `base_ref`; load job type and prompt files from `base_ref`. **For `work.type: human`**: create branch `job/{seq}` from `base_ref`; surface the Human task in the operator inbox; await operator resolution (skip to step 8). `base_ref` is locked from this point — no external actor or event changes it except a squash-merge conflict (step 12), which updates it under dispatcher control before re-entering this step.
   - **Rework/conflict context (cycle > 1):** if `eval_context` is non-empty or `merge_conflict` is set, the dispatcher reads the prompt file content from the repo at `base_ref`, appends a structured context block (see §4.3 for format), and passes the combined string as the prompt to the provider. Human rework tasks have the same block appended to their inbox prompt. On cycle 1 the prompt is the file content alone. Prompt content is always resolved by the dispatcher and delivered to containers via a mounted file — never passed as a path (see §4.3).
3. Inject secrets (decrypted from `secrets.*` KV using age private key) and vars as env vars
4. Issue short-lived scoped NATS JWT for the job (see §7.4)
5. Issue short-lived SSH certificate for the job (see §7.3)
6. Launch container via the configured backend. If `work.review` is declared, the container CMD is the inline review harness rather than a direct agent CLI invocation, and the reviewer prompt (resolved from `base_ref`) plus harness config are injected at launch (see §4.5)
7. Monitor task; on container failure: increment attempt; if `attempt ≤ work_retries`, hard-reset `job/{seq}` to `base_ref` and re-launch; else create Human escalation task and transition to Escalated
8. On work task success (container exit 0, or operator resolves Human task with `Pass`): proceed to Evaluation
9. Transition job to Evaluation; create one task per evaluator. If no evaluators declared, skip to step 12 (auto-pass)
10. Fan out evaluation tasks in parallel; monitor each (see §3.3)
11. Apply eval reduce (see §3.3); handle pass or fail outcomes
12. On eval reduce pass: if the job type declares `wrap_up.type: none`, skip the merge gate and squash-merge entirely — transition straight to Done (the work's effect is external; the job branch is scratch and is deleted unmerged). Otherwise transition to **`WrapUp`** and run the merge gate (see §3.3 Merge Gate) — the whole merge path (queue, gate, squash, conflict rework) runs in `WrapUp`, not `Evaluation`. If default HEAD still equals `base_ref`, or `job/{seq}` has no commits beyond `base_ref`, the gate is skipped. If HEAD moved and the candidate squash-merge is clean, the gate re-runs the required command evaluators against the candidate merge commit — gate pass → proceed to squash-merge below; gate failure → update `base_ref` to current default HEAD, increment cycle (rework_budget NOT consumed), reset `job/{seq}` to new `base_ref`, inject the gate output as findings plus conflict-style context, re-enter Work (step 2). On gate pass or skip: squash-merge `job/{seq}` to default branch. If no commits on `job/{seq}` beyond `base_ref`, this is a no-op. If commits exist and merge is clean → transition job to Done. If conflict → snapshot the current `base_ref` into a local variable `old_base_ref` (this value is held in dispatcher memory only — not persisted to the job record), update `job.base_ref` to current default HEAD, increment cycle (without consuming `rework_budget`), reset `job/{seq}` to new `base_ref`, build conflict context using `old_base_ref` and new `base_ref` (see §4.3 for format), re-enter Work (step 2). **Squash-merge commit message format:** subject line is `job/{seq}: {job_type}`; if the work task's `TaskResult::Work.summary` is non-null, append it as the commit body. Example: `job/42: implement-endpoint\n\nAdded /api/v1/stripe/webhook handler with idempotency key.`
    **Finalization hard failures**: wrap-up is designed to be infallible, but an unexpected error in any finalization step (git plumbing, repo IO — anything other than a `Conflict`, which has its own rework path) creates a Human escalation task (reason `finalize_failed`) and the merge queue advances past the job instead of stalling (design-lifecycle.md: unexpected wrap-up failure → triage).
13. On eval reduce product failure: for `agent | human` work — if under rework budget, increment cycle, inject eval findings, re-enter Work (step 2); if rework budget exhausted, create Human escalation task → Escalated. For `command` work — escalate immediately (rework_budget disallowed). If a required evaluator returned `abort: true`, escalate immediately regardless of remaining budget (reason `eval_abort`; see §1.2 Abort verdict)
14. On escalation Human task completion: read `action`. Post-work (job `Escalated`) — `Retry`: new work task same cycle; `Resolve`: re-enter Evaluation; `Revoke`: Revoked. Pre-work (job `Stalled`) — `Retry`: re-run the failed step (Ready-transition re-validation / re-enqueue → Ready); `Resolve`: rejected (400); `Revoke`: Revoked

**Completeness contract for work containers:** task outcome is determined by container exit code — exit 0 = work succeeded; non-zero = infra/runtime failure (retried per `work_retries`). Calling `submit_result` is optional but provides richer rework context.

---

### 3.3 Evaluation

After a work task succeeds, the dispatcher runs the job's evaluators as an ascending sequence of **stages** (evaluator `stage:`, default 0). When a job enters Evaluation, the dispatcher creates one task per evaluator **in the lowest stage** and fans them out in parallel. Within a stage the evaluators run concurrently exactly as an unstaged fan-out does — a job whose evaluators all share one stage (the default) is byte-for-byte the single-fan-out behavior, and that is the compatibility story. For task creation rules see §1.2. For MCP tool contracts see §4.2.

**Staged progression.** A stage completes when all its tasks reach a terminal state (Done or Failed). If every **required** evaluator in the stage passed, the dispatcher creates the next stage's tasks (one per evaluator, fanned out). If any required evaluator failed or aborted, the later stages are **not created** — they are skipped, not failed, so no task records exist for them — and the eval reduce proceeds immediately over the stages that ran. Advisory (`required: false`) failures never block stage progression. Rework/escalation semantics, abort handling, agent-evaluator retries (`eval_retries`), and human-evaluator pending states are all unchanged **within** a stage; the ordering only governs whether the next stage's tasks are created. A rework cycle re-enters Work and, on the next Evaluation, **restarts from the lowest stage** — stages are recomputed per cycle, never resumed mid-sequence. The per-job additive `eval` entries (§1.1) default to stage 0 unless the creator declares otherwise; the `_defaults.yaml` append (§1.1 Project Default Evaluators) keeps whatever `stage` each default declares and never reorders the list.

**Three evaluator types:**

**`command`** — dispatcher executes the declared CLI command inside a container on the job branch, using the evaluator's `image` (falling back to the job's top-level `image`). The repo is cloned to `/workspace` by the dispatcher-injected bootstrap (see §4.1); `run` executes with cwd `/workspace`. Captures exit code and stdout. After exit, extracts `/workspace/eval-result.json` from the container filesystem via `ContainerBackend::copy_file`; if present and valid JSON, sets `structured`; if absent or unparseable, `structured` is None. Dispatcher writes `TaskResult::Command` to `tasks.*` KV. Exit code is the verdict immediately — no submit step. `eval_retries` does not apply to command evaluators. If you need retry-on-flake, build it into the command (e.g. `cargo test || cargo test`).

**`agent`** — dispatcher invokes the configured `AgentProvider` (see §4.3) with the eval prompt. The provider launches a container, which clones the repo on the job branch, inspects the diff, and calls `submit_eval` to publish its verdict and structured findings. The dispatcher writes `TaskResult::Agent` to `tasks.*` KV. Any container exit — zero or non-zero — without a prior `submit_eval` call is an infra error, retried up to `eval_retries`, then marked Failed (infra error). A zero exit without `submit_eval` is NOT treated as a pass; the verdict can only be recorded by calling `submit_eval`. This is categorically distinct from a task that exited after calling `submit_eval { pass: false }`. See §4.2 for the canonical verdict contract.

**`human`** — dispatcher creates a Human task in `Pending` state. No process launched. Operator submits a `TaskResolution` via the task inbox; the dispatcher writes `TaskResult::Human` to `tasks.*` KV and drives the next state transition.

**Reduce** — applied once the evaluation completes: either the final stage's tasks are all Done or Failed, or a stage short-circuited (a required evaluator failed/aborted, so no later stage was created). The reduce considers every evaluator that ran across all stages; skipped stages contribute nothing.

- If any `required` task is Failed (infra error) → skip rework, escalate immediately
- If a `required: false` task is Failed (infra error) → failure recorded; reduce proceeds; does not trigger escalation (same treatment as a product `pass=false` from an advisory evaluator)
- All eval types are binary pass/fail:
  - `Command` — exit 0 = pass, non-zero = fail
  - `Agent` — `pass` field in `submit_eval` payload (see §4.2)
  - `Human` — `pass` field set by operator
- `required: false` evaluators are advisory — failure (product or infra) is recorded but does not trigger rework or escalation
- Overall result: if any `required` evaluator fails (product failure) → overall fail
- **Abort**: if any `required` agent/human evaluator returned `abort: true`, the fail bypasses rework entirely — escalate immediately with the aborting evaluators' findings, whatever the remaining budget (§1.2 Abort verdict). Advisory aborts are plain advisory fails.

**`EvalResult`** — the rework context passed to the next work cycle:

```rust
pub struct EvalResult {
    pub evaluator: String,
    pub pass: bool,
    pub structured: Option<serde_json::Value>,
}
```

On overall fail (`work.type: agent | human`): if the evaluated cycle N ≤ `rework_budget`, cycle++ and `structured` from all eval results is collected into `AgentRunConfig.eval_context` for the next work task; otherwise escalate.

On overall fail (`work.type: command`): escalate immediately (rework_budget disallowed).

#### Merge Gate

The merge gate closes the evergreen gap: eval runs against the job branch built on `base_ref`, but by the time the reduce passes, other jobs may have landed on the default branch. A textually clean merge can still be semantically broken (job A renamed a function job B calls) — without the gate, that breakage would land untested. The guarantee the gate provides: **no commit reaches the default branch without every required command evaluator passing against the exact tree that lands.**

Applied after the eval reduce passes, before squash-merge — the job is in the **`WrapUp`** state throughout (§2.1); a pass lands (WrapUp→Done), a gate failure reworks (WrapUp→Work):

1. **Skip fast-path** — if the default branch HEAD still equals `base_ref`, or `job/{seq}` has no commits beyond `base_ref`, the evaluators already ran against exactly what will land. Skip the gate; squash-merge directly. Solo jobs pay nothing.
2. **Candidate construction** — if HEAD moved: build the candidate squash commit (the job branch's changes squashed onto current default HEAD) via `git merge-tree --write-tree` + `git commit-tree`, and point a temp ref `merge-gate/{seq}` at it. If the merge conflicts, this is the existing squash-merge-conflict path (§3.2 step 12) — the gate never runs.
3. **Gate tasks** — create one `MergeGate` task (attempt=1, current cycle) per **required command evaluator** (job type's own plus project defaults). Each runs exactly like a command eval container (§3.3 `command` semantics: bootstrap clone, exit code is the verdict, `eval-result.json` extraction, no retries) except `JOB_BRANCH=merge-gate/{seq}`. Agent and human evaluators do not re-run — their verdict is about the change; command evaluators verify the integration. The gate **ignores `stage`**: it re-runs the required command evaluators as a single flat fan-out regardless of which stages they were declared in — staging orders only the Evaluation-phase gate on the job branch, not the integration re-check.
4. **Reduce** — all gate tasks pass → advance the default branch to the candidate commit (this *is* the squash-merge; do not re-merge), delete `job/{seq}` and `merge-gate/{seq}`, transition to Done. Any gate task fails → delete `merge-gate/{seq}`; update `base_ref` to current default HEAD; cycle++ (**rework_budget NOT consumed** — an integration failure is not the author's product failure; same treatment as a merge conflict); reset `job/{seq}` to new `base_ref`; inject the failing command output as `EvalResult` findings plus the conflict-style context block (§4.3: commits and diffstat of what landed since the old base); publish `job-rework-started` (reason: `merge_gate_failure`); re-enter Work.

**Serialization** — the gate is a merge queue of depth 1: at most one job per project is in the gate at a time. Jobs whose eval reduce passes while the gate is occupied queue FIFO; each dequeued job re-checks the skip fast-path against the then-current HEAD. Since the dispatcher already merges sequentially, this adds no new coordination — just a queue in dispatcher memory.

**Bounding** — repeated gate failures don't consume `rework_budget`, so a job that genuinely can't integrate could loop Work → Evaluation → WrapUp (gate) → Work. In practice each rework rebases onto the offending HEAD, so the loop converges unless the default branch keeps moving against the job; `job_deadline` is the backstop. Set one on long-running graphs with high merge concurrency.

**Restart** — `MergeGate` tasks reconcile like command eval tasks (§3.6): exit code is the verdict; a vanished container fails the task. The candidate ref is deterministic from `job/{seq}` + the HEAD recorded when the gate opened, so the dispatcher rebuilds `merge-gate/{seq}` and re-runs the gate on restart.

---

### 3.4 Escalation

See §2.1 (state table rows for `Escalated`) and §1.2 (EscalationAction, TaskResolution, escalation resolution semantics). No additional definition here.

---

### 3.5 Timeout and Deadline

**Task timeout scan** — dispatcher periodically scans for tasks in `Running` state where `now - started_at > task_timeout`. The task is marked Failed and retry logic applies. The applicable `task_timeout` is **per task phase**: a Work-phase task uses the per-job override `Job.timeout` (§1.1) when set, else the type's `resources.task_timeout`; every other phase (Evaluation, MergeGate, Triage) uses the type default. This is what keeps the override work-scoped — evaluators are unaffected by it. The same resolved Work timeout also governs the work agent's own container deadline and the §7.4 credential TTLs, so a longer override does not outlive its credentials. Tasks in `Pending` state are not timed out — the clock starts when execution begins. Human tasks (any phase or type) are excluded from the timeout scan — they have no timeout and no automatic abandonment. This is intentional: human review gates are explicit decisions, not time-bounded.

**Job deadline scan** — if `job_deadline` is set, the dispatcher scans for jobs in Work, Evaluation, or Ready state where `ready_at` is set and `now - ready_at > job_deadline`. Any such job is transitioned to a human-intervention state with a Human task explaining the deadline was exceeded: a job still in **Ready** (no work task yet) goes to **Stalled** (pre-work — Retry re-enqueues, Resolve rejected); a job in **Work** or **Evaluation** goes to **Escalated** (post-work — Resolve also available). If the job has a Running container at the time of deadline expiry, the dispatcher kills it (same as the timeout scan) first. Jobs already in Escalated or Stalled state are excluded — a human is already engaged. Jobs in Frozen or Blocked state (and in WrapUp — landing is platform-owned, like the merge queue) are not checked; the clock does not start until `ready_at` is set (i.e., when the job first enters Ready).

**One-shot enforcement:** a job escalates for deadline at most once. Once the operator resolves a deadline escalation (any resolution action), deadline enforcement is permanently disabled for that job — the deadline's purpose is to summon a human, and the human now owns pacing. The scan therefore also excludes any job whose task log contains a resolved deadline escalation task.

---

### 3.6 Restart Reconciliation

On dispatcher startup, apply in order:

1. Rebuild the `rdeps` index from scratch by scanning all `jobs.*` KV records (the index is a derived cache)
2. For each task in `Running` state, first check `tasks.*` KV for a persisted `TaskResult`. If one exists, the task completed and wrote its result before the crash — use the persisted result as authoritative and proceed without querying the backend. If no `TaskResult` exists, query the configured backend by `container_id` and apply task-type-aware rules:
   - **Still running**: re-attach to container events and resume monitoring
   - **Work task, exited 0**: treat as successful completion; proceed to Evaluation
   - **Work task, exited non-0** or **not found**: treat as failure; apply `work_retries`
   - **Agent eval task, exited 0**: treat as infra error (no `submit_eval` received); apply `eval_retries`
   - **Agent eval task, exited non-0** or **not found**: treat as infra error; apply `eval_retries`
   - **Command eval task, exited (any code)**: exit code is the verdict; proceed as normal completion
   - **Command eval task, not found**: infra failure; task marked Failed; reduce proceeds (escalates if the evaluator is `required`)
   - **MergeGate task** (job in `WrapUp`): a gate in flight is superseded — its Running task is failed, the `merge-gate/{seq}` candidate is dropped, and the job re-enters the merge queue, which re-opens the gate fresh against current HEAD
   - Human eval tasks are never in `Running` state; not subject to this path
3. For each job in `WrapUp` (the merge queue is in-memory and lost on restart), re-enter it into the merge queue — with or without a gate in flight — so landing resumes
4. Transition any Blocked job whose dependencies are all Done to Ready
5. Enqueue all jobs currently in Ready state (including those that were Ready before the crash and any newly-Ready jobs from step 4) into the in-memory work queue

The task log in `tasks.*` KV is the source of truth for execution state. The configured backend must be reachable at startup; the dispatcher will not start if the backend is unavailable.

---

## Part 4: Agent Interface

### 4.1 Environment Variables

**Work containers:**

```
JOB_ID          42
JOB_PROJECT     acme/api
JOB_BRANCH      job/42
BASE_BRANCH     main
REPO_URL        ssh://git@platform/acme/api.git
NATS_URL        nats://...
NATS_TOKEN      <work-scoped JWT — see §7.4>
# secrets (decrypted from age-encrypted NATS KV; named as declared in work.secrets:)
# platform agent credentials (agent containers only): every secret in the reserved global/agents scope, env-named by the secret; declared secrets win on collision
GITHUB_TOKEN    ...
# vars (from NATS KV; named as declared in top-level vars:)
RUST_EDITION    2021
```

**Eval containers** (command + agent; not applicable to human evaluators):

```
JOB_ID          42
JOB_PROJECT     acme/api
JOB_BRANCH      job/42
BASE_BRANCH     main
REPO_URL        ssh://git@platform/acme/api.git
NATS_URL        nats://...
NATS_TOKEN      <eval-scoped JWT — see §7.4>
# secrets (only those declared in the evaluator's own secrets: field)
# vars (from NATS KV; named as declared in top-level vars:)
RUST_EDITION    2021
```

**Workspace bootstrap:** the dispatcher wraps every work and eval container CMD with a standard bootstrap: `git clone --branch $JOB_BRANCH $REPO_URL /workspace && cd /workspace && exec {original CMD}`. Images must provide `git` and an SSH client but do not perform the clone themselves — the platform enforces the `/workspace` contract (command evaluators write `eval-result.json` there; eval agents inspect the diff there).

Work containers are dumb — they read env vars, do work in `/workspace`, commit, and exit. No NATS awareness required beyond optional progress streaming.

---

### 4.2 MCP Servers

Two MCP servers are injected into every agent invocation:

- **`chuggernaut-channel`** — bridges agent processes to NATS for job lifecycle operations
- **`chuggernaut-ko`** — scoped KO client for runtime knowledge queries; connects to NATS using the job's scoped JWT; exposes read access to global, team, and project knowledge buckets

**Tool inventory:**

| Tool | Used by | Purpose |
|---|---|---|
| `update_status` | work + eval agents | Write `ChannelUpdate` to `channels.{owner}.{project}.jobs.{seq}` KV |
| `channel_check` | work + eval agents | Poll inbox; accepts optional `since` (stream sequence number); returns all messages after that sequence |
| `reply` | work + eval agents | Write `AgentReply` to `channels.*` KV |
| `submit_result` | work agents | Publish work summary via `req.work.submit.*`; payload: `{ summary?, structured?, token_usage? }`; dispatcher writes task result and transitions to Evaluation |
| `submit_eval` | eval agents | Publish eval verdict via `req.eval.submit.*`; payload must include `pass: bool`; optional: `abort: bool` (default false; "not satisfiable by rework" — skips remaining rework budget and escalates, §1.2), `structured`, `token_usage`; dispatcher writes `TaskResult::Agent` |
| `submit_review` | inline reviewer agents only | Record the inline review verdict; payload: `{ pass: bool, findings? }`. Local-only: writes `/chuggernaut/review-result.json` for the harness to read — never reaches NATS (see §4.5). Tool is absent outside inline review invocations. |
| `create_job` | factory triage agents only | Publish `req.jobs.create.{owner}.{project}`; payload: `{ type, title?, description?, deps?, knowledge_tags? }`; created job carries `factory` provenance and follows the factory's release policy (see §13.4). Tool is absent outside triage jobs. |

**Canonical completion and verdict contract:**

- **Work containers** — `submit_result` is optional structured context. Task outcome is determined by container exit code: exit 0 = work succeeded; non-zero = infra/runtime failure (retried per `work_retries`). A container exiting 0 without calling `submit_result` is valid.
- **Work containers under the inline review harness** — the author agent's `submit_result` calls are intercepted locally (written to `/chuggernaut/work-result.json`, never sent to NATS mid-loop — otherwise an author call would transition the job to Evaluation while the review loop is still running). The harness sends the single authoritative `submit_result` when the loop completes (see §4.5). The exit-code contract is unchanged: harness exit 0 = work succeeded.
- **Command eval containers** — no submit step, no infra-error path. Exit code is the verdict: exit 0 = pass, non-zero = fail. `eval_retries` does not apply.
- **Agent eval containers** — `submit_eval` is required to record the product verdict. Any container exit (zero or non-zero) without a prior `submit_eval` call is an infra error, not a product verdict. The `pass` field in the `submit_eval` payload is authoritative. `eval_retries` applies to infra errors only — a task that exits with `pass=false` via `submit_eval` is a product failure, not an infra error, and is not retried.

**Idempotency:** when the dispatcher receives `submit_result` or `submit_eval`, it first reads the target task from KV. If already `Done`, it returns success immediately without re-processing. If `Running`, it writes the result, transitions state, and publishes events. Since the dispatcher is single-threaded and the sole writer, there is no race between read and write.

**NATS request reliability:** `submit_result` and `submit_eval` are request-reply calls. The agent SDK must retry these with bounded backoff until an ack is received. This makes submissions survive brief dispatcher restarts.

**Channel message types:**

```rust
pub struct ChannelUpdate {
    pub message: String,
    pub percent: Option<u8>,
}

pub struct OperatorMessage {
    pub text: String,
    pub sent_at: DateTime<Utc>,
}

pub struct AgentReply {
    pub text: String,
    pub sent_at: DateTime<Utc>,
}

pub struct ChannelStatus {
    pub job_seq: u64,
    pub update: Option<ChannelUpdate>,
    pub last_reply: Option<AgentReply>,
}
```

`ChannelUpdate` is overwritten on each `update_status` call. `AgentReply` is overwritten on each `reply` call — reply history is not retained. Operator messages are appended to the `channel-inbox` stream at `channel.inbox.{owner}.{project}.{seq}` — never overwritten.

**Push vs. polling:** agents support two inbox modes:
- **Push** (`claude/channel` experimental capability) — dispatcher delivers new inbox messages mid-run as a `<channel>` tag by subscribing to the `channel-inbox` stream. Claude only.
- **Polling** — agent calls `channel_check` with an optional `since` sequence number; returns all messages published after that sequence. Suitable for Codex. The agent tracks the last consumed sequence and passes it on each subsequent call.

`supports_push_notifications()` on the `AgentProvider` trait (see §4.3) governs which mode the channel MCP server starts in.

**MCP server distribution:** `chuggernaut-channel`, `chuggernaut-ko`, and `chuggernaut-harness` (see §4.5) are built as standalone binaries and shipped alongside the dispatcher. At container launch the dispatcher injects them (as `InjectedFile`s, mode 0755 — see §3.1) into every created container at `/usr/local/bin/chuggernaut-channel` and `/usr/local/bin/chuggernaut-ko` before start. The `McpServerConfig.command` field in each `AgentRunConfig.mcp_servers` entry references one of these paths. The binaries connect to NATS using the `NATS_URL` and `NATS_TOKEN` values passed in `McpServerConfig.env`.

---

### 4.3 Provider Abstraction

The dispatcher invokes agents through an `AgentProvider` trait with per-provider implementations.

```rust
#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn run(&self, config: AgentRunConfig) -> Result<AgentOutput, AgentError>;
    fn supports_push_notifications(&self) -> bool;
}

pub struct AgentRunConfig {
    pub image: String,
    pub prompt: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,      // composed from knowledge libraries (see §4.4)
    pub mcp_servers: Vec<McpServerConfig>,
    pub env: HashMap<String, String>,
    pub task_timeout: Duration,
    pub eval_context: Vec<EvalResult>,      // empty on cycle 1 and merge-conflict cycles; populated on eval-failure rework
    pub merge_conflict: Option<String>,     // set when cycle was triggered by squash-merge conflict
}

pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

pub struct AgentOutput {
    pub exit_code: i32,
}
```

`AgentProvider.run()` is responsible for launching the container using the dispatcher's backend, injecting the provider-specific agent CLI invocation as the container CMD, monitoring the container until it exits, and returning the result. The declared job `image` provides the full development environment including the agent CLI binary and all runtime tooling.

Provider and model are configured at platform level (see §12.4) with per-job-type overrides at `work:` level or per eval step. In v1, the fallback chain is: job-type declaration → platform default. Project-level and team-level defaults are deferred.

**`ClaudeProvider`** — container CMD: `claude -p "$(cat /chuggernaut/prompt.md)" --model {model} --append-system-prompt {system_prompt} --mcp-config {json}`. The `{json}` is the serialized `Vec<McpServerConfig>` passed inline on the command line.

**`CodexProvider`** — before launch, the dispatcher serializes `AgentRunConfig.mcp_servers` into Codex's MCP config format and injects it into the container at `/repo/.codex/config.toml` (same mechanism as the prompt file). Container CMD: `codex exec "$(cat /chuggernaut/prompt.md)" --model {model}` (system prompt prepended to the prompt file content, no native flag).

Structured context surfaces through MCP tools (`submit_result`, `submit_eval`), which send NATS request-reply messages to the dispatcher — the dispatcher never parses agent stdout.

**Job brief format** — when a job carries a `title` and/or `description`, the dispatcher appends this block to the prompt file content for the work agent, for every agent evaluator (evaluators judge against the same brief the author saw), and to Human task prompts:

```
---
## Job Brief
**{title}**

{description}
```

**Rework context format** — on rework cycles (cycle > 1) where `eval_context` is non-empty or `merge_conflict` is set, the dispatcher reads the prompt file from the repo at `base_ref`, appends the following block (after the job brief, if any), and sets `AgentRunConfig.prompt` to the combined string. The provider injects the prompt content into the created container at `/chuggernaut/prompt.md` (put-archive, §3.1) — never inline in the shell command, never via a host bind-mount.

```
---
## Rework Context

### Previous Evaluation Findings
(one block per EvalResult; all results included, pass and fail)
**{evaluator_name}** (pass: {true|false}):
{structured JSON, pretty-printed; or "(no structured findings)" if absent}

### Merge Conflict
(section present only when `merge_conflict` is set)

The `merge_conflict` string is a human-readable plain text block constructed by the dispatcher:
1. Files that could not be cleanly merged — from the conflicted-file list reported by `git merge-tree --write-tree` (repos are bare; there is no worktree for `git status`)
2. Commit summary of what landed on default since the old `base_ref` — from `git log --oneline {old_base_ref}..{new_base_ref}`
3. Diff summary — from `git diff --stat {old_base_ref}..{new_base_ref}`

Example:

```
Conflicting files:
  src/api/routes.rs
  src/handlers/auth.rs

Changes on main since base commit abc1234:
  a3f9b21 job/41: add-auth-middleware
  c72de0a job/39: update-token-format

  src/api/routes.rs  | 47 ++++++++++-------
  src/handlers/auth.rs | 18 ++++---
```
```

`AgentRunConfig.prompt` always carries resolved prompt content, never a path. On cycle 1 it is the prompt file content read from the repo at `base_ref`; on rework cycles the context block above is appended. Delivery is identical on every cycle: content injected into the created container at `/chuggernaut/prompt.md` before start.

---

### 4.4 Knowledge Injection

Knowledge Objects (KOs) follow a `(subject, predicate) → object` model. Each KO is a discrete fact retrievable in O(1). Three scoped buckets (see §1.4 for KV keys).

**Upfront injection (tagged, work containers only) — IMPLEMENTED, repo-versioned form:** job types declare default knowledge tags (`knowledge:`); operators may add more at job creation. At launch the dispatcher resolves the union by reading `tags/{tag}.md` from the project repo at `base_ref` (tags without a file are skipped), concatenates them into a `## Project Knowledge` block, and injects it via `--append-system-prompt` for Claude (prepended to the prompt for Codex). Work containers only — eval containers do not receive a pre-injected system prompt. The repo is the primary tag store; the NATS KO buckets described below (global/team scopes, runtime queries via chuggernaut-ko) remain the deferred extension for knowledge that spans projects.

**Runtime querying (MCP, all agent containers):** agents query further KOs at runtime via the `chuggernaut-ko` MCP server. Both work and eval containers receive NATS JWT credentials with read access to all three knowledge buckets (see §7.4); the MCP server uses these to resolve queries without dispatcher involvement.

`CLAUDE.md` / `.claude/CLAUDE.md` in the repo is picked up automatically by `claude -p` and does not need to be in the KO store.

---

### 4.5 Inline Review Harness

When a job type declares `work.review`, the work container runs an **inline review loop**: the author agent and a reviewer agent alternate inside the same container until the reviewer accepts or the iteration budget runs out, then the harness submits. This is the fast inner loop — same working tree, same build caches, no container relaunch per exchange. The v1 pain this replaces: every author↔reviewer round as a separate dispatched workflow with its own checkout and cold start.

**The inline reviewer is advisory; the outer Evaluation phase is authoritative.** Everything in the work container runs inside the work security boundary with work-scoped credentials — the author process could in principle tamper with the reviewer's verdict, which is exactly why acceptance here never substitutes for the independent evaluators or the merge gate. The loop's job is to drive up first-pass eval success and keep iteration cheap, not to gate the merge.

**Mechanics:**

- `chuggernaut-harness` is a static binary injected like the MCP servers (`InjectedFile`, mode 0755, at `/usr/local/bin/chuggernaut-harness`). When `work.review` is declared, the provider sets it as the container CMD (after the standard bootstrap clone) instead of the direct agent CLI invocation.
- The dispatcher injects `/chuggernaut/harness.json` at launch: the provider-composed author command, author continuation command template, reviewer command, and `iterations`. The harness is provider-agnostic — it executes command strings and never composes CLI flags itself. In v1 only `ClaudeProvider` composes harness configs; `review.provider` resolving to anything but `claude` is rejected at release time (§2.2).
- The reviewer prompt is resolved from `base_ref` at launch and injected at `/chuggernaut/review-prompt.md`, same delivery rules as the work prompt (§4.3): always content, never a path.

**Loop protocol** (author iteration N, review N, N starting at 1):

1. **Author runs.** Iteration 1 is the standard invocation against `/chuggernaut/prompt.md`. Iterations > 1 resume the author's session (`claude -p --continue`) with the reviewer's findings as the new message — the author keeps its conversation context across iterations; the workspace and build caches persist naturally. Author exit 0 = ready for review; non-zero = the harness exits non-zero and the normal `work_retries` path applies.
2. **Reviewer runs** as a fresh process with a fresh session every time — no accumulated sympathy for the author's choices. It inspects the working tree and the diff against `base_ref`, then calls `submit_review { pass, findings? }`. Reviewer exit without a prior `submit_review` is retried once; a second miss records a failed review step and the harness proceeds to submit (the outer eval is the gate — a broken reviewer must not wedge the job).
3. **Verdict.** `pass: true` → acceptance; harness submits. `pass: false` → findings feed iteration N+1. Budget exhausted (`iterations` rounds, default 5) without acceptance → harness submits anyway with the unresolved findings attached; the authoritative evaluators decide.
4. **Final submit.** The harness sends the single `req.work.submit.*` request (bounded-retry until ack, per §4.2): the author's latest intercepted `submit_result` payload, with `structured.inline_review = { iterations, accepted, unresolved_findings? }` merged in. Then it exits 0.

**Local interception:** the channel MCP server runs in the author process with local-submit mode enabled — `submit_result` writes `/chuggernaut/work-result.json` instead of publishing to NATS (see §4.2). In the reviewer process it runs in review mode: `submit_review` (local write) is exposed; `submit_result` is absent. Both processes retain `update_status`, `channel_check`, `reply`, and `chuggernaut-ko` access.

**Step reporting:** around each author iteration and each review invocation, the harness sends `req.step.report.{owner}.{project}.{seq}.{task_id}` (request-reply, bounded retry, **non-fatal on failure** — a lost step report degrades observability, never the loop). The dispatcher appends the `StepRecord` to `steps.*` KV (§1.2) and publishes `step-started` / `step-completed` events (§6.3), which is what the tracker UI renders as the live ping-pong under the job card.

**Timeout:** `task_timeout` covers the entire loop — all author iterations and reviews in one container run. Size it for the budgeted iterations, not a single pass.

---

## Part 5: Version Control

### 5.1 Branch Management

Git is a storage layer, not the center of gravity. The platform manages all branches and commits; users rarely interact with git directly.

- **Repos on disk**: stored on a persistent volume, one bare repo per project at `{repos_root}/{owner}/{project}.git`
- **Default branch**: read at runtime from the git repo's `HEAD` symref (`git symbolic-ref HEAD`); set at project creation (see §12.2); no separate KV entry
- **Dispatcher operations**: all git operations (branch, commit, squash-merge) performed by shelling out to the `git` CLI; gitoxide is not used (push and merge are incomplete in that library)
- **Diff API**: the platform's axum API serves diffs on demand (`git diff`, `git log`) via the VCS NATS subjects (see §6.1) and HTTP routes (see §6.2)
- **Branch protection**: enforced in the SSH layer — only the dispatcher identity may push to protected refs (default branch); see §5.2

**Artifact passing:** all jobs work on a dedicated branch (`job/{seq}`). On evaluation pass, the dispatcher squash-merges to the default branch if any commits exist on the job branch; otherwise the merge is a no-op. Downstream jobs start from the default branch — upstream work is already there by the time they launch, guaranteed by DAG dependency ordering. `deps` establish that ordering; whether an upstream actually produced VCS output depends on what the upstream did, not its job type.

**Branch cleanup:** `job/{seq}` branches are deleted by the dispatcher immediately after the squash-merge on Done and immediately after task cleanup on Revoked. Job branches are not retained after a terminal state is reached.

No separate artifact store for v1. Binary artifact storage (S3/Minio) is deferred.

---

### 5.2 SSH Access

One SSH CA keypair is generated at platform init. The private key is mounted into the dispatcher at runtime. The public key is available to the SSH server (`TrustedUserCAKeys ca.pub`). No per-user key registration required.

**SSH ref authorization by principal:**

| Principal | Push permitted | Pull permitted |
|---|---|---|
| `job:{owner}/{project}:{seq}` | `refs/heads/job/{seq}` in `{owner}/{project}` only | any ref in `{owner}/{project}` |
| `dispatcher` | any protected ref (default branch, tags) | any |
| user email (Member+ on project) | `refs/heads/job/{seq}` only | any ref on projects where Viewer+ |
| user email (Viewer on project) | none | any ref on projects where Viewer+ |

Job principals embed the owner and project because job seqs are only unique per project — a bare `job-{seq}` principal could not be authorized against the right repo.

Per-project read authorization is enforced against the user's `project_roles` claim in their SSH cert extension.

For credential issuance details, see §7.3 (User SSH certs) and §7.4 (Per-job SSH certs).

---

### 5.3 Linked-Origin Projects

A project may be **linked** to an existing externally-hosted repo (GitHub): the external host owns the default branch, chuggernaut never pushes it, and work ships as pull requests. Classic self-hosted projects are unchanged; a project is linked iff its `projects.{owner}.{project}` KV record has `origin` set.

**Branch model.** The local bare repo's `HEAD` symref points at a chuggernaut-owned **`integration`** branch, so the entire §3.2/§3.3 merge machinery (job branches, squash-merge, merge queue, merge gate, SSH branch protection) operates on `integration` untouched — "default branch" *is* integration. The origin's default branch is tracked as `refs/remotes/origin/{main}` via a normal fetch refspec: not a local head, so unpushable through the SSH front. Agents keep talking only to the internal SSH front / local repo; `REPO_URL` never points at the origin.

**Creation** (`req.projects.link`): init bare + `remote add origin` + single-branch fetch refspec; origin main autodetected via `ls-remote --symref` when unspecified; `integration` created at origin main; pre-receive hook installed; the config subset of the starter template (jobs/, prompts/, tasks/ — no README) seeded **skip-existing** onto integration, reaching the origin via the first release PR.

**Credentials.** Project secrets `CHUG_ORIGIN_DEPLOY_KEY` (OpenSSH private key, write deploy key — git fetch/push) and `CHUG_ORIGIN_PAT` (fine-grained PAT, pull requests read/write — PR API), set via `admin secret set` before linking. The `CHUG_` name prefix is **reserved**: declaring such a secret in a job type is a release-validation error and injection skips them — origin credentials are dispatcher-only and never reach a container. Origin git ops decrypt the key to a 0600 tempfile for the duration of the command (`GIT_SSH_COMMAND`, `StrictHostKeyChecking=accept-new`) with a 60s timeout so a hung remote cannot wedge the single-writer actor.

**Origin release** (`req.origin.release`, explicit trigger only): guards — linked project, no Open release, **no merge gate in flight** (a gate completing after the snapshot would land a commit the post-merge reset silently discards), integration ahead of origin main. Sequence (crash-safe): persist `release_counter`+1 → pin `refs/chug/release-{n}` at the integration tip (keeps pre-reset history reachable for held jobs' `base_ref`s) → push integration to the origin as `chug/release-{n}` → open PR `chug/release-{n}` → main (title `chug release {n}`, body lists squash subjects since the last base) → persist `ReleaseState{Open}` + hold. A crash before the final persist burns `n`; the orphan origin branch is harmless.

**Hold.** While a release is Open the project's merge queue is held: jobs still eval and enqueue, nothing lands on integration (`pump_merges` returns early). This makes the post-merge hard reset lossless by construction. Holds are rebuilt from `projects.*` KV at startup, before reconcile re-enqueues recovered jobs.

**Sync** (`req.origin.sync`, also run opportunistically by `origin.status`): fetch, then — PR merged (any merge method) → mark `Merged`, hard-reset `integration` onto the new origin main, clear the hold, pump; held jobs finalize against the new HEAD through the existing gate/conflict-rework paths. PR closed unmerged → mark `Closed`, clear the hold, no reset. No open release → fast-forward integration onto origin main when it has nothing unreleased (external commits flow in); otherwise leave it — divergence surfaces as PR conflicts at the next release (v1 limitation; no automated resolution). Non-GitHub origins (no PR API, e.g. `file://`) release by branch push only; origin main moving off the release base is the merge signal.

**Failure surfacing.** Fetch/push/API failures inside release/sync return errors to the caller (409/422/500 envelopes) — the dispatcher never blocks job execution on origin availability; job launch reads only local refs.

---

## Part 6: API Layer

The API layer is a bridge, not a service:
1. **HTTP → NATS request-reply proxy**: translate authenticated HTTP requests into NATS requests, return responses. No orchestration logic in this layer.
2. **NATS stream → SSE bridge**: subscribe to `job.events.*` streams, forward events to HTTP clients.
3. **Authentication and authorization**: validate JWT cookies and enforce permission rules (see §7.1, §7.5).
4. **Secret value encryption**: on `PUT /secrets/{name}`, encrypt the value with the age public key before forwarding. The API layer never sees the age private key or decrypted values.

Implementation: axum. URL prefix: `/api/v1/`.

---

### 6.1 NATS Subjects

The API layer publishes to these subjects and awaits a reply. Services subscribe and handle.

```
req.jobs.create.{owner}.{project}
req.jobs.get.{owner}.{project}.{seq}
req.jobs.list.{owner}.{project}
req.jobs.release.{owner}.{project}.{seq}
req.jobs.revoke.{owner}.{project}.{seq}
req.jobs.criteria.{owner}.{project}.{seq}                    response: { ref, wrap_up, evaluators: [Evaluator + source: "type"|"job"], errors: [string] } — resolved eval criteria at the job's pinned ref
req.jobtypes.get.{owner}.{project}                           payload: { name }; response: { name, ref, yaml, job_type: JobType|null, errors } — one type in full (raw + parsed, defaults merged) for the library UI
req.tags.list.{owner}.{project}                              response: string[] — available knowledge tags (`tags/*.md` stems at default HEAD; a tag's meaning lives in its repo-versioned markdown file)
req.projects.create                                          payload: { owner, name }; creates the bare repo (+ pre-receive hook, + Code starter template seed, + job counter); 409 if it exists. Platform admins only at the API layer.
req.projects.link                                            payload: { owner, name, origin_url, main_branch? }; linked-origin project creation (§5.3): fetch from origin, HEAD → integration, config seed (skip-existing), project record + counter. Requires CHUG_ORIGIN_* project secrets first. Platform admins only at the API layer.
req.origin.release.{owner}.{project}                         open an origin release (§5.3): push integration → chug/release-{n} on the origin, open the PR, hold the merge queue; 409 when a release is open / a gate is in flight / nothing to release
req.origin.status.{owner}.{project}                          response: { origin, release, release_counter, origin_main_sha, integration_sha, ahead_by, held }; opportunistically reconciles an Open release's PR state
req.origin.sync.{owner}.{project}                            fetch the origin and reconcile (§5.3): merged PR → reset integration onto new origin main + clear hold; closed → clear hold, no reset
req.vcs.file.{owner}.{project}                               payload: { path }; response: { path, ref, content } — one repo file at default HEAD (prompt viewer / repo browser)
req.vcs.tree.{owner}.{project}                               response: { branch, ref, entries: [{path, type, size}] } — full recursive tree at default HEAD (repo browser)
req.graph.get.{owner}.{project}                              response: Job[] (all jobs in project)
req.graph.validate.{owner}.{project}
req.graph.release.{owner}.{project}
req.vcs.diff.{owner}.{project}.{seq}                         no payload; job seq is in subject
req.vcs.tree.{owner}.{project}          payload: { ref }
req.vcs.blob.{owner}.{project}          payload: { ref, path }
req.vcs.log.{owner}.{project}           payload: { ref?, limit? }
req.vars.list.{owner}.{project}
req.vars.get.{owner}.{project}.{name}
req.vars.set.{owner}.{project}.{name}
req.vars.delete.{owner}.{project}.{name}
req.secrets.list.{owner}.{project}
req.secrets.set.{owner}.{project}.{name}
req.secrets.delete.{owner}.{project}.{name}
req.knowledge.get.global                payload: { subject, predicate }
req.knowledge.set.global                payload: { subject, predicate, value }
req.knowledge.delete.global             payload: { subject, predicate }
req.knowledge.list.global               payload: { subject? }
req.knowledge.get.{owner}               payload: { subject, predicate }
req.knowledge.set.{owner}               payload: { subject, predicate, value }
req.knowledge.delete.{owner}            payload: { subject, predicate }
req.knowledge.list.{owner}              payload: { subject? }
req.knowledge.get.{owner}.{project}     payload: { subject, predicate }
req.knowledge.set.{owner}.{project}     payload: { subject, predicate, value }
req.knowledge.delete.{owner}.{project}  payload: { subject, predicate }
req.knowledge.list.{owner}.{project}    payload: { subject? }
req.channel.send.{owner}.{project}.{seq}
req.channel.status.{owner}.{project}.{seq}
req.tasks.list.pending.{owner}.{project}
req.tasks.list.{owner}.{project}.{job_seq}
req.tasks.resolve.{owner}.{project}.{job_seq}.{task_id}
req.steps.list.{owner}.{project}.{job_seq}.{task_id}
req.work.submit.{owner}.{project}.{seq}
req.eval.submit.{owner}.{project}.{seq}.{task_id}
req.step.report.{owner}.{project}.{seq}.{task_id}            harness-only; appends StepRecord (see §4.5)
req.usage.query.{owner}.{project}
req.usage.query.{owner}.{project}.{seq}
req.ssh.sign-user-cert              payload: { "public_key": string }
```

Push subscription management (`POST /api/v1/push/subscribe`, `DELETE /api/v1/push/subscribe/{subscription_id}`) is handled by the API layer directly via `push.*` KV writes/deletes — no NATS request-reply intermediary. The API layer reads the user identity from the JWT cookie to construct the KV key.

---

### 6.2 HTTP Routes

```
# Auth — every authenticated route accepts the JWT session cookie (browser) OR `Authorization: Bearer <jwt>` (machine callers; mint with `admin user token --email … --ttl 720h`). Same RS256 JWT, same verification; roles are baked in at mint time (no revocation list — keep TTLs short).
POST   /auth/login                                                  → 200 OK; sets httpOnly JWT cookie
POST   /auth/logout                                                 → 200 OK; clears cookie
GET    /auth/me                                                     → 200 OK; body: Identity
POST   /auth/ssh-cert    body: { "public_key": string }             → 200 OK; body: { "certificate": string }; signs user's public key; returns 24h SSH cert

# Operator task inbox
GET    /api/v1/projects/{owner}/{project}/tasks/pending             → 200 OK; body: Task[]; all Human tasks in Pending state across all jobs
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/resolve
       body: TaskResolution
       → 200 OK

# Per-job task log (read)
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks          → 200 OK; body: Task[]
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/steps  → 200 OK; body: StepRecord[]; empty array if the task has no inline review loop

# Token usage
GET    /api/v1/projects/{owner}/{project}/usage                     → 200 OK; body: UsageSummary; aggregate across all agent tasks in the project
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/usage          → 200 OK; body: UsageSummary; aggregate for a single job (all cycles and attempts)

# Jobs
POST   /api/v1/projects/{owner}/{project}/jobs
       body: { "type": "implement-endpoint", "title": "Stripe webhook endpoint", "description": "...", "deps": [11, 22], "knowledge_tags": ["payments/stripe-integration"], "eval": [ { "name": "extra-ci", "type": "command", "run": "./ci.sh" } ] }
       title/description optional; the instance's ticket — injected into work and eval prompts as the §4.3 job brief
       knowledge_tags optional; merged with job type's default tags
       eval optional; additive per-job evaluators layered on top of the type's list (§1.1 evaluator schema); validated at release — name collisions with the type's evaluators are a 422
       → 201 Created; body: Job record
GET    /api/v1/projects/{owner}/{project}/jobs                      → 200 OK; body: Job[]
POST   /api/v1/projects                                             body: { owner, name } → 201; platform admins only. Creates repo + hook + Code starter template + counter (§12.2); 409 if it exists.
POST   /api/v1/projects/link                                        body: { owner, name, origin_url, main_branch? } → 201; platform admins only. Linked-origin creation (§5.3); 422 when the CHUG_ORIGIN_* secrets are missing; 409 if it exists.

# Origin (linked projects, §5.3)
GET    /api/v1/projects/{owner}/{project}/origin                    → 200 OK; body: { origin, release, release_counter, origin_main_sha, integration_sha, ahead_by, held }; 404 on classic projects. Viewer+.
POST   /api/v1/projects/{owner}/{project}/origin/release            → 201; opens the release PR and holds the merge queue; 409 when a release is open / gate in flight / nothing to release. Project Admin.
POST   /api/v1/projects/{owner}/{project}/origin/sync               → 200 OK; fetch + reconcile (merged PR → integration reset + hold cleared). Project Admin.
GET    /api/v1/projects/{owner}/{project}/job-types                 → 200 OK; body: [{ name, display_name, description }] (jobs/*.yaml at default HEAD; display metadata for the type picker — a file that fails to parse still lists, stem only)
GET    /api/v1/projects/{owner}/{project}/job-types/{name}          → 200 OK; body: { name, ref, yaml, job_type, errors } — the library view (raw + parsed, defaults merged)
GET    /api/v1/projects/{owner}/{project}/tags                      → 200 OK; body: string[] — available knowledge tags (tags/*.md stems; drives the create-form tag picker)
GET    /api/v1/projects/{owner}/{project}/file?path={path}          → 200 OK; body: { path, ref, content } — one repo file at default HEAD (the create form's prompt links; 404 if absent)
GET    /api/v1/projects/{owner}/{project}/tree                      → 200 OK; body: { branch, ref, entries } — full recursive tree at default HEAD (Files tab)
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}                → 200 OK; body: Job
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/criteria       → 200 OK; body: { ref, wrap_up, evaluators: [Evaluator + source], errors } — the criteria the job will be (or was) judged against, resolved at base_ref (or default HEAD before Ready)
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/release        → 200 OK; body: Job (updated state); 422 with error list if validation fails
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/revoke         → 200 OK; body: Job (updated state); 409 if already Done or Revoked
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/triage         → 200 OK; body: Job (unchanged); dispatches an advisory triage agent (§1.2). 409 unless the job is Escalated or Stalled; 422 if TRIAGE_IMAGE is unconfigured. Member+. Never changes job state

# Graph
GET    /api/v1/projects/{owner}/{project}/graph                     → 200 OK; body: Job[] (all jobs in the project)
POST   /api/v1/projects/{owner}/{project}/graph/validate            → 200 OK if valid; 422 with error list if any Frozen job fails validation
POST   /api/v1/projects/{owner}/{project}/graph/release             → 200 OK; body: Job[] (released jobs); 422 with error list if any job fails validation (nothing released)

# Event streams (SSE)
GET    /api/v1/projects/{owner}/{project}/events                    project-wide stream
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/events         per-job stream

# VCS
GET    /api/v1/projects/{owner}/{project}/diff/{seq}
       response: { "files": [{ "path": string, "additions": int, "deletions": int }], "diff": string (unified diff) }
       behavior by job state:
         Frozen | Blocked | Ready — branch not yet created; returns { "files": [], "diff": "" }
         Work | Evaluation | Escalated — branch exists; returns git diff base_ref..job/{seq}
         Done — branch deleted after squash-merge; returns diff of the squash-merge commit against
           its parent on the default branch (commit found via: git log -1 --grep "^job/{seq}: " {default_branch})
         Revoked — branch deleted, no squash-merge commit; returns { "files": [], "diff": "" }
GET    /api/v1/projects/{owner}/{project}/tree/{ref}
       response: [{ "path": string, "type": "blob"|"tree", "size": int|null }]
GET    /api/v1/projects/{owner}/{project}/blob/{ref}/{path}
       response: { "content": string (raw file content), "encoding": "utf-8"|"base64" }
GET    /api/v1/projects/{owner}/{project}/log
       query params: ref (optional, default: default branch), limit (optional, default: 50)
       response: [{ "hash": string, "message": string, "author": string, "ts": DateTime<Utc> }]

# Ingest (external event sources; Bearer ingest-token auth, not JWT cookies — see §13.2)
POST   /api/v1/projects/{owner}/{project}/ingest/{source}
       body: arbitrary JSON (the event payload)
       → 202 Accepted; API validates the token, wraps the payload in an IngestEvent envelope,
         publishes to ingest.{owner}.{project}.{source}; 401 on bad token; 413 over 1 MiB

# Web Push notifications
POST   /api/v1/push/subscribe                               body: W3C PushSubscription JSON; stored at push.{user_id}.{subscription_id} in NATS KV; returns { "subscription_id": string }
DELETE /api/v1/push/subscribe/{subscription_id}             unregister device; deletes KV entry

# Operator → agent channel
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/messages
       body: { "text": "please focus on the auth module" }; returns 202
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/status         → ChannelStatus (see §4.2)

# Variables
GET    /api/v1/projects/{owner}/{project}/vars                      → 200 OK; body: Var[]
GET    /api/v1/projects/{owner}/{project}/vars/{name}               → 200 OK; body: Var; 404 if not found
PUT    /api/v1/projects/{owner}/{project}/vars/{name}               body: { "value": string } → 204 No Content
DELETE /api/v1/projects/{owner}/{project}/vars/{name}               → 204 No Content; 404 if not found

# Secrets (names only; values never returned)
GET    /api/v1/projects/{owner}/{project}/secrets                   → 200 OK; body: string[] (names only)
PUT    /api/v1/projects/{owner}/{project}/secrets/{name}            body: { "value": string } → 204 No Content; API encrypts value with age public key before writing
DELETE /api/v1/projects/{owner}/{project}/secrets/{name}            → 204 No Content; 404 if not found

# Knowledge
GET    /api/v1/knowledge/global                                     → 200 OK; body: { subject, predicate, value }[]
GET    /api/v1/knowledge/global/{subject}                           → 200 OK; body: { predicate, value }[]
GET    /api/v1/knowledge/global/{subject}/{predicate}               → 200 OK; body: { value: string }; 404 if not found
PUT    /api/v1/knowledge/global/{subject}/{predicate}               body: { "value": string } → 204 No Content
DELETE /api/v1/knowledge/global/{subject}/{predicate}               → 204 No Content
GET    /api/v1/knowledge/{owner}                                    → 200 OK; body: { subject, predicate, value }[]
GET    /api/v1/knowledge/{owner}/{subject}                          → 200 OK; body: { predicate, value }[]
GET    /api/v1/knowledge/{owner}/{subject}/{predicate}              → 200 OK; body: { value: string }; 404 if not found
PUT    /api/v1/knowledge/{owner}/{subject}/{predicate}              body: { "value": string } → 204 No Content
DELETE /api/v1/knowledge/{owner}/{subject}/{predicate}              → 204 No Content
GET    /api/v1/projects/{owner}/{project}/knowledge                 → 200 OK; body: { subject, predicate, value }[]
GET    /api/v1/projects/{owner}/{project}/knowledge/{subject}       → 200 OK; body: { predicate, value }[]
GET    /api/v1/projects/{owner}/{project}/knowledge/{subject}/{predicate}  → 200 OK; body: { value: string }; 404 if not found
PUT    /api/v1/projects/{owner}/{project}/knowledge/{subject}/{predicate}  body: { "value": string } → 204 No Content
DELETE /api/v1/projects/{owner}/{project}/knowledge/{subject}/{predicate}  → 204 No Content
```

**Path encoding:** `{ref}` (which may contain `/`, e.g. `job/42`) and knowledge `{subject}`/`{predicate}` (which may contain `/` or `.`) are single path segments and must be percent-encoded by clients (`job%2F42`, `payments%2Fstripe-integration`). Blob `{path}` is the greedy remainder of the URL — its slashes are not encoded. The owner name `global` is reserved so team-scope knowledge routes cannot collide with the global routes.

---

### 6.3 Events

All events are published exclusively by the dispatcher to `job.events.{owner}.{project}.{seq}.{event_type}`. No other service or container publishes to this stream. Every event is a JSON object with at minimum `{ "job_seq": u64, "project": String, "ts": DateTime<Utc> }` plus event-specific fields.

| Event type | Trigger |
|---|---|
| `job-created` | Dispatcher creates job in KV in response to `req.jobs.create.*` |
| `job-released` | `POST .../release` accepted; Frozen → Ready or Blocked |
| `job-unblocked` | Blocked → Ready (last upstream dep reached Done) |
| `job-started` | Ready → Work; includes `cycle` |
| `job-evaluation-started` | Work → Evaluation; includes `cycle` |
| `job-rework-started` | Evaluation → Work with cycle++; includes new `cycle`, `reason` (`eval_failure` \| `merge_conflict` \| `merge_gate_failure`), and `eval_context` (populated for `eval_failure` and `merge_gate_failure`; empty for `merge_conflict`) |
| `job-merge-gate-started` | Eval reduce passed with moved default HEAD; merge-gate tasks created (see §3.3); includes `cycle` |
| `job-done` | Evaluation → Done |
| `job-escalated` | Any transition to Escalated (work retries exhausted, rework budget exhausted, launch/re-validation failure, required eval infra failure, human work failed, command eval failure, deadline exceeded); includes `reason` field |
| `job-escalation-resolved` | Operator completes escalation Human task; includes `action` (`Retry`/`Resolve`/`Revoke`) |
| `job-revoked` | Any non-terminal → Revoked; includes cascaded job seqs if dependents were also revoked |
| `task-created` | New task written to KV; includes `kind` (`Command`\|`Agent`\|`Human`), `phase` (`Work`\|`Evaluation`\|`MergeGate`), `cycle`, `attempt` |
| `task-started` | Task transitioned to Running |
| `task-completed` | Task reached Done; includes `pass` and `structured` where applicable; includes `token_usage` for agent tasks when reported |
| `task-failed` | Task reached Failed |
| `step-started` | Harness reported a step beginning (see §4.5); includes `task_id`, `step`, `kind` (`author`\|`inline-review`), `iteration` |
| `step-completed` | Harness reported a step finished; includes `status` (`done`\|`failed`) and, for inline-review steps, `pass` and `findings` |

---

### 6.4 Webhooks

A separate webhook service consumes NATS streams directly and pushes to external endpoints. Not part of the API layer — no coupling between the two. All event types from §6.3 are available to webhook consumers.

**SSE delivery** — the API layer subscribes to the `job-events` JetStream stream with a subject filter. The project-wide SSE endpoint filters on `job.events.{owner}.{project}.>`; the per-job endpoint filters on `job.events.{owner}.{project}.{seq}.>`. Each event is forwarded as:

```
id: {nats-sequence}\n
data: {"job_seq":42,"project":"acme/api","ts":"...","event_type":"job-started",...}\n
\n
```

The `id` field carries the NATS stream sequence number. Clients reconnect using the `Last-Event-ID` header; the API replays from that sequence position. Content-Type: `text/event-stream`.

---

### 6.5 Error Responses

All error responses use a consistent JSON envelope:

```json
{ "error": "string" }
```

Validation errors (422) use an `errors` array with per-item detail:

```json
{
  "errors": [
    { "job_seq": 42, "field": "deps", "message": "depends on unknown job #99" }
  ]
}
```

`job_seq` is omitted for errors not tied to a specific job instance (e.g. a schema parse error). The `field` path uses dot notation matching the job type YAML structure. Graph-level operations (POST .../graph/validate, POST .../graph/release) may return multiple entries across multiple jobs in a single 422.

Standard HTTP status codes:
- `400` — malformed request body (unparseable JSON, missing required field)
- `401` — missing or invalid JWT cookie
- `403` — authenticated but insufficient role for the operation
- `404` — job, task, var, secret, or knowledge entry not found
- `409` — conflict (e.g. revoke on a Done job)
- `422` — validation failure (dependency wiring errors, static config errors)
- `500` — internal error (NATS unavailable, git command failure)

---

## Part 7: Identity and Access

### 7.1 Authentication

JWT (RS256). On login, validate credentials against the `users` KV bucket, sign a token:

```json
{ "sub": "user_id", "kind": "user", "project_roles": { "acme/api": "member" }, "exp": ... }
```

API middleware validates the JWT signature and extracts the identity on every request — no external service call per request. Authentication via `httpOnly; Secure; SameSite=Strict` cookie — set on `POST /auth/login`, sent automatically with subsequent requests. XSS cannot read the token; CSRF is prevented by `SameSite=Strict`.

The dispatcher identity holds a long-lived JWT with `kind: dispatcher`, issued at deploy time and rotated via the deployment's secret mechanism.

**`AuthProvider` trait:**

```rust
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, req: &Request) -> Result<Identity, AuthError>;
    async fn authorize(&self, identity: &Identity, action: &Action) -> Result<(), AuthError>;
}
```

Implementations are swappable — replace with Zitadel, Keycloak, or Ory later without touching business logic.

---

### 7.2 User Management

User and project management is CLI-only via `chuggernaut admin ...` — no HTTP surface for admin operations. See §1.3 for the `User` struct and NATS KV key.

---

### 7.3 SSH CA

One SSH CA keypair is generated at platform init. The private key is mounted into the dispatcher at runtime (k8s Secret in k8s deployments, bind-mounted file in Docker deployments). The public key is available to the SSH server configuration (`TrustedUserCAKeys ca.pub`). No per-user key registration required.

**User SSH certs** — `POST /auth/ssh-cert`: user submits `{ "public_key": string }` (their SSH public key, authenticated via JWT cookie); API forwards to dispatcher via `req.ssh.sign-user-cert`; dispatcher signs with the CA private key and returns `{ "certificate": string }` valid for 24 hours, with a principal equal to the user's email. Users interact with git via the `chuggernaut` CLI, which handles cert refresh transparently.

SSH ref authorization rules are defined in §5.2.

---

### 7.4 Per-Job Credentials

At each container launch, the dispatcher issues two short-lived credentials valid for `task_timeout`:

**NATS JWT — work containers:**

```
KV read:    jobs.{owner}.{project}.{seq}
KV read:    tasks.{owner}.{project}.{seq}.*
KV read:    knowledge.global.*
KV read:    knowledge.{owner}.*
KV read:    knowledge.{owner}.{project}.*
KV read:    channels.{owner}.{project}.jobs.{seq}
KV write:   channels.{owner}.{project}.jobs.{seq}
Publish:    req.work.submit.{owner}.{project}.{seq}
Publish:    req.step.report.{owner}.{project}.{seq}.*
Subscribe:  channel.inbox.{owner}.{project}.{seq}
```

The `req.step.report.*` permission backs the inline review harness (§4.5). The inline reviewer runs inside the work container and shares these credentials — it sits inside the work security boundary, which is why its verdict is advisory and the Evaluation phase remains the authoritative gate.

**NATS JWT — eval agent containers (more restricted):**

```
KV read:    tasks.{owner}.{project}.{seq}.{task_id}
KV read:    knowledge.global.*
KV read:    knowledge.{owner}.*
KV read:    knowledge.{owner}.{project}.*
KV read:    channels.{owner}.{project}.jobs.{seq}
KV write:   channels.{owner}.{project}.jobs.{seq}
Publish:    req.eval.submit.{owner}.{project}.{seq}.{task_id}
Subscribe:  channel.inbox.{owner}.{project}.{seq}
```

Work containers do not publish to `job.events.*`. All events are published exclusively by the dispatcher. Eval agents do not write to `tasks.*` KV directly — the dispatcher is the sole writer.

**Factory triage jobs** (see §13) receive the work-container JWT plus one additional permission: `Publish: req.jobs.create.{owner}.{project}` — this is what backs the `create_job` MCP tool. Triage agents never receive release permissions; the dispatcher applies the factory's release policy itself.

**SSH certificates — work containers:**

```
principal: job:{owner}/{project}:{seq}
push: refs/heads/job/{seq}    only their own branch, in their own project
pull: any ref in {owner}/{project}
```

**SSH certificates — eval containers:**

```
principal: job:{owner}/{project}:{seq}
pull: any ref in {owner}/{project}    read-only, no push
```

The NATS operator signing key is mounted into the dispatcher at runtime (k8s Secret in k8s deployments, bind-mounted file in Docker deployments).

---

### 7.5 Permission Rules

| Action | Required |
|---|---|
| Read any project endpoint | Viewer+ on that project |
| Complete / fail a task | Member+ on that project |
| Manage vars, secrets, knowledge | Admin on that project |
| Platform-level config | `platform_admin` |
| Push to default branch | Dispatcher identity only |
| Issue SSH certs | Authenticated user (any role) |

---

## Part 8: Secrets and Variables

### 8.1 Variables

Variables are project-scoped plaintext key-value pairs. Stored in NATS KV at `vars.{owner}.{project}.{name}`. Values are not sensitive — no encryption.

```rust
pub struct Var {
    pub name: String,
    pub value: String,
}

#[async_trait]
pub trait VarStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> Result<()>;
    async fn get(&self, owner: &str, project: &str, name: &str) -> Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> Result<()>;
    async fn list(&self, owner: &str, project: &str) -> Result<Vec<Var>>;  // names and values
}
```

`list` returns both names and values — vars are not sensitive. At job launch the dispatcher reads every var declared in the job type's `vars:` list and injects them as env vars into both work and eval containers. If any declared var is missing at launch, the job transitions to Escalated.

---

### 8.2 Secrets

Secret values are stored in NATS KV (`secrets.{owner}.{project}.{name}`) encrypted with [age](https://github.com/FiloSottile/age) (X25519).

The platform generates an age keypair at init time:
- **Public key** — available to the API layer; used to encrypt values on write (`PUT /secrets/{name}`)
- **Private key** — mounted into the dispatcher at runtime; never exposed outside it

```rust
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> Result<()>;
    async fn get(&self, owner: &str, project: &str, name: &str) -> Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> Result<()>;
    async fn list(&self, owner: &str, project: &str) -> Result<Vec<String>>;  // names only
}
```

`list` returns names only — values are never returned to callers outside the dispatcher. The dispatcher decrypts values at job launch and injects them as env vars. Containers never see the age key or the KV bucket. If any declared secret is missing at launch, the job transitions to Escalated.

Key rotation requires re-encrypting all values with the new public key — a one-time admin operation exposed as a platform CLI command.

---

## Part 9: Knowledge

### 9.1 KO Model

Knowledge Objects follow a `(subject, predicate) → object` model. Each KO is a discrete fact retrievable in O(1) by `(subject, predicate)` key. Three scopes within the single `knowledge` KV bucket (see §1.4 bucket model):

- **Global** (`global.*` keys) — stack conventions, preferred tools and libraries; not platform-specific
- **Team** (`{owner}.*` keys) — team practices, architectural patterns, coding standards
- **Project** (`{owner}.{project}.*` keys) — project-specific facts, decisions, and context

Scope resolution: when deduplicating KOs by `(subject, predicate)`, narrower scopes win (project overrides team overrides global).

**Storage encoding:** subjects and predicates may contain any characters (including `.` and `/`); they are base64url-encoded in KV keys (see §1.4). An O(1) `get` encodes both parts and reads the key directly; `list` by subject is a prefix scan on `{scope}.{b64url(subject)}.`.

---

### 9.2 CRUD and API

All knowledge operations go through the NATS subjects in §6.1 and HTTP routes in §6.2. No separate knowledge service — the dispatcher handles `req.knowledge.*` subjects directly against NATS KV.

`subject?` is optional on `list` operations — omitting it returns all KOs in the bucket.

---

### 9.3 Injection Pipeline

See §4.4 for the full injection pipeline. Summary:

1. **At job creation**: operator-supplied tags are merged with job type default `knowledge` tags to form `knowledge_tags` on the job instance record
2. **At launch (work containers only)**: dispatcher makes three separate list requests per tag (`req.knowledge.list.global`, `req.knowledge.list.{owner}`, `req.knowledge.list.{owner}.{project}`, each with `{ "subject": tag }`), collects KOs from all three buckets, deduplicates by `(subject, predicate)` with narrower scopes winning, and injects via provider-specific mechanism (see §4.3 and §4.4)
3. **At runtime**: agent queries additional KOs via `chuggernaut-ko` MCP server without dispatcher involvement

---

## Part 10: Security

### 10.1 Container Isolation

Job containers run agent code the platform didn't write. Constraints enforced by the dispatcher at launch:

- No privileged mode
- No host network
- No host volume mounts
- Resource limits from job spec (`cpu`, `memory`, `task_timeout`) enforced as container runtime constraints
- Ephemeral filesystem — wiped on exit
- **Egress**: internet access permitted (agents need to pull dependencies, call external APIs); platform-internal addresses blocked — a dedicated bridge network with host firewall rules on the Docker backend, NetworkPolicy on k8s. Containers reach NATS only through the injected `NATS_URL` with their scoped token (see §7.4) — not via free network routing

Image signing deferred.

---

### 10.2 Secrets Discipline

- Secrets exist in two places only: NATS KV (age-encrypted at rest) and container env vars (ephemeral, process-scoped)
- Plaintext values never written to git, task records, logs, or event streams
- Job definitions declare secret names only — never values
- The age private key is dispatcher-only; containers never see it
- `SecretStore.list` returns names only; `get` is called only by the dispatcher at launch
- Eval prompts should instruct agents not to include secret values in findings or notes — the platform cannot enforce this mechanically

---

### 10.3 Audit Trail

Three layers:

- **NATS event stream** — every job and task state transition published to `job.events.*` (see §6.3); append-only; the primary audit log for all execution activity
- **Task results** — every human task completion records operator identity (`operator` field in `TaskResult::Human`), timestamp, and notes; who approved what and when is in the task log in `tasks.*` KV
- **Git history** — squash-merges to default branch; commit message references job seq so any commit is traceable to its originating job

Continuous security audit and inter-service mTLS deferred.

---

### 10.4 Frontend Security

PWA served from the same axum origin as the API. Authentication via `httpOnly; Secure; SameSite=Strict` cookie containing the JWT — set on `POST /auth/login`, sent automatically with every subsequent request. XSS cannot read the token; CSRF is prevented by `SameSite=Strict`. TLS enforced everywhere: an ACME reverse proxy (e.g. Caddy) in front of the API on Docker deployments; cert-manager + Let's Encrypt on the k8s ingress.

---

## Part 11: Mobile / PWA

PWA — single codebase, installable on mobile home screen, served from the axum API server. Designed mobile-first; works on desktop as the primary operator interface.

**Stack:** React + TypeScript + Vite. React Flow (xyflow) for the graph view; `react-diff-view` for diff rendering; `vite-plugin-pwa` for the manifest and service worker. State layer is thin (Zustand or React context) — the SSE stream is the source of truth and the client mostly projects it. Chosen primarily for agent-writability: v2's premise is that agents implement the work, and React/TS is where model output is most reliable.

The SSE event stream (see §6.4) is the data backbone for the UI — the client connects once per project and receives all state changes in real time. No polling.

**Core screens:**
- **Task inbox** — pending Human tasks across all jobs; primary operator interaction surface; sourced from `GET .../tasks/pending`
- **Graph view** — DAG visualization, job states, live updates via SSE
- **Job detail** — state, task log, agent status/progress via `ChannelStatus`, diff for the job branch
- **Escalation flow** — read findings, provide context, complete or fail the escalation task

**Diff rendering** uses `react-diff-view` over the unified diff returned by `GET .../diff/{seq}` — the platform does not implement its own diff view.

**Push notifications** via Web Push API for task inbox alerts. VAPID keypair generated at platform init. The public key is stored at `platform.vapid.public` in NATS KV for distribution to clients; the private key is mounted into the API layer at runtime. Clients register their W3C `PushSubscription` via `POST /api/v1/push/subscribe` (see §6.2); subscriptions are stored in NATS KV at `push.{user_id}.{subscription_id}`.

**Delivery mechanism:** the API layer runs a background task per instance that subscribes to the `job-events` stream with an ephemeral push consumer. For each `task-created` event where `kind` is `Human`, it reads all `push.*` KV entries for users who hold Member+ role on the affected project and enqueues Web Push notifications asynchronously (not blocking event consumption). Notifications are sent using the VAPID private key. Delivery failures (expired subscriptions, HTTP errors from the push service) are silently discarded — they do not affect task execution or event consumption. Push notifications are best-effort: if the API layer is down when a Human task is created, the notification is not re-sent when it comes back up. Operators can always check pending tasks in the task inbox. The notification payload contains: `job_seq`, `project`, `task_id`, and a human-readable summary (e.g. `"New task: security-review on job/42"`).

---

## Part 12: Platform Init and Admin CLI

### 12.1 Bootstrap (`chuggernaut init`)

`chuggernaut init` is a one-time idempotent setup command run before the platform is started for the first time. It must be run from a machine with access to the NATS server and the persistent volume that will hold bare repos.

**Steps performed:**

1. **Keypair generation** (skip if files already exist at the configured paths):
   - JWT RS256 keypair — `jwt_private.pem`, `jwt_public.pem`
   - SSH CA keypair — `ssh_ca`, `ssh_ca.pub`
   - age encryption keypair — `age_private.key`, `age_public.key`
   - VAPID keypair — `vapid_private.pem`, `vapid_public.pem`
   All keys are written to the path specified by the deployment config (bind-mounted into services at runtime).

2. **NATS infrastructure creation** (idempotent; skip if already exists):
   - Create all KV buckets defined in §1.5 with the configured replica count
   - Create `job-events` and `channel-inbox` JetStream streams defined in §1.5
   - Store VAPID public key at `platform.vapid.public` in NATS KV

3. **Default admin user** — if `--admin-email` and `--admin-password` are provided, create the user record (argon2id hash) at `users.{email}` in NATS KV with `platform_admin: true`. Skipped if the user already exists.

Private keys are **never** stored in NATS KV. They are written to local files and must be manually mounted into the dispatcher and API layer via the deployment's secret mechanism (k8s Secrets or bind-mounted files).

---

### 12.2 Project Creation (`chuggernaut admin project create`)

`chuggernaut admin project create --owner {owner} --name {project} [--default-branch main]`

**Steps performed:**

1. Validate `{owner}/{project}` does not already exist (check `counters.{owner}.{project}` in NATS KV)
2. Initialize the sequential ID counter: set `counters.{owner}.{project}` to `0`
3. Initialize a bare git repository on the persistent volume at `{repos_root}/{owner}/{project}.git`; set `HEAD` to `refs/heads/{default-branch}` (via `git symbolic-ref HEAD`)
4. Create an initial empty commit on the default branch so `HEAD` is a valid ref

**Default branch storage:** the default branch name is stored in the git repository's `HEAD` symref (`git symbolic-ref HEAD` returns `refs/heads/{branch}`). The dispatcher reads this at runtime for all operations that reference the default branch (squash-merges, `base_ref` computation, `BASE_BRANCH` env var). There is no separate NATS KV entry for the project's default branch.

Buckets are fixed and created once at platform init (see §1.4 bucket model, §12.1) — no per-project provisioning. Per-project keys appear lazily as jobs, tasks, secrets, vars, and knowledge objects are created. The `rdeps` index for a project is implicitly empty until the first job is created.

---

### 12.3 Admin CLI Reference

All admin operations are performed via `chuggernaut admin ...`. No HTTP surface for admin operations.

```
# Platform bootstrap
chuggernaut init
  --nats-url <url>         NATS server URL (default: nats://localhost:4222)
  --repos-root <path>      Path for bare git repos (default: /data/repos)
  --keys-dir <path>        Path to write/read keypairs (default: /data/keys)
  --admin-email <email>    Create initial platform admin user (optional)
  --admin-password <pass>  Password for initial admin user (required if --admin-email set)

# User management
chuggernaut admin user create --email <email> --password <pass> [--admin]
chuggernaut admin user list
chuggernaut admin user role set --email <email> --project <owner/project> --role <admin|member|viewer>
chuggernaut admin user role unset --email <email> --project <owner/project>
chuggernaut admin user delete --email <email>

# Project management
chuggernaut admin project create --owner <owner> --name <project> [--default-branch <branch>]
chuggernaut admin project list [--owner <owner>]

# Ingest tokens (see §13.2)
chuggernaut admin ingest-token create --project <owner/project> --source <source>   # prints token once
chuggernaut admin ingest-token list --project <owner/project>
chuggernaut admin ingest-token delete --project <owner/project> --source <source>

# Secret key rotation
chuggernaut admin secret rotate-key
  --old-private-key <path>    Current age private key (for decryption)
  --new-public-key <path>     New age public key (for re-encryption)
  Re-encrypts all secrets in all projects with the new public key.
  The new private key must be deployed and the old key discarded after this completes.
```

---

### 12.4 Provider and Model Defaults

Platform-level agent provider and model defaults are supplied as dispatcher configuration at startup — not stored in NATS KV. In v1, the fallback chain is: job-type declaration → platform config default. Per-project and per-team defaults are deferred.

Dispatcher configuration (environment variables or config file):

```
AGENT_PROVIDER_DEFAULT   claude | codex      Required. No built-in default — dispatcher refuses to start without this.
AGENT_MODEL_DEFAULT      string              Optional. If unset, the provider's built-in default model is used.
TRIAGE_IMAGE             string              Optional. Platform image for operator-dispatched triage agents (§1.2). Provider/model reuse AGENT_PROVIDER_DEFAULT / AGENT_MODEL_DEFAULT. Unset → the triage action is unavailable (422). A platform-level image (rather than the failing job's own type image) so triage works uniformly across agent/command/human job types.
```

If a job type declares `provider` and/or `model` at the `work:` level or per evaluator, those override the platform defaults for that job or evaluator. If neither the job type nor the platform config specifies a provider, the dispatcher fails to start with a configuration error.

---

## Part 13: Task Factories and Ingest

External activity — error-tracker alerts, business metrics, user feedback submissions — can drive the job graph. A **task factory** binds an inbound event stream to a **triage agent** that decides, per batch of events, whether to create jobs. Chuggernaut deployments are per-consumer: factories are project-owned configuration, versioned in the project repo like job types.

### 13.1 Model

```
external system → POST .../ingest/{source} → ingest.{owner}.{project}.{source} (JetStream)
                                                      │
                                     factory (durable consumer, batching window)
                                                      │
                                         triage job (work.type: agent, auto-run)
                                                      │
                                    create_job MCP tool → new jobs (Frozen, or
                                    auto-released per factory policy)
```

Factories never create jobs directly — an agent is always in the loop. A "dumb" factory is a trivial triage prompt ("create one `handle-feedback` job per event"). Direct payload→job templating without an agent is deferred.

### 13.2 Ingest

**HTTP surface:** `POST /api/v1/projects/{owner}/{project}/ingest/{source}` (see §6.2). External producers authenticate with a per-source **ingest token** passed as `Authorization: Bearer {token}` — external systems cannot hold JWT cookies. Tokens are generated by `chuggernaut admin ingest-token create --project {owner}/{project} --source {source}` (printed once, stored argon2id-hashed at `ingest-tokens.{owner}.{project}.{source}`). Rotation = re-run create; revocation = `ingest-token delete`.

**Internal producers** (services already inside the deployment holding NATS credentials) may publish to `ingest.{owner}.{project}.{source}` directly, bypassing HTTP.

**Event envelope** — the API layer wraps every accepted payload:

```rust
pub struct IngestEvent {
    pub source: String,
    pub received_at: DateTime<Utc>,
    pub payload: serde_json::Value,   // the POST body, verbatim
}
```

`{source}` is a subject component: `[A-Za-z0-9_-]+`, no dots. Payloads over 1 MiB are rejected with 413. The ingest stream retains 30 days (§1.5); ingestion is fire-and-forget for the producer — 202 means "durably appended", not "triaged".

### 13.3 Factory Definition

`factories/{name}.yaml` in the project repo. Like all live configuration derived from the repo, factory definitions are read from the **default branch HEAD**; the dispatcher reloads them on startup and after every squash-merge to the default branch. (Job types pin to `base_ref` per job; factories are standing configuration and track HEAD.)

```yaml
name: string                   # required; unique within the repo
source: string                 # required; the ingest source this factory consumes
triage: string                 # required; job type name (must be work.type: agent) used for triage jobs
enabled: bool                  # optional; default true
batch_window: duration         # optional; default 5m — wait this long after the first
                               #   unconsumed event before launching triage
max_batch: int                 # optional; default 100 — launch immediately when reached
auto_release: bool             # optional; default false — release policy for jobs the
                               #   triage agent creates (see §13.4)
```

One factory per source is the expected shape, but multiple factories may consume the same source (each gets its own durable consumer). Validation (at reload): `triage` references an existing job type with `work.type: agent`; `source` is well-formed. Invalid factory files are skipped and reported via a `factory-invalid` event; they never block dispatch.

### 13.4 Factory Semantics

The dispatcher runs one durable JetStream consumer per enabled factory on `ingest.{owner}.{project}.{source}`.

**Batching:** on the first unconsumed event, start `batch_window`; when the window elapses or `max_batch` accumulates, create a triage job. **At most one triage job per factory is in flight** (non-terminal) at a time — events arriving while one runs simply accumulate for the next batch. This is the backpressure mechanism: an error tracker in a crash loop produces bigger batches, not more jobs.

**Triage jobs** are regular jobs (§2.1 state machine applies) with `factory` provenance set, no dependencies, created **and immediately released** by the dispatcher — a factory whose triage waits for operator release would not be a factory. The event batch is delivered to the triage container as a JSON array of `IngestEvent` at `/chuggernaut/events.json` (injected, same mechanism as the prompt file, §4.3). Events are acked on the consumer only when the triage job reaches a terminal state, so a dispatcher crash or a revoked triage job redelivers the batch.

**Job creation by the triage agent:** the `create_job` MCP tool (§4.2), backed by the extra JWT permission (§7.4). Created jobs:
- carry `factory: {factory_name}` on the record and `job-created` event
- start Frozen; if the factory declares `auto_release: true`, the dispatcher immediately runs release validation (§2.2) and transitions them to Ready/Blocked — validation failures leave the job Frozen and surface a `factory-release-failed` event rather than escalating
- may declare `deps` on other jobs created *in the same triage run* (enables the agent to plan a small graph, e.g. `investigate → fix → verify`)

Creating zero jobs is a normal, successful triage outcome (event batch judged noise). The triage job type may declare evaluators like any job; the default (no `eval`) auto-passes. Triage jobs typically produce no commits, so their squash-merge is the standard no-op (§5.1).

**Operator visibility:** factory-created Frozen jobs surface exactly like Human tasks in spirit — a Web Push notification (§11) fires on `job-created` events carrying `factory` provenance when `auto_release` is false, so the operator's approval gate is one tap.

### 13.5 New Events

| Event type | Trigger |
|---|---|
| `factory-triggered` | Batch window closed; triage job created; includes `factory`, `batch_size`, triage job seq |
| `factory-release-failed` | `auto_release` validation failed for a factory-created job; includes errors |
| `factory-invalid` | Factory file failed validation at reload; includes `factory`, error |

---

## Appendix: Infrastructure Summary

| Component | Default (self-hosted) | Cloud alternative |
|---|---|---|
| Orchestration / state / events | NATS JetStream | NATS JetStream (managed) |
| Container execution | Docker socket (v1 default); k3s for multi-node | EKS, GKE, AKS |
| Artifact store | _(deferred)_ | S3, GCS, Azure Blob |
| Secrets | age-encrypted NATS KV | swap `SecretStore` impl |
| Variables | NATS KV | — |
| Image registry | Harbor or Zot | ECR, GCR, GHCR |
| Identity / access | JWT (RS256) + SSH CA + NATS KV | Zitadel, Keycloak, Ory (swap `AuthProvider` impl) |
| Version control | git CLI + bare repos on disk | — |
| API | axum | — |

Platform init generates: JWT RS256 keypair, SSH CA keypair, age keypair, VAPID keypair. All private keys are mounted into services at runtime via the deployment's secret mechanism (k8s Secrets in k8s deployments, bind-mounted files in Docker deployments) — never stored in NATS KV. The JWT public key is also mounted into the API layer for token verification; all other private keys are dispatcher-only. See §12.1 for the full bootstrap procedure.

---

## Appendix: Deferred

- **Dependency invalidation**: no automatic invalidation. Terminal jobs are immutable. Any fix must be appended as a new job.
- **Graph replay**: NATS event stream is the version history. Graph state at any point in time is reconstructable by replaying `job.events.*`. No explicit snapshot mechanism needed.
- **Multi-region dispatcher pools**: add a region dimension to the work queue subject when needed.
- **MFA / OAuth2 login**: deferred until user growth warrants it.
- **Image signing**: cosign verification at dispatcher launch time. Deferred.
- **Continuous security audit**: standard security evaluator prompts shipped with the platform. Deferred.
- **Inter-service mTLS**: deferred; NATS scoped credentials are the primary security boundary within the cluster.
- **Binary artifact store**: S3/Minio for non-git artifacts. Deferred; all work product in VCS for v1.
- **macOS bare metal dispatchers**: required for Xcode builds. Execution model needs separate design.
- **Commit signing**: GPG-signed squash-merges. Deferred.
- **Schema registry**: available as a platform service for applications to use; not a platform primitive.
- **User git CLI (`chuggernaut` client)**: a wrapper CLI that transparently refreshes SSH certificates (via `POST /auth/ssh-cert`) before invoking `git`. In v1, users refresh SSH certs manually via the API or their own tooling and use `git` directly with the certificate.
- **Project/team-level provider defaults**: in v1 the fallback chain is job-type declaration → platform config. Per-project and per-team agent provider/model overrides are deferred.
- **Dependency caching for agent containers**: per-project build/dependency caches (cargo registry, node_modules) via persistent volumes or a pull-through registry cache. v1 mitigation: bake toolchains and dependencies into the declared `image`.
- **k8s-Secret-based secret injection**: dispatcher writes a Kubernetes Secret referenced by the Job spec instead of decrypting secrets into env vars it assembles itself, keeping plaintext out of the dispatcher's launch path. v1 injects env vars directly.
- **Direct-mode factories**: event → job templating without a triage agent (payload→job-fields mapping mini-language). v1 factories are triage-only (§13.1); a trivial triage prompt covers the direct case at the cost of agent tokens.
