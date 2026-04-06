# Chuggernaut v2 — Platform Specification

## Introduction

Chuggernaut is an AI-native software delivery platform. Instead of developers writing code directly, they define a **job graph** — a DAG of work units — and AI agents execute each job: implementing features, running evaluations, and iterating until the work passes. Humans stay in the loop at planning time and at review gates, but agents do the heavy lifting in between.

Jobs are declarative: you describe what needs to happen (image, resources, inputs, eval criteria) and the platform handles scheduling, retries, and state transitions. The job graph drives everything — it determines what runs, in what order, and whether the output is acceptable.

## Philosophy

The job graph is the source of truth, not issues or PRs. Version control is one output of the platform, not its center of gravity. Agents do virtually all implementation work; the operator's primary task is planning and reviewing the graph. Every design decision optimizes for consistency and simplicity over performance, since execution is dominated by long-running agent containers.

---

## Core Concepts

### Job Node

The fundamental primitive. A job is a node in a DAG with:
- Named dependency declarations (DAG edges; data transfer is implicit through VCS)
- Optional eval criteria (omit or leave empty for automatic pass after work success)
- A strict state machine (see below)
- A resource envelope and timeout

### Job Type

Declarative YAML, one file per job type, lives under `jobs/` in the repo and is version controlled. Declares only the contract — image, resources, input names, eval criteria, retry limits, secrets, vars. No instance-specific wiring, no steps, no imperative logic.

Valid fields per `work.type`:

| Field | `agent` | `command` |
|---|---|---|
| `image` (top-level) | required | required |
| `resources` (top-level) | optional | optional |
| `work_retries` (top-level) | optional | optional |
| `eval_retries` (top-level; applies only to agent-type evaluators; accepted-but-no-op if no agent evaluators are declared) | optional | optional |
| `rework_limit` (top-level) | optional | disallowed |
| `eval` (top-level) | optional | optional |
| `prompt` | required | disallowed |
| `provider` | optional | disallowed |
| `model` | optional | disallowed |
| `run` | disallowed | required |

Valid fields per evaluator `type`:

| Field | `command` | `agent` | `human` |
|---|---|---|---|
| `name` | required | required | required |
| `run` | required | disallowed | disallowed |
| `prompt` | disallowed | required | required |
| `provider` | disallowed | optional | disallowed |
| `model` | disallowed | optional | disallowed |
| `secrets` | optional | optional | disallowed |
| `required` | optional | optional | optional |

`work.type: human` is deferred. Human participation is available in two places: as an evaluator (`eval: [{ type: human }]`) and as an escalation handler when work retries or rework cycles are exhausted. Top-level human work jobs require a separate inbox-backed execution model and will be specified in v2.

```yaml
# jobs/implement-endpoint.yaml — type: agent
name: implement-endpoint
image: registry.acme.com/agents/impl:latest
work:
  type: agent
  prompt: prompts/work/implement-endpoint.md
  provider: claude      # claude | codex — optional, falls back to project → team → platform default
  model: claude-sonnet-4-6   # optional override
resources:
  cpu: 2
  memory: 4Gi
  timeout: 2h
work_retries: 3     # work task container retries before escalation
eval_retries: 1     # per-agent-evaluator infra retry budget (container crash, network error); default 1; no-op if no agent evaluators are declared
rework_limit: 2     # max rework cycles after the initial; rework_limit: 2 → cycles 1, 2, 3 permitted; failure at end of cycle 3 → escalation
inputs:
  - name: spec         # named dependency slots; declare DAG edges, not data paths
  - name: codebase
eval:
  - name: unit-tests
    type: command
    run: cargo test --no-fail-fast
    # required: true  ← default; omit unless setting to false
  - name: security-review
    type: agent
    prompt: prompts/eval/security-review.md
    provider: claude          # optional per-evaluator override
    model: claude-opus-4-6
    secrets: [GITHUB_TOKEN]   # evaluator-specific secrets; not inherited from top-level
  - name: architecture-review
    type: agent
    prompt: prompts/eval/architecture-review.md
    required: false           # advisory only — failure doesn't force rework
  - name: human-approval
    type: human
    prompt: prompts/eval/human-approval.md
knowledge:                 # default tags for upfront KO injection at launch
  - rust
  - rest-api
secrets: [GITHUB_TOKEN]    # work container only; evaluators declare their own
vars: [RUST_EDITION]       # available to work container and all eval containers
```

```yaml
# jobs/deploy-staging.yaml — type: command
name: deploy-staging
image: registry.acme.com/runners/deploy:latest
work:
  type: command
  run: scripts/deploy.sh staging
resources:
  timeout: 30m
work_retries: 2
```

**Command work jobs must be idempotent.** `work_retries` will re-run the command on failure; the command is responsible for handling any partial side effects from a prior attempt safely. The platform does not track whether external mutations have occurred — it only observes exit code. If your operation cannot be made safe to retry (e.g. a non-idempotent external API call), set `work_retries: 0`.

### Naming Conventions

**`project`** in JSON records is always the combined `"{owner}/{repo}"` slug (e.g. `"acme/api"`). In NATS KV keys and subjects it is split into `{owner}` and `{project}` components (e.g. `jobs.acme.api.42`), where `{project}` is the bare repo name. HTTP routes follow the same split (`/api/v1/projects/{owner}/{project}/...`). These are the same project — three representations of one identifier, derived consistently: `project_slug = "{owner}/{project}"`.

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
  "knowledge_tags": ["rust", "rest-api", "payments/stripe-integration"],
  "created_at": "2026-04-05T10:00:00Z"
}
```

IDs are sequential integers scoped per project (e.g. `acme/api` job #42), maintained via a counter in NATS KV. Human-friendly for communication; uniqueness is guaranteed within the project namespace. `retry_count` and `rework_count` are not stored on the job record — they are derived from the task log (`attempt` on work tasks and `cycle` on tasks respectively).

Each job works on a dedicated branch `job/{seq}`. The branch name is deterministic and stored on the instance at creation time; the actual git branch is created from the default branch when the job first enters Work. The DAG guarantees that all upstream dependencies are Done (and their branches squash-merged to default) before a dependent job starts. Data transfer between jobs is implicit through VCS — downstream jobs pull the default branch and upstream work is already there.

Named inputs are dependency declarations only — they establish DAG edges and ordering, not data routing. Release validation runs on every release request — both per-job (`POST .../jobs/{seq}/release`) and graph-level (`POST .../graph/validate`, `POST .../graph/release`). The dispatcher enforces all of the following before any job leaves Frozen:

**Graph wiring (input wiring rules):**
- Every named input in each job instance references an existing upstream job instance in the same project
- No job instance declares an input that refers to itself (no self-edges)
- No job instance references a downstream job as an input (no cycles — full topological sort required; any cycle is a validation error)
- Every input name declared in the job instance matches a name declared in the job type's `inputs:` list (no undeclared inputs)
- Every input name declared in the job type's `inputs:` list has a corresponding wired instance ID in the job instance (no dangling inputs)

**Static configuration:**
- The job type file (`jobs/{type}.yaml`) exists in the repo at the default branch
- For `work.type: agent` jobs: the `work.prompt` path exists in the repo at the default branch
- For each agent evaluator in `eval:`: the evaluator's `prompt` path exists in the repo at the default branch
- Every secret named in the job type's `secrets:` list (top-level and per-evaluator) has an entry in the `secrets.*` KV bucket
- Every var named in the job type's `vars:` list has an entry in the `vars.*` KV bucket

Any release request that fails validation is rejected with a list of offending job instances and the specific rule violated. A job with no inputs declared in its type skips the wiring rules (there is nothing to wire).

### State Machine

```
Frozen → Ready | Blocked → Work → Evaluation → Done
                              ↑         |
                              └─────────┘  (rework: eval failed, under rework limit)
