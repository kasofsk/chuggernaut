//! Test harness: fake container backend / agent provider, NATS harness,
//! temp-repo builder, fixture seeding (testing.md tiers 1–2).

pub mod backend_suite;
pub mod nats;
pub mod repo;

use agent::{AgentError, AgentOutput, AgentProvider, AgentRunConfig};
use async_trait::async_trait;
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus, LogTail,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Deterministic, scriptable [`ContainerBackend`]: every launch records its config
/// and "exits" with the next scripted exit code (default 0).
pub struct FakeBackend {
    next_id: AtomicU64,
    state: Mutex<FakeBackendState>,
}

/// A side-effect hook awaited when a container launches — the place to
/// simulate the outside world moving (e.g. another job landing on `main`
/// while an evaluation container runs). Consumed in launch order like
/// [`FakeProvider`]'s run hooks.
type LaunchHook =
    Box<dyn FnOnce(ContainerLaunchConfig) -> futures::future::BoxFuture<'static, ()> + Send>;

/// Decides whether a launch is rejected: `Some(reason)` makes `launch` return
/// `BackendError::Launch(reason)` before any container exists — the backend
/// refusing a container up front (bad image, invalid resource limit).
type LaunchFail = Box<dyn Fn(&ContainerLaunchConfig) -> Option<String> + Send>;

#[derive(Default)]
struct FakeBackendState {
    /// Exit codes handed out in launch order; empty → exit 0.
    scripted_exits: Vec<i32>,
    launches: Vec<ContainerLaunchConfig>,
    /// Hooks consumed in launch order, awaited before `launch` returns.
    launch_hooks: Vec<Option<LaunchHook>>,
    /// When set, consulted on every `launch`; a `Some(reason)` rejects it.
    launch_fail: Option<LaunchFail>,
    exits: HashMap<ContainerId, i32>,
    /// Files retrievable via copy_file, keyed by (container path).
    files: HashMap<String, Vec<u8>>,
    /// Returned by `logs` (and sliced by `logs_tail`) for every container.
    logs: Vec<u8>,
    /// When set, `logs_tail` sleeps this long before returning — a stand-in
    /// for a slow/wedged node, to prove one output request never blocks others.
    logs_tail_stall: Option<Duration>,
    /// When set, `logs_tail` fails with this error — an unreachable node, so
    /// the output handler must surface an error envelope rather than hang.
    logs_tail_fail: Option<String>,
    /// Containers removed via `remove`, in call order.
    removed: Vec<ContainerId>,
    /// Ids returned by `list_managed_exited` (the startup-sweep candidates).
    managed_exited: Vec<ContainerId>,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            state: Mutex::new(FakeBackendState::default()),
        }
    }

    /// Queue exit codes for upcoming launches (consumed in order).
    pub fn script_exits(&self, codes: impl IntoIterator<Item = i32>) {
        self.state.lock().unwrap().scripted_exits.extend(codes);
    }

    /// Make a file available to `copy_file` (e.g. `/workspace/eval-result.json`).
    pub fn put_file(&self, path: &str, contents: impl Into<Vec<u8>>) {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.to_string(), contents.into());
    }

    /// Script what `logs` returns for every container (and what `logs_tail`
    /// slices its cursor pages from).
    pub fn put_logs(&self, contents: impl Into<Vec<u8>>) {
        self.state.lock().unwrap().logs = contents.into();
    }

    /// Make `logs_tail` sleep before returning — simulate a slow/wedged node so
    /// a test can prove that one stalled output read never blocks other reads.
    pub fn stall_logs_tail(&self, delay: Duration) {
        self.state.lock().unwrap().logs_tail_stall = Some(delay);
    }

    /// Make `logs_tail` fail with `BackendError::Unavailable` — an unreachable
    /// node, so the output handler must reply with an error envelope.
    pub fn fail_logs_tail(&self, reason: impl Into<String>) {
        self.state.lock().unwrap().logs_tail_fail = Some(reason.into());
    }

    pub fn launches(&self) -> Vec<ContainerLaunchConfig> {
        self.state.lock().unwrap().launches.clone()
    }

    /// Queue a side-effect hook for the next un-hooked launch. The hook is
    /// awaited before `launch` returns the container id — the window during
    /// which the "container" is running (e.g. move `main` while an evaluation
    /// container runs, so the wrap-up merge gate fires).
    pub fn on_launch<F, Fut>(&self, hook: F)
    where
        F: FnOnce(ContainerLaunchConfig) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.state
            .lock()
            .unwrap()
            .launch_hooks
            .push(Some(Box::new(move |cfg| Box::pin(hook(cfg)))));
    }

    /// Reject any launch for which `f` returns `Some(reason)` with
    /// `BackendError::Launch(reason)` — the backend refusing a container before
    /// it ever starts (bad image, invalid resource limit, node pressure). A
    /// rejected launch produces no container and consumes no scripted exit.
    pub fn fail_launch_if<F>(&self, f: F)
    where
        F: Fn(&ContainerLaunchConfig) -> Option<String> + Send + 'static,
    {
        self.state.lock().unwrap().launch_fail = Some(Box::new(f));
    }

    /// Seed the ids that `list_managed_exited` reports — the exited managed
    /// containers a startup sweep should consider.
    pub fn seed_managed_exited(&self, ids: impl IntoIterator<Item = ContainerId>) {
        self.state.lock().unwrap().managed_exited.extend(ids);
    }

    /// Container ids passed to `remove`, in call order.
    pub fn removed(&self) -> Vec<ContainerId> {
        self.state.lock().unwrap().removed.clone()
    }
}

