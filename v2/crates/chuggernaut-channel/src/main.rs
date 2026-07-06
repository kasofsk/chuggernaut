//! chuggernaut-channel — MCP server bridging agent processes to NATS (spec §4.2).
//!
//! Tools: update_status, channel_check, reply, and submit_result (work role)
//! or submit_eval (eval role). Built as a static binary (musl) and injected
//! into every agent container at /usr/local/bin/chuggernaut-channel; connects
//! to NATS via NATS_URL/NATS_CREDS. Transport: newline-delimited JSON-RPC 2.0
//! over stdio.

mod server;

use server::{JobContext, Server};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let ctx = JobContext::from_env().map_err(|e| anyhow::anyhow!(e))?;
    let mut server = Server::new(ctx);

    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("chuggernaut-channel: unparseable message: {e}");
                continue;
            }
        };
        if let Some(response) = server.handle(&msg).await {
            stdout.write_all(serde_json::to_string(&response)?.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
