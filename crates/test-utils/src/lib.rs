//! Test harness: fake container backend / agent provider, NATS harness,
//! temp-repo builder, fixture seeding (testing.md tiers 1–2).

pub mod nats;
pub mod repo;

use agent::{AgentError, AgentOutput, AgentProvider, AgentRunConfig};
use async_trait::async_trait;
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Deterministic, scriptable [`ContainerBackend`]: every launch records its config
/// and "exits" with the next scripted exit code (default 0).
pub struct FakeBackend {
    next_id: AtomicU64,
    state: Mutex<FakeBackendState>,
}

#[derive(Default)]
struct FakeBackendState {
    /// Exit codes handed out in launch order; empty → exit 0.
    scripted_exits: Vec<i32>,
    launches: Vec<ContainerLaunchConfig>,
    exits: HashMap<ContainerId, i32>,
    /// Files retrievable via copy_file, keyed by (container path).
    files: HashMap<String, Vec<u8>>,
    /// Returned by `logs` for every container.
    logs: Vec<u8>,
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

    /// Script what `logs` returns for every container.
    pub fn put_logs(&self, contents: impl Into<Vec<u8>>) {
        self.state.lock().unwrap().logs = contents.into();
    }

    pub fn launches(&self) -> Vec<ContainerLaunchConfig> {
        self.state.lock().unwrap().launches.clone()
    }
}

#[async_trait]
impl ContainerBackend for FakeBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        let id = format!("fake-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let mut st = self.state.lock().unwrap();
        let exit = if st.scripted_exits.is_empty() {
            0
        } else {
            st.scripted_exits.remove(0)
        };
        st.launches.push(config);
        st.exits.insert(id.clone(), exit);
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