#[async_trait]
impl ContainerBackend for FakeBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        // Rejected before allocating an id or recording the launch: a refused
        // container never exists, exactly as a real backend refusal.
        if let Some(reason) = self
            .state
            .lock()
            .unwrap()
            .launch_fail
            .as_ref()
            .and_then(|f| f(&config))
        {
            return Err(BackendError::Launch(reason));
        }
        let id = format!("fake-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let hook = {
            let mut st = self.state.lock().unwrap();
            let exit = if st.scripted_exits.is_empty() {
                0
            } else {
                st.scripted_exits.remove(0)
            };
            let idx = st.launches.len();
            let hook = st.launch_hooks.get_mut(idx).and_then(Option::take);
            st.launches.push(config.clone());
            st.exits.insert(id.clone(), exit);
            hook
        };
        // Awaited outside the lock so the hook can drive the dispatcher, as a
        // real container's lifetime would overlap other work.
        if let Some(hook) = hook {
            hook(config).await;
        }
        Ok(id)
    }

    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError> {
        self.state
            .lock()
            .unwrap()
            .exits
            .get(id)
            .copied()
            .ok_or_else(|| BackendError::NotFound(id.clone()))
    }

    async fn kill(&self, _id: &ContainerId) -> Result<(), BackendError> {
        Ok(())
    }

    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .exits
            .get(id)
            .map(|&exit_code| ContainerStatus::Exited { exit_code }))
    }

    async fn copy_file(
        &self,
        _id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        Ok(self.state.lock().unwrap().files.get(path).cloned())
    }

    async fn logs(&self, _id: &ContainerId) -> Result<Vec<u8>, BackendError> {
        Ok(self.state.lock().unwrap().logs.clone())
    }

    async fn logs_tail(&self, _id: &ContainerId, since: u64) -> Result<LogTail, BackendError> {
        let (logs, stall, fail) = {
            let st = self.state.lock().unwrap();
            (
                st.logs.clone(),
                st.logs_tail_stall,
                st.logs_tail_fail.clone(),
            )
        };
        if let Some(delay) = stall {
            tokio::time::sleep(delay).await;
        }
        if let Some(reason) = fail {
            return Err(BackendError::Unavailable(reason));
        }
        Ok(LogTail::slice(&logs, since))
    }

    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        let mut st = self.state.lock().unwrap();
        // Drop it from the launch bookkeeping so `inspect`/`wait` afterward
        // report it as gone, mirroring a real removed container. Idempotent.
        st.exits.remove(id);
        st.managed_exited.retain(|c| c != id);
        st.removed.push(id.clone());
        Ok(())
    }

    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
        Ok(self.state.lock().unwrap().managed_exited.clone())
    }
}

/// Deterministic, scriptable [`AgentProvider`]: every run records its config
/// and returns the next scripted exit code (default 0). Side effects a real
/// agent would have (commits, submit_eval calls) are scripted per run via
/// [`FakeProvider::on_run`].
pub struct FakeProvider {
    state: Mutex<FakeProviderState>,
    push_notifications: bool,
    /// When set, runs launch a real (fake) container so the run reports a
    /// container id — the handle the dispatcher needs to harvest transcripts
    /// and logs. Mirrors ClaudeProvider, which launches via the backend.
    backend: Option<Arc<dyn ContainerBackend>>,
}

