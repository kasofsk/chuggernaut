use std::sync::Arc;

use async_nats::jetstream;
use dashmap::DashMap;
use petgraph::graph::{DiGraph, NodeIndex};
use tokio::sync::RwLock;

use chuggernaut_types::Job;

use chuggernaut_nats::NatsClient;

use crate::config::Config;
use crate::nats_init::KvStores;

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
}

pub struct DepGraph {
    pub dag: DiGraph<String, ()>,
    pub index: std::collections::HashMap<String, NodeIndex>,
}

impl DepGraph {
    pub fn new() -> Self {
        Self {
            dag: DiGraph::new(),
            index: std::collections::HashMap::new(),
        }
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
        })
    }
}
