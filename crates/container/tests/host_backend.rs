//! Tier-2: `HostBackend` against real processes on the test machine (design
//! #309 P0). No Docker and no NATS — a host task is a process group and a
//! directory, so this suite runs everywhere, including a Docker-less evaluator.
//!
//! Every backend here is given its own root **and its own workspace path**, so
//! the suite never touches the machine's real `/workspace`.
//!
//! Three tests are the exception and say so on the way past: design #440 D3's
//! transient scope needs a systemd with a cgroup-v2 hierarchy that this
//! evaluator does not have, so they self-skip loudly through `scope_or_skip`
//! rather than certifying the mechanism vacuously.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use container::host::{HOST_ROOT_DEFAULT, HOST_WORKSPACE, HostBackend, Supervision};
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
    HostBackend::with_workspace(
        "w1",
        root.join("tasks"),
        root.join("workspace"),
        Supervision::ProcessGroup,
    )
    .unwrap()
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

/// One task's `meta.json` as the launch wrote it.
fn meta_of(root: &std::path::Path, id: &str) -> serde_json::Value {
    let task = id.split_once('/').unwrap().1;
    serde_json::from_str(
        &std::fs::read_to_string(root.join("tasks").join(task).join("meta.json")).unwrap(),
    )
    .unwrap()
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
    assert!(
        meta_of(&root, &id)["unit"].is_null(),
        "a node whose mechanism is the process group records no unit, so kill signals no scope"
    );
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

/// A task reads its own environment and sees the composed set, not the
/// daemon's (design #440 slice 1). The shell adds `PWD` and friends of its own
/// after `execve`, so those are allowed and everything else must have been
/// declared.
#[tokio::test]
async fn a_task_sees_the_composed_environment_and_not_the_daemons() {
    let root = temp_root("env");
    let backend = backend(&root);
    let dump = root.join("env.dump");

    let composed = ["PATH", "HOME", "JOB_PROJECT", "JOB_ID", "CHUG_TASK_ID"];
    let shell_added = ["PWD", "OLDPWD", "SHLVL", "_", "IFS"];
    let daemon_only = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .find(|k| !composed.contains(&k.as_str()) && !shell_added.contains(&k.as_str()))
        .expect("the test process has an environment of its own to leak");

    let id = backend
        .launch(cfg(&format!("env > {}", dump.display())))
        .await
        .unwrap();
    assert_eq!(settle(&backend, &id).await, 0);

    let dumped = std::fs::read_to_string(&dump).unwrap();
    let seen: HashMap<&str, &str> = dumped
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();

    assert_eq!(seen.get("JOB_PROJECT"), Some(&"acme/chug"));
    assert_eq!(seen.get("JOB_ID"), Some(&"309"));
    assert_eq!(seen.get("CHUG_TASK_ID"), Some(&"2"));
    assert!(seen.contains_key("CHUG_HOST_EXIT"), "{dumped}");
    assert!(seen.contains_key("CHUG_HOST_EXIT_TMP"), "{dumped}");
    assert_eq!(
        seen.get("PATH").copied(),
        std::env::var("PATH").ok().as_deref(),
        "PATH is the daemon's, so the clone finds the node's git"
    );
    assert_eq!(
        seen.get("HOME").copied(),
        std::env::var("HOME").ok().as_deref()
    );
    assert!(
        !seen.contains_key(daemon_only.as_str()),
        "{daemon_only} is the daemon's environment and must not reach the task: {dumped}"
    );

    let undeclared: Vec<&str> = seen
        .keys()
        .copied()
        .filter(|k| {
            !shell_added.contains(k) && !k.starts_with("CHUG_HOST_EXIT") && !composed.contains(k)
        })
        .collect();
    assert!(undeclared.is_empty(), "undeclared: {undeclared:?}");

    backend.remove(&id).await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

/// Whether this machine can create the transient scope design #440 D3 requires,
/// announcing the skip when it cannot. A vacuous pass here would certify the one
/// mechanism slices 3–8 of that design are built on.
async fn scope_or_skip(test: &str) -> bool {
    let announce = |reason: String| {
        eprintln!(
            "skipping {test}: {reason}, so design #440 D3's Linux assertion is NOT covered by this \
             run — the macOS half is operator-verified, see \
             docs/reference/runbooks/macos-host-supervision-proof.md"
        );
        false
    };
    match container::host::probe_supervision().await {
        Ok(Supervision::Scope) => {
            cgroup_of(std::process::id().into()).is_some()
                || announce(
                    "this machine reports no unified (cgroup v2) hierarchy, so a scope's cgroup \
                     cannot be read back"
                        .to_string(),
                )
        }
        outcome => announce(format!(
            "this machine cannot create a transient systemd scope ({outcome:?})"
        )),
    }
}

/// The cgroup one live pid sits in, read off the cgroup-v2 line of
/// `/proc/<pid>/cgroup`. `None` on a machine with no unified hierarchy, which is
/// a skip rather than a failed assertion.
fn cgroup_of(pid: i64) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    raw.lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::to_string)
}

