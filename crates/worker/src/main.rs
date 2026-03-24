use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use chuggernaut_channel::{mcp, ChannelState};
use chuggernaut_forgejo_api::{
    CreatePullReviewOptions, ForgejoClient, MergePullRequestOption,
};
use chuggernaut_nats::NatsClient;
use chuggernaut_types::*;

// ---------------------------------------------------------------------------
// CLI args (passed by the Forgejo Action workflow as inputs)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, ValueEnum)]
enum Mode {
    Work,
    Review,
}

#[derive(Parser)]
#[command(name = "chuggernaut-worker", about = "Worker binary that runs inside Forgejo Actions")]
struct Args {
    /// Operating mode
    #[arg(long, env = "CHUGGERNAUT_MODE", default_value = "work")]
    mode: Mode,

    /// Job key (e.g. acme.payments.57)
    #[arg(long, env = "CHUGGERNAUT_JOB_KEY")]
    job_key: String,

    /// NATS server URL
    #[arg(long, env = "CHUGGERNAUT_NATS_URL", default_value = "nats://localhost:4222")]
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
    #[arg(long, env = "CHUGGERNAUT_HEARTBEAT_INTERVAL_SECS", default_value = "10")]
    heartbeat_interval_secs: u64,

    /// Enable channel mode (push notifications to Claude)
    #[arg(long, env = "CHUGGERNAUT_CHANNEL_MODE", default_value = "false")]
    channel_mode: bool,

    /// Working directory for git operations
    #[arg(long, env = "CHUGGERNAUT_WORKDIR", default_value = "/tmp/chuggernaut-work")]
    workdir: String,

    /// Review feedback from a previous run (JSON or plain text)
    #[arg(long, env = "CHUGGERNAUT_REVIEW_FEEDBACK", default_value = "")]
    review_feedback: String,

    /// Whether this is a rework of a previously-reviewed run
    #[arg(long, env = "CHUGGERNAUT_IS_REWORK", default_value = "false")]
    is_rework: bool,

    // -- Review mode args --

    /// PR URL to review (review mode only)
    #[arg(long, env = "CHUGGERNAUT_PR_URL", default_value = "")]
    pr_url: String,

    /// Review level: low, medium, high (review mode only)
    #[arg(long, env = "CHUGGERNAUT_REVIEW_LEVEL", default_value = "high")]
    review_level: String,
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
        mode = ?args.mode,
        "starting chuggernaut-worker"
    );

    match args.mode {
        Mode::Work => run_work(args).await,
        Mode::Review => run_review(args).await,
    }
}

// ---------------------------------------------------------------------------
// Shared: heartbeat
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
// Shared: connect to NATS
// ---------------------------------------------------------------------------

