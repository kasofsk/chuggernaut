//! End-to-end §5.2: real `git clone`/`git push` driven through
//! `chuggernaut ssh-shell` + the pre-receive hook, using git's `ext::`
//! transport in place of sshd (the transport hands the pack protocol to our
//! forced command over stdio exactly as sshd would).

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
        let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
        cli::sshfront::install_pre_receive_hook(&repos_root, "acme", "api", Path::new(BIN))
            .await
            .unwrap();
        Self { _repo: repo, repos_root, scripts: tempfile::tempdir().unwrap() }
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
        // The ext:: transport is opt-in (it runs an arbitrary command).
        .args(["-c", "protocol.ext.allow=always"])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("running git");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn roles_b64(role: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!(r#"{{"acme/api":"{role}"}}"#))
}

#[tokio::test]
async fn job_certs_clone_and_push_only_their_branch() {
    let front = Front::setup().await;
    let work = tempfile::tempdir().unwrap();

    // Work container identity (rw): clone succeeds.
    let job_args = "--kind job --principal job:acme/api:1 --access rw";
    let pull = front.remote("job-pull.sh", "git-upload-pack", job_args);
    let (ok, err) = git(work.path(), &["clone", &pull, "co"]);
    assert!(ok, "clone should succeed: {err}");
    let co = work.path().join("co");

    std::fs::write(co.join("w.txt"), "work output").unwrap();
    git(&co, &["add", "."]);
    git(&co, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "w"]);

    let push = front.remote("job-push.sh", "git-receive-pack", job_args);
    // Own branch: allowed.
    let (ok, err) = git(&co, &["push", &push, "HEAD:refs/heads/job/1"]);
    assert!(ok, "push to own branch should succeed: {err}");
    // Another job's branch: denied by the pre-receive hook.
    let (ok, err) = git(&co, &["push", &push, "HEAD:refs/heads/job/2"]);
    assert!(!ok);
    assert!(err.contains("denied"), "{err}");
    // Protected default branch: denied.
    let (ok, err) = git(&co, &["push", &push, "HEAD:refs/heads/main"]);
    assert!(!ok);
    assert!(err.contains("denied"), "{err}");

    // Read-only (eval) certificate: receive-pack refused at entry.
    let eval_push = front.remote(
        "eval-push.sh",
        "git-receive-pack",
        "--kind job --principal job:acme/api:1 --access ro",
    );
    let (ok, err) = git(&co, &["push", &eval_push, "HEAD:refs/heads/job/1"]);
    assert!(!ok);
    assert!(err.contains("access denied"), "{err}");

    // A job cert for a different project cannot read this one.
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

    // Viewer: clone yes, push refused at entry.
    let viewer = format!("--kind user --principal d@e.com --roles {}", roles_b64("viewer"));
    let pull = front.remote("viewer-pull.sh", "git-upload-pack", &viewer);
    let (ok, err) = git(work.path(), &["clone", &pull, "co"]);
    assert!(ok, "viewer clone: {err}");
    let co = work.path().join("co");
    std::fs::write(co.join("u.txt"), "user change").unwrap();
    git(&co, &["add", "."]);
    git(&co, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "u"]);

    let viewer_push = front.remote("viewer-push.sh", "git-receive-pack", &viewer);
    let (ok, err) = git(&co, &["push", &viewer_push, "HEAD:refs/heads/job/7"]);
    assert!(!ok);
    assert!(err.contains("access denied"), "{err}");

    // Member: job branches yes, default branch and tags no.
    let member = format!("--kind user --principal d@e.com --roles {}", roles_b64("member"));
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

    // Dispatcher: protected refs allowed.
    let dispatcher_push = front.remote(
        "dispatcher-push.sh",
        "git-receive-pack",
        "--kind dispatcher --principal dispatcher",
    );
    let (ok, err) = git(&co, &["push", &dispatcher_push, "HEAD:refs/heads/main"]);
    assert!(ok, "dispatcher push to main: {err}");

    // No identity env (local file:// access, the dispatcher's own path):
    // the hook passes through even though it is installed.
    let clone = test_utils::repo::clone_branch_from(&front.repos_root.join("acme/api.git"), "main").await;
    clone.commit_file("local.txt", b"local", "local").await;
    clone.push("main").await;
}

#[tokio::test]
async fn ssh_shell_rejects_garbage() {
    let front = Front::setup().await;
    // Non-git and malformed commands are refused before any spawn.
    for cmd in ["rm -rf /", "", "git-upload-archive '/acme/api.git'"] {
        let out = std::process::Command::new(BIN)
            .args([
                "ssh-shell", "--repos-root",
                front.repos_root.to_str().unwrap(),
                "--kind", "job", "--principal", "job:acme/api:1",
            ])
            .env("SSH_ORIGINAL_COMMAND", cmd)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{cmd:?} must be rejected");
    }
    // Unknown repository.
    let out = std::process::Command::new(BIN)
        .args([
            "ssh-shell", "--repos-root",
            front.repos_root.to_str().unwrap(),
            "--kind", "job", "--principal", "job:acme/nope:1",
        ])
        .env("SSH_ORIGINAL_COMMAND", "git-upload-pack '/acme/nope.git'")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such repository"));
}
