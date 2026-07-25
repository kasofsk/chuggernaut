//! Minimal MCP stdio server (newline-delimited JSON-RPC 2.0) exposing the
//! §4.2 channel tools. No MCP framework — the protocol surface we need is
//! initialize / tools/list / tools/call, and a static musl binary wants a
//! short dependency list.

use serde_json::{Value, json};
use std::time::Duration;
use store::{NatsStore, buckets, subjects};
use types::{AgentReply, ChannelOrigin, ChannelUpdate};

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
    /// `.creds`-format scoped credentials (§7.4); empty → unauthenticated dev.
    pub nats_creds: String,
    /// The originating task's identity, stamped onto every update/reply so the
    /// event carries its provenance end to end (spec §6.3). Read from the
    /// `CHUG_TASK_ID` / `CHUG_PHASE` / `CHUG_EVALUATOR` env the dispatcher sets.
    pub origin: ChannelOrigin,
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
            task_id: std::env::var("JOB_TASK_ID")
                .ok()
                .and_then(|t| t.parse().ok()),
            nats_url: var("NATS_URL")?,
            nats_creds: std::env::var("NATS_CREDS").unwrap_or_default(),
            origin: ChannelOrigin {
                task_id: std::env::var("CHUG_TASK_ID")
                    .ok()
                    .and_then(|t| t.parse().ok()),
                phase: std::env::var("CHUG_PHASE").ok().filter(|s| !s.is_empty()),
                evaluator: std::env::var("CHUG_EVALUATOR")
                    .ok()
                    .filter(|s| !s.is_empty()),
            },
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

    // TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
    #[allow(clippy::unwrap_used)]
    async fn store(&mut self) -> Result<&NatsStore, String> {
        if self.store.is_none() {
            let store = if self.ctx.nats_creds.is_empty() {
                NatsStore::connect(&self.ctx.nats_url).await
            } else {
                NatsStore::connect_with_creds(&self.ctx.nats_url, &self.ctx.nats_creds).await
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
                let name = params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default();
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
            tool(
                "update_status",
                "Report progress; overwrites the previous update.",
                json!({ "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "percent": { "type": "integer", "minimum": 0, "maximum": 100 } },
                    "required": ["message"] }),
            ),
            tool(
                "channel_check",
                "Poll the operator inbox; pass the last consumed sequence.",
                json!({ "type": "object",
                    "properties": { "since": { "type": "integer" } } }),
            ),
            tool(
                "reply",
                "Reply to the operator; overwrites the previous reply.",
                json!({ "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"] }),
            ),
        ];
        match self.ctx.role.as_str() {
            "eval" => tools.push(tool(
                "submit_eval",
                "Publish the authoritative eval verdict. Required before exit. \
                 Set abort: true only when the work cannot be fixed by rework \
                 (wrong premise, impossible requirement) — it skips retries and \
                 escalates to a human.",
                json!({ "type": "object",
                    "properties": {
                        "pass": { "type": "boolean" },
                        "abort": { "type": "boolean" },
                        "structured": {},
                        "token_usage": {},
                        "cover_html": { "type": "string",
                            "description": "Optional. A small self-contained HTML cover page for this verdict (a visual before/after or diagram) shown beside your text summary. Never required; omit it unless a visual genuinely helps." } },
                    "required": ["pass"] }),
            )),
            _ => tools.push(tool(
                "submit_result",
                "Submit the work summary and structured context.",
                json!({ "type": "object",
                    "properties": {
                        "summary": { "type": "string" },
                        "structured": {},
                        "token_usage": {},
                        "cover_html": { "type": "string",
                            "description": "Optional. A small self-contained HTML cover page showing what you did (visual changelog, before/after, diagram) shown beside your text summary. Never required; omit it unless a visual genuinely helps. Presentational only — the text summary remains canonical." } } }),
            )),
        }
        tools
    }

    // TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
    #[allow(clippy::too_many_lines)]
    async fn call_tool(&mut self, name: &str, args: Value) -> Result<String, String> {
        let (owner, project, seq) = (
            self.ctx.owner.clone(),
            self.ctx.project.clone(),
            self.ctx.seq,
        );
        match (name, self.ctx.role.as_str()) {
            ("update_status", _) => {
                let update = ChannelUpdate {
                    message: args
                        .get("message")
                        .and_then(|m| m.as_str())
                        .ok_or("message is required")?
                        .to_string(),
                    percent: args
                        .get("percent")
                        .and_then(|p| p.as_u64())
                        .map(|p| p as u8),
                    // Stamped by the dispatcher on accept, not here — the
                    // container's clock is not the platform's.
                    at: None,
                    origin: self.ctx.origin.clone(),
                };
                let subject = subjects::channel_update(&owner, &project, seq);
                self.submit(
                    &subject,
                    &serde_json::to_value(&update).map_err(|e| e.to_string())?,
                )
                .await?;
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
                    origin: self.ctx.origin.clone(),
                };
                let subject = subjects::channel_reply(&owner, &project, seq);
                self.submit(
                    &subject,
                    &serde_json::to_value(&reply).map_err(|e| e.to_string())?,
                )
                .await?;
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
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn ctx(role: &str) -> JobContext {
        JobContext {
            owner: "acme".into(),
            project: "api".into(),
            seq: 42,
            role: role.into(),
            task_id: (role == "eval").then_some(7),
            nats_url: "nats://unused".into(),
            nats_creds: String::new(),
            origin: ChannelOrigin::default(),
        }
    }

    #[test]
    fn origin_is_stamped_onto_posts_as_flat_wire_fields() {
        // The binary stamps its container origin onto every post; it must
        // serialize to the flat `task_id`/`phase`/`evaluator` keys the
        // dispatcher parses back off `req.channel.>` (spec §6.3).
        let origin = ChannelOrigin {
            task_id: Some(3),
            phase: Some("Evaluation".into()),
            evaluator: Some("review".into()),
        };
        let update = ChannelUpdate {
            message: "checking".into(),
            percent: Some(50),
            at: None,
            origin: origin.clone(),
        };
        let v = serde_json::to_value(&update).unwrap();
        assert_eq!(v["message"], "checking");
        assert_eq!(v["task_id"], 3);
        assert_eq!(v["phase"], "Evaluation");
        assert_eq!(v["evaluator"], "review");

        let reply = AgentReply {
            text: "on it".into(),
            sent_at: chrono::Utc::now(),
            origin,
        };
        assert_eq!(serde_json::to_value(&reply).unwrap()["task_id"], 3);
    }

    #[test]
    fn legacy_post_without_origin_omits_the_fields() {
        // A default (empty) origin stamps nothing — old consumers see exactly
        // today's `{message, percent}` shape.
        let update = ChannelUpdate {
            message: "hi".into(),
            percent: None,
            at: None,
            origin: ChannelOrigin::default(),
        };
        let v = serde_json::to_value(&update).unwrap();
        assert!(v.get("task_id").is_none());
        assert!(v.get("phase").is_none());
        assert!(v.get("evaluator").is_none());
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
        assert_eq!(
            names,
            vec!["update_status", "channel_check", "reply", "submit_result"]
        );
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
