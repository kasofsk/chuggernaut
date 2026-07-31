//! End-to-end §5.2: real `git clone`/`git push` driven through
//! `chuggernaut ssh-shell` + the pre-receive hook, using git's `ext::`
//! transport in place of sshd (the transport hands the pack protocol to our
//! forced command over stdio exactly as sshd would).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::path::{Path, PathBuf};
use test_utils::repo::TempRepo;

const BIN: &str = env!("CARGO_BIN_EXE_chuggernaut");

struct Front {
    _repo: TempRepo,
    repos_root: PathBuf,
    scripts: tempfile::TempDir,
}

impl Front {
    async fn setup() -> Self {
        let repo = TempRepo::create("acme", "api").await;
        let repos_root = repo
            .bare_path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        cli::sshfront::install_pre_receive_hook(&repos_root, "acme", "api", Path::new(BIN))
            .await
            .unwrap();
        Self {
            _repo: repo,
            repos_root,
            scripts: tempfile::tempdir().unwrap(),
        }
    }

    /// An `ext::` remote URL that runs ssh-shell exactly as sshd would:
    /// fixed identity args (the cert's forced command) + SSH_ORIGINAL_COMMAND.
    fn remote(&self, name: &str, service: &str, identity_args: &str) -> String {
        let script = self.scripts.path().join(name);
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nexport SSH_ORIGINAL_COMMAND=\"{service} '/acme/api.git'\"\n\
                 exec {BIN} ssh-shell --repos-root {} {identity_args}\n",
                self.repos_root.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        format!("ext::{}", script.display())
    }
}

fn git(cwd: &Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new("git")
        .args(["-c", "protocol.ext.allow=always"])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("running git");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn roles_b64(role: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!(r#"{{"acme/api":"{role}"}}"#))
}

/// `container::bootstrap_cmd` clones with `--filter=blob:none`, which only
/// works over git protocol v2. sshd hands upload-pack `version=2` solely when
/// it is configured to `AcceptEnv GIT_PROTOCOL` (git supplies the client half
/// itself). Drop that line and every task container still "clones" fine, then
/// checks out an empty workspace — a silent, total breakage.
///
/// This asserts the dev deployment's config because the ext:: harness below
/// cannot cover it: ext:: never propagates GIT_PROTOCOL, so it is pinned to v0
/// regardless of sshd. Verified against the real sshd container by hand.
#[test]
fn dev_sshd_accepts_git_protocol_v2() {
    let sshd_config =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/dev/sshd_config");
    let text = std::fs::read_to_string(&sshd_config)
        .unwrap_or_else(|e| panic!("reading {}: {e}", sshd_config.display()));
    assert!(
        text.lines()
            .any(|l| l.split_whitespace().eq(["AcceptEnv", "GIT_PROTOCOL"])),
        "sshd_config must AcceptEnv GIT_PROTOCOL or partial clone silently \
         yields empty workspaces; see container::bootstrap_cmd"
    );
}

/// `--single-branch` is what `bootstrap_cmd` actually ships; it must work
/// through the forced command and fetch only the job's own ref.
#[tokio::test]
async fn single_branch_clone_works_over_the_ssh_front() {
    let front = Front::setup().await;
    let work = tempfile::tempdir().unwrap();
    let job_args = "--kind job --principal job:acme/api:1 --access rw";

    let pull = front.remote("sb-pull.sh", "git-upload-pack", job_args);
    let (ok, err) = git(work.path(), &["clone", &pull, "seed"]);
    assert!(ok, "seed clone: {err}");
    let seed = work.path().join("seed");
    std::fs::write(seed.join("f.txt"), "content").unwrap();
    git(&seed, &["add", "."]);
    git(
        &seed,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "c",
        ],
    );
    let push = front.remote("sb-push.sh", "git-receive-pack", job_args);
    let (ok, err) = git(&seed, &["push", &push, "HEAD:refs/heads/job/1"]);
    assert!(ok, "seed push: {err}");

    let (ok, err) = git(
        work.path(),
        &["clone", "--single-branch", "--branch", "job/1", &pull, "co"],
    );
    assert!(ok, "single-branch clone: {err}");
    let co = work.path().join("co");
    assert_eq!(
        std::fs::read_to_string(co.join("f.txt")).unwrap(),
        "content"
    );

    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(&co)
        .args(["for-each-ref", "--format=%(refname)", "refs/remotes/origin"])
        .output()
        .unwrap();
    let refs = String::from_utf8_lossy(&listed.stdout);
    assert!(refs.contains("job/1"), "{refs}");
    assert!(!refs.contains("origin/main"), "fetched main too: {refs}");
}

#[tokio::test]
async fn job_certs_clone_and_push_only_their_branch() {
    let front = Front::setup().await;
    let work = tempfile::tempdir().unwrap();

    let job_args = "--kind job --principal job:acme/api:1 --access rw";
    let pull = front.remote("job-pull.sh", "git-upload-pack", job_args);
    let (ok, err) = git(work.path(), &["clone", &pull, "co"]);
    assert!(ok, "clone should succeed: {err}");
    let co = work.path().join("co");

    std::fs::write(co.join("w.txt"), "work output").unwrap();
    git(&co, &["add", "."]);
    git(
        &co,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "w",
        ],
    );

    let push = front.remote("job-push.sh", "git-receive-pack", job_args);
    let (ok, err) = git(&co, &["push", &push, "HEAD:refs/heads/job/1"]);
    assert!(ok, "push to own branch should succeed: {err}");
    let (ok, err) = git(&co, &["push", &push, "HEAD:refs/heads/job/2"]);
    assert!(!ok);
    assert!(err.contains("denied"), "{err}");
    let (ok, err) = git(&co, &["push", &push, "HEAD:refs/heads/main"]);
    assert!(!ok);
    assert!(err.contains("denied"), "{err}");

    let eval_push = front.remote(
        "eval-push.sh",
        "git-receive-pack",
        "--kind job --principal job:acme/api:1 --access ro",
    );
    let (ok, err) = git(&co, &["push", &eval_push, "HEAD:refs/heads/job/1"]);
    assert!(!ok);
    assert!(err.contains("access denied"), "{err}");

    let foreign_pull = front.remote(
        "foreign-pull.sh",
        "git-upload-pack",
        "--kind job --principal job:acme/web:1 --access rw",
    );
    let (ok, err) = git(work.path(), &["clone", &foreign_pull, "co2"]);
    assert!(!ok);
    assert!(err.contains("access denied"), "{err}");
}

