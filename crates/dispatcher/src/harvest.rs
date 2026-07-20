//! Pulling artifacts out of a container after it exits.
//!
//! Containers are never removed, so their filesystem and logs survive — but
//! only while something remembers the container id. Historically the agent path
//! threw that id away, which is why session transcripts were unrecoverable in
//! practice despite still sitting on the node.
//!
//! Runs on a clone of the handles rather than `&Core` because collection
//! happens inside the per-task `tokio::spawn`, off the actor thread. Nothing
//! here writes job or task state — it returns what it found and lets the actor
//! record it, preserving the single-writer rule.

use crate::core::CoreError;
use agent::AgentOutput;
use container::ContainerBackend;
use std::sync::Arc;
use store::{ArtifactKind, ArtifactStore};
use types::TokenUsage;

/// Handles needed to collect artifacts, cloneable into a spawned task.
#[derive(Clone)]
pub(crate) struct Harvester {
    backend: Arc<dyn ContainerBackend>,
    artifacts: Option<Arc<ArtifactStore>>,
}

impl Harvester {
    pub(crate) fn new(
        backend: Arc<dyn ContainerBackend>,
        artifacts: Option<Arc<ArtifactStore>>,
    ) -> Self {
        Self { backend, artifacts }
    }

    /// Collect everything an agent run left behind: container logs (which carry
    /// the CLI's `--output-format json` result) and the session transcript.
    /// Returns measured token usage if the result object was parseable.
    ///
    /// Best-effort throughout: a job must never fail because its *reporting*
    /// failed. Every miss is logged, since silent absence is indistinguishable
    /// from "the agent produced nothing".
    pub(crate) async fn collect(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        out: &AgentOutput,
    ) -> Option<TokenUsage> {
        self.collect_agent(owner, project, seq, task_id, out).await.1
    }

    /// Full agent-run collection: stores stdout + transcript and returns both
    /// the CLI's final **result text** and measured token usage. Work and eval
    /// runs only need the usage (via [`collect`]); triage (spec §1.2) also needs
    /// the result text — it runs without the channel MCP, so the CLI's own JSON
    /// result on stdout is the only channel for the assessment.
    pub(crate) async fn collect_agent(
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
    /// (command work and command evals).
    pub(crate) async fn collect_logs(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        id: &container::ContainerId,
    ) {
        match self.backend.logs(id).await {
            Ok(bytes) => {
                self.store(owner, project, seq, task_id, ArtifactKind::Stdout, &bytes)
                    .await
            }
            Err(e) => tracing::warn!("job {seq} task {task_id}: no container logs: {e}"),
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

/// Build the artifact store from the dispatcher's `age_artifacts` identity.
/// `None` disables capture rather than failing startup — a platform without the
/// key still runs jobs, it just keeps no transcripts.
pub(crate) async fn artifact_store(
    store: &store::NatsStore,
    identity: Option<&str>,
) -> Result<Option<Arc<ArtifactStore>>, CoreError> {
    let Some(identity) = identity else {
        tracing::warn!("no age_artifacts identity: transcripts and logs will not be captured");
        return Ok(None);
    };
    let crypto = store::ArtifactCrypto::with_identity(identity)
        .map_err(|e| CoreError::Config(format!("age_artifacts identity: {e}")))?;
    let handle = store
        .artifacts(crypto)
        .await
        .map_err(|e| CoreError::Config(format!("artifacts object store: {e}")))?;
    Ok(Some(Arc::new(handle)))
}
