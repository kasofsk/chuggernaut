/**
 * GENERATED — DO NOT EDIT.
 *
 * The example payloads in wire-samples.json (serialized from real Rust values
 * by `chuggernaut schema api-samples`), each restated as a TypeScript literal
 * that `satisfies` the generated type for its schema name.
 *
 * That `satisfies` is the round trip: `tsc` checks bytes serde actually wrote
 * against the types the UI compiles with, and — because each payload is a fresh
 * literal — a field serde emits that the type does not declare is an excess-
 * property error rather than a silent extra key. Importing the JSON directly
 * could not do this: TypeScript widens every string in a JSON module to
 * `string`, so no discriminated union in the contract would be checked at all.
 *
 * Regenerate with `npm run codegen`. The assertions that exercise these values
 * at runtime live in roundtrip.test.ts.
 */

import type {
  DeployReport,
  DesignEntry,
  DispatcherConfigSnapshot,
  FleetStatus,
  GroupEntry,
  Identity,
  Job,
  JobSummary,
  JobType,
  QueueSnapshot,
  Task,
  TaskResolution,
  TaskResult,
} from "./types.gen";

export const wireSamples = {
  DeployReport: {
    from_sha: "1111111",
    health: "ok",
    legs: [
      {
        detail: "cargo build --release",
        name: "build-dispatcher",
        secs: 93,
        status: "ok",
      },
    ],
    rollback: false,
    to_sha: "2222222",
  } satisfies DeployReport,
  DesignEntry: {
    counts: {
      Done: 1,
    },
    jobs: [
      {
        id: 42,
        state: "Done",
        title: "slice B: the derived reads",
        type: "code",
      },
    ],
    name: "design/321-job-groups",
    open: 0,
    path: "docs/design/321-job-groups.md",
    seq: 321,
    slug: "321-job-groups",
    status: "PROPOSED",
    status_stale: true,
    title: "Design #321 — Job groups",
  } satisfies DesignEntry,
  DispatcherConfigSnapshot: {
    agent_model_default: "claude-opus-5",
    agent_provider_default: "claude",
    channel_binary: "/usr/local/bin/chug-channel",
    commits_behind: 2,
    dispatcher_sha: "2222222",
    hook_bin: "/usr/local/bin/chuggernaut",
    main_tip_sha: "3333333",
    nats_url: "nats://127.0.0.1:4222",
    nats_url_container: "nats://host.docker.internal:4222",
    nodes: [
      {
        available: true,
        capacity_observed_at: "2026-07-24T17:07:00Z",
        capacity_source: "node",
        endpoint: "nats",
        name: "gumbo-mini-0",
        refresh_outcome: {
          accepted_at: "2026-07-24T16:00:00Z",
          finished_at: "2026-07-24T16:04:00Z",
          from_sha: "1111111",
          result: {
            error_tail: "systemctl: unit not found",
            result: "failed",
            stage: "swap",
          },
          to_sha: "2222222",
        },
        slots: 4,
        version: "chuggernaut 0.1.0 (2222222)",
      },
    ],
    placement_policy: "busyness",
    repo_url_base: "ssh://git@chug.example",
    repos_root: "/var/lib/chuggernaut/repos",
    schema_epoch: 7,
    secrets_encryption: true,
    triage_image: "chuggernaut/agent-claude:latest",
  } satisfies DispatcherConfigSnapshot,
  FleetStatus: {
    nodes: [
      {
        available: true,
        capacity_observed_at: "2026-07-24T17:07:00Z",
        capacity_source: "node",
        capacity_state: "converged",
        name: "gumbo-mini-0",
        occupied: 1,
        refresh_outcome: {
          accepted_at: "2026-07-24T16:00:00Z",
          finished_at: "2026-07-24T16:04:00Z",
          from_sha: "1111111",
          result: {
            error_tail: "systemctl: unit not found",
            result: "failed",
            stage: "swap",
          },
          to_sha: "2222222",
        },
        running: [
          {
            job_seq: 42,
            job_type: "code",
            phase: "work",
            project: "acme/api",
            started_at: "2026-07-24T17:07:00Z",
            task_id: 3,
            task_kind: "work",
          },
        ],
        slots: 4,
        slots_desired: 4,
        version: "chuggernaut 0.1.0 (2222222)",
      },
    ],
    queue_depth: 3,
  } satisfies FleetStatus,
  GroupEntry: {
    counts: {
      Done: 1,
    },
    doc_path: "docs/design/321-job-groups.md",
    doc_status: "PROPOSED",
    jobs: [
      {
        id: 42,
        state: "Done",
        title: "slice B: the derived reads",
        type: "code",
      },
    ],
    name: "design/321-job-groups",
    open: 0,
  } satisfies GroupEntry,
  Identity: {
    kind: "user",
    platform_admin: true,
    project_roles: {
      "acme/api": "admin",
    },
    sub: "operator@acme.example",
  } satisfies Identity,
  Job: {
    base_ref: "f00dcafe",
    branch: "job/42",
    claim_next: false,
    completed_at: "2026-07-24T19:00:00Z",
    cover_html: "<h1>done</h1>",
    created_at: "2026-07-24T17:00:00Z",
    deps: [41],
    description: "Replace the hand-mirrored interfaces.",
    escalation: {
      at: "2026-07-24T18:30:00Z",
      detail: "the work agent failed three attempts",
      failing_task: 3,
      reason: "work_retries_exhausted",
    },
    eval: [
      {
        image: "chuggernaut/agent-rust:latest",
        model: null,
        name: "ci",
        prompt: null,
        provider: null,
        required: true,
        run: ".chug/tasks/ci.sh",
        secrets: ["CI_TOKEN"],
        stage: 1,
        type: "command",
      },
    ],
    factory: null,
    groups: ["design/321-job-groups", "beacon-import"],
    id: 42,
    inputs: {
      service: "web",
      sha: "4f9c1ab",
    },
    knowledge_tags: ["web"],
    members: [],
    model: "claude-opus-5",
    project: "acme/api",
    ready_at: "2026-07-24T17:05:00Z",
    require_approval: false,
    schedule: "nightly-integration",
    state: "Evaluation",
    task_time_ms: 915000,
    timeout: "45m",
    title: "Generate the TypeScript client",
    type: "code",
  } satisfies Job,
  JobSummary: {
    base_ref: "f00dcafe",
    branch: "job/42",
    claim_next: false,
    completed_at: "2026-07-24T19:00:00Z",
    created_at: "2026-07-24T17:00:00Z",
    deps: [41],
    escalation: {
      at: "2026-07-24T18:30:00Z",
      detail: "the work agent failed three attempts",
      failing_task: 3,
      reason: "work_retries_exhausted",
    },
    eval: [
      {
        image: "chuggernaut/agent-rust:latest",
        model: null,
        name: "ci",
        prompt: null,
        provider: null,
        required: true,
        run: ".chug/tasks/ci.sh",
        secrets: ["CI_TOKEN"],
        stage: 1,
        type: "command",
      },
    ],
    factory: null,
    groups: ["design/321-job-groups", "beacon-import"],
    id: 42,
    inputs: {
      service: "web",
      sha: "4f9c1ab",
    },
    knowledge_tags: ["web"],
    members: [],
    model: "claude-opus-5",
    project: "acme/api",
    ready_at: "2026-07-24T17:05:00Z",
    require_approval: false,
    schedule: "nightly-integration",
    state: "Evaluation",
    task_time_ms: 915000,
    timeout: "45m",
    title: "Generate the TypeScript client",
    type: "code",
  } satisfies JobSummary,
  JobType: {
    description: "Implement a ticket.",
    display_name: "Code",
    eval: [
      {
        image: null,
        model: null,
        name: "ci",
        prompt: null,
        provider: null,
        required: null,
        run: ".chug/tasks/ci.sh",
        secrets: [],
        stage: 0,
        type: "command",
      },
    ],
    eval_retries: 1,
    image: "chuggernaut/agent-rust:latest",
    inputs: [],
    job_deadline: "6h",
    knowledge: ["rust"],
    min_dispatcher: 4,
    name: "code",
    placement: null,
    resources: {
      cpu: 2,
      memory: "4g",
      task_timeout: "45m",
    },
    rework_budget: 3,
    runtime: {
      env: "nix:.#chug-ci",
      mode: "container",
    },
    vars: ["BRANCH"],
    work: {
      model: null,
      prompt: ".chug/prompts/work-code.md",
      provider: "claude",
      review: {
        iterations: 2,
        model: null,
        prompt: ".chug/tasks/review-code.md",
        provider: null,
      },
      run: null,
      secrets: ["GH_TOKEN"],
      type: "agent",
    },
    work_retries: 2,
    wrap_up: {
      type: "merge",
    },
  } satisfies JobType,
  QueueSnapshot: {
    depth: 3,
    entries: [
      {
        position: 2,
        queued_at: "2026-07-24T17:06:00Z",
        seq: 42,
        task_id: 3,
      },
    ],
  } satisfies QueueSnapshot,
  Task: {
    attempt: 1,
    completed_at: "2026-07-24T17:22:15Z",
    container_id: "c0ffee",
    created_at: "2026-07-24T17:05:30Z",
    cycle: 1,
    evaluator: null,
    id: 3,
    infra_loss: false,
    job_seq: 42,
    kind: {
      kind: "Agent",
      model: "claude-opus-5",
      prompt: ".chug/prompts/work.md",
      provider: "claude",
    },
    label: "work",
    pending_reason: "QueuedForCapacity",
    performed_by: "human",
    phase: "Work",
    project: "acme/api",
    queued_at: "2026-07-24T17:06:00Z",
    result: {
      cover_html: "<p>cover</p>",
      kind: "Work",
      structured: {
        files_changed: ["web/src/api.ts"],
      },
      summary: "wired the generator",
      token_usage: {
        cache_read_tokens: 9000,
        cache_write_tokens: null,
        input_tokens: 1200,
        output_tokens: 340,
      },
    },
    reviewed_tip: "deadbeef",
    rework_reason: "EvalFailure",
    session_id: "6f1c",
    stage: 0,
    started_at: "2026-07-24T17:07:00Z",
    state: "Done",
  } satisfies Task,
  TaskResolution: {
    action: "Retry",
    kind: "Escalation",
    structured: {
      notes: "retry with a bigger timeout",
    },
  } satisfies TaskResolution,
  TaskResult: {
    abort: true,
    kind: "Agent",
    pass: false,
    structured: {
      findings: [
        {
          file: "web/src/api.ts",
        },
      ],
    },
    token_usage: null,
  } satisfies TaskResult,
};
