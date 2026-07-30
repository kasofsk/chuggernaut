/**
 * GENERATED — DO NOT EDIT.
 *
 * The §6.2 HTTP surface as TypeScript, generated from .chug/schemas/api.schema.json
 * (itself generated from the Rust `types` crate by `chuggernaut schema api`).
 * Regenerate with `npm run codegen`; `npm run codegen:check` fails CI when
 * this file is stale.
 *
 * The shapes that stay hand-written are in ./envelopes.ts: the replies the
 * dispatcher assembles with `serde_json::json!`, for which no Rust type — and
 * so no schema — exists. ../api.ts re-exports both halves.
 */

/**
 * Where the slot count the scheduler is using came from (design #293 §7/§8).
 * Reported per node on the fleet roster and `fleet.status` so a node running
 * on the boot seed is *visible* rather than indistinguishable from a healthy
 * one — the representation whose absence hid the 2026-07-26 incident for
 * weeks.
 */
export type CapacitySource = "node" | "seed";
/**
 * How far a node's observed capacity is from the operator's intent (design #293
 * §4/§10). Derived on read from intent + observation + the push ledger; never
 * stored on the intent record, which holds only what the operator asked for.
 */
export type CapacityState =
  "converged" | "pending" | "rejected" | "unacknowledged";
/**
 * Status of one deploy leg. An unknown status fails to deserialize, so a
 * malformed leg line is dropped by the harvest rather than corrupting the
 * report.
 */
export type LegStatus = "ok" | "failed" | "skipped";
export type JobState =
  | (
      | "Frozen"
      | "Blocked"
      | "Ready"
      | "Work"
      | "Evaluation"
      | "Done"
      | "Revoked"
    )
  | "Draft"
  | "Batched"
  | "WrapUp"
  | "Escalated"
  | "Stalled";
/**
 * How a worker's most recent self-refresh ended (spec §3.1, ticket #187). The
 * daemon records this when a refresh completes and reports it in `ping` so a
 * FAILED refresh becomes durable, queryable platform state (the fleet snapshot)
 * instead of a node-local `tracing::error` that only the node's own logs hold.
 * A successful refresh swaps the daemon away, so in practice this surfaces
 * failures — the swapped-in daemon reports the new `version` instead.
 */
export type RefreshResult =
  | {
      result: "in_progress";
    }
  | {
      result: "ok";
    }
  | {
      /**
       * A short tail of the failure detail for the operator.
       */
      error_tail: string;
      result: "failed";
      /**
       * Which stage failed: `build`, `drain`, or `swap`.
       */
      stage: string;
    };
export type EscalationAction = "Retry" | "Resolve" | "Revoke";
export type Provider = "claude" | "codex";
export type EvaluatorType = "command" | "agent" | "human";
export type IdentityKind = "user" | "dispatcher";
export type ProjectRole = "viewer" | "member" | "admin";
/**
 * What kind of value an [`Input`] accepts (spec §1.1, design #311 Decision 2).
 */
export type InputKind = "string" | "enum";
export type WorkType = "agent" | "command" | "human";
/**
 * Wrap-up mode after eval-pass (design-lifecycle.md).
 */
export type WrapUpMode = "merge" | "none";
/**
 * Why a task sits `Pending` when the reason is worth showing an operator
 * (spec §3.5). Kept a distinct enum rather than a bool so later parked-reasons
 * (e.g. awaiting a claim) can join without another field.
 */
export type PendingReason = "QueuedForCapacity";
/**
 * The actual performer of a claimed attempt (spec §1.2). Only `Human` exists:
 * normal execution is implied by absence, so the field never restates `kind`.
 */
export type Performer = "human";
export type ReleaseStatus = "open" | "merged" | "closed";
/**
 * Cause of a rework-created Work task (spec §3.3). Mirrors the
 * `job-rework-started` event's `reason`, but persisted on the task record so
 * the tasks list explains itself — a Work task after passed evaluations is no
 * longer a mystery — without event-stream archaeology.
 */
export type ReworkReason =
  "EvalFailure" | "MergeConflict" | "GateCiFailure" | "GateCompileFix";
export type TaskKind =
  | {
      kind: "Command";
      run: string;
    }
  | {
      kind: "Agent";
      model: string | null;
      prompt: string;
      provider: string;
    }
  | {
      kind: "Human";
      prompt: string;
    };
export type TaskPhase =
  ("Work" | "Evaluation") | "MergeGate" | "WrapUp" | "Triage" | "Escalation";
export type TaskResult =
  | {
      /**
       * Optional agent-authored HTML cover page for this result (spec §1.1,
       * §4.3, job #143). Purely presentational — a visual changelog/before-
       * after the operator UI renders in a sandboxed frame beside the text
       * `summary`. Sanitized (size-capped) at ingest; **never** enters the
       * squash body or any downstream prompt, and its absence is never
       * penalized. Defaults None so pre-#143 records still deserialize.
       */
      cover_html?: string | null;
      kind: "Work";
      structured: unknown;
      summary: string | null;
      token_usage: TokenUsage | null;
    }
  | {
      exit_code: number;
      kind: "Command";
      output: string;
      pass: boolean;
      structured: unknown;
    }
  | {
      /**
       * Eval verdict "not satisfiable by rework" (design-lifecycle.md):
       * implies `pass: false`; a required evaluator's abort skips the
       * remaining rework budget and escalates.
       */
      abort: boolean;
      /**
       * Optional agent-authored HTML cover page for an evaluator's verdict
       * summary (job #143), same semantics as [`TaskResult::Work::cover_html`].
       */
      cover_html?: string | null;
      kind: "Agent";
      pass: boolean;
      structured: unknown;
      token_usage: TokenUsage | null;
    }
  | {
      /**
       * Same abort semantics as `Agent`; set via `TaskResolution::Fail`.
       */
      abort: boolean;
      action: EscalationAction | null;
      kind: "Human";
      operator: string;
      pass: boolean;
      resolved_at: string;
      structured: unknown;
      /**
       * Work-task Pass only: the operator's completion summary, carried from
       * `TaskResolution::Pass::summary` so the Reports thread renders
       * human-completed work like an agent's closing summary. Defaults None
       * so pre-summary records still deserialize.
       */
      summary?: string | null;
    }
  | {
      assessment: string;
      kind: "Triage";
      token_usage: TokenUsage | null;
    };
export type TaskState = "Pending" | "Running" | "Done" | "Failed";
/**
 * Operator submission for Human tasks (spec §1.2). Adjacent tagging: `kind`
 * discriminates. `structured` is required (non-null) on `Fail`.
 */
