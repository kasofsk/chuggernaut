//! Node-side nix realise and per-task GC roots (design #373 P1, P2).
//!
//! accepts: a task id, and either the node-declared toolchain path a launch
//! will be given (P1) or the project-declared `runtime.env` it carries (P2);
//! emits: one indirect GC root per task under the node's roots directory, the
//! realised store path a task is pointed at, and the root's removal at task
//! exit; guarantees: the realise is bounded and fails the launch loudly on
//! expiry, only an allow-listed project's environment is ever realised, and the
//! stale-root reaper is best-effort — it leaks disk rather than ever failing a
//! job (spec §3.1 "Node-local nix GC roots", "Project-declared toolchains").
//!
//! The realise and the root are ONE action in both shapes: `nix-store
//! --add-root … --indirect --realise` and `nix build --out-link` each register
//! the root as they realise, which two calls could not.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Prefix of every root this node writes, so a stale root left by a killed
/// worker is *greppable* rather than an invisible pin (design #373 Decision 4).
pub const ROOT_PREFIX: &str = "task-";

/// How long an unclaimed root must have existed before the reaper takes it. Far
/// longer than the gap between a root's creation and its container appearing in
/// the node's own listing, so a launch in flight is never reaped.
pub const REAP_AGE_MIN: Duration = Duration::from_secs(3600);

/// Iteration cap on one reaper pass (STYLE.md Tier 2 rule 3): a roots directory
/// somehow holding more than this is swept across several passes rather than
/// blocking the daemon on one.
const REAP_ENTRIES_MAX: usize = 4096;

/// How much of a failed client's stderr rides in the launch failure.
const ERROR_TAIL_CHARS: usize = 400;

/// The node's nix realise, and the GC roots it holds over what it realised
/// (design #373 Decision 4). A node property assembled from the worker's own
/// config — it never rides the wire or the launch request.
#[derive(Debug, Clone)]
pub struct NixRoots {
    /// The client the realise runs, resolved through the node's profiles so it
    /// cannot be collected by an old-generation GC (design #373 3b).
    pub client: PathBuf,
    /// The flake-aware client a project-declared `runtime.env` is built with
    /// (design #373 P2), resolved through the profiles for the same reason
    /// [`Self::client`] is. `nix-store --realise` takes store paths, never a
    /// flake ref, so this is `nix build`'s binary rather than a second lifecycle.
    pub flake_client: PathBuf,
    /// Projects allowed to have this node realise their declared `runtime.env`
    /// (`WORKER_NIX_PROJECTS`, design #373 Decision 2 rule 3). Empty grants
    /// nobody: a job type asks for an environment, it never asks for a privilege.
    pub projects: Vec<String>,
    /// The node's git key, handed to the flake fetch as `GIT_SSH_COMMAND` so a
    /// `git+ssh://` ref resolves; `None` leaves the fetch to whatever credential
    /// the node's own environment carries.
    pub git_key: Option<PathBuf>,
    /// Worker-writable directory the roots are written to, at the same path
    /// inside the daemon container as on the host — the daemon registers the
    /// indirect root by the path it is given.
    pub gcroots_dir: PathBuf,
    /// The node's nix daemon socket; the daemon does the work, so builders stay
    /// sandboxed as `nixbld` users rather than running as the worker.
    pub daemon_socket: PathBuf,
    /// The store prefix a realise target must resolve into, in the daemon's own
    /// view. The client canonicalizes before the daemon hears anything, so a
    /// target that lands outside it cannot be realised at all.
    pub store_dir: PathBuf,
    /// Bound on one realise (design #373 3c). The realise precedes execution, so
    /// no `task_timeout` covers it.
    pub realise_timeout: Duration,
}

/// The scheme a container-mode environment reference must carry (design #309
/// §9); `xcode:` is host-only and refused by `JobType::validate`.
pub const NIX_ENV_PREFIX: &str = "nix:";

/// One realised project environment: the root holding it, and the store path the
/// task is pointed at.
#[derive(Debug, Clone)]
pub struct Realised {
    pub root: PathBuf,
    pub path: PathBuf,
}

/// One root as the reaper sees it: where it is, whose task it names, and how
/// long it has existed.
#[derive(Debug, Clone)]
pub struct RootEntry {
    pub path: PathBuf,
    pub task_id: String,
    pub age: Duration,
}

