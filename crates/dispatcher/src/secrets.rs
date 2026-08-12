//! Declared project secrets at container launch: reading their values, and the
//! file delivery form (spec §8.2; design #529 S3).
//!
//! - **Accepts:** one level's own `secrets:` and `secret_files:` declarations —
//!   `work`, one evaluator, or `wrap_up.run` — and never another level's.
//! - **Emits:** the decrypted values `container_env` inserts, and a
//!   [`SecretFileDelivery`] — one `0600` injected file per file-delivered name
//!   plus the [`types::secret_file_env_name`] variable pointing at it.
//! - **Guarantees:** a file-delivered value enters the launch environment under
//!   no name, asserted at the merge site every such launch passes through; every
//!   path lies under [`SECRET_FILE_DIR`], inside the injected tree both backends
//!   reclaim at teardown (§3.1); a reserved-prefix name is read for neither form.
//! - **Container launches only:** [`SECRET_FILE_DIR`] is a wire path and a host
//!   launch refuses one outside the variables §4.1 fixes, so
//!   `JobType::validate_secret_files` refuses the declaration on a host level
//!   rather than letting the launch fail on the path (design #322 §2).
//! - **Spec:** §8.2, §4.1, §3.1.
//!
//! `0600` is not a boundary: the task runs as the file's owner and reads it at
//! will (design #529 D3). What it buys is that the value is absent from the
//! task's own `environ`, which its children inherit and anything sharing the uid
//! can read for the process's whole life (M3).

use crate::core::{Core, Result};
use crate::exec::reserved_env_prefix;
use std::collections::HashMap;

/// Where a file-delivered secret lands in the container — under the same
/// injected tree as the SSH credential, which `HostBackend::remove` reclaims by
/// recorded path and a container teardown destroys with the container (§3.1).
pub(crate) const SECRET_FILE_DIR: &str = "/chuggernaut/secrets";

/// The mode a delivered secret file carries (design #529 S3).
pub(crate) const SECRET_FILE_MODE: u32 = 0o600;

/// The path a file-delivered secret's value is written to.
pub(crate) fn secret_file_path(name: &str) -> String {
    format!("{SECRET_FILE_DIR}/{name}")
}

/// What one container launch receives for the secrets it declared as files.
#[derive(Debug, Default)]
pub(crate) struct SecretFileDelivery {
    files: Vec<container::InjectedFile>,
    env: HashMap<String, String>,
}

impl SecretFileDelivery {
    /// Merge into one launch's file set and env — every file-delivered launch
    /// passes through here, so the property the delivery exists for is asserted
    /// against the launch's real env rather than trusted: the value reaches the
    /// container as a file and under no environment name.
    pub(crate) fn merge_into(
        self,
        env: &mut HashMap<String, String>,
        files: &mut Vec<container::InjectedFile>,
    ) {
        for file in &self.files {
            assert_eq!(
                file.mode, SECRET_FILE_MODE,
                "a delivered secret file is {SECRET_FILE_MODE:o}: {}",
                file.container_path
            );
            let name = file
                .container_path
                .rsplit('/')
                .next()
                .unwrap_or(&file.container_path);
            assert!(
                !env.contains_key(name),
                "a file-delivered secret reaches the launch env under its own name: {name}"
            );
        }
        files.extend(self.files);
        env.extend(self.env);
    }

    /// [`Self::merge_into`] for a command container's assembled launch config.
    pub(crate) fn apply(self, config: &mut container::ContainerLaunchConfig) {
        let (env, files) = (&mut config.env, &mut config.files);
        self.merge_into(env, files);
    }
}

impl Core {
    /// The decrypted value of every declared name the project holds, in
    /// declaration order (§8.2): the one read of the `secrets.*` bucket, so the
    /// two delivery forms cannot diverge on which store branch they take or on
    /// which names a reserved prefix seals off (§4.1).
    pub(crate) async fn declared_secret_values(
        &self,
        owner: &str,
        project: &str,
        declared: &[String],
    ) -> Result<Vec<(String, String)>> {
        let injectable = declared.iter().filter(|n| reserved_env_prefix(n).is_none());
        let mut values = Vec::new();
        match &self.secrets {
            Some(secrets) => {
                use store::secrets::SecretStore;
                for name in injectable {
                    if let Some(value) = secrets.get(owner, project, name).await? {
                        values.push((name.clone(), value));
                    }
                }
            }
            None => {
                let secrets = self.store.raw_bucket(store::buckets::SECRETS).await?;
                for name in injectable {
                    if let Some(value) = secrets
                        .get_json::<String>(&format!("{owner}.{project}.{name}"))
                        .await?
                    {
                        values.push((name.clone(), value));
                    }
                }
            }
        }
        Ok(values)
    }

