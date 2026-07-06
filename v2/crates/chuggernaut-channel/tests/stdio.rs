//! End-to-end: the real chuggernaut-channel binary over stdio, submitting to
//! a dispatcher-side responder through real NATS (Docker). Skips without
//! Docker.

use std::process::Stdio;
use std::time::Duration;
use store::NatsStore;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn binary_speaks_mcp_and_submits_over_nats() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else { return };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // Stand-in dispatcher: ack one submit on the work subject.
    let responder = NatsStore::connect(server.url()).await.unwrap();
    let mut sub = responder.subscribe_requests("req.work.submit.acme.api.42").await.unwrap();
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        if let Some(req) = sub.next().await {
            *seen2.lock().await = Some(String::from_utf8_lossy(&req.payload).to_string());
            req.respond(br#"{"ok":true}"#.to_vec()).await;
        }
    });

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_chuggernaut-channel"))
        .env("JOB_ID", "42")
        .env("JOB_PROJECT", "acme/api")
        .env("JOB_BRANCH", "job/42")
        .env("CHANNEL_ROLE", "work")
        .env("NATS_URL", server.url())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn channel binary");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();

    async fn send(stdin: &mut tokio::process::ChildStdin, line: &str) {
        stdin.write_all(line.as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
        stdin.flush().await.unwrap();
    }

    send(&mut stdin, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).await;
    let init: serde_json::Value =
        serde_json::from_str(&stdout.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "chuggernaut-channel");

    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"update_status","arguments":{"message":"working","percent":40}}}"#,
    )
    .await;
    let status: serde_json::Value =
        serde_json::from_str(&stdout.next_line().await.unwrap().unwrap()).unwrap();
    assert_ne!(status["result"]["isError"], true, "{status}");

    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"submit_result","arguments":{"summary":"did the thing"}}}"#,
    )
    .await;
    let submit: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
            .await
            .expect("submit response within 10s")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_ne!(submit["result"]["isError"], true, "{submit}");

    // The dispatcher-side responder saw the payload…
    assert!(seen.lock().await.as_deref().unwrap_or_default().contains("did the thing"));
    // …and update_status wrote the channels KV entry (spec §4.2).
    let entry: types::ChannelEntry = store
        .raw_bucket(store::buckets::CHANNELS)
        .await
        .unwrap()
        .get_json(&store::keys::channel_key("acme", "api", 42))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.update.unwrap().message, "working");

    drop(stdin);
    let _ = child.wait().await;
}