export type TaskResolution =
  | {
      kind: "Pass";
      structured?: unknown;
      /**
       * Work-task Pass only: the human's completion summary, flowing into
       * the squash-merge commit body exactly like an agent's
       * `submit_result` summary (spec §1.2 claims). Ignored on evaluator
       * and escalation resolutions.
       */
      summary?: string | null;
    }
  | {
      /**
       * Human evaluator's "not satisfiable by rework" (design-lifecycle.md);
       * only meaningful on evaluator tasks, ignored elsewhere.
       */
      abort?: boolean;
      kind: "Fail";
      structured: unknown;
    }
  | {
      action: EscalationAction;
      kind: "Escalation";
      structured?: unknown;
    };

/**
 * The originating task's identity, carried end to end on every channel post so
 * the UI attributes a post to a task directly rather than by timestamp
 * correlation (spec §6.3 events). Every field is optional for back-compat:
 * legacy events carry none and render as before.
 */
export interface ChannelOrigin {
  /**
   * The evaluator's name when the post came from an evaluator task.
   */
  evaluator?: string | null;
  phase?: string | null;
  task_id?: number | null;
}
/**
 * The originating task's identity, carried end to end on every channel post so
 * the UI attributes a post to a task directly rather than by timestamp
 * correlation (spec §6.3 events). Every field is optional for back-compat:
 * legacy events carry none and render as before.
 */
export interface ChannelUpdate {
  /**
   * When the dispatcher accepted the post. Stamped on write, not by the
   * container — a container's clock is not ours to trust, and this is what
   * the operator UI ages the message against ("2m ago") when it reads the
   * latest post off the jobs list instead of the event stream. None on posts
   * written before this field existed; the bucket's 7-day TTL ages those out.
   */
  at?: string | null;
  /**
   * The evaluator's name when the post came from an evaluator task.
   */
  evaluator?: string | null;
  message: string;
  percent: number | null;
  phase?: string | null;
  task_id?: number | null;
}
/**
 * One deploy leg's typed record (ticket #187). The legs are the fixed steps of
 * `update.sh`: `build-dispatcher`, `build-images`, `web-publish`,
 * `worker-refresh:{node}` (one per node), `init`, `ssh-front`,
 * `restart-verify`, `sha-advance`.
 */
export interface DeployLeg {
  /**
   * A bounded tail of the underlying failure output (e.g. the worker
   * `worker-refresh.sh` stderr tail), present only on [`LegStatus::Failed`]
   * when the leg could capture it. `error` stays the one-line summary;
   * `detail` carries the real text so the structured result and the
   * escalation prompt show what actually broke (deploy #212). Bounded by the
   * emitter (`update.sh` caps it) so a huge build log cannot bloat the record.
   */
  detail?: string | null;
  /**
   * A short failure reason, present only on [`LegStatus::Failed`].
   */
  error?: string | null;
  /**
   * Leg name — one of the fixed step names above.
   */
  name: string;
  /**
   * Wall-clock seconds the leg took, when measured. Absent for a skipped leg.
   */
  secs?: number | null;
  status: LegStatus;
}
/**
 * The structured result the dispatcher builds from a command work task's
 * harvested `@chug:leg`/`@chug:report` lines (ticket #187). Generic to command
 * work — any job type could emit legs — but a deploy is the consumer that
 * matters: the envelope (`from_sha`/`to_sha`/`rollback`/`health`) frames the
 * leg list. Every field defaults, so a report built from legs alone round-trips
 * and a record written before a given field existed still deserializes.
 */
export interface DeployReport {
  /**
   * SHA the deploy started from (the previously-deployed SHA), if reported.
   */
  from_sha?: string | null;
  /**
   * Post-restart health verdict, if reported (e.g. `"ok"`).
   */
  health?: string | null;
  /**
   * The legs, in emission order.
   */
  legs: DeployLeg[];
  /**
   * Whether the deploy rolled back to the previous binary — restart-verify's
   * health check failed and prod was restored to `from_sha`.
   */
  rollback: boolean;
  /**
   * SHA the deploy targeted, if reported.
   */
  to_sha?: string | null;
}
/**
 * A row of `GET .../designs`: a document under `docs/design/`, joined to the
 * group its jobs carry. Repo-derived, so a design with **no** jobs is a row —
 * which is exactly the row `GET .../groups` cannot represent, and the one the
 * operator most needs to see.
 */
export interface DesignEntry {
  /**
   * Per-state histogram keyed by the state name serde writes, zero states
   * omitted. Not a percentage: "5 Done, 1 Frozen" is the operator's actual
   * question, and a percentage discards *which* one is not done.
   */
  counts: {
    [k: string]: number;
  };
  /**
   * The members, in ascending job seq.
   */
  jobs: GroupJob[];
  /**
   * The label the members carry, verbatim.
   */
  name: string;
  /**
   * Members that are not terminal, via [`JobState::is_terminal`] — the same
   * definition batches and the roll-up's staleness flag already use.
   */
  open: number;
  /**
   * Repo-relative path at default-branch HEAD, e.g.
   * `docs/design/321-job-groups.md`.
   */
  path: string;
  /**
   * The leading `<seq>-` of the slug when the document follows the naming
   * convention, `None` when it does not. A convention, not a rule: the path
   * is the identity (design #321 Decision 2, correction 4).
   */
  seq?: number | null;
  /**
   * The basename without `.md` — the stem a `design/` group name embeds.
   */
  slug: string;
  /**
   * The document's status line, verbatim and unparsed. Absent when the
   * document has none — six of the eight in the tree read `PROPOSED`, one
   * `DRAFT`, one `FINDING`, under no schema and no enforcement.
   */
  status?: string | null;
  /**
   * The design has members, **every** member is terminal, and the status
   * line is non-empty — so the text beside this flag may no longer describe
   * the design. Reported, never acted on: the repo stays the source of truth
   * for a design's status and the operator resolves a discrepancy with an
   * ordinary `design` amendment job (design #321 Decision 8). Deliberately
   * not a machine-checked `implemented` — that needs the front-matter
   * vocabulary, which is #86's to define.
   */
  status_stale: boolean;
  /**
   * The `# …` heading, falling back to the slug.
   */
  title: string;
}
/**
 * One member of a group, as the group views render it: a state badge and a
 * title, with the job page one click away. The same four fields the jobs list
 * projection leads with — deliberately not the whole record, which the roll-up
 * has no use for.
 */
export interface GroupJob {
  id: number;
  state: JobState;
  title: string;
  type: string;
}
/**
 * A snapshot of the dispatcher's runtime configuration for display. Contains
 * no secrets — only names, endpoints, and resolved paths an operator needs to
 * see. Written by the dispatcher at startup, read by the api.
 */
