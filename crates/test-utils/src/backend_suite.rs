//! Behavioral contract for [`container::ContainerBackend`] implementations,
//! shared by the DockerBackend tests and the worker-proxied fleet backend
//! tests (spec §3.1): any backend must pass these against a real local Docker.
//!
//! Callers guard on Docker availability ([`docker_available`]) and clean up
//! with [`rm`] — containers are not auto-removed (copy_file runs after exit).

use container::{ContainerBackend, ContainerLaunchConfig, ContainerStatus, InjectedFile};
use std::collections::HashMap;
use std::process::Command;

/// True when a local Docker daemon answers; pulls the alpine test image.
pub fn docker_available() -> bool {
    let up = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !up {
        return false;
    }
    assert!(
        Command::new("docker")
            .args(["pull", "-q", "alpine:3"])
            .output()
            .expect("docker pull")
            .status
            .success(),
        "failed to pull alpine:3"
    );
    true
}

pub fn cfg(cmd: &str) -> ContainerLaunchConfig {
    ContainerLaunchConfig {
        image: "alpine:3".into(),
        cmd: vec!["sh".into(), "-c".into(), cmd.into()],
        env: HashMap::new(),
        files: vec![],
        cpu_limit: None,
        memory_limit: Some("128Mi".into()),
    }
}

/// Remove a test container by its `{node}/{docker_id}` id.
pub fn rm(id: &str) {
    let cid = id.split_once('/').unwrap().1;
    let _ = Command::new("docker").args(["rm", "-f", cid]).output();
}

/// Log capture is the only window into a failed command task. Both streams
/// must come back; within-stream order holds (cross-stream order is Docker's).
pub async fn logs_capture_both_streams_after_exit(be: &dyn ContainerBackend, node: &str) {
    let id = be
        .launch(cfg(
            "echo to-stdout; echo after; echo to-stderr >&2; exit 3",
        ))
        .await
        .unwrap();
    assert_eq!(be.wait(&id).await.unwrap(), 3);

    let text = String::from_utf8_lossy(&be.logs(&id).await.unwrap()).to_string();
    assert!(text.contains("to-stdout"), "missing stdout: {text:?}");
    assert!(text.contains("to-stderr"), "missing stderr: {text:?}");
    assert!(
        text.find("to-stdout") < text.find("after"),
        "within-stream order broken: {text:?}"
    );

    // Idempotent, like wait — reconciliation may re-read.
    assert_eq!(be.logs(&id).await.unwrap(), be.logs(&id).await.unwrap());

    let unknown = format!("{node}/deadbeefdeadbeef");
    assert!(be.logs(&unknown).await.is_err());
    rm(&id);
}

pub async fn exit_codes_round_trip(be: &dyn ContainerBackend) {
    let ok = be.launch(cfg("exit 0")).await.unwrap();
    let fail = be.launch(cfg("exit 7")).await.unwrap();
    assert_eq!(be.wait(&ok).await.unwrap(), 0);
    assert_eq!(be.wait(&fail).await.unwrap(), 7);
    // wait after exit is idempotent (§3.6 reconciliation relies on this)
    assert_eq!(be.wait(&fail).await.unwrap(), 7);
    rm(&ok);
    rm(&fail);
}

pub async fn env_file_injection_and_copy_out(be: &dyn ContainerBackend) {
    let mut config = cfg(
        "cat /chuggernaut/prompt.md > /out.txt && printf %s \"$FOO\" >> /out.txt && \
         test -x /usr/local/bin/chuggernaut-tool",
    );
    config.env.insert("FOO".into(), "+bar".into());
    config.files = vec![
        InjectedFile {
            container_path: "/chuggernaut/prompt.md".into(),
            contents: b"hello".to_vec(),
            mode: 0o644,
            artifact: None,
        },
        InjectedFile {
            container_path: "/usr/local/bin/chuggernaut-tool".into(),
            contents: b"#!/bin/sh\nexit 0\n".to_vec(),
            mode: 0o755,
            artifact: None,
        },
    ];
    let id = be.launch(config).await.unwrap();
    assert_eq!(
        be.wait(&id).await.unwrap(),
        0,
        "injected binary must be executable"
    );
    let out = be.copy_file(&id, "/out.txt").await.unwrap().unwrap();
    assert_eq!(out, b"hello+bar");
    assert!(be.copy_file(&id, "/no/such/file").await.unwrap().is_none());
    rm(&id);
}

pub async fn inspect_kill_and_not_found(be: &dyn ContainerBackend, node: &str) {
    let id = be.launch(cfg("sleep 30")).await.unwrap();
    assert_eq!(
        be.inspect(&id).await.unwrap(),
        Some(ContainerStatus::Running)
    );
    be.kill(&id).await.unwrap();
    let exit = be.wait(&id).await.unwrap();
    assert_ne!(exit, 0); // SIGKILL → 137
    assert!(matches!(
        be.inspect(&id).await.unwrap(),
        Some(ContainerStatus::Exited { exit_code }) if exit_code == exit
    ));
    // kill on an exited container is idempotent
    be.kill(&id).await.unwrap();
    rm(&id);

    let ghost = format!("{node}/deadbeefdeadbeef");
    assert!(be.inspect(&ghost).await.unwrap().is_none());
    assert!(be.wait(&ghost).await.is_err());
}

/// Run the whole contract.
pub async fn run_all(be: &dyn ContainerBackend, node: &str) {
    logs_capture_both_streams_after_exit(be, node).await;
    exit_codes_round_trip(be).await;
    env_file_injection_and_copy_out(be).await;
    inspect_kill_and_not_found(be, node).await;
}
