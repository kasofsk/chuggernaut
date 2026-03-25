use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use chuggernaut_forgejo_api::{CreatePullReviewOptions, ForgejoClient, MergePullRequestOption};
use chuggernaut_nats::NatsClient;
use chuggernaut_types::*;

// ---------------------------------------------------------------------------
// CLI args (passed by the Forgejo Action workflow as inputs)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, ValueEnum)]
enum PostAction {
    Yield,
    Review,
}

#[derive(Parser)]
#[command(
    name = "chuggernaut-worker",
    about = "Worker binary that runs inside Forgejo Actions"
)]
struct Args {
    /// What to do after the subprocess exits
    #[arg(long, env = "CHUGGERNAUT_POST_ACTION", default_value = "yield")]
    post_action: PostAction,

    /// Job key (e.g. acme.payments.57)
    #[arg(long, env = "CHUGGERNAUT_JOB_KEY")]
    job_key: String,

    /// NATS server URL
    #[arg(
        long,
        env = "CHUGGERNAUT_NATS_URL",
        default_value = "nats://localhost:4222"
    )]
    nats_url: String,

    /// Forgejo base URL
    #[arg(long, env = "CHUGGERNAUT_FORGEJO_URL")]
    forgejo_url: String,

    /// Forgejo API token
    #[arg(long, env = "CHUGGERNAUT_FORGEJO_TOKEN")]
    forgejo_token: String,

    /// Command to run (e.g. "claude" or a mock script for testing)
    #[arg(long, env = "CHUGGERNAUT_COMMAND", default_value = "claude")]
    command: String,

    /// Extra arguments to pass to the command
    #[arg(long, env = "CHUGGERNAUT_COMMAND_ARGS", default_value = "")]
    command_args: String,

    /// Heartbeat interval in seconds
    #[arg(
        long,
        env = "CHUGGERNAUT_HEARTBEAT_INTERVAL_SECS",
        default_value = "10"
    )]
    heartbeat_interval_secs: u64,

    /// Total timeout for this action run in minutes
    #[arg(long, env = "CHUGGERNAUT_TIMEOUT_MINUTES", default_value = "60")]
    timeout_minutes: u64,

    /// How many minutes before the deadline to send a wrap-up warning
    #[arg(long, env = "CHUGGERNAUT_WRAP_UP_MINUTES", default_value = "5")]
    wrap_up_minutes: u64,

    /// Working directory for git operations
    #[arg(
        long,
        env = "CHUGGERNAUT_WORKDIR",
        default_value = "/tmp/chuggernaut-work"
    )]
    workdir: String,

    /// Review feedback from a previous run (yield post-action, rework cycles)
    #[arg(long, env = "CHUGGERNAUT_REVIEW_FEEDBACK", default_value = "")]
    review_feedback: String,

    /// Whether this is a rework of a previously-reviewed run
    #[arg(long, env = "CHUGGERNAUT_IS_REWORK", default_value = "false")]
    is_rework: bool,

    /// PR URL (review post-action)
    #[arg(long, env = "CHUGGERNAUT_PR_URL", default_value = "")]
    pr_url: String,

    /// Review level: low, medium, high (review post-action)
    #[arg(long, env = "CHUGGERNAUT_REVIEW_LEVEL", default_value = "high")]
    review_level: String,

    /// The prompt to pass to Claude (fully resolved, no placeholders)
    #[arg(long, env = "CHUGGERNAUT_PROMPT")]
    prompt: String,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,chuggernaut_worker=debug".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    info!(
        job_key = args.job_key,
        command = args.command,
        post_action = ?args.post_action,
        "starting chuggernaut-worker"
    );

    run(args).await
}

// ---------------------------------------------------------------------------
// Unified run
// ---------------------------------------------------------------------------

