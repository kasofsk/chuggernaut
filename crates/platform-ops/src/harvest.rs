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
//!   `eval-result.json`, a work container's `chug-output.tar.gz`).
//! - **Emits:** the harvested artifacts back to the actor, plus a
//!   `transcript-missing` marker whenever a named session leaves no stored
//!   transcript (design #490 D1b); overlay-layer disposal (`dispose`) once collection is
//!   done; revoke-time deletion of a job's outputs (`delete_outputs`).
//! - **Guarantees:** runs off the actor thread on cloned handles; writes no
//!   job/task state (single-writer preserved).
//! - **Spec:** §3.2 (container removal after harvest); §3.6 (crash sweep).

use agent::AgentOutput;
use container::ContainerBackend;
use std::sync::Arc;
use store::{ArtifactKind, ArtifactStore};
use types::TokenUsage;

/// The one well-known path a work container leaves an output archive at
/// (design #362 Decision 2). A convention, not a schema field: no `outputs:`
/// declaration, no config epoch, and the producing script decides what goes in
/// it and whether to write it at all.
pub const OUTPUT_PATH: &str = "/workspace/chug-output.tar.gz";

/// The escalation reason design #490 D1b names for a session the agent reported
/// and whose transcript the harvest could not resolve.
pub const TRANSCRIPT_UNRESOLVED: &str = "transcript_unresolved";

/// Whether D1b's escalation is **armed**, which it is not: M6 — can a
/// legitimate run name a session and leave no resolvable transcript — is
/// unanswered, and slice 1's repair of the one known cause has not reached
/// production yet. Flipping this is a later slice's decision, not a
/// configuration knob.
const ESCALATION_ARMED: bool = false;

/// Which of design #490 D1b's branches refused a transcript the run had named,
/// and the context an operator needs to see why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissBranch {
    /// Resolution answered, and no file under the searched directory carries
    /// the session's name.
    Zero,
    /// Resolution answered with several files for one session id, which has no
    /// representable answer: the store keys one transcript per task.
    Several,
    /// Resolution itself failed, on a launch the computed-path degrade
    /// ([`Harvester::computed_fallback`]) is not available to.
    Unresolvable,
    /// Resolution named exactly one file and the copy did not produce it: the
    /// over-band refusal, a transport miss, or a path gone by the read.
    Uncopied,
}

impl MissBranch {
    pub fn as_str(self) -> &'static str {
        match self {
            MissBranch::Zero => "zero",
            MissBranch::Several => "several",
            MissBranch::Unresolvable => "unresolvable",
            MissBranch::Uncopied => "uncopied",
        }
    }
}

/// A transcript the agent named and the harvest did not store. Returned rather
/// than acted on, because the Harvester holds no `&mut Core` and a job must
/// never fail because its reporting failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMiss {
    pub branch: MissBranch,
    pub session_id: String,
    /// The directory that was searched — `{CLAUDE_CONFIG_DIR}/projects`.
    pub dir: String,
    /// Every path resolution returned: several for [`MissBranch::Several`], the
    /// one resolved path for [`MissBranch::Uncopied`], none otherwise.
    pub paths: Vec<String>,
    /// What failed, which [`MissBranch::Unresolvable`] and
    /// [`MissBranch::Uncopied`] have — the latter only when the copy errored
    /// rather than finding the path gone.
    pub error: Option<String>,
}

impl TranscriptMiss {
    /// Whether resolution answered and **refused** — D1b's two cases, which it
    /// calls a platform break rather than an empty run. A `find_file` that
    /// never answered is an ordinary reporting miss and stays a warning.
    pub fn is_refusal(&self) -> bool {
        matches!(self.branch, MissBranch::Zero | MissBranch::Several)
    }

    /// Whether the platform dropped a record it had already resolved, which
    /// only the size-band refusal does: a transcript past the artifact store's
    /// blob ceiling has nowhere to be kept, and design #490 exists because that
    /// loss used to be silent.
    pub fn is_lost(&self) -> bool {
        self.branch == MissBranch::Uncopied
            && self
                .error
                .as_deref()
                .is_some_and(|e| e.contains(types::worker::COPY_FILE_TOO_LARGE))
    }