#[tokio::test]
async fn user_and_dispatcher_rules() {
    let front = Front::setup().await;
    let work = tempfile::tempdir().unwrap();

    let viewer = format!(
        "--kind user --principal d@e.com --roles {}",
        roles_b64("viewer")
    );
    let pull = front.remote("viewer-pull.sh", "git-upload-pack", &viewer);
    let (ok, err) = git(work.path(), &["clone", &pull, "co"]);
    assert!(ok, "viewer clone: {err}");
    let co = work.path().join("co");
    std::fs::write(co.join("u.txt"), "user change").unwrap();
    git(&co, &["add", "."]);
    git(
        &co,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "u",
        ],
    );

    let viewer_push = front.remote("viewer-push.sh", "git-receive-pack", &viewer);
    let (ok, err) = git(&co, &["push", &viewer_push, "HEAD:refs/heads/job/7"]);
    assert!(!ok);
    assert!(err.contains("access denied"), "{err}");

    let member = format!(
        "--kind user --principal d@e.com --roles {}",
        roles_b64("member")
    );
    let member_push = front.remote("member-push.sh", "git-receive-pack", &member);
    let (ok, err) = git(&co, &["push", &member_push, "HEAD:refs/heads/job/7"]);
    assert!(ok, "member push to job branch: {err}");
    let (ok, err) = git(&co, &["push", &member_push, "HEAD:refs/heads/main"]);
    assert!(!ok);
    assert!(err.contains("denied"), "{err}");
    git(&co, &["tag", "v1"]);
    let (ok, err) = git(&co, &["push", &member_push, "refs/tags/v1"]);
    assert!(!ok);
    assert!(err.contains("denied"), "{err}");

    let dispatcher_push = front.remote(
        "dispatcher-push.sh",
        "git-receive-pack",
        "--kind dispatcher --principal dispatcher",
    );
    let (ok, err) = git(&co, &["push", &dispatcher_push, "HEAD:refs/heads/main"]);
    assert!(ok, "dispatcher push to main: {err}");

    let clone =
        test_utils::repo::clone_branch_from(&front.repos_root.join("acme/api.git"), "main").await;
    clone.commit_file("local.txt", b"local", "local").await;
    clone.push("main").await;
}

/// §7.3 platform-admin bypass: an admin-flagged cert with NO role grants clones
/// and pushes to a job branch in any project, but the default branch stays
/// dispatcher-only even for an admin.
#[tokio::test]
async fn platform_admin_cert_bypasses_roles() {
    let front = Front::setup().await;
    let work = tempfile::tempdir().unwrap();

    let empty_roles = URL_SAFE_NO_PAD.encode("{}");
    let admin = format!("--kind user --principal admin@e.com --roles {empty_roles} --admin");

    let pull = front.remote("admin-pull.sh", "git-upload-pack", &admin);
    let (ok, err) = git(work.path(), &["clone", &pull, "co"]);
    assert!(ok, "admin clone with no role: {err}");
    let co = work.path().join("co");
    std::fs::write(co.join("a.txt"), "admin change").unwrap();
    git(&co, &["add", "."]);
    git(
        &co,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "a",
        ],
    );

    let admin_push = front.remote("admin-push.sh", "git-receive-pack", &admin);
    let (ok, err) = git(&co, &["push", &admin_push, "HEAD:refs/heads/job/9"]);
    assert!(ok, "admin push to job branch: {err}");
    let (ok, err) = git(&co, &["push", &admin_push, "HEAD:refs/heads/main"]);
    assert!(!ok);
    assert!(err.contains("denied"), "{err}");

    let no_role = format!("--kind user --principal admin@e.com --roles {empty_roles}");
    let no_role_pull = front.remote("norole-pull.sh", "git-upload-pack", &no_role);
    let (ok, err) = git(work.path(), &["clone", &no_role_pull, "co2"]);
    assert!(!ok);
    assert!(err.contains("access denied"), "{err}");
}

#[tokio::test]
async fn ssh_shell_rejects_garbage() {
    let front = Front::setup().await;
    for cmd in ["rm -rf /", "", "git-upload-archive '/acme/api.git'"] {
        let out = std::process::Command::new(BIN)
            .args([
                "ssh-shell",
                "--repos-root",
                front.repos_root.to_str().unwrap(),
                "--kind",
                "job",
                "--principal",
                "job:acme/api:1",
            ])
            .env("SSH_ORIGINAL_COMMAND", cmd)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{cmd:?} must be rejected");
    }
    let out = std::process::Command::new(BIN)
        .args([
            "ssh-shell",
            "--repos-root",
            front.repos_root.to_str().unwrap(),
            "--kind",
            "job",
            "--principal",
            "job:acme/nope:1",
        ])
        .env("SSH_ORIGINAL_COMMAND", "git-upload-pack '/acme/nope.git'")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such repository"));
}
