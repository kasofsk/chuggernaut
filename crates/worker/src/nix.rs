//! Node-side nix realise and per-task GC roots (design #373 P1).
//!
//! accepts: a task id and the node-declared toolchain path a launch will be
//! given; emits: one indirect GC root per task under the node's roots
//! directory, and its removal at task exit; guarantees: the realise is bounded
//! and fails the launch loudly on expiry, and the stale-root reaper is
//! best-effort — it leaks disk rather than ever failing a job (spec §3.1
//! "Node-local nix GC roots").
//!
//! The realise and the root are ONE action: `nix-store --add-root … --indirect
//! --realise` cannot leave a realised closure unrooted, which two calls could.

use std::collections::HashSet;
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
        if let Some(target) = realise_target {
            store_target(target, &self.store_dir)?;
        }
        Ok(())
    }

    /// This task's root path, or `None` when the id cannot name one. Named by
    /// task id so a stale root says whose it was.
    pub fn root_path(&self, task_id: &str) -> Option<PathBuf> {
        is_root_safe(task_id).then(|| self.gcroots_dir.join(format!("{ROOT_PREFIX}{task_id}")))
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
        let root = self.root_path(task_id).ok_or_else(|| {
            format!("task id {task_id:?} cannot name a GC root (expected [A-Za-z0-9_-]+)")
        })?;
        debug_assert!(
            root.starts_with(&self.gcroots_dir),
            "a root lives in the node's roots dir"
        );
        let child = tokio::process::Command::new(&self.client)
            .arg("--add-root")
            .arg(&root)
            .arg("--indirect")
            .arg("--realise")
            .arg(target)
            .env("NIX_REMOTE", "daemon")
            .env("NIX_DAEMON_SOCKET_PATH", &self.daemon_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                format!(
                    "spawning nix client {}: {e} (design #373 3b: the client comes from the \
                     node's profiles, through the mounted store)",
                    self.client.display()
                )
            })?;
        let outcome = tokio::time::timeout(self.realise_timeout, child.wait_with_output()).await;
        self.finish_realise(task_id, target, root, outcome)
    }

    /// The verdict half of [`realise`](Self::realise): keep the root only for a
    /// client that actually wrote one, and drop it on every other path.
    fn finish_realise(
        &self,
        task_id: &str,
        target: &Path,
        root: PathBuf,
        outcome: Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed>,
    ) -> Result<PathBuf, String> {
        let target = target.display();
        let failed = match outcome {
            Err(_) => format!(
                "nix realise of {target} exceeded the node's realise bound \
                 (WORKER_NIX_REALISE_TIMEOUT_SECS={}s) and was killed — the launch is refused, \
                 never requeued as capacity (design #373 3c)",
                self.realise_timeout.as_secs()
            ),
            Ok(Err(e)) => format!("nix realise of {target}: {e}"),
            Ok(Ok(output)) if !output.status.success() => format!(
                "nix realise of {target} exited {}: {}",
                output.status,
                error_tail(&output.stderr)
            ),
            Ok(Ok(_)) if root.symlink_metadata().is_err() => format!(
                "nix realise of {target} reported success but wrote no GC root at {} — the \
                 closure would be collectable mid-task (design #373 Decision 4)",
                root.display()
            ),
            Ok(Ok(_)) => return Ok(root),
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

    fn roots_for(dir: &Path, client: PathBuf, timeout: Duration) -> NixRoots {
        NixRoots {
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

        let root = roots.realise("7", &toolchain).await.unwrap();
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

        let err = roots
            .realise("11", &toolchain)
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
        let err = roots.realise("3", &toolchain).await.unwrap_err();
        assert!(err.contains("path does not exist"), "{err}");

        let liar = fake_client(&dir, "liar", "#!/bin/sh\nexit 0\n");
        let roots = roots_for(&dir, liar, Duration::from_secs(30));
        let err = roots.realise("3", &toolchain).await.unwrap_err();
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