async fn connect_nats(nats_url: &str) -> anyhow::Result<(NatsClient, async_nats::jetstream::Context)> {
    let nats = NatsClient::new(async_nats::connect(nats_url).await?);
    let js = async_nats::jetstream::new(nats.raw().clone());
    info!(url = nats_url, "connected to NATS");

    // Ensure channels KV bucket exists
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
// Work mode
// ---------------------------------------------------------------------------

async fn run_work(args: Args) -> anyhow::Result<()> {
    let (owner, repo, _seq) = parse_job_key(&args.job_key)
        .ok_or_else(|| anyhow::anyhow!("invalid job key: {}", args.job_key))?;
    let repo_full = format!("{owner}/{repo}");

    let (nats, _js) = connect_nats(&args.nats_url).await?;
    let forgejo = ForgejoClient::new(&args.forgejo_url, &args.forgejo_token);
    let cancel = CancellationToken::new();

    // Clone repo and checkout work branch
    let workdir = std::path::PathBuf::from(&args.workdir);
    std::fs::create_dir_all(&workdir)?;
    let repo_dir = chuggernaut_worker::git::clone_repo(
        &args.forgejo_url,
        &repo_full,
        &args.forgejo_token,
        &workdir,
    )?;
    let branch = format!("work/{}", args.job_key);
    chuggernaut_worker::git::checkout_branch(&repo_dir, &branch)?;
    info!(branch, "checked out work branch");

    // Heartbeat
    spawn_heartbeat(nats.clone(), args.job_key.clone(), cancel.clone(), args.heartbeat_interval_secs);

    // Channel MCP state + inbox listener
    let channel_state = Arc::new(Mutex::new(ChannelState::new()));
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);

    tokio::spawn(chuggernaut_channel::nats_inbox_listener(
        nats.raw().clone(),
        args.job_key.clone(),
        Arc::clone(&channel_state),
        out_tx.clone(),
        args.channel_mode,
    ));

    // Launch subprocess
    info!(command = args.command, "launching subprocess");
    let cmd_args: Vec<&str> = args
        .command_args
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect();

    let mut child = Command::new(&args.command)
        .args(&cmd_args)
        .current_dir(&repo_dir)
        .env("CHUGGERNAUT_REVIEW_FEEDBACK", &args.review_feedback)
        .env("CHUGGERNAUT_IS_REWORK", args.is_rework.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    let child_stdin = child.stdin.take().expect("stdin piped");
    let child_stdout = child.stdout.take().expect("stdout piped");

    let mut stdin_writer = tokio::io::BufWriter::new(child_stdin);
    let mut stdout_reader = BufReader::new(child_stdout);

    let mcp_nats = nats.raw().clone();
    let mcp_state = Arc::clone(&channel_state);
    let mcp_job_key = args.job_key.clone();
    let mcp_channel_mode = args.channel_mode;

    let outcome = tokio::select! {
        result = async {
            let mut line_buf = String::new();
            loop {
                line_buf.clear();
                let n = stdout_reader.read_line(&mut line_buf).await?;
                if n == 0 {
                    break;
                }
                let line = line_buf.trim();
                if line.is_empty() {
                    continue;
                }

                let req: mcp::JsonRpcRequest = match serde_json::from_str(line) {
                    Ok(r) => r,
                    Err(_e) => {
                        debug!("non-JSON line from child: {line}");
                        continue;
                    }
                };

                let mcp_js = async_nats::jetstream::new(mcp_nats.clone());
                if let Some(response) = chuggernaut_channel::handle_message(
                    req,
                    &mcp_state,
                    &mcp_nats,
                    &mcp_js,
                    &mcp_job_key,
                    mcp_channel_mode,
                ).await {
                    stdin_writer.write_all(response.as_bytes()).await?;
                    stdin_writer.write_all(b"\n").await?;
                    stdin_writer.flush().await?;
                }

                while let Ok(notification) = out_rx.try_recv() {
                    stdin_writer.write_all(notification.as_bytes()).await?;
                    stdin_writer.write_all(b"\n").await?;
                    stdin_writer.flush().await?;
                }
            }
            Ok::<_, anyhow::Error>(())
        } => {
            match result {
                Ok(()) => {
                    info!("subprocess exited");
                    let status = child.wait().await?;
                    if status.success() {
                        make_outcome_yield(&forgejo, owner, repo, &branch, &args.job_key).await
                    } else {
                        OutcomeType::Fail {
                            reason: format!("subprocess exited with {status}"),
                            logs: None,
                        }
                    }
                }
                Err(e) => OutcomeType::Fail {
                    reason: format!("MCP loop error: {e}"),
                    logs: None,
                },
            }
        }

        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            OutcomeType::Fail {
                reason: "heartbeat failure".to_string(),
                logs: None,
            }
        }
    };

    cancel.cancel();

    let worker_outcome = WorkerOutcome {
        worker_id: format!("action-{}", args.job_key),
        job_key: args.job_key.clone(),
        outcome,
        token_usage: None, // TODO: capture from Claude subprocess
    };
    nats.publish_msg(&subjects::WORKER_OUTCOME, &worker_outcome)
        .await?;

    info!(job_key = args.job_key, "outcome reported, exiting");
    Ok(())
}

