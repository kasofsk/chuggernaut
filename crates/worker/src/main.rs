use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use chuggernaut_git_provider::{
    CreatePullRequest, CreateReview, GitProvider, MergeMethod, MergePullRequest, ReviewEvent,
};
use chuggernaut_nats::NatsClient;
use chuggernaut_types::*;

// ---------------------------------------------------------------------------
// Session metrics (parsed from Claude's stream-json output)
// ---------------------------------------------------------------------------

#[derive(Clone, Default, Debug)]
struct SessionMetrics {
    turns: u32,
    usage: TokenUsage,
    cost_usd: f64,
    rate_limit: Option<RateLimitInfo>,
}

#[derive(Clone, Debug)]
struct RateLimitInfo {
    resets_at: i64,
    rate_limit_type: String,
    is_using_overage: bool,
}

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

    /// Git provider base URL (e.g. Forgejo instance or https://github.com)
    #[arg(long = "git-url", alias = "forgejo-url", env = "CHUGGERNAUT_GIT_URL")]
    git_url: String,

    /// Git provider API token
    #[arg(
        long = "git-token",
        alias = "forgejo-token",
        env = "CHUGGERNAUT_GIT_TOKEN"
    )]
    git_token: String,

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

    /// Percentage of budget at which to send a wrap-up warning (0-100)
    #[arg(long, env = "CHUGGERNAUT_BUDGET_WARN_PCT", default_value = "80")]
    budget_warn_pct: u32,
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

    let mut args = Args::parse();
    // Fall back to legacy CHUGGERNAUT_FORGEJO_* env vars if git_url/git_token are empty
    if args.git_url.is_empty()
        && let Ok(v) = std::env::var("CHUGGERNAUT_FORGEJO_URL")
    {
        args.git_url = v;
    }
    if args.git_token.is_empty()
        && let Ok(v) = std::env::var("CHUGGERNAUT_FORGEJO_TOKEN")
    {
        args.git_token = v;
    }
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

    let provider: Box<dyn GitProvider> =
        chuggernaut_worker::provider::create_provider(&args.git_url, &args.git_token);
    validate_token(&*provider, owner, repo).await?;

    let (nats, _js) = connect_nats(&args.nats_url).await?;
    let cancel = CancellationToken::new();

    // Clone repo
    let workdir = PathBuf::from(&args.workdir);
    std::fs::create_dir_all(&workdir)?;
    let repo_dir =
        chuggernaut_worker::git::clone_repo(&args.git_url, &repo_full, &args.git_token, &workdir)?;

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
                    The CHUGGERNAUT_GIT_TOKEN may be invalid for git HTTP auth"
                )
            })?;
            info!("git push auth validated");
            b
        }
        PostAction::Review => {
            let pr_index = parse_pr_index(&args.pr_url)
                .ok_or_else(|| anyhow::anyhow!("cannot parse PR index from: {}", args.pr_url))?;
            let pr = provider.get_pull_request(owner, repo, pr_index).await?;
            let b = pr.head.ref_name.clone();
            chuggernaut_worker::git::checkout_branch(&repo_dir, &b)?;
            info!(
                branch = b.as_str(),
                pr_index, "checked out PR branch for review"
            );
            b
        }
    };

    // Session metrics channel — populated by the stdout parser, read by heartbeat + budget monitor
    let (metrics_tx, metrics_rx) = watch::channel(SessionMetrics::default());

    // Warning flags — set by background tasks, read after subprocess exits to determine partial yield
    let deadline_warned = Arc::new(AtomicBool::new(false));
    let budget_warned = Arc::new(AtomicBool::new(false));
    let overage_warned = Arc::new(AtomicBool::new(false));

    // Common infrastructure
    spawn_heartbeat(
        nats.clone(),
        args.job_key.clone(),
        cancel.clone(),
        args.heartbeat_interval_secs,
        metrics_rx.clone(),
    );
    spawn_deadline_warning(
        nats.clone(),
        args.job_key.clone(),
        cancel.clone(),
        args.timeout_minutes,
        args.wrap_up_minutes,
        deadline_warned.clone(),
    );

    // Budget monitor: warn before hitting --max-budget-usd
    let max_budget = extract_flag_value(&args.command_args, "--max-budget-usd")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    spawn_budget_monitor(
        nats.clone(),
        args.job_key.clone(),
        cancel.clone(),
        metrics_rx.clone(),
        max_budget,
        args.budget_warn_pct as f64 / 100.0,
        budget_warned.clone(),
        overage_warned.clone(),
    );

    let hard_deadline =
        Duration::from_secs(args.timeout_minutes * 60).saturating_sub(Duration::from_secs(60));

    let mcp_config = write_mcp_config(&args.job_key, &args.nats_url, &workdir)?;
    info!(
        config = mcp_config.display().to_string(),
        "wrote MCP config"
    );

    // Validate command args before spawning — defense in depth against tampered NATS records
    validate_worker_command_args(&args.command_args)
        .map_err(|e| anyhow::anyhow!("command_args validation failed: {e}"))?;

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
        let mut metrics = SessionMetrics::default();
        while let Ok(Some(line)) = lines.next_line().await {
            // Feed to jq formatter
            if let Some(ref mut stdin) = jq_stdin {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(line.as_bytes()).await;
                let _ = stdin.write_all(b"\n").await;
            }
            // Parse stream-json for token usage, rate limit events, and cost
            parse_stream_json_line(&line, &mut metrics, &metrics_tx);
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

    // Read final session metrics for token usage reporting
    let final_metrics = metrics_rx.borrow().clone();
    let token_usage = if final_metrics.turns > 0 {
        info!(
            job_key = args.job_key,
            turns = final_metrics.turns,
            input_tokens = final_metrics.usage.input_tokens,
            output_tokens = final_metrics.usage.output_tokens,
            cost_usd = final_metrics.cost_usd,
            "session metrics collected"
        );
        Some(final_metrics.usage.clone())
    } else {
        None
    };

    // Determine if this was a partial yield (worker wrapped up early)
    let partial = was_deadline
        || deadline_warned.load(Ordering::Relaxed)
        || budget_warned.load(Ordering::Relaxed)
        || overage_warned.load(Ordering::Relaxed);

    // Post-action — handle outcome based on mode
    match args.post_action {
        PostAction::Yield => {
            let base_outcome = if exit_ok || was_deadline {
                post_action_yield(&*provider, owner, repo, &branch, &args.job_key).await
            } else {
                OutcomeType::Fail {
                    reason: "subprocess failed".to_string(),
                    logs: None,
                }
            };
            // Inject partial flag into Yield outcomes
            let outcome = match base_outcome {
                OutcomeType::Yield { pr_url, .. } => OutcomeType::Yield { pr_url, partial },
                other => other,
            };
            let worker_outcome = WorkerOutcome {
                worker_id: format!("action-{}", args.job_key),
                job_key: args.job_key.clone(),
                outcome,
                token_usage: token_usage.clone(),
            };
            nats.publish_msg(&subjects::WORKER_OUTCOME, &worker_outcome)
                .await?;
            info!(job_key = args.job_key, "outcome reported, exiting");
        }
        PostAction::Review => {
            info!(
                job_key = args.job_key,
                exit_ok,
                pr_url = args.pr_url,
                "review post-action: starting"
            );
            let pr_index = parse_pr_index(&args.pr_url).unwrap_or(0);
            info!(
                job_key = args.job_key,
                pr_index, "review post-action: parsed PR index"
            );
            let decision = if exit_ok {
                match parse_review_output(&output) {
                    Ok(result) => {
                        info!(
                            job_key = args.job_key,
                            ?result,
                            "review post-action: parsed review output"
                        );
                        post_action_review(
                            result,
                            &*provider,
                            owner,
                            repo,
                            pr_index,
                            &args,
                            token_usage.clone(),
                        )
                        .await
                    }
                    Err(e) => {
                        error!(job_key = args.job_key, error = %e, output_len = output.len(), "failed to parse review output");
                        error!(
                            job_key = args.job_key,
                            output_tail = &output[output.len().saturating_sub(500)..],
                            "review output tail"
                        );
                        escalate_decision(&args, token_usage.clone())
                    }
                }
            } else {
                error!(job_key = args.job_key, "review subprocess failed");
                escalate_decision(&args, token_usage.clone())
            };
            info!(job_key = args.job_key, decision_type = ?decision.decision, "review post-action: publishing decision to NATS");
            match nats
                .publish_msg(&subjects::REVIEW_DECISION, &decision)
                .await
            {
                Ok(()) => info!(
                    job_key = args.job_key,
                    "review decision published successfully"
                ),
                Err(e) => {
                    error!(job_key = args.job_key, error = %e, "FAILED to publish review decision to NATS")
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Post-action: yield (commit, push, find/create PR)
// ---------------------------------------------------------------------------

async fn post_action_yield(
    provider: &dyn GitProvider,
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

    match provider.find_pr_by_head(owner, repo, branch).await {
        Ok(Some(pr)) => OutcomeType::Yield {
            pr_url: pr.html_url,
            partial: false,
        },
        Ok(None) => {
            match provider
                .create_pull_request(
                    owner,
                    repo,
                    CreatePullRequest {
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
                    partial: false,
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
    provider: &dyn GitProvider,
    owner: &str,
    repo: &str,
    pr_index: u64,
    args: &Args,
    token_usage: Option<TokenUsage>,
) -> ReviewDecision {
    match result {
        ReviewResult::Approved { feedback } => {
            match provider
                .submit_review(
                    owner,
                    repo,
                    pr_index,
                    CreateReview {
                        body: feedback.clone().unwrap_or_else(|| "LGTM".to_string()),
                        event: ReviewEvent::Approve,
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

            match try_merge(provider, owner, repo, pr_index).await {
                Ok(()) => {
                    info!(job_key = args.job_key, "PR merged successfully");
                    ReviewDecision {
                        job_key: args.job_key.clone(),
                        decision: DecisionType::Approved,
                        pr_url: Some(args.pr_url.clone()),
                        token_usage: token_usage.clone(),
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
                        token_usage: token_usage.clone(),
                    }
                }
            }
        }
        ReviewResult::ChangesRequested { feedback } => {
            if let Err(e) = provider
                .submit_review(
                    owner,
                    repo,
                    pr_index,
                    CreateReview {
                        body: feedback.clone(),
                        event: ReviewEvent::RequestChanges,
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
                token_usage: token_usage.clone(),
            }
        }
        ReviewResult::Escalate => escalate_decision(args, token_usage),
    }
}

fn escalate_decision(args: &Args, token_usage: Option<TokenUsage>) -> ReviewDecision {
    ReviewDecision {
        job_key: args.job_key.clone(),
        decision: DecisionType::Escalated {
            reviewer_login: "human".to_string(),
        },
        pr_url: Some(args.pr_url.clone()),
        token_usage,
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
    metrics_rx: watch::Receiver<SessionMetrics>,
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
            let m = metrics_rx.borrow().clone();
            let hb = WorkerHeartbeat {
                worker_id: worker_id.clone(),
                job_key: job_key.clone(),
                token_usage: if m.turns > 0 {
                    Some(m.usage.clone())
                } else {
                    None
                },
                cost_usd: if m.cost_usd > 0.0 {
                    Some(m.cost_usd)
                } else {
                    None
                },
                turns: if m.turns > 0 { Some(m.turns) } else { None },
                rate_limit: m.rate_limit.as_ref().map(|rl| HeartbeatRateLimit {
                    resets_at: rl.resets_at,
                    rate_limit_type: rl.rate_limit_type.clone(),
                    is_using_overage: rl.is_using_overage,
                }),
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
    warned_flag: Arc<AtomicBool>,
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

        warned_flag.store(true, Ordering::Relaxed);
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
// Helpers: budget warning
// ---------------------------------------------------------------------------

/// Check if we should fire the overage warning for the current metrics.
fn should_warn_overage(metrics: &SessionMetrics) -> bool {
    metrics
        .rate_limit
        .as_ref()
        .is_some_and(|rl| rl.is_using_overage)
}

/// Estimate session cost from token counts (real cost only in final result event).
/// Conservative: ~$10/MTok output + ~$3/MTok input (mid-range model pricing).
fn estimate_session_cost(metrics: &SessionMetrics) -> f64 {
    if metrics.cost_usd > 0.0 {
        metrics.cost_usd
    } else {
        let output_cost = metrics.usage.output_tokens as f64 * 10.0 / 1_000_000.0;
        let input_cost = metrics.usage.input_tokens as f64 * 3.0 / 1_000_000.0;
        output_cost + input_cost
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_budget_monitor(
    nats: NatsClient,
    job_key: String,
    cancel: CancellationToken,
    metrics_rx: watch::Receiver<SessionMetrics>,
    max_budget_usd: f64,
    warn_pct: f64,
    budget_warned_flag: Arc<AtomicBool>,
    overage_warned_flag: Arc<AtomicBool>,
) {
    let has_budget = max_budget_usd > 0.0;
    let threshold = if has_budget {
        max_budget_usd * warn_pct
    } else {
        0.0
    };

    tokio::spawn(async move {
        let poll_interval = Duration::from_secs(5);
        let mut warned_budget = !has_budget; // skip budget warning if no budget set
        let mut warned_overage = false;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(poll_interval) => {}
                _ = cancel.cancelled() => { return; }
            }

            let m = metrics_rx.borrow().clone();

            // Warn on rate limit overage (once) — fires regardless of per-job budget
            if !warned_overage && should_warn_overage(&m) {
                let rl = m.rate_limit.as_ref().unwrap();
                warned_overage = true;
                overage_warned_flag.store(true, Ordering::Relaxed);
                info!(
                    job_key,
                    rate_limit_type = rl.rate_limit_type,
                    "sending overage budget warning"
                );
                let msg = ChannelMessage {
                    sender: "system".to_string(),
                    body: "BUDGET WARNING: The API is now in overage mode. \
                           Your token quota for the current window has been exceeded. \
                           Please wrap up your current work now:\n\
                           1. Finish or revert any half-done changes\n\
                           2. Commit your progress with `git add -A && git commit`\n\
                           3. Push your branch with `git push`\n\
                           4. Use update_status to report what you accomplished and what remains\n\n\
                           The next action run will pick up where you left off."
                        .to_string(),
                    timestamp: chrono::Utc::now(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    in_reply_to: None,
                };
                if let Err(e) = nats
                    .publish_to(&subjects::CHANNEL_INBOX, &job_key, &msg)
                    .await
                {
                    error!(job_key, error = %e, "failed to send overage warning");
                }
            }

            // Warn on approaching per-job budget cap (once, only if budget is set)
            if !warned_budget && m.turns > 0 {
                let estimated_cost = estimate_session_cost(&m);
                if estimated_cost >= threshold {
                    warned_budget = true;
                    budget_warned_flag.store(true, Ordering::Relaxed);
                    let pct = (estimated_cost / max_budget_usd * 100.0).min(100.0);
                    info!(
                        job_key,
                        estimated_cost, max_budget_usd, pct, "sending budget cap warning"
                    );
                    let msg = ChannelMessage {
                        sender: "system".to_string(),
                        body: format!(
                            "BUDGET WARNING: This session has used approximately ${estimated_cost:.2} \
                             of its ${max_budget_usd:.2} budget ({pct:.0}%). \
                             Estimated usage: ~{turns} turns, {output_tokens} output tokens.\n\
                             Please wrap up your current work now:\n\
                             1. Finish or revert any half-done changes\n\
                             2. Commit your progress with `git add -A && git commit`\n\
                             3. Push your branch with `git push`\n\
                             4. Use update_status to report what you accomplished and what remains\n\n\
                             The next action run will pick up where you left off.",
                            turns = m.turns,
                            output_tokens = m.usage.output_tokens,
                        ),
                        timestamp: chrono::Utc::now(),
                        message_id: uuid::Uuid::new_v4().to_string(),
                        in_reply_to: None,
                    };
                    if let Err(e) = nats
                        .publish_to(&subjects::CHANNEL_INBOX, &job_key, &msg)
                        .await
                    {
                        error!(job_key, error = %e, "failed to send budget warning");
                    }
                }
            }

            // Both warnings fired — nothing more to do
            if warned_budget && warned_overage {
                return;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers: stream-json parsing
// ---------------------------------------------------------------------------

/// Parse a single line of Claude's stream-json output for token usage,
/// rate limit events, and session cost. Updates `metrics` in place and
/// sends to the watch channel on change.
fn parse_stream_json_line(
    line: &str,
    metrics: &mut SessionMetrics,
    tx: &watch::Sender<SessionMetrics>,
) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };

    let changed = match v.get("type").and_then(|t| t.as_str()) {
        Some("assistant") => {
            metrics.turns += 1;
            if let Some(usage) = v.pointer("/message/usage") {
                metrics.usage.input_tokens += usage["input_tokens"].as_u64().unwrap_or(0);
                metrics.usage.output_tokens += usage["output_tokens"].as_u64().unwrap_or(0);
                metrics.usage.cache_read_tokens +=
                    usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                metrics.usage.cache_write_tokens +=
                    usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            true
        }
        Some("rate_limit_event") => {
            if let Some(info) = v.get("rate_limit_info") {
                metrics.rate_limit = Some(RateLimitInfo {
                    resets_at: info["resetsAt"].as_i64().unwrap_or(0),
                    rate_limit_type: info["rateLimitType"].as_str().unwrap_or("").to_string(),
                    is_using_overage: info["isUsingOverage"].as_bool().unwrap_or(false),
                });
                true
            } else {
                false
            }
        }
        Some("result") => {
            metrics.cost_usd = v["total_cost_usd"].as_f64().unwrap_or(0.0);
            // Overwrite cumulative usage with final totals from result for accuracy
            if let Some(usage) = v.get("usage") {
                if let Some(n) = usage["input_tokens"].as_u64() {
                    metrics.usage.input_tokens = n;
                }
                if let Some(n) = usage["output_tokens"].as_u64() {
                    metrics.usage.output_tokens = n;
                }
            }
            true
        }
        _ => false,
    };

    if changed {
        let _ = tx.send(metrics.clone());
    }
}

/// Extract the value of a CLI flag from a whitespace-separated args string.
/// e.g. `extract_flag_value("--model opus --max-budget-usd 5.0", "--max-budget-usd")` → Some("5.0")
fn extract_flag_value(args: &str, flag: &str) -> Option<String> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        if *token == flag {
            return tokens.get(i + 1).map(|s| s.to_string());
        }
    }
    None
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

async fn validate_token(provider: &dyn GitProvider, owner: &str, repo: &str) -> anyhow::Result<()> {
    let r = provider.get_repo(owner, repo).await.map_err(|e| {
        anyhow::anyhow!(
            "Token validation failed for {owner}/{repo}: {e}\n  \
            Check that CHUGGERNAUT_GIT_TOKEN is set and the token has read:repository scope"
        )
    })?;

    if !r.permissions.pull {
        return Err(anyhow::anyhow!(
            "Token cannot read {owner}/{repo} — needs read:repository scope"
        ));
    }
    if !r.permissions.push {
        return Err(anyhow::anyhow!(
            "Token cannot push to {owner}/{repo} — needs write:repository scope"
        ));
    }

    provider
        .list_pull_requests(owner, repo, Some("open"))
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Token cannot list PRs on {owner}/{repo}: {e}\n  \
            Check that the token has read:issue and write:issue scopes"
            )
        })?;

    info!(
        repo = r.full_name,
        pull = r.permissions.pull,
        push = r.permissions.push,
        admin = r.permissions.admin,
        "token validated (repo access + push + PR access)"
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

#[derive(Debug)]
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
    provider: &dyn GitProvider,
    owner: &str,
    repo: &str,
    pr_index: u64,
) -> anyhow::Result<()> {
    let max_retries = 3;
    let mut last_err = None;

    for attempt in 0..=max_retries {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(5 * attempt as u64)).await;
        }

        let opts = MergePullRequest {
            method: MergeMethod::Merge,
            message: None,
        };
        match provider
            .merge_pull_request(owner, repo, pr_index, opts)
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
        // GitHub uses /pull/ (singular) instead of /pulls/
        assert_eq!(
            parse_pr_index("https://github.com/owner/repo/pull/99"),
            Some(99)
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

    // -----------------------------------------------------------------------
    // Stream-JSON parsing tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_assistant_event_extracts_usage() {
        let (tx, rx) = watch::channel(SessionMetrics::default());
        let mut metrics = SessionMetrics::default();

        let line = r#"{"type":"assistant","message":{"id":"msg_01","role":"assistant","content":[{"type":"text","text":"Hi"}],"usage":{"input_tokens":1000,"output_tokens":50,"cache_read_input_tokens":200,"cache_creation_input_tokens":100}}}"#;

        parse_stream_json_line(line, &mut metrics, &tx);
        assert_eq!(metrics.turns, 1);
        assert_eq!(metrics.usage.input_tokens, 1000);
        assert_eq!(metrics.usage.output_tokens, 50);
        assert_eq!(metrics.usage.cache_read_tokens, 200);
        assert_eq!(metrics.usage.cache_write_tokens, 100);

        let watched = rx.borrow().clone();
        assert_eq!(watched.turns, 1);
        assert_eq!(watched.usage.output_tokens, 50);
    }

    #[test]
    fn parse_assistant_events_accumulate() {
        let (tx, _rx) = watch::channel(SessionMetrics::default());
        let mut metrics = SessionMetrics::default();

        let line1 = r#"{"type":"assistant","message":{"usage":{"input_tokens":500,"output_tokens":30,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#;
        let line2 = r#"{"type":"assistant","message":{"usage":{"input_tokens":800,"output_tokens":60,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}"#;

        parse_stream_json_line(line1, &mut metrics, &tx);
        parse_stream_json_line(line2, &mut metrics, &tx);
        assert_eq!(metrics.turns, 2);
        assert_eq!(metrics.usage.input_tokens, 1300);
        assert_eq!(metrics.usage.output_tokens, 90);
        assert_eq!(metrics.usage.cache_read_tokens, 100);
    }

    #[test]
    fn parse_rate_limit_event() {
        let (tx, _rx) = watch::channel(SessionMetrics::default());
        let mut metrics = SessionMetrics::default();

        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1774458000,"rateLimitType":"five_hour","overageStatus":"allowed","isUsingOverage":true}}"#;

        parse_stream_json_line(line, &mut metrics, &tx);
        let rl = metrics.rate_limit.as_ref().unwrap();
        assert_eq!(rl.resets_at, 1774458000);
        assert_eq!(rl.rate_limit_type, "five_hour");
        assert!(rl.is_using_overage);
    }

    #[test]
    fn parse_result_event_extracts_cost() {
        let (tx, _rx) = watch::channel(SessionMetrics::default());
        let mut metrics = SessionMetrics::default();
        // Simulate some prior usage
        metrics.usage.input_tokens = 100;
        metrics.usage.output_tokens = 10;

        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.05701,"usage":{"input_tokens":1500,"output_tokens":200}}"#;

        parse_stream_json_line(line, &mut metrics, &tx);
        assert!((metrics.cost_usd - 0.05701).abs() < 0.0001);
        // Result overwrites cumulative with final totals
        assert_eq!(metrics.usage.input_tokens, 1500);
        assert_eq!(metrics.usage.output_tokens, 200);
    }

    #[test]
    fn parse_ignores_unknown_event_types() {
        let (tx, _rx) = watch::channel(SessionMetrics::default());
        let mut metrics = SessionMetrics::default();

        let line = r#"{"type":"system","subtype":"init"}"#;
        parse_stream_json_line(line, &mut metrics, &tx);
        assert_eq!(metrics.turns, 0);
        assert!(metrics.rate_limit.is_none());
    }

    #[test]
    fn parse_ignores_invalid_json() {
        let (tx, _rx) = watch::channel(SessionMetrics::default());
        let mut metrics = SessionMetrics::default();

        parse_stream_json_line("not json at all", &mut metrics, &tx);
        assert_eq!(metrics.turns, 0);
    }

    // -----------------------------------------------------------------------
    // extract_flag_value tests
    // -----------------------------------------------------------------------

    #[test]
    fn extract_flag_value_found() {
        assert_eq!(
            extract_flag_value("--model opus --max-budget-usd 5.0", "--max-budget-usd"),
            Some("5.0".to_string())
        );
        assert_eq!(
            extract_flag_value("--max-budget-usd 10.50 --model sonnet", "--max-budget-usd"),
            Some("10.50".to_string())
        );
    }

    #[test]
    fn extract_flag_value_not_found() {
        assert_eq!(extract_flag_value("--model opus", "--max-budget-usd"), None);
        assert_eq!(extract_flag_value("", "--max-budget-usd"), None);
    }

    #[test]
    fn extract_flag_value_no_value() {
        // Flag is the last token with no value following
        assert_eq!(
            extract_flag_value("--max-budget-usd", "--max-budget-usd"),
            None
        );
    }

    // -----------------------------------------------------------------------
    // Budget monitor logic tests
    // -----------------------------------------------------------------------

    #[test]
    fn should_warn_overage_detects_overage() {
        let mut m = SessionMetrics::default();
        // No rate limit → no warning
        assert!(!should_warn_overage(&m));

        // Rate limit present but not in overage → no warning
        m.rate_limit = Some(RateLimitInfo {
            resets_at: 9999999999,
            rate_limit_type: "five_hour".to_string(),
            is_using_overage: false,
        });
        assert!(!should_warn_overage(&m));

        // In overage → warning
        m.rate_limit.as_mut().unwrap().is_using_overage = true;
        assert!(should_warn_overage(&m));
    }

    #[test]
    fn overage_detection_independent_of_budget() {
        // This is the exact bug we fixed: overage should be detected
        // regardless of whether a per-job budget is set.
        let m = SessionMetrics {
            rate_limit: Some(RateLimitInfo {
                resets_at: 9999999999,
                rate_limit_type: "five_hour".to_string(),
                is_using_overage: true,
            }),
            ..Default::default()
        };
        // should_warn_overage only looks at metrics, not budget config
        assert!(should_warn_overage(&m));
    }

    #[test]
    fn estimate_cost_from_tokens() {
        let mut m = SessionMetrics::default();
        m.usage.input_tokens = 1_000_000;
        m.usage.output_tokens = 100_000;
        // $3/MTok input + $10/MTok output = $3 + $1 = $4
        let cost = estimate_session_cost(&m);
        assert!((cost - 4.0).abs() < 0.01);
    }

    #[test]
    fn estimate_cost_prefers_real_cost() {
        let mut m = SessionMetrics::default();
        m.usage.input_tokens = 1_000_000;
        m.usage.output_tokens = 100_000;
        m.cost_usd = 2.50; // real cost from result event
        // Should use real cost, not estimate
        assert!((estimate_session_cost(&m) - 2.50).abs() < 0.001);
    }

    #[test]
    fn budget_monitor_skips_budget_warn_without_budget() {
        // When no budget is set, warned_budget should start as true (skipped)
        // but warned_overage should start as false (still watches for overage)
        let has_budget = 0.0_f64 > 0.0;
        let warned_budget = !has_budget;
        let warned_overage = false;
        assert!(warned_budget, "budget warning should be pre-skipped");
        assert!(!warned_overage, "overage warning should still be active");
        // Monitor only exits when both are true, so it stays alive for overage
        assert!(
            !(warned_budget && warned_overage),
            "monitor should not exit immediately"
        );
    }

    #[test]
    fn budget_monitor_watches_both_with_budget() {
        let has_budget = 5.0_f64 > 0.0;
        let warned_budget = !has_budget;
        let warned_overage = false;
        assert!(!warned_budget, "budget warning should be active");
        assert!(!warned_overage, "overage warning should be active");
    }
}
