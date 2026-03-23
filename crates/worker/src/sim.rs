use std::path::PathBuf;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::info;

use forge2_types::*;

use crate::config::WorkerConfig;
use crate::git;
use crate::lifecycle::WorkerExecutor;

/// SimWorker: creates a branch, commits a placeholder file, opens a PR, sleeps, yields.
pub struct SimWorker {
    pub config: WorkerConfig,
    pub delay_secs: u64,
    pub workdir: PathBuf,
}

impl SimWorker {
    pub fn new(config: WorkerConfig, delay_secs: u64) -> Self {
        let workdir = std::env::temp_dir().join(format!("forge2-sim-{}", config.worker_id));
        std::fs::create_dir_all(&workdir).ok();
        Self {
            config,
            delay_secs,
            workdir,
        }
    }
}

#[async_trait::async_trait]
impl WorkerExecutor for SimWorker {
    async fn execute(
        &self,
        assignment: &Assignment,
        cancel: CancellationToken,
    ) -> OutcomeType {
        let job = &assignment.job;
        let branch = format!("work/{}", job.key);

        // Parse owner/repo
        let parts: Vec<&str> = job.repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            return OutcomeType::Fail {
                reason: format!("invalid repo format: {}", job.repo),
                logs: None,
            };
        }
        let (owner, repo) = (parts[0], parts[1]);

        // Clone
        info!(repo = job.repo, "cloning repo");
        let repo_dir = match git::clone_repo(
            &self.config.forgejo_url,
            &job.repo,
            &self.config.forgejo_token,
            &self.workdir,
        ) {
            Ok(d) => d,
            Err(e) => {
                return OutcomeType::Fail {
                    reason: format!("clone failed: {e}"),
                    logs: None,
                };
            }
        };

        // Checkout branch
        if let Err(e) = git::checkout_branch(&repo_dir, &branch) {
            return OutcomeType::Fail {
                reason: format!("branch checkout failed: {e}"),
                logs: None,
            };
        }

        // Write a placeholder file
        let file_path = repo_dir.join(format!("sim-{}.txt", job.key));
        let content = format!(
            "Sim worker output for job: {}\nTitle: {}\nBody: {}\n",
            job.key, job.title, job.body
        );
        if let Err(e) = std::fs::write(&file_path, &content) {
            return OutcomeType::Fail {
                reason: format!("write failed: {e}"),
                logs: None,
            };
        }

        // Commit
        let commit_msg = format!("sim: {}", job.title);
        if let Err(e) = git::commit_all(&repo_dir, &commit_msg) {
            return OutcomeType::Fail {
                reason: format!("commit failed: {e}"),
                logs: None,
            };
        }

        // Push
        info!(branch, "pushing");
        if let Err(e) = git::push(&repo_dir, &branch) {
            return OutcomeType::Fail {
                reason: format!("push failed: {e}"),
                logs: None,
            };
        }

        // Check for existing PR or create one
        let forgejo = forge2_forgejo_api::ForgejoClient::new(
            &self.config.forgejo_url,
            &self.config.forgejo_token,
        );

        let pr_url = match forgejo.find_pr_by_head(owner, repo, &branch).await {
            Ok(Some(pr)) => {
                info!(pr_number = pr.number, "existing PR found");
                pr.html_url
            }
            _ => {
                // Get default branch
                let base = git::default_branch(&repo_dir).unwrap_or_else(|_| "main".to_string());

                let pr_title = format!("[{}] {}", job.key, job.title);
                let pr_body = format!("{}\n\nJob: {}", job.body, job.key);

                match forgejo
                    .create_pull_request(
                        owner,
                        repo,
                        &forge2_forgejo_api::CreatePullRequestOption {
                            title: pr_title,
                            body: Some(pr_body),
                            head: branch.clone(),
                            base,
                        },
                    )
                    .await
                {
                    Ok(pr) => {
                        info!(pr_number = pr.number, "PR created");
                        pr.html_url
                    }
                    Err(e) => {
                        return OutcomeType::Fail {
                            reason: format!("PR creation failed: {e}"),
                            logs: None,
                        };
                    }
                }
            }
        };

        // Sleep (simulating work)
        info!(delay = self.delay_secs, "simulating work delay");
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(self.delay_secs)) => {}
            _ = cancel.cancelled() => {
                info!("cancelled during delay");
                return OutcomeType::Abandon {};
            }
        }

        info!(job_key = job.key, pr_url, "yielding");
        OutcomeType::Yield { pr_url }
    }
}
