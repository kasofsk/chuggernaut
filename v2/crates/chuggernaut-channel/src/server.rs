//! Minimal MCP stdio server (newline-delimited JSON-RPC 2.0) exposing the
//! §4.2 channel tools. No MCP framework — the protocol surface we need is
//! initialize / tools/list / tools/call, and a static musl binary wants a
//! short dependency list.

use serde_json::{Value, json};
use std::time::Duration;
use store::{NatsStore, buckets, keys, subjects};
use types::{AgentReply, ChannelEntry, ChannelUpdate};

/// Container identity + role, read from the injected env (spec §4.1).
pub struct JobContext {
    pub owner: String,
    pub project: String,
    pub seq: u64,
    /// `work` or `eval` — selects the tool set (§4.2 inventory).
    pub role: String,
    /// Present in eval containers only; addresses `req.eval.submit`.
    pub task_id: Option<u64>,
    pub nats_url: String,
    pub nats_token: String,
}

impl JobContext {
    pub fn from_env() -> Result<Self, String> {
        let var = |k: &str| std::env::var(k).map_err(|_| format!("missing env {k}"));
        let project_slug = var("JOB_PROJECT")?;
        let (owner, project) = project_slug
            .split_once('/')
            .ok_or_else(|| format!("JOB_PROJECT {project_slug:?} is not owner/repo"))?;
        Ok(Self {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: var("JOB_ID")?.parse().map_err(|_| "JOB_ID not a number")?,
            role: std::env::var("CHANNEL_ROLE").unwrap_or_else(|_| "work".into()),
            task_id: std::env::var("JOB_TASK_ID").ok().and_then(|t| t.parse().ok()),
            nats_url: var("NATS_URL")?,
            nats_token: std::env::var("NATS_TOKEN").unwrap_or_default(),
        })
    }
}

pub struct Server {
    ctx: JobContext,
    /// Connected lazily: protocol handshake must work before NATS is up.
    store: Option<NatsStore>,
}

impl Server {
    pub fn new(ctx: JobContext) -> Self {
        Self { ctx, store: None }
    }

    async fn store(&mut self) -> Result<&NatsStore, String> {
        if self.store.is_none() {
            let store = if self.ctx.nats_token.is_empty() {
                NatsStore::connect(&self.ctx.nats_url).await
            } else {
                NatsStore::connect_with_token(&self.ctx.nats_url, &self.ctx.nats_token).await
            }
            .map_err(|e| e.to_string())?;
            self.store = Some(store);
        }
        Ok(self.store.as_ref().unwrap())
    }

