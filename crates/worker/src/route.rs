//! Per-launch mode routing on a node that serves both runtimes (design #309
//! §1): both backends are constructed at boot, and each launch picks one by
//! whether it carries an image.
//!
//! Every other op addresses a container the node already minted an id for, so
//! it routes on that id ([`container::host::names_host_task`]) rather than on
//! anything the request carries. The two listings are the union of both
//! backends' — a dual-mode node's occupancy is the whole node's, which is what
//! makes the node-wide capacity rule (`daemon::enforce_host_capacity`) mean
//! what it says.

use async_trait::async_trait;
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus, LogTail,
    RunningContainer,
};
use std::sync::Arc;

/// The two local backends of a node whose `WORKER_MODES` names both runtimes.
/// Constructed only in that case: a node declaring one mode holds that
/// backend directly, so its behaviour is byte-for-byte what it was before this
/// type existed.
pub struct RoutedBackend {
    container: Arc<dyn ContainerBackend>,
    host: Arc<dyn ContainerBackend>,
}

impl RoutedBackend {
    pub fn new(container: Arc<dyn ContainerBackend>, host: Arc<dyn ContainerBackend>) -> Self {
        Self { container, host }
    }

    /// The mode the request declares, spelled as the image's presence (#309
    /// §1). Neither backend is a fallback for the other — each refuses a launch
    /// in the mode it does not serve, so a routing mistake here is loud.
    fn for_launch(&self, config: &ContainerLaunchConfig) -> &Arc<dyn ContainerBackend> {
        match config.image {
            Some(_) => &self.container,
            None => &self.host,
        }
    }

    /// Which backend owns an id this node handed out.
    fn owner(&self, id: &ContainerId) -> &Arc<dyn ContainerBackend> {
        if container::host::names_host_task(id) {
            &self.host
        } else {
            &self.container
        }
    }
}

#[async_trait]
impl ContainerBackend for RoutedBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        self.for_launch(&config).launch(config).await
    }

    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError> {
        self.owner(id).wait(id).await
    }

    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError> {
        self.owner(id).kill(id).await
    }

    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError> {
        self.owner(id).inspect(id).await
    }

    async fn copy_file(
        &self,
        id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        self.owner(id).copy_file(id, path).await
    }

    async fn copy_file_chunked(
        &self,
        id: &ContainerId,
        path: &str,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        self.owner(id).copy_file_chunked(id, path, max_bytes).await
    }

    async fn find_file(
        &self,
        id: &ContainerId,
        dir: &str,
        name: &str,
    ) -> Result<Vec<String>, BackendError> {
        self.owner(id).find_file(id, dir, name).await
    }

    async fn logs(&self, id: &ContainerId) -> Result<Vec<u8>, BackendError> {
        self.owner(id).logs(id).await
    }

    async fn logs_tail(&self, id: &ContainerId, since: u64) -> Result<LogTail, BackendError> {
        self.owner(id).logs_tail(id, since).await
    }

    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        self.owner(id).remove(id).await
    }

    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
        let mut ids = self.container.list_managed_exited().await?;
        ids.extend(self.host.list_managed_exited().await?);
        Ok(ids)
    }

    async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError> {
        let mut running = self.container.list_managed_running().await?;
        running.extend(self.host.list_managed_running().await?);
        Ok(running)
    }

    async fn managed_running_total(&self) -> Result<u32, BackendError> {
        Ok(self.container.managed_running_total().await?
            + self.host.managed_running_total().await?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use test_utils::FakeBackend;

    fn launch_of(image: Option<&str>) -> ContainerLaunchConfig {
        ContainerLaunchConfig {
            image: image.map(String::from),
            cmd: vec!["true".into()],
            env: Default::default(),
            files: vec![],
            cpu_limit: None,
            memory_limit: None,
            node: None,
            runtime_env: None,
        }
    }

    fn routed() -> (Arc<FakeBackend>, Arc<FakeBackend>, RoutedBackend) {
        let container = Arc::new(FakeBackend::new());
        let host = Arc::new(FakeBackend::new());
        let routed = RoutedBackend::new(container.clone(), host.clone());
        (container, host, routed)
    }

    /// #309 §1's rule, which P0's node-level `backend_kind` could not express:
    /// on a node serving both runtimes the mode resolves per launched task, and
    /// the selector is the image.
    #[tokio::test]
    async fn each_launch_routes_by_its_declared_mode() {
        let (container, host, routed) = routed();

        routed.launch(launch_of(Some("img:latest"))).await.unwrap();
        assert_eq!(container.launches().len(), 1);
        assert_eq!(container.launches()[0].image.as_deref(), Some("img:latest"));
        assert!(host.launches().is_empty());

        routed.launch(launch_of(None)).await.unwrap();
        assert_eq!(host.launches().len(), 1);
        assert_eq!(host.launches()[0].image, None);
        assert_eq!(container.launches().len(), 1);
    }

    /// Every later op addresses the backend that minted the id, so a host
    /// task's `kill` never reaches docker and vice versa.
    #[tokio::test]
    async fn later_ops_follow_the_id_that_launched() {
        let (container, host, routed) = routed();
        let cid = routed.launch(launch_of(Some("img"))).await.unwrap();
        let hid = format!("w1/{}beef-0", container::host::TASK_PREFIX);

        assert!(routed.inspect(&cid).await.unwrap().is_some());
        routed.kill(&hid).await.unwrap();
        routed.kill(&cid).await.unwrap();
        assert_eq!(host.killed(), vec![hid]);
        assert_eq!(container.killed(), vec![cid]);
    }
}
