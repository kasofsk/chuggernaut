//! `chuggernaut schema api-samples` — one serialized example per §6.2 response
//! type, emitted from real Rust values.
//!
//! The generated TypeScript client (`web/src/api/types.gen.ts`) is produced
//! from `.chug/schemas/api.schema.json`, which schemars derives from these same
//! types. That chain proves the client matches the *schema*; it cannot prove
//! the schema matches what **serde** writes — a `skip_serializing_if` schemars
//! reads differently, an adjacent tag, a chrono format. This file closes that
//! gap: `web/src/api/roundtrip.test.ts` parses these bytes against the
//! generated types, so serde's output is checked against the contract the UI
//! compiles with rather than against a payload a TypeScript author imagined.
//!
//! Samples are representative, not exhaustive: one populated value per response
//! record, with optional fields *set* (an absent field proves nothing about the
//! shape it would have had). `committed_schemas_are_current` holds the emitted
//! file current, exactly as it does the schemas.

use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

/// One JSON document, `{"<$defs name>": <payload>}`, sorted by type name so the
/// emitted bytes are a property of this code and not of map iteration order.
pub fn bundle() -> anyhow::Result<serde_json::Value> {
    let job = sample_job();
    let mut samples: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    samples.insert("Job".into(), serde_json::to_value(&job)?);
    samples.insert(
        "JobSummary".into(),
        serde_json::to_value(types::JobSummary::from(&job))?,
    );
    samples.insert("Task".into(), serde_json::to_value(sample_task())?);
    samples.insert(
        "TaskResult".into(),
        serde_json::to_value(sample_eval_result())?,
    );
    samples.insert(
        "TaskResolution".into(),
        serde_json::to_value(sample_resolution())?,
    );
    samples.insert(
        "QueueSnapshot".into(),
        serde_json::to_value(sample_queue())?,
    );
    samples.insert(
        "GroupEntry".into(),
        serde_json::to_value(sample_group_entry())?,
    );
    samples.insert(
        "DesignEntry".into(),
        serde_json::to_value(sample_design_entry())?,
    );
    samples.insert("FleetStatus".into(), serde_json::to_value(sample_fleet())?);
    samples.insert(
        "DispatcherConfigSnapshot".into(),
        serde_json::to_value(sample_dispatcher_snapshot())?,
    );
    samples.insert("Identity".into(), serde_json::to_value(sample_identity())?);
    samples.insert(
        "DeployReport".into(),
        serde_json::to_value(sample_deploy_report())?,
    );
    samples.insert("JobType".into(), serde_json::to_value(sample_job_type()?)?);
    Ok(serde_json::to_value(samples)?)
}

/// A fixed instant, so the emitted file changes only when a *shape* changes.
fn at(stamp: &str) -> DateTime<Utc> {
    stamp.parse().unwrap_or_default()
}

fn sample_evaluator() -> types::Evaluator {
    types::Evaluator {
        name: "ci".into(),
        r#type: types::EvaluatorType::Command,
        image: Some("chuggernaut/agent-rust:latest".into()),
        run: Some(".chug/tasks/ci.sh".into()),
        prompt: None,
        provider: None,
        model: None,
        secrets: vec!["CI_TOKEN".into()],
        required: Some(true),
        stage: 1,
    }
}

fn sample_job() -> types::Job {
    types::Job {
        id: 42,
        project: "acme/api".into(),
        r#type: "code".into(),
        title: "Generate the TypeScript client".into(),
        description: "Replace the hand-mirrored interfaces.".into(),
        cover_html: Some("<h1>done</h1>".into()),
        deps: vec![41],
        members: vec![],
        batch_id: None,
        state: types::JobState::Evaluation,
        branch: "job/42".into(),
        base_ref: Some("f00dcafe".into()),
        knowledge_tags: vec!["web".into()],
        eval: vec![sample_evaluator()],
        timeout: Some("45m".into()),
        model: Some("claude-opus-5".into()),
        inputs: std::collections::BTreeMap::from([
            ("service".to_string(), "web".to_string()),
            ("sha".to_string(), "4f9c1ab".to_string()),
        ]),
        groups: vec!["design/321-job-groups".into(), "beacon-import".into()],
        claim_next: false,
        escalation: Some(types::Escalation {
            reason: "work_retries_exhausted".into(),
            detail: "the work agent failed three attempts".into(),
            failing_task: Some(3),
            at: at("2026-07-24T18:30:00Z"),
        }),
        factory: None,
        schedule: Some("nightly-integration".into()),
        created_at: at("2026-07-24T17:00:00Z"),
        ready_at: Some(at("2026-07-24T17:05:00Z")),
        completed_at: Some(at("2026-07-24T19:00:00Z")),
        task_time_ms: Some(915_000),
    }
}

