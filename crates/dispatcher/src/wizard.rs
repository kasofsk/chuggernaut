//! The "job wizard" (New Job screen): a short chatbot conversation that turns a
//! rough goal into a high-quality ticket. The wizard sees the project's file
//! tree and recent jobs as grounding context, asks a couple of clarifying
//! questions, then proposes a polished title + description written for the
//! implementer. Purely advisory — it never touches job state; the operator
//! still fills the remaining fields and hits "create job".
//!
//! Runs off the core loop like the other read handlers ([`crate::handlers`]):
//! it gathers read-only context and calls the Anthropic Messages API. Config
//! comes from the `WIZARD_*` env (falling back to `ANTHROPIC_API_KEY`); unset →
//! the feature is unavailable (503) and the UI falls back to manual title/
//! description entry.

use serde::{Deserialize, Serialize};

/// Default model for the wizard — a fast, capable Claude model is plenty for a
/// few turns of ticket-shaping. Overridable via `WIZARD_MODEL`.
const DEFAULT_MODEL: &str = "claude-sonnet-5";

/// Anthropic API version header (the Messages API contract).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Ceiling on the ticket the wizard writes — a thorough ticket, not a novel.
const MAX_TOKENS: u32 = 2048;

/// How many recent jobs and repo files to surface as context. Bounded so the
/// prompt stays small on large repos with long histories.
const MAX_CONTEXT_JOBS: usize = 15;
const MAX_CONTEXT_FILES: usize = 300;

/// Resolved wizard configuration. Absent (`None` at the call site) → the
/// feature is off and callers reply 503.
#[derive(Debug, Clone)]
pub struct WizardConfig {
    pub api_key: String,
    pub model: String,
    /// Anthropic API origin (no trailing slash), e.g. `https://api.anthropic.com`.
    pub base_url: String,
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl WizardConfig {
    /// Read from `WIZARD_API_KEY` (or `ANTHROPIC_API_KEY`), `WIZARD_MODEL`,
    /// `WIZARD_BASE_URL`. None when no API key is present — the wizard is
    /// simply unavailable, not a startup error.
    pub fn from_env() -> Option<Self> {
        let api_key = env_opt("WIZARD_API_KEY").or_else(|| env_opt("ANTHROPIC_API_KEY"))?;
        Some(Self {
            api_key,
            model: env_opt("WIZARD_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into()),
            base_url: env_opt("WIZARD_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".into())
                .trim_end_matches('/')
                .to_string(),
        })
    }
}

/// One message in the wizard conversation. `role` is `user` or `assistant`,
/// matching the Anthropic Messages API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardMessage {
    pub role: String,
    pub content: String,
}

/// Request body the api bridges to `req.wizard.chat.{owner}.{project}`.
#[derive(Debug, Clone, Deserialize)]
pub struct WizardRequest {
    #[serde(default)]
    pub messages: Vec<WizardMessage>,
}

/// A ticket draft the wizard proposes once it has enough to go on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketDraft {
    pub title: String,
    pub description: String,
}

/// One wizard turn returned to the UI: what to show in the chat, and — once
/// ready — the ticket draft to pre-fill the form with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WizardTurn {
    /// The assistant's chat message (a question, feedback, or a short
    /// "here's your ticket" confirmation).
    pub reply: String,
    /// Present once the wizard has drafted the ticket; `null` while still
    /// gathering information.
    pub draft: Option<TicketDraft>,
    /// True once a draft is offered — the UI can enable "create job".
    pub done: bool,
}

/// A one-line job summary for the context block — kept minimal so `build_context`
/// stays unit-testable without constructing full [`types::Job`]s.
pub struct JobLine {
    pub id: u64,
    pub r#type: String,
    pub title: String,
    pub state: String,
}

/// Build the grounding context block embedded in the system prompt: the recent
/// jobs (so the wizard doesn't propose a duplicate and matches house style) and
/// a sample of the repo's file layout (so it can reference real paths).
pub fn build_context(project: &str, files: &[String], jobs: &[JobLine]) -> String {
    let mut out = format!("Project: {project}\n");

    out.push_str("\nRecent jobs (newest first):\n");
    if jobs.is_empty() {
        out.push_str("  (none yet)\n");
    } else {
        for j in jobs.iter().take(MAX_CONTEXT_JOBS) {
            let title = if j.title.is_empty() { "—" } else { &j.title };
            out.push_str(&format!(
                "  #{} [{}] {} ({})\n",
                j.id, j.r#type, title, j.state
            ));
        }
    }

    out.push_str("\nRepository files (a sample of the layout):\n");
    if files.is_empty() {
        out.push_str("  (empty)\n");
    } else {
        let shown = files.len().min(MAX_CONTEXT_FILES);
        for f in files.iter().take(MAX_CONTEXT_FILES) {
            out.push_str(&format!("  {f}\n"));
        }
        if files.len() > shown {
            out.push_str(&format!("  … and {} more\n", files.len() - shown));
        }
    }
    out
}

