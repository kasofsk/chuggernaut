//! Tier-2: `HostBackend` against real processes on the test machine (design
//! #309 P0). No Docker and no NATS — a host task is a process group and a
//! directory, so this suite runs everywhere, including a Docker-less evaluator.
//!
//! Every backend here is given its own root **and its own workspace path**, so
//! the suite never touches the machine's real `/workspace`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use container::host::{HOST_ROOT_DEFAULT, HOST_WORKSPACE, HostBackend};
use container::{ContainerBackend, ContainerLaunchConfig, ContainerStatus, InjectedFile};
use std::collections::HashMap;

/// A root this test alone owns, removed by the caller.
fn temp_root(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "chug-host-{name}-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn backend(root: &std::path::Path) -> HostBackend {
    HostBackend::with_workspace("w1", root.join("tasks"), root.join("workspace")).unwrap()
}

fn cfg(script: &str) -> ContainerLaunchConfig {
    ContainerLaunchConfig {
        image: "chuggernaut/agent-rust:prod".into(),
        cmd: vec!["sh".into(), "-c".into(), script.into()],
        env: HashMap::from([
            ("JOB_PROJECT".to_string(), "acme/chug".to_string()),
            ("JOB_ID".to_string(), "309".to_string()),
            ("CHUG_TASK_ID".to_string(), "2".to_string()),
        ]),
        files: Vec::new(),
        cpu_limit: None,
        memory_limit: None,
        node: None,
        runtime_env: None,
    }
}

/// Shell that blocks until the test creates `release`, so "still running" is a
/// handshake rather than a bet on a `sleep` outlasting a loaded CI machine.
/// Bounded, so a test that panics before releasing cannot strand the process.
fn hold_until_released(release: &std::path::Path) -> String {
    format!(
        "i=0; while [ ! -f {} ] && [ $i -lt 1000 ]; do i=$((i+1)); sleep 0.02; done",
        release.display()
    )
}

fn release(path: &std::path::Path) {
    std::fs::write(path, b"go").unwrap();
}

