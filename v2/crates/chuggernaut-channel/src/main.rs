//! chuggernaut-channel — MCP server bridging agent processes to NATS (spec §4.2).
//!
//! Tools: update_status, channel_check, reply, submit_result, submit_eval, and
//! create_job (factory triage jobs only, spec §13.4). Built as a static binary
//! (musl) and volume-mounted read-only into every agent container at
//! /usr/local/bin/chuggernaut-channel. Connects to NATS via NATS_URL/NATS_TOKEN.

fn main() -> anyhow::Result<()> {
    anyhow::bail!("not yet implemented: chuggernaut-channel MCP server")
}