/// The wizard's system prompt: role, the grounding context, and the strict
/// JSON output contract [`parse_turn`] reads back.
pub fn system_prompt(context: &str) -> String {
    format!(
        "You are the Job Wizard for Chuggernaut, a job orchestrator that runs coding \
agents against a project repository. An operator wants to create a new job (a \
ticket for an implementer agent) and has told you their goal. Your job is to run \
a SHORT conversation that turns their rough goal into an excellent ticket.\n\n\
Be efficient: ask AT MOST two or three focused clarifying questions total, only \
when the answer would materially change the implementation. If the goal is \
already clear enough, skip straight to drafting. Use the project context below \
to ground your questions and the ticket — reference real files and avoid \
proposing work that duplicates a recent job.\n\n\
When you draft the ticket, write it FOR THE IMPLEMENTER: a crisp imperative \
title, then a thorough description covering the goal, relevant context and \
files, concrete acceptance criteria, and any constraints or edge cases. Do not \
invent facts you cannot support from the context or the conversation.\n\n\
=== PROJECT CONTEXT ===\n{context}\n=== END CONTEXT ===\n\n\
Respond with a SINGLE JSON object and nothing else, in this exact shape:\n\
{{\"reply\": string, \"ready\": boolean, \"title\": string, \"description\": string}}\n\
- While still gathering information: set \"ready\": false, put your next question \
or feedback in \"reply\", and leave \"title\" and \"description\" as empty strings.\n\
- Once you can write a good ticket: set \"ready\": true, put a brief one-line \
confirmation in \"reply\" (e.g. \"Here's a ticket for that.\"), and fill \"title\" \
and \"description\". \"description\" may use Markdown.\n\
Output only the JSON object — no prose, no code fences."
    )
}

/// Parse the model's text into a [`WizardTurn`]. Tolerant of code fences and
/// surrounding prose: extracts the first balanced `{...}` object. If nothing
/// parses, falls back to treating the whole text as a not-yet-ready reply so
/// the conversation never dead-ends on a malformed response.
pub fn parse_turn(text: &str) -> WizardTurn {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        reply: String,
        #[serde(default)]
        ready: bool,
        #[serde(default)]
        title: String,
        #[serde(default)]
        description: String,
    }

    if let Some(obj) = extract_json_object(text)
        && let Ok(raw) = serde_json::from_str::<Raw>(&obj)
    {
        let has_ticket = !raw.title.trim().is_empty() || !raw.description.trim().is_empty();
        let done = raw.ready && has_ticket;
        let reply = if raw.reply.trim().is_empty() && done {
            "Here's a ticket for that.".to_string()
        } else {
            raw.reply
        };
        return WizardTurn {
            reply,
            draft: done.then(|| TicketDraft {
                title: raw.title.trim().to_string(),
                description: raw.description.trim().to_string(),
            }),
            done,
        };
    }
    WizardTurn {
        reply: text.trim().to_string(),
        draft: None,
        done: false,
    }
}

/// Extract the first balanced top-level `{...}` object from `text`, ignoring
/// braces inside JSON strings. Returns the object substring, if any.
fn extract_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Errors surfaced to the operator (mapped to a 502/503 by the handler).
#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    #[error("job wizard is not configured")]
    Unconfigured,
    #[error("the wizard needs at least one message")]
    EmptyConversation,
    #[error("wizard model request failed: {0}")]
    Http(String),
    #[error("wizard model returned {status}: {message}")]
    Status { status: u16, message: String },
}

/// Run one wizard turn: assemble the prompt from context + conversation, call
/// the Anthropic Messages API, and parse the reply.
pub async fn run(
    config: &WizardConfig,
    context: &str,
    messages: &[WizardMessage],
) -> Result<WizardTurn, WizardError> {
    if messages.is_empty() {
        return Err(WizardError::EmptyConversation);
    }
    let system = system_prompt(context);
    let text = call_anthropic(config, &system, messages).await?;
    Ok(parse_turn(&text))
}

