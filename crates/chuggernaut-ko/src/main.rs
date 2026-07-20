//! chuggernaut-ko — read-only knowledge MCP server (spec §4.2, Part 9).
//!
//! Scoped KO queries against global/team/project scopes using the job's scoped
//! NATS JWT. Built as a static binary (musl) and volume-mounted read-only into
//! every agent container at /usr/local/bin/chuggernaut-ko.

fn main() -> anyhow::Result<()> {
    anyhow::bail!("not yet implemented: chuggernaut-ko MCP server")
}
