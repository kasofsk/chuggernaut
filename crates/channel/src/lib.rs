pub mod mcp;

use std::collections::VecDeque;
use std::sync::Arc;

use chrono::Utc;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use chuggernaut_types::{ChannelMessage, ChannelStatus};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

pub struct ChannelState {
    inbox: VecDeque<ChannelMessage>,
}

impl ChannelState {
    pub fn new() -> Self {
        Self {
            inbox: VecDeque::with_capacity(100),
        }
    }

    pub fn push_message(&mut self, msg: ChannelMessage) {
        if self.inbox.len() >= 100 {
            self.inbox.pop_front();
        }
        self.inbox.push_back(msg);
    }

    pub fn drain_messages(&mut self) -> Vec<ChannelMessage> {
        self.inbox.drain(..).collect()
    }

    pub fn len(&self) -> usize {
        self.inbox.len()
    }
}

// ---------------------------------------------------------------------------
// NATS → inbox subscription
// ---------------------------------------------------------------------------

pub async fn nats_inbox_listener(
    nats: async_nats::Client,
    job_key: String,
    state: Arc<Mutex<ChannelState>>,
    out_tx: mpsc::Sender<String>,
    channel_mode: bool,
) {
    let subject = chuggernaut_types::subjects::CHANNEL_INBOX.format(&job_key);
    info!(subject, "subscribing to channel inbox");

    let mut sub = match nats.subscribe(subject.clone()).await {
        Ok(s) => s,
        Err(e) => {
            error!("failed to subscribe to {subject}: {e}");
            return;
        }
    };

    while let Some(msg) = sub.next().await {
        let channel_msg: ChannelMessage = match serde_json::from_slice(&msg.payload) {
            Ok(m) => m,
            Err(e) => {
                warn!("invalid channel message: {e}");
                continue;
            }
        };

        info!(sender = channel_msg.sender, body = channel_msg.body, "received channel message from NATS inbox");

        if channel_mode {
            info!(sender = channel_msg.sender, message_id = channel_msg.message_id, "pushing channel notification to MCP client");
            let notification = mcp::JsonRpcNotification {
                jsonrpc: "2.0",
                method: mcp::NOTIFY_CHANNEL,
                params: serde_json::json!({
                    "content": channel_msg.body,
                    "meta": {
                        "sender": channel_msg.sender,
                        "message_id": channel_msg.message_id,
                    }
                }),
            };
            if let Ok(json) = serde_json::to_string(&notification) {
                let _ = out_tx.send(json).await;
            }
        }

        state.lock().await.push_message(channel_msg);
    }
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

pub async fn handle_tool_call(
    name: &str,
    arguments: &Value,
    state: &Arc<Mutex<ChannelState>>,
    nats: &async_nats::Client,
    js: &async_nats::jetstream::Context,
    job_key: &str,
) -> Result<Value, String> {
    match name {
        "channel_check" => {
            let messages = state.lock().await.drain_messages();
            info!(job_key, count = messages.len(), "channel_check: draining inbox messages");
            let texts: Vec<Value> = messages
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "sender": m.sender,
                        "body": m.body,
                        "message_id": m.message_id,
                        "timestamp": m.timestamp.to_rfc3339(),
                    })
                })
                .collect();
            Ok(tool_result(
                &serde_json::to_string_pretty(&texts).unwrap_or_else(|_| "[]".into()),
            ))
        }

        "channel_send" | "reply" => {
            let text = arguments
                .get("message")
                .or_else(|| arguments.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let in_reply_to = arguments
                .get("message_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            info!(job_key, tool = name, body = text, in_reply_to = ?in_reply_to, "channel_send/reply: publishing message to NATS outbox");

            let msg = ChannelMessage {
                sender: format!("claude:{job_key}"),
                body: text.to_string(),
                timestamp: Utc::now(),
                message_id: uuid::Uuid::new_v4().to_string(),
                in_reply_to,
            };

            let subject = chuggernaut_types::subjects::CHANNEL_OUTBOX.format(job_key);
            let payload = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
            nats.publish(subject, payload.into())
                .await
                .map_err(|e| format!("NATS publish failed: {e}"))?;

            Ok(tool_result("sent"))
        }

        "update_status" => {
            let status_text = arguments
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let progress = arguments
                .get("progress")
                .and_then(|v| v.as_f64())
                .map(|p| (p / 100.0) as f32);

            info!(job_key, status = status_text, progress = ?progress, "update_status: writing to KV");

            let status = ChannelStatus {
                job_key: job_key.to_string(),
                status: status_text.to_string(),
                progress,
                updated_at: Utc::now(),
            };

            let kv = js
                .get_key_value(chuggernaut_types::buckets::CHANNELS)
                .await
                .map_err(|e| format!("KV lookup failed: {e}"))?;

            let payload = serde_json::to_vec(&status).map_err(|e| e.to_string())?;
            kv.put(job_key, payload.into())
                .await
                .map_err(|e| format!("KV put failed: {e}"))?;

            Ok(tool_result(&format!("status updated: {status_text}")))
        }

        _ => Err(format!("unknown tool: {name}")),
    }
}

pub fn tool_result(text: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    })
}

// ---------------------------------------------------------------------------
// MCP message dispatch
// ---------------------------------------------------------------------------