/// The cgroup of a pid the caller is holding alive, on a machine `scope_or_skip`
/// has already confirmed has a unified hierarchy.
fn live_cgroup(pid: i64) -> String {
    cgroup_of(pid).unwrap_or_else(|| panic!("no cgroup v2 line for live pid {pid}"))
}

/// The process group of one live pid, out of field 5 of `/proc/<pid>/stat` —
/// what makes "this process left the group" an observation rather than an
/// assumption.
fn pgid_of(pid: i64) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    rest.split_whitespace().nth(2)?.parse().ok()
}

/// Whether a pid is a **live** process, so a zombie awaiting its reaper is never
/// read as one that survived.
fn alive(pid: i64) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat
        .rfind(')')
        .and_then(|end| stat.get(end + 1..))
        .and_then(|rest| rest.split_whitespace().next());
    !matches!(state, None | Some("Z"))
}

/// Whether this machine can stage a `setsid()` escapee at all, announcing the
/// skip in `scope_or_skip`'s words when it cannot.
fn setsid_or_skip(test: &str) -> bool {
    let usable = std::process::Command::new("setsid")
        .arg("true")
        .status()
        .is_ok_and(|s| s.success());
    if !usable {
        eprintln!(
            "skipping {test}: this machine has no usable setsid, so design #440 D8's escapee \
             assertion is NOT covered by this run"
        );
    }
    usable
}