export interface DispatcherConfigSnapshot {
  /**
   * `AGENT_MODEL_DEFAULT`, if set.
   */
  agent_model_default: string | null;
  /**
   * `AGENT_PROVIDER_DEFAULT` (`claude` | `codex`).
   */
  agent_provider_default: string;
  /**
   * `CHANNEL_BINARY` path, if the channel MCP is wired.
   */
  channel_binary: string | null;
  /**
   * How many commits `dispatcher_sha` is behind `main_tip_sha`
   * (`rev-list --count`, cached per tip). `Some(0)` = in sync; `None` when
   * drift can't be computed (no self repo, or the deployed SHA is absent
   * from its history).
   */
  commits_behind: number | null;
  /**
   * The running dispatcher binary's own build SHA (`CHUG_GIT_SHA`, baked at
   * build time — the SHA that `version_string()` embeds). `None` for local/
   * dev builds with no SHA baked in. Compared against `main_tip_sha` to show
   * whether prod is in sync. Defaults to `None` for snapshots written before
   * this field existed.
   */
  dispatcher_sha: string | null;
  /**
   * `HOOK_BIN` — pre-receive hook binary path as seen from the SSH front.
   */
  hook_bin: string | null;
  /**
   * Current `main` tip SHA of the platform's own source repo (`SELF_REPO`),
   * re-resolved each scan tick. `None` when `SELF_REPO` is unset or the tip
   * can't be resolved. This is the deploy target the running dispatcher is
   * measured against.
   */
  main_tip_sha: string | null;
  /**
   * The dispatcher's own `NATS_URL`.
   */
  nats_url: string;
  /**
   * `NATS_URL_CONTAINER` — the URL injected into agent containers, if it
   * differs from the dispatcher's own.
   */
  nats_url_container: string | null;
  /**
   * The Docker fleet (`DOCKER_NODES` / `DOCKER_SLOTS`).
   */
  nodes: WorkerNode[];
  /**
   * `PLACEMENT_POLICY` — the active fleet placement policy (`busyness` |
   * `headroom`, §3.1), so the UI can show how the fleet schedules. Defaults
   * to `busyness` for snapshots written before this field existed.
   */
  placement_policy: string;
  /**
   * `REPO_URL_BASE` — clone URL base injected into containers.
   */
  repo_url_base: string;
  /**
   * `REPOS_ROOT` — bare repos on disk.
   */
  repos_root: string;
  /**
   * The job-type config schema epoch this dispatcher understands
   * ([`crate::CONFIG_SCHEMA_EPOCH`], spec §14). Exposed so the merge-time CI
   * check can compare a config's `min_dispatcher` against the *deployed*
   * dispatcher and fail a config that would otherwise merge ahead of the
   * binary. Defaults to `1` (the epoch before this field existed) for older
   * snapshots.
   */
  schema_epoch: number;
  /**
   * Whether the dispatcher loaded the age identity — i.e. secrets are
   * encrypted at rest rather than injected raw (§8.2 dev mode).
   */
  secrets_encryption: boolean;
  /**
   * `TRIAGE_IMAGE` — platform image for operator-dispatched triage agents
   * (§1.2). None → the triage action is unavailable.
   */
  triage_image: string | null;
}
/**
 * One Docker fleet node the dispatcher schedules onto.
 */
export interface WorkerNode {
  /**
   * Node health at snapshot time (spec §3.1): `false` when the node was
   * unreachable and marked out-of-service — placement skips it until it
   * answers again. Defaults to `true` for snapshots written before this
   * field existed.
   */
  available: boolean;
  /**
   * When the node last reported its capacity; `None` when it never has.
   * Together with [`Self::capacity_source`] this is the representation whose
   * absence let a fleet run for weeks on a boot seed nothing had confirmed.
   */
  capacity_observed_at?: string | null;
  /**
   * Where [`Self::slots`] came from (design #293 §7): the node's own report
   * over either transport, or the `DOCKER_NODES` boot seed. `None` for a
   * docker-endpoint node, whose capacity `DOCKER_NODES` still owns outright.
   */
  capacity_source?: CapacitySource | null;
  /**
   * `unix:///var/run/docker.sock` or `tcp://host:2375`.
   */
  endpoint: string;
  name: string;
  /**
   * The node's last self-refresh outcome (ticket #187), last reported by a
   * worker's ping. `None` for docker-endpoint nodes and workers that have
   * not refreshed. A failed refresh is durable platform state here rather
   * than a node-local `tracing::error`. Defaults to `None` for snapshots
   * written before this field existed.
   */
  refresh_outcome?: RefreshOutcome | null;
  /**
   * Max concurrent chuggernaut containers on this node.
   */
  slots: number;
  /**
   * Build version last reported by a worker node's ping (spec §3.1):
   * `chuggernaut` version + git SHA. `None` for docker-endpoint nodes and
   * for workers that have not answered yet. Lets the UI show fleet versions
   * and spot deploy drift after a worker self-refresh.
   */
  version: string | null;
}
/**
 * The last refresh outcome a worker daemon reports (spec §3.1, ticket #187).
 */
export interface RefreshOutcome {
  /**
   * When the daemon accepted the refresh request.
   */
  accepted_at: string;
  /**
   * When it reached a terminal verdict; `None` while `InProgress`.
   */
  finished_at?: string | null;
  /**
   * Version the node was refreshing away from.
   */
  from_sha: string;
  result: RefreshResult;
  /**
   * Target SHA of the refresh.
   */
  to_sha: string;
}
/**
 * Why the dispatcher escalated (→Escalated) or stalled (→Stalled) a job,
 * carried on the job record so operators diagnose from what the API serves
 * rather than from dispatcher logs (spec §1.2, §3.4).
 */
export interface Escalation {
  /**
   * When the escalation was recorded.
   */
  at: string;
  /**
   * Human-readable explanation — the same text shown in the operator's
   * intervention task prompt.
   */
  detail: string;
  /**
   * The task whose failure triggered the escalation, when one exists. None
   * for pre-work escalations that fail before any task runs (launch
   * validation, a deadline that elapsed while still Ready) and for
   * evaluation-phase escalations with no single culprit task. Defaulted so
   * records without it deserialize.
   */
  failing_task?: number | null;
  /**
   * Machine reason code, matching the `job-escalated`/`job-stalled` event
   * reason (e.g. `launch_validation_failed`, `work_retries_exhausted`,
   * `eval_abort`, `job_deadline_exceeded`).
   */
  reason: string;
}
export interface Evaluator {
  /**
   * command/agent: optional, falls back to top-level image; one of the two required.
   */
  image: string | null;
  model: string | null;
  name: string;
  prompt: string | null;
  provider: Provider | null;
  /**
   * Default true; false = advisory.
   */
  required: boolean | null;
  run: string | null;
  secrets: string[];
  /**
   * Staged evaluation ordering (spec §3.3): evaluators run in ascending
   * `stage` order; within a stage they fan out in parallel. A later stage's
   * tasks are created only after every *required* evaluator in the prior
   * stage passes. Default 0 — a single-stage job is byte-for-byte the
   * unstaged behavior. Non-negative (`u32`, enforced at parse).
   */
  stage: number;
  type: EvaluatorType;
}
export interface FileQuery {
  path: string;
}
/**
 * One fleet node's live occupancy. `name`/`slots`/`available`/`version` mirror
 * [`WorkerNode`]; `occupied`/`running` add the live slot usage.
 */
