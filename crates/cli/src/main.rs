use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use futures::StreamExt;

use chuggernaut_nats::NatsClient;
use chuggernaut_types::*;

#[derive(Parser)]
#[command(name = "chuggernaut", about = "chuggernaut workflow orchestration CLI")]
struct Cli {
    /// NATS server URL
    #[arg(long, env = "CHUGGERNAUT_NATS_URL", default_value = "nats://localhost:4222")]
    nats_url: String,

    /// Dispatcher HTTP API URL
    #[arg(long, env = "CHUGGERNAUT_DISPATCHER_URL", default_value = "http://localhost:8080")]
    dispatcher_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new job
    Create {
        /// Repository in owner/repo format
        #[arg(long)]
        repo: String,
        /// Job title
        #[arg(long)]
        title: String,
        /// Job body/description
        #[arg(long, default_value = "")]
        body: String,
        /// Dependency sequence numbers (comma-separated)
        #[arg(long, value_delimiter = ',')]
        deps: Vec<u64>,
        /// Priority (0-100, default 50)
        #[arg(long, default_value = "50")]
        priority: u8,
        /// Required capabilities (comma-separated)
        #[arg(long, value_delimiter = ',')]
        capabilities: Vec<String>,
        /// Review level: human, low, medium, high
        #[arg(long, default_value = "high")]
        review: String,
        /// Initial state: on-ice (omit for auto-compute)
        #[arg(long)]
        initial_state: Option<String>,
    },
    /// List jobs
    Jobs {
        /// Filter by state
        #[arg(long)]
        state: Option<String>,
    },
    /// Show job details
    Show {
        /// Job key (e.g., acme.payments.57)
        key: String,
    },
    /// Requeue a job
    Requeue {
        /// Job key
        key: String,
        /// Target state: on-deck or on-ice
        #[arg(long, default_value = "on-deck")]
        target: String,
    },
    /// Close a job
    Close {
        /// Job key
        key: String,
        /// Revoke instead of Done
        #[arg(long)]
        revoke: bool,
    },
    /// Channel communication with a running Claude session
    Channel {
        #[command(subcommand)]
        action: ChannelAction,
    },
    /// Seed jobs from a fixture file
    Seed {
        /// Repository in owner/repo format
        repo: String,
        /// Path to fixture JSON file
        fixture: String,
        /// Variable substitutions for {{KEY}} placeholders in job bodies (e.g. --var DISPATCHER_URL=https://example.com)
        #[arg(long = "var", value_delimiter = ',')]
        vars: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ChannelAction {
    /// Send a message to a running Claude session
    Send {
        /// Job key (e.g. acme.payments.57)
        key: String,
        /// Message to send
        message: String,
        /// Sender identity
        #[arg(long, default_value = "cli")]
        sender: String,
    },
    /// Watch outgoing messages from a Claude session
    Watch {
        /// Job key (e.g. acme.payments.57)
        key: String,
    },
    /// Read the latest status from a Claude session
    Status {
        /// Job key (e.g. acme.payments.57)
        key: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Create {
            repo,
            title,
            body,
            deps,
            priority,
            capabilities,
            review,
            initial_state,
        } => {
            let review_level: ReviewLevel = serde_json::from_str(&format!("\"{review}\""))?;
            let initial = initial_state
                .map(|s| serde_json::from_str::<JobState>(&format!("\"{s}\"")))
                .transpose()?;

            let req = CreateJobRequest {
                repo,
                title,
                body,
                depends_on: deps,
                priority,
                capabilities,
                platform: None,
                timeout_secs: 3600,
                review: review_level,
                max_retries: 3,
                initial_state: initial,
            };

            let nats = NatsClient::new(async_nats::connect(&cli.nats_url).await?);
            let reply = tokio::time::timeout(
                Duration::from_secs(5),
                nats.request_msg(&subjects::ADMIN_CREATE_JOB, &req),
            )
            .await??;

            let resp: serde_json::Value = serde_json::from_slice(&reply.payload)?;
            if let Some(key) = resp.get("key") {
                println!("{}", key.as_str().unwrap_or(""));
            } else if let Some(err) = resp.get("error") {
                eprintln!("Error: {}", err.as_str().unwrap_or("unknown"));
                std::process::exit(1);
            }
        }

        Commands::Jobs { state } => {
            let mut url = format!("{}/jobs", cli.dispatcher_url);
            if let Some(s) = state {
                url.push_str(&format!("?state={s}"));
            }
            let resp: JobListResponse = reqwest::get(&url).await?.json().await?;
            if resp.jobs.is_empty() {
                println!("No jobs found.");
            } else {
                println!(
                    "{:<25} {:<16} {:<4} {}",
                    "KEY", "STATE", "PRI", "TITLE"
                );
                for job in &resp.jobs {
                    let state_str = serde_json::to_string(&job.state)?;
                    println!(
                        "{:<25} {:<16} {:<4} {}",
                        job.key,
                        state_str.trim_matches('"'),
                        job.priority,
                        job.title
                    );
                }
            }
        }

        Commands::Show { key } => {
            let url = format!("{}/jobs/{key}", cli.dispatcher_url);
            let resp: JobDetailResponse = reqwest::get(&url).await?.json().await?;
            println!("{}", serde_json::to_string_pretty(&resp.job)?);
            if let Some(claim) = resp.claim {
                println!("\nClaim:");
                println!("  worker: {}", claim.worker_id);
                println!("  since: {}", claim.claimed_at);
                println!("  lease deadline: {}", claim.lease_deadline);
            }
            if !resp.activities.is_empty() {
                println!("\nActivities:");
                for a in &resp.activities {
                    println!("  [{}] {}: {}", a.timestamp, a.kind, a.message);
                }
            }
        }

        Commands::Requeue { key, target } => {
            let target: RequeueTarget = serde_json::from_str(&format!("\"{target}\""))?;
            let req = RequeueRequest {
                job_key: key.clone(),
                target,
            };
            let nats = NatsClient::new(async_nats::connect(&cli.nats_url).await?);
            let reply = tokio::time::timeout(
                Duration::from_secs(5),
                nats.request_msg(&subjects::ADMIN_REQUEUE, &req),
            )
            .await??;

            let resp: serde_json::Value = serde_json::from_slice(&reply.payload)?;
            if let Some(err) = resp.get("error") {
                eprintln!("Error: {}", err.as_str().unwrap_or("unknown"));
                std::process::exit(1);
            }
            println!("Requeued {key}");
        }

        Commands::Close { key, revoke } => {
            let req = CloseJobRequest {
                job_key: key.clone(),
                revoke,
            };
            let nats = NatsClient::new(async_nats::connect(&cli.nats_url).await?);
            let reply = tokio::time::timeout(
                Duration::from_secs(5),
                nats.request_msg(&subjects::ADMIN_CLOSE_JOB, &req),
            )
            .await??;

            let resp: serde_json::Value = serde_json::from_slice(&reply.payload)?;
            if let Some(err) = resp.get("error") {
                eprintln!("Error: {}", err.as_str().unwrap_or("unknown"));
                std::process::exit(1);
            }
            let action = if revoke { "Revoked" } else { "Closed" };
            println!("{action} {key}");
        }

        Commands::Channel { action } => match action {
            ChannelAction::Send {
                key,
                message,
                sender,
            } => {
                let msg = chuggernaut_types::ChannelMessage {
                    sender,
                    body: message,
                    timestamp: chrono::Utc::now(),
                    message_id: uuid::Uuid::new_v4().to_string(),
                    in_reply_to: None,
                };
                let nats = NatsClient::new(async_nats::connect(&cli.nats_url).await?);
                nats.publish_to(&subjects::CHANNEL_INBOX, &key, &msg).await?;
                nats.raw().flush().await?;
                println!("Sent to {key}");
            }

            ChannelAction::Watch { key } => {
                let nats = NatsClient::new(async_nats::connect(&cli.nats_url).await?);
                let mut sub = nats.subscribe_dynamic(&subjects::CHANNEL_OUTBOX, &key).await?;
                println!("Watching channel outbox for {key} (Ctrl-C to stop)");
                while let Some(msg) = tokio::time::timeout(
                    Duration::from_secs(3600),
                    sub.next(),
                )
                .await
                .ok()
                .flatten()
                {
                    if let Ok(channel_msg) =
                        serde_json::from_slice::<chuggernaut_types::ChannelMessage>(&msg.payload)
                    {
                        println!(
                            "[{}] {}: {}",
                            channel_msg.timestamp.format("%H:%M:%S"),
                            channel_msg.sender,
                            channel_msg.body
                        );
                    }
                }
            }

            ChannelAction::Status { key } => {
                let client = async_nats::connect(&cli.nats_url).await?;
                let js = async_nats::jetstream::new(client);
                match js.get_key_value(chuggernaut_types::buckets::CHANNELS).await {
                    Ok(kv) => match kv.entry(&key).await? {
                        Some(entry) => {
                            let status: chuggernaut_types::ChannelStatus =
                                serde_json::from_slice(&entry.value)?;
                            println!("Job:      {}", status.job_key);
                            println!("Status:   {}", status.status);
                            if let Some(p) = status.progress {
                                println!("Progress: {:.0}%", p * 100.0);
                            }
                            println!("Updated:  {}", status.updated_at);
                        }
                        None => {
                            println!("No status available for {key}");
                        }
                    },
                    Err(_) => {
                        println!("No status available for {key} (channels bucket not found)");
                    }
                }
            }
        },

        Commands::Seed { repo, fixture, vars } => {
            // Parse --var KEY=VALUE pairs into a map
            let var_map: std::collections::HashMap<String, String> = vars
                .iter()
                .filter_map(|v| {
                    let (k, val) = v.split_once('=')?;
                    Some((k.to_string(), val.to_string()))
                })
                .collect();

            let mut content = std::fs::read_to_string(&fixture)?;

            // Interpolate {{KEY}} placeholders in the fixture
            for (key, value) in &var_map {
                content = content.replace(&format!("{{{{{key}}}}}"), value);
            }

            let fixture: SeedFixture = serde_json::from_str(&content)?;

            let nats = NatsClient::new(async_nats::connect(&cli.nats_url).await?);
            let mut keys: Vec<String> = Vec::new();

            for job_def in &fixture.jobs {
                let deps: Vec<u64> = job_def
                    .deps
                    .iter()
                    .filter_map(|&idx| {
                        // deps reference 1-based indices into the fixture
                        if idx == 0 || idx > keys.len() {
                            None
                        } else {
                            // Extract sequence number from the key
                            parse_job_key(&keys[idx - 1]).map(|(_, _, seq)| seq)
                        }
                    })
                    .collect();

                let req = CreateJobRequest {
                    repo: repo.clone(),
                    title: job_def.title.clone(),
                    body: job_def.body.clone().unwrap_or_default(),
                    depends_on: deps,
                    priority: job_def.priority.unwrap_or(50),
                    capabilities: job_def.capabilities.clone().unwrap_or_default(),
                    platform: None,
                    timeout_secs: 3600,
                    review: ReviewLevel::High,
                    max_retries: 3,
                    initial_state: None,
                };

                let reply = tokio::time::timeout(
                    Duration::from_secs(5),
                    nats.request_msg(&subjects::ADMIN_CREATE_JOB, &req),
                )
                .await??;

                let resp: serde_json::Value = serde_json::from_slice(&reply.payload)?;
                if let Some(key) = resp.get("key").and_then(|k| k.as_str()) {
                    println!("Created: {key} — {}", job_def.title);
                    keys.push(key.to_string());
                } else {
                    let err = resp
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown");
                    eprintln!("Error creating '{}': {err}", job_def.title);
                    keys.push(String::new()); // placeholder to keep indices aligned
                }
            }

            println!("\nSeeded {} jobs", keys.len());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Seed fixture types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct SeedFixture {
    jobs: Vec<SeedJobDef>,
}

#[derive(serde::Deserialize)]
struct SeedJobDef {
    title: String,
    body: Option<String>,
    #[serde(default)]
    deps: Vec<usize>,
    priority: Option<u8>,
    capabilities: Option<Vec<String>>,
}
