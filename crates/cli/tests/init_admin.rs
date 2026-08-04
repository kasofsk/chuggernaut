//! Tier-2: `chuggernaut init` + admin commands against real NATS (Docker,
//! skip-guarded). Keygen shells out to openssl/ssh-keygen — present on any
//! dev/deploy host.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cli::admin::{
    self, AdminArgs, AdminCmd, CloudIdentityCmd, ProjectCmd, RoleCmd, ScopedName, UserCmd,
};
use cli::init::{self, InitArgs};
use store::NatsStore;
use types::User;

fn init_args(server_url: &str, dir: &std::path::Path) -> InitArgs {
    InitArgs {
        nats_url: server_url.to_string(),
        repos_root: dir.join("repos"),
        keys_dir: dir.join("keys"),
        admin_email: Some("root@example.com".into()),
        admin_password: Some("hunter2".into()),
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn init_bootstraps_and_is_idempotent() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    init::run(init_args(server.url(), dir.path()))
        .await
        .unwrap();

    let keys = dir.path().join("keys");
    for name in [
        "jwt_private.pem",
        "jwt_public.pem",
        "oidc_private.pem",
        "oidc_public.pem",
        "ssh_ca",
        "ssh_ca.pub",
        "age_private.key",
        "age_public.key",
        "vapid_private.pem",
        "vapid_public.pem",
        "age_artifacts.key",
        "age_artifacts_public.key",
        "nats_operator.seed",
        "nats_sys_account.seed",
        "nats_account.seed",
        "nats-resolver.conf",
        "dispatcher.creds",
    ] {
        assert!(keys.join(name).exists(), "missing {name}");
    }
    assert_ne!(
        std::fs::read_to_string(keys.join("age_private.key")).unwrap(),
        std::fs::read_to_string(keys.join("age_artifacts.key")).unwrap(),
        "artifacts key must not be the secrets key"
    );
    use std::os::unix::fs::PermissionsExt;
    for private in [
        "age_private.key",
        "age_artifacts.key",
        "nats_operator.seed",
        "dispatcher.creds",
    ] {
        let mode = std::fs::metadata(keys.join(private))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{private} not 0600");
    }
    let resolver = std::fs::read_to_string(keys.join("nats-resolver.conf")).unwrap();
    assert!(resolver.contains("resolver: MEMORY"));
    assert!(
        std::fs::read_to_string(keys.join("dispatcher.creds"))
            .unwrap()
            .contains("BEGIN NATS USER JWT")
    );

    let store = NatsStore::connect(server.url()).await.unwrap();
    store.jobs().await.unwrap();
    let platform = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    let vapid: String = platform.get_json("vapid.public").await.unwrap().unwrap();
    assert!(vapid.contains("PUBLIC KEY"));
    let users = store.raw_bucket(store::buckets::USERS).await.unwrap();
    let user: User = users
        .get_json(&store::keys::user_key("root@example.com"))
        .await
        .unwrap()
        .unwrap();
    assert!(user.platform_admin);
    assert!(auth::verify_password("hunter2", &user.password_hash).unwrap());

    let age_before = std::fs::read_to_string(keys.join("age_private.key")).unwrap();
    init::run(init_args(server.url(), dir.path()))
        .await
        .unwrap();
    assert_eq!(
        age_before,
        std::fs::read_to_string(keys.join("age_private.key")).unwrap()
    );
    let same: User = users
        .get_json(&store::keys::user_key("root@example.com"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.password_hash, same.password_hash);
}

#[tokio::test]
async fn admin_project_and_user_commands() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let repos_root = dir.path().join("repos");

    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let admin_args = |cmd| AdminArgs {
        nats_url: server.url().to_string(),
        keys_dir: dir.path().join("no-keys"),
        cmd,
    };
    admin::run(admin_args(AdminCmd::Project(ProjectCmd::Create {
        owner: "acme".into(),
        name: "api".into(),
        default_branch: "trunk".into(),
        repos_root: repos_root.clone(),
        hook_bin: None,
    })))
    .await
    .unwrap();

    let counters = store.raw_bucket(store::buckets::COUNTERS).await.unwrap();
    assert_eq!(counters.get_json::<u64>("acme.api").await.unwrap(), Some(0));
    let repos = vcs::RepoManager::new(&repos_root);
    assert_eq!(repos.default_branch("acme", "api").await.unwrap(), "trunk");

    for (owner, name) in [("acme", "api"), ("global", "x"), ("has.dot", "x")] {
        assert!(
            admin::run(admin_args(AdminCmd::Project(ProjectCmd::Create {
                owner: owner.into(),
                name: name.into(),
                default_branch: "main".into(),
                repos_root: repos_root.clone(),
                hook_bin: None,
            })))
            .await
            .is_err(),
            "expected {owner}/{name} to be rejected"
        );
    }

    admin::run(admin_args(AdminCmd::User(UserCmd::Create {
        email: "dev@example.com".into(),
        password: "pw".into(),
        admin: false,
    })))
    .await
    .unwrap();
    assert!(
        admin::run(admin_args(AdminCmd::User(UserCmd::Create {
            email: "dev@example.com".into(),
            password: "pw".into(),
            admin: false,
        })))
        .await
        .is_err(),
        "duplicate user create should fail"
    );
    admin::run(admin_args(AdminCmd::User(UserCmd::Delete {
        email: "dev@example.com".into(),
    })))
    .await
    .unwrap();
    let users = store.raw_bucket(store::buckets::USERS).await.unwrap();
    assert_eq!(
        users
            .get_json::<User>(&store::keys::user_key("dev@example.com"))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn admin_user_role_commands() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let admin_args = |cmd| AdminArgs {
        nats_url: server.url().to_string(),
        keys_dir: dir.path().join("no-keys"),
        cmd,
    };
    admin::run(admin_args(AdminCmd::User(UserCmd::Create {
        email: "dev@example.com".into(),
        password: "pw".into(),
        admin: false,
    })))
    .await
    .unwrap();

    let role_of = |slug: &str| {
        let store = store.clone();
        let slug = slug.to_string();
        async move {
            store
                .raw_bucket(store::buckets::USERS)
                .await
                .unwrap()
                .get_json::<User>(&store::keys::user_key("dev@example.com"))
                .await
                .unwrap()
                .unwrap()
                .project_roles
                .get(&slug)
                .copied()
        }
    };

    admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Set {
        email: "dev@example.com".into(),
        project: "acme/api".into(),
        role: "owner".into(),
    }))))
    .await
    .unwrap();
    assert_eq!(role_of("acme/api").await, Some(types::ProjectRole::Admin));

    admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Set {
        email: "dev@example.com".into(),
        project: "acme/web".into(),
        role: "viewer".into(),
    }))))
    .await
    .unwrap();
    admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::List {
        email: "dev@example.com".into(),
    }))))
    .await
    .unwrap();

    admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Remove {
        email: "dev@example.com".into(),
        project: "acme/api".into(),
    }))))
    .await
    .unwrap();
    assert_eq!(role_of("acme/api").await, None);
    assert_eq!(role_of("acme/web").await, Some(types::ProjectRole::Viewer));

    assert!(
        admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Set {
            email: "dev@example.com".into(),
            project: "acme/api".into(),
            role: "superuser".into(),
        }))))
        .await
        .is_err()
    );
    assert!(
        admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Set {
            email: "dev@example.com".into(),
            project: "no-slash".into(),
            role: "member".into(),
        }))))
        .await
        .is_err()
    );
    assert!(
        admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::List {
            email: "ghost@example.com".into(),
        }))))
        .await
        .is_err()
    );
    assert!(
        admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Remove {
            email: "dev@example.com".into(),
            project: "acme/api".into(),
        }))))
        .await
        .is_err()
    );
}

