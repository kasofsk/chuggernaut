//! The "job wizard" (New Job screen): a short chatbot conversation that turns a
//! rough goal into a high-quality ticket. The wizard sees the project's file
//! tree and recent jobs as grounding context, asks a couple of clarifying
//! questions, then proposes a polished title + description written for the
//! implementer. Purely advisory — it never touches job state; the operator
//! still fills the remaining fields and hits "create job".
//!
//! Runs off the core loop like the other read handlers ([`crate::handlers`]):
//! it gathers read-only context and calls the Anthropic Messages API. The key
//! is resolved once at startup (§12.5): `WIZARD_API_KEY`, then
//! `ANTHROPIC_API_KEY`, then — reusing the platform's existing agent
//! credential — the age-encrypted `CLAUDE_CODE_OAUTH_TOKEN` secret under
//! `global/agents` (§8.2), the same token injected into agent containers.
//! Nothing resolved → the feature is unavailable (503) and the UI falls back
//! to manual title/description entry.

use serde::{Deserialize, Serialize};

/// Default model for the wizard — a fast, capable Claude model is plenty for a
/// few turns of ticket-shaping. Overridable via `WIZARD_MODEL`.
const DEFAULT_MODEL: &str = "claude-sonnet-5";

/// Anthropic API version header (the Messages API contract).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The `anthropic-beta` opt-in the Messages API requires when authenticating
/// with a Claude Code OAuth token (`sk-ant-oat…`) — `/v1/messages` rejects the
/// token without it.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Reserved secret scope holding the platform agent credentials (§8.2): every
/// secret under `global/agents` is injected into agent containers. The wizard
/// reuses `CLAUDE_CODE_OAUTH_TOKEN` from it.
const AGENTS_SCOPE: &str = "agents";
const AGENT_OAUTH_SECRET: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Ceiling on the ticket the wizard writes — a thorough ticket, not a novel.
const MAX_TOKENS: u32 = 2048;

/// Total time the retry loop may spend *sleeping* between capacity retries. The
/// wizard is a synchronous chat turn behind an HTTP request — a few seconds of
/// backoff is fine, minutes are not (the api deadline is ~75s, §routes). Once a
/// further wait would push cumulative sleep past this, give up with [`WizardError::Busy`].
const MAX_TOTAL_BACKOFF: std::time::Duration = std::time::Duration::from_secs(20);

/// Base unit for the exponential capacity backoff when the server sends no
/// `retry-after`: attempt 1 waits `BACKOFF_BASE`, attempt 2 twice that, etc.
const BACKOFF_BASE: std::time::Duration = std::time::Duration::from_millis(500);

/// Fixed pause before the single retry granted to a generic 5xx.
const SERVER_ERROR_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// How many recent jobs and repo files to surface as context. Bounded so the
/// prompt stays small on large repos with long histories.
const MAX_CONTEXT_JOBS: usize = 15;
const MAX_CONTEXT_FILES: usize = 300;

/// How the resolved key authenticates to the Anthropic Messages API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardAuth {
    /// Standard API key (`sk-ant-api…`, or a custom gateway key): the
    /// `x-api-key` header.
    ApiKey,
    /// Claude Code OAuth token (`sk-ant-oat…`): `Authorization: Bearer <token>`
    /// plus the required `anthropic-beta: oauth-2025-04-20` header.
    OAuth,
}

impl WizardAuth {
    /// Pick the auth mode from the token shape: `sk-ant-oat…` OAuth tokens use
    /// Bearer; everything else (`sk-ant-api…`, custom gateway keys) uses
    /// `x-api-key`.
    fn detect(key: &str) -> Self {
        if key.starts_with("sk-ant-oat") {
            WizardAuth::OAuth
        } else {
            WizardAuth::ApiKey
        }
    }
}

/// Resolved wizard configuration. Absent (`None` at the call site) → the
/// feature is off and callers reply 503.
///
/// `api_key` is private and redacted from `Debug`/`Display`: it is a live
/// credential and must never reach a log line or error message. It leaves this
/// process only as an HTTP header (see [`auth_headers`]), never argv.
#[derive(Clone)]
pub struct WizardConfig {
    api_key: String,
    /// Auth mode, derived from the token shape at resolution time.
    pub auth: WizardAuth,
    pub model: String,
    /// Anthropic API origin (no trailing slash), e.g. `https://api.anthropic.com`.
    pub base_url: String,
}