impl NixRoots {
    /// Refuse the boot when the node's preconditions are absent from the
    /// daemon's OWN view — the view a containerized worker actually has (design
    /// #367 correction 12's lesson, applied to nix). `realise_target` is the
    /// node-declared toolchain a launch will realise, held to the property the
    /// client needs rather than to mere existence.
    pub fn check(&self, realise_target: Option<&Path>) -> Result<(), String> {
        if !self.gcroots_dir.is_dir() {
            return Err(format!(
                "WORKER_NIX_GCROOTS_DIR {} is not a directory in the daemon's own view — the \
                 node provisions it and chug-worker mounts it read-write at the same path \
                 (design #373 Decision 4)",
                self.gcroots_dir.display()
            ));
        }
        if !self.client.exists() {
            return Err(format!(
                "WORKER_NIX_CLIENT {} is absent in the daemon's own view — mount /nix/store \
                 read-only and the node's profiles into chug-worker (design #373 3b)",
                self.client.display()
            ));
        }
        if !self.daemon_socket.exists() {
            return Err(format!(
                "WORKER_NIX_DAEMON_SOCKET {} is absent in the daemon's own view — mount it \
                 READ-WRITE (connecting to a unix socket needs write on the inode)",
                self.daemon_socket.display()
            ));
        }
        if !self.projects.is_empty() && !self.flake_client.exists() {
            return Err(format!(
                "WORKER_NIX_FLAKE_CLIENT {} is absent in the daemon's own view, and \
                 WORKER_NIX_PROJECTS grants {:?} project-declared toolchains this node could \
                 not realise (design #373 P2)",
                self.flake_client.display(),
                self.projects
            ));
        }
        if let Some(target) = realise_target {
            store_target(target, &self.store_dir)?;
        }
        Ok(())
    }

    /// Whether this node realises the declared environment of the project a
    /// launch names, matched on `JOB_PROJECT` exactly as the KVM grant is
    /// (design #373 Decision 2 rule 3). An empty allow-list admits nobody.
    pub fn admits(&self, env: &HashMap<String, String>) -> bool {
        env.get("JOB_PROJECT")
            .is_some_and(|project| self.projects.iter().any(|allowed| allowed == project))
    }

    /// This task's root path, or `None` when the id cannot name one. Named by
    /// task id so a stale root says whose it was.
    pub fn root_path(&self, task_id: &str) -> Option<PathBuf> {
        is_root_safe(task_id).then(|| self.gcroots_dir.join(format!("{ROOT_PREFIX}{task_id}")))
    }

    /// This task's root path, or the launch refusal an operator reads. One root
    /// per task, so a launch realises its project's environment or the node's
    /// declared toolchain — never both under one name.
    fn root_or_refuse(&self, task_id: &str) -> Result<PathBuf, String> {
        let root = self.root_path(task_id).ok_or_else(|| {
            format!("task id {task_id:?} cannot name a GC root (expected [A-Za-z0-9_-]+)")
        })?;
        debug_assert!(
            root.starts_with(&self.gcroots_dir),
            "a root lives in the node's roots dir"
        );
        Ok(root)
    }

    /// Realise `target` and register the root over it in one bounded action,
    /// leaving no root behind on any failure. The error is the launch failure an
    /// operator reads, so it names the bound it broke.
    pub async fn realise(&self, task_id: &str, target: &Path) -> Result<PathBuf, String> {
        debug_assert!(
            !self.realise_timeout.is_zero(),
            "a zero bound would refuse every realise"
        );
        let target = &store_target(target, &self.store_dir)?;
        let root = self.root_or_refuse(task_id)?;
        let child = self.spawn_client(
            &self.client,
            vec![
                OsString::from("--add-root"),
                root.clone().into_os_string(),
                OsString::from("--indirect"),
                OsString::from("--realise"),
                target.clone().into_os_string(),
            ],
        )?;
        let outcome = tokio::time::timeout(self.realise_timeout, child.wait_with_output()).await;
        self.finish_realise(task_id, &target.display().to_string(), &root, outcome)?;
        Ok(root)
    }

    /// Realise the environment a job type declares and root it in the same
    /// action (design #373 P2, 3a), under the same bound every realise gets.
    /// `installable` is already resolved against the job branch — see
    /// [`flake_installable`].
    pub async fn realise_env(&self, task_id: &str, installable: &str) -> Result<Realised, String> {
        debug_assert!(
            !installable.is_empty(),
            "a resolved installable is never empty"
        );
        let root = self.root_or_refuse(task_id)?;
        let child = self.spawn_client(
            &self.flake_client,
            vec![
                OsString::from("build"),
                OsString::from("--extra-experimental-features"),
                OsString::from("nix-command flakes"),
                OsString::from("--no-write-lock-file"),
                OsString::from("--print-out-paths"),
                OsString::from("--out-link"),
                root.clone().into_os_string(),
                OsString::from(installable),
            ],
        )?;
        let outcome = tokio::time::timeout(self.realise_timeout, child.wait_with_output()).await;
        let output = self.finish_realise(task_id, installable, &root, outcome)?;
        match env_out_path(&output.stdout, &self.store_dir) {
            Ok(path) => Ok(Realised { root, path }),
            Err(e) => {
                self.release(task_id);
                Err(format!("nix build of {installable} {e}"))
            }
        }
    }

