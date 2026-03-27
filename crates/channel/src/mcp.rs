use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub params: Value,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

// ---------------------------------------------------------------------------
// MCP protocol constants
// ---------------------------------------------------------------------------

pub const PROTOCOL_VERSION: &str = "2024-11-05";

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_TOOLS_LIST: &str = "tools/list";
pub const METHOD_TOOLS_CALL: &str = "tools/call";
pub const METHOD_NOTIFICATIONS_INITIALIZED: &str = "notifications/initialized";

pub const NOTIFY_CHANNEL: &str = "notifications/claude/channel";

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn server_capabilities(push_notifications: bool) -> Value {
    let mut caps = serde_json::json!({
        "tools": {}
    });
    if push_notifications {
        caps.as_object_mut().unwrap().insert(
            "experimental".to_string(),
            serde_json::json!({ "claude/channel": {} }),
        );
    }
    caps
}

pub fn server_info() -> Value {
    serde_json::json!({
        "name": "chuggernaut-channel",
        "version": env!("CARGO_PKG_VERSION")
    })
}

pub fn server_instructions(push_notifications: bool) -> String {
    if push_notifications {
        "Messages from the chuggernaut orchestrator arrive as <channel source=\"chuggernaut-channel\"> tags. \
         Each message has a sender and message_id attribute. \
         When you receive a message asking for status, reply using the reply tool with your current progress. \
         You can also proactively update your status using the update_status tool."
            .to_string()
    } else {
        "You have access to chuggernaut channel tools for communicating with the orchestrator. \
         Call channel_check after completing each major step to see if there are pending messages. \
         If someone asks for a status report, respond via channel_send. \
         Use update_status to report your progress."
            .to_string()
    }
}

pub fn tool_definitions(push_notifications: bool) -> Value {
    let mut tools = Vec::new();

    if push_notifications {
        // In channel mode, expose a reply tool + status tool
        tools.push(serde_json::json!({
            "name": "reply",
            "description": "Reply to a message received via the chuggernaut channel",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "string",
                        "description": "The message_id from the <channel> tag to reply to"
                    },
                    "text": {
                        "type": "string",
                        "description": "The reply message"
                    }
                },
                "required": ["text"]
            }
        }));
    } else {
        // In MCP-only mode, expose check + send tools
        tools.push(serde_json::json!({
            "name": "channel_check",
            "description": "Check for pending messages from the chuggernaut orchestrator. Returns a list of messages or an empty list.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }));
        tools.push(serde_json::json!({
            "name": "channel_send",
            "description": "Send a message to the chuggernaut orchestrator",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to send"
                    }
                },
                "required": ["message"]
            }
        }));
    }

    // Both modes get status tool
    tools.push(serde_json::json!({
        "name": "update_status",
        "description": "Update your current work status visible to the chuggernaut orchestrator",
        "inputSchema": {
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "description": "Current status description (e.g. 'exploring codebase', 'implementing feature', 'running tests')"
                },
                "progress": {
                    "type": "number",
                    "description": "Optional progress percentage (0-100)"
                }
            },
            "required": ["status"]
        }
    }));

    // Result submission tools — always available, prompt tells Claude which to use
    tools.push(serde_json::json!({
        "name": "submit_pr",
        "description": "Submit the title and body for the pull request that will be created from your work. Call this once when you are done with all changes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short, descriptive PR title"
                },
                "body": {
                    "type": "string",
                    "description": "PR body in markdown describing what changed and why"
                }
            },
            "required": ["title", "body"]
        }
    }));

    tools.push(serde_json::json!({
        "name": "submit_changes",
        "description": "Submit a summary of the changes you made during rework. This will be posted as a comment on the existing PR. Call this once when you are done with rework.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Markdown summary of the changes you made in response to the reviewer's feedback"
                }
            },
            "required": ["summary"]
        }
    }));

    tools.push(serde_json::json!({
        "name": "submit_review",
        "description": "Submit your review decision for the pull request. Call this once when you have completed your review.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "decision": {
                    "type": "string",
                    "enum": ["approved", "changes_requested", "escalate"],
                    "description": "Your review decision"
                },
                "feedback": {
                    "type": "string",
                    "description": "Review feedback (required for changes_requested, optional for approved)"
                }
            },
            "required": ["decision"]
        }
    }));

    serde_json::json!(tools)
}
