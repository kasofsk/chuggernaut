//! Tier-2: `chuggernaut init` + admin commands against real NATS (Docker,
//! skip-guarded). Keygen shells out to openssl/ssh-keygen — present on any
//! dev/deploy host.

use cli::admin::{self, AdminArgs, AdminCmd, ProjectCmd, RoleCmd, UserCmd};
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
async fn init_bootstraps_and_is_idempotent() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    init::run(init_args(server.url(), dir.path()))
        .await
        .unwrap();

    // Keypairs on disk, private files 0600.
    let keys = dir.path().join("keys");
    for name in [
        "jwt_private.pem",
        "jwt_public.pem",
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
    // The artifacts key is distinct from the secrets key — same generator, but
    // sharing them would hand the API the secrets identity (§10.2).
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

    // Topology + VAPID public + admin user in KV.
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

    // Re-run: keys and the existing user survive untouched.
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
        keys_dir: dir.path().join("no-keys"), // absent → plain connect
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

    // Counter initialized to 0, bare repo has the right default branch.
    let counters = store.raw_bucket(store::buckets::COUNTERS).await.unwrap();
    assert_eq!(counters.get_json::<u64>("acme.api").await.unwrap(), Some(0));
    let repos = vcs::RepoManager::new(&repos_root);
    assert_eq!(repos.default_branch("acme", "api").await.unwrap(), "trunk");

    // Duplicate create fails; reserved/invalid names rejected.
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

    // Re-read the stored user's role on a project (None if unset).
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

    // set: `owner` is the CLI alias for the top project role (Admin).
    admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Set {
        email: "dev@example.com".into(),
        project: "acme/api".into(),
        role: "owner".into(),
    }))))
    .await
    .unwrap();
    assert_eq!(role_of("acme/api").await, Some(types::ProjectRole::Admin));

    // A second set on another project coexists; list runs without error.
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

    // remove: clears just that project's grant.
    admin::run(admin_args(AdminCmd::User(UserCmd::Role(RoleCmd::Remove {
        email: "dev@example.com".into(),
        project: "acme/api".into(),
    }))))
    .await
    .unwrap();
    assert_eq!(role_of("acme/api").await, None);
    assert_eq!(role_of("acme/web").await, Some(types::ProjectRole::Viewer));

    // Errors: bad role, bad slug, unknown user, removing an absent grant.
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
