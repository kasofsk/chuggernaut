//! `chuggernaut ssh-cert` — mint a user SSH certificate from the platform
//! (spec §7.3) so an operator can clone/fetch platform repos over the SSH
//! front. Submits `~/.ssh/id_ed25519.pub` to `POST /auth/ssh-cert` with the
//! caller's JWT and writes the returned cert next to the key as
//! `~/.ssh/id_ed25519-cert-chug.pub`, ready for an `~/.ssh/config` block
//! (`CertificateFile ~/.ssh/id_ed25519-cert-chug.pub`, `IdentitiesOnly yes`).
//!
//! v1 UX is manual refresh (spec §7.5, §1.9.2): re-run when the 24h cert
//! expires. No agent/daemon.

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
pub struct SshCertArgs {
    /// API base URL, e.g. `https://chug.example.com`.
    #[arg(long)]
    pub base_url: String,
    /// File holding the session JWT (what `admin user token` mints), sent as a
    /// `Authorization: Bearer` header.
    #[arg(long)]
    pub token_file: PathBuf,
    /// SSH public key to sign.
    #[arg(long, default_value = "~/.ssh/id_ed25519.pub")]
    pub public_key: String,
    /// Where to write the signed certificate.
    #[arg(long, default_value = "~/.ssh/id_ed25519-cert-chug.pub")]
    pub out: String,
}

/// Expand a leading `~/` against `$HOME`; leave everything else untouched.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

pub async fn run(args: SshCertArgs) -> anyhow::Result<()> {
    let token = std::fs::read_to_string(&args.token_file)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", args.token_file.display()))?
        .trim()
        .to_string();
    let pub_path = expand_home(&args.public_key);
    let public_key = std::fs::read_to_string(&pub_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", pub_path.display()))?
        .trim()
        .to_string();

    let url = format!("{}/auth/ssh-cert", args.base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "public_key": public_key }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("requesting {url}: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("decoding response from {url}: {e}"))?;
    if !status.is_success() {
        let msg = body
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("ssh-cert request failed ({status}): {msg}");
    }
    let cert = body
        .get("certificate")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow::anyhow!("response missing `certificate`"))?;

    let out_path = expand_home(&args.out);
    std::fs::write(&out_path, format!("{}\n", cert.trim()))
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", out_path.display()))?;
    println!("wrote {}", out_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn expands_leading_tilde() {
        // SAFETY: single-threaded test; no other thread reads $HOME here.
        unsafe { std::env::set_var("HOME", "/home/op") };
        assert_eq!(
            expand_home("~/.ssh/id_ed25519.pub"),
            PathBuf::from("/home/op/.ssh/id_ed25519.pub")
        );
        assert_eq!(expand_home("/etc/key.pub"), PathBuf::from("/etc/key.pub"));
        assert_eq!(expand_home("relative.pub"), PathBuf::from("relative.pub"));
    }
}