fn sample_task() -> types::Task {
    types::Task {
        id: 3,
        job_seq: 42,
        project: "acme/api".into(),
        phase: types::TaskPhase::Work,
        cycle: 1,
        kind: types::TaskKind::Agent {
            provider: "claude".into(),
            model: Some("claude-opus-5".into()),
            prompt: ".chug/prompts/work.md".into(),
        },
        state: types::TaskState::Done,
        attempt: 1,
        evaluator: None,
        label: Some("work".into()),
        stage: 0,
        performed_by: Some(types::Performer::Human),
        container_id: Some("c0ffee".into()),
        pending_reason: Some(types::PendingReason::QueuedForCapacity),
        queued_at: Some(at("2026-07-24T17:06:00Z")),
        rework_reason: Some(types::ReworkReason::EvalFailure),
        infra_loss: false,
        session_id: Some("6f1c".into()),
        reviewed_tip: Some("deadbeef".into()),
        result: Some(types::TaskResult::Work {
            summary: Some("wired the generator".into()),
            structured: Some(serde_json::json!({ "files_changed": ["web/src/api.ts"] })),
            token_usage: Some(types::TokenUsage {
                input_tokens: 1_200,
                output_tokens: 340,
                cache_read_tokens: Some(9_000),
                cache_write_tokens: None,
            }),
            cover_html: Some("<p>cover</p>".into()),
        }),
        created_at: at("2026-07-24T17:05:30Z"),
        started_at: Some(at("2026-07-24T17:07:00Z")),
        completed_at: Some(at("2026-07-24T17:22:15Z")),
    }
}

/// A second [`types::TaskResult`] variant: the union's adjacent tagging is the
/// part most likely to schematize differently from how serde writes it, and one
/// variant cannot demonstrate a discriminated union.
fn sample_eval_result() -> types::TaskResult {
    types::TaskResult::Agent {
        pass: false,
        abort: true,
        structured: Some(serde_json::json!({ "findings": [{ "file": "web/src/api.ts" }] })),
        token_usage: None,
        cover_html: None,
    }
}

fn sample_resolution() -> types::TaskResolution {
    types::TaskResolution::Escalation {
        action: types::EscalationAction::Retry,
        structured: Some(serde_json::json!({ "notes": "retry with a bigger timeout" })),
    }
}

fn sample_queue() -> types::QueueSnapshot {
    types::QueueSnapshot {
        depth: 3,
        entries: vec![types::QueueEntry {
            seq: 42,
            task_id: 3,
            position: 2,
            queued_at: at("2026-07-24T17:06:00Z"),
        }],
    }
}

/// One member's worth of group roll-up, shared by both derived reads so the
/// two samples differ where the endpoints differ and nowhere else.
fn sample_group_rollup() -> types::GroupRollup {
    let mut group = types::GroupRollup::empty("design/321-job-groups".into());
    group.jobs.push(types::GroupJob {
        id: 42,
        r#type: "code".into(),
        title: "slice B: the derived reads".into(),
        state: types::JobState::Done,
    });
    group.counts.insert("Done".into(), 1);
    group
}

/// A `GET .../groups` row with its optional document fields SET — an absent
/// field proves nothing about the shape it would have had.
fn sample_group_entry() -> types::GroupEntry {
    types::GroupEntry {
        group: sample_group_rollup(),
        doc_path: Some("docs/design/321-job-groups.md".into()),
        doc_status: Some("PROPOSED".into()),
    }
}

/// A `GET .../designs` row: the same roll-up, reached from the repo side.
fn sample_design_entry() -> types::DesignEntry {
    types::DesignEntry::new(
        "docs/design/321-job-groups.md".into(),
        "321-job-groups",
        types::DesignDocHead {
            title: Some("Design #321 — Job groups".into()),
            status: Some("PROPOSED".into()),
        },
        sample_group_rollup(),
    )
}

fn sample_refresh_outcome() -> types::worker::RefreshOutcome {
    types::worker::RefreshOutcome {
        accepted_at: at("2026-07-24T16:00:00Z"),
        finished_at: Some(at("2026-07-24T16:04:00Z")),
        result: types::worker::RefreshResult::Failed {
            stage: "swap".into(),
            error_tail: "systemctl: unit not found".into(),
        },
        from_sha: "1111111".into(),
        to_sha: "2222222".into(),
    }
}

