# Chuggernaut v2 — Platform Specification

---

## Part 1: Data Model

### 1.1 Job

The fundamental primitive. A **job** is a node in a DAG stored in NATS KV. One Rust struct is the canonical record; the dispatcher is its sole writer.

```rust
pub struct Job {
    pub id: u64,                           // sequential per project; maintained via counter in NATS KV
    pub project: String,                   // "{owner}/{repo}" slug
    pub r#type: String,                    // job type name; references .chug/jobs/{type}.yaml at base_ref
    pub title: String,                     // ticket-style instance identity: what this run is for (may be empty). The type carries the *how*; title/description carry the *what*.
    pub description: String,               // the ticket body; injected into work and eval prompts as the §4.3 job brief
    pub cover_html: Option<String>,        // optional rich cover page for the operator UI: PRESENTATIONAL ONLY — never injected into any agent prompt (the §4.3 job brief consumes only title/description), so it can carry an exciting formatted page without polluting what agents read. Accepted on create and the Draft PATCH, size-capped at ~256 KiB (over → 422). Rendered above the description in a sandboxed iframe (no scripts, no forms) with an injected `default-src 'none'` CSP that also blocks external fetches (external `<img>`, CSS `url()`/`@import`); authors ship self-contained styling. Stored verbatim — containment is at this shared render choke point, not an ingest stripper. None → no cover; defaults None on old records
    pub deps: Vec<u64>,                    // upstream job ids this job depends on (ordering edges: upstreams must be Done first; their work is in this job's base, their structured results available to it). Plain ids, no named roles — picked at creation, validated at release.
    pub members: Vec<u64>,                 // member job ids absorbed into this batch (§2.1 batches). Empty for an ordinary job; non-empty marks a batch — one branch implementing all members, evaluated under the union of their criteria, whose single merge completes every member. Defaults empty on old records
    pub batch_id: Option<u64>,             // set on a member absorbed into a batch: the batch job's id. Some implies the member is (or was) Batched under that batch; cleared when the batch is revoked/fails and the member returns to Frozen. None for ordinary jobs and batches themselves. Defaults None on old records
    pub state: JobState,
    pub branch: String,                    // "job/{id}"; set at creation; actual git branch created when job enters Work
    pub base_ref: Option<String>,          // exact HEAD of default branch; set/updated at every Ready-transition (Frozen→Ready and Blocked→Ready), at Work→Evaluation entry when the branch is rebased onto a moved HEAD (§3.2), and on squash-merge conflict; None until job first enters Ready
    pub knowledge_tags: Vec<String>,       // union of job type defaults and operator-supplied tags at creation
    pub eval: Vec<Evaluator>,              // additive per-job evaluators, layered on top of the type's eval list at execution; the type's evaluators are a floor — creation can add criteria, never remove or override them; name collisions are a release-time error (see design-lifecycle.md)
    pub require_approval: bool,            // the job cannot pass evaluation without an explicit operator sign-off (§3.3 approval gate). ADDITIVE, exactly like `eval`: criteria resolution synthesizes ONE required Human evaluator named `approval` on top of whatever the type declares, never in place of it. Its stage is computed at resolution time as max(stage of every other resolved evaluator) + 1, so it always runs LAST — the operator is never asked to sign off on work a later stage is about to reject. `approval` is a RESERVED evaluator name: a job type or a per-job `eval` entry using it is a release-time 422, whatever this flag says. Settable at creation (POST .../jobs) and editable while the job is pre-Work — Draft, Frozen, Blocked, Ready, Stalled (PUT .../jobs/{seq}/approval, §6.2); past Work entry the criteria are already resolved, so the edit is a 422 naming the state rather than a silent no-op. A batch inherits it from ANY member (§2.1 batches): one merge completes every member, so the strictest member's gate governs the whole batch. Defaults false on old records
    pub claim_next: bool,                  // a human has claimed the job's NEXT work attempt (§1.2 claims): instead of launching, the dispatcher parks that attempt as a Pending task with the declared kind and performed_by: human, then clears the flag — a claim covers exactly one attempt. Defaults false on old records
    pub timeout: Option<String>,           // optional per-job work-task timeout override (duration string, e.g. "45m"), layering over the type's resources.task_timeout exactly like `eval` layers over the type's evaluators — but Work-phase tasks only; evaluators keep the type default. Any valid duration. Parseability validated at release. None → the type default applies
    pub model: Option<String>,             // optional per-job model override for the Work agent (§12.4). The most specific choice, so it wins over every other layer: the job type's work.model, the project default (.chug/jobs/_defaults.yaml), and the platform default. Work-phase agent tasks only — evaluators keep the type/project/platform resolution, exactly as `timeout` scopes to Work. None → the resolution chain applies. Defaults None on old records
    pub inputs: BTreeMap<String, String>,  // the job's EFFECTIVE input values for the type's declared `inputs:` (§1.1 job type). Written by exactly two paths, both single-writer: creation (and the Draft PATCH, the same act repeated) supplies values, and the Ready-transition that FIRST records base_ref fills in a declared `default` for every input the creator did not supply — add-only, never overwriting a supplied value. From that moment it is the complete effective set, and IMMUTABLE: not on rework, not on a work retry, not on a claim, not across a later base_ref update (§3.2 rebase, conflict re-base, pre-work Retry), because a target that changed mid-flight would make the record a lie about at least one cycle. Getting a different target is getting a different job. Delivered as `CHUG_INPUT_*` to every container (§4.1), shown to agents in the §4.3 job brief's `### Inputs` subsection, and recorded in the squash body's `Inputs:` line (§3.2) and the §10.3 event stream. Empty for every type that declares no inputs; defaults empty on old records and is omitted from the wire when empty
    pub groups: Vec<String>,               // what this job is PART OF (design #321): operator-set labels — "design/311-job-inputs", "beacon-import" — so a set of jobs can be enumerated and rolled up. Many per job; the convention (not a platform rule) is a namespace prefix, where design/{stem} names docs/design/{stem}.md. INERT TO EXECUTION: no container env, no agent prompt, no job-type resolution and no state transition reads it — which is why, alone among the fields here, it is MUTABLE IN EVERY STATE including Done and Revoked (PUT .../jobs/{seq}/groups, §6.2). Written by three paths, all single-writer: creation, the Draft PATCH (full-field replace), and that endpoint (add/remove, so two operators grouping one job both succeed). Shape-checked only — at most 8 groups per job, each matching ^[a-z0-9][a-z0-9._/-]*$ and at most 128 characters, unique within the job; all hard errors (422), never truncation. No repo validation, no referential integrity, no cascade on revoke: the registry is advisory, exactly as §4.4 treats a knowledge tag with no file. No aggregate is ever stored — every count and enumeration is derived from the job records at read time. Empty for every ungrouped job; defaults empty on old records and is omitted from the wire when empty
    pub escalation: Option<Escalation>,    // structured record of the job's most recent escalation/stall: reason code, human-readable detail, failing task (when one exists), and timestamp — written at every escalate/stall site so operators diagnose from the record the API serves, not from dispatcher logs. Advisory; no transition consults it. None until the job first escalates. Defaults None on old records
    pub factory: Option<String>,           // factory name when created by a factory triage agent (see §13); None for operator-created jobs
    pub schedule: Option<String>,          // schedule name when an occurrence of .chug/schedules/{name}.yaml created this job (§1.1 schedules); None for every other origin. Written only by the dispatcher's origination path and immutable after creation — an operator's POST jobs never carries it. It is also the key the at-most-one-in-flight rule reads: the most recent job carrying a schedule's name IS that schedule's anchor, so no last-fired state is stored anywhere. Defaults None on old records and is omitted from the wire when absent
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,   // set once (immutably) when job first enters Ready; anchor for job_deadline; None until then
    pub completed_at: Option<DateTime<Utc>>,  // when the job reached a terminal state (Done or Revoked); stamped once by the dispatcher's single state-write path at the terminal transition and never cleared, so the jobs list shows completion without opening the job. None while live. Defaults None on old records
    pub task_time_ms: Option<u64>,         // how long the job spent WORKING: the sum of completed_at - started_at over every task of the job, across cycles and rework attempts. Tasks that never started (parked, queued, cancelled) and the gaps between tasks contribute nothing — completed_at - created_at is mostly the waiting a job does while Frozen and Blocked, which is why that is not the number. Recomputed from the job's own tasks (never a project-wide scan) at the single point a task record is written back, so it is idempotent and self-healing rather than accumulated. None when no task carries a usable span, so a consumer can tell "nothing to show" from a genuine 0; defaults None on old records, which an operator backfills with `chuggernaut admin backfill-task-time`
}

pub enum JobState { Draft, Frozen, Batched, Blocked, Ready, Work, Evaluation, WrapUp, Escalated, Stalled, Done, Revoked }

pub struct Escalation {
    pub reason: String,             // machine reason code, matching the job-escalated / job-stalled event reason (e.g. "launch_validation_failed", "work_retries_exhausted", "eval_abort", "job_deadline_exceeded")
    pub detail: String,             // human-readable explanation — the same text shown in the operator's intervention task prompt
    pub failing_task: Option<u64>,  // the task whose failure triggered the escalation, when one exists; None for pre-work escalations (launch validation, a deadline elapsed while Ready) and evaluation-phase escalations with no single culprit
    pub at: DateTime<Utc>,          // when the escalation was recorded
}
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
  "claim_next": false,
  "factory": null,
  "created_at": "2026-04-05T10:00:00Z",
  "ready_at": null
}
```

**Naming conventions:** `project` in JSON records is always `"{owner}/{repo}"` (e.g. `"acme/api"`). In NATS KV keys and subjects it is split into `{owner}` and `{project}` components (e.g. `jobs.acme.api.42`), where `{project}` is the bare repo name. HTTP routes follow the same split (`/api/v1/projects/{owner}/{project}/...`). These are the same project — three representations of one identifier.

IDs are sequential integers scoped per project, maintained via a counter at `counters.{owner}.{project}`.

**Branch lifecycle:** each job works on a dedicated branch `job/{seq}`. The branch name is deterministic and stored at creation; the actual git branch is created from the default branch when the job first enters Work. Upstream dependencies are guaranteed Done (and their branches squash-merged) before a dependent job starts. Data flows between jobs implicitly through VCS — no separate artifact routing.

#### The config root

Everything the platform reads out of a project repo — job types, prompts, reusable tasks, knowledge tags, schedules — lives under one directory, **`.chug/`**, the way `.github/` holds a repo's GitHub config. A project's chuggernaut config is therefore one tree to find, review, and copy between projects, not five directories scattered through the repo root.

Every config read resolves two candidate paths in order: `.chug/{path}` first, then the bare repo-root `{path}`. The second is the layout that predates the config root; it stays readable because a job pins its `base_ref` at launch, so a job released before a project migrated must still load the config that ref carries. Writes (the §12.2 starter template) only ever use `.chug/`. A file present at both locations resolves to the `.chug/` copy — never a merge of the two.

**Migrating a project** moves the directories and rewrites the paths its job types name (`prompt:`, `run:`), which are ordinary repo paths and follow the files. Because the *deployed* dispatcher is what reads them, a project whose platform predates the config root must deploy a dispatcher that understands `.chug/` **before** the move lands; otherwise its job types become unfindable and no job can be created until the deploy catches up (§14, `docs/runbooks/adhoc-deploy.md`).

#### Job Type

Declarative YAML, one file per job type, lives under `.chug/jobs/` in the repo and is version-controlled. Declares only the contract — image, resources, eval criteria, retry limits, secrets, vars. Not an instance; no imperative logic. (Dependencies are per-instance, chosen at job creation — the type does not declare them.)

**Canonical schema:**

```yaml
# ── Top-level ────────────────────────────────────────────────────────────────
name: string                   # required; unique within the repo (the file stem — the wire identifier)
display_name: string           # optional; human-facing name for the library and the create-form type picker; falls back to name
description: string            # optional; one-line summary shown alongside the display name in the type picker
image: string                  # required for agent/command work; disallowed at top level for human work (container evaluators must declare their own image; see eval.image)

runtime:                       # optional; where this type's tasks run and against which toolchain. Absent = mode: container with no declared environment, which is every job type that predates the block
  mode: container | host       # optional; default container. `host` PARSES AND IS REFUSED by validate() — the mode is designed but unbuilt (design #309 P0/P1), so the refusal is a "not supported by this dispatcher" field-rule error, never a launch that queues for a node that cannot exist
  env: string                  # optional in container mode, where it layers a project-supplied toolchain over the image's userland (design #373); an opaque environment reference the node resolves — `nix:<flake-ref>#<attr>` in either mode, `xcode:<version>` in host mode only (Xcode cannot be containerized, §322 design). A declared env requires min_dispatcher >= the runtime epoch (§14.2)

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

placement:                     # optional; pins every container this job type launches onto a named fleet node (§3.1)
  node: string                 # fleet node name ([A-Za-z0-9_-]+). Shape-checked at parse; that the node is configured is checked at launch (full/unknown → launch error, no spillover)

wrap_up:                       # optional; the job's third step (work → evaluation → wrap-up); see design-lifecycle.md
  type: merge | none           # default merge: squash-merge the job branch through the merge queue/gate. none: eval-pass goes straight to Done — for jobs whose effect is external (deploys, reports); the job branch is scratch and is deleted unmerged
  run: string                  # optional; post-merge command (§3.2). Runs in the WrapUp phase AFTER the squash lands on the default branch, against the merged main content (the container clones the default branch). Ships the merged result (e.g. a web job publishing its built UI); never runs if the job is revoked/escalated before landing. Requires type: merge. A non-zero exit escalates the job — the merge is NOT undone. Must be idempotent (a restart may re-launch it, §3.6)
  name: string                 # optional; human-facing label for the wrap-up task, validated like an evaluator name ([A-Za-z0-9._-]+). Unset → derived from the mode: a command wrap-up takes its script's basename (.chug/tasks/web-publish.sh → web-publish). Stamped onto the task record's `label` so the UI reads `Command · publish`, not a bare `Command`
  image: string                # optional; image for the run container; falls back to top-level image (required when run is set and the job type has no top-level image). Disallowed without run
  secrets: [string]            # optional; secrets injected into the run container; not inherited from work.secrets. Disallowed without run

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

inputs:                        # optional; the values a job of this type accepts. A non-empty list requires min_dispatcher >= the inputs epoch (§14.2). An input is a value handed to the running job — never substituted into this file, so nothing here can be chosen by one
  - name: string               # required; [a-z][a-z0-9_]*, unique within the type — lowercase so the mapping onto one reserved env name is injective
    type: string | enum        # required
    required: bool             # optional; default false. An optional input with no supplied value and no default is absent, never an empty string
    default: string            # optional; disallowed with required: true. Materialized onto the job record, so it must itself satisfy the charset and this declaration's pattern/values
    values: [string]           # required for type: enum; disallowed for type: string. Each value must satisfy the charset
    pattern: string            # optional, type: string only; a regex the WHOLE value must match. May only narrow the default charset, never widen it
    description: string        # optional; shown in the create form and the agent's job brief
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

**Field rules for `runtime`** (design #309 §3, #373 Decision 2 — the whole table
is declared, only the container row is implemented):

| Rule | Detail |
|---|---|
| `mode: container` (and an absent `runtime`) | `image` required for agent/command work exactly as before — a container always needs a root filesystem, so `container + env + no image` is not coherent. `env` optional |
| `mode: host` | **Refused** by `validate()` as not supported by this dispatcher: the mode is designed (top-level `image` disallowed, `env` required) and unbuilt, and the refusal stands in for design #309 P0 until it lands. The host row's own field rules — and the narrowing of the evaluator-image requirement they need — arrive with it |
| Scheme | `nix:<flake-ref>#<attr>` is legal in either mode. `xcode:<version>` requires `mode: host`, so it is a field rule rather than a launch failure — Xcode cannot be containerized. A declared `env` must be non-empty |
| Skew | Any `runtime:` beyond a bare `mode: container` — a declared `env`, or any non-container `mode` — requires `min_dispatcher >=` the epoch the block landed in (§14.2). The gate is structural, not left to authorship: an N−1 dispatcher tolerates the whole unknown `runtime:` field, keeps the still-present `image`, and would run the job containerized against the image's toolchain rather than as declared — a silently dropped constraint. The declaration is the only signal that crosses the skew boundary, because an N−1 dispatcher never runs the new field rules at all. A container-mode `runtime:` with no `env` is ungated: it drops nothing |

**Field rules for `inputs`** (all enforced at parse, so release validation and
`chuggernaut validate` reject them offline):

| Rule | Detail |
|---|---|
| Value charset | Every value matches `^[A-Za-z0-9._:/@+-]{1,256}$` — alphanumerics plus seven punctuation characters. Whitespace, quotes, backticks, backslash and every shell metacharacter are excluded: an input value is an *identifier, not prose*, because it can reach a `run:` script that itself crosses further shells. Free text is what `title`/`description` are for |
| `pattern` narrows only | The effective check is `charset AND pattern` — a declared pattern can never widen the charset. It must be a usable regex, and it matches the **whole** value. An input whose value reaches an argv position wants one: the charset stops metacharacter injection, but not a value beginning with `-` or `/` |
| Bounds | At most 16 declared inputs per type; at most 256 characters per value. Both hard errors, never truncation |
| Declaration | `name` matches `[a-z][a-z0-9_]*` and is unique within the type; `values` is required for `type: enum` and disallowed for `type: string`; `pattern` is `type: string` only; `default` is disallowed with `required: true` and must itself satisfy the charset and the declaration's own `pattern`/`values` |
| Skew | A non-empty `inputs:` requires `min_dispatcher >=` the epoch at which inputs landed. An older dispatcher *tolerates* the unknown `inputs:` field (§14.2) and would run the job with no value at all, so the gate is structural rather than left to authorship |

An `Input` block keeps `deny_unknown_fields` like every other nested block: an
ignored key there could silently drop a `pattern`, which is a validation
control. The kind set is deliberately two — `bool` is an `enum` over `["true",
"false"]` and `int` is a `string` with `pattern: '^[0-9]+$'`, so a third kind
would buy nothing but a second config language.

**Supplying values.** A job supplies inputs at creation (`inputs` on
`POST .../jobs`) and, while Draft, on the full-field `PATCH .../jobs/{seq}`;
`Job.inputs` (§1.1) is the record. A **claim** and a **rework** cycle supply
none — neither redefines the job — and a **batch** whose members carry inputs is
rejected with a `members` field error, because a batch collapses N members into
one run and values do not union the way `deps` and `eval` do.

**Where a bad input fails** (§2.2's three passes, each deciding only what it can
know):

| Failure | Pass | Result |
|---|---|---|
| Malformed name, value outside the charset, value over the length cap, more than 16 entries | Creation | 422 on the create/PATCH — needs no job type file, so the operator gets it back on the form |
| Undeclared input name; missing `required` input; `enum` value not in `values`; `string` value not matching `pattern` | Release-time (pass 1) | `ValidationError { field: "inputs.{name}" }`; the release is rejected and the job stays Frozen |
| The type's `inputs:` declaration changed between release and Blocked→Ready | Ready-transition (pass 2) | Re-checked at `base_ref` with the rest of static config; a failure parks the job Stalled |

**When a default becomes a value.** A declared `default` is *materialized*, not
consulted at launch: the same single-writer write that **first** records
`base_ref` fills every declared input the creator did not supply, so
`Job.inputs` is the effective set every audit surface reads. That moment is the
Ready-transition, not release-time — pass 1 checks against current HEAD and is
explicitly not an execution guarantee, while `base_ref` is the ref the run uses.
For a job released straight to Ready the two coincide; for one released into
Blocked they do not, and resolving against anything else would let a job execute
a script from `base_ref` with a default read from a different tree. A declared
`default` satisfies pass 1's presence check, so a missing *optional* input is
never a release error. Later `base_ref` movements do not re-resolve — defaults
resolve exactly once. An optional input with neither a supplied value nor a
`default` stays **absent**, never an empty string.

How a value reaches a container is specified with the container environment: one
`CHUG_INPUT_{NAME}` key per resolved value, delivered to work, wrap-up and eval
containers alike (§4.1).

**`work.type: human`** — no container is launched. The dispatcher creates a `Human` task in `Pending` state in the Work phase; it surfaces in the operator task inbox. The operator performs the work manually, then resolves via `POST .../tasks/{task_id}/resolve`:
- `TaskResolution::Pass` — work complete; proceeds to Evaluation (or Done if no evaluators). A `summary` on the Pass is both used as the squash-merge commit body *and* persisted on the stored `TaskResult::Human { summary }`, so the operator's report renders in the Reports thread like an agent's closing summary.
- `TaskResolution::Fail` — operator cannot/will not complete; job → Escalated with a Human escalation task.

`work_retries` is disallowed for human work — there is no container to retry. Human work tasks are excluded from the timeout scan; use `job_deadline` to bound wall-clock time. If eval fails and rework budget remains, a new Human task is created for the next cycle with all eval findings injected. Command/agent evaluators on human-work jobs run in their per-evaluator `image` (required, since there is no top-level image to fall back to).

**Command work jobs must be idempotent.** `work_retries` will re-run the command on failure; the command is responsible for safe handling of any partial side effects. Set `work_retries: 0` if the operation cannot be made safe to retry.

**Example job type files:**

```yaml
# .chug/jobs/implement-endpoint.yaml
name: implement-endpoint
image: registry.acme.com/agents/impl:latest
work:
  type: agent
  prompt: .chug/prompts/work/implement-endpoint.md
  provider: claude
  model: claude-sonnet-4-6
  review:
    prompt: .chug/prompts/review/implement-endpoint.md
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
    prompt: .chug/prompts/eval/security-review.md
    provider: claude
    model: claude-opus-4-6
    secrets: [GITHUB_TOKEN]
  - name: architecture-review
    type: agent
    prompt: .chug/prompts/eval/architecture-review.md
    required: false
  - name: human-approval
    type: human
    prompt: .chug/prompts/eval/human-approval.md
knowledge:
  - rust
  - rest-api
secrets: [GITHUB_TOKEN]
vars: [RUST_EDITION]
```

```yaml
# .chug/jobs/deploy-staging.yaml
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
  (draft 2020-12). Canonical copies live in `.chug/schemas/` (platform repo,
  regenerated under test). In a project repo, commit it next to the files it
  describes and add a yaml-language-server modeline for in-editor validation,
  autocomplete, and hover docs:

  ```sh
  chuggernaut schema job-type > .chug/jobs/.job-type.schema.json
  ```
  ```yaml
  # yaml-language-server: $schema=.job-type.schema.json
  name: implement-endpoint
  ...
  ```

- `chuggernaut validate .chug/jobs/*.yaml .chug/schedules/*.yaml` runs the static
  slice offline (parse + the field-rules matrices below, with a sibling
  `_defaults.yaml` merged) — for contributors and CI. The file kind follows the
  path: a file under `schedules/` is a schedule. Repo-dependent checks (prompt
  files exist, secrets/vars set) still run at release (§2.2).

#### Project Defaults

An optional file `.chug/jobs/_defaults.yaml` declares project-wide defaults applied to **every** job type. It carries two things: `eval` (evaluators appended to every type's `eval` list — how a project gates all changes on an evergreen test suite without each job type author remembering to declare it) and `model` (a project-level default agent model, §12.4):

```yaml
# .chug/jobs/_defaults.yaml
model: claude-opus-4-8            # optional; project-level default agent model (§12.4)
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
- `model` is folded in as a fallback for every agent that does not declare its own `model` — the work agent and each agent evaluator (the same reach as the platform default, §12.4). It sits **below** a job type's own `model` and **above** the platform default. Command/human work and evaluators take no model and are untouched.

#### Schedules

A **schedule** is time-triggered job creation: one file per schedule under `.chug/schedules/`, resolved from default-branch HEAD like every other config directory (design [#310](docs/design/310-scheduled-jobs.md)). It is repo-versioned so a schedule change ships in the same commit as the job type it fires and clears the same gates.

```yaml
# .chug/schedules/nightly-integration.yaml
name: nightly-integration     # required; unique within the repo, and equal to the file stem
job_type: code                # required; the .chug/jobs/{job_type}.yaml this schedule creates a job of
cron: "0 2 * * *"             # required; five-field UTC cron (below)
enabled: true                 # optional; default true — a disabled schedule loads and validates but never fires
title: Nightly integration    # optional; the created job's title; defaults to `name`
description: |                # required when the target declares `work.type: agent` (the §4.3 job brief); optional otherwise
  Run the nightly integration suite.
min_dispatcher: 5             # optional; §14.2 skew gate, same meaning as on a job type
inputs:                       # optional; the values every occurrence supplies to the job it creates
  image_tag: ghcr.io/acme/api:4f9c1ab
```

**`inputs:`** is a flat `name: value` map, judged by the same rules an operator's `POST jobs` map is (§1.1 `inputs:`): the charset, the 256-character value bound, the 16-entry count bound, and the lowercase name form. It is the created job's **supplied** set — the schedule passes values through, it does not resolve them, so a declared `default` the schedule omits materializes once at the Ready transition exactly as it does for an API-created job. Because a schedule names its `job_type`, the values are also judged against that type's declaration wherever both files are readable: an input the type does not declare, a value failing its `pattern`/`values`, and a `required` input the schedule never supplies are all errors at `chuggernaut validate` and at reload, not at 3am. A non-empty `inputs:` **requires `min_dispatcher >=` the schedule-inputs epoch** (§14.2) — the field is invisible to a dispatcher that predates it, and a dropped value fires a job with a different meaning rather than a failed one.

**Cron is a deliberate subset**, evaluated in **UTC**: five whitespace-separated fields (`minute hour day-of-month month day-of-week`), each `*`, `N`, `N-M`, `*/S`, or a comma-list of the last three. Day-of-week is `0`–`6`, Sunday first. No `@daily` aliases, no `L`/`W`/`#`, no month or weekday names, no seconds and no year field — an expression is a copy of a GitHub Actions `schedule:` string, so anything the two sides would read differently is rejected. **When day-of-month and day-of-week are *both* restricted (neither is `*`), an occurrence matches if *either* matches** — the POSIX OR rule; when one is `*`, both must match. "Restricted" means anything other than a bare `*`, so a stepped day field such as `*/3` is restricted and participates in the OR rule — Vixie cron instead exempts any field *beginning* with `*` and would AND the same expression, which is the one case where the two readings of a copied string diverge. There is no `timezone:` field: a schedule's meaning must not depend on where the dispatcher runs, and UTC is also why DST never arises.