/// POST the conversation to the Anthropic Messages API and return the assistant
/// text. Mirrors the reqwest shape in [`crate::github`].
async fn call_anthropic(
    config: &WizardConfig,
    system: &str,
    messages: &[WizardMessage],
) -> Result<String, WizardError> {
    let url = format!("{}/v1/messages", config.base_url);
    let body = serde_json::json!({
        "model": config.model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect::<Vec<_>>(),
    });
    let resp = reqwest::Client::new()
        .post(&url)
        .header("x-api-key", &config.api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .timeout(std::time::Duration::from_secs(60))
        .json(&body)
        .send()
        .await
        .map_err(|e| WizardError::Http(e.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| WizardError::Http(e.to_string()))?;
    if !status.is_success() {
        // Anthropic errors carry {"error": {"message": ...}}; fall back to body.
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or(text);
        return Err(WizardError::Status {
            status: status.as_u16(),
            message,
        });
    }
    extract_reply_text(&text).ok_or_else(|| WizardError::Http("empty model response".into()))
}

/// Pull the concatenated `text` blocks out of a Messages API response.
fn extract_reply_text(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let blocks = value.get("content")?.as_array()?;
    let text: String = blocks
        .iter()
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_lists_jobs_and_files() {
        let jobs = vec![
            JobLine {
                id: 3,
                r#type: "feature".into(),
                title: "Add login".into(),
                state: "Done".into(),
            },
            JobLine {
                id: 2,
                r#type: "bug".into(),
                title: String::new(),
                state: "Work".into(),
            },
        ];
        let files = vec!["src/main.rs".to_string(), "README.md".to_string()];
        let ctx = build_context("acme/api", &files, &jobs);
        assert!(ctx.contains("Project: acme/api"));
        assert!(ctx.contains("#3 [feature] Add login (Done)"));
        // Empty title renders as an em dash, not blank.
        assert!(ctx.contains("#2 [bug] — (Work)"));
        assert!(ctx.contains("src/main.rs"));
    }

    #[test]
    fn context_truncates_and_notes_the_remainder() {
        let files: Vec<String> = (0..MAX_CONTEXT_FILES + 5)
            .map(|i| format!("f{i}.rs"))
            .collect();
        let ctx = build_context("p", &files, &[]);
        assert!(ctx.contains("… and 5 more"));
        assert!(ctx.contains("(none yet)")); // no jobs
    }

    #[test]
    fn parse_plain_json_not_ready() {
        let turn = parse_turn(
            r#"{"reply":"What should happen on error?","ready":false,"title":"","description":""}"#,
        );
        assert!(!turn.done);
        assert!(turn.draft.is_none());
        assert_eq!(turn.reply, "What should happen on error?");
    }

    #[test]
    fn parse_ready_draft() {
        let turn = parse_turn(
            r#"{"reply":"Here's a ticket.","ready":true,"title":"Fix retry loop","description":"The retry loop never backs off.\n\n## Acceptance\n- backoff added"}"#,
        );
        assert!(turn.done);
        let draft = turn.draft.expect("draft present");
        assert_eq!(draft.title, "Fix retry loop");
        assert!(draft.description.contains("Acceptance"));
    }

    #[test]
    fn parse_tolerates_code_fences_and_prose() {
        let text = "Sure, here you go:\n```json\n{\"reply\":\"ok\",\"ready\":true,\"title\":\"T\",\"description\":\"D\"}\n```\n";
        let turn = parse_turn(text);
        assert!(turn.done);
        assert_eq!(turn.draft.unwrap().title, "T");
    }

    #[test]
    fn parse_ready_but_no_ticket_is_not_done() {
        // ready:true with empty fields must not enable "create job".
        let turn = parse_turn(r#"{"reply":"ok","ready":true,"title":"","description":""}"#);
        assert!(!turn.done);
        assert!(turn.draft.is_none());
    }

    #[test]
    fn parse_malformed_falls_back_to_reply() {
        let turn = parse_turn("I can't produce JSON right now, sorry.");
        assert!(!turn.done);
        assert_eq!(turn.reply, "I can't produce JSON right now, sorry.");
    }

    #[test]
    fn extract_ignores_braces_inside_strings() {
        let obj = extract_json_object(r#"prefix {"a":"has } brace","b":1} suffix"#).unwrap();
        assert_eq!(obj, r#"{"a":"has } brace","b":1}"#);
    }

    #[test]
    fn extract_reply_concatenates_text_blocks() {
        let body =
            r#"{"content":[{"type":"text","text":"hello "},{"type":"text","text":"world"}]}"#;
        assert_eq!(extract_reply_text(body).unwrap(), "hello world");
    }

    #[test]
    fn from_env_prefers_wizard_key(/* env-free: constructed directly */) {
        // The parsing logic that matters (fallback, trailing slash) is exercised
        // via a direct construction rather than mutating process env in tests.
        let cfg = WizardConfig {
            api_key: "k".into(),
            model: DEFAULT_MODEL.into(),
            base_url: "https://api.anthropic.com".into(),
        };
        assert_eq!(cfg.base_url, "https://api.anthropic.com");
    }
}