fn sample_fleet() -> types::FleetStatus {
    types::FleetStatus {
        nodes: vec![types::FleetNode {
            name: "gumbo-mini-0".into(),
            slots: Some(4),
            occupied: 1,
            available: true,
            version: Some("chuggernaut 0.1.0 (2222222)".into()),
            refresh_outcome: Some(sample_refresh_outcome()),
            capacity_source: Some(types::CapacitySource::Node),
            capacity_observed_at: Some(at("2026-07-24T17:07:00Z")),
            slots_desired: Some(4),
            capacity_state: Some(types::CapacityState::Converged),
            capacity_note: None,
            running: vec![types::SlotOccupant {
                project: "acme/api".into(),
                job_seq: 42,
                task_id: 3,
                task_kind: "work".into(),
                job_type: "code".into(),
                phase: "work".into(),
                started_at: Some(at("2026-07-24T17:07:00Z")),
            }],
        }],
        queue_depth: 3,
    }
}

fn sample_dispatcher_snapshot() -> types::DispatcherConfigSnapshot {
    types::DispatcherConfigSnapshot {
        nodes: vec![types::WorkerNode {
            name: "gumbo-mini-0".into(),
            endpoint: "nats".into(),
            slots: 4,
            available: true,
            version: Some("chuggernaut 0.1.0 (2222222)".into()),
            refresh_outcome: Some(sample_refresh_outcome()),
            capacity_source: Some(types::CapacitySource::Node),
            capacity_observed_at: Some(at("2026-07-24T17:07:00Z")),
        }],
        agent_provider_default: "claude".into(),
        agent_model_default: Some("claude-opus-5".into()),
        triage_image: Some("chuggernaut/agent-claude:latest".into()),
        repos_root: "/var/lib/chuggernaut/repos".into(),
        repo_url_base: "ssh://git@chug.example".into(),
        nats_url: "nats://127.0.0.1:4222".into(),
        nats_url_container: Some("nats://host.docker.internal:4222".into()),
        channel_binary: Some("/usr/local/bin/chug-channel".into()),
        hook_bin: Some("/usr/local/bin/chuggernaut".into()),
        secrets_encryption: true,
        dispatcher_sha: Some("2222222".into()),
        main_tip_sha: Some("3333333".into()),
        commits_behind: Some(2),
        placement_policy: "busyness".into(),
        schema_epoch: types::CONFIG_SCHEMA_EPOCH,
    }
}

fn sample_identity() -> types::Identity {
    types::Identity {
        sub: "operator@acme.example".into(),
        kind: types::IdentityKind::User,
        project_roles: [("acme/api".to_string(), types::ProjectRole::Admin)]
            .into_iter()
            .collect(),
        platform_admin: true,
    }
}

fn sample_deploy_report() -> types::DeployReport {
    types::DeployReport {
        from_sha: Some("1111111".into()),
        to_sha: Some("2222222".into()),
        rollback: false,
        health: Some("ok".into()),
        legs: vec![types::DeployLeg {
            name: "build-dispatcher".into(),
            status: types::LegStatus::Ok,
            secs: Some(93),
            error: None,
            detail: Some("cargo build --release".into()),
        }],
    }
}

/// Parsed from YAML rather than built field by field: [`types::JobType`] is the
/// one covered type whose canonical source *is* a `.chug/jobs/*.yaml` file, and the
/// parser is what the job-type detail endpoint serves through.
fn sample_job_type() -> anyhow::Result<types::JobType> {
    let yaml = r"
name: code
display_name: Code
description: Implement a ticket.
image: chuggernaut/agent-rust:latest
work:
  type: agent
  prompt: .chug/prompts/work-code.md
  provider: claude
  review:
    prompt: .chug/tasks/review-code.md
    iterations: 2
  secrets:
    - GH_TOKEN
wrap_up:
  type: merge
resources:
  cpu: 2
  memory: 4g
  task_timeout: 45m
job_deadline: 6h
work_retries: 2
eval_retries: 1
rework_budget: 3
eval:
  - name: ci
    type: command
    run: .chug/tasks/ci.sh
knowledge:
  - rust
vars:
  - BRANCH
";
    Ok(types::JobType::parse(yaml)?)
}