    /// One bounded nix child against the node's daemon, so every realise runs
    /// the same way. The node's git key rides as `GIT_SSH_COMMAND` because a
    /// project's flake ref is fetched from the platform's own git front.
    fn spawn_client(
        &self,
        client: &Path,
        args: Vec<OsString>,
    ) -> Result<tokio::process::Child, String> {
        let mut command = tokio::process::Command::new(client);
        command
            .args(args)
            .env("NIX_REMOTE", "daemon")
            .env("NIX_DAEMON_SOCKET_PATH", &self.daemon_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(key) = &self.git_key {
            command.env(
                "GIT_SSH_COMMAND",
                format!(
                    "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new",
                    key.display()
                ),
            );
        }
        command.spawn().map_err(|e| {
            format!(
                "spawning nix client {}: {e} (design #373 3b: the client comes from the \
                 node's profiles, through the mounted store)",
                client.display()
            )
        })
    }

    /// The verdict half of [`realise`](Self::realise): keep the root only for a
    /// client that actually wrote one, and drop it on every other path.
    fn finish_realise(
        &self,
        task_id: &str,
        target: &str,
        root: &Path,
        outcome: Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed>,
    ) -> Result<std::process::Output, String> {
        let failed = match outcome {
            Err(_) => format!(
                "nix realise of {target} exceeded the node's realise bound \
                 (WORKER_NIX_REALISE_TIMEOUT_SECS={}s) and was killed — the launch is refused, \
                 never requeued as capacity (design #373 3c)",
                self.realise_timeout.as_secs()
            ),
            Ok(Err(e)) => format!("nix realise of {target}: {e}"),
            Ok(Ok(output)) => {
                if !output.status.success() {
                    format!(
                        "nix realise of {target} exited {}: {}",
                        output.status,
                        error_tail(&output.stderr)
                    )
                } else if root.symlink_metadata().is_err() {
                    format!(
                        "nix realise of {target} reported success but wrote no GC root at {} — \
                         the closure would be collectable mid-task (design #373 Decision 4)",
                        root.display()
                    )
                } else {
                    return Ok(output);
                }
            }
        };
        self.release(task_id);
        Err(failed)
    }

    /// Drop one task's root at task exit — the lifecycle `platform-ops`'s
    /// `dispose` drives, reaching this node over the existing `remove` op.
    /// Best-effort: a failed removal leaks disk, never a job.
    pub fn release(&self, task_id: &str) {
        let Some(root) = self.root_path(task_id) else {
            return;
        };
        match std::fs::remove_file(&root) {
            Ok(()) => tracing::debug!(root = %root.display(), "nix GC root released"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(root = %root.display(), "releasing nix GC root failed: {e}");
            }
        }
    }

    /// Reap roots whose task is neither live nor recent (design #373 Correction
    /// C5: container mode has no task dir for a root to die with). Best-effort
    /// and bounded — it returns how many it removed and never fails anything.
    pub fn reap(&self, live: &HashSet<String>, age_min: Duration) -> usize {
        let entries = match self.entries() {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(dir = %self.gcroots_dir.display(), "nix GC root reaper skipped: {e}");
                return 0;
            }
        };
        let mut removed = 0;
        for path in reap_plan(&entries, live, age_min) {
            match std::fs::remove_file(path) {
                Ok(()) => {
                    removed += 1;
                    tracing::info!(root = %path.display(), "reaped a stale nix GC root");
                }
                Err(e) => tracing::warn!(root = %path.display(), "reaping stale root failed: {e}"),
            }
        }
        debug_assert!(
            removed <= entries.len(),
            "the reaper removes what it listed"
        );
        removed
    }

    /// The roots this node currently holds, bounded by [`REAP_ENTRIES_MAX`]. An
    /// entry the daemon cannot stat is skipped rather than failing the pass.
    fn entries(&self) -> Result<Vec<RootEntry>, String> {
        let dir = std::fs::read_dir(&self.gcroots_dir)
            .map_err(|e| format!("reading {}: {e}", self.gcroots_dir.display()))?;
        let now = std::time::SystemTime::now();
        let mut entries = Vec::new();
        for entry in dir.take(REAP_ENTRIES_MAX) {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(task_id) = name.strip_prefix(ROOT_PREFIX) else {
                continue;
            };
            let age = entry
                .path()
                .symlink_metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .unwrap_or_default();
            entries.push(RootEntry {
                path: entry.path(),
                task_id: task_id.to_string(),
                age,
            });
        }
        Ok(entries)
    }
}

