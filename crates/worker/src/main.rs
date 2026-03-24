use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use chuggernaut_channel::{mcp, ChannelState};
use chuggernaut_forgejo_api::ForgejoClient;
use chuggernaut_nats::NatsClient;
use chuggernaut_types::*;

// ---------------------------------------------------------------------------
// CLI args (passed by the Forgejo Action workflow as inputs)
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "chuggernaut-worker", about = "Worker binary that runs inside Forgejo Actions")]
struct Args {
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

    /// Command to run (e.g. "claude-code" or a mock script for testing)
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
        "starting chuggernaut-worker"
    );

    // Parse repo from job key
    let (owner, repo, _seq) = parse_job_key(&args.job_key)
        .ok_or_else(|| anyhow::anyhow!("invalid job key: {}", args.job_key))?;
    let repo_full = format!("{owner}/{repo}");

    // Connect to NATS
    let nats = NatsClient::new(async_nats::connect(&args.nats_url).await?);
    let js = async_nats::jetstream::new(nats.raw().clone());
    info!(url = args.nats_url, "connected to NATS");

    // Ensure channels KV bucket exists
    js.create_key_value(async_nats::jetstream::kv::Config {
        bucket: buckets::CHANNELS.to_string(),
        history: 1,
        storage: async_nats::jetstream::stream::StorageType::File,
        ..Default::default()
    })
    .await?;

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

    // Start heartbeat task
    let hb_nats = nats.clone();
    let hb_job_key = args.job_key.clone();
    let hb_cancel = cancel.clone();
    let hb_interval = Duration::from_secs(args.heartbeat_interval_secs);
    tokio::spawn(async move {
        let worker_id = format!("action-{}", hb_job_key);
        let mut interval = tokio::time::interval(hb_interval);
        let mut consecutive_failures = 0u32;
        loop {
            interval.tick().await;
            if hb_cancel.is_cancelled() {
                break;
            }
            let hb = WorkerHeartbeat {
                worker_id: worker_id.clone(),
                job_key: hb_job_key.clone(),
            };
            match hb_nats.publish_msg(&subjects::WORKER_HEARTBEAT, &hb).await {
                Ok(_) => {
                    consecutive_failures = 0;
                }
                Err(e) => {
                    consecutive_failures += 1;
                    warn!(consecutive_failures, "heartbeat failed: {e}");
                    if consecutive_failures >= 3 {
                        error!("3 consecutive heartbeat failures, cancelling");
                        hb_cancel.cancel();
                        break;
                    }
                }
            }
        }
    });

    // Start channel MCP state + inbox listener
    let channel_state = Arc::new(Mutex::new(ChannelState::new()));
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);

    tokio::spawn(chuggernaut_channel::nats_inbox_listener(
        nats.raw().clone(),
        args.job_key.clone(),
        Arc::clone(&channel_state),
        out_tx.clone(),
        args.channel_mode,
    ));

    // Subscribe to preemption
    let mut preempt_sub = nats
        .raw()
        .subscribe(subjects::DISPATCH_PREEMPT.format(&format!("action-{}", args.job_key)))
        .await?;

    // Launch the command (Claude Code or mock) as a subprocess
    // The channel MCP server communicates via the subprocess's stdin/stdout
    info!(command = args.command, "launching subprocess");
    let cmd_args: Vec<&str> = args
        .command_args
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect();

    // If using claude, add MCP config
    // For now, the command is responsible for its own MCP setup
    let mut child = Command::new(&args.command)
        .args(&cmd_args)
        .current_dir(&repo_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    let child_stdin = child.stdin.take().expect("stdin piped");
    let child_stdout = child.stdout.take().expect("stdout piped");

    // Bridge: child stdout → MCP handler → channel state / NATS
    // Bridge: channel out_rx → child stdin (MCP notifications)
    let mut stdin_writer = tokio::io::BufWriter::new(child_stdin);
    let mut stdout_reader = BufReader::new(child_stdout);

    // MCP request/response loop + preemption monitoring
    let mcp_nats = nats.raw().clone();
    let mcp_state = Arc::clone(&channel_state);
    let mcp_job_key = args.job_key.clone();
    let mcp_channel_mode = args.channel_mode;
    let _mcp_cancel = cancel.clone();

    let outcome = tokio::select! {
        // Main MCP loop: read from child stdout, process, write to child stdin
        result = async {
            let mut line_buf = String::new();
            loop {
                line_buf.clear();
                let n = stdout_reader.read_line(&mut line_buf).await?;
                if n == 0 {
                    // Child closed stdout (exited)
                    break;
                }
                let line = line_buf.trim();
                if line.is_empty() {
                    continue;
                }

                // Parse as MCP request from Claude
                let req: mcp::JsonRpcRequest = match serde_json::from_str(line) {
                    Ok(r) => r,
                    Err(_e) => {
                        debug!("non-JSON line from child: {line}");
                        continue;
                    }
                };

                // Handle MCP request
                if let Some(response) = chuggernaut_channel::handle_message(
                    req,
                    &mcp_state,
                    &mcp_nats,
                    &mcp_js_placeholder(&mcp_nats, &mcp_job_key).await,
                    &mcp_job_key,
                    mcp_channel_mode,
                ).await {
                    stdin_writer.write_all(response.as_bytes()).await?;
                    stdin_writer.write_all(b"\n").await?;
                    stdin_writer.flush().await?;
                }

                // Also forward any outgoing notifications (from inbox listener)
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
                    // Check exit status
                    let status = child.wait().await?;
                    if status.success() {
                        make_outcome_yield(&forgejo, owner, repo, &branch, &args.job_key, &args.forgejo_url).await
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

        // Preemption
        msg = preempt_sub.next() => {
            if let Some(msg) = msg {
                if let Ok(notice) = serde_json::from_slice::<PreemptNotice>(&msg.payload) {
                    info!(reason = notice.reason, "preempted");
                }
            }
            cancel.cancel();
            let _ = child.kill().await;
            OutcomeType::Abandon {}
        }

        // Cancellation (heartbeat failure)
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            OutcomeType::Abandon {}
        }
    };

    // Stop heartbeat
    cancel.cancel();

    // Report outcome
    let worker_outcome = WorkerOutcome {
        worker_id: format!("action-{}", args.job_key),
        job_key: args.job_key.clone(),
        outcome,
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
    _forgejo_url: &str,
) -> OutcomeType {
    // Commit and push any remaining changes
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

    // Find or create PR
    match forgejo.find_pr_by_head(owner, repo, branch).await {
        Ok(Some(pr)) => OutcomeType::Yield {
            pr_url: pr.html_url,
        },
        Ok(None) => {
            // Create PR
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

/// Get a JetStream context for the channel MCP's KV operations.
async fn mcp_js_placeholder(
    nats: &async_nats::Client,
    _job_key: &str,
) -> async_nats::jetstream::Context {
    async_nats::jetstream::new(nats.clone())
}
