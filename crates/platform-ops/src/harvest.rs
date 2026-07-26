//! Pulling artifacts out of a container after it exits.
//!
//! Everything a finished task leaves behind — container logs (carrying the
//! CLI's JSON result), the session transcript, an evaluator's
//! `eval-result.json` — must be copied out *before* the container is removed;
//! [`dispose`](Harvester::dispose) reclaims its overlay layer once that's done.
//! A cargo-building job's overlay is 5–10 GB, so leaving exited containers
//! around fills the host disk (the 2026-07-21 outage).
//!
//! Runs on a clone of the handles rather than on the dispatcher's core because
//! collection happens inside the per-task `tokio::spawn`, off the actor thread.
//! Nothing here writes job or task state — it returns what it found and lets
//! the actor record it, preserving the single-writer rule.
//!
//! - **Accepts:** an exited container's handles (logs, session transcript,
//!   `eval-result.json`).
//! - **Emits:** the harvested artifacts back to the actor; overlay-layer
//!   disposal (`dispose`) once collection is done.
//! - **Guarantees:** runs off the actor thread on cloned handles; writes no
//!   job/task state (single-writer preserved).
//! - **Spec:** §3.2 (container removal after harvest); §3.6 (crash sweep).

use agent::AgentOutput;
use container::ContainerBackend;
use std::sync::Arc;
use store::{ArtifactKind, ArtifactStore};
use types::TokenUsage;

/// Handles needed to collect artifacts, cloneable into a spawned task.
#[derive(Clone)]
pub struct Harvester {
    backend: Arc<dyn ContainerBackend>,
    artifacts: Option<Arc<ArtifactStore>>,
}

impl Harvester {
    pub fn new(backend: Arc<dyn ContainerBackend>, artifacts: Option<Arc<ArtifactStore>>) -> Self {
        Self { backend, artifacts }
    }

    /// Collect everything an agent run left behind: container logs (the CLI's
    /// `--output-format stream-json` event stream, whose final `type:"result"`
    /// event carries usage/result) and the session transcript.
    /// Returns measured token usage if the result object was parseable.
    ///
    /// Best-effort throughout: a job must never fail because its *reporting*
    /// failed. Every miss is logged, since silent absence is indistinguishable
    /// from "the agent produced nothing".
    pub async fn collect(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        out: &AgentOutput,
    ) -> Option<TokenUsage> {
        self.collect_agent(owner, project, seq, task_id, out)
            .await
            .1
    }

    /// Full agent-run collection: stores stdout + transcript and returns both
    /// the CLI's final **result text** and measured token usage. Work and eval
    /// runs only need the usage (via [`collect`]); triage (spec §1.2) also needs
    /// the result text — it runs without the channel MCP, so the CLI's own JSON
    /// result on stdout is the only channel for the assessment.
    pub async fn collect_agent(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        out: &AgentOutput,
    ) -> (Option<String>, Option<TokenUsage>) {
        let Some(id) = &out.container_id else {
            return (None, None); // provider without a container (fakes, stubs)
        };

        let logs = match self.backend.logs(id).await {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                tracing::warn!("job {seq} task {task_id}: no container logs: {e}");
                None
            }
        };

        // Both come from the logs, not the transcript: the CLI's JSON result is
        // a documented interface, while the transcript format is internal and
        // version-unstable.
        let usage = logs.as_deref().and_then(agent::claude::parse_usage);
        if usage.is_none() {
            tracing::debug!("job {seq} task {task_id}: no usage in agent stdout");
        }
        let result = logs.as_deref().and_then(agent::claude::parse_result);

        // Permission denials are the feedback loop on the §4.3 profiles: a
        // too-tight policy degrades an agent silently (it is told "denied" and
        // carries on with less), so an unreported denial shows up only as
        // mysteriously worse work. WARN — an agent hitting its policy is either
        // a prompt that asks for the wrong thing or a profile that needs
        // widening, and both want a human to look.
        let denials = logs
            .as_deref()
            .map(agent::claude::parse_permission_denials)
            .unwrap_or_default();
        if !denials.is_empty() {
            tracing::warn!(
                "job {seq} task {task_id}: {} tool call(s) denied by the permission profile: {}",
                denials.len(),
                denials.join("; ")
            );
        }

        if let Some(bytes) = logs {
            self.store(owner, project, seq, task_id, ArtifactKind::Stdout, &bytes)
                .await;
        }