/// One `systemctl` verb, and whether it succeeded.
fn systemctl(args: &[&str]) -> bool {
    std::process::Command::new("systemctl")
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

/// A pid a launched process recorded for itself, waited for rather than raced.
fn wait_for_pid(path: &std::path::Path) -> i64 {
    for _ in 0..400 {
        let recorded = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| raw.trim().parse().ok());
        if let Some(pid) = recorded {
            return pid;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!(
        "the stand-in daemon never recorded a task pid at {}",
        path.display()
    );
}

/// A stand-in for the daemon's own unit: a scope holding a shell that launches
/// one task through the composition `HostBackend` ships, and records its pid.
fn stand_in_daemon(
    daemon_unit: &str,
    task_unit: &str,
    pidfile: &std::path::Path,
) -> std::process::Child {
    let inner = container::host::supervised_launch(
        Supervision::Scope,
        task_unit,
        ["sleep", "120"].map(String::from).to_vec(),
    )
    .join(" ");
    std::process::Command::new("systemd-run")
        .args([
            "--scope",
            "--quiet",
            "--collect",
            &format!("--unit={daemon_unit}"),
            "--",
            "sh",
            "-c",
            &format!(
                "{inner} & printf %s \"$!\" > {}; sleep 120",
                pidfile.display()
            ),
        ])
        .spawn()
        .unwrap()
}

/// Half one of design #440 D3: the backend launches a task into a transient
/// scope of its own, whose cgroup is **not** inside the launching process's — so
/// a kill of the launcher's cgroup cannot reach it.
#[tokio::test]
async fn a_host_task_runs_in_its_own_supervision_unit() {
    if !scope_or_skip("a_host_task_runs_in_its_own_supervision_unit").await {
        return;
    }
    let root = temp_root("scope");
    let backend = HostBackend::with_workspace(
        "w1",
        root.join("tasks"),
        root.join("workspace"),
        Supervision::Scope,
    )
    .unwrap();

    let gate = root.join("release-scope");
    let id = backend
        .launch(cfg(&hold_until_released(&gate)))
        .await
        .unwrap();
    let task = id.split_once('/').unwrap().1;
    let meta = meta_of(&root, &id);
    let unit = meta["unit"].as_str().unwrap().to_string();
    assert_eq!(unit, format!("chug-task-{task}.scope"));

    let task_cgroup = live_cgroup(meta["pid"].as_i64().unwrap());
    let launcher_cgroup = live_cgroup(std::process::id().into());
    assert!(
        task_cgroup.ends_with(&unit),
        "the task runs in its own unit: {task_cgroup}"
    );
    if launcher_cgroup != "/" {
        assert!(
            !task_cgroup.starts_with(&format!("{}/", launcher_cgroup.trim_end_matches('/'))),
            "a task inside the launcher's cgroup dies with it: {task_cgroup} under {launcher_cgroup}"
        );
    }

    release(&gate);
    assert_eq!(settle(&backend, &id).await, 0);
    backend.remove(&id).await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

/// Design #440 D8, the half that took code: a `setsid()` escapee leaves the
/// task's process group and stays in its cgroup, so the group signal cannot
/// reach it and `kill` ends it only by signalling the scope by name (#309 §2).
#[tokio::test]
async fn a_kill_reaches_a_setsid_escapee_through_the_scope() {
    let test = "a_kill_reaches_a_setsid_escapee_through_the_scope";
    if !scope_or_skip(test).await || !setsid_or_skip(test) {
        return;
    }
    let root = temp_root("escapee");
    let backend = HostBackend::with_workspace(
        "w1",
        root.join("tasks"),
        root.join("workspace"),
        Supervision::Scope,
    )
    .unwrap();

    let pidfile = root.join("escapee.pid");
    let id = backend
        .launch(cfg(&format!(
            "setsid sh -c 'printf %s \"$$\" > {}; sleep 120' & sleep 120",
            pidfile.display()
        )))
        .await
        .unwrap();
    let meta = meta_of(&root, &id);
    let unit = meta["unit"].as_str().unwrap().to_string();

    let escapee = wait_for_pid(&pidfile);
    assert_ne!(
        pgid_of(escapee),
        Some(meta["pgid"].as_i64().unwrap()),
        "setsid left the escapee in the task's process group, so nothing was tested"
    );
    assert!(
        live_cgroup(escapee).ends_with(&unit),
        "leaving the process group does not leave the cgroup — that is what D8 rests on"
    );

    backend.kill(&id).await.unwrap();
    for _ in 0..200 {
        if !alive(escapee) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !alive(escapee),
        "the escapee outlived a kill of its task: the scope signal is the only one that could \
         reach it, and it did not (#440 D8)"
    );

    assert_ne!(settle(&backend, &id).await, 0, "a killed task never passes");
    backend.remove(&id).await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

/// **The crux of design #440** (D3), half two: tearing down the unit that
/// launched a host task leaves the task running. This is the assertion slices
/// 3–8 assume and spec §3.1's drain guarantee rests on in host mode.
#[tokio::test]
async fn a_host_task_survives_the_teardown_of_the_launching_unit() {
    if !scope_or_skip("a_host_task_survives_the_teardown_of_the_launching_unit").await {
        return;
    }
    let root = temp_root("teardown");
    let daemon_unit = format!("chug-proof-daemon-{}.scope", std::process::id());
    let task_unit = format!("chug-proof-task-{}.scope", std::process::id());
    let pidfile = root.join("stand-in.pid");
    let mut stand_in = stand_in_daemon(&daemon_unit, &task_unit, &pidfile);

    let task_pid = wait_for_pid(&pidfile);
    assert!(
        live_cgroup(task_pid).ends_with(&task_unit),
        "the stand-in's task is in its own scope"
    );
    assert!(
        systemctl(&["kill", "--signal=SIGKILL", &daemon_unit]),
        "the stand-in daemon's unit could not be torn down, so nothing was proven"
    );
    assert!(
        !stand_in.wait().unwrap().success(),
        "the stand-in daemon survived the teardown of its own unit"
    );
    assert!(
        alive(task_pid),
        "the task died with the unit that launched it — #440 D3 does NOT hold on this machine, \
         and the whole native-daemon program rests on it"
    );
    assert!(
        live_cgroup(task_pid).ends_with(&task_unit),
        "and it is still the scope's, not reparented into the launcher's"
    );

    systemctl(&["kill", "--signal=SIGKILL", &task_unit]);
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
