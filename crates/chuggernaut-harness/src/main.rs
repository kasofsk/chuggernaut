//! chuggernaut-harness — inline review loop driver (spec §4.5).
//!
//! Runs as the work container CMD when the job type declares `work.review`.
//! Alternates author and reviewer agent processes until the reviewer accepts or
//! the iteration budget runs out, then sends the single authoritative
//! `submit_result`. Provider-agnostic: it executes command strings composed by
//! the dispatcher-side provider and injected at /chuggernaut/harness.json.
//! Static binary (musl), injected at /usr/local/bin/chuggernaut-harness.

#![allow(dead_code)]

mod config;
mod steps;

fn main() -> anyhow::Result<()> {
    anyhow::bail!("not yet implemented: chuggernaut-harness inline review loop")
}