export interface FleetNode {
  /**
   * Node health at snapshot time (spec §3.1): `false` when out of service.
   */
  available: boolean;
  /**
   * The daemon's reason for refusing the desired value, when it refused one —
   * shown beside the node's slot widget until the operator changes the request.
   */
  capacity_note?: string | null;
  /**
   * When the node last reported its capacity; `None` when it never has.
   */
  capacity_observed_at?: string | null;
  /**
   * Where [`Self::slots`] came from (design #293 §7/§8): `node` once the node
   * has reported over either transport, `seed` while the `DOCKER_NODES` boot
   * value is still standing in for a report that never arrived. `None` for a
   * docker-endpoint node, whose capacity `DOCKER_NODES` still owns.
   */
  capacity_source?: CapacitySource | null;
  /**
   * How far intent and observation are apart (design #293 §4/§10). `None` when
   * there is no intent to reconcile.
   */
  capacity_state?: CapacityState | null;
  name: string;
  /**
   * Busy slot count (`running.len()`), denormalized so the UI needn't count.
   */
  occupied: number;
  /**
   * The node's last self-refresh outcome (ticket #187), last reported by a
   * worker's ping — so a failed refresh is visible in the live fleet, not
   * just the node's logs. `None` when the node has not refreshed (or is a
   * docker-endpoint node).
   */
  refresh_outcome?: RefreshOutcome | null;
  /**
   * The occupied slots on this node.
   */
  running: SlotOccupant[];
  /**
   * Total concurrent-container capacity ([`WorkerNode::slots`]). `None` for a
   * node observed only through a running container (not in the configured
   * roster) — its cap is unknown from occupancy alone.
   */
  slots: number | null;
  /**
   * The operator's **desired** slot count for this node (design #293 §2), from
   * the `fleet.capacity` intent record. `None` when no operator has ever set
   * one. Display only: [`Self::slots`] stays the number the scheduler uses, and
   * intent is structurally incapable of placing work.
   */
  slots_desired?: number | null;
  /**
   * Build version last reported by a worker node's ping, if any.
   */
  version: string | null;
}
/**
 * What one busy slot is running (spec §3.1) — enough for the UI to link back to
 * the job/task without a second fetch.
 */
export interface SlotOccupant {
  /**
   * Job sequence (`Job::id`).
   */
  job_seq: number;
  /**
   * The job type (`Job::type`).
   */
  job_type: string;
  /**
   * Job phase (`JobState`), lowercased: `work`, `evaluation`, `wrap_up`, …
   */
  phase: string;
  /**
   * `owner/project` slug the job belongs to (a job seq is only unique within
   * a project).
   */
  project: string;
  /**
   * When the container launched (`Task::started_at`), if known.
   */
  started_at: string | null;
  /**
   * Task id within the job (`Task::id`).
   */
  task_id: number;
  /**
   * Task phase: `work` | `eval` | `gate` | `wrap_up` | `triage`
   * (from `TaskPhase`).
   */
  task_kind: string;
}
/**
 * Live fleet occupancy (spec §3.1): which slots on which node are busy and what
 * job/task each busy slot is running. The [`DispatcherConfigSnapshot`]
 * describes the fleet *statically* (names, slot counts, versions); this reports
 * live *usage*, which the config snapshot can't — with more than one node the
 * UI can't otherwise place work on nodes.
 *
 * Published by the dispatcher (the single writer) to the `platform` bucket
 * (key `fleet.status`) on every task launch/exit and after restart
 * re-attachment — a full snapshot each change, cheap at our scale. Read back by
 * the api at `GET /api/v1/platform/fleet`. No dedicated event announces the
 * change: every occupancy change coincides with a task lifecycle event
 * (`task-launched`/`task-queued`, task/job state) already on the job-event
 * stream, on which an SSE client refetches.
 */
export interface FleetStatus {
  /**
   * One entry per fleet node, whether idle or busy.
   */
  nodes: FleetNode[];
  /**
   * Launches parked waiting for a free slot — the §3.5 launch capacity queue
   * depth. Best-effort: whatever the dispatcher can surface, `0` when nothing
   * waits.
   */
  queue_depth: number;
}
/**
 * A row of `GET .../groups`: the roll-up, plus the design document the name
 * conventionally refers to when one is there.
 *
 * `doc_path`/`doc_status` are best-effort and present only for a
 * `design/`-namespaced name that resolves to a document at default-branch
 * HEAD — the knowledge-tag posture (spec §4.4: a tag with no file is skipped).
 * A group whose document is absent still lists; it just renders without a
 * status.
 */
export interface GroupEntry {
  /**
   * Per-state histogram keyed by the state name serde writes, zero states
   * omitted. Not a percentage: "5 Done, 1 Frozen" is the operator's actual
   * question, and a percentage discards *which* one is not done.
   */
  counts: {
    [k: string]: number;
  };
  /**
   * Where the document was found, so a reader fetching it back through
   * `GET .../file` never re-derives the path (the `req.tags.list` posture).
   */
  doc_path?: string | null;
  /**
   * The document's status line, verbatim and unparsed (design #321
   * Decision 8). The platform compares it to nothing and infers nothing
   * from it.
   */
  doc_status?: string | null;
  /**
   * The members, in ascending job seq.
   */
  jobs: GroupJob[];
  /**
   * The label the members carry, verbatim.
   */
  name: string;
  /**
   * Members that are not terminal, via [`JobState::is_terminal`] — the same
   * definition batches and the roll-up's staleness flag already use.
   */
  open: number;
}
/**
 * A group and how its members are doing — the shape both derived reads carry,
 * so a design's roll-up and a group's roll-up are the same thing rendered
 * twice rather than two shapes to keep in step.
 */
export interface GroupRollup {
  /**
   * Per-state histogram keyed by the state name serde writes, zero states
   * omitted. Not a percentage: "5 Done, 1 Frozen" is the operator's actual
   * question, and a percentage discards *which* one is not done.
   */
  counts: {
    [k: string]: number;
  };
  /**
   * The members, in ascending job seq.
   */
  jobs: GroupJob[];
  /**
   * The label the members carry, verbatim.
   */
  name: string;
  /**
   * Members that are not terminal, via [`JobState::is_terminal`] — the same
   * definition batches and the roll-up's staleness flag already use.
   */
  open: number;
}
export interface Identity {
  kind: IdentityKind;
  platform_admin: boolean;
  project_roles: {
    [k: string]: ProjectRole;
  };
  sub: string;
}
/**
 * One declared job input (spec §1.1, design #311 Decision 2): a name, a kind,
 * and the narrowing that makes a supplied value safe to hand to a script.
 *
 * A nested block, so it keeps `deny_unknown_fields` like every other
 * gate-relevant block (§14.2): an ignored key here could silently drop a
 * `pattern`, and `pattern` is a validation control, not decoration.
 *
 * The kind set is deliberately two. `bool` is an `enum` over `["true",
 * "false"]`, `int` is a `string` with `pattern: '^[0-9]+$'`, and lists have no
 * env representation that is not an encoding decision — the env value is a
 * string either way, so a richer type system here would be a second config
 * language.
 */
