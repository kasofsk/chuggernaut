use std::sync::Arc;

use tracing::{debug, warn};

use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs::{kv_cas_rmw, kv_get, kv_put};
use crate::state::DispatcherState;

/// Create dependency records for a new job.
/// Writes forward deps for the job and updates reverse deps on each dependency.
pub async fn create_deps(
    state: &Arc<DispatcherState>,
    job_key: &str,
    dep_keys: &[String],
) -> DispatcherResult<()> {
    // Write forward deps for the new job
    let forward = DepRecord {
        depends_on: dep_keys.to_vec(),
        depended_on_by: Vec::new(),
    };
    crate::jobs::kv_cas_create(&state.kv.deps, job_key, &forward).await?;

    // Update reverse deps on each dependency
    for dep_key in dep_keys {
        add_reverse_dep(state, dep_key, job_key).await?;
    }

    // Update petgraph
    {
        let mut graph = state.graph.write().await;
        let job_node = graph.ensure_node(job_key);
        for dep_key in dep_keys {
            let dep_node = graph.ensure_node(dep_key);
            // Check for cycles before adding
            if has_path(&graph, job_node, dep_node) {
                // This shouldn't happen since we checked earlier, but be safe
                return Err(DispatcherError::CycleDetected {
                    from: job_key.to_string(),
                    to: dep_key.clone(),
                });
            }
            // Edge: job_key -> dep_key means "job_key depends on dep_key"
            graph.dag.add_edge(job_node, dep_node, ());
        }
    }

    debug!(job_key, deps = ?dep_keys, "dependencies created");
    Ok(())
}

