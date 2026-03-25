use std::collections::HashMap;
use std::sync::Arc;

use async_nats::jetstream;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use petgraph::graph::{DiGraph, NodeIndex};
use tokio::sync::RwLock;

use chuggernaut_types::{HeartbeatRateLimit, Job, TokenUsage};

use chuggernaut_nats::NatsClient;

use crate::config::Config;
use crate::nats_init::KvStores;

// ---------------------------------------------------------------------------
// Token tracking
// ---------------------------------------------------------------------------

/// Tracks token usage and rate limit state across all active workers.
pub struct TokenTracker {
    /// Per-worker latest snapshot from heartbeat.
    pub snapshots: HashMap<String, WorkerSnapshot>,
    /// Most recent rate limit event from any worker.
    pub rate_limit: Option<GlobalRateLimit>,
}

pub struct WorkerSnapshot {
    pub job_key: String,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub updated_at: DateTime<Utc>,
}

pub struct GlobalRateLimit {
    pub resets_at: DateTime<Utc>,
    pub rate_limit_type: String,
    pub is_using_overage: bool,
    pub reported_at: DateTime<Utc>,
    pub reported_by: String,
}

impl TokenTracker {
    pub fn new() -> Self {
        Self {
            snapshots: HashMap::new(),
            rate_limit: None,
        }
    }

    pub fn update_usage(&mut self, job_key: &str, usage: &TokenUsage, cost_usd: f64) {
        self.snapshots.insert(
            job_key.to_string(),
            WorkerSnapshot {
                job_key: job_key.to_string(),
                usage: usage.clone(),
                cost_usd,
                updated_at: Utc::now(),
            },
        );
    }

    pub fn update_rate_limit(&mut self, job_key: &str, rl: &HeartbeatRateLimit) {
        self.rate_limit = Some(GlobalRateLimit {
            resets_at: DateTime::from_timestamp(rl.resets_at, 0).unwrap_or_default(),
            rate_limit_type: rl.rate_limit_type.clone(),
            is_using_overage: rl.is_using_overage,
            reported_at: Utc::now(),
            reported_by: job_key.to_string(),
        });
    }

    pub fn remove_worker(&mut self, job_key: &str) {
        self.snapshots.remove(job_key);
    }

    /// Clear stale rate limit state (resets_at is in the past).
    pub fn clear_stale_rate_limit(&mut self) {
        if let Some(ref rl) = self.rate_limit
            && rl.resets_at < Utc::now()
        {
            self.rate_limit = None;
        }
    }
}

impl Default for TokenTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Dispatcher state
// ---------------------------------------------------------------------------

/// Shared dispatcher state, wrapped in Arc for use across tasks.
pub struct DispatcherState {
    pub config: Config,
    pub nats: NatsClient,
    pub kv: KvStores,

    /// In-memory job index: job_key -> Job
    pub jobs: DashMap<String, Job>,

    /// In-memory dependency DAG + node index lookup.
    /// Protected by RwLock since graph mutations (add/remove nodes/edges)
    /// need exclusive access, but reads are frequent.
    pub graph: RwLock<DepGraph>,

    /// Token usage and rate limit tracking across workers.
    pub token_tracker: RwLock<TokenTracker>,
}

pub struct DepGraph {
    pub dag: DiGraph<String, ()>,
    pub index: std::collections::HashMap<String, NodeIndex>,
}

impl Default for DepGraph {
    fn default() -> Self {
        Self {
            dag: DiGraph::new(),
            index: std::collections::HashMap::new(),
        }
    }
}

impl DepGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create a node for the given job key.
    pub fn ensure_node(&mut self, key: &str) -> NodeIndex {
        if let Some(&idx) = self.index.get(key) {
            idx
        } else {
            let idx = self.dag.add_node(key.to_string());
            self.index.insert(key.to_string(), idx);
            idx
        }
    }

    /// Remove a node and all its edges.
    pub fn remove_node(&mut self, key: &str) {
        if let Some(&idx) = self.index.get(key) {
            self.dag.remove_node(idx);
            self.index.remove(key);
            // petgraph may reuse node indices after removal, so rebuild the index
            // from the graph to stay consistent.
            self.rebuild_index();
        }
    }

    fn rebuild_index(&mut self) {
        self.index.clear();
        for idx in self.dag.node_indices() {
            if let Some(key) = self.dag.node_weight(idx) {
                self.index.insert(key.clone(), idx);
            }
        }
    }
}

impl DispatcherState {
    pub fn new(
        config: Config,
        client: async_nats::Client,
        js: jetstream::Context,
        kv: KvStores,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            nats: NatsClient::with_jetstream(client, js),
            kv,
            jobs: DashMap::new(),
            graph: RwLock::new(DepGraph::new()),
            token_tracker: RwLock::new(TokenTracker::new()),
        })
    }

    pub fn new_namespaced(
        config: Config,
        client: async_nats::Client,
        js: jetstream::Context,
        kv: KvStores,
        prefix: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            nats: NatsClient::with_prefix(client, js, prefix),
            kv,
            jobs: DashMap::new(),
            graph: RwLock::new(DepGraph::new()),
            token_tracker: RwLock::new(TokenTracker::new()),
        })
    }
}
