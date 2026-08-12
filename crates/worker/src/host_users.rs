//! Per-project unix users on a host node (design #537 D3/D6): the daemon
//! resolves each project its `WORKER_HOST_PROJECTS` names to a user of its own
//! and hands the answer to [`container::host::HostBackend`], which is told node
//! facts rather than discovering them.
//!
//! The name is **derived**, `chug-{project}` from the second component of the
//! `owner/project` slug (D6), so the roster the node already declares is the
//! roster of users that must exist and no second declaration is needed. The
//! binding is off until a node sets `WORKER_HOST_USERS`, and a project it
//! declares whose user this node cannot resolve is recorded as unresolvable —
//! a warning at boot (D5) and a refusal at that project's launch, never a
//! fall back to the daemon's own uid.

use container::host::{HostUsers, ProjectUser, TaskUser};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The prefix every project user carries (design #537 D6), so every account a
/// node provisions for the platform is greppable and none can collide with a
/// machine's own.
pub const USER_PREFIX: &str = "chug-";

/// How much of a `passwd` entry one lookup buffers. `getpwnam_r` fails with
/// `ERANGE` rather than truncating, so an entry larger than this is a named
/// error and never a home directory read short.
const PASSWD_BUFFER: usize = 16 * 1024;

/// The user a project's host tasks run as, `chug-{project}` from the second
/// component of the slug. `None` when the slug is not an `owner/project` pair,
/// which [`crate::config::WorkerConfig`] already refuses at parse.
pub fn derived_user(project: &str) -> Option<String> {
    let (owner, name) = project.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{USER_PREFIX}{name}"))
}

/// This node's `{project → user}` binding, resolved once at boot. An
/// undeclared node binds nobody and its launches run as the daemon exactly as
/// they did before design #537, so the map is empty rather than absent-shaped.
pub fn resolve(node: &str, declared: bool, projects: &[String]) -> HostUsers {
    if !declared {
        return HostUsers::default();
    }
    let mut bound = BTreeMap::new();
    for project in projects {
        let Some(user) = derived_user(project) else {
            continue;
        };
        bound.insert(project.clone(), resolved_one(node, project, user));
    }
    debug_assert!(
        bound.len() <= projects.len(),
        "a binding names only projects the node declared"
    );
    HostUsers::new(bound)
}

/// One project's user as this node has it, announced either way: a boot
/// refusal here would brick a daemon under `KeepAlive` the moment an operator
/// listed a project before creating its user, so the loud failure belongs to
/// the deploy and the launch (design #537 D5).
fn resolved_one(node: &str, project: &str, user: String) -> ProjectUser {
    match lookup(&user) {
        Ok(resolved) => {
            tracing::info!(
                node = %node,
                project = %project,
                user = %resolved.name,
                uid = resolved.uid,
                gid = resolved.gid,
                home = %resolved.home.display(),
                "host tasks for this project run as its own unix user (design #537 D1)"
            );
            ProjectUser::Bound(resolved)
        }
        Err(reason) => {
            tracing::warn!(
                node = %node,
                project = %project,
                user = %user,
                "WORKER_HOST_USERS binds this project to a unix user this node cannot resolve, so \
                 every host launch of it is REFUSED rather than run as the daemon's own uid \
                 (design #537 D5): {reason}"
            );
            ProjectUser::Unresolved { user, reason }
        }
    }
}

/// One user out of the node's own passwd database, in the daemon's own view —
/// which is the only view that decides whether a launch can enter it (design
/// #537 D3, `docs/reference/style.md` Tier 2 rule 7).
fn lookup(user: &str) -> Result<TaskUser, String> {
    let entry = passwd_entry(user)?;
    if entry.home.as_os_str().is_empty() || !entry.home.is_absolute() {
        return Err(format!(
            "unix user {user} exists but its home {:?} is not an absolute path, and a task's HOME \
             follows the user it runs as",
            entry.home
        ));
    }
    Ok(entry)
}

/// The `getpwnam_r` call itself. Reentrant rather than `getpwnam` because the
/// daemon is threaded and the non-reentrant call answers into static storage.
fn passwd_entry(user: &str) -> Result<TaskUser, String> {
    let name = std::ffi::CString::new(user).map_err(|e| format!("user name {user:?}: {e}"))?;
    let mut buffer = vec![0 as libc::c_char; PASSWD_BUFFER];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is to live local storage whose length is passed alongside it, and the call writes only into `entry`, `buffer` and `found`.
    let rc = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if rc != 0 {
        return Err(format!(
            "looking up unix user {user} on this node failed: {}",
            std::io::Error::from_raw_os_error(rc)
        ));
    }
    if found.is_null() {
        return Err(format!(
            "this node has no unix user {user} — it is the operator's to create, with root, per \
             design #537 D9"
        ));
    }
    Ok(TaskUser {
        name: user.to_string(),
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        // SAFETY: `getpwnam_r` answered with a match, so `pw_dir` points into `buffer` and is NUL-terminated.
        home: PathBuf::from(
            unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) }
                .to_string_lossy()
                .into_owned(),
        ),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// D6's derivation, and the collision it exists to make loud: two projects
    /// of different owners with one name derive one user, which is the failure
    /// design #537 exists to prevent rather than an inconvenience.
    #[test]
    fn a_project_user_is_derived_from_the_slug() {
        assert_eq!(derived_user("acme/beacon").unwrap(), "chug-beacon");
        assert_eq!(
            derived_user("a/beacon").unwrap(),
            derived_user("b/beacon").unwrap(),
            "the collision D6 makes a hard parse error is real, not hypothetical"
        );
        for bad in ["beacon", "acme/", "/beacon", ""] {
            assert!(derived_user(bad).is_none(), "must not derive from {bad:?}");
        }
    }

    /// A node that declares nothing binds nobody, which is what makes this
    /// land inert: every host launch on today's fleet runs exactly as it did.
    #[test]
    fn an_undeclared_node_binds_nobody() {
        let projects = vec!["acme/beacon".to_string()];
        assert!(resolve("w1", false, &projects).is_empty());
        assert!(resolve("w1", true, &[]).is_empty());
    }

    /// A declared project whose user is absent is recorded as unresolvable, so
    /// the launch is refused by name — the fall back to the daemon's uid is the
    /// silent failure D5 refuses.
    #[test]
    fn a_declared_project_with_no_user_is_unresolvable_rather_than_absent() {
        let project = "acme/chug-537-no-such-user".to_string();
        let users = resolve("w1", true, std::slice::from_ref(&project));
        assert!(!users.is_empty());
        let env = std::collections::HashMap::from([("JOB_PROJECT".to_string(), project.clone())]);
        assert!(
            users.binding(&env).is_none(),
            "an unresolvable user is never a binding"
        );
        let refusal = users.refusal("w1", &env).expect("the launch is refused");
        assert!(refusal.contains(&project), "{refusal}");
        assert!(refusal.contains("chug-chug-537-no-such-user"), "{refusal}");
    }

    /// The lookup answers out of the node's own passwd database, so the user
    /// this test process is running as resolves with a home of its own.
    #[test]
    fn the_running_user_resolves_with_its_own_home() {
        let Ok(name) = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) else {
            return;
        };
        let Ok(user) = lookup(&name) else {
            return;
        };
        assert_eq!(user.name, name);
        assert!(user.home.is_absolute(), "{:?}", user.home);
        // SAFETY: `getuid` takes no arguments, cannot fail and returns a plain integer.
        assert_eq!(user.uid, unsafe { libc::getuid() });
    }
}