    /// The one sentence both the log line and the marker artifact carry, so an
    /// operator reading either sees the same account of what happened.
    pub fn detail(&self) -> String {
        let name = agent::transcript_name(&self.session_id);
        let dir = &self.dir;
        match self.branch {
            MissBranch::Zero => format!(
                "the agent reported session {} and no file named {name} exists under {dir}",
                self.session_id
            ),
            MissBranch::Several => format!(
                "{} files are named {name} under {dir} ({}) — one session id names one \
                 transcript, so none is stored",
                self.paths.len(),
                self.paths.join(", ")
            ),
            MissBranch::Unresolvable => format!(
                "resolving {name} under {dir} failed: {}",
                self.error.as_deref().unwrap_or("unknown error")
            ),
            MissBranch::Uncopied => self.detail_uncopied(),
        }
    }

    /// The [`MissBranch::Uncopied`] sentence, which has three shapes: the
    /// size-band refusal names the ceiling and the loss, any other copy error
    /// names itself, and no error at all means the path was gone by the read.
    fn detail_uncopied(&self) -> String {
        let path = self
            .paths
            .first()
            .map(String::as_str)
            .unwrap_or("the session transcript");
        match self.error.as_deref() {
            Some(e) if self.is_lost() => format!(
                "the session transcript {path} was NOT stored: {e} — a transcript over {} bytes \
                 exceeds the artifact store's blob ceiling and this run's record is lost",
                store::MAX_BLOB_BYTES
            ),
            Some(e) => format!("copying the session transcript {path} failed: {e}"),
            None => format!("{path} resolved and was gone by the read"),
        }
    }

    /// The marker artifact's body (design #490 D1b): which branch fired, the
    /// session it fired for, and what was searched. JSON so the residual miss
    /// rate M6 asks for is countable straight out of the artifact store.
    pub fn marker(&self) -> Vec<u8> {
        let mut doc = serde_json::json!({
            "branch": self.branch.as_str(),
            "session_id": self.session_id,
            "dir": self.dir,
            "file": agent::transcript_name(&self.session_id),
            "detail": self.detail(),
        });
        if let Some(map) = doc.as_object_mut() {
            if self.is_refusal() {
                map.insert("reason".into(), TRANSCRIPT_UNRESOLVED.into());
            }
            if self.is_lost() {
                map.insert("lost".into(), true.into());
            }
            if !self.paths.is_empty() {
                map.insert("paths".into(), self.paths.clone().into());
            }
            if let Some(error) = &self.error {
                map.insert("error".into(), error.clone().into());
            }
        }
        serde_json::to_vec_pretty(&doc).unwrap_or_default()
    }

    /// Design #490 D1b's escalation, **staged**: the reason code and detail the
    /// dispatcher would escalate with, and `None` while [`ESCALATION_ARMED`] is
    /// false — so this outcome changes no job's state today.
    pub fn escalation(&self) -> Option<(&'static str, String)> {
        (ESCALATION_ARMED && self.is_refusal()).then(|| (TRANSCRIPT_UNRESOLVED, self.detail()))
    }
}