export interface Input {
  /**
   * A value the platform materializes onto the job record when the creator
   * supplies none — not a create-form pre-fill, so what actually ran is on
   * the audit surfaces. Disallowed with `required: true`, and validated here
   * against the charset and this declaration's own `pattern`/`values`: a
   * default no supply path could have produced would otherwise arrive by the
   * back door and be caught only at launch.
   */
  default?: string | null;
  /**
   * Shown in the create form and in the agent's job brief.
   */
  description?: string | null;
  /**
   * `[a-z][a-z0-9_]*` ([`crate::inputs::INPUT_NAME_PATTERN`]) — lowercase so
   * the mapping onto one reserved env name is injective.
   */
  name: string;
  /**
   * A regex the **whole** value must match; `type: string` only. It may only
   * narrow the default charset, never widen it (the effective check is
   * `charset AND pattern` — [`crate::inputs::check_value`]). An input whose
   * value reaches an argv position wants one: the charset stops metacharacter
   * injection but not a value that begins with `-` or `/`.
   */
  pattern?: string | null;
  /**
   * Default false. An optional input with no supplied value and no `default`
   * is *absent*, never an empty string: `set -u` catches an unset
   * `$CHUG_INPUT_SHA` loudly, where an empty string would silently run
   * `update.sh ` with no argument.
   */
  required: boolean;
  type: InputKind;
  /**
   * The closed list for `type: enum`; disallowed for `type: string`.
   */
  values?: string[];
}
/**
 * A node in the project DAG. Stored in NATS KV at `jobs.{owner}.{project}.{seq}`;
 * the dispatcher is its sole writer.
 */
export interface Job {
  /**
   * Exact HEAD of default branch; set/updated at every Ready-transition and on
   * squash-merge conflict; None until job first enters Ready.
   */
  base_ref: string | null;
  /**
   * Set on a member job absorbed into a batch: the batch job's id. `Some`
   * implies the job is (or was) [`JobState::Batched`] under that batch;
   * cleared when the batch is revoked/fails and the member returns to Frozen.
   * None for ordinary jobs and batches themselves; defaulted for old records.
   */
  batch_id?: number | null;
  /**
   * `"job/{id}"`; set at creation; actual git branch created when job enters Work.
   */
  branch: string;
  /**
   * A human has claimed the job's NEXT work attempt (spec §1.2 claims):
   * instead of launching a container, the dispatcher parks that attempt as
   * a Pending task with the declared kind and `performed_by: human`, then
   * clears this flag — a claim covers exactly one attempt. Defaults false
   * so records that predate claims deserialize.
   */
  claim_next: boolean;
  /**
   * When the job reached a terminal state (Done or Revoked). Set once by the
   * dispatcher's single state-write path at the terminal transition and never
   * cleared — so the jobs list can show completion time and duration without
   * opening the job. None while the job is still live; defaulted so records
   * written before completion stamping existed deserialize.
   */
  completed_at?: string | null;
  /**
   * Optional rich, richly-formatted cover page for the operator UI (spec
   * §1.1, §4.3). Purely presentational: unlike [`Job::description`], it is
   * **never** injected into any agent prompt — the job brief consumes only
   * title/description, so the cover can carry HTML the UI sandboxes without
   * polluting what agents read. Authors should ship self-contained styling
   * (no external scripts/network). None for records that predate it and for
   * jobs with a plain-text brief; defaulted so older records deserialize.
   */
  cover_html?: string | null;
  created_at: string;
  /**
   * Upstream job ids this job depends on. Edges are ordering: upstreams
   * must be Done first (their work is in this job's base, their structured
   * results are available to it). Plain ids, no named roles — picked at
   * creation, validated (existence, no cycles) at release.
   */
  deps: number[];
  description: string;
  /**
   * Structured record of the job's most recent escalation or stall (spec
   * §1.2, §3.4): the reason code, a human-readable detail, the failing task
   * (when one exists), and when it happened — so operators see WHY on the
   * record instead of reconstructing it from dispatcher logs. Written at
   * every escalate/stall call site; advisory, no transition consults it.
   * None until the job first escalates; defaulted so older records
   * deserialize.
   */
  escalation?: Escalation | null;
  /**
   * Additive per-job evaluators (design-lifecycle.md): layered on top of the
   * type's `eval:` list at execution. The type's evaluators are a floor —
   * creation can add criteria, never remove or override them. Name
   * collisions with the type's evaluators are a release-time error.
   */
  eval: Evaluator[];
  /**
   * Factory name when created by a factory triage agent (spec §13); None for
   * operator-created jobs.
   */
  factory: string | null;
  /**
   * What this job is **part of** (spec §1.1 `groups:`, design #321): the
   * operator's own labels — `design/311-job-inputs`, `beacon-import` — so a
   * group of jobs can be enumerated and rolled up. Many per job; shape-checked
   * by [`crate::groups::check_groups`] and nothing else. Empty for every job
   * nobody grouped, which is every job that predates the field — defaulted so
   * old records deserialize, skipped on the wire when empty so such a record
   * is byte-identical to what it is today.
   *
   * **Inert to execution** (design #321 Decision 3), and that is a property
   * rather than an intention: no container env, no agent prompt, no job-type
   * resolution and no state transition reads it, pinned by
   * `groups_never_reach_the_job_brief` (`dispatcher::exec`) and the tier-2
   * byte-identical-env trace. That is what makes the third write path safe —
   * unlike every other field here, `groups` is **mutable in every state,
   * including `Done` and `Revoked`** (`req.jobs.groups.*`): annotating a
   * finished ticket with what it was part of does not change what it did.
   *
   * Deliberately *not* a knowledge tag, whose resolution at `base_ref` changes
   * what the agent is told; deliberately not a dep, which is ordering; and
   * deliberately carrying no aggregate — every count and every enumeration is
   * derived from the job records at read time (Decision 4).
   */
  groups?: string[];
  /**
   * Sequential per project; maintained via counter in NATS KV.
   */
  id: number;
  /**
   * The job's **effective** input values (spec §1.1 `inputs:`, design #311).
   * Empty for every job whose type declares no inputs, which is every job
   * that predates the feature — defaulted so old records deserialize, and
   * skipped on the wire when empty so such a record is byte-identical to what
   * it is today.
   *
   * A `BTreeMap` for deterministic ordering (like [`crate::JobType::unknown`]):
   * the map is an audit surface, and a stable order is what makes two records
   * comparable.
   *
   * **Written by exactly two paths**, both on the single-writer dispatcher:
   * creation (and the Draft edit, which is the same act repeated), and the
   * Ready-transition that *first* records [`Job::base_ref`], which fills in a
   * declared `default` for every input the creator did not supply — add-only,
   * never overwriting a supplied value. From that moment this is the complete
   * effective set: what the run acted on, beside the config version it acted
   * under. **Immutable thereafter** — not on rework, not on a work retry, not
   * on a claim, and not across a later `base_ref` update (a re-resolved
   * default would make the target mutable mid-flight). Getting a different
   * target is getting a different job.
   */
  inputs?: {
    [k: string]: string;
  };
  /**
   * Union of job type defaults and operator-supplied tags at creation.
   */
  knowledge_tags: string[];
  /**
   * Member job ids absorbed into this batch (design-lifecycle.md, spec §2.1
   * batches). Empty for an ordinary job; non-empty marks this job as a
   * **batch** — one branch implementing all members, evaluated under the
   * union of their criteria, whose single merge completes every member.
   * Serde-defaulted so records written before batches deserialize.
   */
  members: number[];
  /**
   * Optional per-job model override for the Work agent (spec §1.1, §12.4).
   * The most specific choice an operator can make, so it wins over every
   * other layer: the job type's `work.model`, the project default
   * (`.chug/jobs/_defaults.yaml`), and the platform default (`AGENT_MODEL_DEFAULT`).
   * Applies to Work-phase agent tasks only — evaluators keep the
   * type/project/platform resolution, exactly as [`Job::timeout`] scopes to
   * Work. None → the resolution chain applies. Defaulted for records written
   * before per-job model selection existed.
   */
  model: string | null;
  /**
   * `"{owner}/{repo}"` slug.
   */
  project: string;
  /**
   * Set once (immutably) when job first enters Ready; anchor for `job_deadline`.
   */
  ready_at: string | null;
  state: JobState;
  /**
   * How long the job spent **working**: the sum of its own tasks' spans, per
   * [`crate::task::task_time_ms`]. Carried on the record — not derived by the
   * UI — so the jobs list can show a job's duration without a per-row task
   * fetch, and recomputed from that one job's tasks whenever one of them is
   * written back, so a missed write self-heals instead of drifting.
   *
   * Distinct from `completed_at - created_at`, which is dominated by the
   * waiting a job does while Frozen and Blocked. None while no task of the
   * job carries a usable span, and on records written before the field
   * existed — a `Some(0)` genuinely means "took no measurable time".
   */
  task_time_ms?: number | null;
  /**
   * Optional per-job work-task timeout override (duration string, §1.1).
   * Layers over the job type's `resources.task_timeout` exactly like [`eval`]
   * layers over the type's evaluators — but for Work tasks only; evaluators
   * keep the type default. Any valid duration (shorter or longer). Parseability
   * is validated at release, consistent with "wiring validated at release, not
   * creation". None means the type default applies.
   */
  timeout: string | null;
  /**
   * Ticket-style instance identity: what this particular run is for.
   * The type carries the *how* (prompts, evaluators); title/description
   * carry the *what*, and are injected into work and eval prompts as the
   * job brief (§4.3). Empty for jobs whose prompt is self-contained.
   */
  title: string;
  /**
   * Job type name; references `.chug/jobs/{type}.yaml` at `base_ref`.
   */
  type: string;
}
/**
 * The jobs-**list** projection (spec §6.1): every [`Job`] field except the two
 * heavy prose ones, `description` and `cover_html`.
 *
 * Those two dominate the payload — on the dogfood project they are 78% of a
 * 578 KB list reply — and no list consumer reads either: the operator table
 * renders title/type/state, and its search matches id/title/type only. They
 * stay on the single-job reply, which is where the UI renders them.
 *
 * Serialize-only by design: this is a wire shape, never a stored record, and
 * nothing should be able to round-trip a summary back into a [`Job`] with an
 * empty description. `job_summary_mirrors_job_fields` pins the field set, so
 * a field added to [`Job`] fails the build until someone decides whether the
 * list should carry it.
 */