async fn settle(backend: &HostBackend, id: &String) -> i32 {
    for _ in 0..200 {
        if let Some(ContainerStatus::Exited { exit_code }) = backend.inspect(id).await.unwrap() {
            return exit_code;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("host task {id} never exited");
}

/// The round trip the whole prototype rests on: launch a trivial command, watch
/// it run, read its merged output through both log paths, harvest a file it
/// wrote, and reclaim everything.
#[tokio::test]
async fn launch_inspect_logs_copy_and_remove() {
    let root = temp_root("roundtrip");
    let backend = backend(&root);
    let workspace = root.join("workspace");
    let marker = workspace.join("eval-result.json");

    let gate = root.join("release-roundtrip");
    let mut config = cfg(&format!(
        "mkdir -p {}; echo out-line; echo err-line >&2; printf '{{\"pass\":true}}' > {}; {}; exit 3",
        workspace.display(),
        marker.display(),
        hold_until_released(&gate)
    ));
    config.files = vec![InjectedFile {
        container_path: root.join("injected.txt").display().to_string(),
        contents: b"injected".to_vec(),
        mode: 0o600,
        artifact: None,
    }];
    let id = backend.launch(config).await.unwrap();
    assert!(id.starts_with("w1/"), "ids stay {{node}}/{{task}}: {id}");
    assert_eq!(
        backend.inspect(&id).await.unwrap(),
        Some(ContainerStatus::Running)
    );

    let running = backend.list_managed_running().await.unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].id, id);
    assert_eq!(running[0].project.as_deref(), Some("acme/chug"));
    assert_eq!((running[0].job, running[0].task), (Some(309), Some(2)));
    assert_eq!(backend.managed_running_total().await.unwrap(), 1);

    release(&gate);
    assert_eq!(settle(&backend, &id).await, 3, "the exit status survives");
    assert!(backend.list_managed_running().await.unwrap().is_empty());
    assert!(backend.list_managed_exited().await.unwrap().contains(&id));

    let logs = backend.logs(&id).await.unwrap();
    let text = String::from_utf8_lossy(&logs).to_string();
    assert!(
        text.contains("out-line") && text.contains("err-line"),
        "{text}"
    );

    let tail = backend.logs_tail(&id, 0).await.unwrap();
    assert_eq!(tail.data, logs);
    assert_eq!(tail.offset, logs.len() as u64);
    let rest = backend.logs_tail(&id, 4).await.unwrap();
    assert_eq!(rest.data, logs[4..], "offsets address the same bytes");
    let past = backend.logs_tail(&id, 9_999).await.unwrap();
    assert!(past.data.is_empty());
    assert_eq!(past.offset, logs.len() as u64);

    let harvested = backend.copy_file(&id, &marker.display().to_string()).await;
    assert_eq!(harvested.unwrap(), Some(b"{\"pass\":true}".to_vec()));
    assert_eq!(
        backend
            .copy_file(&id, &root.join("nope").display().to_string())
            .await
            .unwrap(),
        None,
        "an absent artifact is silence, never an error"
    );

    backend.remove(&id).await.unwrap();
    assert_eq!(backend.inspect(&id).await.unwrap(), None);
    assert!(!workspace.exists(), "remove reclaims the workspace");
    assert!(
        !root.join("injected.txt").exists(),
        "remove reclaims exactly the paths the launch wrote"
    );
    backend.remove(&id).await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

/// #309 §2 option (iii) enforced, not assumed: a second concurrent launch is
/// refused as transient `NoCapacity` — the §3.5 shape that queues and retries
/// without spending the job's retry budget — and admitted once the first exits.
#[tokio::test]
async fn one_host_task_at_a_time() {
    let root = temp_root("exclusion");
    let backend = backend(&root);

    let gate = root.join("release-exclusion");
    let first = backend
        .launch(cfg(&hold_until_released(&gate)))
        .await
        .unwrap();
    let err = backend.launch(cfg("true")).await.unwrap_err();
    assert!(
        matches!(err, container::BackendError::NoCapacity(_)),
        "a busy host node is transient, never a hard failure: {err}"
    );
    assert!(err.to_string().contains(&first), "names the holder: {err}");

    release(&gate);
    assert_eq!(settle(&backend, &first).await, 0);
    let second = backend.launch(cfg("true")).await.unwrap();
    assert_ne!(second, first, "each launch mints its own task");
    assert_eq!(settle(&backend, &second).await, 0);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A killed task dies as a **group** and reports a non-zero status, so a §3.5
/// timeout kill on a host node is as terminal as it is in container mode.
#[tokio::test]
async fn kill_stops_the_process_group() {
    let root = temp_root("kill");
    let backend = backend(&root);

    let id = backend.launch(cfg("sleep 60")).await.unwrap();
    backend.kill(&id).await.unwrap();
    assert_ne!(settle(&backend, &id).await, 0, "a killed task never passes");
    backend.remove(&id).await.unwrap();

    let missing = "w1/host-0-0".to_string();
    assert!(
        backend.kill(&missing).await.is_err(),
        "an unknown id is not found"
    );
    assert_eq!(backend.inspect(&missing).await.unwrap(), None);
    std::fs::remove_dir_all(&root).unwrap();
}

/// The pid-identity rule (#309 §2(b)) across a daemon restart, which is the
/// failure this prototype most needs to get right: a task whose process is gone
/// without an `exit_code` must read as EXITED, because a false "running" is what
/// §3.6 hears as "re-attach" and hangs until `task_timeout`.
#[tokio::test]
async fn a_task_lost_to_a_restart_reads_as_exited_not_running() {
    let root = temp_root("restart");
    let tasks = root.join("tasks");
    let id = {
        let backend = backend(&root);
        let id = backend.launch(cfg("sleep 30")).await.unwrap();
        assert_eq!(
            backend.inspect(&id).await.unwrap(),
            Some(ContainerStatus::Running)
        );
        id
    };
    let task = id.split_once('/').unwrap().1;

    let meta_path = tasks.join(task).join("meta.json");
    let raw = std::fs::read_to_string(&meta_path).unwrap();
    let mut meta: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let pid = meta["pid"].as_i64().unwrap();
    meta["start_time"] = serde_json::Value::String("0".into());
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();

    let restarted = backend(&root);
    assert_eq!(
        restarted.inspect(&id).await.unwrap(),
        Some(ContainerStatus::Exited { exit_code: -1 }),
        "pid {pid} is live but is not this task's process"
    );
    assert!(
        restarted.list_managed_running().await.unwrap().is_empty(),
        "a recycled pid must never hold a fleet slot"
    );
    assert!(restarted.list_managed_exited().await.unwrap().contains(&id));

    restarted.kill(&id).await.unwrap();
    restarted.remove(&id).await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

/// The two constants a host node's whole path story rests on, asserted so a
/// silent edit to either is a red test rather than a transcript harvest that
/// finds nothing (#309 §2(a)).
#[test]
fn the_workspace_and_root_paths_are_the_documented_ones() {
    assert_eq!(
        HOST_WORKSPACE, "/workspace",
        "bootstrap_cmd clones here and agent::transcript_path's -workspace slug is derived from it"
    );
    assert!(HOST_ROOT_DEFAULT.starts_with('/'));
    assert!(!HOST_ROOT_DEFAULT.starts_with("/nix/store/"));
}