        if let Some(session_id) = &out.session_id {
            let path = agent::transcript_path(session_id);
            match self.backend.copy_file(id, &path).await {
                Ok(Some(bytes)) => {
                    self.store(
                        owner,
                        project,
                        seq,
                        task_id,
                        ArtifactKind::SessionTranscript,
                        &bytes,
                    )
                    .await;
                }
                // Absent whenever the CLI never started — a bad image, a failed
                // clone. Worth a line: it also fires if the CLI ever changes
                // where it writes transcripts.
                Ok(None) => tracing::warn!(
                    "job {seq} task {task_id}: no transcript at {path} \
                     (agent may not have started)"
                ),
                Err(e) => tracing::warn!("job {seq} task {task_id}: transcript copy failed: {e}"),
            }
        }
        (result, usage)
    }

    /// Collect just the logs, for a container the dispatcher launched itself
    /// (command work and command evals). Returns the harvested bytes so a caller
    /// that also needs the output inline (e.g. the gate monitor threading a
    /// failing stage's compiler stderr into the gate-fix brief, job #154) does
    /// not have to fetch them a second time.
    pub async fn collect_logs(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        id: &container::ContainerId,
    ) -> Option<Vec<u8>> {
        match self.backend.logs(id).await {
            Ok(bytes) => {
                self.store(owner, project, seq, task_id, ArtifactKind::Stdout, &bytes)
                    .await;
                Some(bytes)
            }
            Err(e) => {
                tracing::warn!("job {seq} task {task_id}: no container logs: {e}");
                None
            }
        }
    }

    /// Remove a finished container after its artifacts are captured (spec §3.1:
    /// the container lifecycle ends in removal). This is the disk-leak fix —
    /// each work/eval overlay holds a full `/workspace/target` build. Called
    /// last, once every `logs`/`copy_file` read is done. Best-effort: a failed
    /// removal leaks disk but must never fail a job, so it only warns.
    pub async fn dispose(&self, seq: u64, task_id: u64, id: &container::ContainerId) {
        if let Err(e) = self.backend.remove(id).await {
            tracing::warn!("job {seq} task {task_id}: removing container {id} failed: {e}");
        }
    }

    async fn store(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        kind: ArtifactKind,
        bytes: &[u8],
    ) {
        let Some(artifacts) = &self.artifacts else {
            return; // artifact storage not configured
        };
        if bytes.is_empty() {
            return;
        }
        if let Err(e) = artifacts
            .put(owner, project, seq, task_id, kind, bytes)
            .await
        {
            tracing::warn!(
                "job {seq} task {task_id}: storing {} failed: {e}",
                kind.as_str()
            );
        }
    }
}

/// Parse the structured deploy report a command work task emits on stdout
/// (ticket #187). `update.sh` prints one `@chug:leg {json}` line per deploy leg
/// plus a single `@chug:report {json}` envelope; this harvests them from the
/// captured container logs — the same bytes [`Harvester::collect_logs`] stores —
/// into a [`types::DeployReport`] for the task's structured result. Generic to
/// command work: any job type could emit legs, but a deploy is the consumer that
/// matters.
///
/// Forgiving like every other harvest: a malformed marker line is skipped rather
/// than failing the whole report, and non-marker output is ignored untouched.
/// Returns `None` when no marker line was present, so an ordinary command task's
/// result is left exactly as it was.
pub fn parse_deploy_report(logs: &str) -> Option<types::DeployReport> {
    use types::deploy::{LEG_MARKER, REPORT_MARKER};
    let mut report = types::DeployReport::default();
    let mut found = false;
    for line in logs.lines() {
        let line = line.trim();
        // Order matters only in that neither marker is a prefix of the other.
        if let Some(rest) = line.strip_prefix(LEG_MARKER) {
            if let Ok(leg) = serde_json::from_str::<types::DeployLeg>(rest.trim()) {
                report.legs.push(leg);
                found = true;
            }
        } else if let Some(rest) = line.strip_prefix(REPORT_MARKER) {
            // The single deploy envelope: from/to SHAs, rollback, health. Parsed
            // into a DeployReport (its `legs` default empty) so we reuse one type.
            if let Ok(env) = serde_json::from_str::<types::DeployReport>(rest.trim()) {
                report.from_sha = env.from_sha.or(report.from_sha.take());
                report.to_sha = env.to_sha.or(report.to_sha.take());
                report.rollback = env.rollback;
                report.health = env.health.or(report.health.take());
                found = true;
            }
        }
    }
    found.then_some(report)
}

/// A misconfiguration that stops the artifact store being built at all — as
/// opposed to the best-effort misses everything else here logs. Its own type
/// rather than the dispatcher's `CoreError` so the context owes the lifecycle
/// crate nothing; the caller classifies it (today: a startup config error).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ArtifactStoreError(String);

/// Build the artifact store from the dispatcher's `age_artifacts` identity.
/// `None` disables capture rather than failing startup — a platform without the
/// key still runs jobs, it just keeps no transcripts.
pub async fn artifact_store(
    store: &store::NatsStore,
    identity: Option<&str>,
) -> Result<Option<Arc<ArtifactStore>>, ArtifactStoreError> {
    let Some(identity) = identity else {
        tracing::warn!("no age_artifacts identity: transcripts and logs will not be captured");
        return Ok(None);
    };
    let crypto = store::ArtifactCrypto::with_identity(identity)
        .map_err(|e| ArtifactStoreError(format!("age_artifacts identity: {e}")))?;
    let handle = store
        .artifacts(crypto)
        .await
        .map_err(|e| ArtifactStoreError(format!("artifacts object store: {e}")))?;
    Ok(Some(Arc::new(handle)))
}