/// After the subprocess exits successfully, check if there's work to commit
/// and find/create the PR.
async fn make_outcome_yield(
    forgejo: &ForgejoClient,
    owner: &str,
    repo: &str,
    branch: &str,
    job_key: &str,
) -> OutcomeType {
    let repo_dir = std::path::PathBuf::from(format!(
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
// Review mode
// ---------------------------------------------------------------------------

async fn run_review(args: Args) -> anyhow::Result<()> {
    let (owner, repo, _seq) = parse_job_key(&args.job_key)
        .ok_or_else(|| anyhow::anyhow!("invalid job key: {}", args.job_key))?;
    let repo_full = format!("{owner}/{repo}");

    let (nats, _js) = connect_nats(&args.nats_url).await?;
    let forgejo = ForgejoClient::new(&args.forgejo_url, &args.forgejo_token);
    let cancel = CancellationToken::new();

    // Parse PR index from URL
    let pr_index = parse_pr_index(&args.pr_url)
        .ok_or_else(|| anyhow::anyhow!("cannot parse PR index from: {}", args.pr_url))?;

    // Clone repo and checkout the PR branch so Claude can read the code
    let workdir = std::path::PathBuf::from(&args.workdir);
    std::fs::create_dir_all(&workdir)?;
    let repo_dir = chuggernaut_worker::git::clone_repo(
        &args.forgejo_url,
        &repo_full,
        &args.forgejo_token,
        &workdir,
    )?;

    // Checkout the PR's head branch
    let pr = forgejo.get_pull_request(&owner, &repo, pr_index).await?;
    let pr_branch = &pr.head.ref_field;
    chuggernaut_worker::git::checkout_branch(&repo_dir, pr_branch)?;
    info!(pr_branch = pr_branch.as_str(), pr_index, "checked out PR branch for review");

    // Heartbeat
    spawn_heartbeat(nats.clone(), args.job_key.clone(), cancel.clone(), args.heartbeat_interval_secs);

    // Get the diff for the review prompt
    let diff = get_pr_diff(&repo_dir)?;

    // Run Claude to review the diff
    let decision = run_review_subprocess(&args, &repo_dir, &diff, &cancel).await;

    cancel.cancel();

    // Act on the decision
    let review_decision = match decision {
        Ok(ReviewResult::Approved { feedback }) => {
            // Post approval review
            let _ = forgejo.submit_review(&owner, &repo, pr_index, &CreatePullReviewOptions {
                body: feedback.clone().unwrap_or_else(|| "LGTM".to_string()),
                event: "APPROVED".to_string(),
            }).await;

            // Attempt merge (with retries for lock contention)
            match try_merge(&forgejo, &owner, &repo, pr_index).await {
                Ok(()) => {
                    info!(job_key = args.job_key, "PR merged successfully");
                    ReviewDecision {
                        job_key: args.job_key.clone(),
                        decision: DecisionType::Approved,
                        pr_url: Some(args.pr_url.clone()),
                        token_usage: None, // TODO: capture from Claude
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
        Ok(ReviewResult::ChangesRequested { feedback }) => {
            // Post changes-requested review
            let _ = forgejo.submit_review(&owner, &repo, pr_index, &CreatePullReviewOptions {
                body: feedback.clone(),
                event: "REQUEST_CHANGES".to_string(),
            }).await;

            ReviewDecision {
                job_key: args.job_key.clone(),
                decision: DecisionType::ChangesRequested { feedback },
                pr_url: Some(args.pr_url.clone()),
                token_usage: None,
            }
        }
        Ok(ReviewResult::Escalate) => {
            ReviewDecision {
                job_key: args.job_key.clone(),
                decision: DecisionType::Escalated {
                    reviewer_login: "human".to_string(),
                },
                pr_url: Some(args.pr_url.clone()),
                token_usage: None,
            }
        }
        Err(e) => {
            error!(job_key = args.job_key, error = %e, "review subprocess failed");
            // Escalate on failure — don't block the pipeline
            ReviewDecision {
                job_key: args.job_key.clone(),
                decision: DecisionType::Escalated {
                    reviewer_login: "human".to_string(),
                },
                pr_url: Some(args.pr_url.clone()),
                token_usage: None,
            }
        }
    };

    nats.publish_msg(&subjects::REVIEW_DECISION, &review_decision)
        .await?;

    info!(job_key = args.job_key, "review decision reported, exiting");
    Ok(())
}

/// The structured result from a review subprocess.
enum ReviewResult {
    Approved { feedback: Option<String> },
    ChangesRequested { feedback: String },
    Escalate,
}

/// Run the review subprocess (Claude) and parse its decision.
async fn run_review_subprocess(
    args: &Args,
    repo_dir: &std::path::Path,
    diff: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<ReviewResult> {
    let cmd_args: Vec<&str> = args
        .command_args
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect();

    let mut child = Command::new(&args.command)
        .args(&cmd_args)
        .current_dir(repo_dir)
        .env("CHUGGERNAUT_MODE", "review")
        .env("CHUGGERNAUT_REVIEW_LEVEL", &args.review_level)
        .env("CHUGGERNAUT_PR_DIFF", diff)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);
    let mut output = String::new();

    tokio::select! {
        result = async {
            let mut line_buf = String::new();
            loop {
                line_buf.clear();
                let n = reader.read_line(&mut line_buf).await?;
                if n == 0 { break; }
                output.push_str(&line_buf);
            }
            child.wait().await
        } => {
            let status = result?;
            if !status.success() {
                return Err(anyhow::anyhow!("review subprocess exited with {status}"));
            }
            parse_review_output(&output)
        }

        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            Err(anyhow::anyhow!("cancelled by heartbeat failure"))
        }
    }
}

/// Parse the review subprocess output.
/// Expected JSON: {"decision": "approved"|"changes_requested", "feedback": "..."}
fn parse_review_output(output: &str) -> anyhow::Result<ReviewResult> {
    // Try to find a JSON object in the output (Claude may output other text)
    for line in output.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let decision = v.get("decision").and_then(|d| d.as_str()).unwrap_or("");
            let feedback = v.get("feedback").and_then(|f| f.as_str()).map(|s| s.to_string());

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

/// Get the diff between main and HEAD.
fn get_pr_diff(repo_dir: &std::path::Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .current_dir(repo_dir)
        .args(["diff", "origin/main...HEAD"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("git diff failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Parse PR index from a URL like "http://forgejo/owner/repo/pulls/123".
fn parse_pr_index(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

/// Attempt to merge a PR, retrying a few times if locked.
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

        match forgejo.merge_pull_request(owner, repo, pr_index, &opts).await {
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
        assert_eq!(parse_pr_index("http://forgejo/acme/repo/pulls/42"), Some(42));
        assert_eq!(parse_pr_index("http://forgejo:3000/org/project/pulls/1"), Some(1));
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
        assert!(matches!(result, ReviewResult::Approved { feedback } if feedback == Some("Looks good".to_string())));
    }

    #[test]
    fn parse_review_output_changes_requested() {
        let output = r#"{"decision": "changes_requested", "feedback": "Fix the tests"}"#;
        let result = parse_review_output(output).unwrap();
        assert!(matches!(result, ReviewResult::ChangesRequested { feedback } if feedback == "Fix the tests"));
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