**Schema tolerance follows §14** exactly as a job type's does — a file read live from HEAD can merge ahead of the binary that parses it, so unknown *top-level* fields are tolerated with a warning and `min_dispatcher` gates a file that genuinely needs a newer dispatcher.

**Validation** runs at two layers. At **merge time**, `chuggernaut validate .chug/schedules/*.yaml` applies the field rules offline and `.chug/tasks/ci.sh` gates every changed schedule file; the `description` and `inputs:` rules are checked against the target job type when its file sits in the same config root, and its existence is otherwise a release-time check like a prompt file's. At **reload time** an invalid file is **skipped** and the project's remaining schedules load normally — an invalid trigger file never blocks dispatch. A project loads at most 64 schedule files; entries beyond the cap are refused and reported, never silently truncated.

**Origination is dispatcher-side and rides the §3.5 scan tick**, so it inherits the single writer and initiates nothing while draining (§3.6). Everything the tick decides reduces to one value per schedule — the **anchor**, the instant an occurrence must be strictly *after* in order to fire:

| The schedule's most recent job (by `created_at`, found via `Job.schedule`) | Anchor | Behavior |
| --- | --- | --- |
| none — never fired | when this dispatcher first loaded the file (in-memory) | no backfill |
| non-terminal (anything but Done/Revoked, **including** Frozen, Stalled and Escalated) | n/a — no fire is possible | **skip**: at most one job in flight per schedule |
| terminal | its `completed_at`, falling back to `created_at` | catch-up across restarts; skipped occurrences consumed |

The tick fires **exactly one** job if any occurrence falls in `(anchor, now]`, and none otherwise — so a dispatcher down across six occurrences fires once on recovery, not six times and not zero. A skipped occurrence is **consumed, not deferred**: it never runs later, because the anchor of the job that blocked it is that job's *completion*. Nothing about last-fired is stored — it is derived from the job records, exactly as `retry_count` is derived from the task log. The search back from `now` is bounded at 366 days; a dispatcher down longer than that arms for the next occurrence instead of catching up.

The job an occurrence creates carries `schedule` provenance on its record and in its `job-created` event, takes its `title` and `description` from the schedule file, and is **released immediately** (§2.2 validation runs at once; a validation failure leaves it Frozen, which — being non-terminal — stops the schedule rather than repeating nightly). Two events report the tick, both job-scoped like every other event (§6.3): `schedule-fired` on the created job, and `schedule-skipped` on the **blocking** job, at most once per occurrence rather than once per 30-second tick. An invalid schedule file has no job to attach an event to, so it is logged only.

The schedule table is held in memory and reloaded at startup, after every squash-merge to the default branch, and on a periodic backstop (every 20th scan tick), so the 30-second tick does no git I/O; a schedule *change* therefore takes effect within one refresh, and a schedule never misfires because of a stale table.

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
    pub label: Option<String>,            // human-facing task label from job-type config (§1.1): a wrap-up task's wrap_up.name (or a derived default), and an evaluator's name mirrored here so the UI reads one label field for every task kind. None for work/escalation/triage tasks and old records (which fall back to `evaluator`)
    pub performed_by: Option<Performer>,  // who actually performed a claimed attempt (claims, below): the declared kind stays immutable; a claim records the human performer here. None for every normally-executed attempt and for old records
    pub container_id: Option<String>,     // backend-assigned container ID (Docker or k8s); None for Human tasks. Persisted the instant the container launches — while the task is still Running — and kept after exit, so a live container (not just a finished one) is nameable for logs and artifact tooling
    pub rework_reason: Option<ReworkReason>,  // why a rework cycle created this Work task (§3.3): set at rework re-entry so a Work task appearing after passed evaluations self-explains. None for cycle-1 work, evaluation/gate/wrap-up tasks, and every non-Work task. Defaults None on old records
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub enum TaskPhase { Work, Evaluation, MergeGate, WrapUp, Triage, Escalation }  // Escalation: the operator-facing Human decision task; stamped distinctly so a resolution never reads as a Work·pass row (see Escalation task phase, below)

pub enum TaskKind {
    Command { run: String },
    Agent   { provider: String, model: Option<String>, prompt: String },
    Human   { prompt: String },
}

pub enum Performer { Human }  // absence of performed_by = executed per the declared kind

pub enum ReworkReason { EvalFailure, MergeConflict, GateCiFailure, GateCompileFix }  // cause of a rework-created Work task (§3.3); mirrors the job-rework-started event reason, persisted on the task so the tasks list self-explains. GateCompileFix = the gate-fix fast path (job #154): a compile-only gate failure repaired by a scoped fix task that returns straight to the gate, no re-review

pub enum TaskState { Pending, Running, Done, Failed }