async fn run(args: Args) -> anyhow::Result<()> {
    let (owner, repo, _seq) = parse_job_key(&args.job_key)
        .ok_or_else(|| anyhow::anyhow!("invalid job key: {}", args.job_key))?;
    let repo_full = format!("{owner}/{repo}");

    let forgejo = ForgejoClient::new(&args.forgejo_url, &args.forgejo_token);
    validate_forgejo_token(&forgejo, &owner, &repo).await?;

    let (nats, _js) = connect_nats(&args.nats_url).await?;
    let cancel = CancellationToken::new();

    // Clone repo
    let workdir = PathBuf::from(&args.workdir);
    std::fs::create_dir_all(&workdir)?;
    let repo_dir = chuggernaut_worker::git::clone_repo(
        &args.forgejo_url,
        &repo_full,
        &args.forgejo_token,
        &workdir,
    )?;

    // Branch checkout — differs by post-action
    let branch = match args.post_action {
        PostAction::Yield => {
            let b = format!("work/{}", args.job_key);
            chuggernaut_worker::git::checkout_branch(&repo_dir, &b)?;
            info!(branch = b.as_str(), "checked out work branch");
            // Validate push auth early
            chuggernaut_worker::git::push(&repo_dir, &b).map_err(|e| {
                anyhow::anyhow!(
                    "git push auth check failed: {e}\n  \
                    The CHUGGERNAUT_FORGEJO_TOKEN may be invalid for git HTTP auth"
                )
            })?;
            info!("git push auth validated");
            b
        }
        PostAction::Review => {
            let pr_index = parse_pr_index(&args.pr_url)
                .ok_or_else(|| anyhow::anyhow!("cannot parse PR index from: {}", args.pr_url))?;
            let pr = forgejo.get_pull_request(&owner, &repo, pr_index).await?;
            let b = pr.head.ref_field.clone();
            chuggernaut_worker::git::checkout_branch(&repo_dir, &b)?;
            info!(
                branch = b.as_str(),
                pr_index, "checked out PR branch for review"
            );
            b
        }
    };

    // Common infrastructure
    spawn_heartbeat(
        nats.clone(),
        args.job_key.clone(),
        cancel.clone(),
        args.heartbeat_interval_secs,
    );
    spawn_deadline_warning(
        nats.clone(),
        args.job_key.clone(),
        cancel.clone(),
        args.timeout_minutes,
        args.wrap_up_minutes,
    );

    let hard_deadline =
        Duration::from_secs(args.timeout_minutes * 60).saturating_sub(Duration::from_secs(60));

    let mcp_config = write_mcp_config(&args.job_key, &args.nats_url, &workdir)?;
    info!(
        config = mcp_config.display().to_string(),
        "wrote MCP config"
    );

    // Build subprocess command
    info!(command = args.command, "launching subprocess");
    let mut cmd_args: Vec<String> = args
        .command_args
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    cmd_args.push("--print".into());
    cmd_args.push(args.prompt.clone());
    cmd_args.push("--mcp-config".into());
    cmd_args.push(mcp_config.display().to_string());

    let mut child = Command::new(&args.command)
        .args(&cmd_args)
        .current_dir(&repo_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    // Pipe stdout through jq for formatted action log output on stderr,
    // while also collecting the raw output for review parsing.
    let stdout = child.stdout.take().expect("stdout piped");
    let (output_tx, output_rx) = tokio::sync::oneshot::channel::<String>();

    // Spawn jq as a formatter: reads Claude's stream-json, writes human-readable to stderr
    let mut jq_child = Command::new("jq")
        .args(["-rj", "--unbuffered", PEEK_JQ_FILTER])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(std::io::stderr()))
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();

    tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut collected = String::new();
        let mut jq_stdin = jq_child.as_mut().and_then(|c| c.stdin.take());
        while let Ok(Some(line)) = lines.next_line().await {
            // Feed to jq formatter
            if let Some(ref mut stdin) = jq_stdin {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(line.as_bytes()).await;
                let _ = stdin.write_all(b"\n").await;
            }
            collected.push_str(&line);
            collected.push('\n');
        }
        // Close jq stdin so it exits
        drop(jq_stdin);
        if let Some(ref mut c) = jq_child {
            let _ = c.wait().await;
        }
        let _ = output_tx.send(collected);
    });

    // Wait for subprocess with cancel + deadline
    let (exit_ok, was_deadline) = tokio::select! {
        status = child.wait() => {
            match status {
                Ok(s) if s.success() => {
                    info!("subprocess exited successfully");
                    (true, false)
                }
                Ok(s) => {
                    warn!("subprocess exited with {s}");
                    (false, false)
                }
                Err(e) => {
                    error!("subprocess error: {e}");
                    (false, false)
                }
            }
        }
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            warn!("cancelled by heartbeat failure");
            (false, false)
        }
        _ = tokio::time::sleep(hard_deadline) => {
            warn!(job_key = args.job_key, "hard deadline reached, killing subprocess");
            let _ = child.kill().await;
            (false, true)
        }
    };

    cancel.cancel();
    let output = output_rx.await.unwrap_or_default();

    // Post-action — handle outcome based on mode
    match args.post_action {
        PostAction::Yield => {
            let outcome = if exit_ok || was_deadline {
                post_action_yield(&forgejo, &owner, &repo, &branch, &args.job_key).await
            } else {
                OutcomeType::Fail {
                    reason: "subprocess failed".to_string(),
                    logs: None,
                }
            };
            let worker_outcome = WorkerOutcome {
                worker_id: format!("action-{}", args.job_key),
                job_key: args.job_key.clone(),
                outcome,
                token_usage: None,
            };
            nats.publish_msg(&subjects::WORKER_OUTCOME, &worker_outcome)
                .await?;
            info!(job_key = args.job_key, "outcome reported, exiting");
        }
        PostAction::Review => {
            let pr_index = parse_pr_index(&args.pr_url).unwrap_or(0);
            let decision = if exit_ok {
                match parse_review_output(&output) {
                    Ok(result) => {
                        post_action_review(result, &forgejo, &owner, &repo, pr_index, &args).await
                    }
                    Err(e) => {
                        error!(job_key = args.job_key, error = %e, "failed to parse review output");
                        escalate_decision(&args)
                    }
                }
            } else {
                error!(job_key = args.job_key, "review subprocess failed");
                escalate_decision(&args)
            };
            nats.publish_msg(&subjects::REVIEW_DECISION, &decision)
                .await?;
            info!(job_key = args.job_key, "review decision reported, exiting");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Post-action: yield (commit, push, find/create PR)
// ---------------------------------------------------------------------------

async fn post_action_yield(
    forgejo: &ForgejoClient,
    owner: &str,
    repo: &str,
    branch: &str,
    job_key: &str,
) -> OutcomeType {
    let repo_dir = PathBuf::from(format!(
        "/tmp/chuggernaut-work/{}",
        format!("{owner}/{repo}").replace('/', "_")
    ));

    if let Err(e) = chuggernaut_worker::git::commit_all(&repo_dir, &format!("work: {job_key}")) {
        debug!("nothing to commit or commit failed: {e}");
    }
    if let Err(e) = chuggernaut_worker::git::push(&repo_dir, branch) {
        warn!("push failed: {e}");
        return OutcomeType::Fail {
            reason: format!("git push failed: {e}"),
            logs: None,
        };
    }

    match forgejo.find_pr_by_head(owner, repo, branch).await {
        Ok(Some(pr)) => OutcomeType::Yield {
            pr_url: pr.html_url,
        },
        Ok(None) => {
            match forgejo
                .create_pull_request(
                    owner,
                    repo,
                    &chuggernaut_forgejo_api::CreatePullRequestOption {
                        title: format!("[{job_key}] Work"),
                        body: Some(format!("Automated work for job {job_key}")),
                        head: branch.to_string(),
                        base: "main".to_string(),
                    },
                )
                .await
            {
                Ok(pr) => OutcomeType::Yield {
                    pr_url: pr.html_url,
                },
                Err(e) => OutcomeType::Fail {
                    reason: format!("failed to create PR: {e}"),
                    logs: None,
                },
            }
        }
        Err(e) => OutcomeType::Fail {
            reason: format!("failed to find PR: {e}"),
            logs: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Post-action: review (parse output, post review, merge)
// ---------------------------------------------------------------------------

async fn post_action_review(
    result: ReviewResult,
    forgejo: &ForgejoClient,
    owner: &str,
    repo: &str,
    pr_index: u64,
    args: &Args,
) -> ReviewDecision {
    match result {
        ReviewResult::Approved { feedback } => {
            match forgejo
                .submit_review(
                    owner,
                    repo,
                    pr_index,
                    &CreatePullReviewOptions {
                        body: feedback.clone().unwrap_or_else(|| "LGTM".to_string()),
                        event: "APPROVED".to_string(),
                    },
                )
                .await
            {
                Ok(review) => info!(
                    job_key = args.job_key,
                    review_id = review.id,
                    "approval review submitted"
                ),
                Err(e) => {
                    error!(job_key = args.job_key, error = %e, "failed to submit approval review")
                }
            }

            match try_merge(forgejo, owner, repo, pr_index).await {
                Ok(()) => {
                    info!(job_key = args.job_key, "PR merged successfully");
                    ReviewDecision {
                        job_key: args.job_key.clone(),
                        decision: DecisionType::Approved,
                        pr_url: Some(args.pr_url.clone()),
                        token_usage: None,
                    }
                }
                Err(e) => {
                    warn!(job_key = args.job_key, error = %e, "merge failed, requesting changes");
                    ReviewDecision {
                        job_key: args.job_key.clone(),
                        decision: DecisionType::ChangesRequested {
                            feedback: format!("Approved but merge failed: {e}"),
                        },
                        pr_url: Some(args.pr_url.clone()),
                        token_usage: None,
                    }
                }
            }
        }
        ReviewResult::ChangesRequested { feedback } => {
            if let Err(e) = forgejo
                .submit_review(
                    owner,
                    repo,
                    pr_index,
                    &CreatePullReviewOptions {
                        body: feedback.clone(),
                        event: "REQUEST_CHANGES".to_string(),
                    },
                )
                .await
            {
                error!(job_key = args.job_key, error = %e, "failed to submit changes-requested review");
            }

            ReviewDecision {
                job_key: args.job_key.clone(),
                decision: DecisionType::ChangesRequested { feedback },
                pr_url: Some(args.pr_url.clone()),
                token_usage: None,
            }
        }
        ReviewResult::Escalate => escalate_decision(args),
    }
}

fn escalate_decision(args: &Args) -> ReviewDecision {
    ReviewDecision {
        job_key: args.job_key.clone(),
        decision: DecisionType::Escalated {
            reviewer_login: "human".to_string(),
        },
        pr_url: Some(args.pr_url.clone()),
        token_usage: None,
    }
}

// ---------------------------------------------------------------------------
// Helpers: heartbeat
// ---------------------------------------------------------------------------

fn spawn_heartbeat(
    nats: NatsClient,
    job_key: String,
    cancel: CancellationToken,
    interval_secs: u64,
) {
    let hb_interval = Duration::from_secs(interval_secs);
    tokio::spawn(async move {
        let worker_id = format!("action-{}", job_key);
        let mut interval = tokio::time::interval(hb_interval);
        let mut consecutive_failures = 0u32;
        loop {
            interval.tick().await;
            if cancel.is_cancelled() {
                break;
            }
            let hb = WorkerHeartbeat {
                worker_id: worker_id.clone(),
                job_key: job_key.clone(),
            };
            match nats.publish_msg(&subjects::WORKER_HEARTBEAT, &hb).await {
                Ok(_) => {
                    consecutive_failures = 0;
                }
                Err(e) => {
                    consecutive_failures += 1;
                    warn!(consecutive_failures, "heartbeat failed: {e}");
                    if consecutive_failures >= 3 {
                        error!("3 consecutive heartbeat failures, cancelling");
                        cancel.cancel();
                        break;
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers: deadline warning
// ---------------------------------------------------------------------------

fn spawn_deadline_warning(
    nats: NatsClient,
    job_key: String,
    cancel: CancellationToken,
    timeout_minutes: u64,
    wrap_up_minutes: u64,
) {
    if wrap_up_minutes >= timeout_minutes {
        warn!(
            timeout_minutes,
            wrap_up_minutes, "wrap-up window >= timeout, skipping deadline warning"
        );
        return;
    }

    let warn_after = Duration::from_secs((timeout_minutes - wrap_up_minutes) * 60);
    let remaining = wrap_up_minutes;

    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::time::sleep(warn_after) => {}
            _ = cancel.cancelled() => { return; }
        }

        info!(
            job_key,
            remaining_minutes = remaining,
            "sending deadline wrap-up warning"
        );

        let msg = ChannelMessage {
            sender: "system".to_string(),
            body: format!(
                "DEADLINE WARNING: You have approximately {remaining} minutes remaining before this action run is terminated. \
                 Please wrap up your current work now:\n\
                 1. Finish or revert any half-done changes\n\
                 2. Commit your progress with `git add -A && git commit`\n\
                 3. Push your branch with `git push`\n\
                 4. Use update_status to report what you accomplished and what remains\n\n\
                 The next action run will pick up where you left off."
            ),
            timestamp: chrono::Utc::now(),
            message_id: uuid::Uuid::new_v4().to_string(),
            in_reply_to: None,
        };

        if let Err(e) = nats
            .publish_to(&subjects::CHANNEL_INBOX, &job_key, &msg)
            .await
        {
            error!(job_key, error = %e, "failed to send deadline warning");
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers: MCP config, Forgejo validation, NATS
// ---------------------------------------------------------------------------

fn write_mcp_config(job_key: &str, nats_url: &str, workdir: &Path) -> anyhow::Result<PathBuf> {
    let config = serde_json::json!({
        "mcpServers": {
            "chuggernaut": {
                "command": "chuggernaut-channel",
                "args": [
                    "--job-key", job_key,
                    "--nats-url", nats_url,
                ],
            }
        }
    });

    let path = workdir.join("mcp-config.json");
    std::fs::write(&path, serde_json::to_string_pretty(&config)?)?;
    Ok(path)
}

async fn validate_forgejo_token(
    forgejo: &ForgejoClient,
    owner: &str,
    repo: &str,
) -> anyhow::Result<()> {
    let r = forgejo.get_repo(owner, repo).await.map_err(|e| {
        anyhow::anyhow!(
            "Forgejo token validation failed for {owner}/{repo}: {e}\n  \
            Check that CHUGGERNAUT_FORGEJO_TOKEN is set and the token has read:repository scope"
        )
    })?;

    if !r.permissions.pull {
        return Err(anyhow::anyhow!(
            "Forgejo token cannot read {owner}/{repo} — needs read:repository scope"
        ));
    }
    if !r.permissions.push {
        return Err(anyhow::anyhow!(
            "Forgejo token cannot push to {owner}/{repo} — needs write:repository scope"
        ));
    }

    forgejo
        .list_pull_requests(owner, repo, Some("open"))
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Forgejo token cannot list PRs on {owner}/{repo}: {e}\n  \
            Check that the token has read:issue and write:issue scopes"
            )
        })?;

    info!(
        repo = r.full_name,
        pull = r.permissions.pull,
        push = r.permissions.push,
        admin = r.permissions.admin,
        "Forgejo token validated (repo access + push + PR access)"
    );
    Ok(())
}

async fn connect_nats(
    nats_url: &str,
) -> anyhow::Result<(NatsClient, async_nats::jetstream::Context)> {
    let nats = NatsClient::new(async_nats::connect(nats_url).await?);
    let js = async_nats::jetstream::new(nats.raw().clone());
    info!(url = nats_url, "connected to NATS");

    js.create_key_value(async_nats::jetstream::kv::Config {
        bucket: buckets::CHANNELS.to_string(),
        history: 1,
        storage: async_nats::jetstream::stream::StorageType::File,
        ..Default::default()
    })
    .await?;

    Ok((nats, js))
}

// ---------------------------------------------------------------------------
// Helpers: review output parsing
// ---------------------------------------------------------------------------

enum ReviewResult {
    Approved { feedback: Option<String> },
    ChangesRequested { feedback: String },
    Escalate,
}

fn parse_review_output(output: &str) -> anyhow::Result<ReviewResult> {
    for line in output.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let decision = v.get("decision").and_then(|d| d.as_str()).unwrap_or("");
            let feedback = v
                .get("feedback")
                .and_then(|f| f.as_str())
                .map(|s| s.to_string());

            return match decision {
                "approved" | "approve" => Ok(ReviewResult::Approved { feedback }),
                "changes_requested" | "request_changes" => Ok(ReviewResult::ChangesRequested {
                    feedback: feedback.unwrap_or_else(|| "Changes requested".to_string()),
                }),
                "escalate" => Ok(ReviewResult::Escalate),
                _ => Err(anyhow::anyhow!("unknown review decision: {decision}")),
            };
        }
    }

    Err(anyhow::anyhow!(
        "no valid review JSON found in subprocess output"
    ))
}

// ---------------------------------------------------------------------------
// Helpers: jq peek filter, PR merge
// ---------------------------------------------------------------------------

/// jq filter that renders Claude's stream-json output as a human-readable log.
/// Adapted from forge's peek-stream.sh.
const PEEK_JQ_FILTER: &str = r#"
if .type == "assistant" then
  (.message.content // [])[] |
  if .type == "thinking" then
    "\u001b[2m\u001b[3m" + (.thinking | split("\n")[0:3] | join("\n   ")) + "\u001b[0m\n"
  elif .type == "text" then
    "\u001b[37m" + .text + "\u001b[0m\n"
  elif .type == "tool_use" then
    "\u001b[36m[" + .name + "]\u001b[0m"
    + if .name == "Bash" then " $ " + (.input.command // "" | split("\n")[0][:120])
      elif .name == "Read" then " " + (.input.file_path // "")
      elif .name == "Write" then " " + (.input.file_path // "") + " (" + ((.input.content // "") | length | tostring) + " chars)"
      elif .name == "Edit" then
        " " + (.input.file_path // "") + "\n"
        + "\u001b[31m- " + ((.input.old_string // "") | split("\n")[0:5] | join("\n- ") | .[:300]) + "\u001b[0m\n"
        + "\u001b[32m+ " + ((.input.new_string // "") | split("\n")[0:5] | join("\n+ ") | .[:300]) + "\u001b[0m"
      elif .name == "Grep" then " " + (.input.pattern // "")
      elif .name == "Glob" then " " + (.input.pattern // "")
      elif .name == "Agent" then " " + (.input.description // (.input.prompt // "" | .[:80]))
      else " " + (.input | tostring | .[:100])
      end + "\n"
  else empty
  end
elif .type == "tool_result" then
  "\u001b[2m   done\u001b[0m\n"
elif .type == "result" then
  "\n\u001b[32msession complete\u001b[0m\n"
elif .type == "error" then
  "\u001b[31m[error] " + (.error.message // "unknown") + "\u001b[0m\n"
elif .type == "system" then
  "\u001b[2m[system] " + (.subtype // "unknown") + "\u001b[0m\n"
else empty
end
"#;

fn parse_pr_index(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

async fn try_merge(
    forgejo: &ForgejoClient,
    owner: &str,
    repo: &str,
    pr_index: u64,
) -> anyhow::Result<()> {
    let opts = MergePullRequestOption {
        method: "merge".to_string(),
        merge_message_field: None,
    };

    let max_retries = 3;
    let mut last_err = None;

    for attempt in 0..=max_retries {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(5 * attempt as u64)).await;
        }

        match forgejo
            .merge_pull_request(owner, repo, pr_index, &opts)
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(attempt, error = %e, "merge attempt failed");
                last_err = Some(e);
            }
        }
    }

    Err(anyhow::anyhow!(
        "merge failed after {} attempts: {}",
        max_retries + 1,
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_index_valid() {
        assert_eq!(
            parse_pr_index("http://forgejo/acme/repo/pulls/42"),
            Some(42)
        );
        assert_eq!(
            parse_pr_index("http://forgejo:3000/org/project/pulls/1"),
            Some(1)
        );
    }

    #[test]
    fn parse_pr_index_invalid() {
        assert_eq!(parse_pr_index("http://forgejo/acme/repo/pulls/"), None);
        assert_eq!(parse_pr_index(""), None);
    }

    #[test]
    fn parse_review_output_approved() {
        let output = r#"Some thinking text...
{"decision": "approved", "feedback": "Looks good"}
"#;
        let result = parse_review_output(output).unwrap();
        assert!(
            matches!(result, ReviewResult::Approved { feedback } if feedback == Some("Looks good".to_string()))
        );
    }

    #[test]
    fn parse_review_output_changes_requested() {
        let output = r#"{"decision": "changes_requested", "feedback": "Fix the tests"}"#;
        let result = parse_review_output(output).unwrap();
        assert!(
            matches!(result, ReviewResult::ChangesRequested { feedback } if feedback == "Fix the tests")
        );
    }

    #[test]
    fn parse_review_output_escalate() {
        let output = r#"{"decision": "escalate"}"#;
        let result = parse_review_output(output).unwrap();
        assert!(matches!(result, ReviewResult::Escalate));
    }

    #[test]
    fn parse_review_output_no_json() {
        let output = "just some text with no JSON";
        assert!(parse_review_output(output).is_err());
    }
}