/// The store path a realise target resolves to in the daemon's OWN view, or why
/// it cannot be realised at all. The property is checked rather than existence,
/// because a bind of the operator's stable path resolves that symlink host-side
/// and leaves a plain directory at a non-store path — which the nix client
/// refuses, having canonicalized before the daemon hears anything (design #373
/// 3b).
fn store_target(target: &Path, store_dir: &Path) -> Result<PathBuf, String> {
    let resolved = target.canonicalize().map_err(|e| {
        format!(
            "the toolchain this node realises ({}) does not resolve in the daemon's own view \
             ({e}) — chug-worker must mount the PARENT of that host path read-only, so the \
             operator's symlink into {} survives into the container (design #373 3b)",
            target.display(),
            store_dir.display()
        )
    })?;
    if !resolved.starts_with(store_dir) || resolved == store_dir {
        return Err(format!(
            "the toolchain this node realises ({}) resolves to {} in the daemon's own view, \
             which is not under {} — `nix-store --realise` canonicalizes CLIENT-side and \
             refuses a non-store path, so mount the stable path's PARENT read-only rather \
             than the path itself, and declare it as a direct symlink into the store \
             (design #373 3b)",
            target.display(),
            resolved.display(),
            store_dir.display()
        ));
    }
    Ok(resolved)
}

/// The flake installable a launch's declared `runtime.env` resolves to (design
/// #373 3a): a **relative** ref is rewritten against the job branch's own
/// repository at `sha`, and every other ref passes through untouched. `rev=`
/// rides beside `ref=` because a branch tip moves under a launch in flight.
pub fn flake_installable(
    env_ref: &str,
    repo_url: &str,
    branch: &str,
    sha: Option<&str>,
) -> Result<String, String> {
    let installable = env_ref.strip_prefix(NIX_ENV_PREFIX).ok_or_else(|| {
        format!(
            "runtime.env {env_ref:?} does not name a nix environment (expected \
             '{NIX_ENV_PREFIX}<flake-ref>#<attr>') — this node serves no other scheme in \
             container mode (design #373 Decision 2 rule 1)"
        )
    })?;
    if installable.is_empty() {
        return Err(format!("runtime.env {env_ref:?} names no flake reference"));
    }
    let attr = match relative_attr(installable) {
        Some(attr) => attr?,
        None => return Ok(installable.to_string()),
    };
    if !is_url_safe(repo_url) || !is_url_safe(branch) {
        return Err(format!(
            "the relative reference {env_ref:?} resolves against branch {branch:?} of \
             {repo_url:?}, which carries a character a flake ref cannot (design #373 3a)"
        ));
    }
    let rev = match sha.filter(|s| !s.is_empty()) {
        Some(sha) if is_commit_sha(sha) => format!("&rev={sha}"),
        Some(sha) => return Err(format!("the job branch commit {sha:?} is not a commit sha")),
        None => String::new(),
    };
    Ok(format!("git+{repo_url}?ref={branch}{rev}{attr}"))
}

/// The `#attr` half of a relative reference (`.` or `.#attr`), or `None` when
/// the reference is absolute. A relative form nix would resolve against the
/// worker's own working directory is refused rather than passed through, since
/// there is no checkout on the host to resolve against (design #373 3a).
fn relative_attr(installable: &str) -> Option<Result<&str, String>> {
    if installable == "." {
        return Some(Ok(""));
    }
    if installable.starts_with(".#") {
        return Some(Ok(&installable[1..]));
    } else if installable.starts_with('.') {
        return Some(Err(format!(
            "the relative reference {installable:?} is not the '.#<attr>' form container mode \
             resolves against the job branch (design #373 3a)"
        )));
    }
    None
}

/// Whether a value may be spliced into a flake ref's URL: no separator of the
/// ref grammar itself, so nothing a launch carries can redirect the fetch.
fn is_url_safe(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['?', '#', '&', ' ', '"', '\'', '\\'])
        && !value.contains("..")
}

/// Whether a value is a git commit sha — hex, and long enough to name one.
fn is_commit_sha(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// The store path `nix build --print-out-paths` reported, held to the same
/// property a realise target is: a path outside the store is not something a
/// task can be pointed at.
fn env_out_path(stdout: &[u8], store_dir: &Path) -> Result<PathBuf, String> {
    let text = String::from_utf8_lossy(stdout);
    let path = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "reported success but printed no output path".to_string())?;
    if !Path::new(path).starts_with(store_dir) {
        return Err(format!(
            "printed {path:?}, which is not under {}",
            store_dir.display()
        ));
    }
    Ok(PathBuf::from(path))
}

