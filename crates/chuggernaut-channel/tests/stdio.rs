//! End-to-end: the real chuggernaut-channel binary over stdio, submitting to
//! a dispatcher-side responder through real NATS (Docker). Skips without
//! Docker.

use std::process::Stdio;
use std::time::Duration;
use store::NatsStore;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn binary_speaks_mcp_and_submits_over_nats() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // Stand-in dispatcher: ack the work submit, and the channel post that
    // `update_status` now sends. The binary no longer writes `channels` KV
    // itself — the dispatcher owns that bucket — so what we assert here is the
    // wire message the binary emits.
    let responder = NatsStore::connect(server.url()).await.unwrap();
    let mut sub = responder
        .subscribe_requests("req.work.submit.acme.api.42")
        .await
        .unwrap();
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        if let Some(req) = sub.next().await {
            *seen2.lock().await = Some(String::from_utf8_lossy(&req.payload).to_string());
            req.respond(br#"{"ok":true}"#.to_vec()).await;
        }
    });

    let mut chan_sub = responder
        .subscribe_requests("req.channel.update.acme.api.42")
        .await
        .unwrap();
    let posted = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let posted2 = posted.clone();
    tokio::spawn(async move {
        if let Some(req) = chan_sub.next().await {
            *posted2.lock().await = Some(String::from_utf8_lossy(&req.payload).to_string());
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

    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
    )
    .await;
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

    // The dispatcher-side responder saw the work payload…
    assert!(
        seen.lock()
            .await
            .as_deref()
            .unwrap_or_default()
            .contains("did the thing")
    );

    // …and update_status arrived as a ChannelUpdate on req.channel.update,
    // rather than as a direct KV write.
    let post = posted.lock().await.clone().expect("channel update posted");
    let update: types::ChannelUpdate = serde_json::from_str(&post).expect("ChannelUpdate shape");
    assert_eq!(update.message, "working");
    assert_eq!(update.percent, Some(40));

    // Nothing wrote the channels bucket: only the dispatcher does, and this
    // test stands in for it without persisting.
    let entry: Option<types::ChannelEntry> = store
        .raw_bucket(store::buckets::CHANNELS)
        .await
        .unwrap()
        .get_json(&store::keys::channel_key("acme", "api", 42))
        .await
        .unwrap();
    assert!(
        entry.is_none(),
        "the binary must not write channels KV itself"
    );

    drop(stdin);
    let _ = child.wait().await;
}
