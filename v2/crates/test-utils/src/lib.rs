//! Test harness: fake container backend / agent provider, NATS harness,
//! temp-repo builder, fixture seeding (testing.md tiers 1–2).

pub mod repo;

use async_trait::async_trait;
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

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
}

// TODO: FakeProvider (scriptable AgentProvider), NATS server harness (spawned
// nats-server or testcontainers), temp bare-repo builder, `e2e!` skip guard,
// fixture seeding helpers.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg() -> ContainerLaunchConfig {
        ContainerLaunchConfig {
            image: "test:latest".into(),
            cmd: vec!["true".into()],
            env: HashMap::new(),
            volumes: vec![],
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
