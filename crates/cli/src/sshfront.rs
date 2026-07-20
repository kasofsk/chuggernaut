//! The SSH front (spec §5.2): `chuggernaut ssh-shell` runs as every
//! certificate's forced command, `chuggernaut ssh-authz` as every bare
//! repo's pre-receive hook.
//!
//! sshd authenticates the cert against `TrustedUserCAKeys` and runs the
//! forced command embedded at signing time (identity args below);
//! `ssh-shell` parses `SSH_ORIGINAL_COMMAND`, gates repo entry, and execs
//! the real git service with the identity exported for the hook. The hook
//! enforces the per-ref push table — that split exists because receive-pack
//! only learns which refs are updated after the pack arrives.

use anyhow::{Context, Result, bail};
use auth::ssh::{
    self, CertAccess, GitService, Principal, authorize_pull, authorize_push_entry,
    authorize_ref_push, parse_git_command,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use types::ProjectRole;

#[derive(clap::Args, Debug)]
pub struct SshShellArgs {
    /// `user` | `job` | `dispatcher` — informational; authorization derives
    /// from the principal itself.
    #[arg(long)]
    pub kind: String,
    /// §5.2 principal: email, `job:{owner}/{project}:{seq}`, or `dispatcher`.
    #[arg(long)]
    pub principal: String,
    /// `rw` | `ro` (per-job certs; §7.4).
    #[arg(long, default_value = "rw")]
    pub access: String,
    /// base64url(JSON) of the user's project_roles claim.
    #[arg(long)]
    pub roles: Option<String>,
    #[arg(long, env = "REPOS_ROOT", default_value = "/data/repos")]
    pub repos_root: PathBuf,
}

fn decode_roles(encoded: Option<&str>) -> Result<HashMap<String, ProjectRole>> {
    match encoded {
        None => Ok(HashMap::new()),
        Some(b64) => {
            let bytes = URL_SAFE_NO_PAD
                .decode(b64.as_bytes())
                .context("roles base64")?;
            serde_json::from_slice(&bytes).context("roles json")
        }
    }
}

/// Forced command entry point. On success this never returns — it exits the
/// process with the git service's status.
pub async fn run_shell(args: SshShellArgs) -> Result<()> {
    let original = std::env::var("SSH_ORIGINAL_COMMAND")
        .ok()
        .filter(|c| !c.is_empty())
        .context("interactive access is not allowed (no SSH_ORIGINAL_COMMAND)")?;
    let (service, owner, project) = parse_git_command(&original)
        .with_context(|| format!("unsupported command {original:?}"))?;

    let principal = Principal::parse(&args.principal);
    let access = CertAccess::parse(&args.access)
        .with_context(|| format!("bad --access {:?}", args.access))?;
    let roles = decode_roles(args.roles.as_deref())?;

    let allowed = match service {
        GitService::UploadPack => authorize_pull(&principal, &roles, &owner, &project),
        GitService::ReceivePack => {
            authorize_push_entry(&principal, access, &roles, &owner, &project)
        }
    };
    if !allowed {
        bail!(
            "access denied: {} may not {} {owner}/{project}",
            args.principal,
            match service {
                GitService::UploadPack => "read",
                GitService::ReceivePack => "write to",
            }
        );
    }

    let repo = args.repos_root.join(&owner).join(format!("{project}.git"));
    if !repo.is_dir() {
        bail!("no such repository: {owner}/{project}");
    }

    let status = tokio::process::Command::new(service.command())
        .arg(&repo)
        .env(ssh::ENV_PRINCIPAL, &args.principal)
        .env(ssh::ENV_ACCESS, access.as_str())
        .env(ssh::ENV_ROLES, args.roles.as_deref().unwrap_or_default())
        .env(ssh::ENV_REPO, format!("{owner}/{project}"))
        .status()
        .await
        .with_context(|| format!("spawning {}", service.command()))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Pre-receive hook entry point: cwd is the bare repo, stdin carries
/// `<old> <new> <refname>` lines. No identity env → trusted local access
/// (the dispatcher operates on the repos directly) → allow.
pub async fn run_authz() -> Result<()> {
    let Ok(principal_str) = std::env::var(ssh::ENV_PRINCIPAL) else {
        return Ok(());
    };
    let principal = Principal::parse(&principal_str);
    let access = std::env::var(ssh::ENV_ACCESS)
        .ok()
        .and_then(|a| CertAccess::parse(&a))
        .unwrap_or(CertAccess::ReadOnly);
    let roles = decode_roles(
        std::env::var(ssh::ENV_ROLES)
            .ok()
            .filter(|r| !r.is_empty())
            .as_deref(),
    )?;
    let repo = std::env::var(ssh::ENV_REPO).context(ssh::ENV_REPO)?;
    let (owner, project) = repo
        .split_once('/')
        .with_context(|| format!("bad {} {repo:?}", ssh::ENV_REPO))?;

    let default_branch = default_branch(Path::new(".")).await?;

    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(stdin));
    while let Some(line) = lines.next_line().await? {
        let mut fields = line.split_whitespace();
        let (Some(_old), Some(_new), Some(refname)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("malformed ref update line {line:?}");
        };
        if !authorize_ref_push(
            &principal,
            access,
            &roles,
            owner,
            project,
            refname,
            &default_branch,
        ) {
            bail!("push to {refname} denied for {principal_str}");
        }
    }
    Ok(())
}

async fn default_branch(repo: &Path) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo)
        .output()
        .await
        .context("running git symbolic-ref")?;
    if !out.status.success() {
        bail!(
            "git symbolic-ref failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Re-exported: the body generator lives with the identity contract it bakes
/// (`auth::ssh`), the installer with the repo layout (`vcs`).
pub use auth::ssh::pre_receive_hook_body;

/// Install the pre-receive hook into a bare repo.
pub async fn install_pre_receive_hook(
    repos_root: &Path,
    owner: &str,
    project: &str,
    chuggernaut_bin: &Path,
) -> Result<()> {
    vcs::RepoManager::new(repos_root)
        .install_pre_receive_hook(owner, project, &pre_receive_hook_body(chuggernaut_bin))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip() {
        let roles = HashMap::from([("acme/api".to_string(), ProjectRole::Member)]);
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&roles).unwrap());
        assert_eq!(decode_roles(Some(&encoded)).unwrap(), roles);
        assert!(decode_roles(None).unwrap().is_empty());
        assert!(decode_roles(Some("!!!")).is_err());
    }

    #[test]
    fn hook_body_is_a_shell_script() {
        let body = pre_receive_hook_body(Path::new("/usr/local/bin/chuggernaut"));
        assert!(body.starts_with("#!/bin/sh\n"));
        assert!(body.contains("/usr/local/bin/chuggernaut ssh-authz"));
        // Local access (no identity env) must not depend on the baked path.
        assert!(body.contains("[ -z \"$CHUGGERNAUT_PRINCIPAL\" ] && exit 0"));
    }
}
