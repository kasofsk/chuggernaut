//! §12.1 keypair generation. Idempotent: each key is skipped if its private
//! file already exists; a missing public file is re-derived from the private.
//!
//! JWT/VAPID keys shell out to `openssl` and the SSH CA to `ssh-keygen` — the
//! same standard tooling the deployment host needs anyway (sshd, TLS); only
//! the age key is generated in-process (the dispatcher consumes it directly).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub struct KeygenReport {
    pub generated: Vec<String>,
    pub skipped: Vec<String>,
}

#[allow(
    clippy::too_many_lines,
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
pub async fn ensure_all(dir: &Path) -> Result<KeygenReport> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    let mut report = KeygenReport {
        generated: vec![],
        skipped: vec![],
    };

    ensure(&mut report, dir, "jwt_private.pem", |path| async move {
        run(
            "openssl",
            &[
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out",
                path.to_str().unwrap(),
            ],
        )
        .await
    })
    .await?;
    ensure(&mut report, dir, "jwt_public.pem", |path| async move {
        let private = path.with_file_name("jwt_private.pem");
        run(
            "openssl",
            &[
                "pkey",
                "-in",
                private.to_str().unwrap(),
                "-pubout",
                "-out",
                path.to_str().unwrap(),
            ],
        )
        .await
    })
    .await?;

    let ssh_ca_fresh = !dir.join("ssh_ca").exists();
    ensure(&mut report, dir, "ssh_ca", |path| async move {
        run(
            "ssh-keygen",
            &[
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "chuggernaut-ssh-ca",
                "-f",
                path.to_str().unwrap(),
            ],
        )
        .await
    })
    .await?;
    if ssh_ca_fresh {
        report.generated.push("ssh_ca.pub".into());
    } else {
        ensure(&mut report, dir, "ssh_ca.pub", |path| async move {
            let private = path.with_file_name("ssh_ca");
            let pubkey = run("ssh-keygen", &["-y", "-f", private.to_str().unwrap()]).await?;
            write_key(&path, &pubkey, false).await
        })
        .await?;
    }

    ensure(&mut report, dir, "age_private.key", |path| async move {
        let (identity, _) = store::secrets::generate_age_keypair();
        write_key(&path, &format!("{identity}\n"), true).await
    })
    .await?;
    ensure(&mut report, dir, "age_public.key", |path| async move {
        let identity = tokio::fs::read_to_string(path.with_file_name("age_private.key")).await?;
        let public = store::secrets::age_public_from_identity(&identity)?;
        write_key(&path, &format!("{public}\n"), false).await
    })
    .await?;

    ensure(&mut report, dir, "age_artifacts.key", |path| async move {
        let (identity, _) = store::secrets::generate_age_keypair();
        write_key(&path, &format!("{identity}\n"), true).await
    })
    .await?;
    ensure(
        &mut report,
        dir,
        "age_artifacts_public.key",
        |path| async move {
            let identity =
                tokio::fs::read_to_string(path.with_file_name("age_artifacts.key")).await?;
            let public = store::secrets::age_public_from_identity(&identity)?;
            write_key(&path, &format!("{public}\n"), false).await
        },
    )
    .await?;

    ensure(&mut report, dir, "nats_operator.seed", |path| async move {
        let kp = nkeys::KeyPair::new_operator();
        write_key(&path, &format!("{}\n", seed_of(&kp)?), true).await
    })
    .await?;
    for name in ["nats_sys_account.seed", "nats_account.seed"] {
        ensure(&mut report, dir, name, |path| async move {
            let kp = nkeys::KeyPair::new_account();
            write_key(&path, &format!("{}\n", seed_of(&kp)?), true).await
        })
        .await?;
    }

    ensure(&mut report, dir, "vapid_private.pem", |path| async move {
        run(
            "openssl",
            &[
                "ecparam",
                "-name",
                "prime256v1",
                "-genkey",
                "-noout",
                "-out",
                path.to_str().unwrap(),
            ],
        )
        .await
    })
    .await?;
    ensure(&mut report, dir, "vapid_public.pem", |path| async move {
        let private = path.with_file_name("vapid_private.pem");
        run(
            "openssl",
            &[
                "ec",
                "-in",
                private.to_str().unwrap(),
                "-pubout",
                "-out",
                path.to_str().unwrap(),
            ],
        )
        .await
    })
    .await?;

    for private in [
        "jwt_private.pem",
        "ssh_ca",
        "age_private.key",
        "vapid_private.pem",
    ] {
        restrict(&dir.join(private)).await?;
    }

    ensure_nats_artifacts(dir, &mut report).await?;
    Ok(report)
}

/// Artifacts derived from the NATS seeds, re-created when missing:
/// `nats-resolver.conf` (mounted into the NATS server) and
/// `dispatcher.creds` (unrestricted platform-account user, mounted into the
/// dispatcher).
async fn ensure_nats_artifacts(dir: &Path, report: &mut KeygenReport) -> Result<()> {
    async fn seed(dir: &Path, name: &str) -> Result<nkeys::KeyPair> {
        let path = dir.join(name);
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        nkeys::KeyPair::from_seed(text.trim()).map_err(|e| anyhow::anyhow!("parsing {name}: {e}"))
    }
    let operator = seed(dir, "nats_operator.seed").await?;
    let sys = seed(dir, "nats_sys_account.seed").await?;
    let account = seed(dir, "nats_account.seed").await?;

    ensure(report, dir, "nats-resolver.conf", |path| async move {
        let conf = auth::nats::resolver_config(
            &auth::nats::operator_jwt(&operator, "chuggernaut")?,
            &sys.public_key(),
            &auth::nats::system_account_jwt(&operator, &sys.public_key())?,
            &account.public_key(),
            &auth::nats::account_jwt(&operator, &account.public_key(), "CHUG")?,
        );
        write_key(&path, &conf, false).await
    })
    .await?;

    ensure(report, dir, "dispatcher.creds", |path| async move {
        let signer = auth::nats::NatsUserSigner::from_account_seed(
            &seed_of_str(dir, "nats_account.seed").await?,
        )?;
        let creds = signer.mint_creds("dispatcher", &auth::nats::Permissions::default(), None)?;
        write_key(&path, &creds, true).await
    })
    .await?;
    Ok(())
}

async fn seed_of_str(dir: &Path, name: &str) -> Result<String> {
    Ok(tokio::fs::read_to_string(dir.join(name))
        .await?
        .trim()
        .to_string())
}

fn seed_of(kp: &nkeys::KeyPair) -> Result<String> {
    kp.seed().map_err(|e| anyhow::anyhow!("nkey seed: {e}"))
}

async fn ensure<F, Fut>(
    report: &mut KeygenReport,
    dir: &Path,
    name: &str,
    generate: F,
) -> Result<()>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let path = dir.join(name);
    if path.exists() {
        report.skipped.push(name.to_string());
    } else {
        generate(path)
            .await
            .with_context(|| format!("generating {name}"))?;
        report.generated.push(name.to_string());
    }
    Ok(())
}

async fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("running {program} (is it installed?)"))?;
    if !out.status.success() {
        bail!(
            "{program} {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(crate) async fn write_key(path: &Path, contents: &str, private: bool) -> Result<String> {
    tokio::fs::write(path, contents).await?;
    if private {
        restrict(path).await?;
    }
    Ok(String::new())
}

async fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .with_context(|| format!("chmod 600 {}", path.display()))?;
    Ok(())
}
