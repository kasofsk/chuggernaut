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
#[allow(
    clippy::expect_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
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
        image: Some("alpine:3".into()),
        cmd: vec!["sh".into(), "-c".into(), cmd.into()],
        env: HashMap::new(),
        files: vec![],
        cpu_limit: None,
        memory_limit: Some("128Mi".into()),
        node: None,
        runtime_env: None,
    }
}

/// Remove a test container by its `{node}/{docker_id}` id.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
pub fn rm(id: &str) {
    let cid = id.split_once('/').unwrap().1;
    let _ = Command::new("docker").args(["rm", "-f", cid]).output();
}

/// Log capture is the only window into a failed command task. Both streams
/// must come back; within-stream order holds (cross-stream order is Docker's).
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
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

    assert_eq!(be.logs(&id).await.unwrap(), be.logs(&id).await.unwrap());

    let unknown = format!("{node}/deadbeefdeadbeef");
    assert!(be.logs(&unknown).await.is_err());
    rm(&id);
}

/// Live output: `logs_tail` must return a monotonically-growing cursor while
/// the container is still RUNNING (the whole point of the /output endpoint),
/// and keep serving the same offsets after exit so a poller never loses the
/// tail. `follow: false`, so no call ever hangs.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
pub async fn logs_tail_grows_while_running(be: &dyn ContainerBackend, node: &str) {
    let id = be
        .launch(cfg(
            "i=0; while [ $i -lt 15 ]; do echo line-$i; i=$((i+1)); sleep 0.1; done",
        ))
        .await
        .unwrap();

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
    assert!(
        seen.find("line-0") < seen.find("line-14"),
        "reassembled output out of order: {seen:?}"
    );

    let at_end = be.logs_tail(&id, cursor).await.unwrap();
    assert!(at_end.data.is_empty(), "caught-up read should be empty");
    assert_eq!(at_end.offset, cursor);

    let unknown = format!("{node}/deadbeefdeadbeef");
    assert!(be.logs_tail(&unknown, 0).await.is_err());
    rm(&id);
}

#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
pub async fn exit_codes_round_trip(be: &dyn ContainerBackend) {
    let ok = be.launch(cfg("exit 0")).await.unwrap();
    let fail = be.launch(cfg("exit 7")).await.unwrap();
    assert_eq!(be.wait(&ok).await.unwrap(), 0);
    assert_eq!(be.wait(&fail).await.unwrap(), 7);
    assert_eq!(be.wait(&fail).await.unwrap(), 7);
    rm(&ok);
    rm(&fail);
}

#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
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

#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
pub async fn inspect_kill_and_not_found(be: &dyn ContainerBackend, node: &str) {
    let id = be.launch(cfg("sleep 30")).await.unwrap();
    assert_eq!(
        be.inspect(&id).await.unwrap(),
        Some(ContainerStatus::Running)
    );
    be.kill(&id).await.unwrap();
    let exit = be.wait(&id).await.unwrap();
    assert_ne!(exit, 0);
    assert!(matches!(
        be.inspect(&id).await.unwrap(),
        Some(ContainerStatus::Exited { exit_code }) if exit_code == exit
    ));
    be.kill(&id).await.unwrap();
    rm(&id);

    let ghost = format!("{node}/deadbeefdeadbeef");
    assert!(be.inspect(&ghost).await.unwrap().is_none());
    assert!(be.wait(&ghost).await.is_err());
}

/// Live occupancy accounting (spec §3.1): a running container appears in
/// `list_managed_running` tagged with the `(project, job, task)` it was
/// launched for, and drops out once it exits. This is the input both the fleet
/// occupancy snapshot (#138) and busyness placement (#153) read, so every
/// backend — including the NATS-proxied worker fleet — must report it faithfully
/// (the DockerBackend's own list is unit-covered, but the proxy path was not).
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
pub async fn running_count_reflects_launch_and_exit(be: &dyn ContainerBackend, node: &str) {
    let mut config = cfg("sleep 30");
    config.env.insert("JOB_PROJECT".into(), "acme/api".into());
    config.env.insert("JOB_ID".into(), "51".into());
    config.env.insert("CHUG_TASK_ID".into(), "7".into());
    let id = be.launch(config).await.unwrap();

    let running = be.list_managed_running().await.unwrap();
    let mine = running
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("launched container {id} missing from {running:?}"));
    assert_eq!(mine.project.as_deref(), Some("acme/api"));
    assert_eq!(mine.job, Some(51));
    assert_eq!(mine.task, Some(7));
    assert!(id.starts_with(node), "id {id} should carry node {node}");

    be.kill(&id).await.unwrap();
    assert_ne!(be.wait(&id).await.unwrap(), 0);
    be.remove(&id).await.unwrap();
    let after = be.list_managed_running().await.unwrap();
    assert!(
        !after.iter().any(|c| c.id == id),
        "exited+removed container {id} still counted as occupied: {after:?}"
    );
}

/// Resolution by name (design #490 D1a): a backend answers which files under a
/// directory carry a name, in the wire paths [`ContainerBackend::copy_file`]
/// takes straight back, with one, none and several distinguishable and the
/// bound refusing rather than returning a longer list. The container has
/// **exited** by the time this runs, which is the constraint that rules out
/// `exec find`.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): test-harness code — docs/reference/style.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
)]
pub async fn find_file_resolves_by_name(be: &dyn ContainerBackend) {
    let name = "session.jsonl";
    let over = container::FIND_FILE_MATCHES_MAX + 1;
    let id = be
        .launch(cfg(&format!(
            "mkdir -p /projects/-workspace /two/a /two/b && \
             echo one > /projects/-workspace/{name} && \
             echo other > /projects/-workspace/other.jsonl && \
             echo a > /two/a/{name} && echo b > /two/b/{name} && \
             i=0; while [ $i -lt {over} ]; do mkdir -p /many/d-$i; \
             echo x > /many/d-$i/{name}; i=$((i+1)); done"
        )))
        .await
        .unwrap();
    assert_eq!(be.wait(&id).await.unwrap(), 0);

    let resolved = be.find_file(&id, "/projects", name).await.unwrap();
    assert_eq!(
        resolved,
        vec![format!("/projects/-workspace/{name}")],
        "one match comes back as the caller's own wire path"
    );
    assert_eq!(
        be.copy_file(&id, &resolved[0]).await.unwrap().unwrap(),
        b"one\n",
        "a resolved path must be readable by copy_file unchanged"
    );

    assert!(
        be.find_file(&id, "/projects", "absent.jsonl")
            .await
            .unwrap()
            .is_empty(),
        "a name nothing carries is an empty list, not an error"
    );
    assert_eq!(
        be.find_file(&id, "/two", name).await.unwrap().len(),
        2,
        "several must be countable, never collapsed to one"
    );

    let err = be
        .find_file(&id, "/many", name)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(types::worker::FIND_FILE_TOO_MANY),
        "past the bound the scan must refuse by name: {err}"
    );
    rm(&id);
}

/// Run the whole contract.
pub async fn run_all(be: &dyn ContainerBackend, node: &str) {
    find_file_resolves_by_name(be).await;
    logs_capture_both_streams_after_exit(be, node).await;
    logs_tail_grows_while_running(be, node).await;
    exit_codes_round_trip(be).await;
    env_file_injection_and_copy_out(be).await;
    inspect_kill_and_not_found(be, node).await;
    running_count_reflects_launch_and_exit(be, node).await;
}