export interface JobSummary {
  base_ref: string | null;
  batch_id?: number | null;
  branch: string;
  /**
   * The job's latest channel post, for the muted progress line the operator
   * table shows under a live job's title. Not a [`Job`] field — it lives in
   * the `channels` bucket — and the one deliberate *addition* the projection
   * makes over the record.
   *
   * Carrying it here is what lets a cold page load stop replaying the
   * project's entire event history just to learn what a handful of live jobs
   * are doing. Only populated for non-terminal jobs; None otherwise.
   */
  channel?: ChannelUpdate | null;
  claim_next: boolean;
  completed_at?: string | null;
  created_at: string;
  deps: number[];
  escalation?: Escalation | null;
  eval: Evaluator[];
  factory: string | null;
  /**
   * What the job is part of ([`Job::groups`]). Carried by the list because
   * the list is where filtering and the group chips happen (design #321
   * Decision 7), and bounded small by construction (at most
   * `GROUPS_COUNT_MAX` × `GROUP_NAME_LEN_MAX` bytes), unlike the prose fields
   * the projection drops.
   *
   * Skipped when empty, matching the record rather than the projection's
   * other list fields: the two shapes must stay *assignable* in the generated
   * client (`web/src/api/types.gen.ts`), where the job page hands a full
   * `Job` to code typed on `JobSummary`. A required field here against an
   * optional one there is exactly the mismatch that breaks.
   */
  groups?: string[];
  id: number;
  /**
   * The job's effective inputs ([`Job::inputs`]). Carried by the list because
   * they are what a parameterized job *is* — a `deploy` row that does not say
   * which service it deploys is unreadable — and bounded small by
   * construction (at most `INPUTS_COUNT_MAX` short values), unlike the prose
   * fields the projection drops.
   */
  inputs?: {
    [k: string]: string;
  };
  knowledge_tags: string[];
  members: number[];
  model: string | null;
  project: string;
  ready_at: string | null;
  state: JobState;
  task_time_ms?: number | null;
  timeout: string | null;
  title: string;
  type: string;
}
/**
 * The top-level job-type struct deliberately does **not** carry
 * `deny_unknown_fields` (unlike every nested block below): an unknown
 * top-level key is captured into [`JobType::unknown`] and surfaced as a
 * *warning*, not a parse error. This is the schema-tolerance half of the
 * config/binary version-skew contract (spec §14): job-type config is read
 * live from the default branch, so a config change can land ahead of the
 * running dispatcher. A newly-added top-level section (a future `wrap_up`,
 * `deploy`, …) an older binary doesn't know about means "a feature is quietly
 * off" — acceptable when flagged loudly, and vastly preferable to the
 * 2026-07-22 incident where the strict parser rejected the whole config and
 * escalated every job.
 *
 * Laxity is safe *only* at the top level. The nested blocks keep
 * `deny_unknown_fields` because an unknown field inside a security-relevant
 * section is not benign: an ignored key inside an [`Evaluator`] could silently
 * skip a *gate* (a typo'd `required: flase`, a mis-nested check), turning
 * "config ahead of binary" into "a merge gate quietly disabled". Those stay
 * hard errors; see [`JobType::validate`].
 */