    /// Handle one JSON-RPC message; None for notifications (no response).
    pub async fn handle(&mut self, msg: &Value) -> Option<Value> {
        let method = msg.get("method")?.as_str()?;
        let id = msg.get("id").cloned();
        // Notifications (no id) get no response.
        id.as_ref()?;

        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "chuggernaut-channel", "version": env!("CARGO_PKG_VERSION") },
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tool_definitions() })),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.call_tool(name, args).await {
                    Ok(text) => Ok(json!({
                        "content": [{ "type": "text", "text": text }],
                    })),
                    Err(e) => Ok(json!({
                        "content": [{ "type": "text", "text": e }],
                        "isError": true,
                    })),
                }
            }
            other => Err(json!({ "code": -32601, "message": format!("unknown method {other}") })),
        };

        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        })
    }

    fn tool_definitions(&self) -> Vec<Value> {
        let mut tools = vec![
            tool("update_status", "Report progress; overwrites the previous update.",
                json!({ "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "percent": { "type": "integer", "minimum": 0, "maximum": 100 } },
                    "required": ["message"] })),
            tool("channel_check", "Poll the operator inbox; pass the last consumed sequence.",
                json!({ "type": "object",
                    "properties": { "since": { "type": "integer" } } })),
            tool("reply", "Reply to the operator; overwrites the previous reply.",
                json!({ "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"] })),
        ];
        match self.ctx.role.as_str() {
            "eval" => tools.push(tool(
                "submit_eval",
                "Publish the authoritative eval verdict. Required before exit.",
                json!({ "type": "object",
                    "properties": {
                        "pass": { "type": "boolean" },
                        "structured": {},
                        "token_usage": {} },
                    "required": ["pass"] }),
            )),
            _ => tools.push(tool(
                "submit_result",
                "Submit the work summary and structured context.",
                json!({ "type": "object",
                    "properties": {
                        "summary": { "type": "string" },
                        "structured": {},
                        "token_usage": {} } }),
            )),
        }
        tools
    }

    async fn call_tool(&mut self, name: &str, args: Value) -> Result<String, String> {
        let (owner, project, seq) =
            (self.ctx.owner.clone(), self.ctx.project.clone(), self.ctx.seq);
        match (name, self.ctx.role.as_str()) {
            ("update_status", _) => {
                let update = ChannelUpdate {
                    message: args
                        .get("message")
                        .and_then(|m| m.as_str())
                        .ok_or("message is required")?
                        .to_string(),
                    percent: args.get("percent").and_then(|p| p.as_u64()).map(|p| p as u8),
                };
                self.update_channel(|entry| entry.update = Some(update)).await?;
                Ok("status updated".into())
            }
            ("reply", _) => {
                let reply = AgentReply {
                    text: args
                        .get("text")
                        .and_then(|t| t.as_str())
                        .ok_or("text is required")?
                        .to_string(),
                    sent_at: chrono::Utc::now(),
                };
                self.update_channel(|entry| entry.last_reply = Some(reply)).await?;
                Ok("reply sent".into())
            }
            ("channel_check", _) => {
                let since = args.get("since").and_then(|s| s.as_u64()).unwrap_or(0);
                let subject = subjects::channel_inbox(&owner, &project, seq);
                let msgs = self
                    .store()
                    .await?
                    .read_subject_after(buckets::STREAM_CHANNEL_INBOX, &subject, since, 64)
                    .await
                    .map_err(|e| e.to_string())?;
                let items: Vec<Value> = msgs
                    .iter()
                    .map(|(stream_seq, payload)| {
                        json!({
                            "seq": stream_seq,
                            "message": serde_json::from_slice::<Value>(payload)
                                .unwrap_or(Value::Null),
                        })
                    })
                    .collect();
                Ok(json!({ "messages": items }).to_string())
            }
            ("submit_result", "work") => {
                let subject = subjects::work_submit(&owner, &project, seq);
                self.submit(&subject, &args).await
            }
            ("submit_eval", "eval") => {
                if args.get("pass").and_then(|p| p.as_bool()).is_none() {
                    return Err("pass (boolean) is required".into());
                }
                let task_id = self.ctx.task_id.ok_or("JOB_TASK_ID not set")?;
                let subject = subjects::eval_submit(&owner, &project, seq, task_id);
                self.submit(&subject, &args).await
            }
            (other, role) => Err(format!("tool {other:?} is not available in role {role:?}")),
        }
    }

    /// §4.2 reliability: bounded retry until the dispatcher acks.
    async fn submit(&mut self, subject: &str, payload: &Value) -> Result<String, String> {
        let bytes = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        let reply = self
            .store()
            .await?
            .request_with_retry(subject, &bytes, 10, Duration::from_millis(500))
            .await
            .map_err(|e| format!("submit failed after retries: {e}"))?;
        let body: Value = serde_json::from_slice(&reply.payload).unwrap_or(Value::Null);
        if body.get("error").is_some() {
            return Err(format!("dispatcher rejected submission: {body}"));
        }
        Ok("submitted".into())
    }

    async fn update_channel(
        &mut self,
        apply: impl FnOnce(&mut ChannelEntry),
    ) -> Result<(), String> {
        let key = keys::channel_key(&self.ctx.owner, &self.ctx.project, self.ctx.seq);
        let bucket = self
            .store()
            .await?
            .raw_bucket(buckets::CHANNELS)
            .await
            .map_err(|e| e.to_string())?;
        let mut entry: ChannelEntry = bucket
            .get_json(&key)
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or(ChannelEntry { update: None, last_reply: None });
        apply(&mut entry);
        bucket.put_json(&key, &entry).await.map_err(|e| e.to_string())
    }
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(role: &str) -> JobContext {
        JobContext {
            owner: "acme".into(),
            project: "api".into(),
            seq: 42,
            role: role.into(),
            task_id: (role == "eval").then_some(7),
            nats_url: "nats://unused".into(),
            nats_token: String::new(),
        }
    }

    #[tokio::test]
    async fn handshake_and_tool_list_work_without_nats() {
        let mut server = Server::new(ctx("work"));
        let init = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
            .await
            .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "chuggernaut-channel");

        // Notification: no response.
        assert!(
            server
                .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .await
                .is_none()
        );

        let list = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .await
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["update_status", "channel_check", "reply", "submit_result"]);
    }

    #[tokio::test]
    async fn eval_role_swaps_submit_tool_and_rejects_wrong_role() {
        let mut server = Server::new(ctx("eval"));
        let list = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
            .await
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"submit_eval"));
        assert!(!names.contains(&"submit_result"));

        let call = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "submit_result", "arguments": {} } }))
            .await
            .unwrap();
        assert_eq!(call["result"]["isError"], true);

        // submit_eval without pass is rejected before any NATS traffic.
        let call = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "submit_eval", "arguments": {} } }))
            .await
            .unwrap();
        assert_eq!(call["result"]["isError"], true);
    }
}