/// A `cloud-identities.*` record round-trips through the admin CLI (design
/// #313 A5): set writes the operator's coordinates, delete removes them, and
/// the record never touches the secrets bucket.
#[tokio::test]
async fn admin_cloud_identity_round_trip() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let admin_args = |cmd| AdminArgs {
        nats_url: server.url().to_string(),
        keys_dir: dir.path().join("no-keys"),
        cmd,
    };
    let scoped = || ScopedName {
        project: "acme/api".into(),
        name: "gcp-artifact-writer".into(),
    };
    admin::run(admin_args(AdminCmd::CloudIdentity(CloudIdentityCmd::Set {
        scoped: scoped(),
        audience: "//iam.googleapis.com/projects/1/providers/chuggernaut".into(),
        service_account: "deployer@acme.iam.gserviceaccount.com".into(),
        token_ttl_secs: Some(900),
    })))
    .await
    .unwrap();

    let bucket = store
        .raw_bucket(store::buckets::CLOUD_IDENTITIES)
        .await
        .unwrap();
    let key = "acme.api.gcp-artifact-writer";
    assert_eq!(
        bucket
            .get_json::<types::CloudIdentity>(key)
            .await
            .unwrap()
            .unwrap(),
        types::CloudIdentity {
            audience: "//iam.googleapis.com/projects/1/providers/chuggernaut".into(),
            service_account: "deployer@acme.iam.gserviceaccount.com".into(),
            token_ttl_secs: Some(900),
        }
    );
    assert_eq!(
        store
            .raw_bucket(store::buckets::SECRETS)
            .await
            .unwrap()
            .keys_with_prefix("acme.api.")
            .await
            .unwrap(),
        Vec::<String>::new(),
        "a cloud identity must never be written to the secrets bucket"
    );

    admin::run(admin_args(AdminCmd::CloudIdentity(
        CloudIdentityCmd::List {
            project: "acme/api".into(),
        },
    )))
    .await
    .unwrap();
    admin::run(admin_args(AdminCmd::CloudIdentity(
        CloudIdentityCmd::Delete { scoped: scoped() },
    )))
    .await
    .unwrap();
    assert_eq!(
        bucket.get_json::<types::CloudIdentity>(key).await.unwrap(),
        None
    );
}

/// A name that is not a KV key segment, and a record missing the coordinates
/// it exists to carry, are refused at write rather than stored to fail at
/// release.
#[tokio::test]
async fn admin_cloud_identity_rejects_malformed_records() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    NatsStore::connect(server.url())
        .await
        .unwrap()
        .ensure_topology()
        .await
        .unwrap();

    for (project, name, audience, service_account) in [
        ("acme/api", "bad.name", "aud", "sa@acme.example"),
        ("acme/api", "ok-name", "  ", "sa@acme.example"),
        ("acme/api", "ok-name", "aud", ""),
        ("no-slash", "ok-name", "aud", "sa@acme.example"),
    ] {
        assert!(
            admin::run(AdminArgs {
                nats_url: server.url().to_string(),
                keys_dir: dir.path().join("no-keys"),
                cmd: AdminCmd::CloudIdentity(CloudIdentityCmd::Set {
                    scoped: ScopedName {
                        project: project.into(),
                        name: name.into(),
                    },
                    audience: audience.into(),
                    service_account: service_account.into(),
                    token_ttl_secs: None,
                }),
            })
            .await
            .is_err(),
            "expected {project}.{name} ({audience:?}, {service_account:?}) to be rejected"
        );
    }
}