/// Add a reverse dep: dep_key.depended_on_by += [job_key]
async fn add_reverse_dep(
    state: &Arc<DispatcherState>,
    dep_key: &str,
    job_key: &str,
) -> DispatcherResult<()> {
    let jk = job_key.to_string();
    match kv_cas_rmw::<DepRecord, _>(
        &state.kv.deps,
        dep_key,
        state.config.cas_max_retries,
        |record| {
            if !record.depended_on_by.contains(&jk) {
                record.depended_on_by.push(jk.clone());
            }
            Ok(())
        },
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(DispatcherError::Kv(msg)) if msg.contains("not found") => {
            // Dep has no dep record yet — create one with just the reverse ref
            let record = DepRecord {
                depends_on: Vec::new(),
                depended_on_by: vec![job_key.to_string()],
            };
            // Use put instead of create in case there's a race
            kv_put(&state.kv.deps, dep_key, &record).await?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Check if there's a path from `from` to `to` in the DAG using DFS.
fn has_path(
    graph: &crate::state::DepGraph,
    from: petgraph::graph::NodeIndex,
    to: petgraph::graph::NodeIndex,
) -> bool {
    use petgraph::visit::Dfs;
    let mut dfs = Dfs::new(&graph.dag, from);
    while let Some(node) = dfs.next(&graph.dag) {
        if node == to {
            return true;
        }
    }
    false
}

/// Check if adding edge from -> to would create a cycle.
/// (Check if `to` can reach `from` in the current graph.)
pub async fn would_create_cycle(state: &Arc<DispatcherState>, from: &str, to: &str) -> bool {
    let graph = state.graph.read().await;
    let from_node = match graph.index.get(from) {
        Some(&idx) => idx,
        None => return false, // new node can't create cycle
    };
    let to_node = match graph.index.get(to) {
        Some(&idx) => idx,
        None => return false,
    };
    // Would create cycle if `to` can reach `from`
    has_path(&graph, to_node, from_node)
}

/// Walk reverse deps when a job transitions to Done.
/// For each dependent, check if all its deps are Done → transition to OnDeck.
pub async fn propagate_unblock(
    state: &Arc<DispatcherState>,
    done_job_key: &str,
) -> DispatcherResult<Vec<String>> {
    let mut unblocked = Vec::new();

    // Get dependents from the dep record
    let dependents = {
        let graph = state.graph.read().await;
        if let Some(&node) = graph.index.get(done_job_key) {
            // Find all nodes that have an edge TO this node (i.e., depend on it)
            graph
                .dag
                .neighbors_directed(node, petgraph::Direction::Incoming)
                .filter_map(|n| graph.dag.node_weight(n).cloned())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };

    for dependent_key in &dependents {
        // Check if ALL deps of this dependent are Done
        let all_done = match kv_get::<DepRecord>(&state.kv.deps, dependent_key).await? {
            Some((record, _)) => {
                let mut all = true;
                for dep_key in &record.depends_on {
                    if let Some(job) = state.jobs.get(dep_key) {
                        if job.state != JobState::Done {
                            all = false;
                            break;
                        }
                    } else {
                        all = false;
                        break;
                    }
                }
                all
            }
            None => true, // no dep record means no deps
        };

        if all_done {
            // Check that the dependent is actually in Blocked state.
            // Important: drop the Ref before calling transition_job,
            // which needs a write lock on the same DashMap shard.
            let is_blocked = state
                .jobs
                .get(dependent_key)
                .map(|job| job.state == JobState::Blocked)
                .unwrap_or(false);

            if is_blocked {
                match crate::jobs::transition_job(
                    state,
                    dependent_key,
                    JobState::OnDeck,
                    "deps_resolved",
                    None,
                )
                .await
                {
                    Ok(_) => {
                        debug!(dependent_key, "unblocked by {done_job_key}");
                        unblocked.push(dependent_key.clone());
                    }
                    Err(e) => {
                        warn!(dependent_key, "failed to unblock: {e}");
                    }
                }
            }
        }
    }

    Ok(unblocked)
}

/// Rebuild the petgraph DAG from chuggernaut.deps KV.
pub async fn rebuild_graph(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    use futures::StreamExt;

    let mut graph = state.graph.write().await;
    *graph = crate::state::DepGraph::new();

    let keys = state.kv.deps.keys().await?;
    tokio::pin!(keys);

    while let Some(key) = keys.next().await {
        match key {
            Ok(key_str) => {
                if let Some((record, _)) = kv_get::<DepRecord>(&state.kv.deps, &key_str).await? {
                    let node = graph.ensure_node(&key_str);
                    for dep_key in &record.depends_on {
                        let dep_node = graph.ensure_node(dep_key);
                        graph.dag.add_edge(node, dep_node, ());
                    }
                }
            }
            Err(e) => {
                warn!("error reading dep key: {e}");
            }
        }
    }

    debug!(
        nodes = graph.dag.node_count(),
        edges = graph.dag.edge_count(),
        "dependency graph rebuilt"
    );
    Ok(())
}

/// Repair reverse dep indexes — for every forward dep A → B, ensure B.depended_on_by contains A.
pub async fn repair_reverse_indexes(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    use futures::StreamExt;

    let keys = state.kv.deps.keys().await?;
    tokio::pin!(keys);

    let mut repairs = 0u32;

    while let Some(key) = keys.next().await {
        match key {
            Ok(key_str) => {
                if let Some((record, _)) = kv_get::<DepRecord>(&state.kv.deps, &key_str).await? {
                    for dep_key in &record.depends_on {
                        // Ensure dep_key.depended_on_by contains key_str
                        let ks = key_str.clone();
                        match kv_cas_rmw::<DepRecord, _>(
                            &state.kv.deps,
                            dep_key,
                            state.config.cas_max_retries,
                            |dep_record| {
                                if !dep_record.depended_on_by.contains(&ks) {
                                    dep_record.depended_on_by.push(ks.clone());
                                }
                                Ok(())
                            },
                        )
                        .await
                        {
                            Ok(_) => {
                                repairs += 1;
                            }
                            Err(DispatcherError::Kv(msg)) if msg.contains("not found") => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
            Err(e) => {
                warn!("error reading dep key during repair: {e}");
            }
        }
    }

    if repairs > 0 {
        warn!(repairs, "repaired reverse dep indexes");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DepGraph;

    #[test]
    fn has_path_direct_edge() {
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        let b = g.ensure_node("b");
        g.dag.add_edge(a, b, ());
        assert!(has_path(&g, a, b));
        assert!(!has_path(&g, b, a));
    }

    #[test]
    fn has_path_transitive() {
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        let b = g.ensure_node("b");
        let c = g.ensure_node("c");
        g.dag.add_edge(a, b, ());
        g.dag.add_edge(b, c, ());
        assert!(has_path(&g, a, c));
        assert!(!has_path(&g, c, a));
    }

    #[test]
    fn has_path_no_connection() {
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        let b = g.ensure_node("b");
        let c = g.ensure_node("c");
        g.dag.add_edge(a, b, ());
        assert!(!has_path(&g, a, c));
        assert!(!has_path(&g, c, a));
    }

    #[test]
    fn has_path_diamond() {
        // a -> b, a -> c, b -> d, c -> d
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        let b = g.ensure_node("b");
        let c = g.ensure_node("c");
        let d = g.ensure_node("d");
        g.dag.add_edge(a, b, ());
        g.dag.add_edge(a, c, ());
        g.dag.add_edge(b, d, ());
        g.dag.add_edge(c, d, ());
        assert!(has_path(&g, a, d));
        assert!(!has_path(&g, d, a));
    }

    #[test]
    fn cycle_detection_simple() {
        // a -> b exists. Adding b -> a would create a cycle.
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        let b = g.ensure_node("b");
        g.dag.add_edge(a, b, ());
        // To check if b -> a would create cycle: does a reach b? yes.
        assert!(has_path(&g, a, b));
    }

    #[test]
    fn cycle_detection_transitive() {
        // a -> b -> c. Adding c -> a would create a cycle.
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        let b = g.ensure_node("b");
        let c = g.ensure_node("c");
        g.dag.add_edge(a, b, ());
        g.dag.add_edge(b, c, ());
        // would_create_cycle(c, a) checks: has_path(a, c)? yes.
        assert!(has_path(&g, a, c));
    }

    #[test]
    fn cycle_detection_self_loop() {
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        // self-loop: has_path(a, a)?
        assert!(has_path(&g, a, a));
    }

    #[test]
    fn no_cycle_parallel_chains() {
        // a -> b, c -> d. Adding a -> d is safe.
        let mut g = DepGraph::new();
        let a = g.ensure_node("a");
        let b = g.ensure_node("b");
        let c = g.ensure_node("c");
        let d = g.ensure_node("d");
        g.dag.add_edge(a, b, ());
        g.dag.add_edge(c, d, ());
        // would_create_cycle(a, d) checks: has_path(d, a)? no.
        assert!(!has_path(&g, d, a));
    }

    #[test]
    fn ensure_node_idempotent() {
        let mut g = DepGraph::new();
        let idx1 = g.ensure_node("a");
        let idx2 = g.ensure_node("a");
        assert_eq!(idx1, idx2);
        assert_eq!(g.dag.node_count(), 1);
    }

    #[test]
    fn remove_node_cleans_up() {
        let mut g = DepGraph::new();
        g.ensure_node("a");
        let b = g.ensure_node("b");
        g.ensure_node("c");
        g.dag.add_edge(g.index["a"], b, ());
        assert_eq!(g.dag.node_count(), 3);

        g.remove_node("b");
        assert_eq!(g.dag.node_count(), 2);
        assert!(!g.index.contains_key("b"));
        assert!(g.index.contains_key("a"));
        assert!(g.index.contains_key("c"));
    }
}