    /// The file delivery for one level's `secret_files:` (design #529 S3): a
    /// `0600` file per declared name and the `{NAME}_FILE` variable naming its
    /// path. A level declaring none receives no file and no variable.
    pub(crate) async fn secret_file_delivery(
        &self,
        owner: &str,
        project: &str,
        declared: &[String],
    ) -> Result<SecretFileDelivery> {
        let mut delivery = SecretFileDelivery::default();
        if declared.is_empty() {
            return Ok(delivery);
        }
        for (name, value) in self
            .declared_secret_values(owner, project, declared)
            .await?
        {
            let path = secret_file_path(&name);
            delivery.files.push(container::InjectedFile {
                container_path: path.clone(),
                contents: value.into_bytes(),
                mode: SECRET_FILE_MODE,
                artifact: None,
            });
            delivery
                .env
                .insert(types::secret_file_env_name(&name), path);
        }
        Ok(delivery)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn delivered(names: &[&str]) -> SecretFileDelivery {
        let mut delivery = SecretFileDelivery::default();
        for name in names {
            let path = secret_file_path(name);
            delivery.files.push(container::InjectedFile {
                container_path: path.clone(),
                contents: format!("value-of-{name}").into_bytes(),
                mode: SECRET_FILE_MODE,
                artifact: None,
            });
            delivery.env.insert(types::secret_file_env_name(name), path);
        }
        delivery
    }

    /// The shape a task reads (design #529 S3, correction 5): the value at
    /// `0600` under the injected tree, the path in `{NAME}_FILE` — the spelling
    /// `.chug/tasks/deploy.sh` already prefers — and the value under no
    /// environment name at all.
    #[test]
    fn a_file_delivered_secret_is_a_path_in_the_env_and_a_value_on_disk() {
        let (mut env, mut files) = (HashMap::new(), Vec::new());
        delivered(&["MINI_DEPLOY_KEY"]).merge_into(&mut env, &mut files);

        assert_eq!(
            env.get("MINI_DEPLOY_KEY_FILE").map(String::as_str),
            Some("/chuggernaut/secrets/MINI_DEPLOY_KEY")
        );
        assert_eq!(env.get("MINI_DEPLOY_KEY"), None);
        assert!(
            !env.values().any(|v| v.contains("value-of-")),
            "the value reaches the environment under no name: {env:?}"
        );
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].container_path,
            "/chuggernaut/secrets/MINI_DEPLOY_KEY"
        );
        assert_eq!(files[0].mode, 0o600);
        assert_eq!(files[0].contents, b"value-of-MINI_DEPLOY_KEY");
        assert!(
            files[0].container_path.starts_with(SECRET_FILE_DIR),
            "delivery lands in the tree teardown reclaims"
        );
    }

    /// Negative space: a level declaring nothing is delivered nothing, so a job
    /// type that never adopts the form launches exactly as it did.
    #[test]
    fn a_level_declaring_no_file_delivery_receives_none() {
        let (mut env, mut files) = (HashMap::new(), Vec::new());
        delivered(&[]).merge_into(&mut env, &mut files);

        assert!(env.is_empty() && files.is_empty());
    }

    /// The assert the merge site carries: the same name delivered both ways is
    /// the one shape release validation refuses, and it must not pass silently
    /// here either.
    #[test]
    #[should_panic(expected = "under its own name")]
    fn a_value_also_env_delivered_trips_the_merge_assert() {
        let mut env = HashMap::from([("MINI_DEPLOY_KEY".to_string(), "inline-value".to_string())]);
        let mut files = Vec::new();
        delivered(&["MINI_DEPLOY_KEY"]).merge_into(&mut env, &mut files);
    }
}
