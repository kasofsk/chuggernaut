//! Tier-2: §12.1 keypair generation on its own. Needs no broker and no Docker —
//! `ensure_all` only touches the filesystem and shells out to
//! `openssl`/`ssh-keygen`, present on any dev or deploy host.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn oidc_keypair_is_generated_separate_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keys");

    let first = cli::keygen::ensure_all(&keys).await.unwrap();
    assert!(first.generated.contains(&"oidc_private.pem".to_string()));
    assert!(first.generated.contains(&"oidc_public.pem".to_string()));

    let private = keys.join("oidc_private.pem");
    let public = keys.join("oidc_public.pem");
    assert_eq!(
        mode(&private),
        0o600,
        "the issuer private half must be 0600"
    );
    assert_ne!(
        std::fs::read(&private).unwrap(),
        std::fs::read(keys.join("jwt_private.pem")).unwrap(),
        "the issuer key must not be the session-signing key (#313 A2)"
    );

    let kid = auth::oidc::kid_from_public_pem(&std::fs::read_to_string(&public).unwrap()).unwrap();
    let before = (
        std::fs::read(&private).unwrap(),
        std::fs::read(&public).unwrap(),
    );

    let second = cli::keygen::ensure_all(&keys).await.unwrap();
    assert!(
        second.generated.is_empty(),
        "re-init generated {:?}",
        second.generated
    );
    assert!(second.skipped.contains(&"oidc_private.pem".to_string()));
    assert_eq!(before.0, std::fs::read(&private).unwrap());
    assert_eq!(before.1, std::fs::read(&public).unwrap());
    assert_eq!(mode(&private), 0o600);
    assert_eq!(
        kid,
        auth::oidc::kid_from_public_pem(&std::fs::read_to_string(&public).unwrap()).unwrap()
    );
}
