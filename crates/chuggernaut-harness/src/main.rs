//! chuggernaut-harness — inline review loop driver (spec §4.5).
//!
//! Runs as the work container CMD when the job type declares `work.review`.
//! Alternates author and reviewer agent processes until the reviewer accepts or
//! the iteration budget runs out, then sends the single authoritative
//! `submit_result`. Provider-agnostic: it executes command strings composed by
//! the dispatcher-side provider and injected at /chuggernaut/harness.json.
//! Static binary (musl), injected at /usr/local/bin/chuggernaut-harness.

// Scaffold: config/steps are consumed once the loop lands.
#![allow(dead_code)]

mod config;
mod steps;

fn main() -> anyhow::Result<()> {
    // TODO (spec §4.5):
    // 1. Load HarnessConfig from /chuggernaut/harness.json
    // 2. Loop, up to config.iterations rounds:
    //    a. Run author (iteration 1: author_cmd; >1: author_continue_cmd with
    //       findings block as the message). Non-zero exit → exit non-zero
    //       (work_retries path).
    //    b. Run reviewer_cmd (fresh session); read verdict from
    //       /chuggernaut/review-result.json (written by submit_review). Missing
    //       verdict → retry the review once, then record a failed step and
    //       proceed to submit.
    //    c. Report step-started/step-completed around each run via
    //       req.step.report (bounded retry, non-fatal on failure).
    //    d. pass=true → break; pass=false → findings feed the next round.
    // 3. Send req.work.submit with the author's latest intercepted
    //    /chuggernaut/work-result.json payload, merging
    //    structured.inline_review = { iterations, accepted, unresolved_findings }.
    //    Bounded retry until ack, then exit 0.
    anyhow::bail!("not yet implemented: chuggernaut-harness inline review loop")
}