Work → Escalated                          (work retries exhausted OR rebase conflict post-work)
Evaluation → Escalated                    (rework limit exhausted OR squash-merge conflict post-eval)
Escalated → Work | Evaluation | Revoked
Frozen | Blocked | Ready | Escalated → Revoked  (operator revoke)
Done, Revoked — terminal
```

- **Frozen** — created but awaiting operator approval; no execution begins until explicitly released
- **Blocked** — waiting on upstream dependencies; auto-transitions to Ready when all deps reach Done
- **Ready** — queued for execution
- **Work** — work task executing; job stays here across retries within the current cycle
- **Evaluation** — evaluation tasks running (commands, agents, human); job stays here until all tasks resolve; rework loops job back to Work
- **Escalated** — human intervention required; triggered by work retries exhausted, rebase/merge conflict, or rework limit exceeded; operator resolves via task inbox
- **Revoked** — terminal; only reachable from Frozen, Blocked, Ready, or Escalated. Jobs in Work or Evaluation cannot be directly revoked — the operator must escalate them first (or wait for them to reach Escalated naturally), then revoke from Escalated. On transition to Revoked, the dispatcher immediately cascades Revoked to all direct and transitive dependents that are in Frozen or Blocked state (they can never complete since an upstream will never reach Done). Ready dependents are also cascaded. Dependents already in Work, Evaluation, or Escalated are left in their current state and must be handled explicitly by the operator

The dispatcher is the sole writer of `jobs.*` KV — including creation. All transitions flow through the dispatcher; no other service writes job records. The API layer and other consumers may read job records.

Job state is the authoritative phase marker. Fine-grained execution detail lives in the task log (see Tasks).

### Graph Modes

Two primary modes:

1. **New feature / project**: graph is statically known and operator-approved before any work begins. No execution until review is complete.
2. **Ongoing development**: jobs are added only. Instead of removing job nodes, they are revoked. Large new features are planned and reviewed statically before launching.

Operators submit job creation requests via `POST /jobs`; the dispatcher creates the job record in KV and publishes the `job-created` event. All jobs start in Frozen state. No job enters the graph without explicit operator action.

### Tasks

Tasks are the unit of execution within a job's Work and Evaluation phases. They are a chronological log — created by the dispatcher as it transitions job state, never referencing each other. No task graph; no task dependencies.

Each rework loop is a **cycle**. Cycle 1 = first Work execution + first Evaluation. Cycle 2 = rework Work + second Evaluation. Tasks carry a cycle number tying them to the rework loop.

`rework_limit` is the maximum number of rework cycles permitted after the initial one. With `rework_limit: 2`, cycles 1, 2, and 3 are permitted; eval failure at the end of cycle 3 triggers escalation. Equivalently: escalation occurs when `current_cycle > rework_limit + 1`.

```rust
pub struct Task {
    pub id: u64,                          // sequential within job, 1-indexed
    pub job_seq: u64,
    pub project: String,
    pub phase: TaskPhase,
    pub cycle: u32,                       // increments on each rework
    pub kind: TaskKind,
    pub state: TaskState,
    pub attempt: u32,                     // 1-indexed; each retry creates a new task record with attempt+1
    pub container_id: Option<String>,     // Docker container ID; set when container is launched; None for Human tasks
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,      // set when task transitions to Running
    pub completed_at: Option<DateTime<Utc>>,
}

pub enum TaskPhase { Work, Evaluation }

pub enum TaskKind {
    Command { run: String },
    Agent   { provider: String, model: Option<String>, prompt: String },
    Human   { prompt: String },           // surfaces to operator task inbox
}

pub enum TaskState { Pending, Running, Done, Failed }

pub enum TaskResult {
    Work    { summary: Option<String>, structured: Option<serde_json::Value> },
    Command { pass: bool, exit_code: i32, output: String, structured: Option<serde_json::Value> },
    Agent   { pass: bool, structured: Option<serde_json::Value> },
    Human   { pass: bool, structured: Option<serde_json::Value>, action: Option<EscalationAction> },
}

pub enum EscalationAction { Retry, Resolve, Revoke }