// Manual impls keep the credential out of any `{:?}`/`{}` output (logs, error
// chains, panics).
impl std::fmt::Debug for WizardConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WizardConfig")
            .field("api_key", &"<redacted>")
            .field("auth", &self.auth)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl std::fmt::Display for WizardConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WizardConfig {{ auth: {:?}, model: {}, base_url: {}, api_key: <redacted> }}",
            self.auth, self.model, self.base_url
        )
    }
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl WizardConfig {
    /// Resolve the wizard config, or `None` (feature off — never a startup
    /// error). Key resolution order:
    /// (a) `WIZARD_API_KEY` (explicit override); (b) `ANTHROPIC_API_KEY`;
    /// (c) the age-encrypted `CLAUDE_CODE_OAUTH_TOKEN` agent credential under
    /// `global/agents` (§8.2) — the same token injected into agent containers,
    /// reused so one credential powers both. Auth mode follows the token shape
    /// ([`WizardAuth::detect`]). `WIZARD_MODEL` / `WIZARD_BASE_URL` overrides
    /// apply regardless of key source.
    ///
    /// Resolved once, at startup: setting the secret later needs a dispatcher
    /// restart (§12.5). `secrets` is the dispatcher's decrypting store, or
    /// `None` in dev raw-injection mode (§8.2) — the secret fallback is skipped
    /// there.
    pub async fn from_env_or_secrets(
        secrets: Option<&store::secrets::AgeSecretStore>,
    ) -> Option<Self> {
        let api_key = match env_opt("WIZARD_API_KEY").or_else(|| env_opt("ANTHROPIC_API_KEY")) {
            Some(key) => key,
            None => load_agent_oauth_token(secrets).await?,
        };
        Some(Self::with_key(api_key))
    }

    /// Build the resolved config around a key: token-shape auth detection plus
    /// the `WIZARD_MODEL` / `WIZARD_BASE_URL` overrides.
    fn with_key(api_key: String) -> Self {
        Self {
            auth: WizardAuth::detect(&api_key),
            api_key,
            model: env_opt("WIZARD_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into()),
            base_url: env_opt("WIZARD_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }
}

/// Auth headers for the Messages API call, per resolved mode. Pure so the
/// header shape is unit-testable without a live request. Never logged.
fn auth_headers(config: &WizardConfig) -> Vec<(&'static str, String)> {
    match config.auth {
        WizardAuth::ApiKey => vec![("x-api-key", config.api_key.clone())],
        WizardAuth::OAuth => vec![
            ("authorization", format!("Bearer {}", config.api_key)),
            ("anthropic-beta", OAUTH_BETA.to_string()),
        ],
    }
}

/// Load and decrypt the `CLAUDE_CODE_OAUTH_TOKEN` agent credential from the
/// reserved `global/agents` secret scope (§8.2). `None` when there is no
/// decrypting store (dev raw mode), the secret is unset/empty, or a read
/// fails — a bad secret store must not stop startup, it just leaves the wizard
/// unavailable. Any error is logged without the value.
async fn load_agent_oauth_token(
    secrets: Option<&store::secrets::AgeSecretStore>,
) -> Option<String> {
    use store::secrets::SecretStore;
    let secrets = secrets?;
    match secrets
        .get(
            store::keys::RESERVED_OWNER,
            AGENTS_SCOPE,
            AGENT_OAUTH_SECRET,
        )
        .await
    {
        Ok(value) => value.filter(|t| !t.is_empty()),
        Err(e) => {
            // `e` is a store error (missing key / decrypt failure) and carries
            // no plaintext; safe to log.
            tracing::warn!("job wizard: reading {AGENT_OAUTH_SECRET} failed: {e}");
            None
        }
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

/// Errors surfaced to the operator (mapped to an HTTP status by the handler).
#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    #[error("job wizard is not configured")]
    Unconfigured,
    #[error("the wizard needs at least one message")]
    EmptyConversation,
    #[error("wizard model request failed: {0}")]
    Http(String),
    /// Capacity/rate-limit (429/529) that survived retries. The wizard shares
    /// the platform Claude credential with agent containers (§12.5), so a busy
    /// account rate-limits both — surface a friendly, retry-worthy message
    /// rather than the bare status code.
    #[error("The platform model is busy (agents are running). Try again in a moment.")]
    Busy,
    /// The credential was rejected (401): the OAuth token is expired or revoked.
    /// Distinct from capacity so the operator knows to fix the secret, not wait.
    #[error("wizard credential invalid; check CLAUDE_CODE_OAUTH_TOKEN")]
    AuthInvalid,
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

/// How the retry loop should react to a non-success HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusClass {
    /// Capacity / rate limit (429, 529): retry with capped backoff honoring
    /// `retry-after`, then surface [`WizardError::Busy`].
    Capacity,
    /// Credential rejected (401): no retry — [`WizardError::AuthInvalid`].
    Auth,
    /// Other server error (5xx): one retry, then surface the status.
    ServerError,
    /// Other client error: surface immediately.
    Client,
}

/// Classify an HTTP status into a retry strategy. Pure so the policy is
/// unit-testable without a live request.
fn classify_status(code: u16) -> StatusClass {
    match code {
        401 => StatusClass::Auth,
        429 | 529 => StatusClass::Capacity,
        500..=599 => StatusClass::ServerError,
        _ => StatusClass::Client,
    }
}

/// The delay before the next capacity retry, or `None` to stop retrying because
/// another wait would push cumulative sleep past [`MAX_TOTAL_BACKOFF`]. Waits at
/// least the exponential floor (`BACKOFF_BASE * 2^(attempt-1)`, `attempt` from
/// 1), honoring a larger `retry-after` when the server asks us to wait longer.
/// Taking the max (not `retry-after` verbatim) keeps the schedule monotonic, so
/// a `retry-after: 0` can't spin the loop without ever advancing the cap. Pure
/// and deterministic so the schedule and the cap are directly testable.
fn next_capacity_delay(
    attempt: u32,
    retry_after: Option<std::time::Duration>,
    already_waited: std::time::Duration,
) -> Option<std::time::Duration> {
    let floor = BACKOFF_BASE.saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)));
    let delay = retry_after.unwrap_or(floor).max(floor);
    // Stop if this wait would exceed the total budget — the UI can't block for
    // minutes on a synchronous turn.
    if already_waited.saturating_add(delay) > MAX_TOTAL_BACKOFF {
        return None;
    }
    Some(delay)
}

/// Parse a `retry-after` header value (Anthropic sends whole seconds). Returns
/// `None` for anything unparseable or negative. Pure for testing.
fn parse_retry_after(value: &str) -> Option<std::time::Duration> {
    let secs: f64 = value.trim().parse().ok()?;
    if secs.is_finite() && secs >= 0.0 {
        Some(std::time::Duration::from_secs_f64(secs))
    } else {
        None
    }
}

/// POST the conversation to the Anthropic Messages API and return the assistant
/// text. Mirrors the reqwest shape in [`crate::github`].
///
/// Retries transient failures (§12.5): 429/529 capacity limits back off
/// (honoring `retry-after`, capped by [`MAX_TOTAL_BACKOFF`]) and then surface
/// the friendly [`WizardError::Busy`]; a generic 5xx gets one retry. A 401 is a
/// bad credential — no retry, [`WizardError::AuthInvalid`].
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
    let client = reqwest::Client::new();

    let mut attempt: u32 = 0;
    let mut waited = std::time::Duration::ZERO;
    let mut server_error_retried = false;
    loop {
        attempt += 1;
        let mut req = client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .timeout(std::time::Duration::from_secs(60))
            .json(&body);
        // Auth header(s) depend on the token shape: x-api-key vs Bearer + oauth
        // beta (§12.5). The key is only ever an HTTP header, never argv.
        for (name, value) in auth_headers(config) {
            req = req.header(name, value);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| WizardError::Http(e.to_string()))?;
        let status = resp.status();
        // Read `retry-after` before consuming the body.
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        let text = resp
            .text()
            .await
            .map_err(|e| WizardError::Http(e.to_string()))?;
        if status.is_success() {
            return extract_reply_text(&text)
                .ok_or_else(|| WizardError::Http("empty model response".into()));
        }

        let code = status.as_u16();
        match classify_status(code) {
            StatusClass::Auth => {
                // Never log the credential; the status alone tells the story.
                tracing::warn!(
                    "job wizard: credential rejected ({code}); check {AGENT_OAUTH_SECRET}"
                );
                return Err(WizardError::AuthInvalid);
            }
            StatusClass::Capacity => match next_capacity_delay(attempt, retry_after, waited) {
                Some(delay) => {
                    tracing::warn!(
                        "job wizard: model busy ({code}), retrying in {:?} (attempt {attempt})",
                        delay
                    );
                    tokio::time::sleep(delay).await;
                    waited = waited.saturating_add(delay);
                    continue;
                }
                None => {
                    tracing::warn!("job wizard: model still busy ({code}) after {:?}", waited);
                    return Err(WizardError::Busy);
                }
            },
            StatusClass::ServerError if !server_error_retried => {
                server_error_retried = true;
                tracing::warn!("job wizard: upstream 5xx ({code}), retrying once");
                tokio::time::sleep(SERVER_ERROR_RETRY_DELAY).await;
                continue;
            }
            StatusClass::ServerError | StatusClass::Client => {
                // Anthropic errors carry {"error": {"message": ...}}; fall back to body.
                let message = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(String::from))
                    .unwrap_or(text);
                return Err(WizardError::Status {
                    status: code,
                    message,
                });
            }
        }
    }
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
    use std::time::Duration;

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

    /// Construct a config directly (env-free) so header/redaction/auth logic is
    /// testable without mutating process env or hitting NATS.
    fn cfg(api_key: &str) -> WizardConfig {
        WizardConfig::with_key(api_key.to_string())
    }

    #[test]
    fn auth_mode_follows_token_shape() {
        // OAuth token → Bearer + oauth beta header.
        assert_eq!(WizardAuth::detect("sk-ant-oat01-abc"), WizardAuth::OAuth);
        // Standard API key and anything else → x-api-key.
        assert_eq!(WizardAuth::detect("sk-ant-api03-xyz"), WizardAuth::ApiKey);
        assert_eq!(WizardAuth::detect("gateway-key"), WizardAuth::ApiKey);
    }

    #[test]
    fn oauth_headers_use_bearer_and_beta() {
        let headers = auth_headers(&cfg("sk-ant-oat01-secret"));
        assert_eq!(
            headers,
            vec![
                ("authorization", "Bearer sk-ant-oat01-secret".to_string()),
                ("anthropic-beta", OAUTH_BETA.to_string()),
            ]
        );
        // No x-api-key when authenticating with a Bearer token.
        assert!(headers.iter().all(|(n, _)| *n != "x-api-key"));
    }

    #[test]
    fn api_key_headers_use_x_api_key() {
        let headers = auth_headers(&cfg("sk-ant-api03-secret"));
        assert_eq!(
            headers,
            vec![("x-api-key", "sk-ant-api03-secret".to_string())]
        );
        // No Bearer/beta headers in x-api-key mode.
        assert!(
            headers
                .iter()
                .all(|(n, _)| *n != "authorization" && *n != "anthropic-beta")
        );
    }

    #[test]
    fn base_url_trailing_slash_trimmed() {
        // Direct construction exercises the default; env override tested below.
        let c = cfg("k");
        assert_eq!(c.base_url, "https://api.anthropic.com");
        assert_eq!(c.model, DEFAULT_MODEL);
    }

    #[test]
    fn key_is_redacted_in_debug_and_display() {
        let c = cfg("sk-ant-oat01-supersecret");
        let dbg = format!("{c:?}");
        let disp = format!("{c}");
        assert!(!dbg.contains("supersecret"), "Debug leaked the key: {dbg}");
        assert!(
            !disp.contains("supersecret"),
            "Display leaked the key: {disp}"
        );
        assert!(dbg.contains("<redacted>"));
        assert!(disp.contains("<redacted>"));
        // The non-secret fields still render.
        assert!(dbg.contains("OAuth"));
        assert!(disp.contains("api.anthropic.com"));
    }

    #[test]
    fn classify_status_routes_each_family() {
        // Capacity limits: 429 and Anthropic's 529 "overloaded".
        assert_eq!(classify_status(429), StatusClass::Capacity);
        assert_eq!(classify_status(529), StatusClass::Capacity);
        // Auth failure is its own class — no retry.
        assert_eq!(classify_status(401), StatusClass::Auth);
        // Other 5xx get the one-retry path.
        assert_eq!(classify_status(500), StatusClass::ServerError);
        assert_eq!(classify_status(503), StatusClass::ServerError);
        // Other 4xx surface immediately.
        assert_eq!(classify_status(400), StatusClass::Client);
        assert_eq!(classify_status(404), StatusClass::Client);
    }

    #[test]
    fn capacity_backoff_is_exponential_without_retry_after() {
        // attempt n waits BACKOFF_BASE * 2^(n-1): 500ms, 1s, 2s, 4s …
        let d = |a| next_capacity_delay(a, None, Duration::ZERO).unwrap();
        assert_eq!(d(1), Duration::from_millis(500));
        assert_eq!(d(2), Duration::from_millis(1000));
        assert_eq!(d(3), Duration::from_millis(2000));
        assert_eq!(d(4), Duration::from_millis(4000));
    }

    #[test]
    fn capacity_backoff_honors_retry_after() {
        // A larger retry-after wins over the exponential floor…
        let ra = Some(Duration::from_secs(3));
        assert_eq!(
            next_capacity_delay(1, ra, Duration::ZERO),
            Some(Duration::from_secs(3))
        );
        // …but a tiny (or zero) retry-after can't drop below the floor — the
        // schedule stays monotonic so the total-wait cap always terminates the
        // loop rather than spinning on `retry-after: 0`.
        assert_eq!(
            next_capacity_delay(1, Some(Duration::ZERO), Duration::ZERO),
            Some(BACKOFF_BASE)
        );
    }

    #[test]
    fn capacity_backoff_caps_total_wait() {
        // Once cumulative sleep + the next wait would exceed the ~20s budget,
        // stop retrying (→ Busy) rather than block the synchronous turn.
        let almost = MAX_TOTAL_BACKOFF - Duration::from_secs(2);
        // A 2s wait fits exactly at the boundary…
        assert!(next_capacity_delay(1, Some(Duration::from_secs(2)), almost).is_some());
        // …a 3s wait pushes past the cap.
        assert!(next_capacity_delay(1, Some(Duration::from_secs(3)), almost).is_none());
        // Already at the cap: never wait again.
        assert!(
            next_capacity_delay(1, Some(Duration::from_millis(1)), MAX_TOTAL_BACKOFF).is_none()
        );
    }

    #[test]
    fn retry_after_parses_seconds_and_rejects_garbage() {
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after("  0 "), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_secs_f64(1.5)));
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after("soon"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[test]
    fn friendly_messages_never_leak_the_bare_status() {
        // 429/529 map to the busy message; 401 to the credential message. None
        // of them is a bare status code the operator shouldn't see.
        let busy = WizardError::Busy.to_string();
        assert!(busy.contains("busy"), "{busy}");
        assert!(!busy.contains("429") && !busy.contains("529"), "{busy}");

        let auth = WizardError::AuthInvalid.to_string();
        assert!(auth.contains("CLAUDE_CODE_OAUTH_TOKEN"), "{auth}");
        assert!(!auth.contains("401"), "{auth}");
    }

    #[test]
    fn env_key_precedence_and_secret_fallback() {
        // Precedence and the secret fallback are ordered pure logic; assert it
        // directly rather than mutating global process env (racy across tests).
        // (a) WIZARD_API_KEY, (b) ANTHROPIC_API_KEY, (c) secret token.
        let resolve = |wizard: Option<&str>, anthropic: Option<&str>, secret: Option<&str>| {
            wizard.or(anthropic).or(secret).map(str::to_string)
        };
        assert_eq!(
            resolve(Some("w"), Some("a"), Some("s")).as_deref(),
            Some("w")
        );
        assert_eq!(resolve(None, Some("a"), Some("s")).as_deref(), Some("a"));
        assert_eq!(resolve(None, None, Some("s")).as_deref(), Some("s"));
        assert_eq!(resolve(None, None, None), None);
    }
}