export interface JobType {
  /**
   * One-line summary shown alongside the display name in the type picker.
   */
  description: string | null;
  /**
   * Human-facing name for the library and the create-form type picker;
   * falls back to `name`.
   */
  display_name: string | null;
  eval: Evaluator[];
  eval_retries: number | null;
  /**
   * Required for agent/command work; disallowed at top level for human work.
   */
  image: string | null;
  /**
   * The values a job of this type accepts (spec §1.1, design #311). Empty for
   * every job type that declares none, which is every job type that predates
   * the feature.
   *
   * An input is a **value delivered to a running container**, never a
   * substitution into this file: nothing here can select an image, an
   * evaluator, a secret or a `run:` string, so the job type resolves without
   * reading a job's inputs at all (#311 Decision 1). Parameterization happens
   * inside the work, where `deploy.sh` reads `$CHUG_INPUT_SERVICE`.
   *
   * A non-empty list requires [`JobType::min_dispatcher`] — see
   * [`JobType::validate`]. To an N-1 dispatcher `inputs:` is just an unknown
   * top-level field it tolerates (captured into [`JobType::unknown`]), so the
   * declaration would be silently ignored and the container would launch with
   * no value at all; `min_dispatcher` is a field that dispatcher *does* parse,
   * which is why the skew gate is structural rather than left to authorship.
   */
  inputs: Input[];
  job_deadline: string | null;
  knowledge: string[];
  /**
   * Minimum dispatcher schema epoch this config requires (spec §14). When
   * set and greater than the running dispatcher's
   * [`crate::version::CONFIG_SCHEMA_EPOCH`], the config is ahead of the
   * binary: the dispatcher parks the job with a platform-level diagnostic
   * ("config requires dispatcher >= X") instead of launching it, and the
   * merge-time CI check fails the config's own build if it can reach a
   * deployed dispatcher advertising an older epoch. Author it in the same
   * commit that relies on a schema feature the previous generation lacks, so
   * "merging config" can never silently become "deploying config".
   */
  min_dispatcher?: number | null;
  name: string;
  /**
   * Optional placement pin (spec §3.1). When set, every container this job
   * type launches is placed on the named fleet node instead of the
   * most-free one. A single pin — no labels, no anti-affinity, no spillover.
   */
  placement: Placement | null;
  resources: Resources | null;
  rework_budget: number | null;
  vars: string[];
  work: WorkSpec;
  work_retries: number | null;
  wrap_up: WrapUpSpec;
}
/**
 * Placement pin (spec §3.1). Shape-only here: `node` names a fleet node, but
 * whether that node is actually configured cannot be checked offline (the
 * fleet lives in the dispatcher's env), so release/`validate` only enforce the
 * name is a well-formed node token — the launch honors or errors on it.
 */
export interface Placement {
  /**
   * Fleet node name to pin onto (`[A-Za-z0-9_-]+`, the same subject-safe
   * token the fleet config uses).
   */
  node: string | null;
}
export interface Resources {
  cpu: number | null;
  /**
   * Memory limit: a positive integer, optionally suffixed with a binary
   * unit (`Ki`/`Mi`/`Gi`), or plain bytes — e.g. `512Mi`, `4Gi`, `1048576`.
   * No other suffixes (`5g`, `4GB` are rejected). Validated at parse time so
   * a bad value fails offline instead of at container launch.
   */
  memory: string | null;
  task_timeout: string | null;
}
export interface WorkSpec {
  /**
   * agent only.
   */
  model: string | null;
  /**
   * agent/human: required. command: disallowed.
   */
  prompt: string | null;
  /**
   * agent only.
   */
  provider: Provider | null;
  /**
   * agent only; enables the inline review loop (spec §4.5).
   */
  review: ReviewSpec | null;
  /**
   * command only.
   */
  run: string | null;
  /**
   * Secrets injected into the work container (agent/command). Scoped here
   * because that is the only container they reach — evaluators declare
   * their own (§4.1). Disallowed for human work (no container).
   */
  secrets: string[];
  type: WorkType;
}
/**
 * Inline review loop declaration (spec §1.1, §4.5). The reviewer runs inside
 * the work container; its acceptance gates `submit_result`, not the merge.
 */
export interface ReviewSpec {
  /**
   * Max author↔reviewer rounds before submitting anyway. Default 5.
   */
  iterations: number | null;
  model: string | null;
  /**
   * Path to reviewer prompt file in repo (resolved from base_ref).
   */
  prompt: string;
  /**
   * Defaults to the work provider. v1 supports claude only (release-time
   * validation).
   */
  provider: Provider | null;
}
/**
 * Wrap-up declaration (design-lifecycle.md): the job's third step. A block
 * rather than a bare scalar so future wrap-up behavior (e.g. a
 * `deployed/{env}` tag ref) extends it without reshaping the schema.
 */
export interface WrapUpSpec {
  /**
   * Image for the `run` container; falls back to the job's top-level image
   * (like an evaluator, §1.1). Required when `run` is set and the job type
   * declares no top-level image (`work.type: human`).
   */
  image?: string | null;
  /**
   * Human-facing label for the wrap-up task, so it is as self-describing as
   * an evaluator (`Command · publish` instead of a bare `Command`, job #146).
   * Validated like an evaluator name. Unset → derived from the mode (see
   * [`WrapUpSpec::label`]): a command wrap-up takes its script's basename
   * (`.chug/tasks/web-publish.sh` → `web-publish`).
   */
  name?: string | null;
  /**
   * Optional post-merge command (spec §3.2, design-lifecycle.md wrap-up
   * hook): a shell command run in the WrapUp phase *after* the squash lands
   * on the default branch, against the merged main content. It ships the
   * merged result — a web job publishing its built UI, say — so it only runs
   * once the merge is final, and never at all if the job is revoked or
   * escalated before landing. Valid with `type: merge` only. A non-zero exit
   * escalates the job (the merge is not undone). The command clones the
   * default branch, so it must be idempotent (a restart may re-launch it).
   */
  run?: string | null;
  /**
   * Secrets injected into the `run` container. Scoped here because that is
   * the only container they reach; not inherited from `work.secrets`.
   */
  secrets?: string[];
  type: WrapUpMode;
}
export interface LoginBody {
  email: string;
  password: string;
}
export interface MemberRoleBody {
  /**
   * `owner` | `member` | `viewer` (`owner` is the top project role, §7.5).
   */
  role: string;
}
/**
 * Immutable link configuration, mirrored from the bare repo's git config
 * (`remote.origin.url`) for API/UI consumption.
 */
