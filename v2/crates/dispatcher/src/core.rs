//! Single-writer event loop: owns job records, task log tail, DAG, and the work
//! queue inside one tokio task; everything else sends messages (spec §3.1).
