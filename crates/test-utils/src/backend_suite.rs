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
        node: None,
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

/// Live output: `logs_tail` must return a monotonically-growing cursor while
/// the container is still RUNNING (the whole point of the /output endpoint),
/// and keep serving the same offsets after exit so a poller never loses the
/// tail. `follow: false`, so no call ever hangs.
pub async fn logs_tail_grows_while_running(be: &dyn ContainerBackend, node: &str) {
    // A line every 100ms, then exit — output accrues across polls.
    let id = be
        .launch(cfg(
            "i=0; while [ $i -lt 15 ]; do echo line-$i; i=$((i+1)); sleep 0.1; done",
        ))
        .await
        .unwrap();

    // Read from the cursor while it runs: output appears, the cursor advances,
    // and it never goes backwards.
    let mut cursor = 0u64;
    let mut seen = String::new();
    for _ in 0..60 {
        let tail = be.logs_tail(&id, cursor).await.unwrap();
        assert!(tail.offset >= cursor, "cursor moved backwards");
        seen.push_str(&String::from_utf8_lossy(&tail.data));
        cursor = tail.offset;
        if seen.contains("line-0") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        seen.contains("line-0"),
        "no live output while running: {seen:?}"
    );
    let mid = cursor;

    // Let it finish and drain the rest from the same cursor.
    assert_eq!(be.wait(&id).await.unwrap(), 0);
    for _ in 0..60 {
        let tail = be.logs_tail(&id, cursor).await.unwrap();
        seen.push_str(&String::from_utf8_lossy(&tail.data));
        cursor = tail.offset;
        if seen.contains("line-14") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(cursor >= mid, "cursor shrank after exit");
    assert!(
        seen.contains("line-14"),
        "tail after exit incomplete: {seen:?}"
    );
    // The reconstructed stream is contiguous and ordered per stream.
    assert!(
        seen.find("line-0") < seen.find("line-14"),
        "reassembled output out of order: {seen:?}"
    );

    // Caught up: a cursor at the end yields empty data and the same offset.
    let at_end = be.logs_tail(&id, cursor).await.unwrap();
    assert!(at_end.data.is_empty(), "caught-up read should be empty");
    assert_eq!(at_end.offset, cursor);

    // Unknown container errors, like `logs`.
    let unknown = format!("{node}/deadbeefdeadbeef");
    assert!(be.logs_tail(&unknown, 0).await.is_err());
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
    logs_tail_grows_while_running(be, node).await;
    exit_codes_round_trip(be).await;
    env_file_injection_and_copy_out(be).await;
    inspect_kill_and_not_found(be, node).await;
}