export interface OriginLink {
  /**
   * `owner/repo` for the GitHub REST API, parsed from `url` at link time.
   * `None` for non-GitHub origins (e.g. `file://` test fixtures) — release
   * then pushes the branch but cannot open a PR.
   */
  github_repo: string | null;
  /**
   * The origin's default branch (usually `main`), autodetected at link time.
   */
  main_branch: string;
  /**
   * Git URL of the external origin (`ssh://git@github.com/owner/repo.git`).
   */
  url: string;
}
export interface OutputQuery {
  /**
   * Byte cursor: return output from here on. 0 (default) reads from the start.
   */
  since?: number;
}
/**
 * One queued launch belonging to the requested project.
 */
export interface QueueEntry {
  /**
   * 1-indexed position in the global FIFO — the "N" in the badge.
   */
  position: number;
  /**
   * When the launch joined the queue (mirrors `Task::queued_at`).
   */
  queued_at: string;
  seq: number;
  task_id: number;
}
/**
 * A point-in-time view of the capacity launch queue, scoped to one project.
 */
export interface QueueSnapshot {
  /**
   * Total launches queued across the whole fleet — the "of M" in the badge.
   */
  depth: number;
  /**
   * The requested project's queued launches, in FIFO order.
   */
  entries: QueueEntry[];
}
/**
 * One origin release: a frozen snapshot of `integration` pushed to the origin
 * as `chug/release-{number}` with a PR into the origin's default branch.
 */
export interface ReleaseState {
  /**
   * `refs/remotes/origin/{main}` at PR-open time.
   */
  base_main_sha: string;
  /**
   * `integration` at PR-open time (== tip of `chug/release-{number}`).
   */
  integration_sha: string;
  number: number;
  pr_number: number;
  pr_url: string;
  status: ReleaseStatus;
}
export interface SshCertBody {
  public_key: string;
}
/**
 * Unit of execution within a job's Work and Evaluation phases. Chronological log,
 * no task graph. Stored at `tasks.{owner}.{project}.{job_seq}.{task_id}`.
 */
export interface Task {
  /**
   * 1-indexed; each retry is a new task record with attempt+1.
   */
  attempt: number;
  completed_at: string | null;
  /**
   * Backend-assigned container ID (Docker or k8s); None for Human tasks.
   * Persisted the instant the container launches — while the task is still
   * Running — and kept after exit, so operators and artifact tooling can name
   * a live container, not just a finished one.
   */
  container_id: string | null;
  created_at: string;
  cycle: number;
  /**
   * Evaluator name for Evaluation/MergeGate tasks; None for work and
   * escalation tasks. Ties the task to its `eval:` declaration — restart
   * reconciliation and the UI both need the mapping.
   */
  evaluator: string | null;
  /**
   * Sequential within job, 1-indexed.
   */
  id: number;
  /**
   * Set when reconciliation retired this attempt because its container was
   * gone at restart (spec §3.6): docker pruned it, the node rebooted, colima
   * restarted. That is an infrastructure loss, NOT a real nonzero exit — the
   * relaunch does not consume a `work_retries`/`eval_retries` budget, and
   * these markers are counted to cap infra relaunches per task before
   * escalating (`infra_loss`). Defaulted so records written before it existed
   * still deserialize; false for every real failure and completion.
   */
  infra_loss: boolean;
  job_seq: number;
  kind: TaskKind;
  /**
   * Human-facing label for the task, so every task kind is as
   * self-describing as an evaluator (job #146). Set from the job-type config:
   * a wrap-up task carries its `wrap_up.name` (or a derived default), and an
   * evaluator task mirrors its `evaluator` name here so the UI reads one
   * label field for both. None for work/escalation/triage tasks and for
   * records written before labels existed (they fall back to `evaluator`).
   */
  label?: string | null;
  /**
   * Why this task is `Pending`, when the reason is worth surfacing (spec
   * §3.5). Set to [`PendingReason::QueuedForCapacity`] when the capacity
   * launch queue defers a container launch (no free fleet slot); cleared the
   * instant the launch succeeds. Absent for a task Pending for any other
   * reason — a parked human/claimed attempt, or a just-created task awaiting
   * its first launch — so the UI can distinguish a queued launch from an
   * idle Pending. Defaulted + skipped so records written before it existed
   * still deserialize and non-queued tasks carry no key.
   */
  pending_reason?: PendingReason | null;
  /**
   * Who actually performed this attempt when it differs from what `kind`
   * declares (spec §1.2 claims): a claimed attempt keeps its declared kind
   * — the job type's immutable requirement — and records the human
   * performer here. Absent (None) for every normally-executed attempt and
   * for records written before claims existed.
   */
  performed_by?: Performer | null;
  phase: TaskPhase;
  project: string;
  /**
   * When this launch first joined the capacity queue (spec §3.5), stamped
   * alongside [`Self::pending_reason`] and cleared on launch. Persisted so
   * the queue survives a dispatcher restart *faithfully*: reconciliation
   * re-queues Pending launches sorted by this timestamp (stable FIFO across
   * restarts, not reconcile iteration order), and the max-queue-wait backstop
   * measures the total wait from it rather than from process-local time —
   * under frequent auto-deploys the in-memory clock would otherwise reset
   * every restart and never fire. None for non-queued tasks. Defaulted so
   * pre-existing records still deserialize.
   */
  queued_at?: string | null;
  result: TaskResult | null;
  /**
   * Evaluation/MergeGate tasks only: the branch tip SHA this evaluator round
   * judged, resolved at launch (spec §3.3, job #155). Persisted so a later
   * cycle's re-review can show the reviewer "what you reviewed last time" and
   * compute the `last_reviewed_tip..HEAD` delta — and so that context
   * survives a dispatcher restart (rebuilt from the task log, not memory).
   * None for work/escalation/triage tasks and pre-#155 records.
   */
  reviewed_tip?: string | null;
  /**
   * Why a rework cycle created this Work task (spec §3.3): set at rework
   * re-entry so a Work task appearing after passed evaluations is
   * self-explaining. None for cycle-1 work, evaluation/gate/wrap-up tasks,
   * and every non-Work task. Defaulted so records written before rework
   * causes were recorded still deserialize.
   */
  rework_reason?: ReworkReason | null;
  /**
   * Agent tasks only: the session id handed to the agent CLI, which names
   * its transcript. Recorded at task creation so the artifact stays
   * addressable across a dispatcher restart, and so a later cycle can resume
   * the conversation.
   */
  session_id: string | null;
  /**
   * Evaluation stage this task belongs to (spec §3.3 staged evaluation).
   * Carries the evaluator's `stage:` for Evaluation/MergeGate tasks so the
   * UI can group a cycle's tasks by stage; 0 for work/escalation/triage
   * tasks, which have no stage. Defaulted for records written before staging.
   */
  stage: number;
  started_at: string | null;
  state: TaskState;
}
export interface TokenUsage {
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  input_tokens: number;
  output_tokens: number;
}
