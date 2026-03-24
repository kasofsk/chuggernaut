use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ImageExt;
use testcontainers_modules::nats::{Nats, NatsServerCmd};
use tokio::sync::{mpsc, Mutex};

use chuggernaut_channel::{handle_message, handle_tool_call, mcp, nats_inbox_listener, ChannelState};
use chuggernaut_types::{ChannelMessage, ChannelStatus};

// ---------------------------------------------------------------------------
// Shared NATS container
// ---------------------------------------------------------------------------

static TEST_NATS_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

fn test_nats_port() -> u16 {
    *TEST_NATS_PORT.get_or_init(|| {
        std::thread::spawn(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let nats_cmd = NatsServerCmd::default().with_jetstream();
                let container = Nats::default()
                    .with_cmd(&nats_cmd)
                    .start()
                    .await
                    .unwrap();
                let port = container.get_host_port_ipv4(4222).await.unwrap();
                Box::leak(Box::new(container));
                port
            })
        })
        .join()
        .unwrap()
    })
}

async fn nats_client() -> async_nats::Client {
    let url = format!("nats://127.0.0.1:{}", test_nats_port());
    async_nats::connect(&url).await.unwrap()
}

async fn setup_js(nats: &async_nats::Client) -> async_nats::jetstream::Context {
    let js = async_nats::jetstream::new(nats.clone());
    // Ensure channels KV bucket exists (idempotent)
    js.create_key_value(async_nats::jetstream::kv::Config {
        bucket: chuggernaut_types::buckets::CHANNELS.to_string(),
        history: 1,
        storage: async_nats::jetstream::stream::StorageType::File,
        ..Default::default()
    })
    .await
    .unwrap();
    js
}

fn make_msg(sender: &str, body: &str) -> ChannelMessage {
    ChannelMessage {
        sender: sender.to_string(),
        body: body.to_string(),
        timestamp: chrono::Utc::now(),
        message_id: uuid::Uuid::new_v4().to_string(),
        in_reply_to: None,
    }
}

/// Unique job key per test to avoid cross-test interference.
fn unique_job_key() -> String {
    format!("test.channel.{}", &uuid::Uuid::new_v4().to_string()[..8])
}

// ---------------------------------------------------------------------------
// MCP protocol tests (initialize, tools/list, tools/call)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mcp_initialize_channel_mode() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let req = mcp::JsonRpcRequest {
        id: Some(serde_json::json!(1)),
        method: "initialize".to_string(),
        params: serde_json::json!({}),
    };

    let resp = handle_message(req, &state, &nats, &js, &job_key, true)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();

    assert_eq!(parsed["result"]["protocolVersion"], mcp::PROTOCOL_VERSION);
    assert!(parsed["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
    assert_eq!(parsed["result"]["serverInfo"]["name"], "chuggernaut-channel");
}

#[tokio::test]
async fn mcp_initialize_mcp_mode() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let req = mcp::JsonRpcRequest {
        id: Some(serde_json::json!(1)),
        method: "initialize".to_string(),
        params: serde_json::json!({}),
    };

    let resp = handle_message(req, &state, &nats, &js, &job_key, false)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();

    assert!(parsed["result"]["capabilities"].get("experimental").is_none());
    assert!(parsed["result"]["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn mcp_tools_list_channel_mode() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let req = mcp::JsonRpcRequest {
        id: Some(serde_json::json!(2)),
        method: "tools/list".to_string(),
        params: serde_json::json!({}),
    };

    let resp = handle_message(req, &state, &nats, &js, &job_key, true)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let tools = parsed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    assert!(names.contains(&"reply"));
    assert!(names.contains(&"update_status"));
    assert!(!names.contains(&"channel_check"));
}

#[tokio::test]
async fn mcp_tools_list_mcp_mode() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let req = mcp::JsonRpcRequest {
        id: Some(serde_json::json!(2)),
        method: "tools/list".to_string(),
        params: serde_json::json!({}),
    };

    let resp = handle_message(req, &state, &nats, &js, &job_key, false)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let tools = parsed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    assert!(names.contains(&"channel_check"));
    assert!(names.contains(&"channel_send"));
    assert!(names.contains(&"update_status"));
    assert!(!names.contains(&"reply"));
}

