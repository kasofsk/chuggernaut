#![allow(dead_code, unused_imports)]

use std::sync::Arc;

pub use std::time::Duration;

pub use chuggernaut_test_utils as test_utils;
pub use futures::StreamExt;
pub use testcontainers::ImageExt;
pub use testcontainers::runners::AsyncRunner;
pub use testcontainers_modules::nats::{Nats, NatsServerCmd};

pub use chuggernaut_dispatcher::{
    config::Config, handlers, jobs, nats_init, recovery, state::DispatcherState,
};
pub use chuggernaut_types::*;

// ---------------------------------------------------------------------------
// Shared NATS container — one per test process, leaked for lifetime.
// Started on a dedicated OS thread to avoid tokio runtime conflicts
// when multiple #[tokio::test] instances race to initialise.
// ---------------------------------------------------------------------------

pub static TEST_NATS_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

pub fn test_nats_port() -> u16 {
    *TEST_NATS_PORT.get_or_init(|| {
        std::thread::spawn(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let nats_cmd = NatsServerCmd::default().with_jetstream();
                let container = Nats::default().with_cmd(&nats_cmd).start().await.unwrap();
                let port = container.get_host_port_ipv4(4222).await.unwrap();
                test_utils::register_container_cleanup(container.id());
                Box::leak(Box::new(container));
                port
            })
        })
        .join()
        .unwrap()
    })
}

/// Each test gets its own UUID-namespaced dispatcher state.
/// All KV buckets and NATS subjects are prefixed so tests run in parallel
/// without interfering with each other.
pub async fn setup() -> Arc<DispatcherState> {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 60,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        git_url: None,
        git_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
        archive_threshold: 200,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());

    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();

    DispatcherState::new_namespaced(config, client, js, kv, prefix)
}

/// Like `setup()` but accepts overrides for Config fields.
pub async fn setup_with_config(overrides: impl FnOnce(&mut Config)) -> Arc<DispatcherState> {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let mut config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 60,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        git_url: None,
        git_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
        archive_threshold: 200,
    };
    overrides(&mut config);

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
    DispatcherState::new_namespaced(config, client, js, kv, prefix)
}

/// Start the HTTP server and return the base URL.
pub async fn start_http(state: Arc<DispatcherState>) -> String {
    let app = chuggernaut_dispatcher::http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Helper: wait for a job to reach the given state, with timeout.
pub async fn wait_for_state(
    state: &Arc<DispatcherState>,
    key: &str,
    target: JobState,
    timeout_secs: u64,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if tokio::time::Instant::now() > deadline {
            let current = state.jobs.get(key).unwrap().state;
            panic!("timeout waiting for {target:?}: job is in {current:?} after {timeout_secs}s");
        }
        let current = state.jobs.get(key).unwrap().state;
        if current == target {
            return;
        }
        if current == JobState::Failed {
            panic!("job failed while waiting for {target:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
