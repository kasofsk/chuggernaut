//! Tier-2 tests: DockerBackend against the local Docker daemon. Skips when
//! Docker is unavailable. Uses alpine; pulled once in the guard.

use container::docker::DockerBackend;
use container::{ContainerBackend, ContainerLaunchConfig, ContainerStatus, InjectedFile};
use std::collections::HashMap;
use std::process::Command;

/// Backend + pulled image, or None to skip.
fn docker() -> Option<DockerBackend> {
    let up = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !up {
        eprintln!("skipping: Docker daemon unavailable");
        return None;
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
    Some(DockerBackend::local(8).expect("local backend"))
}

fn cfg(cmd: &str) -> ContainerLaunchConfig {
    ContainerLaunchConfig {
        image: "alpine:3".into(),
        cmd: vec!["sh".into(), "-c".into(), cmd.into()],
        env: HashMap::new(),
        files: vec![],
        cpu_limit: None,
        memory_limit: Some("128Mi".into()),
    }
}

/// Containers are not auto-removed (copy_file runs after exit); tests clean up.
fn rm(id: &str) {
    let cid = id.split_once('/').unwrap().1;
    let _ = Command::new("docker").args(["rm", "-f", cid]).output();
}

/// Log capture is the only window into a failed command task — `TaskResult`
/// carries no output. Both streams must come back.
///
/// Note what is *not* asserted: cross-stream ordering. Docker orders frames by
/// timestamp, and a measured run returned stderr before an earlier stdout write
/// in the same millisecond. Only within-stream order is guaranteed.
#[tokio::test]
async fn logs_capture_both_streams_after_exit() {
    let Some(be) = docker() else { return };
    let id = be
        .launch(cfg("echo to-stdout; echo after; echo to-stderr >&2; exit 3"))
        .await
        .unwrap();
    assert_eq!(be.wait(&id).await.unwrap(), 3);

    // Read *after* exit: a failed container is exactly when logs matter, and
    // follow:false must not hang on an already-dead container.
    let text = String::from_utf8_lossy(&be.logs(&id).await.unwrap()).to_string();
    assert!(text.contains("to-stdout"), "missing stdout: {text:?}");
    assert!(text.contains("to-stderr"), "missing stderr: {text:?}");
    assert!(
        text.find("to-stdout") < text.find("after"),
        "within-stream order broken: {text:?}"
    );

    // Idempotent, like wait — reconciliation may re-read.
    assert_eq!(be.logs(&id).await.unwrap(), be.logs(&id).await.unwrap());

    let unknown = "local/deadbeefdeadbeef".to_string();
    assert!(be.logs(&unknown).await.is_err());
    rm(&id);
}

#[tokio::test]
async fn exit_codes_round_trip() {
    let Some(be) = docker() else { return };
    let ok = be.launch(cfg("exit 0")).await.unwrap();
    let fail = be.launch(cfg("exit 7")).await.unwrap();
    assert_eq!(be.wait(&ok).await.unwrap(), 0);
    assert_eq!(be.wait(&fail).await.unwrap(), 7);
    // wait after exit is idempotent (§3.6 reconciliation relies on this)
    assert_eq!(be.wait(&fail).await.unwrap(), 7);
    rm(&ok);
    rm(&fail);
}

#[tokio::test]
async fn env_file_injection_and_copy_out() {
    let Some(be) = docker() else { return };
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
        },
        InjectedFile {
            container_path: "/usr/local/bin/chuggernaut-tool".into(),
            contents: b"#!/bin/sh\nexit 0\n".to_vec(),
            mode: 0o755,
        },
    ];
    let id = be.launch(config).await.unwrap();
    assert_eq!(be.wait(&id).await.unwrap(), 0, "injected binary must be executable");
    let out = be.copy_file(&id, "/out.txt").await.unwrap().unwrap();
    assert_eq!(out, b"hello+bar");
    assert!(be.copy_file(&id, "/no/such/file").await.unwrap().is_none());
    rm(&id);
}

#[tokio::test]
async fn inspect_kill_and_not_found() {
    let Some(be) = docker() else { return };
    let id = be.launch(cfg("sleep 30")).await.unwrap();
    assert_eq!(be.inspect(&id).await.unwrap(), Some(ContainerStatus::Running));
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

    let ghost = "local/deadbeefdeadbeef".to_string();
    assert!(be.inspect(&ghost).await.unwrap().is_none());
    assert!(be.wait(&ghost).await.is_err());
}