/// What an agent run's harvest found: the CLI's final result text, its measured
/// token usage, and whether the transcript the run named went unstored.
#[derive(Debug, Default)]
pub struct AgentHarvest {
    /// The CLI's own JSON result text, when it was parseable.
    pub result: Option<String>,
    /// Token usage from that same result event.
    pub usage: Option<TokenUsage>,
    /// Design #490 D1b's outcome, for the dispatcher to decide on once
    /// [`TranscriptMiss::escalation`] is armed.
    pub transcript_miss: Option<TranscriptMiss>,
}

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

    /// Collect everything an agent run left behind — container logs (the CLI's
    /// `--output-format stream-json` stream, whose final `type:"result"` event
    /// carries the usage and the result text) and the session transcript.
    ///
    /// Best-effort throughout: a job must never fail because its *reporting*
    /// failed, so every miss is logged rather than raised.
    pub async fn collect_agent(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        out: &AgentOutput,
    ) -> AgentHarvest {
        let Some(id) = &out.container_id else {
            return AgentHarvest::default();
        };

        let logs = match self.backend.logs(id).await {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                tracing::warn!("job {seq} task {task_id}: no container logs: {e}");
                None
            }
        };

        let usage = logs.as_deref().and_then(agent::claude::parse_usage);
        if usage.is_none() {
            tracing::debug!("job {seq} task {task_id}: no usage in agent stdout");
        }
        let result = logs.as_deref().and_then(agent::claude::parse_result);

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

        let mut transcript_miss = None;
        if let Some(session_id) = &out.session_id {
            transcript_miss = self
                .collect_transcript(owner, project, seq, task_id, id, session_id)
                .await;
        }
        if let Some(miss) = &transcript_miss {
            self.report_transcript_miss(owner, project, seq, task_id, miss)
                .await;
        }
        AgentHarvest {
            result,
            usage,
            transcript_miss,
        }
    }

    /// Make a miss loud (design #490 D1b): D1b's two refusing branches and the
    /// over-band loss log at **error** while an ordinary reporting miss stays a
    /// warning, and every branch leaves a marker artifact so the absence is
    /// readable from the job's artifact list rather than from a node's logs.
    async fn report_transcript_miss(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        miss: &TranscriptMiss,
    ) {
        let message = format!("job {seq} task {task_id}: {}", miss.detail());
        if miss.is_refusal() || miss.is_lost() {
            tracing::error!("{message}");
        } else {
            tracing::warn!("{message}");
        }
        self.store(
            owner,
            project,
            seq,
            task_id,
            ArtifactKind::TranscriptMissing,
            &miss.marker(),
        )
        .await;
    }

    /// Resolve the run's transcript by the session id the platform supplied
    /// (design #490 D1) and store it, read in bounded slices so a long
    /// session's record survives `copy_file`'s single-reply bound.
    ///
    /// Best-effort like the rest of the harvest: every way of ending with no
    /// stored transcript is **returned** as a [`TranscriptMiss`], never raised
    /// here — the Harvester holds no `&mut Core` (design #490 D1b).
    async fn collect_transcript(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        id: &container::ContainerId,
        session_id: &str,
    ) -> Option<TranscriptMiss> {
        let dir = agent::transcript_dir();
        let name = agent::transcript_name(session_id);
        let miss = |branch, paths: Vec<String>, error: Option<String>| {
            Some(TranscriptMiss {
                branch,
                session_id: session_id.to_string(),
                dir: dir.clone(),
                paths,
                error,
            })
        };
        let resolved = match self.backend.find_file(id, &dir, &name).await {
            Ok(paths) => paths,
            Err(e) => {
                let Some(computed) = Self::computed_fallback(id, session_id, &e.to_string()) else {
                    return miss(MissBranch::Unresolvable, vec![], Some(e.to_string()));
                };
                tracing::warn!(
                    "job {seq} task {task_id}: this node does not know find_file ({e}), so the \
                     transcript was read from the computed path {computed} instead"
                );
                vec![computed]
            }
        };
        let path = match resolved.as_slice() {
            [only] => only.clone(),
            [] => return miss(MissBranch::Zero, vec![], None),
            several => return miss(MissBranch::Several, several.to_vec(), None),
        };
        match self
            .backend
            .copy_file_chunked(id, &path, store::MAX_BLOB_BYTES)
            .await
        {
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
                None
            }
            Ok(None) => miss(MissBranch::Uncopied, vec![path], None),
            Err(e) => miss(MissBranch::Uncopied, vec![path], Some(e.to_string())),
        }
    }

    /// Design #490 D1a's degrade: a daemon that does not know `find_file`
    /// answers `unknown op`, and today's computed path is right on the container
    /// node every such daemon is.
    ///
    /// Never taken for a **host** task: its cwd is under the node's host root,
    /// and the CLI slugifies a resolved realpath (job #492), so a computed path
    /// there would find nothing silently.
    fn computed_fallback(
        id: &container::ContainerId,
        session_id: &str,
        error: &str,
    ) -> Option<String> {
        (error.contains(types::worker::UNKNOWN_OP) && !container::host::names_host_task(id))
            .then(|| agent::transcript_path(session_id))
    }

    /// Collect the output archive a **work-side** container left at
    /// [`OUTPUT_PATH`] (spec §3.2): harvested if present, before `dispose`,
    /// absent without complaint, over-band refused without storing.
    ///
    /// Its own method rather than a line in
    /// [`collect_agent`](Harvester::collect_agent), which the agent-work and
    /// agent-eval paths share — the scope rule is spec §3.2's.
    pub async fn collect_output(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        id: &container::ContainerId,
    ) {
        let bytes = match self
            .backend
            .copy_file_chunked(id, OUTPUT_PATH, store::MAX_BLOB_BYTES)
            .await
        {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return,
            Err(e) => {
                let (actionable, message) = Self::output_copy_failure(seq, task_id, &e.to_string());
                if actionable {
                    tracing::error!("{message}");
                } else {
                    tracing::warn!("{message}");
                }
                return;
            }
        };
        let Some(artifacts) = &self.artifacts else {
            return;
        };
        if let Err(e) = artifacts
            .put(owner, project, seq, task_id, ArtifactKind::Output, &bytes)
            .await
        {
            tracing::warn!(
                "job {seq} task {task_id}: storing {OUTPUT_PATH} failed: {e} — the outputs \
                 bucket may be at its byte ceiling, which refuses new outputs until older \
                 ones age out (raise CHUG_OUTPUTS_MAX_BYTES)"
            );
        }
    }

    /// The log line for an output copy that failed, and whether it names an
    /// operator action. Only the size-band refusal does — a transport miss or an
    /// N-1 worker that does not know the chunked op is an ordinary reporting
    /// miss, and telling its operator to move the output to a bucket would send
    /// them to the wrong action.
    fn output_copy_failure(seq: u64, task_id: u64, error: &str) -> (bool, String) {
        if error.contains(types::worker::COPY_FILE_TOO_LARGE) {
            return (
                true,
                format!(
                    "job {seq} task {task_id}: {OUTPUT_PATH} was NOT stored: {error} — an output \
                     over {} bytes belongs in a bucket, not in the artifact store",
                    store::MAX_BLOB_BYTES
                ),
            );
        }
        (
            false,
            format!("job {seq} task {task_id}: {OUTPUT_PATH} could not be read: {error}"),
        )
    }

    /// Drop a job's outputs when it is revoked (design #362 R2). Best-effort and
    /// outputs-only: a revoked job is still an audit record, so its transcripts,
    /// stdout and attachments stay.
    pub async fn delete_outputs(&self, owner: &str, project: &str, seq: u64) {
        let Some(artifacts) = &self.artifacts else {
            return;
        };
        match artifacts.delete_outputs_for_job(owner, project, seq).await {
            Ok(0) => {}
            Ok(n) => tracing::info!("job {seq}: revoked — deleted {n} output archive(s)"),
            Err(e) => tracing::warn!("job {seq}: deleting outputs on revoke failed: {e}"),
        }
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
            return;
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
        if let Some(rest) = line.strip_prefix(LEG_MARKER) {
            if let Ok(leg) = serde_json::from_str::<types::DeployLeg>(rest.trim()) {
                report.legs.push(leg);
                found = true;
            }
        } else if let Some(rest) = line.strip_prefix(REPORT_MARKER)
            && let Ok(env) = serde_json::from_str::<types::DeployReport>(rest.trim())
        {
            report.from_sha = env.from_sha.or(report.from_sha.take());
            report.to_sha = env.to_sha.or(report.to_sha.take());
            report.rollback = env.rollback;
            report.health = env.health.or(report.health.take());
            found = true;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The harvest asks for a **wire** path (design #322 §2): a host backend
    /// rebases it into the task directory, and a path outside the two prefixes
    /// is refused there rather than read off the node.
    #[test]
    fn the_output_archive_is_addressed_by_a_wire_path() {
        assert!(
            OUTPUT_PATH.starts_with(&format!("{}/", container::WIRE_WORKSPACE)),
            "{OUTPUT_PATH} is unmappable on a host node"
        );
    }

    /// An agent run whose container is `id` and whose CLI reported `session`.
    fn agent_run(id: &str, session: &str) -> AgentOutput {
        AgentOutput {
            exit_code: 0,
            container_id: Some(id.to_string()),
            session_id: Some(session.to_string()),
        }
    }

    /// Design #490 D1a's degrade, which is the whole reason the additive op
    /// needs no `WORKER_RPC_VERSION` bump — and it is scoped: an unrefreshed
    /// node is a **container** node whose computed path is right, where a host
    /// task's cwd is under the host root and the CLI slugifies a resolved
    /// realpath (job #492), so computing there would find nothing silently.
    #[test]
    fn the_computed_path_is_a_fallback_for_a_container_node_only() {
        let unknown = format!(
            "{} Some(\"find_file\") on chug.worker.w1.find_file",
            types::worker::UNKNOWN_OP
        );
        assert_eq!(
            Harvester::computed_fallback(&"w1/c0ffee".to_string(), "s-1", &unknown),
            Some(agent::transcript_path("s-1"))
        );
        assert_eq!(
            Harvester::computed_fallback(&"w1/host-1-0".to_string(), "s-1", &unknown),
            None,
            "a host task's computed path is wrong, so the degrade is not available to it"
        );
        for other in [
            "node w1 unreachable",
            "worker transport for w1/c1: timed out",
        ] {
            assert_eq!(
                Harvester::computed_fallback(&"w1/c0ffee".to_string(), "s-1", other),
                None,
                "only a daemon that does not know the op degrades: {other}"
            );
        }
    }

    /// The resolution the slice exists for (design #490 D1): the transcript is
    /// read at the path `find_file` returned, and the CLI's directory slug is
    /// never computed on that path.
    #[tokio::test]
    async fn a_resolved_transcript_is_read_where_it_was_found() {
        let backend = std::sync::Arc::new(test_utils::FakeBackend::new());
        let resolved = format!(
            "{}/-Users-ci-host-tasks-t-1-workspace/{}",
            agent::transcript_dir(),
            agent::transcript_name("s-1")
        );
        backend.put_file(&resolved, b"{\"type\":\"summary\"}".to_vec());

        let harvested = Harvester::new(backend.clone(), None)
            .collect_agent("acme", "chug", 7, 1, &agent_run("w1/c0ffee", "s-1"))
            .await;
        assert_eq!(
            backend.copied(),
            vec![resolved],
            "the transcript is read where it resolved, never at a computed path"
        );
        assert_eq!(
            harvested.transcript_miss, None,
            "a stored transcript leaves no transcript-missing marker"
        );
    }

    /// The same run against a daemon that answers `unknown op`: the transcript
    /// still arrives, from the computed path.
    #[tokio::test]
    async fn a_daemon_without_find_file_still_yields_a_transcript() {
        let backend = std::sync::Arc::new(test_utils::FakeBackend::new());
        let computed = agent::transcript_path("s-1");
        backend.put_file(&computed, b"{\"type\":\"summary\"}".to_vec());
        backend.fail_find_file(format!(
            "{} Some(\"find_file\") on chug.worker.w1.find_file",
            types::worker::UNKNOWN_OP
        ));

        let harvested = Harvester::new(backend.clone(), None)
            .collect_agent("acme", "chug", 7, 1, &agent_run("w1/c0ffee", "s-1"))
            .await;
        assert_eq!(backend.copied(), vec![computed]);
        assert_eq!(
            harvested.transcript_miss, None,
            "a degrade is not a miss: the transcript arrived"
        );
    }

    /// The marker artifact's body, parsed the way a survey of the residual miss
    /// rate (design #490 M6) would parse it.
    fn marker_of(miss: &TranscriptMiss) -> serde_json::Value {
        serde_json::from_slice(&miss.marker()).expect("the marker is JSON")
    }

    /// Zero and several read nothing — picking one of several would be the guess
    /// D1 removed, and there is nothing to pick from zero — and both are
    /// **refusals** (design #490 D1b): logged at error, with a marker naming
    /// which branch fired.
    #[tokio::test]
    async fn zero_and_several_read_nothing_and_leave_a_refusal_marker() {
        let backend = std::sync::Arc::new(test_utils::FakeBackend::new());
        let harvested = Harvester::new(backend.clone(), None)
            .collect_agent("acme", "chug", 7, 1, &agent_run("w1/c0ffee", "s-1"))
            .await;
        assert!(
            backend.copied().is_empty(),
            "nothing resolved, nothing read"
        );
        let miss = harvested.transcript_miss.expect("zero matches is a miss");
        assert_eq!(miss.branch, MissBranch::Zero);
        assert!(miss.is_refusal(), "zero is a platform break, not a warning");
        let marker = marker_of(&miss);
        assert_eq!(marker["branch"], "zero");
        assert_eq!(marker["session_id"], "s-1");
        assert_eq!(marker["dir"], agent::transcript_dir());
        assert_eq!(marker["file"], agent::transcript_name("s-1"));
        assert_eq!(marker["reason"], TRANSCRIPT_UNRESOLVED);
        assert!(marker.get("paths").is_none());

        let several: Vec<String> = ["-workspace", "-elsewhere"]
            .iter()
            .map(|slug| {
                format!(
                    "{}/{slug}/{}",
                    agent::transcript_dir(),
                    agent::transcript_name("s-2")
                )
            })
            .collect();
        for path in &several {
            backend.put_file(path, b"a session".to_vec());
        }
        let harvested = Harvester::new(backend.clone(), None)
            .collect_agent("acme", "chug", 7, 2, &agent_run("w1/c0ffee", "s-2"))
            .await;
        assert!(
            backend.copied().is_empty(),
            "several matches for one session id is refused, not resolved by picking one"
        );
        let miss = harvested
            .transcript_miss
            .expect("several matches is a miss");
        assert_eq!(miss.branch, MissBranch::Several);
        assert!(miss.is_refusal());
        let marker = marker_of(&miss);
        assert_eq!(marker["branch"], "several");
        assert_eq!(marker["reason"], TRANSCRIPT_UNRESOLVED);
        let mut named: Vec<String> = marker["paths"]
            .as_array()
            .expect("the several case names what it found")
            .iter()
            .filter_map(|p| p.as_str().map(String::from))
            .collect();
        named.sort();
        let mut expected = several.clone();
        expected.sort();
        assert_eq!(
            named, expected,
            "every match is named, so none is guessed at"
        );
    }

    /// A resolution that never answered is the third branch: the marker records
    /// it, carrying the error, but it is a reporting miss rather than one of
    /// D1b's refusals — a host task on an N-1 daemon has no computed-path
    /// degrade, and an unreachable node is not a platform break.
    #[tokio::test]
    async fn a_resolution_that_never_answered_is_marked_but_is_not_a_refusal() {
        let backend = std::sync::Arc::new(test_utils::FakeBackend::new());
        backend.fail_find_file("node w1 unreachable");

        let harvested = Harvester::new(backend.clone(), None)
            .collect_agent("acme", "chug", 7, 1, &agent_run("w1/c0ffee", "s-3"))
            .await;
        let miss = harvested
            .transcript_miss
            .expect("a resolution that failed is a miss");
        assert_eq!(miss.branch, MissBranch::Unresolvable);
        assert!(!miss.is_refusal());
        let marker = marker_of(&miss);
        assert_eq!(marker["branch"], "unresolvable");
        assert!(
            marker["error"]
                .as_str()
                .is_some_and(|e| e.contains("node w1 unreachable")),
            "the marker carries the resolution error: {marker}"
        );
        assert!(
            marker.get("reason").is_none(),
            "D1b's reason code belongs to the two refusing branches only"
        );
    }

    /// Design #490 D1b stages the escalation behind M6, and M6 is unanswered —
    /// so no branch escalates, and this diff cannot change a job's state. The
    /// reason code is written down for the slice that arms it.
    #[test]
    fn the_escalation_is_written_and_unarmed() {
        for branch in [
            MissBranch::Zero,
            MissBranch::Several,
            MissBranch::Unresolvable,
            MissBranch::Uncopied,
        ] {
            let miss = TranscriptMiss {
                branch,
                session_id: "s-1".into(),
                dir: agent::transcript_dir(),
                paths: vec![],
                error: None,
            };
            assert_eq!(
                miss.escalation(),
                None,
                "{} escalates only once M6 says a legitimate run never produces it",
                branch.as_str()
            );
        }
        assert_eq!(TRANSCRIPT_UNRESOLVED, "transcript_unresolved");
    }

    /// A path that resolved and yielded no bytes is a miss too — the over-band
    /// refusal, and the N-1 degrade whose computed path holds nothing — so "no
    /// transcript and no marker" can only mean the run named no session, which
    /// is the property design #490 M6's survey counts against.
    #[tokio::test]
    async fn a_resolved_transcript_that_is_never_read_is_marked_as_uncopied() {
        let path = format!(
            "{}/-workspace/{}",
            agent::transcript_dir(),
            agent::transcript_name("s-1")
        );
        let refusal = types::worker::copy_file_too_large(
            &path,
            store::MAX_BLOB_BYTES + 1,
            store::MAX_BLOB_BYTES,
        );
        let backend = std::sync::Arc::new(test_utils::FakeBackend::new());
        backend.put_file(&path, b"a session".to_vec());
        backend.fail_copy_file(refusal.clone());

        let miss = Harvester::new(backend.clone(), None)
            .collect_agent("acme", "chug", 7, 1, &agent_run("w1/c0ffee", "s-1"))
            .await
            .transcript_miss
            .expect("a resolved transcript that was never read is a miss");
        assert_eq!(miss.branch, MissBranch::Uncopied);
        assert!(miss.is_lost(), "the over-band refusal loses the record");
        assert!(!miss.is_refusal(), "it is not one of D1b's two refusals");
        let marker = marker_of(&miss);
        assert_eq!(marker["branch"], "uncopied");
        assert_eq!(marker["lost"], true);
        assert_eq!(marker["paths"], serde_json::json!([path]));
        assert!(marker.get("reason").is_none());
        assert!(
            miss.detail().contains("was NOT stored"),
            "{}",
            miss.detail()
        );

        let transport = TranscriptMiss {
            error: Some("node w1 unreachable".into()),
            ..miss
        };
        assert!(!transport.is_lost(), "a transport miss lost nothing");
        assert!(marker_of(&transport).get("lost").is_none());

        let backend = std::sync::Arc::new(test_utils::FakeBackend::new());
        backend.fail_find_file(format!(
            "{} Some(\"find_file\") on chug.worker.w1.find_file",
            types::worker::UNKNOWN_OP
        ));
        let miss = Harvester::new(backend.clone(), None)
            .collect_agent("acme", "chug", 7, 2, &agent_run("w1/c0ffee", "s-1"))
            .await
            .transcript_miss
            .expect("a computed path that holds no transcript is a miss, not a silence");
        assert_eq!(miss.branch, MissBranch::Uncopied);
        assert!(!miss.is_lost() && !miss.is_refusal());
        assert_eq!(
            miss.paths,
            vec![agent::transcript_path("s-1")],
            "the marker names the path that was read"
        );
        assert!(
            miss.detail().contains("gone by the read"),
            "{}",
            miss.detail()
        );
    }

    /// Design #362 S1's failure posture: only the size-band refusal earns the
    /// louder level and the move-it-to-a-bucket instruction. An N-1 worker that
    /// does not know `copy_file_chunk` answers with a `WorkerError::Other`
    /// too, and a whole node's work containers must not blame its refresh on a
    /// size the archive never had.
    #[test]
    fn only_an_over_band_refusal_names_the_operator_action() {
        let refusal = types::worker::copy_file_too_large(
            OUTPUT_PATH,
            store::MAX_BLOB_BYTES + 1,
            store::MAX_BLOB_BYTES,
        );
        let (actionable, message) = Harvester::output_copy_failure(7, 2, &refusal);
        assert!(actionable, "{message}");
        assert!(message.contains("belongs in a bucket"), "{message}");
        assert!(message.contains("was NOT stored"), "{message}");

        for other in [
            r#"unknown op Some("copy_file_chunk") on chug.worker.w1.copy_file_chunk"#,
            "node w1 unreachable",
            "worker transport for w1/c3: timed out",
        ] {
            let (actionable, message) = Harvester::output_copy_failure(7, 2, other);
            assert!(!actionable, "{message}");
            assert!(!message.contains("belongs in a bucket"), "{message}");
            assert!(message.contains(other), "{message}");
        }
    }
}