/// Async so a hook can drive the dispatcher (e.g. submit_eval) and await the
/// ack before the "container" exits — exactly the ordering the channel MCP
/// server guarantees (spec §4.2 bounded-retry-until-ack).
type RunHook = Box<dyn FnOnce(AgentRunConfig) -> futures::future::BoxFuture<'static, ()> + Send>;

#[derive(Default)]
struct FakeProviderState {
    /// Exit codes handed out in run order; empty → exit 0.
    scripted_exits: Vec<i32>,
    /// Hooks consumed in run order, invoked with the run's config before it
    /// "exits" — the place to simulate submit_result/submit_eval side effects.
    hooks: Vec<Option<RunHook>>,
    runs: Vec<AgentRunConfig>,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProvider {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FakeProviderState::default()),
            push_notifications: true,
            backend: None,
        }
    }

    /// Launch through `backend`, so runs report a container id and artifact
    /// capture can be exercised. Without this a run has no container, like a
    /// provider stub.
    pub fn with_backend(backend: Arc<dyn ContainerBackend>) -> Self {
        Self {
            backend: Some(backend),
            ..Self::new()
        }
    }

    pub fn script_exits(&self, codes: impl IntoIterator<Item = i32>) {
        self.state.lock().unwrap().scripted_exits.extend(codes);
    }

    /// Queue a side-effect hook for the next un-hooked run. The hook is awaited
    /// before the run returns its exit code.
    pub fn on_run<F, Fut>(&self, hook: F)
    where
        F: FnOnce(AgentRunConfig) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.state
            .lock()
            .unwrap()
            .hooks
            .push(Some(Box::new(move |cfg| Box::pin(hook(cfg)))));
    }

    pub fn runs(&self) -> Vec<AgentRunConfig> {
        self.state.lock().unwrap().runs.clone()
    }
}

#[async_trait]
impl AgentProvider for FakeProvider {
    async fn run(&self, config: AgentRunConfig) -> Result<AgentOutput, AgentError> {
        let (exit_code, hook) = {
            let mut st = self.state.lock().unwrap();
            let exit = if st.scripted_exits.is_empty() {
                0
            } else {
                st.scripted_exits.remove(0)
            };
            let idx = st.runs.len();
            let hook = st.hooks.get_mut(idx).and_then(Option::take);
            st.runs.push(config.clone());
            (exit, hook)
        };
        // Launch before the hook so the container exists for the whole run,
        // as it would for a real provider.
        let container_id = match &self.backend {
            Some(backend) => Some(
                backend
                    .launch(ContainerLaunchConfig {
                        image: config.image.clone(),
                        cmd: vec!["agent".into()],
                        env: config.env.clone(),
                        files: config.files.clone(),
                        cpu_limit: None,
                        memory_limit: None,
                        node: config.node.clone(),
                    })
                    .await?,
            ),
            None => None,
        };
        if let Some(hook) = hook {
            hook(config.clone()).await;
        }
        Ok(AgentOutput {
            exit_code,
            container_id,
            session_id: Some(config.session_id),
        })
    }

    fn supports_push_notifications(&self) -> bool {
        self.push_notifications
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg() -> ContainerLaunchConfig {
        ContainerLaunchConfig {
            image: "test:latest".into(),
            cmd: vec!["true".into()],
            env: HashMap::new(),
            files: vec![],
            cpu_limit: None,
            memory_limit: None,
            node: None,
        }
    }

    #[tokio::test]
    async fn fake_backend_scripts_exits_in_order() {
        let be = FakeBackend::new();
        be.script_exits([1, 0]);
        let a = be.launch(cfg()).await.unwrap();
        let b = be.launch(cfg()).await.unwrap();
        let c = be.launch(cfg()).await.unwrap();
        assert_eq!(be.wait(&a).await.unwrap(), 1);
        assert_eq!(be.wait(&b).await.unwrap(), 0);
        assert_eq!(be.wait(&c).await.unwrap(), 0); // default
        assert_eq!(be.launches().len(), 3);
    }

    #[tokio::test]
    async fn fake_backend_serves_files() {
        let be = FakeBackend::new();
        be.put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());
        let id = be.launch(cfg()).await.unwrap();
        let f = be
            .copy_file(&id, "/workspace/eval-result.json")
            .await
            .unwrap();
        assert_eq!(f.unwrap(), br#"{"ok":true}"#);
        assert!(be.copy_file(&id, "/missing").await.unwrap().is_none());
    }
}