pub async fn handle_message(
    req: mcp::JsonRpcRequest,
    state: &Arc<Mutex<ChannelState>>,
    nats: &async_nats::Client,
    js: &async_nats::jetstream::Context,
    job_key: &str,
    channel_mode: bool,
) -> Option<String> {
    match req.method.as_str() {
        mcp::METHOD_INITIALIZE => {
            let result = serde_json::json!({
                "protocolVersion": mcp::PROTOCOL_VERSION,
                "capabilities": mcp::server_capabilities(channel_mode),
                "serverInfo": mcp::server_info(),
                "instructions": mcp::server_instructions(channel_mode),
            });
            let resp = mcp::JsonRpcResponse::success(req.id.unwrap_or(Value::Null), result);
            Some(serde_json::to_string(&resp).unwrap())
        }

        mcp::METHOD_NOTIFICATIONS_INITIALIZED => None,

        mcp::METHOD_TOOLS_LIST => {
            let result = serde_json::json!({
                "tools": mcp::tool_definitions(channel_mode)
            });
            let resp = mcp::JsonRpcResponse::success(req.id.unwrap_or(Value::Null), result);
            Some(serde_json::to_string(&resp).unwrap())
        }

        mcp::METHOD_TOOLS_CALL => {
            let tool_name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));

            info!(tool = tool_name, job_key, "MCP tools/call invoked");
            let result = handle_tool_call(tool_name, &arguments, state, nats, js, job_key).await;

            let resp = match &result {
                Ok(_) => {
                    info!(tool = tool_name, job_key, "tool call succeeded");
                    mcp::JsonRpcResponse::success(req.id.unwrap_or(Value::Null), result.unwrap())
                }
                Err(msg) => {
                    warn!(tool = tool_name, job_key, error = msg.as_str(), "tool call failed");
                    mcp::JsonRpcResponse::error(req.id, -32000, msg.clone())
                }
            };
            Some(serde_json::to_string(&resp).unwrap())
        }

        other => {
            debug!(method = other, "unhandled MCP method");
            if req.id.is_some() {
                let resp = mcp::JsonRpcResponse::error(
                    req.id,
                    -32601,
                    format!("method not found: {other}"),
                );
                Some(serde_json::to_string(&resp).unwrap())
            } else {
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chuggernaut_types::ChannelMessage;

    fn make_msg(sender: &str, body: &str) -> ChannelMessage {
        ChannelMessage {
            sender: sender.to_string(),
            body: body.to_string(),
            timestamp: Utc::now(),
            message_id: uuid::Uuid::new_v4().to_string(),
            in_reply_to: None,
        }
    }

    #[test]
    fn state_push_and_drain() {
        let mut state = ChannelState::new();
        assert_eq!(state.len(), 0);

        state.push_message(make_msg("cli", "hello"));
        state.push_message(make_msg("cli", "world"));
        assert_eq!(state.len(), 2);

        let msgs = state.drain_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body, "hello");
        assert_eq!(msgs[1].body, "world");
        assert_eq!(state.len(), 0);
    }

    #[test]
    fn state_drain_empty() {
        let mut state = ChannelState::new();
        let msgs = state.drain_messages();
        assert!(msgs.is_empty());
    }

    #[test]
    fn state_bounded_buffer_drops_oldest() {
        let mut state = ChannelState::new();
        for i in 0..105 {
            state.push_message(make_msg("cli", &format!("msg-{i}")));
        }
        assert_eq!(state.len(), 100);

        let msgs = state.drain_messages();
        // Oldest 5 were dropped; first message should be msg-5
        assert_eq!(msgs[0].body, "msg-5");
        assert_eq!(msgs[99].body, "msg-104");
    }

    #[test]
    fn tool_definitions_channel_mode() {
        let tools = mcp::tool_definitions(true);
        let tools = tools.as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"reply"));
        assert!(names.contains(&"update_status"));
        assert!(!names.contains(&"channel_check"));
        assert!(!names.contains(&"channel_send"));
    }

    #[test]
    fn tool_definitions_mcp_mode() {
        let tools = mcp::tool_definitions(false);
        let tools = tools.as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"channel_check"));
        assert!(names.contains(&"channel_send"));
        assert!(names.contains(&"update_status"));
        assert!(!names.contains(&"reply"));
    }

    #[test]
    fn capabilities_channel_mode() {
        let caps = mcp::server_capabilities(true);
        assert!(caps.get("experimental").is_some());
        assert!(caps["experimental"].get("claude/channel").is_some());
    }

    #[test]
    fn capabilities_mcp_mode() {
        let caps = mcp::server_capabilities(false);
        assert!(caps.get("experimental").is_none());
        assert!(caps.get("tools").is_some());
    }

    #[test]
    fn jsonrpc_response_success() {
        let resp = mcp::JsonRpcResponse::success(
            Value::Number(1.into()),
            serde_json::json!({"ok": true}),
        );
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["result"]["ok"], true);
        assert!(parsed.get("error").is_none());
    }

    #[test]
    fn jsonrpc_response_error() {
        let resp = mcp::JsonRpcResponse::error(Some(Value::Number(2.into())), -32601, "not found".into());
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["error"]["code"], -32601);
        assert_eq!(parsed["error"]["message"], "not found");
        assert!(parsed.get("result").is_none());
    }

    #[test]
    fn tool_result_format() {
        let r = tool_result("hello");
        let content = r["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello");
    }

    #[test]
    fn channel_message_serde_roundtrip() {
        let msg = make_msg("cli:david", "what's your status?");
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ChannelMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sender, "cli:david");
        assert_eq!(parsed.body, "what's your status?");
        assert_eq!(parsed.message_id, msg.message_id);
    }

    #[test]
    fn channel_status_serde_roundtrip() {
        let status = ChannelStatus {
            job_key: "acme.payments.57".to_string(),
            status: "implementing feature".to_string(),
            progress: Some(0.45),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: ChannelStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.job_key, "acme.payments.57");
        assert_eq!(parsed.status, "implementing feature");
        assert!((parsed.progress.unwrap() - 0.45).abs() < 0.001);
    }
}