pub enum TaskResult {
    Work    { summary: Option<String>, structured: Option<serde_json::Value>, token_usage: Option<TokenUsage>, cover_html: Option<String> },  // cover_html: optional agent-authored presentational cover page (§4.2), like Job::cover_html — size-capped at ~64 KiB and rejected (not truncated) at ingest, stored and served with the task, never entering the squash body or any prompt; None → no cover, defaults None on old records
    Command { pass: bool, exit_code: i32, output: String, structured: Option<serde_json::Value> },
    Agent   { pass: bool, abort: bool, structured: Option<serde_json::Value>, token_usage: Option<TokenUsage>, cover_html: Option<String> },  // cover_html: as on Work, for an evaluator's verdict summary
    Human   { pass: bool, abort: bool, structured: Option<serde_json::Value>, action: Option<EscalationAction>, operator: String, resolved_at: DateTime<Utc>, summary: Option<String> },  // summary: the operator's completion summary on a work-task Pass, carried from TaskResolution::Pass::summary (defaults None; omitted from the wire when absent — pre-summary records still deserialize)
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
    Pass       { structured: Option<serde_json::Value>, summary: Option<String> },  // summary: work-task Pass only — the human's completion summary, flowing into the squash-merge commit body like an agent's submit_result; ignored elsewhere
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
| Claimed work attempt (Work phase, any declared kind, `performed_by: human`) | `Pass`, `Fail` | 400 if `Escalation` submitted |
| Human evaluator task (Evaluation phase) | `Pass`, `Fail` | 400 if `Escalation` submitted |
| Post-work escalation task (Escalated state) | `Escalation` (`Retry`/`Resolve`/`Revoke`) | 400 if `Pass` or `Fail` submitted |
| Pre-work escalation task (Stalled state) | `Escalation` (`Retry`/`Revoke`) | 400 if `Pass`/`Fail` submitted, or `Escalation` with `action: Resolve` |

`action` is only meaningful on escalation tasks. `structured` is required (non-null) on `Fail`; optional on all others.

**Escalation task phase.** A Human escalation task is stamped with its own `Escalation` phase — not the phase of the step that failed. This keeps the operator's resolution from rendering as a spurious `Work · pass` row and lets the UI label it distinctly. The failed phase is not lost: it is recorded on the job's `Escalation` record (`failing_task`, `reason`) and drives the resume-at-failed-phase Retry below. Records written before this stamped escalations under `Work`; the UI treats any Human-kind result carrying an `action` as an escalation regardless, so old records still read correctly.

**Escalation `Retry` — resume at the failed phase.** `Retry` on a post-work escalation re-runs *only the phase that exhausted*, never redoing finished work. The phase is determined from the `Escalation` record — the `failing_task`'s phase when one was recorded (work/wrap-up failures and launch-queue timeouts name a culprit), else the machine `reason` (an eval-reduce escalation names no single culprit). Semantics per phase:

- **Work** exhausted (`work_retries_exhausted`) → a new work task in the same cycle, `attempt++`, branch used AS-IS; the `work_retries` budget is *not* reset (an operator who wants more attempts resolves again).
- **Evaluation** exhausted (`eval_infra_failure`, `eval_abort`, `rework_budget_exhausted`) → re-enter Evaluation against the intact branch and `base_ref` with a *fresh* eval fan-out (a new grant of `eval_retries`). No work task is created and the work `attempt` counter is untouched — the work already succeeded.
- **Wrap-up** failure (`wrap_up_failed`) → re-run only the `wrap_up.run` publish command at a fresh attempt; the squash has already landed, so the merge is never redone.

In every case the **cycle number is untouched** — cycles bump only on a real eval FAIL → rework, not on an escalation retry. An unknown/legacy `reason` resumes at Work, the historical default.

**Revoke closes tasks.** Revoking a job (§2.1 Revoked transition) force-closes every Pending human/escalation task it owns, the same way it kills the job's Running containers — a terminal job asks nothing of a human, so no zombie should survive in the operator inbox. Each such task is marked `Done` with a synthetic `TaskResult::Human` carrying `pass: false`, `action: Revoke`, and `operator: "system"`, recording that the revoke — not an operator resolution — retired it. As a second line of defense, the operator inbox (`req.tasks.list.pending`, §6.1) and the derived `awaiting_human` job field both filter out any Pending task whose owning job is already terminal, so records predating this rule disappear from the inbox without a migration.

**Token usage:** agent work tasks (`TaskResult::Work`) and agent eval tasks (`TaskResult::Agent`) carry optional `token_usage` populated at submit time. `token_usage` is `None` when a container crashes before submitting — this is expected and does not affect retry logic. Command and human tasks never carry token usage.

**NATS KV key:** `tasks.{owner}.{project}.{job_seq}.{task_id}`

**Task creation rules:**

| Trigger | Tasks created |
|---|---|
| Job enters Work (new cycle), `work.type: agent\|command` | One work task (attempt=1) |
| Job enters Work (new cycle), `work.type: human` | One `Human` work task (attempt=1); no container launched |
| Work task fails, `work_retries` available | New task record (same cycle, attempt++); `job/{seq}` **recovered or reset** before re-launch (see §3.2 crash recovery): if it carries commits beyond `base_ref` (a prior attempt pushed before crashing) it is kept as-is and the retry prompt notes the resume; otherwise it is hard-reset to `base_ref` |
| Work retries exhausted | Human escalation task → job → Escalated |
| Human-performed work attempt (declared human, or claimed): operator resolves `Fail` | Normal work-attempt failure: task marked Failed; `work_retries` available → new task record (same cycle, attempt++, launched per the DECLARED kind). Unlike a container crash, this is a deliberate handoff at a clean commit boundary: `job/{seq}` is **preserved as-is** (any commits the operator pushed survive, like an eval-failure rework — §3.2 crash recovery), and the `Fail` `structured` notes are injected into the next attempt's context like eval findings. Else (no retries) → Human escalation task → job → Escalated |
| Job enters Work (any cycle/attempt) with `claim_next` set | The attempt parks as a Pending task with the declared kind and `performed_by: human`; no container; claim consumed (`claim_next` cleared) |
| Operator grants `Retry` on escalation | Resumes at the phase that failed (never re-runs finished work). Work exhaustion (`work_retries`) → new work task (same cycle, attempt++, branch used AS-IS, budget NOT reset). Evaluation exhaustion (`eval_retries` / abort / rework budget) → re-enter Evaluation against the intact branch with a fresh eval fan-out — no work task, no work attempt burned. Wrap-up failure → re-run only the publish command (the squash already landed). Cycle is untouched in every case. The failed phase is read from the escalation's `failing_task` phase, else its `reason` |
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

**Claims — human-performed attempts of any kind** — a human may **claim** a job's next work attempt (`POST .../jobs/{seq}/claim`, §6.2): "I've got this one locally." The claim does NOT change the task's kind — kind is the job type's declared *requirement* and stays immutable; the claim is an execution annotation (`performed_by: human`) covering **exactly one attempt**. Any kind is human-performable: an agent-typed code task (the human writes the code on `job/{seq}` and pushes over the §5.2 SSH front), a command-typed deploy task (the human runs the deploy by hand), and human-typed tasks trivially.

The claim is recorded on the job (`claim_next`) and consumed inside `launch_work_task` — the same serialized single-writer code path that would launch the container — so an attempt is either launched or parked, never both, with no race window. While pending (job Frozen/Blocked/Ready, or awaiting a rework/retry launch) the claim rides until launch; `DELETE .../claim` clears it. Claiming conflicts (409) while an attempt is in flight: a Running work task, or an already-parked Pending one. Once parked, the way out is resolving the task — `Pass` (with optional `summary`/`structured`, flowing into the squash-merge commit exactly like an agent's `submit_result`) proceeds to Evaluation with the branch as-is; `Fail` consumes the attempt through the normal failure path, so the NEXT attempt launches per the **declared** kind — an agent picks the work right back up, nothing to "convert back". A `Fail` resolution is a deliberate **handoff at a clean commit boundary**, not a crash: the branch is **preserved as-is** — any commits the operator pushed to `job/{seq}` before handing off survive untouched (like an eval-failure rework, contrast the container-crash reset in §3.2) — and the `Fail` `structured` notes ride into the next attempt's context exactly like eval findings, so the handoff instructions reach the agent that picks the work up. Rework cycles likewise launch unclaimed; the human re-claims if they want the rework too.

A parked claimed attempt is visibly *in progress by a human*: it appears in the pending inbox alongside Human-kind waits (distinguished by `performed_by`), the job payload's `awaiting_human` carries `"claimed": true`, and `started_at` is set at park time — the claim is the "I'm starting" declaration. Parked attempts are exempt from the §3.5 task-timeout scan (they are Pending, not Running) and survive dispatcher restarts (§3.6 recovery leaves Pending work tasks waiting on the inbox).

**Escalation resolution** — when the operator completes an escalation Human task, `action` drives the next transition:
- `Retry` — resumes at the phase that failed, never re-running finished work (see the *Escalation `Retry` — resume at the failed phase* rules above): Work exhaustion re-enters Work (same cycle, attempt++, branch used as-is — not reset to `base_ref` since the operator may have modified it); Evaluation exhaustion re-enters Evaluation with a fresh eval fan-out; a wrap-up failure re-runs only the publish. No cycle increment and no rework budget consumed in any case.
- `Resolve` — re-enters Evaluation with the current branch. The operator has done the work and is submitting it for evaluation.
- `Revoke` — terminates the job.

Escalation never bypasses evaluation — `Done` is only reachable via an evaluation pass.

**Pre-Work escalations** — escalations raised before any work task exists put the job in the **`Stalled`** state (not `Escalated`): Blocked→Stalled on re-validation failure (see §2.1); job-deadline escalation from Ready (Ready→Stalled, see §3.5); launch-time validation failure from Ready (Ready→Stalled, see §2.1 and §3.2 — a first-cycle launch parks the job it never started, whatever the failing pass). They accept only `Retry` and `Revoke`; `Resolve` is rejected with 400. For these, `Retry` re-attempts the failed step — re-runs Ready-transition re-validation for a `Blocked` park, re-enqueues the job for execution for a `Ready` one (deadline or launch validation) — rather than creating a work task. The distinction is carried by the state itself: `Stalled` has no transition into `Work` or `Evaluation`, so a mis-routed `Resolve` is impossible by construction rather than guarded at resolve time.

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

**Object store** — file storage; deletion allowed (unlike the deny-delete streams).

| Object store | Max age | Notes |
|---|---|---|
| `artifacts` | 90d | Per-task blobs (transcripts, container logs, §4.2) **and** per-job attachments (§1.6); chunked internally, so blobs are not bound by `max_payload`; gzip + age-encrypted at rest under the `age_artifacts` key |
| `outputs` | 14d, plus a byte ceiling | Per-task **output archives** harvested from work containers (§3.2); same key layout, same crypto. A **second** bucket rather than a second retention policy, because JetStream's `max_age` is a property of the bucket: a build byproduct on its own clock and its own ceiling can never displace a transcript, which is the audit record of what an agent did. Both dials are the operator's — `CHUG_OUTPUTS_MAX_AGE_DAYS` and `CHUG_OUTPUTS_MAX_BYTES` in the environment of `chuggernaut init`, re-applied to the live bucket on every init; the defaults (14 days, 8 GiB) are a starting point, and the ceiling is sized from the node's free disk. At the ceiling further outputs are **refused**, never evicted, so no stored archive is ever half-there |

---

### 1.6 Job Attachments

Operators sometimes need to carry a **file** alongside a job — a screenshot on a
bug report, a reference document, a log excerpt. These **attachments** are
operator-uploaded blobs scoped to a single job.

- **Storage.** Attachments share the `artifacts` object store with per-task
  transcripts/logs (§4.2): chunked internally so a screenshot is not bound by
  NATS's 1MB `max_payload`, and gzip + age-encrypted at rest under the
  `age_artifacts` key. Object name: `{owner}.{project}.{job_seq}.attachments.{filename}`.
  Because a task id is always numeric, the literal `attachments` segment can
  never collide with a per-task artifact key. Each object's description carries
  the client-supplied content type and original byte length, so a listing
  reports both without opening the blob.
- **Presentational, never injected.** Like [`Job::cover_html`] (§1.1), an
  attachment is reference material for humans (the operator, a human-work
  performer): it is served to the UI but is **never** injected into any agent
  prompt — the §4.3 job brief consumes only title/description. (Binary content
  such as an image is not agent-readable text in any case.)
- **API surface** (§6.2), served directly by the api against the object store —
  not through a dispatcher req/reply, which the 1MB `max_payload` would break
  for a screenshot, exactly as the per-task artifact routes already do:
  - `GET .../jobs/{seq}/attachments` — list `{ name, content_type, size }` (Viewer+)
  - `GET .../jobs/{seq}/attachments/{name}` — download the decrypted bytes under the stored content type (Viewer+)
  - `PUT .../jobs/{seq}/attachments/{name}` — upload/replace; the raw request body is the file bytes and `Content-Type` is stored (Member+). Size-capped at 16 MiB (over → 413); a path-traversal or control-character filename → 400
  - `DELETE .../jobs/{seq}/attachments/{name}` — remove (Member+); absent → 404

Attachments are independent of the job record — they are not a `Job` field, so
they may be added or removed at any point in a job's life without a state
transition, and old records need no migration.

---

## Part 2: Job Lifecycle

### 2.1 State Machine

The authoritative definition of all valid job state transitions. No transition exists outside this table.

| From | To | Trigger | Guard | Effect |
|---|---|---|---|---|
| _(creation)_ | `Frozen` | Dispatcher handles `req.jobs.create.*` | — | Write job record to KV; publish `job-created`; update `rdeps` index |
| _(creation)_ | `Draft` | Dispatcher handles `req.jobs.create.*` with `draft: true` | — | Write job record to KV; publish `job-created`; update `rdeps` index |
| `Draft` | `Draft` | `PATCH .../jobs/{seq}` accepted (full-field replace) | Job is Draft | Rewrite the creation-payload fields (type, title, description, deps, knowledge_tags, eval, timeout, model, inputs, groups); validation identical to create (deferred to release); publish `job-updated` with the changed field names |
| `Draft` | `Draft` | `POST .../jobs/{seq}/members` accepted | Job is a Draft **batch** | Add/remove members (§2.1 draft batches); adds re-validated per-candidate (422); no absorption (members stay Frozen); keeps ≥1 member; publish `job-updated` with `members` |
| `Draft` | `Ready` | `POST .../release` accepted | All deps Done | Finalize the edited definition; record `base_ref` = current default HEAD; set `ready_at`; materialize declared input defaults onto `inputs` (§1.1); publish `job-finalized` then `job-released`. **A Draft batch** first re-validates its members and absorbs them (see `Frozen→Batched`), computing the dep/eval unions + auto-description; a stale member (or <2) is a 422 and the batch stays Draft |
| `Draft` | `Blocked` | `POST .../release` accepted | At least one dep not Done | Finalize the edited definition; publish `job-finalized` then `job-released`. A Draft batch absorbs its members as in the `Ready` row |
| `Draft` | `Frozen` | `POST .../jobs/{seq}/finalize` accepted | Job is Draft | Finalize the edited definition (validate field rules + evaluator collisions like release; wiring/static config deferred to release, as for a freshly-created Frozen job §2.1); park the job Frozen (re-batchable) instead of scheduling it; publish `job-finalized`. **A Draft batch** re-validates and absorbs its members (dep/eval unions + auto-description computed) exactly as an atomic create; a stale member (or <2) is a 422 and the batch stays Draft. Validation failure rejects (422) and the job stays Draft |
| `Frozen` | `Draft` | `POST .../jobs/{seq}/draft` accepted | Job is Frozen (never released) | Reopen for editing; publish `job-drafted`. **A batch** reopened here un-absorbs its members (see `Batched→Frozen`) so membership can be edited before finalize re-absorbs |
| `Frozen` | `Batched` | A batch that names this job as a member is created (`req.jobs.create.*` with `members`), or a Draft batch naming it is finalized/released (§2.1 draft batches) | Job is Frozen, matches the batch type, is not already batched, is not itself a batch, and carries no inputs (§2.1 batches) | Set `batch_id` = the batch's seq; publish `job-batched` |
| `Batched` | `Frozen` | The owning batch is revoked/fails (§2.1 batches) | — | Clear `batch_id`; publish `job-unbatched` (the member is re-batchable) |
| `Batched` | `Done` | The owning batch reaches Done — its single merge completes every member | — | Stamp `completed_at`; publish `job-completed-via-batch` (with `batch_id`), then unblock the member's dependents exactly as an individual Done would |
| `Frozen` | `Ready` | `POST .../release` accepted | All deps Done | Record `base_ref` = current default HEAD; set `ready_at`; materialize declared input defaults onto `inputs` (add-only, only on this first pin — §1.1); publish `job-released` |
| `Frozen` | `Blocked` | `POST .../release` accepted | At least one dep not Done | Publish `job-released` |
| `Blocked` | `Ready` | Last upstream dep reaches Done | All deps Done; re-validation of static config at `base_ref` passes | Record `base_ref` = current default HEAD; set `ready_at`; materialize declared input defaults onto `inputs` (add-only, only on this first pin — §1.1); publish `job-unblocked` |
| `Blocked` | `Stalled` | Last upstream dep reaches Done | Re-validation of static config at `base_ref` fails (file deleted or renamed since release) | Create Human task describing the missing file; publish `job-stalled` |
| `Ready` | `Work` | Dispatcher picks up job | — | Create work task (cycle=1, attempt=1); create branch `job/{seq}` from `base_ref`; launch container or surface Human task; publish `job-started` |
| `Ready` | `Stalled` | `job_deadline` elapsed before work started (§3.5) | — | Create Human task noting the deadline; publish `job-stalled` |
| `Ready` | `Stalled` | Launch-time validation fails (declared secret or var missing from KV, contract error, or an input value outside the charset) | — | Create Human task naming what failed, reason `launch_validation_failed` (`config_schema_skew` for §14.2 skew); launch nothing; publish `job-stalled`. A pre-Work park (§575): no work task exists, so `Resolve` must be impossible by construction |
| `Work` | `Work` | Container work task fails, retries remain | `attempt ≤ work_retries` | Hard-reset `job/{seq}` to `base_ref` (a container failure discards the attempt — contrast rework re-entry below, and the human `Fail` handoff in §1.2, both of which preserve the branch), unless the crashed attempt pushed commits (then recovered as-is, see §3.2 crash recovery); create new task (same cycle, attempt++). An **agent** container that exits 0 but produced **nothing** (no commits beyond `base_ref` **and** empty summary) fails here too, reason `no_output_produced` (§3.2 empty-output guard) |
| `Work` | `Work` | Human-performed work attempt resolved `Fail`, retries remain | `attempt ≤ work_retries` | **Preserve** `job/{seq}` as-is (deliberate handoff at a clean commit boundary — operator commits survive); inject the `Fail` `structured` notes into the next attempt's context like eval findings; create new task (same cycle, attempt++, launched per the DECLARED kind), §1.2 |
| `Work` | `Escalated` | Work task fails, no retries left | `attempt > work_retries` | Create Human escalation task; publish `job-escalated` |
| `Work` | `Escalated` | Human work task resolved `Fail` | — | Create Human escalation task; publish `job-escalated` |
| `Work` | `Escalated` | Launch-time validation fails on a rework re-entry (cycle > 1) | — | Create Human escalation task; publish `job-escalated` |
| `Work` | `Evaluation` | Work task succeeds | — | If default HEAD moved past `base_ref`, rebase `job/{seq}` onto HEAD and set `base_ref` = HEAD (§3.2 step 9; bookkeeping, no cycle/budget change; a conflict falls through on the old base); create one task per evaluator in the lowest `stage` (§3.3 staged evaluation; attempt=1); publish `job-evaluation-started` |
| `Evaluation` | `Evaluation` | A stage completes with every required evaluator passing and a later stage remains | — | Create the next stage's evaluator tasks (attempt=1); a short-circuited stage creates none |
| `Evaluation` | `Evaluation` | Agent eval container exits without `submit_eval`, retries remain | `attempt ≤ eval_retries` | Create new eval task (same cycle, attempt++) |
| `Evaluation` | `Escalated` | Any required eval task exhausts `eval_retries` (infra error) | — | Create Human escalation task; publish `job-escalated` |
| `Evaluation` | `Work` | Eval reduce: product failure, under rework budget | evaluated cycle N ≤ `rework_budget` | cycle++; collect eval findings into rework context; **preserve** `job/{seq}` (base_ref unchanged — the prior cycle's commits carry forward; the agent fixes in place); publish `job-rework-started` (reason: `eval_failure`) |
| `Evaluation` | `Escalated` | Eval reduce: product failure, rework budget exhausted | evaluated cycle N > `rework_budget` | Create Human escalation task; publish `job-escalated` |
| `Evaluation` | `Escalated` | Eval reduce: `work.type: command` and any required evaluator failed | — | Create Human escalation task; publish `job-escalated` (rework_budget disallowed for command) |
| `Evaluation` | `WrapUp` | Eval reduce passes; `wrap_up: merge` | — | Enter wrap-up: enqueue on the per-project merge queue; publish `job-wrapup-started` |
| `Evaluation` | `Done` | Eval reduce passes; `wrap_up: none` | — | Delete the scratch branch `job/{seq}`; publish `job-done` |
| `WrapUp` | `WrapUp` | Default HEAD moved past `base_ref`; candidate squash-merge clean; merge gate required (see §3.3) | — | Create one `MergeGate` task per required command evaluator against the candidate merge commit; job remains in WrapUp |
| `WrapUp` | `WrapUp` | Squash landed on the default branch and `wrap_up.run` is declared (see §3.2) | — | Launch the `wrap_up.run` command task (phase `WrapUp`) against the merged default branch; the merge queue advances (the publish is external); job remains in WrapUp |
| `WrapUp` | `Work` | Squash-merge conflict (any cycle; rework budget NOT consumed) | — | Update `base_ref` to current default HEAD; cycle++; build conflict context (see §4.3); rebase `job/{seq}` onto the new `base_ref`, committing the 3-way-merged tree as a WIP commit (conflict markers in the conflicting hunks only; agent resolves in place); publish `job-rework-started` (reason: `merge_conflict`) |
| `WrapUp` | `Work` | Merge-gate task fails (any cycle; rework budget NOT consumed) | — | Update `base_ref` to current default HEAD; cycle++; inject gate output as findings plus conflict-style context (see §3.3); rebase `job/{seq}` onto the new `base_ref`, committing the 3-way-merged tree as a WIP commit (agent resolves any markers in place); publish `job-rework-started` (reason: `merge_gate_failure`) |
| `WrapUp` | `Done` | All required evaluators passed; merge gate passed or skipped (see §3.3); squash-merge clean or no-op; `wrap_up.run` absent or its command exited 0 | — | Squash-merge `job/{seq}` to default branch (no-op if no commits); delete branch `job/{seq}`; publish `job-done` |
| `WrapUp` | `Escalated` | Unexpected hard wrap-up failure — git plumbing / repo IO, not a conflict (design-lifecycle.md) — OR the `wrap_up.run` command exited non-zero (§3.2; the merge already landed and is NOT undone) | — | Create Human escalation task; the merge queue advances past the job rather than wedging; publish `job-escalated` |
| `Escalated` | `Work` | Operator resolves escalation with `action: Retry` and **Work** is what failed (`work_retries_exhausted`) | — | Create new work task (same cycle, attempt++, branch as-is); publish `job-escalation-resolved` |
| `Escalated` | `Evaluation` | Operator resolves escalation with `action: Retry` and **Evaluation** is what failed (`eval_infra_failure`/`eval_abort`/`rework_budget_exhausted`), OR with `action: Resolve` | — | Re-enter Evaluation (fresh eval fan-out on Retry — no new work task); publish `job-escalation-resolved` |
| `Escalated` | `WrapUp` | Operator resolves escalation with `action: Retry` and the **wrap-up** publish is what failed (`wrap_up_failed`) | — | Re-run only the `wrap_up.run` command (the squash already landed); publish `job-escalation-resolved` |
| `Escalated` | `Revoked` | Operator resolves escalation Human task with `action: Revoke` | — | See Revoked transition below; publish `job-escalation-resolved` then `job-revoked` |
| `Stalled` | `Ready` | Operator resolves a pre-Work escalation with `action: Retry`; the failed step succeeds (re-validation passes / re-enqueue) — see §1.2 pre-Work escalations | — | Record `base_ref` = current default HEAD; set `ready_at` if unset; enqueue; publish `job-escalation-resolved` then `job-unblocked` |
| `Stalled` | `Stalled` | Operator resolves a pre-Work escalation with `action: Retry`; the failed step still fails | — | Create a new Human task describing the failure; job remains Stalled |
| `Stalled` | `Revoked` | Operator resolves a pre-Work escalation with `action: Revoke` | — | See Revoked transition below; publish `job-escalation-resolved` then `job-revoked` |
| any non-terminal | `Revoked` | `POST .../revoke` | Job not already Done or Revoked | Kill Running tasks; **close Pending human/escalation tasks** (mark `Done` with a synthetic `TaskResult::Human` — `operator: "system"`, `action: Revoke` — see §1.2 revoke-closes-tasks) so none linger in the operator inbox; cascade Revoked to Frozen/Blocked/Ready dependents (transitively); dependents in Draft/Work/Evaluation/WrapUp/Escalated/Stalled left in current state (a Draft dependent is not yet committed to the graph — its dep-on-a-Revoked-job surfaces as a release-time validation error instead); delete branch `job/{seq}` if it exists; **if the revoked job is a batch, return each `Batched` member to Frozen** (clear `batch_id`, publish `job-unbatched`) rather than dropping it — members are not graph dependents of the batch, so the cascade never touches them; publish `job-revoked` |

**State descriptions:**
- **Draft** — created with `draft: true`, or a Frozen job reopened via `POST .../draft`; its definition is editable (full-field replace via `PATCH .../jobs/{seq}`) before it enters the DAG for real. Invisible to scheduling, holds no branch, cannot be claimed. Leaves via `release` (→ Ready/Blocked, which finalizes the edited definition), `finalize` (→ Frozen, which finalizes the edited definition but parks it re-batchable rather than scheduling it, `POST .../jobs/{seq}/finalize`), or `revoke`. Other jobs may declare deps on a Draft — they simply stay Blocked/Frozen until it is released and completes. Only a Frozen never-released job may return here; once released, a job is never editable again
- **Frozen** — created, awaiting operator approval; no execution begins
- **Batched** — absorbed into a batch (§2.1 batches): the member's changes will be produced on the batch's single branch, and its completion fans out from the batch's merge. Invisible to scheduling, holds no branch of its own, cannot be claimed or released — like Draft. Leaves via `Batched→Done` (batch merged), `Batched→Frozen` (batch revoked/failed; the member is re-batchable), or `revoke`
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

**Batches.** Related small jobs of the same type accumulate (e.g. web-polish tickets); running them serially wastes agent cycles, gate runs, and publishes. A **batch** collapses that: a batch *is* a job that absorbs N members of the **same type**, produces **one branch** with all the changes, is evaluated under the **union** of the members' criteria, and whose **single completion** finishes every member.

- **Creation** — `POST .../jobs` with a `members: [seq, …]` payload (plus `type`, optional `title`/`description`). Validated **at creation**: ≥2 members; each exists, is **Frozen**, matches `type`, is **not already batched**, is **not itself a batch** (no nesting), and **carries no inputs** — a batch is one run, and input values do not union the way `deps` and `eval` do (§1.1); a member with inputs is released on its own. Member-on-member deps *within* the batch are allowed — satisfied jointly, they drop out of the batch's deps; the batch depends on the **union of the members' external deps minus the members**. The members' additive `eval` lists are unioned **by name** onto the batch (identical duplicates dedup; a same-name-different-definition clash is a creation error, via the same collision primitive as per-job evaluators). `require_approval` unions as an **OR** — the batch requires sign-off if **any** member does, because one merge completes every member and the strictest member's gate has to govern the whole thing. The description defaults to an auto-index (`Batch of N {type} jobs: #a #b …`). Each member goes **Frozen→Batched** with `batch_id` set (`job-batched`).
- **Draft batches** — composing a batch is naturally an editing session, so `POST .../jobs` with **`draft: true`** stages a **Draft batch** instead of committing one atomically. The member list is validated **per-candidate** (same rules as above) but members are **not absorbed** — they stay **Frozen** (still claimable / batchable elsewhere); the draft holds a non-binding list and computes **no** dep/eval union yet. Membership is edited while Draft via **`POST .../jobs/{seq}/members` `{ add?: [seq], remove?: [seq] }`** (Draft-only, 409 otherwise; adds re-validated per-candidate, 422; `job-updated` with `members`). `PATCH .../jobs/{seq}` continues to govern title/description; membership changes only through the members endpoint. A Draft batch keeps **at least one member** (a non-empty `members` list *is* the batch marker) — the finalize/release floor of ≥2 is enforced only when it **leaves Draft**. **Absorption happens at finalize or release**: the members are re-validated against *current* state (one may have been released/claimed/batched meanwhile — a clear field error names the offender and the batch stays Draft with nothing absorbed), then the dep/eval unions and auto-description are computed and each member goes **Frozen→Batched** exactly as an atomic create would. Reopening a batch with **`POST .../jobs/{seq}/draft`** (Frozen→Draft) **un-absorbs** its members (Batched→Frozen, `job-unbatched`) for editing; finalize re-absorbs. Revoking a Draft batch is trivial — nothing was absorbed, so its would-be members are left untouched.
- **Lifecycle** — thereafter the batch is an ordinary job: release, claim/park, branch `job/{batchseq}`, reworks with branch preservation, merge gate, and the shared type's single `wrap_up` (a web batch publishes **once**). Budgets are per-job in `ExecState`, so the batch gets its own `work_retries`/`rework_budget` for the whole thing.
- **Prompt** — the work agent and every evaluator receive a batch-aware §4.3 brief: a preamble (*"This is a job batch: implement all N tickets below in this one branch; address every ticket; your closing summary must cover each by number."*) followed by each member's ticket under a `### Ticket #{seq}: {title}` heading. The reviewer judges per-ticket completeness.
- **Completion** — when the batch reaches Done its completion **fans out**: each member goes **Batched→Done** (stamping `completed_at`, `job-completed-via-batch` with `batch_id`), then each member's dependents unblock exactly as if it had run individually. The single squash's commit body **opens with the member list** (`Batch of N {type} jobs: #a #b …`, mirroring the auto-index), above the agent's closing summary and above the `Inputs:` line a parameterized job adds (§3.2), so git history records which tickets the one merge closed.
- **Failure / revoke** — a revoked or failed batch returns each `Batched` member to **Frozen** with `batch_id` cleared (`job-unbatched`), so it is re-batchable and re-releasable on its own.

`Batched` is invisible to scheduling, holds no branch of its own, and — like Draft — cannot be claimed or released.

---

### 2.2 Release Validation

Validation runs in three passes, each at the right moment:

1. **Release-time** — fast feedback before any execution is committed; checked against current HEAD. Not an execution guarantee.
2. **Ready-transition** — static file existence re-checked at `base_ref` (the exact HEAD locked when the job enters Ready). Eliminates TOCTOU drift.
3. **Launch-time** — secrets, vars and the job's own input values re-checked immediately before injection. Catches anything deleted between release and launch.

**Release-time checks (applied on every `release` request, per-job and graph-level):**

Graph wiring rules (all five must hold):
- Every dependency references an existing, non-Revoked upstream job in the same project
- No job references itself (no self-edges)
- No job references a downstream job (no cycles; full topological sort required)
- No duplicate dependencies

Static configuration (fail-fast check against current HEAD of default branch):
- The job type file (`.chug/jobs/{type}.yaml`) exists
- For `work.type: agent` jobs: `work.prompt` path exists
- For `work.type: agent` jobs with a `review` block: `work.review.prompt` path exists; the resolved review provider (`work.review.provider`, defaulting to the resolved work provider) is `claude` — inline review is not supported for other providers in v1
- If `.chug/jobs/_defaults.yaml` exists: it parses against the evaluator schema, and no default evaluator name collides with an evaluator declared in any job type being validated
- For each agent or human evaluator (including project defaults): the evaluator's `prompt` path exists
- Every secret named in `secrets:` (`work.secrets` and per-evaluator) has an entry in the `secrets.*` KV bucket
- Every var named in `vars:` has an entry in the `vars.*` KV bucket
- No declared secret or var name uses the reserved `CHUG_` prefix (§5.3)
- Every input the job supplies is declared by the type, every `required` input has a value, every `enum` value is in its `values` and every `string` value matches its `pattern` — reported per input as `field: "inputs.{name}"` (§1.1). A declared `default` satisfies the presence check; it is materialized at the Ready-transition, not here

**Ready-transition re-validation** (Blocked→Ready only; Frozen→Ready already had the check at release):

The dispatcher re-validates static configuration (job type file, all prompt paths for agent and human evaluators, and the job's supplied inputs against the type's `inputs:` declaration) at `base_ref`. If re-validation fails, the job transitions to Escalated with a Human task describing the missing file. The inputs re-check is there because the declaration is a file-derived fact pinned to `base_ref` — a type that grew a `required` input between release and unblock must fail here. Secrets and vars are not re-checked at this point — their values are live KV entries, not pinned to `base_ref`. The same write that pins `base_ref` materializes declared input defaults, exactly once (§1.1).

**Launch-time validation:**

Secrets and vars declared in the job type are checked immediately before injection. If any are missing, the job transitions to Escalated.

The job's own input values are re-checked in the same pass, against the shape floor only (charset, length, name form, count — §1.1) and consulting no declaration, since the declaration was judged at release and again at the Ready transition. A violation parks the job exactly like a missing secret. This is the belt to the earlier passes' braces — the defense-in-depth check for a record written before the rule existed — and reaching it means an earlier pass was bypassed.

Any release request that fails validation is rejected with a list of offending job instances and the specific rule violated. A job with no dependencies skips wiring rules.

All subsequent execution uses `base_ref` exclusively — the moving default branch is never consulted again after Ready-transition.

---

### 2.3 Graph Operations

Two primary modes:
1. **New feature / project**: graph is statically known and operator-approved before any work begins.
2. **Ongoing development**: jobs are added only; to remove work, revoke jobs. Large new features are planned and reviewed statically before launching.

Operators submit job creation requests via `POST /jobs`; the dispatcher creates the job record in KV and publishes `job-created`. Jobs start in Frozen state by default; with `draft: true` they start in Draft (§2.1) so their definition can be iterated on (`PATCH .../jobs/{seq}`) before release. A Frozen never-released job can be reopened to Draft via `POST .../jobs/{seq}/draft`.

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
3. On job `Revoked`: kill Running tasks; close Pending human/escalation tasks (§1.2 revoke-closes-tasks); cascade Revoked to Frozen/Blocked/Ready dependents (transitively); leave Work/Evaluation/Escalated dependents in current state
4. On `req.jobs.release.*`: run release validation (see §2.2); if valid and all deps Done, record `base_ref` and transition Frozen → Ready and enqueue for execution; else transition Frozen → Blocked
5. Execute Ready jobs from the work queue (see §3.2). The dispatcher maintains an in-memory FIFO queue of Ready job IDs. A job is enqueued when it enters Ready state (via steps 2 or 4 above, or after restart reconciliation). The execution loop dequeues one job at a time and drives it through §3.2; container monitoring runs concurrently so multiple containers may run simultaneously, but state transitions remain sequential.
6. On `req.vcs.*`: serve VCS queries by shelling out to `git` against the bare repo on disk
7. On restart: reconciliation pass (see §3.6)

**Dispatcher backends** — interface with two implementations:
- **Docker fleet** — the v1 production default: one or more Docker daemons. The single-node form (local socket; also the dev backend) and the multi-node form are the same backend — the node list just has one entry or several. Platform services (dispatcher, API, NATS, sshd, TLS proxy) run under docker compose on one node; every node runs a Docker daemon reachable over TCP+mTLS (or an SSH tunnel) and executes agent containers.
- **k8s (k3s self-hosted, or EKS/GKE/AKS)** — scale-out option beyond what a small Docker fleet covers; built when needed, not before.

**Docker fleet semantics.** The dispatcher is already the scheduler — single writer, work queue, retries — so the fleet needs no cluster orchestration, only dumb endpoints:
- **Config**: a node list, each entry `{ name, endpoint, slots }` where `slots` caps concurrent containers on that node. A single entry pointing at the local socket is the single-node deployment. Endpoints take three forms: `unix://` (local socket), `tcp://` (a Docker daemon on a private network or tunnel), and the literal **`worker`** — a NATS-proxied `chuggernaut worker` daemon on the node (below). Node names are subject-safe tokens (`[A-Za-z0-9_-]+`), validated at config parse.
- **Placement**: at launch, pick the in-service node under the platform's **placement policy** (`PLACEMENT_POLICY`, §12.4 — one setting for the whole fleet, not per job type). Two policies:
  - **`busyness`** (the default): the node with the **fewest running jobs**; ties broken by most free slots, then by name. An idle node always beats a busy one regardless of slot counts — on an asymmetric fleet (e.g. air=4 slots, nuc=2), an idle nuc (0 running / 2 free) takes the next job over a busy air (1 running / 3 free), so the small node isn't starved until the big one is nearly full.
  - **`headroom`**: the node with the **most free slots** (`slots − running`); ties broken by name. Maximizes absolute headroom for burst absorption — the original rule.

  Both policies read the same ping-provided inputs (slots + running count); no policy needs a new RPC, and both exclude full/0-slot and out-of-service nodes identically. No preemption. When no eligible node has a free slot, placement returns a distinct **no-capacity** signal (`BackendError::NoCapacity`) — a *transient* condition, not a launch failure: the dispatcher queues the launch and retries when a slot frees (see §3.5 launch capacity queue), rather than failing the task. The one affinity control is an **optional pin**: a job type may set `placement: { node: <name> }` (§1.1), which threads through `ContainerLaunchConfig.node` and forces every container that job launches onto the named node — this **overrides the policy entirely**. A pin is honored or it fails — a pinned node that is full or out-of-service yields the same transient no-capacity signal as the unpinned case (queued and retried, no spillover onto another node); an unknown pinned name is a hard launch failure naming the known nodes. The node name cannot be checked offline (the fleet list lives in the dispatcher's env), so `validate`/release only shape-check it (`[A-Za-z0-9_-]+`). No labels, no anti-affinity, and **no per-run mechanism**: that pin is a job-type field, and nothing on the job record — inputs included — participates in `ContainerLaunchConfig.node`. Worker-node free slots come from the ping reply (the worker counts its own running `chuggernaut.managed` containers). The active policy is surfaced in the platform config snapshot (§6) so the UI can show how the fleet schedules.
- **Out-of-service nodes**: a node that fails its probe is marked *out of service* — logged, excluded from placement, and re-probed on each launch, so a node recovering (or its SSH tunnel reconnecting) rejoins without a dispatcher restart. The platform snapshot's `WorkerNode.available` reflects this for the UI. Placement onto an out-of-service node never happens; a pin onto one fails like a full node.
- **Container IDs**: `ContainerId` is already opaque; the fleet backend encodes placement as `{node}/{docker_id}` and routes `wait`/`kill`/`inspect`/`copy_file` to the owning daemon.
- **Node failure**: a node dying makes its containers report not-found — exactly the condition §3.5/§3.6 already classify as task failure; retries land on healthy nodes. At startup the dispatcher **degrades rather than refusing to start**: a plain docker fleet pings every node and comes up as long as at least one node with slots > 0 responds, marking any unreachable node *out of service* (above) — this is what lets a remote worker reached over an SSH-tunneled `tcp://` endpoint be down at boot without crash-looping the dispatcher. It fails fast only when the *whole* fleet is unreachable. **Startup capacity is a fleet-level property, evaluated once across every transport: the dispatcher refuses to start only if no worker-endpoint node is reachable *and* no reachable docker-endpoint node has slots > 0.** A fleet with at least one reachable worker node starts — with a loud warning when its total capacity is zero — and launches queue via the §3.5 NoCapacity path until capacity is observed or commanded (see "Dynamic worker registration" below). A 0-slot node is placement-inert (it holds no capacity and is never chosen for placement) and therefore can *never* block startup on its own account, reachable or not — a pinned-off 0-slot placeholder beside a live worker with free slots must come up. In a mixed **worker** fleet every node is probed and marked in/out of service identically, and the "no live capacity" hard-fail is still applied *once* over the whole fleet — never per-node, and never per-sub-backend. The transports are deliberately asymmetric in **what** that one check demands of them: a docker-endpoint node must be reachable *with slots > 0*, because its slot count is static config that only a restart can change, so zero there is a fatal misconfiguration; a worker node need only be **reachable**, because its capacity is observed after boot and operator-changeable at runtime (below). Only *capacity* is narrowed this way — **reachability is not**, so a fleet with no reachable node of either transport still fails fast. The check runs **after** each node's startup probe has applied any ping-reported capacity, so it reads observed numbers rather than boot seeds. An unreachable node (either transport) is logged and marked out of service (skipped by placement, re-probed on each placement attempt), and rejoins without a dispatcher restart when it answers again.
- **Addressing discipline**: `REPO_URL` and `NATS_URL` injected into containers must be reachable from every node — never `localhost`.
- Container-internal files (MCP binaries, prompt, events batch) are **injected via the put-archive API** into the created container before start (see §4.2, §4.3) — no host bind-mounts, so nothing needs to exist on remote node filesystems (worker nodes substitute their node-local static artifacts, below). The permitted exceptions are a small closed class of **worker-provisioned node properties**, each added worker-side and never a launch input: a **node-local build cache** (see "Node-local build caching" below), a single writable mount that carries **no job state** — a build accelerator only, safe to be empty/cold, and never affecting correctness once mounted; and the **read-only toolchain mounts** of a KVM launch (see "Node-local device passthrough" below), which are the opposite — required, and fatal to the launch when absent.

**Live fleet occupancy.** The config snapshot above describes the fleet *statically* (node names, slot counts, versions); a companion `FleetStatus` record reports live *usage* so the UI can place work on nodes — the config snapshot can't, and with more than one node it is otherwise unknowable. Per node: `name`, total `slots`, `occupied` count, `available`, `version` (reusing the config snapshot's fields), plus the capacity provenance defined under "Dynamic worker registration" below — `capacity_source` (`node` | `seed`) and `capacity_observed_at`; per occupied slot: `project`, job `seq`, task `id`, task kind (`work`/`eval`/`gate`/`wrap_up`/`triage`), `job_type`, job `phase`, and `started_at`. It also carries the **launch-queue depth** (jobs parked waiting for capacity, §3.5). Occupancy is **rebuilt from the live containers the backend reports** (`list_managed_running`, tagged `{node}/{docker_id}` + `(project, job, task)`) — never from in-memory bookkeeping — so it is correct straight after a restart's re-attachment (§3.6), which reaps orphans and re-attaches survivors before the first publish. The dispatcher (the single writer) republishes a **full snapshot** to the `platform` bucket (key `fleet.status`, beside `dispatcher.config`) on every task launch/exit, writing back only when the serialized bytes change — cheap at our scale, and an idle fleet republishes nothing. The api serves it read-only (platform admins) at `GET /api/v1/platform/fleet`; no new event type is needed to push changes — every occupancy change coincides with a task lifecycle event (`task-created`/`task-queued`/`task-launched`, task/job state) already on the job-event stream, on which an SSE client refetches the snapshot.

**Worker nodes (`chuggernaut worker`).** A worker node runs a daemon that connects OUT to NATS and executes container operations against its local Docker socket — no Docker endpoint is exposed on any network and the node has no listening port. The dispatcher's fleet backend proxies each `ContainerBackend` op as JSON request-reply on `req.worker.{node}.{op}` (`launch`, `kill`, `inspect`, `copy_file`, `logs`, `ping`); `wait` is implemented dispatcher-side as an inspect poll, so worker restarts are transparent (containers keep running; the poll re-attaches). The daemon authenticates with scoped credentials minted by `chuggernaut admin worker-creds --node {name}` (subscribe `req.worker.{name}.>` + inbox reply publish — nothing else).

**Small-message discipline**: every worker request fits NATS's default 1MB max_payload. The launch request carries metadata plus small dynamic files inline (prompt, per-job credentials, harness config — KBs); **static artifacts are node-local**, provisioned at deploy time with the worker itself (the agent images, and the channel MCP binary baked into the worker's image at the same git SHA). A launch references them by name (`"channel"`) and the daemon substitutes its local copy; an unknown name fails the launch. The client side enforces a payload-size guard — a launch that exceeds it is a wiring bug (bulk bytes leaking back into the launch path), not a transport problem. **Replies are bounded too.** `logs` replies are tailed to the most recent ~700KB with a truncation marker; `copy_file` replies are bounded by the same `max_payload` but **refuse rather than truncate** — a partial file is worthless where a partial log tail is not — so a file whose base64 reply would not fit comes back as a named error carrying the path, the size and the bound, never as a reply that cannot be published and leaves the caller waiting out its op timeout. That bound is a property of the worker transport, not of `copy_file` itself: a container on a Docker-endpoint node is read in-process, with no NATS hop and no bound, so the same file that is refused on a worker node is copied whole there. A file legitimately larger than one reply — an output archive (§3.2) — travels over the separate **`copy_file_chunk`** op, which returns one bounded slice from a byte offset plus the whole-file length, so the caller reassembles it in a loop bounded by its own ceiling. The whole file is measured before the first slice is sent: one over the caller's ceiling comes back as the same named refusal, so an over-band read costs one round trip rather than a truncated archive. `copy_file_chunk` is **additive** — an N-1 daemon answers it with the unknown-op error, which decodes on both sides — so it does not bump `WORKER_RPC_VERSION` (§14.1), and during a mixed-version deploy window an output harvest against an un-refreshed node degrades to a logged miss. The ping reply carries the worker's build version and artifact hashes — and its current capacity (below) — and the dispatcher warns (never refuses) on version drift.

**Dynamic worker registration.** Fleet membership is not fixed at dispatcher boot. A worker daemon **announces itself** over NATS and the dispatcher merges it into the live fleet with **no restart** — the fix for having to edit `DOCKER_NODES` + restart just to add a node or change its slot count. The daemon publishes a **fire-and-forget** `WorkerAnnounce { node, slots, slots_max, capacity_epoch, capacity_generation, version }` on the plain (non-JetStream) subject `event.worker.announce` every ~15s; the dispatcher subscribes and forwards each announce **into the single-writer actor** as a mailbox message (`Msg::WorkerAnnounce`), so every fleet mutation happens on the actor thread — no shared registry, no locks over the decision, exactly like every other state change. Semantics:
- **Slot source — the node owns its capacity, and the scheduler reads exactly one number per node.** A worker daemon holds a current slot count (first-boot value: `WORKER_SLOTS`, default 4) and reports it over two transports of the same source: the `WorkerAnnounce` push (~15s) and the `ping` reply (pulled at the startup probe and at every placement attempt). Both carry `slots`, `slots_max`, a `capacity_epoch` stamped once at daemon start, and a `capacity_generation` the daemon bumps on every change. The dispatcher orders observations by the pair `(capacity_epoch, capacity_generation)` and applies an **announce** only when that pair is at least the last one it applied for the node, so a stale in-flight announce cannot undo a fresher observation; because the epoch advances on every daemon restart, a restarted daemon's generation-0 observations are accepted rather than discarded. A **`ping` reply is applied unconditionally and resets that watermark** — it is a request/reply on a live connection and so cannot be stale, and this guarantees no ordering anomaly can permanently freeze a node's capacity. The `ping` path also matters because it is *pulled*: a failure there marks the node out of service (loud), whereas a denied announce publish is silent on the dispatcher side. A daemon predating these fields omits the pair; a missing pair reads as `(0, 0)`, which applies only before the node's first ordered observation.
- **Precedence and merge.** `DOCKER_NODES` remains the supported static seed for *membership*. For a `worker` endpoint its slot number is a **pre-observation fallback only**: it applies until the node's first capacity observation and can never override one afterwards. The fleet records report `capacity_source` (`node` | `seed`) and `capacity_observed_at` per node, so a node still serving a seed number is visible as such rather than indistinguishable from a healthy one. Merge by node name is otherwise unchanged: a matching worker seed (or a previously-announced node) has its slot count, version, schedulability, and reachability refreshed in place; an unknown name **joins** as a new worker node with its own `req.worker.{node}.>` RPC channel; a name held by a **docker-endpoint** seed is refused (an announce cannot repurpose a directly-driven daemon into a NATS-proxied one); and newly-observed capacity is seen **immediately** — the actor re-drains the §3.5 launch capacity queue on the same turn, so launches parked for no-capacity fire onto the fresh node. Entries for `unix://`/`tcp://` docker-endpoint nodes are unaffected: `DOCKER_NODES` remains their capacity owner, and the single-source rule above is scoped to worker nodes.
- **Operator capacity control (runtime, no restart, no rebuild).** A platform admin sets a node's **desired** slot count from the operator UI (`PUT /api/v1/platform/fleet/{node}/capacity`, §6.2). The dispatcher persists it as intent in the `platform` bucket (key `fleet.capacity`, `{ slots, set_by, set_at }` per node) and sends it to the daemon as a command on `req.worker.{node}.set_slots` — a subject already covered by the daemon's existing subscribe grant, so no credential change is required. The daemon validates the value against `slots_max` (default: the node's CPU count, overridable with `WORKER_SLOTS_MAX`), adopts or rejects it, bumps its capacity generation, and announces immediately. **Intent is never read by placement** — the scheduler reads only the observed value — and the dispatcher re-pushes intent when an observation disagrees with it, bounded to one push per node per scan tick, with a rejected value treated as terminal until the operator changes it. A daemon restart or self-refresh swap therefore reverts to its boot `WORKER_SLOTS` and is reconciled back within a scan tick; worker nodes hold no capacity state of their own.
- **Lowering below occupancy drains; it never kills.** Reducing a node's cap below its live occupancy leaves running containers alone: free slots (`slots − running`) go non-positive and placement skips the node until occupancy falls under the new cap, with blocked launches queued via the §3.5 capacity queue (no retry budget consumed). `slots: 0` is a full drain — the node is placement-inert, as any 0-slot node already is, and never vetoes startup on its own account (the fleet-level rule below governs the case where *every* node is at zero). The §3.5 maximum queue wait still applies to queued launches, so a fleet-wide drain still escalates queued jobs.
- **Heartbeat loss = deregistration.** The announce doubles as a heartbeat. A dynamically-announced node whose heartbeat lapses past a timeout (default 60s, comfortably above the announce interval) is marked **unschedulable**: placement skips it and the platform snapshot shows it down, but its **already-running containers are never killed** — `route`/`wait`/`inspect` still reach the node, and the poll-based `wait` re-attaches on reconnect (§3.6 semantics unchanged). Queued launches targeted at a lost node simply wait for other capacity. A fresh announce re-admits it. **Static (`DOCKER_NODES`) seeds are never heartbeat-gated** — they keep using the ping-based out-of-service path above, so a seed that also announces (reporting its capacity) is not deregistered merely for going quiet.
- **Zero-seed boot.** The dispatcher may boot with **zero configured nodes** (`DOCKER_NODES` present but empty): startup **succeeds** with zero capacity, and launches queue via the NoCapacity path until the first worker announces and supplies capacity. This is a special case of the narrowed startup rule below, not a carve-out from it — zero seeds and zero-slot worker seeds behave identically.
- **Startup capacity, narrowed: worker capacity never vetoes the boot.** The fleet-level startup rule (stated under "Node failure" above, restated in §3.6) is: the dispatcher refuses to start only if **no worker-endpoint node is reachable** and no reachable docker-endpoint node has slots > 0. A fleet with at least one reachable worker node starts with a loud warning when its total capacity is zero, and launches queue via the NoCapacity path until capacity is observed or commanded. The asymmetry follows from ownership: a docker-endpoint node's slot count is static config that only a restart can change, so zero there remains a fatal misconfiguration, whereas a worker node's capacity is observed and runtime-changeable — zero means "not yet reported, or deliberately drained", and refusing to boot on it would make an operator-commanded drain unrecoverable from the UI that caused it. Only *capacity* is narrowed: **reachability is not**, so a fleet with no reachable node of either transport still fails fast (the crash-loop guard that catches bad credentials or a wrong `NATS_URL`). The gate is evaluated **after** each node's startup probe has applied any ping-reported capacity.
- **Capacity provenance is visible, and never-observed is a warning.** Because a seed number can stand in only before the first observation, the pair `capacity_source` / `capacity_observed_at` states which is in force per node. A worker-endpoint node that answers pings but has never been observed for capacity within a few minutes of the dispatcher's start is warned about at a bounded cadence — that signature (RPC works, announce does not) is a denied `event.worker.announce` publish grant, which is otherwise silent dispatcher-side.
- **UI surface.** Announced nodes flow into the live-fleet records (`fleet.status` and the config snapshot's `nodes`, §3.1 above), so the operator UI reflects live fleet membership — joins, slot changes, capacity provenance, and heartbeat-loss deregistrations — not just the boot `DOCKER_NODES` list. `WORKER_SLOTS` is the node's **first-boot value only**, not the way an operator changes capacity: that is the capacity control above.
- **NATS permission.** A worker daemon's minted creds (`chuggernaut admin worker-creds`, §7.4) must **allow publish to `event.worker.announce`** — in an operator-mode server a non-empty publish allow-list is strict, so without this grant the announce is denied and dynamic registration silently no-ops (for a node the dispatcher already knows, capacity still arrives on the `ping` pull, and the never-observed warning above is what makes the denial visible; an unknown node the dispatcher cannot ping stays invisible entirely). The grant rides in `auth::nats::worker_permissions` alongside `_INBOX.>`; **existing worker creds must be re-minted on deploy** for it to take effect. Only a backend that can actually route to announced nodes (the worker fleet backend) acts on announces — a single-node Docker deployment drops a stray announce rather than inserting a node it can never place work on.

**Worker self-refresh.** Worker images (the daemon, which bakes `chuggernaut` + the channel binary, and the `agent`/`agent-rust` job-container images) are built *on the node* at the deployed SHA, so a deploy must re-run those builds or the node drifts behind the dispatcher. The dispatcher host often **cannot ssh the worker** (a tailnet may block tagged→tagged ssh), so control is inverted: the worker daemon exposes a **`refresh` op** on its existing `req.worker.{node}.>` subjects. On `refresh { sha, tag }` the daemon:

1. **Fetches the build context itself** over the existing ssh front (`:2222`) with a node credential — a shallow fetch of the repo's advertised **HEAD** (its default-branch tip, the same *ref*-fetch path agent clones use), then a hard check that `FETCH_HEAD` resolves to the requested SHA before building. It fetches a ref rather than a raw commit on purpose: the ssh front's bare repos enable only `uploadpack.allowFilter` (not `allowAnySHA1InWant`), so a `want <sha>` fetch would be refused — and in the deploy path HEAD *is* the requested SHA, since prod refreshes to the commit it just checked out (tip of the platform repo it hosts itself). A HEAD/SHA mismatch aborts the refresh (drift stays, deploy warns) rather than building the wrong tree. No bytes ride the RPC and no Docker endpoint is exposed.
2. **Builds the three images locally** (native arch preserved) at that SHA.
3. **Replaces the daemon.** The daemon runs *inside* the `chug-worker` container, so it cannot remove itself; it schedules a **detached sibling** that does `docker rm -f chug-worker` + `docker run` of the new image with the same env and the **same host bind mounts** (keys, docker socket, cache). Those bind sources are recovered by inspecting the live `chug-worker` before removal — not reconstructed from the swapper's `$HOME` (the swapper runs as root, so a re-derived home would bind an empty dir and strand the daemon without NATS creds). `docker rm -f` hits only the daemon container — **in-flight job containers survive**, and the dispatcher's poll-based `wait` (an inspect poll, above) re-attaches over the new daemon and still delivers the exit code. That sibling is **named and retained** (not `--rm`) and its inner `docker rm -f` keeps stderr, so when a replacement daemon fails to start the reason survives on the node as the swapper's own log instead of being deleted with it — the node is then a node with no daemon *and* a recorded cause. Bounded: one retained swapper per node, the previous one removed at the next swap. This record cannot ride back to the dispatcher — the daemon that would report it is the thing being replaced.

The `refresh` op returns as soon as the daemon accepts (reporting the version it is refreshing *from*); the build/swap run in the background and the **new version surfaces on a later `ping`**, which clears the drift warning. The deploy flow requests refresh for every `worker`-endpoint node on every deploy, emits a `worker-refresh:{node}` leg per node, and **fails the deploy for any node that does not confirm onto the target SHA** — a deploy never reports success while a worker silently stayed behind.

**The fan-out is parallel, and the first failure cancels the rest.** Node builds are completely independent — each node rebuilds its own three images locally and swaps its own daemon — so refreshing the fleet serially made a deploy cost the *sum* of the node build times when it should cost the *maximum*. Every node's refresh is therefore requested **up front** and the confirmations are collected **concurrently**, each against its own per-node deadline, with each leg carrying that node's own elapsed time. Because a deploy that has already lost one node cannot succeed, the remaining in-flight refreshes are **cancelled** rather than left to build for another ten minutes: the daemon exposes a **`refresh_cancel { sha }` op** which aborts the refresh converging on that SHA — it signals the refresh script's **process group** (the shell *and* the `docker build` it is blocked on, SIGTERM then SIGKILL after a grace window), and the script's signal handler runs the same cleanup a failed build does, so a cancel strands no staged image generation. A cancel for a *different* target SHA is a no-op: a node converging on someone else's refresh is not this deploy's to stop. Deploy-level semantics are unchanged by cancellation — every unconfirmed or cancelled node fails the deploy, so there are no half-deploys.

**Version-skew window (why the swap is deliberately not two-phase).** A failed deploy can leave a node already swapped onto the new images while the dispatcher stays on the old SHA. This window is **benign and pre-existing**: on every *successful* deploy all worker nodes swap during the refresh step while the dispatcher only restarts several steps later, so "workers ahead of the dispatcher" is a state the platform runs in for minutes on every deploy, and the worker RPC is versioned for it. A two-phase *build-everywhere-then-swap-everywhere* shape was considered and **rejected**: the live image tags flip at the end of a node's **build** phase (the retag-swap), so gating only the daemon swap would gate the smaller half of the window, while requiring the staged temp-tagged generation to survive across two RPCs — which breaks the cleanup that stops a failed refresh from stranding a whole image generation, and a deploy dying between the phases would strand one on *every* node. Cancelling early is the mitigation that actually narrows the window. A node that had already entered its swap when the cancel arrived **stays swapped**: the daemon declines the cancel with that reason, and the reason is recorded in the node's deploy leg so the operator can see exactly which nodes moved.

**Refresh progress is observable from the deploy job.** A refresh rebuilds three images and can run for many minutes, so it must never be a silent wait — the deploy job's own task output is the diagnosis surface, not an ssh session and `docker logs` on the node. The refresh script **announces each phase before it runs** (context fetch, each of the three image builds, label verification, retag-swap, prune, daemon swap) on its stdout; the daemon reads those markers as the script streams and reports the current phase, its age, and a small bounded window of recent output lines in **`ping.refresh_progress`** (live state — `RefreshOutcome` remains the durable verdict). The deploy's confirm loop polls that same `ping` and **relays** what it sees to stdout: a line on every phase change, and an elapsed-time heartbeat while a phase runs long, so a stalled build is distinguishable from a slow one *while it happens*. The deploy's ssh transcript is streamed through unbuffered, so those lines land in the deploy task's log as they are produced. When a node never confirms, the last relayed phase and output lines are reported into the failing leg's `detail`, so diagnosis starts from the job page. After the fan-out, the deploy also copies a **bounded tail of each node's own transcript** into its stdout, node by node, so the nodes' necessarily-interleaved live lines are readable as per-node stories rather than being lost with the temp dir that held them. The worker daemon container is run with a log level that actually emits those relayed lines (`RUST_LOG` unset means the binary logs at `error` only, i.e. nothing about a refresh), so the node-side copy is a real second resort. Confirmation semantics are unchanged by any of this: only a confirmed swap passes the leg.

**Drain guarantee.** Refreshing must never interrupt in-flight job containers, and the daemon must not replace itself **between accepting a launch and the container existing**. The build phase runs with launches flowing normally (it takes minutes); only the brief **swap window quiesces**: the daemon refuses new launches with the transient no-capacity signal (queued and retried by the dispatcher, never a task failure) and waits for any accepted-but-not-yet-created launch to finish before it swaps. If the build or drain fails, the daemon reopens launches and stays on the old image — drift is surfaced, not an outage. Per-node worker version is exposed in the platform config snapshot (`WorkerNode.version`) so the UI can show fleet versions and spot drift.

**Node-local build caching.** Repeated cargo builds in agent containers otherwise compile cold every task. A worker node MAY provision a **node-local build cache** to reuse compilation across the jobs it runs. This is a **node property, provisioned entirely by the worker daemon** — the dispatcher's launch message stays small and cache-ignorant, and neither the wire launch request nor the shared launch config carries any cache field. Mechanics:

- **Opt-in per node.** The worker reads `WORKER_CACHE_DIR` (a host path on the node). Unset ⇒ caching is off and nothing changes. Set ⇒ the daemon enables the cache and creates the directory **in its own filesystem view**, which is the host's only when the daemon runs natively on the node — the shipped daemon runs containerized with that path unmounted, so its create lands in the container's writable layer and **the host path is a node-provisioning prerequisite** (design #372 C3).
- **The mount.** When enabled, the worker's own Docker backend adds a single host bind-mount of the cache dir into every container it launches, at a fixed container path (`/cache/sccache`), **writable** — sccache writes through it. The dispatcher's Docker backend never sets this, so a plain fleet stays bind-mount-free (§3.1 "no host bind-mounts"). The cache carries **no job state** — it is a build accelerator only, so an empty or cold cache is always safe.
- **Refused, not created empty.** Like the toolchain mounts below, the cache is declared as a typed bind mount rather than a bind string, so a `WORKER_CACHE_DIR` that does not exist on the node **fails the launch** — the engine's refusal names the path — instead of being materialized as an empty directory that never persists anything. A node whose cache dir is missing is therefore loudly out of service rather than silently compiling cold on every task.
- **The env.** The daemon injects `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR=/cache/sccache` and `CARGO_INCREMENTAL=0` into the launched container's environment — added purely from the node's own config, with zero dispatcher or wire involvement. `sccache` is baked into the agent-rust image; if it is ever absent, `RUSTC_WRAPPER` pointing at a missing binary degrades to an ordinary uncached build rather than failing. `CARGO_INCREMENTAL=0` is required for the cache to work at all on workspace crates: sccache declines to cache an incremental compilation, and the dev profile enables incremental for exactly those crates — so without it only registry dependencies are cached. It costs nothing here, since containers are disposable. Since #352 removed the warm-target seed (§4.1) this cache is the **only** compile reuse a build image has, which is what makes the liveness guard in `.chug/tasks/ci.sh` expensive when it trips: a run without the wrapper is a fully cold compile (275s against 186s warm, measured on air), so that gate and the per-run hit rate it prints are both loud.
- **Concurrency & ownership.** The host cache dir is provisioned with the node and owned by neither the daemon nor any container: a containerized daemon's own create never reaches it (above), and since the mount became typed no container creates it either — which is what makes "refused, not created empty" an operator-visible failure rather than an unreachable one. Concurrent containers on the same node share the one cache dir; sccache is built for concurrent access (it locks), so sharing is safe.

**Node-local device passthrough.** A worker node MAY pass a hardware device through to the containers it launches, together with the read-only host toolchain those containers need to use it — today `/dev/kvm` and a nix-provisioned Android SDK (design #367). Like the build cache this is a **node property provisioned entirely by the worker daemon**: neither the wire launch request nor the shared launch config carries a device or mount field, and the dispatcher's own Docker backend never sets one. Mechanics:

- **Opt-in per node, granted per project.** `WORKER_KVM` names the device (`1` ⇒ `/dev/kvm`); unset ⇒ nothing changes. `WORKER_KVM_PROJECTS` is a comma-separated `owner/project` allow-list matched against the launch's `JOB_PROJECT` — **empty grants nobody**, so enabling the device on a node and granting it to a project are two separate acts. The daemon **refuses to start** when the device is declared and its node is absent: a node never advertises a capability it cannot serve.
- **All or nothing.** An admitted launch gets the device (`/dev/kvm`, `rwm`) *and* the read-only toolchain mounts — the node's `/nix/store` at its own path, `WORKER_ANDROID_SDK_DIR` at `/opt/android-sdk`, and, on a node that provisions them, `WORKER_FLUTTER_DIR` at `/opt/flutter` and `WORKER_JDK_DIR` at `/opt/jdk`. Any other launch on the same node gets none of them.
- **One leaf per tool, each optional.** Each toolchain is its own setting bound at its own container path, never a shared parent directory: they are complementary rather than overlapping (Flutter ships Dart, the gradle wrapper and the engine artifacts; only the Android SDK ships `adb`, `emulator` and `platform-tools`; only the JDK gives gradle a `java`, which the nix wrappers' own JDK resolution does not reach), and a node that provisions some and not others is legal by construction. `WORKER_FLUTTER_DIR` or `WORKER_JDK_DIR` unset ⇒ no mount and no `FLUTTER_ROOT` / `JAVA_HOME`, and the launch is exactly what it was.
- **A stable path, never a store hash.** `WORKER_ANDROID_SDK_DIR`, `WORKER_FLUTTER_DIR` and `WORKER_JDK_DIR` name activation-maintained stable paths that the docker engine resolves host-side at each container create, so a launch always gets the node's current toolchain. A value carrying a nix store hash is a hard config error — a content hash in operator-typed config goes silently wrong at the next `nixos-rebuild`.
- **Refused, not created empty.** These mounts are declared as typed bind mounts rather than bind strings, so a missing source is **refused by the engine at create** instead of materialized as an empty directory the task fails inside of later.
- **The env.** The daemon injects `ANDROID_SDK_ROOT` and `ANDROID_HOME` (`/opt/android-sdk`), `ANDROID_USER_HOME`, a writable `HOME`, and — each only on a node provisioning that leaf — `FLUTTER_ROOT` (`/opt/flutter`) and `JAVA_HOME` (`/opt/jdk`) into an admitted launch, from the node's own config with zero dispatcher or wire involvement. `HOME` must stay in the container's writable layer: the emulator writes `$HOME/.android` even when `ANDROID_USER_HOME` is set.

**Node-local nix GC roots.** The toolchain mounts above hand a task nix store paths, and a store path nothing roots is garbage the node's own `nix-gc` may collect **mid-task**. A worker node MAY therefore hold a **GC root over what it realises, for exactly the task's lifetime** (design #373 P1). Like the mounts it is a **node property provisioned entirely by the worker daemon**: no wire field, no launch-config field, and the dispatcher's own Docker backend never participates. Mechanics:

- **Opt-in per node.** `WORKER_NIX_GCROOTS_DIR` names a worker-writable host directory; unset ⇒ no realise and no roots, and nothing changes. `WORKER_NIX_CLIENT` (default `/nix/var/nix/profiles/system/sw/bin/nix-store`), `WORKER_NIX_DAEMON_SOCKET` (default `/nix/var/nix/daemon-socket/socket`), `WORKER_NIX_STORE_DIR` (default `/nix/store`) and `WORKER_NIX_REALISE_TIMEOUT_SECS` (default 30) are the remaining knobs. The daemon **refuses to start** when the roots directory, the client or the socket is absent **from its own view**, or when the toolchain path it will realise does not **resolve into the store** in that same view — a containerized daemon's view is the container's, so those host paths (the store read-only, the profiles tree read-only, the socket directory read-write, the roots directory read-write **at the same path as on the host**, since the nix daemon resolves a root path in its own namespace, and — on a node that attaches a KVM device — the **directory holding** the declared toolchain path, read-only at the same path) are mounted into the **`chug-worker` container itself** and are node-provisioning preconditions rather than assumptions. The toolchain's *parent* rather than the toolchain path itself, because the client resolves `--realise`'s argument before the nix daemon hears anything and a bind whose source is the operator's symlink has it resolved away by the mount, leaving a non-store path the client refuses; the declared path is therefore required to be a direct symlink into the store. Nothing is added to a *launched* container by any of this — the closed class of launch-time node properties above is unchanged.
- **The client comes through the profiles, never out of the store.** `chug-worker` is long-lived and survives many `nixos-rebuild`s, and the engine resolves a bind source host-side at container create — so a client resolved to a store path at create is pinned to one generation and `nix.gc --delete-older-than` can collect it out from under the running daemon. The profiles tree is itself a GC root, so a client resolved through it at each use follows the node's current generation. Config naming a store hash is refused for this reason and for §3.1's SDK reason alike.
- **The realise and the root are one action, bounded.** Before an admitted launch the daemon realises the toolchain the node declares — `WORKER_ANDROID_SDK_DIR`, and only it: a Flutter or JDK leaf mounted beside it holds **no** root, which is one of the things the declared project toolchains below supersede — and registers an **indirect root named by task id** in the same command, through the nix daemon socket. **One root per task**, so a launch that declares its own `runtime.env` (below) roots that instead; the node's own toolchain loses nothing, being reachable from the system profile. The realise happens *before execution*, so no `task_timeout` covers it: exceeding `WORKER_NIX_REALISE_TIMEOUT_SECS` **fails the launch** (`Launch`, never the retryable `NoCapacity` — a realise that broke the bound will not get faster by being requeued). The bound must fit **inside the `launch` RPC's own budget**, since the realise runs within that call: a value the caller would have abandoned first is refused at config-parse time, because past it the task fails on transport and the named refusal is never seen.
- **Released at task exit, reaped after a crash.** The root is dropped when the container is removed — the same disposal step that reclaims the overlay. A worker that dies holding roots leaves them named by task id, so a bounded, best-effort reaper removes roots that no live task claims and that have outlived a grace period. Reaping is skipped entirely for a pass that cannot **count** the node's containers — the call that fails loudly on an unreachable engine, rather than the listing, which degrades an unreachable node to an empty set — and a failed reap leaks disk rather than ever failing a job.
- **Single-tenant by policy.** The socket lets the node's nix daemon spend unbounded build CPU and disk on the worker's behalf, and it sits in the process that also holds the docker socket and the node's credentials, so a node holding project toolchains serves **one** project (design #373 Decision 1).

**Project-declared toolchains (`runtime.env`).** A job type may name the toolchain its tasks run against (§1.1), and an allow-listed node realises **that** rather than its own declared one (design #373 P2). This is the one place a launch message participates in the node's nix machinery: `runtime.env` rides the launch request beside `image`, because it comes from the same resolved job-type config. Mechanics:

- **Granted per project, node-side, fail-closed.** `WORKER_NIX_PROJECTS` is a comma-separated `owner/project` allow-list matched against the launch's `JOB_PROJECT`, exactly as `WORKER_KVM_PROJECTS` is; unset or empty **grants nobody**. A job type asks for an *environment*; it never asks for a *privilege*, so no config a project can merge widens this. **Granting it grants evaluation**: flake evaluation is client-side and unsandboxed, so an allow-listed project's own flake code runs inside `chug-worker`, beside the docker socket, the NATS creds and the git key. That is tolerable only under the single-tenancy rule above.
- **Refused, never dropped.** A launch declaring `runtime.env` on a node that realises no environments, or for a project the node does not allow-list, or naming a scheme other than `nix:`, **fails the launch** (`Launch`, never `NoCapacity`) with a message naming the reference, the project and the setting. Falling through to a container without the toolchain is the silent-drop class this design exists to close; the bootstrap guard (§4.1) closes the remaining N-1 case.
- **Relative references resolve against the job branch, at its commit.** `nix:.#attr` is rewritten node-side to `git+{REPO_URL}?ref={JOB_BRANCH}&rev={JOB_SHA}#attr`, so a toolchain bump ships in the same commit as the code needing it and a push landing mid-launch cannot swap the toolchain under the task. `rev` is omitted only when the branch did not resolve to a commit at launch. Any other reference is passed to nix verbatim; a relative form that is not `.#attr` is refused, since the worker has no checkout for nix to resolve against. The rewritten fetch uses the node's own git credential, which is read-only and scoped to a single repository (§5.2), so a node resolves relative references only for the project its key was minted for; every other project names an absolute reference.
- **The realise, the root and the bound are the ones above.** The declared environment is built with the node's flake client (`WORKER_NIX_FLAKE_CLIENT`, default `/nix/var/nix/profiles/system/sw/bin/nix`, through the profiles for the same reason the store-path client is), whose out-link **is** the indirect GC root named by task id — one root per task, released at exit, reaped after a crash. A launch declaring an environment takes that root instead of the node's own toolchain root, which stays reachable from the node's system profile.
- **The task sees a store and a path.** The node's store — `WORKER_NIX_STORE_DIR`, whatever it is — is mounted read-only at its own path for such a launch, beside the KVM grant's own store mount when the two are different paths, and `CHUG_ENV_PATH` names the realised closure — one variable, whatever the environment contains, which is what retires the one-mount-per-tool shape the KVM grant still uses.
- **A cold toolchain does not run at all.** The realise is capped at 45s by the `launch` RPC's own budget while a cold Flutter/Android closure costs tens of minutes, so an environment not already substituted on the node **fails the launch** rather than running slowly. Warming is the project's, out of band: a scheduled job declaring the same `runtime.env` is warmed by this very pre-launch realise, and a binary cache is the node's `nix.conf` (design #373 Decision 5, C6).

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
    /// Remove an exited container, reclaiming its writable overlay layer.
    /// Idempotent; force=false — callers remove only after harvesting.
    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError>;
    /// IDs of exited managed containers across every node — the §3.6 sweep set.
    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError>;
    /// Running managed containers across every node, each tagged with the
    /// `(project, job, task)` it serves — the §3.6 orphan-reap set. Best-effort
    /// per node: an unreachable node is skipped, not fatal.
    async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError>;
}

pub struct ContainerLaunchConfig {
    pub image: String,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    pub files: Vec<InjectedFile>,       // written into the created container before start
    pub cpu_limit: Option<f64>,         // fractional CPUs
    pub memory_limit: Option<String>,   // e.g. "4Gi"
    pub node: Option<String>,           // optional placement pin (§3.1); None = default placement
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

**Output archives — one well-known path, no declaration.** A **work-side** container (agent work, command work, wrap-up) may leave an archive at `/workspace/chug-output.tar.gz`; the dispatcher harvests it at exit, before removal, into the `outputs` object store (§1.5) as the task's `output.tar.gz` artifact, served by the same per-task artifact routes as a transcript. `eval-result.json` is the precedent: one path the platform reads from every container of a kind, with nothing declared in the job type — so there is no `outputs:` schema, no config epoch, and no conditional-capture expression language. A script that wants failure-only capture writes the tarball in a shell `trap`; conditionality lives in the work script, where it always did. **Evaluator, merge-gate and triage containers are not read** — an evaluator's structured output already has its own channel (`eval-result.json`, §3.3), and reading the eval fan-out would multiply the disk pressure the `outputs` ceiling is sized against. Absence is the ordinary case and is silent. The archive is capped at the platform's 16 MiB blob ceiling — the same number as a job attachment (§1.6) — and an archive over it is **refused by name and stored not at all**, never truncated, because a partial archive carries nothing where a partial log tail carries its tail. The refusal is logged and the task's own result is unchanged: harvest is reporting, and a job must never fail because its reporting failed. Anything above the band belongs in a cloud bucket, not here. **Only the over-band refusal is logged at error level**, because it alone names an action its operator can take; every other harvest miss — an unreachable node, an N-1 worker that does not yet answer `copy_file_chunk` (§14.1) — is an ordinary warning, since telling a whole node's work containers to move an output to a bucket during a routine worker refresh would send its operator to the wrong action.

**Container lifecycle ends in removal.** A container's writable overlay holds a full `/workspace` checkout plus whatever the task built (a cargo `target/` is 5–10 GB), so an exited container that is never removed leaks that overlay to the host disk — enough to fill a node in a day and take the platform down (the 2026-07-21 outage). The dispatcher therefore removes each container in its task-exit handling, *after* it has harvested everything it needs from the exited container: logs (artifacts), the session transcript, `eval-result.json`, and a work container's output archive. Removal is best-effort and idempotent (`remove`, force=false) — a failed removal leaks disk but must never fail a job. Containers orphaned by a crash before that handling runs are reclaimed by the startup sweep (§3.6).

**Revoking a job drops its outputs, never its audit record.** Outputs scale with what a job *built*, so they are the next thing in the disk class `remove` exists for. A revoke deletes the job's `output.tar.gz` objects — best-effort, off the single-writer loop, the same never-fail-a-job discipline as removal — and touches no transcript, stdout or attachment: a revoked job is still the record of what an agent did. A **retry** deletes nothing, because a retry is a new task id and per-attempt artifacts already coexist under distinct keys.

---

### 3.2 Work Execution

For each Ready job, the dispatcher executes the following sequence:

1. Run the §2.2 launch-time pass (contract at `base_ref`, declared secrets/vars present, input values inside the charset), then transition job Ready → Work; create work task (cycle=1, attempt=1)
   - **A failed pass parks the job before the transition:** it stays `Ready` and moves to `Stalled` (§2.1, §575) with reason `launch_validation_failed` — `config_schema_skew` for a §14.2 skew — creating no work task and launching nothing. Only a rework re-entry, which is already past `Work`, parks `Escalated`.
   - **Claims (§1.2):** every work-task launch (this step, retries, escalation Retry, rework re-entry) first consults `claim_next` inside the serialized launch path: when set, the attempt is parked as a Pending task with the declared kind and `performed_by: human` instead of launching a container, and the claim is consumed. Steps 3–7 are skipped for the parked attempt; resolution (Pass/Fail, §1.2) drives it from there.
2. **For `work.type: agent | command`**: create branch `job/{seq}` from `base_ref`; load job type and prompt files from `base_ref`. **For `work.type: human`**: create branch `job/{seq}` from `base_ref`; surface the Human task in the operator inbox; await operator resolution (skip to step 8). `base_ref` is locked from this point — no external actor or event changes it except a squash-merge conflict (step 12), which updates it under dispatcher control before re-entering this step.
   - **Rework/conflict context (cycle > 1):** if `eval_context` is non-empty or `merge_conflict` is set, the dispatcher reads the prompt file content from the repo at `base_ref`, appends a structured context block (see §4.3 for format), and passes the combined string as the prompt to the provider. Human rework tasks have the same block appended to their inbox prompt. On cycle 1 the prompt is the file content alone. Prompt content is always resolved by the dispatcher and delivered to containers via a mounted file — never passed as a path (see §4.3).
3. Inject secrets (decrypted from `secrets.*` KV using age private key) and vars as env vars
4. Issue short-lived scoped NATS JWT for the job (see §7.4)
5. Issue short-lived SSH certificate for the job (see §7.3)
6. Launch container via the configured backend. If `work.review` is declared, the container CMD is the inline review harness rather than a direct agent CLI invocation, and the reviewer prompt (resolved from `base_ref`) plus harness config are injected at launch (see §4.5)
7. Monitor task; on container failure: increment attempt; if `attempt ≤ work_retries`, prepare `job/{seq}` (recover-or-reset, see **Crash recovery** below) and re-launch; else create Human escalation task and transition to Escalated
8. On work task success (container exit 0, or operator resolves Human task with `Pass`): proceed to Evaluation — **unless the empty-output guard fires** (see **Empty-output guard** below): an **agent** container that exits 0 but left `job/{seq}` with no commits beyond `base_ref` **and** no work summary is not a success — it is a genuine failure, retried per `work_retries`
9. **Pre-eval rebase:** if default HEAD has moved past `base_ref`, rebase `job/{seq}` onto current HEAD before launching evaluators, so evaluation tests exactly the commit stack that would merge. On success, update `job.base_ref` to the HEAD it was rebased onto (this is what evaluation ran against; it lets the wrap-up merge gate fire only if HEAD moves *again* during evaluation). This is bookkeeping, not rework: cycle is not incremented and `rework_budget` is not consumed; each replayed commit keeps its original author and committer. If the rebase conflicts (or fails), leave `job/{seq}` exactly as pushed (no commits lost) and keep the old `base_ref` — evaluation proceeds on the old base and the wrap-up merge-gate/conflict machinery (step 12) handles the stale stacking. If HEAD equals `base_ref`, no rebase happens and behavior is byte-identical to a job that never raced. Then transition job to Evaluation; create one task per evaluator. If no evaluators declared, skip to step 12 (auto-pass)
10. Fan out evaluation tasks in parallel; monitor each (see §3.3)
11. Apply eval reduce (see §3.3); handle pass or fail outcomes
12. On eval reduce pass: if the job type declares `wrap_up.type: none`, skip the merge gate and squash-merge entirely — transition straight to Done (the work's effect is external; the job branch is scratch and is deleted unmerged). Otherwise transition to **`WrapUp`** and run the merge gate (see §3.3 Merge Gate) — the whole merge path (queue, gate, squash, conflict rework) runs in `WrapUp`, not `Evaluation`. If default HEAD still equals `base_ref`, or `job/{seq}` has no commits beyond `base_ref`, the gate is skipped. If HEAD moved and the candidate squash-merge is clean, the gate re-runs the required command evaluators against the candidate merge commit — gate pass → proceed to squash-merge below; gate failure → update `base_ref` to current default HEAD, increment cycle (rework_budget NOT consumed), rebase `job/{seq}` onto the new `base_ref` — committing the 3-way-merged tree (`git merge-tree`, reusing the tree it already writes; conflict markers in the conflicting hunks only) as a single WIP commit parented on the new base — so the agent resolves markers in place rather than reimplementing, inject the gate output as findings plus conflict-style context, re-enter Work (step 2). On gate pass or skip: squash-merge `job/{seq}` to default branch. If no commits on `job/{seq}` beyond `base_ref`, this is a no-op. If commits exist and merge is clean → **land** the job (see the post-merge wrap-up command below), which transitions to Done. If conflict → snapshot the current `base_ref` into a local variable `old_base_ref` (this value is held in dispatcher memory only — not persisted to the job record), update `job.base_ref` to current default HEAD, increment cycle (without consuming `rework_budget`), rebase `job/{seq}` onto the new `base_ref` by committing the 3-way-merged tree as a WIP commit (conflict markers in the conflicting hunks only; the agent resolves them in place, NOT from scratch), build conflict context using `old_base_ref` and new `base_ref` (see §4.3 for format), re-enter Work (step 2). Eval-failure rework (§3.3, distinct from these merge-driven paths) keeps `base_ref` unchanged and **preserves** `job/{seq}` on re-entry — the prior cycle's commits carry forward and the agent fixes in place. **Guard:** a job with no evaluators auto-squashes with no review to catch markers, so before landing a squash the dispatcher scans the previously-conflicted files in the squash tree for residual conflict markers; if any remain (a WIP rebase the agent never resolved), it escalates (`unresolved_conflict_markers`) instead of merging. **Squash-merge commit message format:** subject line is `job/{seq}: {job_type}`; if the work task's `TaskResult::Work.summary` is non-null, append it as the commit body. Example: `job/42: implement-endpoint\n\nAdded /api/v1/stripe/webhook handler with idempotency key.` The body **opens** with the job's parameterization before that summary: a batch's member list (§2.1), then an `Inputs: name=value …` line when `Job.inputs` is non-empty (§1.1) — a job that is neither batched nor parameterized keeps the body exactly as the agent wrote it. Note what this line is *not*: a `wrap_up: type: none` job (`deploy`, `rollback`) produces no squash commit at all, so for those the durable record of what a run acted on is the §10.3 event stream plus the job record, never git history.
    **Finalization hard failures**: wrap-up is designed to be infallible, but an unexpected error in any finalization step (git plumbing, repo IO — anything other than a `Conflict`, which has its own rework path) creates a Human escalation task (reason `finalize_failed`) and the merge queue advances past the job instead of stalling (design-lifecycle.md: unexpected wrap-up failure → triage).
    **Post-merge wrap-up command (`wrap_up.run`)**: when the squash lands and the job type declares a `wrap_up.run` command, the job does **not** go straight to Done. Instead the dispatcher launches a command task (phase `WrapUp`, `TaskKind::Command`) that clones the **default branch** — the squash is already on it, so its HEAD carries exactly the merged content the command must ship — and the job **stays in `WrapUp`** while it runs. The merge queue advances immediately (the publish is an external effect, like a `wrap_up: none` deploy, not a merge that others block on). On exit 0 → Done (branch cleanup, `job-done`). On non-zero exit (or a container that never launched) → escalate (reason `wrap_up_failed`): the squash **already landed and is not undone** — only the external publish failed, and a human (or a manual re-run, e.g. `.chug/jobs/web-publish.yaml`) finishes it. The command must be idempotent: if the dispatcher restarts between the squash landing and the publish completing, reconciliation (§3.6) re-launches or re-verifies it — the presence of a `WrapUp`-phase task in the log is the marker that the merge is already done and only the publish remains, so recovery does not re-drive the merge queue. `wrap_up.run` requires `wrap_up.type: merge`; a `wrap_up: none` job has no merge to follow.
13. On eval reduce product failure: for `agent | human` work — if under rework budget, increment cycle, inject eval findings, re-enter Work (step 2); if rework budget exhausted, create Human escalation task → Escalated. For `command` work — escalate immediately (rework_budget disallowed). If a required evaluator returned `abort: true`, escalate immediately regardless of remaining budget (reason `eval_abort`; see §1.2 Abort verdict)
14. On escalation Human task completion: read `action`. Post-work (job `Escalated`) — `Retry`: new work task same cycle; `Resolve`: re-enter Evaluation; `Revoke`: Revoked. Pre-work (job `Stalled`) — `Retry`: re-run the failed step (Ready-transition re-validation / re-enqueue → Ready); `Resolve`: rejected (400); `Revoke`: Revoked

**Crash recovery (branch resume).** A work task that crashes — container dies, node goes out of service, dispatcher restarts mid-task — may have already pushed commits to `job/{seq}`. Because the branch name is **deterministic** (`job/{seq}`, one branch per job, stored at creation), the next attempt can find that work as a pure lookup rather than a search. So whenever an attempt re-launches for the same cycle (the step-7 retry, the restart in-flight recovery of §3.6, and escalation `Retry`), the dispatcher prepares the branch by **recover-or-reset**:

- **Branch absent** → create it at `base_ref`. A job with no prior attempt behaves exactly as before.
- **Branch present with commits beyond `base_ref`** → a previous attempt pushed before it was interrupted. Keep the branch untouched so the retry *resumes* that work instead of redoing it, and inject a **resume note** into the work agent's prompt ("a previous attempt left commits on this branch — review them before continuing") so it does not blindly duplicate work. The recovered branch is **not** rebased here; it may be behind a moved default branch, and that stale-behind case is left for the pre-eval rebase (step 9) and the merge gate (step 12) to resolve, exactly as for any solo job.
- **Branch present with nothing beyond `base_ref`** → nothing to recover; hard-reset to `base_ref` (the clean-slate retry; a no-op in this case).

Recovery targets *crashes* specifically — a real container failure, launch failure, or infra loss has no clean commit boundary, so the recover-or-reset above is the only safe rule. A **human `Fail` resolution** (§1.2) is categorically different: it is a **deliberate handoff at a clean commit boundary**, so — like an eval-failure rework — the branch is **preserved as-is** and the operator's `Fail` `structured` notes are injected into the next attempt's context like eval findings. This is what makes claim → push commit → `Fail`-with-notes a working handoff: the commits the operator pushed reach the next agent, they are not wiped. Only genuine crash/retry paths reset; a resolution never does.

**Completeness contract for work containers:** task outcome is determined by container exit code — exit 0 = work succeeded; non-zero = infra/runtime failure (retried per `work_retries`). Calling `submit_result` is optional but provides richer rework context.

**Optional cover pages on agent outputs.** `submit_result` (and an agent evaluator's `submit_eval`) may include an optional `cover_html` field alongside the canonical text summary — a small self-contained HTML cover page (a visual changelog, a before/after, a diagram) the operator UI renders beside the summary in a sandboxed frame, exactly like `Job::cover_html` (§1.1). It is **presentational only**: the text summary stays canonical and required, the cover is ignored by the merge gate and squash body and never enters any prompt, and its absence is never penalized (evaluators must not require it). It is sanitized (size-capped at ~64 KiB) and **rejected — not truncated** — at ingest on the dispatcher side before the task record is stored, so an oversized cover comes back as an actionable error the agent can react to. Stored on the task record (`TaskResult::Work`/`Agent` `cover_html`) and served through the task/reports API. **Containment is at render, not ingest:** the bytes are stored verbatim and neutralized only where they are shown — one shared choke point (`CoverWidget`), a `sandbox=""` iframe (no scripts, no forms, no same-origin) with an injected `default-src 'none'` CSP that also blocks the passive external fetches a bare sandbox still permits (an external `<img>`, a CSS `url()`/`@import`), so a hostile cover can neither execute nor phone home. Both producers (job briefs, agent outputs) share this one render path.

**Empty-output guard.** A headless work agent's session ends with its final message — when the CLI's turn ends the container dies, so an agent that defers its commit (e.g. runs verification as a background task and ends the turn before it finishes) exits 0 with all its work **uncommitted in the container filesystem**. To the dispatcher that looks like success, but `job/{seq}` carries nothing beyond `base_ref` and the work is lost. So exit 0 alone is not sufficient: when an **`agent`** work container exits 0 **and** `job/{seq}` has no commits beyond `base_ref` **and** the work summary is empty, the attempt is marked **Failed** with machine reason **`no_output_produced`** (recorded on the task result and the `task-failed` event, so the UI shows "exited without producing changes" rather than a silent Done → review-fail cycle) and routed through the normal `work_retries` relaunch path — burning a work retry, because the agent genuinely failed (distinct from an infrastructure loss, §3.6, which relaunches without spending budget). A **non-empty summary** overrides the guard: an agent whose correct outcome is *no change* says so via `submit_result`, and an empty branch with a substantive summary still proceeds to Evaluation. The guard targets exactly the observed failure signature — exit 0 **and** empty branch **and** empty summary. **Scope:** it applies to `work.type: agent` only. A `command` work task's effect is external (a deploy or build produces no branch commits by design), so its exit code stays authoritative per the completeness contract above; and Human work `Pass` is a deliberate operator resolution, never guarded.

---

### 3.3 Evaluation

After a work task succeeds, the dispatcher runs the job's evaluators as an ascending sequence of **stages** (evaluator `stage:`, default 0). When a job enters Evaluation, the dispatcher creates one task per evaluator **in the lowest stage** and fans them out in parallel. Within a stage the evaluators run concurrently exactly as an unstaged fan-out does — a job whose evaluators all share one stage (the default) is byte-for-byte the single-fan-out behavior, and that is the compatibility story. For task creation rules see §1.2. For MCP tool contracts see §4.2.

**Staged progression.** A stage completes when all its tasks reach a terminal state (Done or Failed). If every **required** evaluator in the stage passed, the dispatcher creates the next stage's tasks (one per evaluator, fanned out). If any required evaluator failed or aborted, the later stages are **not created** — they are skipped, not failed, so no task records exist for them — and the eval reduce proceeds immediately over the stages that ran. Advisory (`required: false`) failures never block stage progression. Rework/escalation semantics, abort handling, agent-evaluator retries (`eval_retries`), and human-evaluator pending states are all unchanged **within** a stage; the ordering only governs whether the next stage's tasks are created. A rework cycle re-enters Work and, on the next Evaluation, **restarts from the lowest stage** — stages are recomputed per cycle, never resumed mid-sequence. The per-job additive `eval` entries (§1.1) default to stage 0 unless the creator declares otherwise; the `_defaults.yaml` append (§1.1 Project Default Evaluators) keeps whatever `stage` each default declares and never reorders the list.

**The approval gate** (§1.1 `require_approval`) is synthesized into the resolved criteria as one required Human evaluator named `approval`, at stage `max(stage of every other resolved evaluator) + 1` — computed at resolution time, not hardcoded, so it stays last as job types and project defaults change. It is otherwise an ordinary human evaluator: it surfaces in the operator inbox, resolves `Pass` (proceed to wrap-up) / `Fail` (rework with the notes as eval context) / `Fail` with `abort: true` (escalate as unfixable), is closed by a revoke like any other Pending human task (§1.2), and survives a dispatcher restart (§3.6). Its prompt is dispatcher-supplied text, not a repo path, so requiring approval adds nothing to a project's config tree.

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

**Re-review context (cycle N > 1).** When an **agent** evaluator that already reviewed this job on an earlier cycle is launched again, the dispatcher prepends a **re-review context block** to its prompt so it focuses on what changed rather than re-deriving the whole review each cycle. The block carries: (1) **your previous review** — that evaluator's own prior verdict and structured findings (from its last `EvalResult`); (2) **what you reviewed** — the branch tip SHA the prior round judged, persisted as `Task.reviewed_tip` at each eval launch; (3) **what changed since** — the delta diff `reviewed_tip..HEAD` (one `git diff` against the bare repo, size-capped in the prompt), *unless* the branch was rebased by a conflict/gate rework since — detected from the current cycle's work `rework_reason` and an ancestry check — in which case the delta is meaningless and the block says so, deferring to the full diff in the workspace; (4) a compact **job-history digest** — a few lines per cycle with each round's verdicts, rework reasons, and the work agent's summary. The framing is explicit that the delta is *focus, not scope*: the full diff remains authoritative and the pass verdict still asserts the **whole** branch meets the bar (guarding against rubber-stamp anchoring). First reviews (cycle 1), evaluators added between cycles (no prior review), and command evaluators are unchanged. The block is assembled entirely from persisted records (the task log + the bare repo), so it survives a dispatcher restart.

#### Merge Gate

The merge gate closes the evergreen gap: eval runs against the job branch built on `base_ref`, but by the time the reduce passes, other jobs may have landed on the default branch. A textually clean merge can still be semantically broken (job A renamed a function job B calls) — without the gate, that breakage would land untested. The guarantee the gate provides: **no commit reaches the default branch without every required command evaluator passing against the exact tree that lands.**

Because evaluation entry rebases the branch onto current HEAD and advances `base_ref` to it (§3.2 step 9), evaluation already ran against the exact stack that would merge in the common case — a job that raced during **Work**. The gate therefore fires only when HEAD moves *again* between evaluation entry and wrap-up (a job that lands **during** this job's evaluation): the one remaining window where the tested stacking is stale. A pre-eval rebase conflict is the exception — it leaves `base_ref` on the old base, so the gate (or squash-merge conflict) still catches that job at wrap-up.

Applied after the eval reduce passes, before squash-merge — the job is in the **`WrapUp`** state throughout (§2.1); a pass lands (WrapUp→Done), a gate failure reworks (WrapUp→Work):

1. **Skip fast-path** — if the default branch HEAD still equals `base_ref`, or `job/{seq}` has no commits beyond `base_ref`, the evaluators already ran against exactly what will land. Skip the gate; squash-merge directly. Solo jobs — and jobs whose race was already resolved by the pre-eval rebase — pay nothing.
2. **Candidate construction** — if HEAD moved: build the candidate squash commit (the job branch's changes squashed onto current default HEAD) via `git merge-tree --write-tree` + `git commit-tree`, and point a temp ref `merge-gate/{seq}` at it. If the merge conflicts, this is the existing squash-merge-conflict path (§3.2 step 12) — the gate never runs.
3. **Gate tasks** — create one `MergeGate` task (attempt=1, current cycle) per **required command evaluator** (job type's own plus project defaults). Each runs exactly like a command eval container (§3.3 `command` semantics: bootstrap clone, exit code is the verdict, `eval-result.json` extraction, no retries) except `JOB_BRANCH=merge-gate/{seq}`. Agent and human evaluators do not re-run — their verdict is about the change; command evaluators verify the integration. The gate runs the required command evaluators **staged** — grouped ascending by `stage`, one stage at a time, stopping at the first stage that fails (job #154) — so a failure's *class* falls out of which stage failed (see the gate-fix fast path below). A single-stage gate is the flat fan-out.
4. **Reduce** — all gate tasks pass → advance the default branch to the candidate commit (this *is* the squash-merge; do not re-merge), delete `job/{seq}` and `merge-gate/{seq}`, transition to Done. A gate stage fails → classify the failure (below) and either take the **gate-fix fast path** (compile-class) or the full rework loop. Full rework: delete `merge-gate/{seq}`; update `base_ref` to current default HEAD; cycle++ (**rework_budget NOT consumed** — an integration failure is not the author's product failure; same treatment as a merge conflict); rebase `job/{seq}` onto the new `base_ref` by committing the 3-way-merged tree as a WIP commit (conflict markers in the conflicting hunks only; the agent resolves in place); inject the failing command output as `EvalResult` findings plus the conflict-style context block (§4.3: commits and diffstat of what landed since the old base); publish `job-rework-started` (reason: `merge_gate_failure`); re-enter Work.

**Gate-fix fast path (job #154).** When the gate fails on an already-approved branch and the failure is a **mechanical compile error** (a semantic collision after rebasing onto moved main — a moved/renamed symbol, a changed signature), a full re-review is waste: the right response is a narrowly-scoped fix task that repairs compilation and goes **straight back to the gate**, where gate CI still runs as the final authority. This saves two CI runs and an agent review per occurrence.

- **Deterministic classification** — because the gate runs staged (item 3), the failure class falls out of *which stage failed* + exit status, never output parsing: `compile` = the first (build) stage failed while a distinct later stage was still queued; `test` = a later stage failed (the build passed), **or** the gate is a single opaque stage that can't be told apart. An LLM triage of ambiguous failures is out of scope for v1 — deterministic-or-full-loop keeps it safe: only an unambiguous compile-class failure takes the fast path.
- **The gate-fix task** — for `compile` only: a new Work-flavor task (`rework_reason: GateCompileFix`) with a scoped brief — "the branch was approved by review; after rebase onto main it no longer compiles; make the minimal change to restore compilation; do not add features or restructure" — carrying the failing gate stage's findings. It lands on the same preserved branch (normal branch-preservation rules).
- **Short-circuit return** — on the fix task's completion, re-enter the merge gate directly (rebuild the candidate, re-run gate CI) — **no reviewer, no eval-phase CI**. The gate is forced to re-run even when HEAD hasn't moved again, because the fix was never reviewed and gate CI is its only validation. Gate CI remains the full required set — nothing lands with less validation than today.
- **Safety rails** — gate-fix rounds are bounded (2 per landing, counted separately from `rework_budget`, rebuilt from the task log on restart). On exhaustion — or if a gate-fix round then fails with class `test` — the failure falls back to the **full** rework loop (the reviewer sees it again; the failure wasn't mechanical after all). `test`-class failures always take the full loop in v1. The squash body notes the gate-fix round for audit, and the fix task carries a `gate-fix` label (§1.2/#146) so the story reads `gate-fix` rather than a bare Work row.

**Serialization** — the gate is a merge queue of depth 1: at most one job per project is in the gate at a time. Jobs whose eval reduce passes while the gate is occupied queue FIFO; each dequeued job re-checks the skip fast-path against the then-current HEAD. Since the dispatcher already merges sequentially, this adds no new coordination — just a queue in dispatcher memory.

**Bounding** — repeated gate failures don't consume `rework_budget`, so a job that genuinely can't integrate could loop Work → Evaluation → WrapUp (gate) → Work. In practice each rework rebases onto the offending HEAD, so the loop converges unless the default branch keeps moving against the job; `job_deadline` is the backstop. Set one on long-running graphs with high merge concurrency.

**Restart** — `MergeGate` tasks reconcile like command eval tasks (§3.6): exit code is the verdict; a vanished container fails the task. The candidate ref is deterministic from `job/{seq}` + the HEAD recorded when the gate opened, so the dispatcher rebuilds `merge-gate/{seq}` and re-runs the gate on restart.

---

### 3.4 Escalation

See §2.1 (state table rows for `Escalated`) and §1.2 (EscalationAction, TaskResolution, escalation resolution semantics). No additional definition here.

---

### 3.5 Timeout and Deadline

**Task timeout scan** — dispatcher periodically scans for tasks in `Running` state where `now - started_at > task_timeout`. The task is marked Failed and retry logic applies. The killed container's logs are harvested into its `stdout.log` artifact on this path too, best-effort and off the single-writer loop: the task's own exit monitor is blocked on the container's `wait`, which never returns when what broke is the node holding the container — so a task the dispatcher gives up on must not also lose the only record of why. The applicable `task_timeout` is **per task phase**: a Work-phase task uses the per-job override `Job.timeout` (§1.1) when set, else the type's `resources.task_timeout`; every other phase (Evaluation, MergeGate, Triage) uses the type default. This is what keeps the override work-scoped — evaluators are unaffected by it. The same resolved Work timeout also governs the work agent's own container deadline and the §7.4 credential TTLs, so a longer override does not outlive its credentials. Tasks in `Pending` state are not timed out — the clock starts when execution begins. Human tasks (any phase or type) are excluded from the timeout scan — they have no timeout and no automatic abandonment. This is intentional: human review gates are explicit decisions, not time-bounded.

**Launch capacity queue** — when placement reports no free slot on any node (`BackendError::NoCapacity`, §3.1), a container launch is **queued rather than failed**. The task is parked `Pending` and stamped **visibly queued**: `pending_reason: QueuedForCapacity` and `queued_at` are persisted on the task record (both cleared the instant it launches), so an operator watching the tasks table sees a *queued* badge — waiting for a fleet slot — rather than a bare Pending indistinguishable from a parked claim, and a `task-queued` event surfaces the wait. **No retry budget is consumed** — queueing is not a task failure, so it never burns `work_retries`/`eval_retries` and never escalates a healthy job the way a genuine crash would. The queue is a FIFO in dispatcher memory (single-writer: it lives in the actor, like the Ready queue). It drains when a slot frees: every container exit already flows through the dispatcher, and each such event re-attempts the queued launches (a periodic sweep on the §3.5 scan is the backstop); the *same* task record then launches — no new attempt, no attempt-number inflation. **Every** launch path queues on no-capacity: the command paths (work, evaluator, merge gate, wrap-up) inline where placement is consulted directly, and **agent evaluator** launches through a signal-back — the provider erases `NoCapacity` into a generic error, so the spawned run reports it to the actor, which parks and queues the launch exactly like a command one (this is the fix for the saturated-fleet bug where an agent evaluator instant-failed with no verdict and burned `eval_retries` within milliseconds). Genuinely-unreachable-node or other launch errors keep failing the task as before (only no-capacity is transient). **Drain priority**: finishing-phase launches (evaluation, merge gate, wrap-up) drain **ahead of** queued work launches — completing an in-flight job frees capacity fastest and bounds WIP, so a job that has finished its work never loses its evaluation slot to one that has not started; within a priority class the queue is FIFO. A launch that waits past a generous **maximum queue wait** (default 30 min) — measured from the persisted `queued_at`, so the clock accumulates across restarts — and across re-deferrals when a resumed agent-evaluator launch loses the slot race and signals `NoCapacity` back (the re-defer preserves the first defer's `queued_at`) — rather than resetting on each — is the backstop for a wedged fleet: the task is failed and the job escalates with reason `no_free_slots_timeout`, naming the capacity stall so an operator frees capacity or revokes — never by exhausting a retry budget. Queued launches survive a dispatcher restart (§3.6): a `Pending` command task with no container is re-queued on startup and resumes when a slot frees (an agent evaluator's queued relaunch is re-driven the same way from its `Pending` record); re-queueing preserves the persisted `queued_at`, so FIFO fairness within a priority class is reconstructed as it stood — not shuffled into reconcile's job-iteration order — which matters under the frequent restarts eager auto-deploy causes. The live queue is exposed read-only for display: the dispatcher answers a cheap `req.queue.list.{owner}.{project}` off the actor with a `{ depth, entries }` snapshot (fleet-wide depth and positions, entries scoped to the project so no cross-project coordinates leak), which the api forwards as `GET /api/v1/projects/{owner}/{project}/queue`; the UI derives each queued task's *position N of M* from it. Serving it on demand (rather than folding the fast-changing queue into the platform config snapshot) keeps the live order honest with the least machinery.

**Job deadline scan** — if `job_deadline` is set, the dispatcher scans for jobs in Work, Evaluation, or Ready state where `ready_at` is set and `now - ready_at > job_deadline`. Any such job is transitioned to a human-intervention state with a Human task explaining the deadline was exceeded: a job still in **Ready** (no work task yet) goes to **Stalled** (pre-work — Retry re-enqueues, Resolve rejected); a job in **Work** or **Evaluation** goes to **Escalated** (post-work — Resolve also available). If the job has a Running container at the time of deadline expiry, the dispatcher kills it (same as the timeout scan) first. Jobs already in Escalated or Stalled state are excluded — a human is already engaged. Jobs in Frozen or Blocked state (and in WrapUp — landing is platform-owned, like the merge queue) are not checked; the clock does not start until `ready_at` is set (i.e., when the job first enters Ready).

**Schedule tick** — the same scan drives time-triggered job creation (§1.1 schedules): for every loaded schedule of every project it fires one job when an occurrence falls past the schedule's anchor, skips the occurrence when a prior run is still non-terminal, and does nothing otherwise. The tick is bounded per project (at most 64 schedules) and does no git I/O — the schedule table is refreshed separately (§1.1) — and it initiates nothing while draining (§3.6). Because the decision asks whether *any* occurrence falls in `(anchor, now]` rather than whether `now` matches the expression, a slow tick coalesces rather than drops: an occurrence is observed at most one scan interval late and never early, and correctness does not rest on the interval staying under a minute.

**One-shot enforcement:** a job escalates for deadline at most once. Once the operator resolves a deadline escalation (any resolution action), deadline enforcement is permanently disabled for that job — the deadline's purpose is to summon a human, and the human now owns pacing. The scan therefore also excludes any job whose task log contains a resolved deadline escalation task.

---

### 3.6 Restart Reconciliation

On dispatcher startup, apply in order:

1. Rebuild the `rdeps` index from scratch by scanning all `jobs.*` KV records (the index is a derived cache)
2. For each task in `Running` state, first check `tasks.*` KV for a persisted `TaskResult`. If one exists, the task completed and wrote its result before the crash — use the persisted result as authoritative and proceed without querying the backend. If no `TaskResult` exists, query the configured backend by `container_id` and apply task-type-aware rules:
   - **Still running**: re-attach to container events and resume monitoring
   - **Work task, exited 0**: treat as successful completion; proceed to Evaluation
   - **Work task, exited non-0**: treat as failure; apply `work_retries`
   - **Agent eval task, exited 0**: treat as infra error (no `submit_eval` received); apply `eval_retries`
   - **Agent eval task, exited non-0**: treat as infra error; apply `eval_retries`
   - **Command eval task, exited (any code)**: exit code is the verdict; proceed as normal completion
   - **Recovered exit of a Work or WrapUp task: harvest first.** When such a task's container is found *already exited*, its logs are harvested into the task's `stdout.log` artifact — and any `@chug:leg`/`@chug:report` stream parsed into the task's structured result — *before* the container is reclaimed and the outcome above applied, exactly as a live monitor does at exit (§3.2 capture-before-removal). This is the normal path for a **self-deploy**, which restarts the dispatcher supervising it and whose container then exits within seconds: without the harvest a deploy job keeps no account of its own deploy.
   - **Container GONE (`not found`) for any Work or Evaluation task whose `container_id` was recorded** — the container demonstrably existed and then vanished (docker pruned it, the node rebooted, colima restarted): an **infrastructure loss**, categorically distinct from a real nonzero exit. The attempt is **relaunched without consuming a `work_retries`/`eval_retries` budget** — the same way a conflict rework does not consume `rework_budget` — and the retired task and its `task-failed` event are stamped with reason `infra_loss`. Infra relaunches are **capped per task** (this cycle, this evaluator; default 3): once the cap is exceeded the job **escalates with reason `infra_loss`** rather than relaunching forever, so a genuinely-vanishing environment still surfaces to a human. This does not touch the real-exit paths (a real nonzero exit keeps burning budget), the Merge Gate restart-kill behavior, or the single-writer model. A Running Work task with **no recorded `container_id`** (a launch that never reported one) cannot be proven to have run, so it keeps the plain-failure semantics above (`work_retries`), not the infra-loss path.
   - **MergeGate task** (job in `WrapUp`): a gate in flight is superseded — its Running task is failed, the `merge-gate/{seq}` candidate is dropped, and the job re-enters the merge queue, which re-opens the gate fresh against current HEAD
   - **WrapUp command task** (job in `WrapUp`, `wrap_up.run`): its presence means the squash **already landed** and only the publish remains, so the merge queue is NOT re-driven (that would re-squash). Recover the publish itself: a live container is re-attached; a task already `Done` lands the job; a task already `Failed` replays the escalation; a dead or never-launched one is **relaunched** (the command is idempotent by contract) — the publish is never dropped
   - Human eval tasks are never in `Running` state; not subject to this path
3. For each job in `WrapUp` (the merge queue is in-memory and lost on restart), re-enter it into the merge queue — with or without a gate in flight — so landing resumes. A job whose squash already landed (a `WrapUp` command task exists) is handled by the step-2 WrapUp-command rule instead, which recovers the publish rather than re-entering the merge queue
4. Transition any Blocked job whose dependencies are all Done to Ready
5. Enqueue all jobs currently in Ready state (including those that were Ready before the crash and any newly-Ready jobs from step 4) into the in-memory work queue
6. **Sweep exited containers**: list exited `chuggernaut.managed` containers (`list_managed_exited`) and `remove` each one whose task is already terminal in KV — or which has no owning task record at all. A container still bound to a live (`Running`/`Pending`) task is kept, since step 2 may re-attach to it. This reclaims overlays orphaned by a crash between a container's exit and the task-exit removal that normally frees it (§3.1). Runs after the in-flight recovery above so any container a live task will resume has been settled and protected first; best-effort, so a Docker error here only warns and never blocks startup.
7. **Sweep orphaned running containers**: list *running* `chuggernaut.managed` containers across every node (`list_managed_running`) and `kill` each one that no live `Running` task owns. Every launch stamps identity labels (`chuggernaut.project`/`.job`/`.task`, sourced from the container env), so a running container resolves to its owning task from a single list call. A container is **kept** only when it re-attaches to a live task — matched by those identity labels *or* by a live `Running` task's recorded `container_id` — so step-2 recovery's monitor still lands. Anything else is an **orphan** and is killed to free its fleet slot, logging and emitting a `container-reaped` platform event `reaped orphan container {id} for {job}/{task}`. A container carrying the marker but **no identity labels is never reaped** — only logged: the marker plus a full identity is what proves a container was launched by the dispatcher, and the marker alone is what a container *inherits from its image*. Reaping on the marker alone killed the whole worker fleet on every dispatcher restart once the built images carried the same key (#268); images now use the disjoint `chug.managed` key, and this guard keeps the sweep correct on the label's meaning rather than on any container's name. The cost is that a container predating the identity labels is no longer reaped and its slot must be freed by hand. This is the durable fix for a crash-restart that fails an in-flight task as container-gone while its container keeps running and holds a slot — left alone, the slot leaks until an operator manually removes it and every retry fails with *no free slots*. Reaping (not re-adoption) is deliberate: the task was already failed, the work is lost either way, and a re-adopted container would run unmonitored. Runs **after** steps 2 and 6, so the `Running`-task set it reads already reflects every task this boot relaunched, and still **before** the message loop and launch-queue drain start — single-writer ordering means no concurrent launch can race the reap. Best-effort per node: an unreachable node is logged and skipped rather than blocking startup or the other nodes' reap.

The task log in `tasks.*` KV is the source of truth for execution state. The configured backend must be reachable at startup, but a multi-node fleet only needs *one* node to answer — a reachable docker-endpoint node with slots > 0, or any reachable worker node, whose capacity is observed after boot (§3.1 startup capacity): unreachable nodes are marked out of service and excluded from placement (§3.1), and the dispatcher fails to start only when the whole fleet is unavailable.

**Graceful shutdown (drain).** The restart above is only lossless if each shutdown leaves records that already match reality — so reconciliation reconciles from truth rather than inferring it. On **SIGTERM** (the deploy path's `launchctl kickstart -k`; SIGINT is treated identically) the dispatcher **drains** before exiting, all inside the single-writer loop so it needs no locks:

1. **Flip into draining mode.** A `Drain` message flips a flag *inside the actor*. While draining the core initiates **no new work** — no container launches, no gate starts, no wrap-up launches (every launch path early-returns), and the §3.5 launch queue simply **holds** its entries — while it keeps **processing** messages already in flight: container exits and resolutions still record to KV normally.
2. **Sweep the mailbox.** Process every message already queued in the actor's channel (non-blocking: what is present, not what may yet arrive). This lands the writes that lived only in transit — most importantly a just-launched container's id (its `TaskContainerStarted`), which #76 stamps at launch but which can still be in the mailbox when the signal arrives.
3. **Audit and flush.** For every `Running` task still missing a `container_id`, recover it from the live fleet's identity labels (`list_managed_running`) and stamp it, so the record names the real container. Everything else that lived only in dispatcher memory is already re-derived on restart: the Ready queue (step 5), the launch queue (Pending command tasks, per §3.5), gate/merge-queue state (steps 2–3), and exec state (rebuilt during recovery). A final config snapshot (§3.1) is re-published.
4. **Exit 0** within a bounded window (default ~10s; launchd sends SIGKILL after regardless). The drain is **robust to being cut short**: each flush is an independent KV write that only ever makes a record *more* accurate, so a truncated drain never leaves records worse than an abrupt kill would.

Running agent/eval **containers are not stopped** — they keep running and are re-attached on restart (step 2, "still running"); that is the entire point. The drain explicitly does **not** wait for tasks or jobs to finish and does not drain the fleet.

---

## Part 4: Agent Interface

### 4.1 Environment Variables

**Work containers:**

```
JOB_ID          42
JOB_PROJECT     acme/api
JOB_BRANCH      job/42
JOB_SHA         4b84d25...        # the job branch's commit at launch; absent until the branch exists (§3.1 project toolchains)
BASE_BRANCH     main
REPO_URL        ssh://git@platform/acme/api.git
NATS_URL        nats://...
NATS_TOKEN      <work-scoped JWT — see §7.4>
CHUG_TASK_ID    43              # originating task id, stamped onto channel posts (§6.3)
CHUG_PHASE      Work            # originating task phase, stamped onto channel posts (§6.3)
# secrets (decrypted from age-encrypted NATS KV; named as declared in work.secrets:)
# platform agent credentials (agent containers only): every secret in the reserved global/agents scope, env-named by the secret; declared secrets win on collision
GITHUB_TOKEN    ...
# vars (from NATS KV; named as declared in top-level vars:) — CHUG_* names are reserved (§5.3) and skipped
RUST_EDITION    2021
# inputs (from Job.inputs; one key per input with a resolved value) — injected LAST, see below
CHUG_INPUT_SHA  4f9c1ab
```

**Eval containers** (command + agent; not applicable to human evaluators):

```
JOB_ID          42
JOB_PROJECT     acme/api
JOB_BRANCH      job/42
JOB_SHA         4b84d25...        # the job branch's commit at launch (§3.1 project toolchains)
BASE_BRANCH     main
REPO_URL        ssh://git@platform/acme/api.git
NATS_URL        nats://...
NATS_TOKEN      <eval-scoped JWT — see §7.4>
JOB_TASK_ID     43              # eval task id; addresses req.eval.submit (§4.2)
CHUG_TASK_ID    43              # originating task id, stamped onto channel posts (§6.3)
CHUG_PHASE      Evaluation      # originating task phase, stamped onto channel posts (§6.3)
CHUG_EVALUATOR  review          # evaluator name, stamped onto channel posts (§6.3)
# secrets (only those declared in the evaluator's own secrets: field)
# vars (from NATS KV; named as declared in top-level vars:) — CHUG_* names are reserved (§5.3) and skipped
RUST_EDITION    2021
# inputs (from Job.inputs) — evaluators receive them too, see below
CHUG_INPUT_SHA  4f9c1ab
```

**Job inputs (`CHUG_INPUT_*`).** An input declared as `sha` (§1.1) is delivered as `CHUG_INPUT_SHA` — `name.to_uppercase()` under one reserved namespace, which is injective because input names are lowercase-only. The namespace sits inside the `CHUG_` prefix §5.3 reserves for secrets *and* vars, so no project-declared name can collide with an input; inputs are therefore injected **last**, asserting at the insertion site that the namespace was empty. Delivery rules:

- **One key per input with a resolved value**, and nothing else. `Job.inputs` is the effective set (supplied values plus materialized defaults, §1.1), so a declared *optional* input with neither a supplied value nor a `default` gets **no key at all** — never an empty string. That is what lets a `set -eu` script fail loudly instead of acting on a blank argument, and what makes `${CHUG_INPUT_X:-fallback}` mean what its author expects.
- **Work, wrap-up and eval containers all receive them.** An evaluator is a gate, and a value in a gate's environment can only matter if a repo author first wrote a branch that reads it — a reviewed commit in the project repo, on the same path as the `eval:` declaration itself. Choosing *which* script runs is the capability an input may never have (§1.1: no job-type field is selectable by an input); supplying a value to a script someone already wrote is not. The eval floor holds because an input cannot open a path, only travel one, and both the declaration and the value are on the record (§10.3). A work-only narrowing was rejected because §4.3 sends the job brief to every agent evaluator by construction, so it would leave the *more* suggestible gate reading inputs.
- **Values are re-checked immediately before injection** (§2.2 launch-time pass): a value outside the default charset parks the job rather than launching it.
- A job whose `inputs` is empty — every job of every type that declares none — gets a container env byte-identical to one composed without the feature.

**Workspace bootstrap:** the dispatcher wraps every work and eval container CMD with a standard bootstrap: `git clone --branch $JOB_BRANCH $REPO_URL /workspace && cd /workspace && exec {original CMD}`. Images must provide `git` and an SSH client but do not perform the clone themselves — the platform enforces the `/workspace` contract (command evaluators write `eval-result.json` there; eval agents inspect the diff there).

**A declared `runtime.env` is bootstrapped or refused, never skipped.** When the job type declares one (§1.1), the same wrapper is prefixed with a guard: the task runs only if the node injected `CHUG_ENV_PATH` — the store path it realised (§3.1) — **and that path is present in the container**, and then runs with `$CHUG_ENV_PATH/bin` at the head of `PATH`. The second half catches a node that injected a path out of a store it did not hand over, which would otherwise put a non-existent directory on `PATH` and pass the guard. A node that dropped the declaration therefore fails the task **loudly, naming the reference**, instead of building against whatever the image happens to carry; that is what makes the wire field additive without the N-1 silent-drop class §14.1 exists to prevent. A job type declaring no env is wrapped byte-identically to before.

**Build caching is the node's job, not the image's (image property, not dispatcher machinery).** A build image bakes a toolchain and **no compiled artifacts of the project itself**; compile reuse comes from the node-local cache (§3.1) alone. Like that cache this is entirely an **image concern** — it adds nothing to the launch message, the wire, or the dispatcher, and touches no per-job state. The `agent-rust` image (`deploy/prod/Dockerfile.agent-rust`) bakes an empty `CARGO_TARGET_DIR=/opt/chug-cargo-target`, a path *outside* `/workspace` so a multi-gigabyte `target/` never lands in the clone the agent commits from. That path must be a **literal, stable one**: sccache's hash covers the target-derived paths cargo passes rustc, so a per-job or per-container target dir shares nothing with the node cache and silently drops its hit rate to zero.

An image MAY instead bake a prebuilt `target/` — a *warm-target seed* — and `agent-rust` did until #352. It was deleted as **measured redundant**: against the real `.chug/tasks/ci.sh` command set on air, a seeded build took 141s and an unseeded one on a warm node cache 186s, for a seed costing 2.26GB in every image, ~600s on every worker-refresh leg (§3.1), and the bulk of that refresh's disk peak. A node cache holding the same workspace costs ~479MB, is shared by all concurrent containers on the node instead of copy-on-written per container, and stays warm between deploys instead of going stale until the next image rebuild. A seed is therefore justified only where the node cache cannot reach — and where it is used, it must not change the build **profile** (a seed compiled differently from what agents run has different fingerprints, and cargo ignores it entirely).

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

Provider and model are configured at platform level (see §12.4) with per-job-type overrides at `work:` level or per eval step. The model resolution chain is: per-job override → job-type declaration → project default (`.chug/jobs/_defaults.yaml`) → platform default (§12.4). Provider resolution is job-type declaration → platform default; per-project provider defaults and team-level defaults are deferred.

**`ClaudeProvider`** — container CMD: `claude -p "$(cat /chuggernaut/prompt.md)" --settings /chuggernaut/agent-settings.json --output-format stream-json --verbose --model {model} --append-system-prompt {system_prompt} --mcp-config /chuggernaut/mcp-config.json`. `--output-format stream-json` (which `-p` requires be paired with `--verbose`) makes the CLI emit JSONL events — assistant text, tool_use, and a final `type:"result"` event — to stdout *as it works*, so the live log viewer (and the harvested `stdout.log` artifact) show intermediate activity rather than silence until exit. The final result event carries the same `usage`/`result`/`session_id` fields the single-object `json` format did, so usage/result harvesting is unchanged; exit-code semantics are unchanged. The serialized `Vec<McpServerConfig>` (`{"mcpServers": …}`) carries the channel server's `NATS_CREDS`, so it is **not** passed inline on argv — inline argv leaks into `ps`, `/proc/*/cmdline`, and crash reports. Instead the dispatcher-composed payload is injected as a mode-0600 file at `/chuggernaut/mcp-config.json` (the CLI accepts a path for `--mcp-config`); credentials travel via injected files or container env, never argv.

**Permission profiles** — every agent run carries a **permission profile**, injected as a mode-0644 settings file at `/chuggernaut/agent-settings.json` and passed via `--settings`. The profile is chosen by **role** at the launch site, not declared in `.chug/jobs/*.yaml`:

| Role | Profile | Policy |
|---|---|---|
| Work (§3.2), factory triage (§13.4) | `Work` | Permissive: `Bash`, `Edit`, `Write`, the read tools, and the channel MCP server. Work agents build, edit, commit and push. |
| Agent evaluator (§3.3), job triage (§1.2) | `Review` | Read-only: the read tools, the channel MCP server, and a narrow set of `Bash(git …)`/inspection prefixes. **No bare `Bash`**, so any unlisted command is denied; `cargo`, `npm`, `npx` and `make` are additionally denied explicitly. |

The container remains the security boundary (there is no adversary model here — a work agent is trusted with the workspace). The profiles exist to direct *effort*: an evaluator that runs the build spends minutes of a shared Docker host reproducing exactly the signal the stage-1 `ci` command evaluator is about to produce, and delays its verdict to do it. Compiling, testing and linting belong to command evaluators; agent evaluators read the diff and judge it against the brief.

Because these runs are headless, a denied tool call is reported to the model as a denial and the run continues — it never blocks on a prompt nobody can answer. Denials are parsed from the result object's `permission_denials` and logged at WARN by the harvester, so a mis-scoped profile surfaces as an operator-visible warning rather than silently degraded work.

**`CodexProvider`** — before launch, the dispatcher serializes `AgentRunConfig.mcp_servers` into Codex's MCP config format and injects it into the container at `/repo/.codex/config.toml` (same mechanism as the prompt file). Container CMD: `codex exec "$(cat /chuggernaut/prompt.md)" --model {model}` (system prompt prepended to the prompt file content, no native flag).

Structured context surfaces through MCP tools (`submit_result`, `submit_eval`), which send NATS request-reply messages to the dispatcher — the dispatcher never parses agent stdout.

**Job brief format** — when a job carries a `title`, a `description` and/or `inputs`, the dispatcher appends this block to the prompt file content for the work agent, for every agent evaluator (evaluators judge against the same brief the author saw), and to Human task prompts:

```
---
## Job Brief
**{title}**

{description}

### Inputs
<untrusted_input>
image_tag: 4f9c1ab
service: web
</untrusted_input>
```

**Inputs in the brief** — the `### Inputs` subsection carries the job's **effective** inputs (§1.1), one `name: value` line per input with a resolved value, in name order. A job whose `inputs` is empty — every job of every type that declares none — emits **no** subsection, not an empty one, so its brief is byte-identical to one composed without the feature (the prompt-side twin of the `CHUG_INPUT_*` rule in §4.1). The subsection is nested at `###` under `## Job Brief` and never emits a sibling of it: the input charset (§1.1) excludes `#` and every newline, so no value can promote itself to a heading, and the `<untrusted_input>` delimiter is a readability aid for the model rather than the control. Agent evaluators receive it for the same reason they receive the rest of the brief — an evaluator judges against the same target the author saw, and the value is a value, never a capability (§4.1).

**Prompt cleanliness** — the job brief is the single point where a job's instance identity enters any prompt, and it consumes **only** `title`, `description` and `inputs`. The job record's `cover_html` (§1.1) is deliberately excluded: it is a presentational cover page for the operator UI and must never reach an agent. Because there is exactly one choke point, this is airtight — a regression test asserts the brief block is byte-identical with and without `cover_html` set. Rich formatting therefore never pollutes what work, eval, or triage agents read; the description stays plain text/markdown.

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

The block also states that **the merge is already committed on `job/{seq}`**: the dispatcher has rebased the branch onto the new `base_ref` and written the 3-way-merged tree as a WIP commit, with conflict markers (`<<<<<<< / ======= / >>>>>>>`) present only in the listed files. The agent must **resolve the markers in place and commit** — not reimplement the change from scratch, since everything else already merged cleanly onto the new base.

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

**Upfront injection (tagged, work containers only) — IMPLEMENTED, repo-versioned form:** job types declare default knowledge tags (`knowledge:`); operators may add more at job creation. At launch the dispatcher resolves the union by reading `.chug/tags/{tag}.md` from the project repo at `base_ref` (tags without a file are skipped), concatenates them into a `## Project Knowledge` block, and injects it via `--append-system-prompt` for Claude (prepended to the prompt for Codex). Work containers only — eval containers do not receive a pre-injected system prompt. The repo is the primary tag store; the NATS KO buckets described below (global/team scopes, runtime queries via chuggernaut-ko) remain the deferred extension for knowledge that spans projects.

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
| user email (`platform_admin`) | `refs/heads/job/{seq}` in any project | any ref in any project |
| user email (Member+ on project) | `refs/heads/job/{seq}` only | any ref on projects where Viewer+ |
| user email (Viewer on project) | none | any ref on projects where Viewer+ |

Job principals embed the owner and project because job seqs are only unique per project — a bare `job-{seq}` principal could not be authorized against the right repo.

Per-project read authorization is enforced against the user's `project_roles` claim in their SSH cert extension. A `platform_admin` flag rides alongside that claim in the cert (see §7.3): it is treated as Member+ on every project for push and Viewer+ for pull, so an admin can operate any job branch without an explicit role grant — but the default branch stays dispatcher-only even for a platform admin. Certificates issued before the flag existed carry no admin bit and keep the role-only behavior.

For credential issuance details, see §7.3 (User SSH certs) and §7.4 (Per-job SSH certs).

---

### 5.3 Linked-Origin Projects

A project may be **linked** to an existing externally-hosted repo (GitHub): the external host owns the default branch, chuggernaut never pushes it, and work ships as pull requests. Classic self-hosted projects are unchanged; a project is linked iff its `projects.{owner}.{project}` KV record has `origin` set.

**Branch model.** The local bare repo's `HEAD` symref points at a chuggernaut-owned **`integration`** branch, so the entire §3.2/§3.3 merge machinery (job branches, squash-merge, merge queue, merge gate, SSH branch protection) operates on `integration` untouched — "default branch" *is* integration. The origin's default branch is tracked as `refs/remotes/origin/{main}` via a normal fetch refspec: not a local head, so unpushable through the SSH front. Agents keep talking only to the internal SSH front / local repo; `REPO_URL` never points at the origin.

**Creation** (`req.projects.link`): init bare + `remote add origin` + single-branch fetch refspec; origin main autodetected via `ls-remote --symref` when unspecified; `integration` created at origin main; pre-receive hook installed; the config subset of the starter template (.chug/jobs/, .chug/prompts/, .chug/tasks/ — no README) seeded **skip-existing** onto integration, reaching the origin via the first release PR.

**Credentials.** Project secrets `CHUG_ORIGIN_DEPLOY_KEY` (OpenSSH private key, write deploy key — git fetch/push) and `CHUG_ORIGIN_PAT` (fine-grained PAT, pull requests read/write — PR API), set via `admin secret set` before linking. The `CHUG_` name prefix is **reserved**: declaring such a **secret or var** in a job type is a release-validation error and injection skips them — origin credentials are dispatcher-only and never reach a container, and the §6.3 task-origin stamps (`CHUG_TASK_ID`, `CHUG_PHASE`, `CHUG_EVALUATOR`) share the namespace, so neither may be shadowed by project config. Secret and var names are KV-validated to `[A-Za-z0-9_]+` at write time (§1.4), so the prefix is legal to *store* — the job type is where it is refused. Origin git ops decrypt the key to a 0600 tempfile for the duration of the command (`GIT_SSH_COMMAND`, `StrictHostKeyChecking=accept-new`) with a 60s timeout so a hung remote cannot wedge the single-writer actor.

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
req.health                                                   no payload; response: { dispatcher: "ok", version } — round-trips the core actor; the api's GET /api/v1/health (§6.6) bridges it. No responder / wedged actor → no reply → the api returns 503.
req.fleet.capacity.set                                       payload: { node, slots, by }; response: { node, desired, observed, state } — records the operator's DESIRED slot count as intent (`fleet.capacity`, §3.1) and commands the node on `req.worker.{node}.set_slots`. Answers without waiting on the node RPC (the actor is single-threaded), so the reply is "recorded and converging"; 404 unknown node, 409 a docker-endpoint node (DOCKER_NODES owns those). Platform admins only at the API layer.
req.jobs.create.{owner}.{project}
req.jobs.get.{owner}.{project}.{seq}
req.jobs.list.{owner}.{project}
req.jobs.release.{owner}.{project}.{seq}
req.jobs.revoke.{owner}.{project}.{seq}
req.jobs.claim.{owner}.{project}.{seq}
req.jobs.unclaim.{owner}.{project}.{seq}
req.jobs.criteria.{owner}.{project}.{seq}                    response: { ref, wrap_up, evaluators: [Evaluator + source: "type"|"job"], errors: [string] } — resolved eval criteria at the job's pinned ref
req.jobtypes.get.{owner}.{project}                           payload: { name }; response: { name, ref, path, yaml, job_type: JobType|null, errors } — one type in full (raw + parsed, defaults merged) for the library UI; `path` is the location the definition resolved to (§1.1)
req.tags.list.{owner}.{project}                              response: [{ name, path }] — available knowledge tags (`.chug/tags/*.md` at default HEAD; a tag's meaning lives in its repo-versioned markdown file). `path` is the location the tag resolved to (§1.1), so a reader fetching its contents back never re-guesses the layout
req.projects.create                                          payload: { owner, name }; creates the bare repo (+ pre-receive hook, + Code starter template seed, + job counter); 409 if it exists. Platform admins only at the API layer.
req.projects.link                                            payload: { owner, name, origin_url, main_branch? }; linked-origin project creation (§5.3): fetch from origin, HEAD → integration, config seed (skip-existing), project record + counter. Requires CHUG_ORIGIN_* project secrets first. Platform admins only at the API layer.
req.origin.release.{owner}.{project}                         open an origin release (§5.3): push integration → chug/release-{n} on the origin, open the PR, hold the merge queue; 409 when a release is open / a gate is in flight / nothing to release
req.origin.status.{owner}.{project}                          response: { origin, release, release_counter, origin_main_sha, integration_sha, ahead_by, held }; opportunistically reconciles an Open release's PR state
req.origin.sync.{owner}.{project}                            fetch the origin and reconcile (§5.3): merged PR → reset integration onto new origin main + clear hold; closed → clear hold, no reset
req.vcs.file.{owner}.{project}                               payload: { path }; response: { path, ref, content } — one repo file at default HEAD (prompt viewer / repo browser)
req.vcs.tree.{owner}.{project}                               response: { branch, ref, entries: [{path, type, size}] } — full recursive tree at default HEAD (repo browser)
req.graph.get.{owner}.{project}                              response: Job[] (all jobs in project)
req.groups.list.{owner}.{project}                            response: [{ name, doc_path?, doc_status?, jobs: [{id, type, title, state}], counts: {State: n}, open }] — every group the project's jobs name (§1.1 `groups`), DERIVED at read time from the job records: a group exists because a job says so, so no aggregate is stored and an empty group is unrepresentable. `counts` omits zero states; `open` counts non-terminal members. `doc_path`/`doc_status` are best-effort, only for a design/-namespaced name resolving to docs/design/{stem}.md at default HEAD
req.designs.list.{owner}.{project}                           response: [{ path, slug, seq?, title, status?, status_stale, name, jobs, counts, open }] — every docs/design/*.md at default HEAD joined to its group's roll-up (the same shape req.groups.list serves). Repo-derived, so a design with NO jobs is a row. `status` is the verbatim first `Status:` line; `status_stale` flags a design whose members all reached a terminal state — at least one of them a member other than the design's own authoring job (the member whose seq is the document's number) — while that line still says something. Bounded by DESIGNS_MAX
req.graph.validate.{owner}.{project}
req.graph.release.{owner}.{project}
req.vcs.diff.{owner}.{project}.{seq}                         payload: { since? } (byte cursor, default 0); job seq is in subject. Replies with ONE PAGE of the diff (see §6.2) — the text is capped per reply so a diff of any size fits under NATS's max_payload, and every page carries a `digest` of the whole diff so a caller can tell pages of one diff from pages of a diff that moved; a summary too large even for an empty page is a 422, never a reply that cannot be published
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
req.ssh.sign-user-cert              payload: { public_key, email }; response: { certificate } — §7.3 user cert. `email` is the authenticated caller's, injected by the API from the JWT (never client-supplied); the dispatcher loads that user's roles from the record and signs a 24h cert (principal = email, roles in the forced command). 503 when the CA key isn't mounted; 404 when the user record is missing.
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
       cover_html optional; a rich presentational cover page (§1.1) rendered above the description in the UI, never injected into any prompt; size-capped at ~256 KiB (a larger value → 422)
       knowledge_tags optional; merged with job type's default tags
       eval optional; additive per-job evaluators layered on top of the type's list (§1.1 evaluator schema); validated at release — name collisions with the type's evaluators are a 422
       inputs optional; { name: value } for the type's declared inputs: (§1.1). Shape-checked here (name form, charset, length, count → 422); whether each name is declared and each value satisfies its declaration is release-time, reported as inputs.{name}. Declared defaults are materialized at the Ready-transition, not echoed back on this reply
       require_approval optional; bool, default false — gate the job on an explicit operator sign-off (§1.1). Additive to the type's criteria, never a replacement
       groups optional; [name] — what the job is part of (§1.1). Shape-checked here (count, name shape, name length, duplicates → 422) and nowhere else: a group has no declaration to check against and no registry to exist in, so nothing defers to release. A label can equally be added later, in any state, via PUT .../jobs/{seq}/groups
       draft optional (default false); with draft:true the job lands in Draft (§2.1) so its definition can be edited before release, instead of Frozen
       members optional; with a non-empty members:[seq,…] the request creates a BATCH (§2.1 batches) instead of an ordinary job — it absorbs those existing Frozen same-type jobs (→ Batched) and lands one branch for all of them. Validated at creation (≥2 members, each Frozen/same-type/not-already-batched/not-a-batch/no-inputs); 422 otherwise. With draft:true the batch lands in Draft and members are NOT absorbed (they stay Frozen) — a DRAFT batch composed via the members endpoint below and absorbed only at finalize/release (§2.1 draft batches)
       → 201 Created; body: Job record (a batch carries members; each absorbed member carries batch_id)
GET    /api/v1/projects/{owner}/{project}/jobs                      → 200 OK; body: Job[]
PATCH  /api/v1/projects/{owner}/{project}/jobs/{seq}                → 200 OK; body: Job; Member+. Full-field replace of a Draft job's definition (same body shape as create, minus draft); 409 unless the job is Draft (§2.1). Validation identical to create (deferred to release)
POST   /api/v1/projects                                             body: { owner, name } → 201; platform admins only. Creates repo + hook + Code starter template + counter (§12.2); 409 if it exists.
POST   /api/v1/projects/link                                        body: { owner, name, origin_url, main_branch? } → 201; platform admins only. Linked-origin creation (§5.3); 422 when the CHUG_ORIGIN_* secrets are missing; 409 if it exists.

# Members (project-role management, §7.3/§7.5)
GET    /api/v1/projects/{owner}/{project}/members                   → 200 OK; body: { members: [{ email, role }] } — users holding a role on the project. Platform admins only.
PUT    /api/v1/projects/{owner}/{project}/members/{email}           body: { role: "owner"|"member"|"viewer" } → 200 OK; grants/updates the user's role. Platform admins only; 404 if the user is unknown. (`owner` = the top project role, `admin`.)
DELETE /api/v1/projects/{owner}/{project}/members/{email}           → 200 OK; clears the user's role on the project. Platform admins only; 404 if the user is unknown.

# Origin (linked projects, §5.3)
GET    /api/v1/projects/{owner}/{project}/origin                    → 200 OK; body: { origin, release, release_counter, origin_main_sha, integration_sha, ahead_by, held }; 404 on classic projects. Viewer+.
POST   /api/v1/projects/{owner}/{project}/origin/release            → 201; opens the release PR and holds the merge queue; 409 when a release is open / gate in flight / nothing to release. Project Admin.
POST   /api/v1/projects/{owner}/{project}/origin/sync               → 200 OK; fetch + reconcile (merged PR → integration reset + hold cleared). Project Admin.
GET    /api/v1/projects/{owner}/{project}/job-types                 → 200 OK; body: [{ name, display_name, description }] (.chug/jobs/*.yaml at default HEAD; display metadata for the type picker — a file that fails to parse still lists, stem only)
GET    /api/v1/projects/{owner}/{project}/job-types/{name}          → 200 OK; body: { name, ref, path, yaml, job_type, errors } — the library view (raw + parsed, defaults merged; path = where the definition resolved)
GET    /api/v1/projects/{owner}/{project}/groups                    → 200 OK; body: [{ name, doc_path?, doc_status?, jobs: [{id, type, title, state}], counts, open }] — the groups the project's jobs name, with each group's members and per-state roll-up (§1.1 `groups`). Viewer+. DERIVED on read: no count, member list or enumeration is stored anywhere, so a group cannot disagree with the job records. Ordered by name; `counts` omits zero states; `open` counts members that are not terminal. `doc_status` is the design document's `Status:` line, VERBATIM and unparsed — the platform compares it to nothing
GET    /api/v1/projects/{owner}/{project}/designs                   → 200 OK; body: [{ path, slug, seq?, title, status?, status_stale, name, jobs, counts, open }] — the design registry: every docs/design/*.md at default HEAD with its verbatim status line and the roll-up of the jobs grouped under design/{slug}. Viewer+. The complement of /groups, not a duplicate: /groups is member-derived and /designs is repo-derived, so a design NOBODY HAS FILED A JOB AGAINST is a row here (empty job list) and cannot be one there. `status_stale` = has a member other than its own authoring job (the member whose seq is the document's number — a design belongs to its own group, so counting it would call every design stale the day it merged), every member terminal, status line non-empty — reported, never acted on: the repo stays the source of truth for a design's status (design #321 Decision 8)
GET    /api/v1/projects/{owner}/{project}/tags                      → 200 OK; body: [{ name, path }] — available knowledge tags (.chug/tags/*.md; drives the create-form tag picker; path = where the tag resolved)
GET    /api/v1/projects/{owner}/{project}/file?path={path}          → 200 OK; body: { path, ref, content } — one repo file at default HEAD (the create form's prompt links; 404 if absent)
GET    /api/v1/projects/{owner}/{project}/tree                      → 200 OK; body: { branch, ref, entries } — full recursive tree at default HEAD (Files tab)
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}                → 200 OK; body: Job
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/criteria       → 200 OK; body: { ref, wrap_up, evaluators: [Evaluator + source], errors } — the criteria the job will be (or was) judged against, resolved at base_ref (or default HEAD before Ready)
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/release        → 200 OK; body: Job (updated state); 422 with error list if validation fails. Accepted from Frozen or Draft (a Draft is finalized in the same step, §2.1)
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/finalize       → 200 OK; body: Job (updated state); Member+; finalizes an edited Draft → Frozen (validates the definition like release, then parks it re-batchable, §2.1); 409 unless the job is Draft; 422 with error list if validation fails (job stays Draft)
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/draft          → 200 OK; body: Job (updated state); Member+; reopens a Frozen (never-released) job for editing → Draft; 409 unless the job is Frozen (§2.1). A batch reopened here un-absorbs its members (Batched→Frozen, job-unbatched) for editing
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/members        body: { add?: [seq], remove?: [seq] } → 200 OK; body: Job (updated batch); Member+; edits a DRAFT batch's member list while composing it (§2.1 draft batches); adds re-validated per-candidate (422); 409 unless the job is a Draft batch. Members are not absorbed here — absorption is deferred to finalize/release
PUT    /api/v1/projects/{owner}/{project}/jobs/{seq}/approval       body: { require: bool } → 200 OK; body: Job (updated); Member+ — exactly the privilege that resolves the resulting task, no new role; sets/clears the job's operator sign-off gate (§1.1 `require_approval`). Accepted only in the PRE-WORK states (Draft, Frozen, Blocked, Ready, Stalled): past Work entry the job's evaluation criteria are already resolved, so the edit could not take effect and answers 422 naming the state rather than succeeding as a no-op. Publishes `job-updated` with fields:["require_approval"]; a request that changes nothing writes nothing and publishes nothing
PUT    /api/v1/projects/{owner}/{project}/jobs/{seq}/groups         body: { add?: [name], remove?: [name] } → 200 OK; body: Job (updated); Member+; adds/removes the job's group labels (§1.1 `groups`). Accepted in EVERY state, terminal included — a group is an annotation, inert to execution, and is most often set after the job finished. Add/remove rather than a whole-list replace, so it is idempotent and two operators grouping one job from two tabs both succeed; removes apply first. The RESULTING list is re-checked against the §1.1 bounds (count, name shape, name length, duplicates) → 422 naming the rule; publishes `job-updated` with fields:["groups"]. A request that changes nothing writes nothing and publishes nothing
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/revoke         → 200 OK; body: Job (updated state); 409 if already Done or Revoked
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/claim          → 200 OK; body: Job; Member+; 409 while a work attempt is in flight or the job is terminal (§1.2 claims)
DELETE /api/v1/projects/{owner}/{project}/jobs/{seq}/claim          → 200 OK; body: Job; Member+; 409 if no pending claim (a materialized claim is resolved via its task)
POST   /api/v1/projects/{owner}/{project}/jobs/{seq}/triage         → 200 OK; body: Job (unchanged); dispatches an advisory triage agent (§1.2). 409 unless the job is Escalated or Stalled; 422 if TRIAGE_IMAGE is unconfigured. Member+. Never changes job state

# Platform fleet (the read-only snapshot route GET /api/v1/platform/fleet is specified in §3.1)
PUT    /api/v1/platform/fleet/{node}/capacity  body: { slots } → 202; platform admins only. Sets the node's desired slot count (§3.1); 404 unknown node, 409 for a docker-endpoint node, 422 above the node's reported maximum.

# Graph
GET    /api/v1/projects/{owner}/{project}/graph                     → 200 OK; body: Job[] (all jobs in the project)
POST   /api/v1/projects/{owner}/{project}/graph/validate            → 200 OK if valid; 422 with error list if any Frozen job fails validation
POST   /api/v1/projects/{owner}/{project}/graph/release             → 200 OK; body: Job[] (released jobs); 422 with error list if any job fails validation (nothing released)

# Event streams (SSE)
GET    /api/v1/projects/{owner}/{project}/events                    project-wide stream
GET    /api/v1/projects/{owner}/{project}/jobs/{seq}/events         per-job stream

# VCS
GET    /api/v1/projects/{owner}/{project}/diff/{seq}
       query params: since (optional byte cursor into the diff text, default 0)
       response: { "files": [{ "path": string, "additions": int, "deletions": int }], "offset": int,
                   "data": string (unified diff from `since` to `offset`), "done": bool,
                   "digest": string (sha-256 hex of the whole diff text), "diff": string (unified diff) }
       A diff has no size bound (job #342's was 1.3MB) and a NATS reply cannot exceed max_payload, so
       this endpoint is CURSOR-PAGED like `.../tasks/{id}/output`: `data` carries the diff from `since`
       on, capped per response against its JSON-ESCAPED length; the caller hands `offset` back as the
       next `since` until `done` and concatenates. Byte offsets are stable for one diff CONTENT, not
       for one job: the diff is regenerated from live refs on every page, so a Work/Evaluation job whose
       branch moves mid-read changes the diff under the cursor. `digest` identifies the diff each page
       was cut from and rides EVERY page; a caller must compare it against the first page's and, on a
       change, restart at since=0 rather than concatenate — otherwise a shrunken diff answers a stale
       cursor with an empty `done: true` page and the caller keeps an unmarked partial, which is the
       silent truncation this contract exists to prevent. The web client restarts a bounded number of
       times and then fails loudly (`api.diff`).
       `files` rides the FIRST page (since=0) only — it is what the UI renders before any hunk text
       arrives. `diff` is the legacy unpaged field, RETAINED for callers that do not page: it carries
       the whole diff only when one page held it (every diff under the cap, i.e. all but the outliers)
       and is empty otherwise, so an over-size diff never attempts an unpublishable single reply. A
       caller that stops early therefore sees `done: false` — a partial diff is always marked as one.
       behavior by job state (`data`/`diff` below are the whole diff when it fits one page):
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
| `job-created` | Dispatcher creates job in KV in response to `req.jobs.create.*`, or a schedule occurrence fires (§1.1 schedules); Frozen, or Draft with `draft: true`. Includes `inputs` (the **supplied** set) when the job carries any, and `schedule` when a schedule created it — each omitted entirely otherwise |
| `job-updated` | `PATCH .../jobs/{seq}` accepted (a Draft job's definition was edited), `POST .../jobs/{seq}/members` accepted (a Draft batch's membership was edited, §2.1 draft batches), `PUT .../jobs/{seq}/groups` accepted (the job's group labels changed, §1.1 — the one such event a **terminal** job can emit), or `PUT .../jobs/{seq}/approval` accepted (the job's sign-off gate changed, §1.1); includes `fields` (the changed field names — `["members"]` for a membership edit, `["groups"]` for a group edit, `["require_approval"]` for a gate edit — not the full payload) |
| `job-drafted` | Frozen → Draft; a never-released job was reopened for editing (`POST .../draft`) |
| `job-finalized` | The edited definition of a Draft was finalized — either Draft → Ready or Blocked as part of `POST .../release` (fires alongside `job-released`), or Draft → Frozen via `POST .../jobs/{seq}/finalize` (parked re-batchable, §2.1) |
| `job-batched` | Frozen → Batched; a job was absorbed into a batch (`POST .../jobs` with `members`, or a Draft batch finalized/released, §2.1 draft batches); includes `batch_id` (§2.1 batches) |
| `job-unbatched` | Batched → Frozen; the owning batch was revoked/failed or reopened to Draft, and the member was released (re-batchable); includes `batch_id` |
| `job-completed-via-batch` | Batched → Done; the owning batch's merge completed the member; includes `batch_id` (§2.1 batches) |
| `job-released` | `POST .../release` accepted; Frozen/Draft → Ready or Blocked; includes `state`, and `inputs` when the job carries any — the **effective** set for a job admitted Ready (the write that pins `base_ref` materialized its defaults), the supplied set for one parked Blocked (which resolved nothing yet) |
| `job-unblocked` | Blocked → Ready (last upstream dep reached Done), or a pre-Work escalation resolved with `Retry`; includes `inputs` (the **effective** set) when the job carries any. Read together with `job-created`, the two answer "what was asked for" and "what actually ran", and their difference is exactly the materialized defaults (§1.1) |
| `job-started` | Ready → Work; includes `cycle` |
| `job-evaluation-started` | Work → Evaluation; includes `cycle` |
| `job-rework-started` | Evaluation → Work with cycle++; includes new `cycle`, `reason` (`eval_failure` \| `merge_conflict` \| `merge_gate_failure`), and `eval_context` (populated for `eval_failure` and `merge_gate_failure`; empty for `merge_conflict`) |
| `job-merge-gate-started` | Eval reduce passed with moved default HEAD; merge-gate tasks created (see §3.3); includes `cycle` |
| `job-done` | Evaluation → Done |
| `job-escalated` | Any transition to Escalated (work retries exhausted, rework budget exhausted, launch validation failing on a rework re-entry, required eval infra failure, human work failed, command eval failure, deadline exceeded); includes `reason` field |
| `job-stalled` | Any transition to Stalled — every pre-Work park (§575): Blocked re-validation failure, a first-cycle launch validation failure, a deadline elapsed while Ready; includes `reason` field |
| `job-escalation-resolved` | Operator completes escalation Human task; includes `action` (`Retry`/`Resolve`/`Revoke`) |
| `job-revoked` | Any non-terminal → Revoked; includes cascaded job seqs if dependents were also revoked |
| `schedule-fired` | An occurrence of a schedule (§1.1 schedules) created a job; published on the **created** job; includes `schedule` and `occurrence_at` |
| `schedule-skipped` | An occurrence came due while a prior run of the same schedule was non-terminal; published on the **blocking** job; includes `schedule` and `occurrence_at`. At most one per occurrence — not one per scan tick |
| `task-created` | New task written to KV; includes `kind` (`Command`\|`Agent`\|`Human`), `phase` (`Work`\|`Evaluation`\|`MergeGate`), `cycle`, `attempt` |
| `task-started` | Task transitioned to Running |
| `task-completed` | Task reached Done; includes `pass` and `structured` where applicable; includes `token_usage` for agent tasks when reported |
| `task-failed` | Task reached Failed |
| `step-started` | Harness reported a step beginning (see §4.5); includes `task_id`, `step`, `kind` (`author`\|`inline-review`), `iteration` |
| `step-completed` | Harness reported a step finished; includes `status` (`done`\|`failed`) and, for inline-review steps, `pass` and `findings` |
| `channel-update` | Agent posted progress via `req.channel.update.*` (see §4.2); includes `message`, optional `percent`, and the originating-task fields below |
| `channel-reply` | Agent replied to the operator via `req.channel.reply.*` (see §4.2); includes `text`, `sent_at`, and the originating-task fields below |

**Channel-post origin fields.** `channel-update` and `channel-reply` events carry the identity of the task that produced them, so a consumer attributes a post to its task directly rather than by correlating timestamps against task windows (which is ambiguous the moment two tasks overlap). The channel binary stamps these from its container env (`CHUG_TASK_ID` / `CHUG_PHASE` / `CHUG_EVALUATOR`, set by the dispatcher when composing the agent container) onto every post; the dispatcher preserves them on the event:

- `task_id`: `u64` — the originating task's id.
- `phase`: optional string — `Work` or `Evaluation`.
- `evaluator`: optional string — the evaluator's name, present only for evaluator posts.

All three are **optional for back-compat**: a post from an older container (or a command binary that runs no channel MCP) carries none, and the event omits the fields entirely — old consumers render it exactly as before.

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

### 6.6 Health

```
GET /api/v1/health   → 200 { "dispatcher": "ok", "version": string }   (dispatcher answered)
                     → 503 { "dispatcher": "error", "error": string }  (no responder / wedged)
```

A liveness probe that proves the **dispatcher**, not just the api process. The api issues a bounded (`~3s`) `req.health` NATS request (§6.1) that round-trips the dispatcher's single-writer core actor; a genuine `{"dispatcher":"ok","version"}` reply returns `200`, anything else — no responder (a crash-looping dispatcher), a timeout (a wedged actor), or an unexpected body — returns `503`. Because it round-trips the actor, a dispatcher whose HTTP-answering api is up while its state loop is dead reads as **unhealthy**, which is the case the api-only probe missed on the 2026-07-22 outage.

**Unauthenticated by design.** The endpoint is exempt from §7.1 auth: the body leaks only liveness and the build version, never any project data. This is what lets an outside client (and the `deploy` job's `.chug/tasks/deploy-health.sh` gate) confirm a release came up. The gate requires all of a `200`, an `application/json` content-type, and the health JSON — a `text/html` body is an automatic fail, so the SPA fallback (which answers `200 index.html` for any unknown route) can never masquerade as health. If project-liveness detail is ever added, gate the endpoint and update the deploy gate to authenticate.

---

### 6.7 OIDC issuer documents

```
GET /.well-known/openid-configuration → 200 { "issuer", "jwks_uri", "response_types_supported", "subject_types_supported", "id_token_signing_alg_values_supported" }
GET /.well-known/jwks.json            → 200 { "keys": [ { "kty": "RSA", "alg": "RS256", "use": "sig", "kid", "n", "e" } ] }
                                      → 404 on a platform with no oidc_public.pem mounted (§12.1)
```

The workload-identity issuer's two public documents (design #313 A4), served over the public half of the §12.1 OIDC issuer keypair. The JWK set is RFC 7517 and holds exactly one key; its `kid` is the RFC 7638 thumbprint `auth::oidc::kid_from_public_pem` derives, the same value a minted workload token carries in its header — one derivation, called from both, because a `kid` that disagrees with the published set fails at a cloud STS wearing an error that names the issuer.

**Issuer.** The `issuer` member and every minted token's `iss` come from one setting, `OIDC_ISSUER` (default `https://chug.kasofsk.xyz`), resolved by `auth::oidc::issuer_from_env` in the api and the dispatcher alike. It must be an absolute `https` identifier with no trailing slash — a cloud STS compares it byte-for-byte — and a process that reads a malformed one refuses to start. `jwks_uri` is always the issuer plus `/.well-known/jwks.json`, so the two documents cannot name different keys.

**Unauthenticated by design, and unexposed.** Both are exempt from §7.1 auth: they are public, integrity-only documents that carry no project data and no private key material. The exemption is narrow and deliberate — they are the only routes built by `api::oidc::public_routes`, apart from the authenticated surface. **Serving them is not exposing them.** The api binds loopback and nothing routes these paths to the public internet; a provider is registered with an uploaded JWK set instead (#313 D1). Making them reachable is an infrastructure decision (#313 A4 prices three options), not a code change.

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

**User SSH certs** — `POST /auth/ssh-cert`: user submits `{ "public_key": string }` (their SSH public key, authenticated via JWT cookie); API forwards to dispatcher via `req.ssh.sign-user-cert`; dispatcher signs with the CA private key and returns `{ "certificate": string }` valid for 24 hours, with a principal equal to the user's email. The user's `project_roles` and `platform_admin` flag (read from the user record, never a client-supplied value) ride in the cert's forced command so the SSH front and pre-receive hook can authorize per §5.2. Users interact with git via the `chuggernaut` CLI, which handles cert refresh transparently.

**Project-role management** — a user's `project_roles` are granted/revoked out-of-band and take effect on the next cert mint (24h cert lifetime bounds staleness):
- **CLI**: `chuggernaut admin user role set --email E --project owner/name --role owner|member|viewer`, plus `role list --email E` and `role remove --email E --project owner/name`. `owner` is the operator-facing alias for the top project role (`admin`).
- **API** (`platform_admin` only): `PUT /api/v1/projects/{owner}/{project}/members/{email}` with `{ "role": "owner"|"member"|"viewer" }`, `DELETE` to remove, and `GET .../members` to list. The API forwards to the dispatcher (`req.members.{set,remove,list}`), which is the single writer of the `users.*` bucket.

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
| Manage project roles (grant/revoke members) | `platform_admin` |
| Platform-level config | `platform_admin` |
| Push to default branch | Dispatcher identity only (never a user, even `platform_admin`) |
| Push to any job branch | Member+ on that project, or `platform_admin` (any project) |
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

`list` returns both names and values — vars are not sensitive. At job launch the dispatcher reads every var declared in the job type's `vars:` list and injects them as env vars into both work and eval containers. If any declared var is missing at launch, the job parks: `Stalled` on a first launch (a pre-Work park, §575) and `Escalated` on a rework re-entry.

The `CHUG_` name prefix is **reserved** for vars exactly as it is for secrets (§5.3): a `CHUG_`-prefixed name in `vars:` is a release-validation error, and injection skips it so a stored one cannot clobber a task-origin stamp (§4.1, §6.3).

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

`list` returns names only — values are never returned to callers outside the dispatcher. The dispatcher decrypts values at job launch and injects them as env vars. Containers never see the age key or the KV bucket. If any declared secret is missing at launch, the job parks: `Stalled` on a first launch (a pre-Work park, §575) and `Escalated` on a rework re-entry.

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

### 9.4 Documentation Jobs and the Docs Tree

A project's markdown documentation is a first-class output produced by agent jobs, gated through the normal merge path like any code change. Two job types produce documentation, split because their review criteria differ:

- **`design`** — architecture/plan documents that argue a decision's tradeoffs and set direction, written to `docs/design/{seq}-{slug}.md` — the leading `{seq}` is the filing job's number, and the shape is enforced by the stage-1 lint below because the Designs view joins a design to its jobs on that filename. Typical flow: a `design` job lands the document, then `code`/`web` jobs depend on it and cite it. The reviewer judges whether the document addresses the brief, weighs its alternatives and tradeoffs honestly, and stays consistent with `spec.md` and the codebase as they exist.
- **`docs`** — reference/wiki pages that teach, written anywhere under `docs/`. The reviewer judges accuracy against the current code (spot-checking claims against the source), placement/navigation, and audience fit.

**The docs tree is the wiki.** The repo's `docs/` directory is the project wiki root; `docs/design/` holds design documents. Documentation is versioned with the code it describes and travels with the project repo, exactly like job types and prompts (§1.1) rather than living in a separate control plane. Knowledge tags (§4.4, `.chug/tags/{tag}.md`) unify into this tree over time — a tag becomes a `docs/` page marked injectable via front-matter — but that migration is deferred; the two stores coexist today.

**Gating.** Both types stage their evaluators (§3.3): stage 0 is the agent reviewer described above; stage 1 is a shared documentation lint (`.chug/tasks/doc-lint.sh` — markdown well-formedness, intra-repo link resolution, best-effort code-path existence, and the `docs/design/{seq}-{slug}.md` filename shape) that runs alongside the appended project `ci` default (§1.1). Both stage-1 checks are diff-aware and self-skip a diff with no relevant files, so a doc-only change is gated in seconds — `ci` skips its build because no `crates/**` path changed, and `doc-lint` runs only over the changed `.md` files.

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
   - OIDC issuer RS256 keypair — `oidc_private.pem`, `oidc_public.pem` (workload tokens; separate from the session key so a session JWT and a workload token can never be confused)
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

Platform-level agent provider and model defaults are supplied as dispatcher configuration at startup — not stored in NATS KV.

**Model resolution chain** (most specific wins): per-job override (`Job.model`, §1.1) → job-type declaration (`work.model` / evaluator `model:`) → project default (`.chug/jobs/_defaults.yaml` `model:`, §1.1) → platform config default (`AGENT_MODEL_DEFAULT`). The per-job override applies to the **Work agent only** (evaluators keep the type/project/platform resolution, exactly as `Job.timeout` scopes to Work); every other layer applies to the work agent and agent evaluators alike. **Provider** resolution is unchanged: job-type declaration → platform config default (per-project provider defaults and per-job provider overrides remain deferred).

Dispatcher configuration (environment variables or config file):

```
AGENT_PROVIDER_DEFAULT   claude | codex      Required. No built-in default — dispatcher refuses to start without this.
AGENT_MODEL_DEFAULT      string              Optional. Bottom of the model resolution chain. If unset, the provider's built-in default model is used.
TRIAGE_IMAGE             string              Optional. Platform image for operator-dispatched triage agents (§1.2). Provider/model reuse AGENT_PROVIDER_DEFAULT / AGENT_MODEL_DEFAULT. Unset → the triage action is unavailable (422). A platform-level image (rather than the failing job's own type image) so triage works uniformly across agent/command/human job types.
PLACEMENT_POLICY         busyness | headroom  Optional. Fleet placement policy (§3.1), platform-wide (not per job type). busyness = fewest running jobs (ties: most free slots, then name); headroom = most free slots (ties: name). Unset → busyness. An unknown value is a hard startup config error.
```

If a job type declares `provider` and/or `model` at the `work:` level or per evaluator, those override the project/platform defaults for that job or evaluator. If neither the job type, the project default, nor the platform config specifies a provider, the dispatcher fails to start with a configuration error. Triage runs at the platform defaults (§1.2) — it is not tied to a job type and does not consult the project or per-job model.

### 12.5 Job Wizard Credential *(retired)*

The New Job screen's optional "job wizard" — a short chatbot conversation that shaped a rough goal into a ticket, calling the Anthropic Messages API from the dispatcher — has been **retired.** Its role is served by the collaborative draft editor. The `/wizard` route, the dispatcher `wizard` module, the `req.wizard.chat.*` subject, and the `wizard_available` config-snapshot field have been removed. `WIZARD_API_KEY` / `ANTHROPIC_API_KEY` / `WIZARD_MODEL` / `WIZARD_BASE_URL` no longer have any effect.

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

## Part 14: Config & Version Skew

Job-type config is read **live** from a project's default branch (a per-consumer
forge, repo-versioned by design — CLAUDE.md). Auto-deploy (#108) also gives
every rollout a mixed-version window: all services build from **one SHA** per
deploy, but `update.sh` refreshes workers and restarts the dispatcher in leg
order, not atomically. Both facts mean the platform must tolerate the running
binary and the config/peer services being **one deploy generation** out of step.

On 2026-07-22 they were not: job #63 merged a `wrap_up` section into
`.chug/jobs/web.yaml`; the running dispatcher's strict parser rejected the unknown key
and escalated every `web` job at launch (`launch_validation_failed`, first
victim #69) until an operator deployed. Merging config had silently become
deploying config. This part defines the rules that make that a *detected,
explained, non-destructive* condition.

### 14.1 The N±1 wire contract

Every cross-service wire surface — the worker RPC (`crates/store/src/worker.rs`
ops), the channel protocol, and the job-type config schema — must tolerate one
generation of skew in **either** direction:

- **Additive-only.** New fields/ops are optional; the old side ignores what it
  does not recognize.
- **Graceful degradation, never a crash or escalation storm.** An unknown worker
  op logs and falls back; an unknown config field is ignored with a warning. The
  2026-07-22 `logs_tail` (unknown worker op) and `wrap_up` (unknown config
  field) incidents are the counterexamples.
- **Declared versions.** `types::version` holds one monotonic integer per
  surface (`CONFIG_SCHEMA_EPOCH`, `WORKER_RPC_VERSION`, `CHANNEL_PROTOCOL_VERSION`).
  A change that genuinely **cannot** satisfy N-1 compat bumps the relevant
  constant *in the same commit* as the breaking code, and must fail its own CI
  (§14.3) rather than merge a coordinated-deploy requirement silently.

### 14.2 Schema tolerance (config ahead of binary)

Job-type YAML validation distinguishes two laxity classes:

- **Unknown *top-level* fields are tolerated** — the config is accepted, the
  field is ignored, and a warning names the file and field. Rationale: an ignored
  top-level section (a future `wrap_up`, `deploy`, …) means *a feature is quietly
  off* until the dispatcher is deployed — acceptable when flagged. `JobType`
  therefore drops `deny_unknown_fields`, capturing unknowns into
  `JobType::unknown` and surfacing them via `config_warnings()`. The warning is
  both logged (`tracing::warn!` in `load_job_type`) and published once per
  affected job, at first launch, as a `config-warning` event on the job event
  stream — so it shows in the UI's per-job feed without grepping dispatcher logs.
  A dedicated platform-level health/settings surface (all active config warnings
  in one place) is a follow-up, not yet wired.
- **Unknown fields inside gate-relevant blocks stay hard errors.** An unknown
  key inside an `Evaluator` (a typo'd `required`, a mis-nested check) could
  silently **skip a merge gate** — "config ahead of binary" becoming "a gate
  quietly disabled". Every nested block (`work`, `eval`, `wrap_up`, `resources`,
  …) keeps `deny_unknown_fields`; such a config is refused outright at parse.

A job type may also declare `min_dispatcher: <epoch>`. When it exceeds the
running dispatcher's `CONFIG_SCHEMA_EPOCH`, the config is ahead of the binary:
`load_job_type` refuses it with a diagnostic naming the file, field, and needed
version. At launch this parks the job **pre-Work (Stalled)** with that reason —
Retry/Revoke only, one park, no per-launch escalation storm. Every other
first-cycle launch failure parks the same way under `launch_validation_failed`
(§575); only a rework re-entry, past `Work` already, escalates.

Some schema features require that declaration rather than leaving it to the
author: a non-empty `inputs:` (§1.1) is a field rule error unless
`min_dispatcher` is at least the epoch inputs landed in — on a job type, the
epoch job inputs landed in; on a **schedule file**, the later epoch a schedule's
`inputs:` landed in, because a dispatcher that understands the first still drops
the second. A `runtime:` block beyond a bare `mode: container` (§1.1) is gated
the same way and for the same reason: the block is invisible to an N-1
dispatcher, which keeps the still-present `image` and runs the job containerized
against the image's toolchain instead of as declared. The rule exists because `min_dispatcher` is the one field an
N-1 dispatcher **does** parse — it cannot see `inputs:` or `runtime:` at all, so
without the declaration it would accept the config and run the job
unparameterized. Each feature freezes its own constant at the epoch it shipped
(`types::version::INPUTS_SCHEMA_EPOCH`, `SCHEDULE_INPUTS_SCHEMA_EPOCH`,
`RUNTIME_SCHEMA_EPOCH`), so a later bump for an unrelated feature never
retroactively raises what an existing config must declare — and those constants,
not `CONFIG_SCHEMA_EPOCH`, are where a reader finds which epoch bought what.

### 14.3 Merge-time gate

The dispatcher publishes its `CONFIG_SCHEMA_EPOCH` in the config snapshot
(`GET /api/v1/platform/config` → `dispatcher.schema_epoch`). `.chug/tasks/ci.sh`'s
config-skew gate — and `chuggernaut validate --deployed-epoch <N>` — compare a
config's declared `min_dispatcher` against the **deployed** dispatcher's epoch
and **fail the config's own CI** ("requires dispatcher >= X; deploy first or gate
it") before it can merge. The gate is pure shell so a config-only change stays
fast, is best-effort about reaching the API (falling back to the checkout's own
epoch), and never blocks on an unreachable dispatcher. This is the first line of
defense; the runtime park (§14.2) is the fallback if a skewed config reaches a
launch anyway.

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

Platform init generates: JWT RS256 keypair, OIDC issuer RS256 keypair, SSH CA keypair, age keypair, VAPID keypair. All private keys are mounted into services at runtime via the deployment's secret mechanism (k8s Secrets in k8s deployments, bind-mounted files in Docker deployments) — never stored in NATS KV. The JWT public key is also mounted into the API layer for token verification, and the OIDC issuer public key for JWKS publication; all other private keys are dispatcher-only. See §12.1 for the full bootstrap procedure.

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
- **Project/team-level provider defaults**: a project-level default **model** (`.chug/jobs/_defaults.yaml` `model:`) and a per-job model override (`Job.model`) are supported (§12.4). Per-project/per-team **provider** defaults and all team-level defaults remain deferred.
- **Per-project / cross-node dependency caching**: per-project caches (node_modules, a pull-through registry cache) and cache sharing *across* nodes. The node-local build cache (§3.1, "Node-local build caching") ships now and covers the cargo-compile case within a node; richer per-project and cross-node caching remains deferred. v1 mitigation for the rest: bake toolchains and dependencies into the declared `image`.
- **k8s-Secret-based secret injection**: dispatcher writes a Kubernetes Secret referenced by the Job spec instead of decrypting secrets into env vars it assembles itself, keeping plaintext out of the dispatcher's launch path. v1 injects env vars directly.
- **Direct-mode factories**: event → job templating without a triage agent (payload→job-fields mapping mini-language). v1 factories are triage-only (§13.1); a trivial triage prompt covers the direct case at the cost of agent tokens.
