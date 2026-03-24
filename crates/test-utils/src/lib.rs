//! Shared test infrastructure for chuggernaut integration tests.
//!
//! Provides standardized container images, versions, and setup helpers
//! for NATS, Forgejo, and Forgejo Actions runner.
//!
//! # Container lifecycle
//!
//! Test containers are cleaned up via two mechanisms:
//! - **watchdog** (testcontainers feature): cleans up on SIGTERM/SIGINT/SIGQUIT
//! - **atexit hook**: cleans up on normal process exit
//!
//! Tests that share a container across the process should use `Box::leak` to
//! keep the container alive, then call [`register_container_cleanup`] with the
//! container ID so it gets removed when the process exits.

use std::sync::Mutex;
use std::time::Duration;

use testcontainers::core::{CmdWaitFor, ExecCommand, IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers_modules::nats::{Nats, NatsServerCmd};

// ---------------------------------------------------------------------------
// Pinned container versions
// ---------------------------------------------------------------------------

pub const FORGEJO_IMAGE: &str = "codeberg.org/forgejo/forgejo";
pub const FORGEJO_TAG: &str = "14";

pub const RUNNER_IMAGE: &str = "data.forgejo.org/forgejo/runner";
pub const RUNNER_TAG: &str = "11";

pub const RUNNER_ENV_IMAGE: &str = "chuggernaut-runner-env:latest";

pub const ADMIN_USER: &str = "chuggernaut-admin";
pub const ADMIN_PASS: &str = "chuggernaut-admin";
pub const REVIEWER_USER: &str = "chuggernaut-reviewer";
pub const REVIEWER_PASS: &str = "chuggernaut-reviewer";

// ---------------------------------------------------------------------------
// Container cleanup (atexit hook)
// ---------------------------------------------------------------------------

static CONTAINER_IDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ATEXIT_REGISTERED: std::sync::Once = std::sync::Once::new();

extern "C" fn cleanup_containers() {
    let ids = CONTAINER_IDS.lock().unwrap();
    if ids.is_empty() {
        return;
    }
    eprintln!("test cleanup: removing {} container(s)", ids.len());
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f"])
        .args(ids.iter().map(|s| s.as_str()))
        .stderr(std::process::Stdio::null())
        .output();
}

/// Register a container ID for removal when the process exits.
///
/// Use this after `Box::leak`-ing a shared container so it gets cleaned up
/// on normal exit. Signal-based cleanup is handled by the testcontainers
/// `watchdog` feature.
pub fn register_container_cleanup(container_id: &str) {
    ATEXIT_REGISTERED.call_once(|| unsafe {
        libc::atexit(cleanup_containers);
    });
    CONTAINER_IDS
        .lock()
        .unwrap()
        .push(container_id.to_string());
}

// ---------------------------------------------------------------------------
// NATS
// ---------------------------------------------------------------------------

/// Start a shared NATS container with JetStream. Call once per process via OnceLock.
pub async fn start_nats() -> ContainerAsync<Nats> {
    let nats_cmd = NatsServerCmd::default().with_jetstream();
    Nats::default()
        .with_cmd(&nats_cmd)
        .start()
        .await
        .unwrap()
}

/// Get the host port for a NATS container.
pub async fn nats_port(container: &ContainerAsync<Nats>) -> u16 {
    container.get_host_port_ipv4(4222).await.unwrap()
}

// ---------------------------------------------------------------------------
// Forgejo
// ---------------------------------------------------------------------------

/// Start a Forgejo container with Actions enabled and INSTALL_LOCK.
pub async fn start_forgejo() -> ContainerAsync<GenericImage> {
    let container = GenericImage::new(FORGEJO_IMAGE, FORGEJO_TAG)
        .with_exposed_port(3000.tcp())
        .with_wait_for(WaitFor::seconds(5))
        .with_env_var("FORGEJO__database__DB_TYPE", "sqlite3")
        .with_env_var("FORGEJO__security__INSTALL_LOCK", "true")
        .with_env_var("FORGEJO__service__DISABLE_REGISTRATION", "true")
        .with_env_var("FORGEJO__actions__ENABLED", "true")
        .with_env_var("FORGEJO__log__LEVEL", "Warn")
        .with_startup_timeout(Duration::from_secs(120))
        .start()
        .await
        .unwrap();

    let port = container.get_host_port_ipv4(3000.tcp()).await.unwrap();
    wait_for_forgejo_api(port).await;

    container
}

/// Get the host port for a Forgejo container.
pub async fn forgejo_port(container: &ContainerAsync<GenericImage>) -> u16 {
    container.get_host_port_ipv4(3000.tcp()).await.unwrap()
}

/// The URL to reach Forgejo from the host.
pub fn forgejo_host_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The URL to reach Forgejo from inside a Docker container (e.g., the runner).
pub fn forgejo_internal_url(port: u16) -> String {
    format!("http://host.docker.internal:{port}")
}

/// Wait for the Forgejo API to be ready.
pub async fn wait_for_forgejo_api(port: u16) {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/api/v1/version");
    for _ in 0..60 {
        if client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    panic!("Forgejo API not ready after 120s");
}

/// Create admin user(s) in Forgejo and return an API token.
/// Optionally creates a separate reviewer user.
pub async fn setup_forgejo_users(
    container: &ContainerAsync<GenericImage>,
    port: u16,
    create_reviewer: bool,
) -> ForgejoCredentials {
    let url = forgejo_host_url(port);

    // Create admin user
    container
        .exec(
            ExecCommand::new([
                "su-exec", "git", "forgejo", "admin", "user", "create",
                "--admin",
                "--username", ADMIN_USER,
                "--password", ADMIN_PASS,
                "--email", "admin@chuggernaut.test",
                "--must-change-password=false",
            ])
            .with_cmd_ready_condition(CmdWaitFor::exit_code(0)),
        )
        .await
        .expect("failed to create admin user");

    // Create admin token
    let http = reqwest::Client::new();
    let token_resp: serde_json::Value = http
        .post(format!("{url}/api/v1/users/{ADMIN_USER}/tokens"))
        .basic_auth(ADMIN_USER, Some(ADMIN_PASS))
        .json(&serde_json::json!({
            "name": "test-token",
            "scopes": ["all"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let admin_token = token_resp["sha1"].as_str().unwrap().to_string();

    let reviewer_token = if create_reviewer {
        // Create reviewer user
        container
            .exec(
                ExecCommand::new([
                    "su-exec", "git", "forgejo", "admin", "user", "create",
                    "--admin",
                    "--username", REVIEWER_USER,
                    "--password", REVIEWER_PASS,
                    "--email", "reviewer@chuggernaut.test",
                    "--must-change-password=false",
                ])
                .with_cmd_ready_condition(CmdWaitFor::exit_code(0)),
            )
            .await
            .expect("failed to create reviewer user");

        let token_resp: serde_json::Value = http
            .post(format!("{url}/api/v1/users/{REVIEWER_USER}/tokens"))
            .basic_auth(REVIEWER_USER, Some(REVIEWER_PASS))
            .json(&serde_json::json!({
                "name": "reviewer-token",
                "scopes": ["all"]
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        Some(token_resp["sha1"].as_str().unwrap().to_string())
    } else {
        None
    };

    ForgejoCredentials {
        admin_token,
        reviewer_token,
    }
}

pub struct ForgejoCredentials {
    pub admin_token: String,
    pub reviewer_token: Option<String>,
}

/// Get a runner registration token from Forgejo.
pub async fn get_runner_registration_token(port: u16, admin_token: &str) -> String {
    let url = forgejo_host_url(port);
    let http = reqwest::Client::new();
    let resp: serde_json::Value = http
        .get(format!("{url}/api/v1/admin/runners/registration-token"))
        .header("Authorization", format!("token {admin_token}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    resp["token"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Start a Forgejo Actions runner with Docker socket passthrough.
/// Uses the official Forgejo runner image with register + daemon.
pub async fn start_runner(
    forgejo_port: u16,
    runner_reg_token: &str,
    runner_config_path: &str,
) -> ContainerAsync<GenericImage> {
    let internal_url = forgejo_internal_url(forgejo_port);
    let labels = format!("ubuntu-latest:docker://{RUNNER_ENV_IMAGE}");
    let register_and_run = format!(
        "cd /data && forgejo-runner register --no-interactive --instance '{internal_url}' --token '{runner_reg_token}' --name runner --labels '{labels}' -c /data/config.yaml && forgejo-runner daemon -c /data/config.yaml"
    );

    GenericImage::new(RUNNER_IMAGE, RUNNER_TAG)
        .with_user("root")
        .with_mount(Mount::bind_mount(
            "/var/run/docker.sock",
            "/var/run/docker.sock",
        ))
        .with_mount(Mount::bind_mount(
            runner_config_path,
            "/data/config.yaml",
        ))
        .with_cmd(["sh", "-c", &register_and_run])
        .with_startup_timeout(Duration::from_secs(60))
        .start()
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// Repo setup helpers
// ---------------------------------------------------------------------------

/// Create an org and repo in Forgejo. Returns (org_name, repo_name).
pub async fn create_test_repo(
    port: u16,
    token: &str,
    org: &str,
    repo: &str,
) {
    let url = forgejo_host_url(port);
    let http = reqwest::Client::new();

    http.post(format!("{url}/api/v1/orgs"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"username": org, "visibility": "public"}))
        .send()
        .await
        .unwrap();

    http.post(format!("{url}/api/v1/orgs/{org}/repos"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({
            "name": repo,
            "default_branch": "main",
            "auto_init": true
        }))
        .send()
        .await
        .unwrap();
}

/// Push a file to a repo via the Forgejo API.
pub async fn push_file(
    port: u16,
    token: &str,
    org: &str,
    repo: &str,
    path: &str,
    content: &str,
    message: &str,
) {
    let url = forgejo_host_url(port);
    let http = reqwest::Client::new();
    http.post(format!("{url}/api/v1/repos/{org}/{repo}/contents/{path}"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({
            "message": message,
            "content": base64_encode(content)
        }))
        .send()
        .await
        .unwrap();
}

/// Set a repo action secret.
pub async fn set_repo_secret(
    port: u16,
    token: &str,
    org: &str,
    repo: &str,
    name: &str,
    value: &str,
) {
    let url = forgejo_host_url(port);
    let http = reqwest::Client::new();
    http.put(format!("{url}/api/v1/repos/{org}/{repo}/actions/secrets/{name}"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"data": value}))
        .send()
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

pub fn base64_encode(s: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = s.as_bytes();
    let mut result = String::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | bytes[i + 2] as u32;
        result.push(CHARS[(n >> 18 & 63) as usize] as char);
        result.push(CHARS[(n >> 12 & 63) as usize] as char);
        result.push(CHARS[(n >> 6 & 63) as usize] as char);
        result.push(CHARS[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        result.push(CHARS[(n >> 18 & 63) as usize] as char);
        result.push(CHARS[(n >> 12 & 63) as usize] as char);
        result.push(CHARS[(n >> 6 & 63) as usize] as char);
        result.push('=');
    } else if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        result.push(CHARS[(n >> 18 & 63) as usize] as char);
        result.push(CHARS[(n >> 12 & 63) as usize] as char);
        result.push('=');
        result.push('=');
    }
    result
}
