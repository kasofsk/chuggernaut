use std::sync::Arc;
use std::time::Duration;

use chuggernaut_nats::NatsClient;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use chuggernaut_types::*;

use crate::config::WorkerConfig;

/// Trait for worker execution implementations.
#[async_trait::async_trait]
pub trait WorkerExecutor: Send + Sync {
    /// Execute a job assignment. Returns an OutcomeType.
    async fn execute(
        &self,
        assignment: &Assignment,
        cancel: CancellationToken,
    ) -> OutcomeType;
}

/// Run the worker lifecycle loop.
pub async fn run(config: WorkerConfig, executor: Arc<dyn WorkerExecutor>) -> anyhow::Result<()> {
    let client = async_nats::connect(&config.nats_url).await?;
    let nats = NatsClient::new(client);
    info!(worker_id = config.worker_id, "connected to NATS");

    // Register
    register(&nats, &config).await?;

    // Subscribe to assignment and preempt channels
    let mut assign_sub = nats.subscribe_dynamic(&subjects::DISPATCH_ASSIGN, &config.worker_id).await?;
    let mut preempt_sub = nats.subscribe_dynamic(&subjects::DISPATCH_PREEMPT, &config.worker_id).await?;
    let _help_sub = nats.subscribe_dynamic(&subjects::INTERACT_DELIVER, &config.worker_id).await?;

    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    // Handle SIGTERM / CTRL+C
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("shutdown signal received");
        shutdown_clone.cancel();
    });

    loop {
        // Publish idle
        publish_idle(&nats, &config.worker_id).await;

        // Wait for assignment or shutdown
        let assignment: Assignment;
        tokio::select! {
            msg = assign_sub.next() => {
                match msg {
                    Some(msg) => {
                        match serde_json::from_slice::<Assignment>(&msg.payload) {
                            Ok(a) => assignment = a,
                            Err(e) => {
                                warn!("invalid assignment payload: {e}");
                                continue;
                            }
                        }
                    }
                    None => break, // subscription closed
                }
            }
            _ = reregister_loop(&nats, &config) => {
                // This arm completes only if the loop ends (shouldn't)
                continue;
            }
            _ = shutdown.cancelled() => {
                unregister(&nats, &config.worker_id).await;
                return Ok(());
            }
        }

        info!(
            job_key = assignment.job.key,
            is_rework = assignment.is_rework,
            "received assignment"
        );

        // Execute the job
        let cancel = CancellationToken::new();
        let cancel_for_heartbeat = cancel.clone();
        let job_key = assignment.job.key.clone();
        let worker_id = config.worker_id.clone();

        // Heartbeat task
        let nats_hb = nats.clone();
        let hb_job_key = job_key.clone();
        let hb_worker_id = worker_id.clone();
        let hb_interval = Duration::from_secs(config.heartbeat_interval_secs);
        let hb_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(hb_interval);
            let mut consecutive_failures = 0u32;
            loop {
                interval.tick().await;
                if cancel_for_heartbeat.is_cancelled() {
                    break;
                }
                let hb = WorkerHeartbeat {
                    worker_id: hb_worker_id.clone(),
                    job_key: hb_job_key.clone(),
                };
                match nats_hb
                    .publish_msg(&subjects::WORKER_HEARTBEAT, &hb)
                    .await
                {
                    Ok(_) => {
                        consecutive_failures = 0;
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        warn!(consecutive_failures, "heartbeat publish failed: {e}");
                        if consecutive_failures >= 3 {
                            error!("3 consecutive heartbeat failures, cancelling");
                            cancel_for_heartbeat.cancel();
                            break;
                        }
                    }
                }
            }
        });

        // Preemption handled via select on preempt_sub below

        // Execute
        let outcome = tokio::select! {
            outcome = executor.execute(&assignment, cancel.clone()) => {
                outcome
            }
            msg = preempt_sub.next() => {
                if let Some(msg) = msg {
                    if let Ok(notice) = serde_json::from_slice::<PreemptNotice>(&msg.payload) {
                        info!(reason = notice.reason, "preempted");
                    }
                }
                cancel.cancel();
                OutcomeType::Abandon {}
            }
            _ = shutdown.cancelled() => {
                cancel.cancel();
                OutcomeType::Abandon {}
            }
        };

        // Stop heartbeat
        cancel.cancel();
        hb_handle.abort();

        // Report outcome
        let worker_outcome = WorkerOutcome {
            worker_id: worker_id.clone(),
            job_key: job_key.clone(),
            outcome,
        };
        nats.publish_msg(&subjects::WORKER_OUTCOME, &worker_outcome)
            .await?;

        info!(job_key, "outcome reported");

        // If shutdown was triggered during execution, exit
        if shutdown.is_cancelled() {
            unregister(&nats, &config.worker_id).await;
            return Ok(());
        }
    }

    Ok(())
}

async fn register(nats: &NatsClient, config: &WorkerConfig) -> anyhow::Result<()> {
    let reg = WorkerRegistration {
        worker_id: config.worker_id.clone(),
        capabilities: config.capabilities.clone(),
        worker_type: config.worker_type.clone(),
        platform: config.platform.clone(),
    };

    // Request-reply with timeout
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        nats.request_msg(&subjects::WORKER_REGISTER, &reg),
    )
    .await;

    match reply {
        Ok(Ok(msg)) => {
            // Check for error in reply
            if let Ok(err) = serde_json::from_slice::<ErrorResponse>(&msg.payload) {
                if !err.error.is_empty() && err.error != "{}" {
                    anyhow::bail!("registration rejected: {}", err.error);
                }
            }
            info!(worker_id = config.worker_id, "registered");
            Ok(())
        }
        Ok(Err(e)) => anyhow::bail!("registration failed: {e}"),
        Err(_) => anyhow::bail!("registration timed out"),
    }
}

async fn publish_idle(nats: &NatsClient, worker_id: &str) {
    let event = IdleEvent {
        worker_id: worker_id.to_string(),
    };
    if let Err(e) = nats
        .publish_msg(&subjects::WORKER_IDLE, &event)
        .await
    {
        warn!("failed to publish idle: {e}");
    }
}

async fn unregister(nats: &NatsClient, worker_id: &str) {
    let event = UnregisterEvent {
        worker_id: worker_id.to_string(),
    };
    if let Err(e) = nats
        .publish_msg(&subjects::WORKER_UNREGISTER, &event)
        .await
    {
        warn!("failed to publish unregister: {e}");
    }
    info!(worker_id, "unregistered");
}

/// Re-register periodically while idle.
async fn reregister_loop(nats: &NatsClient, config: &WorkerConfig) {
    let mut interval = tokio::time::interval(Duration::from_secs(config.reregister_interval_secs));
    loop {
        interval.tick().await;
        if let Err(e) = register(nats, config).await {
            debug!("re-registration failed: {e}");
        }
    }
}

/// Publish an activity update for the current job.
pub async fn publish_activity(nats: &NatsClient, job_key: &str, kind: &str, message: &str) {
    let append = ActivityAppend {
        job_key: job_key.to_string(),
        entry: ActivityEntry {
            timestamp: chrono::Utc::now(),
            kind: kind.to_string(),
            message: message.to_string(),
        },
    };
    if let Err(e) = nats
        .publish_msg(&subjects::ACTIVITY_APPEND, &append)
        .await
    {
        debug!("failed to publish activity: {e}");
    }
}