pub enum TaskResolution {
    Pass       { structured: Option<serde_json::Value> },
    Fail       { structured: serde_json::Value },              // structured required on fail
    Escalation { action: EscalationAction, structured: Option<serde_json::Value> },  // only valid on escalation Human tasks
}
```

`action` is only set on Human tasks created for escalation (work retries exhausted, rebase conflict, or rework limit exceeded). It is ignored on Human tasks in the normal Evaluation phase.

**Task creation rules:**

| Trigger | Tasks created |
|---|---|
| Job enters Work (new cycle) | One work task (attempt=1) for the job type's `work:` definition |
| Work task fails, `work_retries` available | New task record (same cycle, attempt++) |
| Work retries exhausted | Human task: `{ prompt: "work failed N times — decide" }` → job → Escalated |
| Operator grants `Retry` on escalation | New work task (same cycle, attempt++); `work_retries` budget is **not** reset — if this attempt fails, escalate again immediately (no further automatic retries) |
| Rebase conflict after work success | Human task: `{ prompt: "rebase conflict on job/{seq} — resolve and push" }` → job → Escalated |
| Squash-merge conflict after eval pass | Human task: `{ prompt: "merge conflict on job/{seq} — resolve and push" }` → job → Escalated |
| Job enters Evaluation | One task (attempt=1) per evaluator declared in job type |
| Agent eval task: container exits non-zero with no prior `submit_eval`, `eval_retries` available | New task record (same cycle, attempt++); other eval tasks continue in parallel |
| Agent eval task: retries exhausted | Final task record marked Failed (infra error); this is distinct from a task completing with `pass=false`. Reduce proceeds once all tasks are Done or Failed |
| Eval reduce fails (`work.type: agent`), under rework limit | Job re-enters Work (cycle++); all eval results from this cycle feed into the next work task's rework context |
| Eval reduce fails (`work.type: agent`), rework limit exhausted | Human task: `{ prompt: "eval failed N cycles — decide" }` → job → Escalated |
| Eval reduce fails (`work.type: command`) | `rework_limit` is disallowed for command jobs; eval failure always escalates: Human task → job → Escalated |

`work_retries` counts container failures (crash, timeout, non-zero exit from an infra error) within a single Work phase. `eval_retries` counts the same for individual eval containers. `rework_limit` counts evaluation-driven rework cycles and is entirely separate from both.

**Human tasks** surface in the operator task inbox regardless of which phase or cycle they belong to. A human evaluator in the job type creates a `Human` task in the Evaluation phase — the job stays in `Evaluation` state. The operator's decision (pass/fail + structured JSON) is the task result. The eval reduce applies uniformly to all task results including Human.

**Escalation resolution**: when the operator completes an escalation Human task, `action` drives the next transition — `Retry` re-enters Work in the **same cycle** (the cycle counter does not increment because no evaluation-driven rework has occurred; operator is treating the escalation as a fresh attempt within the current cycle). `Retry` grants exactly one attempt: `work_retries` is not reset, so if the new attempt fails it escalates again immediately. `Resolve` re-enters Evaluation with the current branch (operator has done the work and is submitting it for evaluation). `Revoke` terminates the job. Escalation never bypasses evaluation — `Done` is only reachable via an evaluation pass.

**Example task log:**

```
cycle=1  Work        Agent    attempt=1  Failed            ← infra error; new record created
cycle=1  Work        Agent    attempt=2  Done              ← retry
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
KV:     rdeps.{owner}.{project}.{seq}                   inverse dependency index (dispatcher-maintained)
KV:     counters.{owner}.{project}                      per-project sequential ID counter
KV:     tasks.{owner}.{project}.{job_seq}.{task_id}     task log entries
KV:     channels.{owner}.{project}.jobs.{seq}           agent progress updates
KV:     vars.{owner}.{project}.{name}                   project-scoped variable values (plaintext strings)
KV:     secrets.{owner}.{project}.{name}                age-encrypted secret values
KV:     users.{email}                                   user accounts
KV:     knowledge.global                                global knowledge (tools, libraries, stack conventions)
KV:     knowledge.{owner}                               team-level knowledge
KV:     knowledge.{owner}.{project}                     project-level knowledge
KV:     platform.vapid.public                           Web Push VAPID public key
Stream: job-events  subjects: job.events.{owner}.{project}.{seq}.{event_type}   event bus / audit log
```

### Dispatcher

The **sole writer** of all job state. A single process drives all orchestration, task execution, and state transitions sequentially. No competing writers. All jobs run on Linux containers.

Responsibilities:

1. On `req.jobs.create.*`: write the job record to `jobs.*` KV, then publish `job-created` to the event stream. The `rdeps` index is updated as a best-effort cache after the job write — it is not a source of truth and is always rebuilt from `jobs.*` KV on startup (see restart reconciliation)
2. On job `Done`: read `rdeps` index, check each dependent job's dep states, transition newly-unblocked Blocked → Ready jobs
3. On job `Revoked`: read `rdeps` index, cascade Revoked to all dependents in Frozen, Blocked, or Ready state (transitively); dependents in Work, Evaluation, or Escalated are left in their current state
4. On `req.jobs.release.*`: validate input wiring (all five rules above); if invalid, reject with errors. Check dep states; transition Frozen → Ready (all deps Done) or Frozen → Blocked
5. Pull Ready jobs and execute them (see Dispatcher Execution below)
6. On restart: reconciliation pass — query Docker for each task in `Running` state and resume or resolve it (see Restart Reconciliation below). Any job with all dependencies Done but still Blocked is transitioned to Ready

### Failure Recovery

1. **Timeout scan**: dispatcher periodically scans for tasks in `Running` state where `now - started_at > job.resources.timeout`. The task is marked Failed and retry logic applies. Tasks in `Pending` state are not timed out — the clock starts when execution begins. Human tasks (Evaluation phase and escalation) are excluded from the timeout scan — they have no timeout and no automatic abandonment. A job waiting on an unresolved human task remains blocked indefinitely; operators must manage their task inbox. This is intentional: human review gates are explicit decisions, not time-bounded.
2. **Restart reconciliation**: on dispatcher startup, rebuild the `rdeps` index from scratch by scanning all job records in `jobs.*` KV (the index is a derived cache, not a source of truth). Then apply the following in order:
   1. For each task in `Running` state, query Docker by `container_id`:
      - **Still running**: re-attach to container events and resume monitoring as normal. The container's NATS credentials remain valid for the restart window; if they have since expired the container will be unable to submit and will eventually time out or be reaped by the timeout scan.
      - **Exited 0**: treat as successful completion — proceed to the next execution step (rebase for agent work tasks, result processing for eval tasks).
      - **Exited non-0** or **not found**: treat as failure; apply normal retry logic (retry if under `work_retries` / `eval_retries`, else escalate).
   2. Transition any Blocked job whose dependencies are all Done to Ready.
   The task log in KV is the source of truth for execution state. Docker is required to be reachable at startup; the dispatcher will not start if the Docker socket is unavailable.

---

## Dispatcher Execution

The dispatcher drives all job execution directly. No separate runner service.

**For each Ready job:**

1. Transition job Ready → Work; create work task for cycle 1
2. Inject secrets (decrypted from `secrets.*` KV using age private key) and vars as env vars
3. Issue short-lived scoped NATS JWT for the job (using the NATS operator signing key)
4. Issue short-lived SSH certificate for the job (using the SSH CA private key)
5. Launch container via the configured backend (Docker socket or k3s)
6. Monitor task; on container failure: increment attempt, re-launch if under `work_retries`, else create Human escalation task and transition to Escalated
7. On work task success (`work.type: agent` only): rebase `job/{seq}` onto current default branch. If clean → proceed to step 8. If conflict → create Human escalation task and transition to Escalated. For `work.type: command` jobs, skip this step — no branch commits are produced; proceed directly to step 8
8. Transition job to Evaluation; create one task per evaluator in job type. If the job type declares no evaluators (`eval:` omitted or empty), skip to step 11 — Evaluation is a no-op pass.
9. Fan out evaluation tasks in parallel; monitor each — for **agent** eval tasks: container exit without a prior `submit_eval` is an infra error, retry up to `eval_retries`; if retries exhausted, mark task Failed (infra error). For **command** eval tasks: exit code is the verdict immediately (no retries)
10. Apply eval reduce once all eval tasks are Done or Failed: if any required task is Failed (infra error), skip rework and escalate immediately; if all required tasks are Done and any have `pass=false`, that is a product failure
11. On eval reduce pass: for `work.type: agent` jobs — squash-merge `job/{seq}` to default branch. If clean → transition job to Done. If conflict → create Human escalation task and transition to Escalated; the operator resolves the conflict on `job/{seq}` and re-enters via the `Resolve` escalation action. For `work.type: command` jobs — transition job directly to Done (no branch commits to merge)
12. On eval reduce product failure: for `work.type: agent` jobs — if under rework limit, increment cycle, inject all eval findings into rework context, re-enter Work (step 7 — rebase applies again); if rework limit exhausted, create Human escalation task and transition to Escalated. For `work.type: command` jobs — `rework_limit` is disallowed, so eval failure always escalates immediately (no rework loop)
13. On escalation Human task completion: read `action` and drive next transition — `Retry`: create new work task in the **same cycle** and execute the work task (agent or command, per job type); for agent jobs the existing `job/{seq}` branch is used (operator may have modified it during escalation) and rebase onto default occurs after work task success (step 7), not before. `Resolve`: re-enter Evaluation (step 8); for agent jobs the operator has fixed the branch directly; for command jobs the operator asserts the operation completed successfully and evaluation should proceed. `Revoke`: transition to Revoked

### Environment Variables Injected at Launch

**Work containers:**

```
JOB_ID          42
JOB_PROJECT     acme/api
JOB_BRANCH      job/42
BASE_BRANCH     main
REPO_URL        ssh://git@platform/acme/api.git
NATS_URL        nats://...
NATS_TOKEN      <work-scoped JWT — see Per-Job Machine Credentials>
# secrets (decrypted from age-encrypted NATS KV; named as declared in top-level secrets:)
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
NATS_TOKEN      <eval-scoped JWT — more restricted; see Per-Job Machine Credentials>
# secrets (only those declared in the evaluator's own secrets: field; none by default)
# vars (from NATS KV; named as declared in top-level vars:)
RUST_EDITION    2021
```

Work containers are dumb — they read env vars, clone the repo, branch, do work, commit, and exit. No NATS awareness required beyond optional progress streaming.

### Evaluation Tasks

After a work task succeeds the dispatcher creates evaluation tasks for the current cycle. Command and agent eval containers receive a separate, more restricted credential set (eval-scoped NATS JWT including knowledge read, read-only SSH cert) and only the secrets explicitly declared on their evaluator entry — they do not inherit the work container's secrets. They receive the same job context vars and repo coordinates so they can clone and inspect the full working tree. Results are written to `tasks.*` KV as `TaskResult` variants. The dispatcher waits for all evaluation tasks to complete, then applies the reduce.

### Dispatcher Backends

Interface with two implementations:

- **Docker socket** — local dev and single-node self-hosted
- **k3s** — production, multi-node, resource-constrained runner pools

---

## Evaluation

When Work completes the dispatcher creates one task per evaluator declared in the job type and fans them out in parallel. Three evaluator types:

**`command`** — dispatcher executes a CLI command inside a container on the job branch. Captures exit code and stdout. Dispatcher writes `TaskResult::Command` to `tasks.*` KV.

**`agent`** — dispatcher invokes the configured `AgentProvider` with the eval prompt (path resolved from the repo). The provider launches a container from the evaluator's job `image`, which clones the repo on the job branch, inspects the diff, reasons about the criteria, and calls `submit_eval` to publish its verdict and structured findings to the dispatcher. The dispatcher writes `TaskResult::Agent` to `tasks.*` KV. **Verdict contract**: pass/fail is set by the `pass` field in the `submit_eval` payload — this is the product-level outcome. A non-zero container exit with no prior `submit_eval` call is an infra/runtime error: the dispatcher retries up to `eval_retries` and then marks the task Failed (infra error), which is distinct from a completed task with `pass=false`.

**`human`** — dispatcher creates a Human task in `Pending` state. No process launched. Operator submits a `TaskResolution` via the task inbox; the dispatcher validates it, writes `TaskResult::Human` to `tasks.*` KV, and drives the next state transition.

All eval task results land in `tasks.{owner}.{project}.{seq}.{task_id}`. Dispatcher waits for all N evaluation tasks to reach `Done` or `Failed`, then applies the reduce.

### Reduce

All evaluators are binary pass/fail:

- `Command` — exit code is the verdict: exit 0 = pass, non-zero = fail. The dispatcher captures stdout as `output`. It then attempts to parse the full stdout as JSON; if successful, `structured` is set from the parsed value (e.g. `{ "failed_tests": [...] }`); if stdout is not valid JSON, `structured` is None. Commands that want structured rework context print JSON to stdout; commands that don't just print human-readable text. There is no submit step and no infra-error path — the dispatcher cannot distinguish a container crash from a legitimate test failure, so any non-zero exit is treated as a product failure. `eval_retries` does not apply to command evaluators; if you need retry-on-flake, build it into the command (e.g. `cargo test || cargo test`).
- `Agent` — pass/fail set by the `pass` field in the `submit_eval` payload (product verdict). Non-zero exit without a prior `submit_eval` = infra error, consumed by `eval_retries`. `structured` in `submit_eval` carries optional reasoning (e.g. `{ "notes": "..." }`).
- `Human` — pass/fail set by the operator; `structured` carries the reasoning (e.g. `{ "notes": "..." }`)

Overall result: if any `required` evaluator (default) fails → overall fail. `required: false` evaluators are advisory — their failure is recorded but does not trigger rework.

On overall fail (`work.type: agent`): cycle++; `structured` from all eval results is collected into `AgentRunConfig.eval_context` and passed to the next work task. If `current_cycle > rework_limit + 1`: create Human escalation task, transition job to Escalated.

On overall fail (`work.type: command`): `rework_limit` is disallowed for command jobs; create Human escalation task immediately, transition job to Escalated. No rework loop.

`EvalResult` is the rework context passed to the next work cycle:

```rust
pub struct EvalResult {
    pub evaluator: String,                  // evaluator `name` field from job type
    pub pass: bool,
    pub structured: Option<serde_json::Value>,  // evaluator-defined JSON; shape varies by type
}
```

---

## Artifact Passing

All work product lives in the version control system. Each job works on a dedicated branch (`job/{seq}`); on evaluation pass the dispatcher squash-merges to the default branch. Downstream jobs start from the default branch — upstream work is already there by the time they launch, guaranteed by the DAG dependency ordering.

No separate artifact store for v1. Binary artifact storage (S3/Minio) is deferred.

---

## Channel MCP Server

Two MCP servers are injected into every agent invocation:

- **`chuggernaut-channel`** — bridges Claude processes to NATS for job lifecycle operations. Built on the v1 `crates/channel/` foundation.
- **`chuggernaut-ko`** — scoped knockout KV client for runtime knowledge queries. Connects to NATS using the job's scoped JWT; exposes read access to global, team, and project knowledge buckets.

### Tools

| Tool | Used by | Purpose |
|---|---|---|
| `update_status` | work + eval agents | Write `ChannelUpdate` to `channels.{owner}.{project}.jobs.{seq}` KV |
| `channel_check` | work + eval agents | Poll inbox for messages from operator |
| `reply` | work + eval agents | Send message to operator via NATS outbox |
| `submit_result` | work agents | Publish work summary to dispatcher via `req.work.submit.*` request-reply; dispatcher writes task result and transitions to Evaluation |
| `submit_eval` | eval agents | Publish eval verdict and findings to dispatcher via `req.eval.submit.*` request-reply; payload must include `pass: bool`; dispatcher writes `TaskResult::Agent` to `tasks.*` KV |

**Completion contract:**

- **Work containers** — `submit_result` is optional structured context. Task outcome is determined by container exit code: exit 0 = work succeeded; non-zero = infra/runtime failure (retried per `work_retries`). A container exiting 0 without calling `submit_result` is valid and produces a work task with no structured summary.
- **Command eval containers** — no submit step and no infra-error path. Exit code is the verdict: exit 0 = pass, non-zero = fail. `eval_retries` is not applicable.
- **Agent eval containers** — `submit_eval` is required to record the product verdict. The dispatcher uses the `pass` field in the payload as the evaluation outcome. A non-zero container exit with no prior `submit_eval` is treated as an infra error and retried per `eval_retries`; it is not a product-level failure.

**NATS request reliability**: `submit_result` and `submit_eval` are request-reply calls. The agent SDK must retry these with bounded backoff until an ack is received. This makes submissions survive brief dispatcher restarts — the dispatcher comes back up and the in-flight request succeeds on the next retry. If the dispatcher remains down until the NATS JWT expires, the container will no longer be able to submit and should exit non-zero; normal timeout and retry logic then applies on the next dispatcher start.

`submit_pr` and `submit_review` from v1 are replaced by `submit_result` and `submit_eval` respectively.

### Channel Update

```rust
pub struct ChannelUpdate {
    pub message: String,
    pub percent: Option<u8>,
}
```

Written by `update_status`; overwritten on each call. Used by the UI to display live agent progress on the job detail screen.

### Operator → Agent Messages

Operators send messages to a running agent via `POST .../messages`. Each message is handled in two steps:

1. Written to the `channels.*` KV entry as `last_message` (overwrites any previous operator message — no history, most recent only).
2. Published to `channel.inbox.{owner}.{project}.{seq}` as a NATS subject for live push delivery. If no agent is subscribed the live publish is a no-op; the KV write has already persisted the message for polling.

```rust
pub struct OperatorMessage {
    pub text: String,       // free-form text; delivered to the agent as a <channel> tag or via channel_check
    pub sent_at: DateTime<Utc>,
}
```

Agent replies (`reply` tool) are published to `channel.outbox.{owner}.{project}.{seq}` and stored in the `channels.*` KV entry alongside the current `ChannelUpdate` and `last_message`. The KV entry holds only the **most recent** reply — there is no message history. The job detail view exposes the current status update, last agent reply, and last operator message.

### GET .../status Response

`GET /api/v1/projects/{owner}/{project}/jobs/{seq}/status` returns:

```rust
pub struct ChannelStatus {
    pub job_seq: u64,
    pub update: Option<ChannelUpdate>,      // most recent update_status call; None if none yet
    pub last_reply: Option<OperatorMessage>, // most recent agent reply; None if none yet
    pub last_message: Option<OperatorMessage>, // most recent operator message; None if none yet
}
```

### Supports Two Modes

- **Push notifications** (`claude/channel` experimental capability) — dispatcher can interrupt Claude mid-run with a message delivered as a `<channel>` tag, via the `channel.inbox.*` NATS subject
- **Polling** — Claude calls `channel_check` periodically; reads `last_message` from the `channels.*` KV entry. Suitable for clients that don't support push (e.g. Codex). The guarantee is that polling always sees the most recent message — if multiple messages are sent between polls, only the latest is visible.

---

## Agents

Agents are first-class participants in the state machine, not scripts bolted onto a human workflow.

### Provider Abstraction

The dispatcher invokes agents through an `AgentProvider` trait with per-provider implementations. Provider and model are configured at platform, team, or project level, with job type able to override at the `work:` level or per eval step.

`AgentProvider.run()` is responsible for launching the container using the dispatcher's backend (Docker socket or k3s), injecting the provider-specific agent CLI invocation as the container CMD, monitoring the container until it exits, and returning the result. The declared job `image` provides the full development environment including the agent CLI binary and all runtime tooling.

```rust
#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn run(&self, config: AgentRunConfig) -> Result<AgentOutput, AgentError>;
    fn supports_push_notifications(&self) -> bool;
}