#[tokio::test]
async fn mcp_notifications_initialized_returns_none() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let req = mcp::JsonRpcRequest {
        id: None,
        method: "notifications/initialized".to_string(),
        params: serde_json::json!({}),
    };

    let resp = handle_message(req, &state, &nats, &js, &job_key, true).await;
    assert!(resp.is_none());
}

#[tokio::test]
async fn mcp_unknown_method_with_id_returns_error() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let req = mcp::JsonRpcRequest {
        id: Some(serde_json::json!(99)),
        method: "bogus/method".to_string(),
        params: serde_json::json!({}),
    };

    let resp = handle_message(req, &state, &nats, &js, &job_key, true)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["error"]["code"], -32601);
}

#[tokio::test]
async fn mcp_unknown_method_notification_returns_none() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let req = mcp::JsonRpcRequest {
        id: None,
        method: "bogus/notification".to_string(),
        params: serde_json::json!({}),
    };

    let resp = handle_message(req, &state, &nats, &js, &job_key, true).await;
    assert!(resp.is_none());
}

// ---------------------------------------------------------------------------
// Tool: channel_check (drains inbox)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channel_check_empty() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let result = handle_tool_call(
        "channel_check",
        &serde_json::json!({}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(text).unwrap();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn channel_check_drains_messages() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    // Push messages into state
    state.lock().await.push_message(make_msg("cli", "hello"));
    state.lock().await.push_message(make_msg("cli", "status?"));

    let result = handle_tool_call(
        "channel_check",
        &serde_json::json!({}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(text).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["body"], "hello");
    assert_eq!(messages[1]["body"], "status?");

    // Second check should be empty (drained)
    let result2 = handle_tool_call(
        "channel_check",
        &serde_json::json!({}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await
    .unwrap();
    let text2 = result2["content"][0]["text"].as_str().unwrap();
    let messages2: Vec<serde_json::Value> = serde_json::from_str(text2).unwrap();
    assert!(messages2.is_empty());
}

// ---------------------------------------------------------------------------
// Tool: channel_send / reply (publishes to NATS outbox)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channel_send_publishes_to_outbox() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    // Subscribe to outbox before sending
    let outbox_subject = chuggernaut_types::subjects::CHANNEL_OUTBOX.format(&job_key);
    let mut sub = nats.subscribe(outbox_subject).await.unwrap();

    let result = handle_tool_call(
        "channel_send",
        &serde_json::json!({"message": "I'm working on it"}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await
    .unwrap();

    assert_eq!(result["content"][0]["text"], "sent");

    // Verify the message arrived on the outbox subject
    let msg = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("timeout waiting for outbox message")
        .unwrap();
    let channel_msg: ChannelMessage = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(channel_msg.body, "I'm working on it");
    assert_eq!(channel_msg.sender, format!("claude:{job_key}"));
    assert!(channel_msg.in_reply_to.is_none());
}

#[tokio::test]
async fn reply_tool_publishes_with_in_reply_to() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let outbox_subject = chuggernaut_types::subjects::CHANNEL_OUTBOX.format(&job_key);
    let mut sub = nats.subscribe(outbox_subject).await.unwrap();

    let result = handle_tool_call(
        "reply",
        &serde_json::json!({"text": "all good", "message_id": "orig-123"}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await
    .unwrap();

    assert_eq!(result["content"][0]["text"], "sent");

    let msg = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("timeout")
        .unwrap();
    let channel_msg: ChannelMessage = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(channel_msg.body, "all good");
    assert_eq!(channel_msg.in_reply_to.as_deref(), Some("orig-123"));
}

// ---------------------------------------------------------------------------
// Tool: update_status (writes to NATS KV)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_status_writes_kv() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let result = handle_tool_call(
        "update_status",
        &serde_json::json!({"status": "running tests", "progress": 75}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await
    .unwrap();

    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("running tests"));

    // Verify KV entry
    let kv = js
        .get_key_value(chuggernaut_types::buckets::CHANNELS)
        .await
        .unwrap();
    let entry = kv.entry(&job_key).await.unwrap().unwrap();
    let status: ChannelStatus = serde_json::from_slice(&entry.value).unwrap();
    assert_eq!(status.status, "running tests");
    assert!((status.progress.unwrap() - 0.75).abs() < 0.001);
    assert_eq!(status.job_key, job_key);
}

#[tokio::test]
async fn update_status_without_progress() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let result = handle_tool_call(
        "update_status",
        &serde_json::json!({"status": "exploring codebase"}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await
    .unwrap();

    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("exploring codebase"));

    let kv = js
        .get_key_value(chuggernaut_types::buckets::CHANNELS)
        .await
        .unwrap();
    let entry = kv.entry(&job_key).await.unwrap().unwrap();
    let status: ChannelStatus = serde_json::from_slice(&entry.value).unwrap();
    assert!(status.progress.is_none());
}

// ---------------------------------------------------------------------------
// Tool: unknown tool returns error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_tool_returns_error() {
    let nats = nats_client().await;
    let js = setup_js(&nats).await;
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let job_key = unique_job_key();

    let result = handle_tool_call(
        "nonexistent_tool",
        &serde_json::json!({}),
        &state,
        &nats,
        &js,
        &job_key,
    )
    .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown tool"));
}

// ---------------------------------------------------------------------------
// NATS inbox listener: messages arrive in state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbox_listener_queues_messages() {
    let nats = nats_client().await;
    let job_key = unique_job_key();
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let (out_tx, _out_rx) = mpsc::channel::<String>(256);

    // Spawn listener
    tokio::spawn(nats_inbox_listener(
        nats.clone(),
        job_key.clone(),
        Arc::clone(&state),
        out_tx,
        false, // MCP mode — no push notifications
    ));

    // Give the listener time to subscribe
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Publish a message to the inbox subject
    let msg = make_msg("cli:david", "what are you doing?");
    let inbox_subject = chuggernaut_types::subjects::CHANNEL_INBOX.format(&job_key);
    let payload = serde_json::to_vec(&msg).unwrap();
    nats.publish(inbox_subject, payload.into()).await.unwrap();
    nats.flush().await.unwrap();

    // Wait for the message to arrive
    tokio::time::sleep(Duration::from_millis(200)).await;

    let messages = state.lock().await.drain_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sender, "cli:david");
    assert_eq!(messages[0].body, "what are you doing?");
}

// ---------------------------------------------------------------------------
// NATS inbox listener: channel mode emits push notification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbox_listener_channel_mode_emits_notification() {
    let nats = nats_client().await;
    let job_key = unique_job_key();
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);

    tokio::spawn(nats_inbox_listener(
        nats.clone(),
        job_key.clone(),
        Arc::clone(&state),
        out_tx,
        true, // Channel mode — should emit notifications
    ));

    tokio::time::sleep(Duration::from_millis(100)).await;

    let msg = make_msg("cli:david", "status report please");
    let inbox_subject = chuggernaut_types::subjects::CHANNEL_INBOX.format(&job_key);
    let payload = serde_json::to_vec(&msg).unwrap();
    nats.publish(inbox_subject, payload.into()).await.unwrap();
    nats.flush().await.unwrap();

    // Read the notification from the output channel
    let notification_json = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
        .await
        .expect("timeout waiting for notification")
        .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&notification_json).unwrap();
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["method"], "notifications/claude/channel");
    assert_eq!(parsed["params"]["content"], "status report please");
    assert_eq!(parsed["params"]["meta"]["sender"], "cli:david");

    // Message should also be queued in state
    let messages = state.lock().await.drain_messages();
    assert_eq!(messages.len(), 1);
}

// ---------------------------------------------------------------------------
// NATS inbox listener: invalid messages are skipped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbox_listener_skips_invalid_json() {
    let nats = nats_client().await;
    let job_key = unique_job_key();
    let state = Arc::new(Mutex::new(ChannelState::new()));
    let (out_tx, _out_rx) = mpsc::channel::<String>(256);

    tokio::spawn(nats_inbox_listener(
        nats.clone(),
        job_key.clone(),
        Arc::clone(&state),
        out_tx,
        false,
    ));

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Publish invalid JSON
    let inbox_subject = chuggernaut_types::subjects::CHANNEL_INBOX.format(&job_key);
    nats.publish(inbox_subject.clone(), "not json".into())
        .await
        .unwrap();

    // Then publish a valid message
    let msg = make_msg("cli", "valid");
    let payload = serde_json::to_vec(&msg).unwrap();
    nats.publish(inbox_subject, payload.into()).await.unwrap();
    nats.flush().await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Only the valid message should be queued
    let messages = state.lock().await.drain_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "valid");
}