/// The reap decision, pure: a root goes only when no live task claims it AND it
/// has outlived `age_min`. Both halves matter — the age is what keeps a root
/// created seconds before its container exists from being reaped under it.
fn reap_plan<'a>(
    entries: &'a [RootEntry],
    live: &HashSet<String>,
    age_min: Duration,
) -> Vec<&'a Path> {
    entries
        .iter()
        .filter(|e| !live.contains(&e.task_id) && e.age >= age_min)
        .map(|e| e.path.as_path())
        .collect()
}

/// Whether a task id may name a root: the charset that stays one path segment,
/// so no launch env can ever write a root outside the node's roots directory.
fn is_root_safe(task_id: &str) -> bool {
    !task_id.is_empty()
        && task_id.len() <= 64
        && task_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The last [`ERROR_TAIL_CHARS`] characters of a failed client's stderr, for the
/// launch failure the operator reads.
fn error_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let start = text
        .char_indices()
        .rev()
        .take(ERROR_TAIL_CHARS)
        .last()
        .map_or(0, |(i, _)| i);
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// A fake nix client that behaves like `nix-store --add-root … --realise`:
    /// it writes the root symlink its second argument names.
    fn fake_client(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Canonicalized, because every realise target assertion compares a resolved
    /// path against the store prefix and a `/tmp` that is itself a symlink would
    /// make the two disagree for a reason no node has.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chug-nix-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// A stand-in for the node's stable toolchain path: a symlink into this
    /// temp node's store, the one shape the worker container can resolve.
    fn store_toolchain(dir: &Path) -> PathBuf {
        let realised = dir.join("store").join("aaaa-toolchain");
        std::fs::create_dir_all(&realised).unwrap();
        let stable = dir.join("toolchain");
        let _ = std::fs::remove_file(&stable);
        std::os::unix::fs::symlink(&realised, &stable).unwrap();
        stable
    }

    /// Retry the HARNESS's own `ETXTBSY` race: a sibling test writing its fake
    /// client while this one forks makes the exec fail, which is an artifact of
    /// executables written by the suite itself and never something a node does.
    async fn without_the_exec_race<F, Fut, T>(mut attempt: F) -> Result<T, String>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        for _ in 0..8 {
            match attempt().await {
                Err(e) if e.contains("Text file busy") => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                outcome => return outcome,
            }
        }
        attempt().await
    }

    fn roots_for(dir: &Path, client: PathBuf, timeout: Duration) -> NixRoots {
        NixRoots {
            flake_client: client.clone(),
            projects: vec!["acme/beacon".to_string()],
            git_key: None,
            client,
            gcroots_dir: dir.to_path_buf(),
            daemon_socket: dir.join("socket"),
            store_dir: dir.join("store"),
            realise_timeout: timeout,
        }
    }

    /// A root is named by task id under the node's roots dir, and an id that
    /// could escape that dir names no root at all — a launch env must never be
    /// able to write outside it.
    #[test]
    fn root_path_is_derived_from_the_task_id() {
        let roots = roots_for(
            Path::new("/var/lib/chuggernaut/gcroots"),
            PathBuf::from("/nix/var/nix/profiles/system/sw/bin/nix-store"),
            Duration::from_secs(60),
        );
        assert_eq!(
            roots.root_path("42"),
            Some(PathBuf::from("/var/lib/chuggernaut/gcroots/task-42"))
        );
        for bad in ["", "../escape", "a/b", "7 8", "task.1", &"9".repeat(65)] {
            assert_eq!(roots.root_path(bad), None, "must refuse {bad:?}");
        }
    }

    /// The reap decision (design #373 Decision 4): a live task's root stays, a
    /// young root stays — it may be a launch whose container does not exist yet
    /// — and only an old, unclaimed root goes.
    #[test]
    fn reap_plan_spares_live_and_young_roots() {
        let entry = |task: &str, age_secs: u64| RootEntry {
            path: PathBuf::from(format!("/roots/task-{task}")),
            task_id: task.to_string(),
            age: Duration::from_secs(age_secs),
        };
        let entries = [
            entry("live", 9_000),
            entry("young", 5),
            entry("stale", 9_000),
        ];
        let live = HashSet::from(["live".to_string()]);
        let plan = reap_plan(&entries, &live, Duration::from_secs(3600));
        assert_eq!(plan, vec![Path::new("/roots/task-stale")]);
        assert!(
            reap_plan(&entries, &HashSet::new(), Duration::from_secs(86_400)).is_empty(),
            "nothing is old enough for a day-long grace"
        );
    }

    /// The realise and the root are one action: a successful client leaves the
    /// root in place over the STORE path the stable path resolved to, and
    /// `release` — the lifecycle `dispose` drives — takes it away again, twice
    /// over without complaint.
    #[tokio::test]
    async fn realise_creates_the_root_and_release_removes_it() {
        let dir = temp_dir("root-lifecycle");
        let toolchain = store_toolchain(&dir);
        let client = fake_client(
            &dir,
            "nix-store",
            "#!/bin/sh\nln -sfn \"$5\" \"$2\"\nexit 0\n",
        );
        let roots = roots_for(&dir, client, Duration::from_secs(30));

        let root = without_the_exec_race(|| roots.realise("7", &toolchain))
            .await
            .unwrap();
        assert_eq!(root, dir.join("task-7"));
        assert!(root.symlink_metadata().is_ok(), "the root must exist");
        assert_eq!(
            std::fs::read_link(&root).unwrap(),
            dir.join("store").join("aaaa-toolchain"),
            "the root points at the store path, not the stable path"
        );

        roots.release("7");
        assert!(root.symlink_metadata().is_err(), "the root must be gone");
        roots.release("7");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A realise past the node's bound fails the LAUNCH, loudly and by name
    /// (design #373 3c) — the daemon maps this to `WorkerError::Launch`, never
    /// `NoCapacity` — and leaves no half-made root behind.
    #[tokio::test]
    async fn realise_over_the_bound_fails_naming_the_bound() {
        let dir = temp_dir("bound");
        let toolchain = store_toolchain(&dir);
        let client = fake_client(&dir, "nix-store", "#!/bin/sh\nsleep 30\n");
        let roots = roots_for(&dir, client, Duration::from_millis(200));

        let err = without_the_exec_race(|| roots.realise("11", &toolchain))
            .await
            .expect_err("an over-bound realise fails");
        assert!(
            err.contains("WORKER_NIX_REALISE_TIMEOUT_SECS"),
            "the failure must name the bound: {err}"
        );
        assert!(!dir.join("task-11").exists(), "no root may survive: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A client that fails carries its own stderr into the launch failure, and a
    /// client that "succeeds" without writing a root is refused too — an
    /// unrooted closure is exactly the hazard this exists to close.
    #[tokio::test]
    async fn realise_failure_names_the_cause() {
        let dir = temp_dir("failure");
        let toolchain = store_toolchain(&dir);
        let broken = fake_client(
            &dir,
            "broken",
            "#!/bin/sh\necho 'error: path does not exist' >&2\nexit 1\n",
        );
        let roots = roots_for(&dir, broken, Duration::from_secs(30));
        let err = without_the_exec_race(|| roots.realise("3", &toolchain))
            .await
            .unwrap_err();
        assert!(err.contains("path does not exist"), "{err}");

        let liar = fake_client(&dir, "liar", "#!/bin/sh\nexit 0\n");
        let roots = roots_for(&dir, liar, Duration::from_secs(30));
        let err = without_the_exec_race(|| roots.realise("3", &toolchain))
            .await
            .unwrap_err();
        assert!(err.contains("wrote no GC root"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reaper over a real roots directory: the stale root goes, the live
    /// one stays, and a foreign file is untouched.
    #[test]
    fn reaper_removes_a_stale_root_and_spares_a_live_one() {
        let dir = temp_dir("reap");
        for name in ["task-live", "task-stale", "not-a-root"] {
            std::os::unix::fs::symlink("/nix/store/whatever", dir.join(name)).unwrap();
        }
        let roots = roots_for(&dir, PathBuf::from("/bin/true"), Duration::from_secs(30));

        let live = HashSet::from(["live".to_string()]);
        assert_eq!(roots.reap(&live, Duration::ZERO), 1);
        assert!(dir.join("task-live").symlink_metadata().is_ok());
        assert!(dir.join("not-a-root").symlink_metadata().is_ok());
        assert!(dir.join("task-stale").symlink_metadata().is_err());

        assert_eq!(
            roots.reap(&live, REAP_AGE_MIN),
            0,
            "a fresh root is never old enough for the shipped grace"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R3's rewrite (design #373 3a): a RELATIVE ref resolves against the job
    /// branch's own repository at the commit the launch carries, so a toolchain
    /// bump and the code needing it are the same commit — and `rev=` rides
    /// beside `ref=` because a branch tip moves under a launch in flight.
    #[test]
    fn a_relative_env_resolves_against_the_job_branch_and_its_commit() {
        let url = "ssh://git@front:2222/acme/beacon.git";
        let sha = "4b84d2596f0e2b1c0a9a7d3e2f1c0b9a8d7e6f5a";
        assert_eq!(
            flake_installable("nix:.#chug-mobile", url, "job/403", Some(sha)).unwrap(),
            format!("git+{url}?ref=job/403&rev={sha}#chug-mobile")
        );
        assert_eq!(
            flake_installable("nix:.#chug-mobile", url, "job/403", None).unwrap(),
            format!("git+{url}?ref=job/403#chug-mobile")
        );
        assert_eq!(
            flake_installable("nix:.", url, "main", Some(sha)).unwrap(),
            format!("git+{url}?ref=main&rev={sha}")
        );
        assert_eq!(
            flake_installable("nix:.#chug-ci", url, "job/403", Some("")).unwrap(),
            format!("git+{url}?ref=job/403#chug-ci"),
            "an empty sha is no sha, not a broken one"
        );
    }

    /// An ABSOLUTE ref is the node's to fetch verbatim — the whole point of the
    /// rewrite is that only `.#attr` has no checkout to resolve against — and
    /// every shape the node cannot serve is refused by name rather than handed
    /// to nix to fail on.
    #[test]
    fn an_absolute_env_passes_through_and_the_unservable_is_refused() {
        let url = "ssh://git@front:2222/acme/beacon.git";
        for absolute in [
            "github:acme/toolchains#chug-mobile",
            "git+https://example.invalid/t.git?ref=v2#chug-ci",
        ] {
            assert_eq!(
                flake_installable(&format!("nix:{absolute}"), url, "job/403", None).unwrap(),
                absolute
            );
        }

        let err = flake_installable("xcode:16.2", url, "job/403", None).unwrap_err();
        assert!(err.contains("does not name a nix environment"), "{err}");
        assert!(flake_installable("nix:", url, "job/403", None).is_err());

        let err = flake_installable("nix:./sub#a", url, "job/403", None).unwrap_err();
        assert!(err.contains("'.#<attr>'"), "{err}");
        let err = flake_installable("nix:.#a", url, "job/403", Some("nope")).unwrap_err();
        assert!(err.contains("not a commit sha"), "{err}");
        assert!(
            flake_installable("nix:.#a", url, "job/403?x=1", Some(&"a".repeat(40))).is_err(),
            "nothing a launch carries may redirect the fetch"
        );
        assert!(flake_installable("nix:.#a", "", "job/403", None).is_err());
    }

    /// The node-side allow-list (design #373 Decision 2 rule 3): a job type asks
    /// for an environment, never for a privilege, so an empty list grants
    /// NOBODY and only the named project is admitted.
    #[test]
    fn the_env_allow_list_is_fail_closed() {
        let mut roots = roots_for(
            Path::new("/var/lib/chuggernaut/gcroots"),
            PathBuf::from("/bin/true"),
            Duration::from_secs(30),
        );
        let env = |project: &str| HashMap::from([("JOB_PROJECT".to_string(), project.to_string())]);

        assert!(roots.admits(&env("acme/beacon")));
        assert!(!roots.admits(&env("acme/other")));
        assert!(!roots.admits(&HashMap::new()), "no project is not a match");

        roots.projects = Vec::new();
        assert!(
            !roots.admits(&env("acme/beacon")),
            "an empty allow-list grants nobody"
        );
    }

    /// A project-declared environment is realised and rooted in ONE action, the
    /// task is pointed at what was realised, and `release` takes the root away
    /// again — the same lifecycle the node's own toolchain gets.
    #[tokio::test]
    async fn realise_env_roots_the_closure_and_reports_its_path() {
        let dir = temp_dir("env-realise");
        let client = fake_client(
            &dir,
            "nix",
            "#!/bin/sh\nout=\"$(dirname \"$7\")/store/aaaa-env\"\nmkdir -p \"$out/bin\"\n\
             ln -sfn \"$out\" \"$7\"\necho \"$out\"\n",
        );
        let roots = roots_for(&dir, client, Duration::from_secs(30));

        let realised = without_the_exec_race(|| {
            roots.realise_env("9", "git+ssh://front/acme/beacon.git?ref=job/403#chug-ci")
        })
        .await
        .unwrap();
        assert_eq!(realised.root, dir.join("task-9"));
        assert_eq!(realised.path, dir.join("store").join("aaaa-env"));
        assert!(
            realised.root.symlink_metadata().is_ok(),
            "the root must exist"
        );

        roots.release("9");
        assert!(realised.root.symlink_metadata().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A declared environment that breaks the node's bound fails the LAUNCH
    /// naming the bound and leaves NO root, and a client that reports a path
    /// outside the store is refused for the same reason a realise target is.
    #[tokio::test]
    async fn realise_env_over_the_bound_or_outside_the_store_leaves_no_root() {
        let dir = temp_dir("env-refusals");
        let slow = fake_client(&dir, "slow", "#!/bin/sh\nsleep 30\n");
        let roots = roots_for(&dir, slow, Duration::from_millis(200));
        let err = without_the_exec_race(|| roots.realise_env("4", "github:acme/t#ci"))
            .await
            .unwrap_err();
        assert!(err.contains("WORKER_NIX_REALISE_TIMEOUT_SECS"), "{err}");
        assert!(!dir.join("task-4").exists(), "no root may survive: {err}");

        let stray = fake_client(
            &dir,
            "stray",
            "#!/bin/sh\nln -sfn /tmp \"$7\"\necho /tmp/elsewhere\n",
        );
        let roots = roots_for(&dir, stray, Duration::from_secs(30));
        let err = without_the_exec_race(|| roots.realise_env("4", "github:acme/t#ci"))
            .await
            .unwrap_err();
        assert!(err.contains("not under"), "{err}");
        assert!(!dir.join("task-4").exists(), "no root may survive: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A node that allow-lists a project must be able to realise for it: a
    /// missing flake client refuses the BOOT rather than every launch, the same
    /// posture the store-path client already gets.
    #[test]
    fn the_boot_check_refuses_a_granted_node_without_a_flake_client() {
        let dir = temp_dir("check-flake");
        std::fs::write(dir.join("socket"), b"").unwrap();
        let mut roots = roots_for(&dir, PathBuf::from("/bin/true"), Duration::from_secs(30));
        roots.flake_client = PathBuf::from("/definitely/not/nix");

        let err = roots.check(None).expect_err("a granted node needs it");
        assert!(err.contains("WORKER_NIX_FLAKE_CLIENT"), "{err}");
        assert!(err.contains("acme/beacon"), "{err}");

        roots.projects = Vec::new();
        assert!(
            roots.check(None).is_ok(),
            "a node granting nobody realises no flake and needs no flake client"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reaper that cannot read its directory reports zero and moves on — it
    /// leaks disk, it never fails a job — and the boot check names the missing
    /// precondition rather than skipping silently.
    #[test]
    fn a_broken_reaper_is_quiet_and_the_boot_check_is_loud() {
        let roots = roots_for(
            Path::new("/definitely/not/a/roots/dir"),
            PathBuf::from("/bin/true"),
            Duration::from_secs(30),
        );
        assert_eq!(roots.reap(&HashSet::new(), Duration::ZERO), 0);
        roots.release("7");

        let err = roots
            .check(None)
            .expect_err("a missing roots dir refuses the boot");
        assert!(err.contains("WORKER_NIX_GCROOTS_DIR"), "{err}");

        let dir = temp_dir("check");
        let roots = roots_for(&dir, PathBuf::from("/definitely/not/nix"), Duration::MAX);
        let err = roots
            .check(None)
            .expect_err("a missing client refuses the boot");
        assert!(err.contains("WORKER_NIX_CLIENT"), "{err}");

        let roots = roots_for(&dir, PathBuf::from("/bin/true"), Duration::MAX);
        let err = roots
            .check(None)
            .expect_err("a missing socket refuses the boot");
        assert!(err.contains("WORKER_NIX_DAEMON_SOCKET"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The toolchain a launch realises must RESOLVE INTO THE STORE in the
    /// daemon's own view, not merely exist: binding the operator's stable path
    /// itself resolves that symlink host-side and leaves a plain directory the
    /// client refuses, so existence is exactly the property that would pass
    /// while every admitted launch failed.
    #[test]
    fn the_boot_check_refuses_a_realise_target_outside_the_store() {
        let dir = temp_dir("check-target");
        std::fs::write(dir.join("socket"), b"").unwrap();
        let roots = roots_for(&dir, PathBuf::from("/bin/true"), Duration::from_secs(30));

        let err = roots
            .check(Some(Path::new("/definitely/not/a/toolchain")))
            .expect_err("an unmounted realise target refuses the boot");
        assert!(err.contains("/definitely/not/a/toolchain"), "{err}");
        assert!(err.contains("PARENT"), "{err}");

        let leaf_bound = dir.join("android-sdk");
        std::fs::create_dir_all(&leaf_bound).unwrap();
        let err = roots
            .check(Some(&leaf_bound))
            .expect_err("a stable path bound as its own leaf is a NON-STORE directory");
        assert!(err.contains("is not under"), "{err}");
        assert!(
            err.contains(&dir.join("store").display().to_string()),
            "{err}"
        );

        assert!(
            roots.check(Some(&store_toolchain(&dir))).is_ok(),
            "a symlink into the store — what a parent bind preserves — passes"
        );
        assert!(roots.check(None).is_ok(), "a node that realises nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
