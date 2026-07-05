//! Docker fleet backend — the v1 production default (spec §3.1).
//!
//! One or more Docker daemons: local socket single-node, TCP+mTLS or SSH-tunnel
//! endpoints multi-node. Slot-capped least-loaded placement; `ContainerId`
//! encodes the owning node as `{node}/{docker_id}`. Files are injected via
//! put-archive after create, before start.
//!
//! TODO: implement with `bollard`.

/// One daemon in the fleet.
pub struct DockerNode {
    pub name: String,
    /// `unix:///var/run/docker.sock` or `tcp://host:2376` (mTLS).
    pub endpoint: String,
    /// Max concurrent containers on this node.
    pub slots: u32,
}

pub struct DockerBackend {
    pub nodes: Vec<DockerNode>,
}