pub struct AgentRunConfig {
    pub image: String,                     // container image from job type
    pub prompt: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,    // composed from knowledge libraries
    pub mcp_servers: Vec<McpServerConfig>,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub eval_context: Vec<EvalResult>,     // empty on cycle 1; populated on rework cycles
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

Structured context is surfaced through MCP tools (`submit_result`, `submit_eval`), which send NATS request-reply messages to the dispatcher — the dispatcher never parses agent stdout. For **work containers**, task outcome is determined by container exit code; calling `submit_result` is optional but recommended for richer rework context. For **agent eval containers**, `submit_eval` is required to record the product verdict — a container exit without a prior `submit_eval` call is always treated as an infra error, not a pass or fail.

**`ClaudeProvider`** — injects container CMD: `claude -p {prompt} --model {model} --append-system-prompt {system_prompt} --mcp-config {json}`

**`CodexProvider`** — mounts `.codex/config.toml` (containing the MCP server config) into the container at `/repo/.codex/config.toml` before launch; injects container CMD: `codex exec {prompt} --model {model}` (system prompt prepended to the task prompt, no native flag)

Push notifications (`claude/channel` experimental capability) are Claude-only. Codex agents fall back to polling mode (`channel_check`) automatically — `supports_push_notifications()` governs which mode the channel MCP server starts in.

### Provider Configuration

Declared under `work:` in the job type or per eval step; falls back to project → team → platform default:

```yaml
work:
  type: agent
  prompt: prompts/work/implement-endpoint.md
  provider: claude           # claude | codex
  model: claude-sonnet-4-6   # optional override

eval:
  - type: agent
    prompt: prompts/eval/security-review.md
    provider: claude
    model: claude-opus-4-6   # heavier model for security review
```

### Prompt / Knowledge Libraries

Knowledge Objects (KOs) follow a `(subject, predicate) → object` model (see [arxiv:2603.17781](https://arxiv.org/abs/2603.17781)). Each KO is a discrete fact retrievable in O(1). Three scoped buckets:

- **Global** (`knowledge.global`) — stack conventions, preferred tools and libraries; not platform-specific
- **Team** (`knowledge.{owner}`) — team practices, architectural patterns, coding standards
- **Project** (`knowledge.{owner}.{project}`) — project-specific facts, decisions, and context

#### Upfront injection (tagged)

Job types declare a default set of knowledge tags; operators may add more at job creation time. The union of both sets is the job's `knowledge_tags`. At launch the dispatcher resolves each tag as a KO subject using the `list-by-subject` primitive (`req.knowledge.list.*.{subject}`): it fetches all `(tag, *)` KOs from global → team → project buckets, deduplicating by `(subject, predicate)` with narrower scopes winning. The resulting KOs are concatenated and injected via `--append-system-prompt` for Claude, prepended to the prompt for Codex.

#### Runtime querying (MCP)

Agents also have access to a scoped knockout MCP server at runtime. They can query for further KOs — e.g. project-specific facts written by a prior job — without the dispatcher needing to know what they'll need upfront. The NATS JWT for work containers includes read access to all three knowledge buckets for their project scope.

`CLAUDE.md` / `.claude/CLAUDE.md` in the repo is picked up automatically by `claude -p` and does not need to be in the KO store.

---

## Version Control

Git is a storage layer, not the center of gravity. The platform manages all branches and commits; users rarely interact with git directly.

- **Repos on disk**: stored on a persistent volume, one bare repo per project
- **Dispatcher operations**: all git operations (branch, commit, squash-merge) are performed by shelling out to the `git` CLI. gitoxide is not used — push and merge are incomplete in that library.
- **Diff API**: the platform's axum API serves diffs on demand (`git diff`, `git log`) — no separate web UI needed
- **Direct user access**: rare; served via git-over-SSH against the repo volume. Users authenticate with SSH certificates (see Identity section). No git hosting service (Forgejo, GitHub) required.
- **Branch protection**: enforced in the SSH layer — only the dispatcher identity may push to protected refs (default branch)

---

## API Layer

The API layer is a bridge, not a service. Core responsibilities:

1. **HTTP → NATS request-reply proxy**: translate authenticated HTTP requests into NATS requests, return responses. No orchestration logic in this layer — job state transitions, task management, and graph operations all happen in the dispatcher.
2. **NATS stream → SSE bridge**: subscribe to `job.events.*` streams, forward events to connected HTTP clients.
3. **Authentication and authorization**: validate JWT cookies and enforce permission rules on every request via the `AuthProvider` trait.
4. **Secret value encryption**: on `PUT /secrets/{name}`, encrypt the value with the age public key before forwarding to NATS KV. The API layer never sees the age private key or decrypted values.

All platform-internal communication is via NATS. The dispatcher and all other services speak NATS directly — the API layer is the only component that speaks HTTP, and only because clients require it.

Implementation: axum. Versioning: `/api/v1/` URL prefix.

### NATS Request-Reply Subjects

The API layer publishes to these subjects and awaits a reply. Services subscribe and handle — the API has no knowledge of what's behind each subject.

```
req.jobs.create.{owner}.{project}
req.jobs.get.{owner}.{project}.{seq}
req.jobs.list.{owner}.{project}
req.jobs.release.{owner}.{project}.{seq}
req.jobs.revoke.{owner}.{project}.{seq}
req.graph.get.{owner}.{project}
req.graph.validate.{owner}.{project}
req.graph.release.{owner}.{project}
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
req.knowledge.get.global.{subject}.{predicate}
req.knowledge.set.global.{subject}.{predicate}
req.knowledge.delete.global.{subject}.{predicate}
req.knowledge.list.global
req.knowledge.list.global.{subject}
req.knowledge.get.{owner}.{subject}.{predicate}
req.knowledge.set.{owner}.{subject}.{predicate}
req.knowledge.delete.{owner}.{subject}.{predicate}
req.knowledge.list.{owner}
req.knowledge.list.{owner}.{subject}
req.knowledge.get.{owner}.{project}.{subject}.{predicate}
req.knowledge.set.{owner}.{project}.{subject}.{predicate}
req.knowledge.delete.{owner}.{project}.{subject}.{predicate}
req.knowledge.list.{owner}.{project}
req.knowledge.list.{owner}.{project}.{subject}
req.channel.send.{owner}.{project}.{seq}
req.channel.status.{owner}.{project}.{seq}
req.tasks.list.pending.{owner}.{project}
req.tasks.list.{owner}.{project}.{job_seq}
req.tasks.resolve.{owner}.{project}.{job_seq}.{task_id}
req.work.submit.{owner}.{project}.{seq}
req.eval.submit.{owner}.{project}.{seq}.{task_id}
req.ssh.sign-user-cert
```

### HTTP Surface

```
# Auth
POST   /auth/login                                                  → sets httpOnly JWT cookie
POST   /auth/logout                                                 → clears cookie
GET    /auth/me                                                     → current identity
POST   /auth/ssh-cert                                               → sign user's public key; returns 24h SSH cert

# Operator task inbox
# Surfaces all pending Human tasks across all jobs — escalations and planned human reviews

GET    /api/v1/projects/{owner}/{project}/tasks/pending
       returns all Human tasks in Pending state across all jobs

POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/resolve
       body: TaskResolution

# Per-job task log (read)
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks

# Jobs (read + lifecycle mutations)
POST   /api/v1/projects/{owner}/{project}/jobs                      → forward to dispatcher via req.jobs.create.*; dispatcher creates Frozen job; body: { "type": "implement-endpoint", "inputs": { "spec": 11, "codebase": 22 }, "knowledge_tags": ["payments/stripe-integration"] }
                                                                    knowledge_tags is optional; merged with the job type's default tags to form the instance's full tag set
GET    /api/v1/projects/{owner}/{project}/jobs
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/release        → validate input wiring; Frozen → Ready | Blocked; rejected with validation errors if wiring is invalid
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/revoke         → Frozen | Blocked | Ready | Escalated → Revoked; rejected (409) if job is in Work or Evaluation — escalate first

# Graph (read + validation + release)
GET    /api/v1/projects/{owner}/{project}/graph
POST   /api/v1/projects/{owner}/{project}/graph/validate           → validate all Frozen jobs have valid input wiring before release
POST   /api/v1/projects/{owner}/{project}/graph/release            → release all Frozen jobs with valid input wiring; each transitions to Ready or Blocked; equivalent to calling per-job release on each Frozen job in dependency order

# Event streams (SSE — NATS stream bridged to HTTP)
GET    /api/v1/projects/{owner}/{project}/events               project-wide stream
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/events    per-job stream

# VCS
GET    /api/v1/projects/{owner}/{project}/diff/{seq}
GET    /api/v1/projects/{owner}/{project}/tree/{ref}
GET    /api/v1/projects/{owner}/{project}/blob/{ref}/{path}
GET    /api/v1/projects/{owner}/{project}/log

# Operator → agent channel
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/messages      body: { "text": "please focus on the auth module" }; API forwards to dispatcher via req.channel.send.*; dispatcher writes last_message to channels.* KV then publishes to channel.inbox.{owner}.{project}.{seq}; returns 202
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/status        returns ChannelStatus (see Channel section)

# Variables (project-scoped)
GET    /api/v1/projects/{owner}/{project}/vars
PUT    /api/v1/projects/{owner}/{project}/vars/{name}
DELETE /api/v1/projects/{owner}/{project}/vars/{name}

# Secrets (names only; values never returned)
GET    /api/v1/projects/{owner}/{project}/secrets
PUT    /api/v1/projects/{owner}/{project}/secrets/{name}
DELETE /api/v1/projects/{owner}/{project}/secrets/{name}

# Knowledge libraries
GET    /api/v1/knowledge/global                                     → list all KOs
GET    /api/v1/knowledge/global/{subject}                           → list all KOs for subject
GET    /api/v1/knowledge/global/{subject}/{predicate}
PUT    /api/v1/knowledge/global/{subject}/{predicate}
DELETE /api/v1/knowledge/global/{subject}/{predicate}
GET    /api/v1/knowledge/{owner}                                    → list all KOs
GET    /api/v1/knowledge/{owner}/{subject}                          → list all KOs for subject
GET    /api/v1/knowledge/{owner}/{subject}/{predicate}
PUT    /api/v1/knowledge/{owner}/{subject}/{predicate}
DELETE /api/v1/knowledge/{owner}/{subject}/{predicate}
GET    /api/v1/projects/{owner}/{project}/knowledge                 → list all KOs
GET    /api/v1/projects/{owner}/{project}/knowledge/{subject}       → list all KOs for subject
GET    /api/v1/projects/{owner}/{project}/knowledge/{subject}/{predicate}
PUT    /api/v1/projects/{owner}/{project}/knowledge/{subject}/{predicate}
DELETE /api/v1/projects/{owner}/{project}/knowledge/{subject}/{predicate}
```

### Event Types

All events are published exclusively by the dispatcher to `job.events.{owner}.{project}.{seq}.{event_type}`. No other service or container publishes to this stream. Every event is a JSON object with at minimum `{ "job_seq": u64, "project": String, "ts": DateTime<Utc> }` plus event-specific fields.

| Event type | Published by | Trigger |
|---|---|---|
| `job-created` | Dispatcher | Dispatcher creates job in KV in response to `req.jobs.create.*` |
| `job-released` | Dispatcher | `POST .../release` accepted; Frozen → Ready or Blocked |
| `job-unblocked` | Dispatcher | Blocked → Ready (last upstream dep reached Done) |
| `job-started` | Dispatcher | Ready → Work; includes `cycle` |
| `job-evaluation-started` | Dispatcher | Work → Evaluation; includes `cycle` |
| `job-rework-started` | Dispatcher | Eval reduce failed; Evaluation → Work with cycle++; includes new `cycle` and collected `eval_context` |
| `job-done` | Dispatcher | Evaluation → Done |
| `job-escalated` | Dispatcher | Work retries exhausted, rebase/merge conflict, or rework limit exceeded; includes `reason` field |
| `job-escalation-resolved` | Dispatcher | Operator completes escalation Human task; includes `action` (`Retry`/`Resolve`/`Revoke`) |
| `job-revoked` | Dispatcher | Frozen \| Blocked \| Ready \| Escalated → Revoked; includes cascaded job seqs if dependents were also revoked |
| `task-created` | Dispatcher | New task written to KV |
| `task-started` | Dispatcher | Task transitioned to Running |
| `task-completed` | Dispatcher | Task reached Done (includes `pass` and `structured` where applicable) |
| `task-failed` | Dispatcher | Task reached Failed |

When `POST .../tasks/{id}/resolve` is called, the API layer forwards the `TaskResolution` payload to the dispatcher via NATS request-reply (`req.tasks.resolve.{owner}.{project}.{job_seq}.{task_id}`). The dispatcher handles the request: validates the `TaskResolution`, writes `TaskResult::Human` to `tasks.*` KV, publishes the `task-completed` event, and drives the next state transition. The dispatcher replies with success or error; the API returns the corresponding HTTP response. The dispatcher is the sole writer of `tasks.*` KV and the sole publisher of task events — the API layer never writes task state directly.

### SSE Event Stream

The API layer subscribes to the `job-events` JetStream stream with a subject filter. The project-wide SSE endpoint filters on `job.events.{owner}.{project}.>`; the per-job endpoint filters on `job.events.{owner}.{project}.{seq}.>`. Each event is forwarded as a proper SSE frame:

```
id: {nats-sequence}\n
data: {"job_seq":42,"project":"acme/api","ts":"...","event_type":"job-started",...}\n
\n
```

The `id` field carries the NATS stream sequence number. Clients that disconnect reconnect using the `Last-Event-ID` header; the API replays from that sequence position in the stream. Content-Type is `text/event-stream`.

### Webhooks

A separate webhook service consumes NATS streams directly and pushes to external endpoints. Not part of the API layer — no coupling between the two.

---

## Identity and Access Control

Principle: deny by default, minimum necessary scope, short-lived credentials. No external auth service for v1 — the entire IAM footprint is a JWT keypair, an SSH CA keypair, and NATS KV.

User and project management (create user, assign roles, create project) is CLI-only via `chuggernaut admin ...` — no HTTP surface for admin ops.

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

pub enum IdentityKind { User, Dispatcher }
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

The dispatcher identity holds a long-lived JWT with `kind: dispatcher`, issued at deploy time and rotated via the deployment's secret mechanism.

### SSH Certificate Authority

One SSH CA keypair is generated at platform init. The private key is mounted into the dispatcher at runtime (k8s Secret in k8s deployments, bind-mounted file in Docker deployments). The public key is available to the SSH server configuration.

**User SSH certs** — the API layer handles `POST /auth/ssh-cert`: the user submits their public key (authenticated via JWT cookie); the API forwards the request to the dispatcher via NATS (`req.ssh.sign-user-cert`); the dispatcher signs with the CA private key and returns a certificate valid for 24 hours with a principal equal to the user's email. Users interact with git via the `chuggernaut` CLI, which handles cert refresh transparently.

**Per-job SSH certs** — issued by the dispatcher at job launch. Signed with the CA private key, validity equals the job timeout, principal is `job-{seq}`. Ref permissions are enforced by principal name in the SSH layer.

The SSH server trusts one CA (`TrustedUserCAKeys ca.pub`). No per-user key registration required.

**SSH ref authorization** enforced at the SSH layer by principal:

| Principal | Push permitted | Pull permitted |
|---|---|---|
| `job-{seq}` | `refs/heads/job/{seq}` only | any |
| `dispatcher` | any protected ref (default branch, tags) | any |
| user email | none | any ref on projects where Viewer+ |

Direct git-over-SSH access for users is read-only in v1. Authenticated users may clone and fetch to inspect working branches; all writes go through the dispatcher. Per-project read authorization is enforced by the SSH layer against the user's `project_roles` claim in their SSH cert extension (encoded at cert issuance time).

```
KV: ssh_keys — not used; cert-based auth only
```

### Permission Rules

| Action | Required |
|---|---|
| Read any project endpoint | Viewer+ on that project |
| Complete / fail a task | Member+ on that project |
| Manage vars, secrets, knowledge | Admin on that project |
| Platform-level config | `platform_admin` |
| Push to default branch | Dispatcher identity only |
| Issue SSH certs | Authenticated user (any role) |

### Per-Job Machine Credentials

At job launch, the dispatcher issues two short-lived credentials scoped to the specific job (duration = job timeout):

**NATS JWT** — subject allow list for work containers:
```
KV read:    jobs.{owner}.{project}.{seq}
KV read:    tasks.{owner}.{project}.{seq}.*
KV read:    knowledge.global.*
KV read:    knowledge.{owner}.*
KV read:    knowledge.{owner}.{project}.*
KV read:    channels.{owner}.{project}.jobs.{seq}
KV write:   channels.{owner}.{project}.jobs.{seq}
Publish:    req.work.submit.{owner}.{project}.{seq}
Sub/Pub:    channel.{inbox|outbox}.{owner}.{project}.{seq}
```

Work containers do **not** publish to `job.events.*`. All events are published exclusively by the dispatcher. The `req.work.submit` subject is how the work agent signals completion with a summary (see `submit_result` tool); the dispatcher validates, writes the task result, and publishes the appropriate events.

**NATS JWT** — subject allow list for eval agent containers (more restricted):
```
KV read:    tasks.{owner}.{project}.{seq}.{task_id}
KV read:    knowledge.global.*
KV read:    knowledge.{owner}.*
KV read:    knowledge.{owner}.{project}.*
KV read:    channels.{owner}.{project}.jobs.{seq}
KV write:   channels.{owner}.{project}.jobs.{seq}
Publish:    req.eval.submit.{owner}.{project}.{seq}.{task_id}
Sub/Pub:    channel.{inbox|outbox}.{owner}.{project}.{seq}
```

Eval agents do **not** write to `tasks.*` KV directly. The `req.eval.submit` subject is how the eval agent signals completion with structured findings; the dispatcher validates, writes `TaskResult::Agent` to `tasks.*` KV, and drives the next transition. The dispatcher is the sole writer of `tasks.*` KV.

The NATS operator signing key is mounted into the dispatcher at runtime (k8s Secret in k8s deployments, bind-mounted file in Docker deployments).

**SSH certificates** — work containers:
```
push: refs/heads/job/{seq}    only their own branch
pull: any
```

**SSH certificates** — eval containers:
```
pull: any    read-only, no push
```

### Vars

Variables are project-scoped plaintext key-value pairs. Stored in NATS KV at `vars.{owner}.{project}.{name}`. The API layer forwards reads and writes via NATS request-reply; no encryption — values are not sensitive. At job launch the dispatcher reads every var declared in the job type's `vars:` list from the `vars.*` bucket and injects them as env vars into both work and eval containers. All declared vars are guaranteed to exist at this point — their presence is validated at release time (see Release Validation).

```rust
pub struct Var {
    pub name: String,
    pub value: String,
}
```

### VarStore Trait

```rust
#[async_trait]
pub trait VarStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> Result<()>;
    async fn get(&self, owner: &str, project: &str, name: &str) -> Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> Result<()>;
    async fn list(&self, owner: &str, project: &str) -> Result<Vec<Var>>;  // names and values
}
```

Unlike secrets, `list` returns both names and values — vars are not sensitive.

### Secrets

Secret values are stored in NATS KV (`secrets.{owner}.{project}.{name}`) encrypted with [age](https://github.com/FiloSottile/age) (X25519).

The platform generates an age keypair at init time:

- **Public key** — available to the API layer; used to encrypt values on write (`PUT /secrets/{name}`)
- **Private key** — mounted into the dispatcher at runtime; never exposed outside it

The dispatcher decrypts values at job launch and injects them as env vars. Containers never see the key or the KV bucket. All declared secrets are guaranteed to exist at this point — their presence is validated at release time (see Release Validation).

### SecretStore Trait

```rust
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> Result<()>;
    async fn get(&self, owner: &str, project: &str, name: &str) -> Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> Result<()>;
    async fn list(&self, owner: &str, project: &str) -> Result<Vec<String>>;  // names only
}
```

`list` returns names only — values are never returned to callers outside the dispatcher. Key rotation requires re-encrypting all values with the new public key — a one-time admin operation exposed as a platform CLI command.

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
| Identity / access | JWT (RS256) + SSH CA + NATS KV | Zitadel, Keycloak, Ory (swap AuthProvider impl) |
| Version control | git CLI + bare repos on disk | — |
| API | axum | — |

Platform init generates: JWT RS256 keypair, SSH CA keypair, age keypair, VAPID keypair. All private keys are mounted into services at runtime via the deployment's secret mechanism (k8s Secrets in k8s deployments, bind-mounted files in Docker deployments) — never stored in NATS KV. The JWT public key is also mounted into the API layer for token verification; all other private keys are dispatcher-only.

All infrastructure is Terraformable via Kubernetes providers.

---

## Schema Registry

The platform does not own a schema registry. The platform's internal type contracts are Rust structs and enums — enforced by the compiler, not at runtime.

A schema registry is available as a platform service for applications built on the platform to use if they choose. It is not a platform primitive.

---

## Security

### Private by Default

All projects deny access unless explicitly granted. Covered by the IAM layer — no additional mechanism needed.

### Container Isolation

Job containers run agent code the platform didn't write. Constraints enforced by the dispatcher at launch:

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
- The age private key is dispatcher-only; containers never see it
- `SecretStore.list` returns names only; `get` is called only by the dispatcher at launch
- Eval prompts should instruct agents not to include secret values in findings or notes — platform cannot enforce this mechanically

### Audit Trail

Three layers, all already in the design:

- **NATS event stream** — every job and task state transition published to `job.events.*`; append-only; the primary audit log for all execution activity
- **Task results** — every human task completion records operator identity, timestamp, and notes; who approved what and when is in the task log
- **Git history** — squash-merges to default branch; commit message references job seq so any commit is traceable to its originating job

Continuous security audit and inter-service mTLS deferred.

### Frontend Security

PWA served from the same axum origin as the API. Authentication via `httpOnly; Secure; SameSite=Strict` cookie containing the JWT — set on `POST /auth/login`, sent automatically with every subsequent request. XSS cannot read the token; CSRF is prevented by `SameSite=Strict`. TLS enforced everywhere via cert-manager + Let's Encrypt on the k8s ingress.

---

## Mobile

PWA — single codebase, installable on mobile home screen, served from the axum API server. Designed mobile-first; works on desktop as the primary operator interface. Frontend framework TBD.

The SSE event stream is the data backbone for the UI — the client connects once per project and receives all state changes in real time. No polling.

**Core screens:**

- **Task inbox** — pending human tasks across all jobs; primary operator interaction surface
- **Graph view** — DAG visualization, job states, live updates via SSE
- **Job detail** — state, task log, agent status/progress, diff for the job branch
- **Escalation flow** — read findings, provide context, complete or fail the task

Push notifications via Web Push API for task inbox alerts. VAPID keypair generated at platform init. The public key is stored in NATS KV (`platform.vapid.public`) for distribution to clients; the private key is mounted into the API layer at runtime.

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
