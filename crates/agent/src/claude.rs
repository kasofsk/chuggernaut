//! ClaudeProvider (spec §4.3).
//!
//! CMD (inside the workspace bootstrap): `claude -p "$(cat /chuggernaut/prompt.md)"
//! --model {model} --append-system-prompt {system_prompt}
//! --mcp-config /chuggernaut/mcp-config.json`.
//! Prompt content is injected at `/chuggernaut/prompt.md` — never a path, never
//! inline in the shell command. The `--mcp-config` payload carries NATS
//! credentials (the channel MCP server's `NATS_CREDS` env), so it is injected
//! as a mode-0600 file rather than passed inline in argv: argv leaks into `ps`,
//! `/proc/*/cmdline`, and crash reports (spec §4.3). Supports push notifications
//! (`claude/channel` experimental capability).

use crate::{AgentError, AgentOutput, AgentProvider, AgentRunConfig, McpServerConfig, PROMPT_PATH};
use async_trait::async_trait;
use container::{ContainerBackend, ContainerLaunchConfig, InjectedFile, bootstrap_cmd};
use std::sync::Arc;

/// Where the `--mcp-config` payload is injected. Kept out of argv because it
/// carries the job-scoped NATS credential (spec §4.3); mode 0600 so only the
/// container's own root can read it.
pub const MCP_CONFIG_PATH: &str = "/chuggernaut/mcp-config.json";

pub struct ClaudeProvider {
    backend: Arc<dyn ContainerBackend>,
}

/// A composed agent invocation: the shell line plus any provider-owned files it
/// references. Every credential-bearing payload rides in `files` (mode 0600),
/// never in `command` — so `ps`/`cmdline` never sees a secret. Work,
/// agent-evaluator, and inline-review author/reviewer commands all compose
/// through here and inherit that property.
pub(crate) struct Invocation {
    pub command: String,
    pub files: Vec<InjectedFile>,
}

impl ClaudeProvider {
    pub fn new(backend: Arc<dyn ContainerBackend>) -> Self {
        Self { backend }
    }

    /// The shell line executed after the workspace bootstrap's clone+cd, plus
    /// the provider-owned files it references.
    ///
    /// `--dangerously-skip-permissions`: headless agents cannot answer
    /// permission prompts; the container is the sandbox (the agent image
    /// sets IS_SANDBOX=1 so the flag is accepted under root).
    ///
    /// `--session-id` makes the transcript filename deterministic so it can be
    /// harvested after exit. `--output-format json` makes stdout a single
    /// result object carrying real `usage` and the session id — the CLI's
    /// documented interface, as opposed to the transcript, whose format is
    /// internal and version-unstable. Nothing read agent stdout before this.
    ///
    /// The MCP config is written to [`MCP_CONFIG_PATH`] (mode 0600) and passed
    /// by path: the CLI accepts a file for `--mcp-config`, and the payload
    /// carries the NATS credential, which must never enter argv.
    fn claude_invocation(config: &AgentRunConfig) -> Invocation {
        let mut command = format!(
            "claude -p \"$(cat {PROMPT_PATH})\" --dangerously-skip-permissions \
             --output-format json --session-id {}",
            shell_quote(&config.session_id)
        );
        if let Some(model) = &config.model {
            command.push_str(&format!(" --model {}", shell_quote(model)));
        }
        if let Some(system) = &config.system_prompt {
            command.push_str(&format!(" --append-system-prompt {}", shell_quote(system)));
        }
        let mut files = vec![];
        if !config.mcp_servers.is_empty() {
            let json = mcp_config_json(&config.mcp_servers);
            command.push_str(&format!(" --mcp-config {MCP_CONFIG_PATH}"));
            files.push(InjectedFile {
                container_path: MCP_CONFIG_PATH.to_string(),
                contents: json.into_bytes(),
                mode: 0o600,
                artifact: None,
            });
        }
        Invocation { command, files }
    }
}

#[async_trait]
impl AgentProvider for ClaudeProvider {
    async fn run(&self, config: AgentRunConfig) -> Result<AgentOutput, AgentError> {
        let invocation = Self::claude_invocation(&config);

        let mut files = vec![InjectedFile {
            container_path: PROMPT_PATH.to_string(),
            contents: config.prompt.clone().into_bytes(),
            mode: 0o644,
            artifact: None,
        }];
        files.extend(invocation.files);
        files.extend(config.files.iter().cloned());

        let mut env = config.env.clone();
        env.insert("CLAUDE_CONFIG_DIR".into(), crate::CLAUDE_CONFIG_DIR.into());

        let launch = ContainerLaunchConfig {
            image: config.image.clone(),
            cmd: bootstrap_cmd(&["sh".into(), "-c".into(), invocation.command]),
            env,
            files,
            cpu_limit: None,    // resource limits ride on the dispatcher's
            memory_limit: None, // command-container path; provider adds none yet
            node: config.node.clone(),
        };
        let id = self.backend.launch(launch).await?;
        let out = |exit_code| AgentOutput {
            exit_code,
            container_id: Some(id.clone()),
            session_id: Some(config.session_id.clone()),
        };

        // The provider owns the timeout for its own container (§3.5's scan has
        // historically had no id for agent tasks). A timeout still returns Ok:
        // the container is killed, but the caller must be handed the id so it
        // can harvest the transcript — a timed-out run is the one most worth
        // reading. Exit -1 marks it, matching the dispatcher's failure encoding.
        match tokio::time::timeout(config.task_timeout, self.backend.wait(&id)).await {
            Ok(exit) => Ok(out(exit?)),
            Err(_elapsed) => {
                tracing::warn!(
                    "agent container {id} exceeded task_timeout {:?}; killing",
                    config.task_timeout
                );
                let _ = self.backend.kill(&id).await;
                Ok(out(-1))
            }
        }
    }

    fn supports_push_notifications(&self) -> bool {
        true
    }
}

/// Pull real token usage out of the CLI's `--output-format json` result object,
/// which is its documented interface — unlike the session transcript, whose
/// format Anthropic documents as internal and version-unstable.
///
/// The result object is the last JSON value on stdout; anything the container
/// printed before it (the workspace clone, npm noise) is skipped by scanning
/// for the last line that parses. Returns `None` rather than erroring: usage is
/// reporting, and must never fail a task that otherwise succeeded.
pub fn parse_usage(stdout: &[u8]) -> Option<types::TokenUsage> {
    let text = String::from_utf8_lossy(stdout);
    let value: serde_json::Value = text
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line.trim()).ok())?;
    let usage = value.get("usage")?;
    let n = |key: &str| usage.get(key).and_then(|v| v.as_u64());
    Some(types::TokenUsage {
        input_tokens: n("input_tokens").unwrap_or(0),
        output_tokens: n("output_tokens").unwrap_or(0),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
    })
}

/// Pull the agent's final result text out of the CLI's `--output-format json`
/// result object — its `result` field. Used to capture a **triage assessment**
/// (spec §1.2), which runs without the channel MCP: there is no `submit_result`
/// call, so the CLI's own JSON result on stdout is the only channel for the
/// written output.
///
/// Same last-parseable-line scan as [`parse_usage`] — anything the container
/// printed before the result object (clone, npm noise) is skipped. Returns
/// `None` when stdout carries no result object or the result is empty.
pub fn parse_result(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let value: serde_json::Value = text
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line.trim()).ok())?;
    value
        .get("result")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// Claude CLI `--mcp-config` payload: `{"mcpServers": {name: {command, args, env}}}`.
fn mcp_config_json(servers: &[McpServerConfig]) -> String {
    let map: serde_json::Map<String, serde_json::Value> = servers
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                serde_json::json!({
                    "command": s.command,
                    "args": s.args,
                    "env": s.env,
                }),
            )
        })
        .collect();
    serde_json::json!({ "mcpServers": map }).to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn config() -> AgentRunConfig {
        AgentRunConfig {
            image: "img:latest".into(),
            prompt: "do the thing".into(),
            model: Some("claude-sonnet-4-6".into()),
            system_prompt: Some("KO facts".into()),
            mcp_servers: vec![McpServerConfig {
                name: "chuggernaut-channel".into(),
                command: "/usr/local/bin/chuggernaut-channel".into(),
                args: vec![],
                env: HashMap::from([("NATS_URL".into(), "nats://x".into())]),
            }],
            files: vec![],
            env: HashMap::new(),
            task_timeout: Duration::from_secs(60),
            eval_context: vec![],
            merge_conflict: None,
            session_id: "da08d5f3-844e-430e-8363-39b4882f437b".into(),
            node: None,
        }
    }

    /// Recorded verbatim from `claude -p ... --output-format json` on CLI
    /// 2.1.211 (an auth-failure run, which still emits a full result object).
    /// This is an externally-owned contract: if the CLI changes shape, this
    /// fixture is where it should break.
    const RESULT_JSON: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":401,"duration_ms":499,"duration_api_ms":0,"num_turns":1,"result":"Invalid API key","stop_reason":"stop_sequence","session_id":"6f1db8aa-e7a0-465c-ad2c-492d6ef2cd86","total_cost_usd":0,"usage":{"input_tokens":12,"cache_creation_input_tokens":34,"cache_read_input_tokens":56,"output_tokens":78,"service_tier":"standard"},"modelUsage":{},"permission_denials":[],"terminal_reason":"api_error","uuid":"c5596749-3e8f-4175-8a7a-be175d506a9e"}"#;

    /// A realistic job-scoped NATS credential — user JWT + NKEY seed — as it
    /// arrives in the channel MCP server's `env`. The whole point of the change
    /// is that no fragment of this ever reaches argv.
    const NATS_CREDS: &str = "-----BEGIN NATS USER JWT-----\n\
        eyJ0eXAiOiJKV1QiLCJhbGciOiJlZDI1NTE5LW5rZXkifQ.PAYLOAD.SIG\n\
        ------END NATS USER JWT------\n\n\
        -----BEGIN USER NKEY SEED-----\n\
        SUAGC3DCT7DHY6TQKEPNXKVHTHULNVR7KE5G6QYWQ2Q4JW3AB2LG5UGXNU\n\
        ------END USER NKEY SEED------\n";

    /// A config whose channel MCP server carries the live NATS credential in
    /// its env — the shape the dispatcher hands every real work/eval run.
    fn config_with_creds() -> AgentRunConfig {
        let mut c = config();
        c.mcp_servers = vec![McpServerConfig {
            name: "chuggernaut-channel".into(),
            command: "/usr/local/bin/chuggernaut-channel".into(),
            args: vec![],
            env: HashMap::from([
                ("NATS_URL".into(), "nats://x".into()),
                ("NATS_CREDS".into(), NATS_CREDS.into()),
            ]),
        }];
        c
    }

    /// Assert the composed argv leaks no credential material, and — when the run
    /// has MCP servers — that the credential instead rides in a mode-0600 file.
    fn assert_no_creds_in_argv(inv: &Invocation) {
        for needle in [
            NATS_CREDS,
            "NATS USER JWT",
            "USER NKEY SEED",
            "SUAGC3DCT7DHY6TQKEPNXKVHTHULNVR7KE5G6QYWQ2Q4JW3AB2LG5UGXNU",
            "NATS_CREDS",
        ] {
            assert!(
                !inv.command.contains(needle),
                "credential fragment {needle:?} leaked into argv: {}",
                inv.command
            );
        }
    }

    #[test]
    fn invocation_composes_all_flags() {
        let inv = ClaudeProvider::claude_invocation(&config());
        assert!(inv.command.starts_with(
            "claude -p \"$(cat /chuggernaut/prompt.md)\" --dangerously-skip-permissions"
        ));
        assert!(inv.command.contains("--model 'claude-sonnet-4-6'"));
        assert!(inv.command.contains("--append-system-prompt 'KO facts'"));
        // The MCP config travels by path, never inline.
        assert!(
            inv.command
                .contains("--mcp-config /chuggernaut/mcp-config.json")
        );
        assert!(!inv.command.contains("chuggernaut-channel"));
    }

    /// Without these two the transcript is unaddressable and usage unmeasurable
    /// — the whole point of capture.
    #[test]
    fn invocation_pins_session_and_json_output() {
        let cmd = ClaudeProvider::claude_invocation(&config()).command;
        assert!(
            cmd.contains("--session-id 'da08d5f3-844e-430e-8363-39b4882f437b'"),
            "{cmd}"
        );
        assert!(cmd.contains("--output-format json"), "{cmd}");
    }

    #[test]
    fn minimal_invocation_omits_optional_flags() {
        let mut c = config();
        c.model = None;
        c.system_prompt = None;
        c.mcp_servers = vec![];
        let inv = ClaudeProvider::claude_invocation(&c);
        assert_eq!(
            inv.command,
            "claude -p \"$(cat /chuggernaut/prompt.md)\" --dangerously-skip-permissions \
             --output-format json --session-id 'da08d5f3-844e-430e-8363-39b4882f437b'"
        );
        assert!(inv.files.is_empty());
    }

    /// The credential leak that motivated this change: a plain work task's argv
    /// must carry no JWT/NKEY material.
    #[test]
    fn plain_work_argv_carries_no_credentials() {
        assert_no_creds_in_argv(&ClaudeProvider::claude_invocation(&config_with_creds()));
    }

    /// An agent-evaluator run composes through the same primitive — same
    /// credential-out-of-argv guarantee.
    #[test]
    fn agent_evaluator_argv_carries_no_credentials() {
        let mut c = config_with_creds();
        c.system_prompt = Some("evaluate the change against the brief".into());
        c.eval_context = vec![];
        assert_no_creds_in_argv(&ClaudeProvider::claude_invocation(&c));
    }

    /// The inline-review author/reviewer commands are composed the same way
    /// (spec §4.5); assert the shape once here.
    #[test]
    fn inline_review_argv_carries_no_credentials() {
        let mut c = config_with_creds();
        c.merge_conflict = Some("crates/store/src/lib.rs".into());
        assert_no_creds_in_argv(&ClaudeProvider::claude_invocation(&c));
    }

    /// The credential must land in the injected file — mode 0600, at the
    /// documented path — and nowhere else.
    #[test]
    fn mcp_config_file_is_private_and_carries_the_creds() {
        let inv = ClaudeProvider::claude_invocation(&config_with_creds());
        let file = inv
            .files
            .iter()
            .find(|f| f.container_path == MCP_CONFIG_PATH)
            .expect("mcp config file injected");
        assert_eq!(
            file.mode, 0o600,
            "credential file must not be world/group readable"
        );
        // The payload is JSON, so newlines are escaped — assert on the NKEY seed
        // and JWT marker, which survive serialization verbatim.
        let contents = String::from_utf8(file.contents.clone()).unwrap();
        assert!(
            contents.contains("SUAGC3DCT7DHY6TQKEPNXKVHTHULNVR7KE5G6QYWQ2Q4JW3AB2LG5UGXNU"),
            "creds must ride in the file"
        );
        assert!(contents.contains("NATS USER JWT"));
        assert!(file.artifact.is_none());
    }

    #[test]
    fn parses_usage_from_the_real_result_shape() {
        let usage = parse_usage(RESULT_JSON.as_bytes()).expect("usage");
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 78);
        assert_eq!(usage.cache_write_tokens, Some(34));
        assert_eq!(usage.cache_read_tokens, Some(56));
    }

    /// The clone and any image noise print before the result object.
    #[test]
    fn parses_usage_past_leading_container_noise() {
        let mut stdout = b"Cloning into '/workspace'...\nnpm notice\n".to_vec();
        stdout.extend_from_slice(RESULT_JSON.as_bytes());
        assert_eq!(parse_usage(&stdout).unwrap().input_tokens, 12);
    }

    /// Usage is reporting; a task that worked must not fail because stdout was
    /// unparseable (wrong format, truncated log, killed container).
    #[test]
    fn missing_or_garbage_usage_is_none_not_an_error() {
        assert!(parse_usage(b"").is_none());
        assert!(parse_usage(b"plain text output").is_none());
        assert!(parse_usage(br#"{"type":"result","session_id":"x"}"#).is_none());
    }

    /// The transcript path is measured, not documented — the published docs say
    /// `/workspace` slugs to `workspace`; the CLI actually writes `-workspace`.
    #[test]
    fn transcript_path_uses_the_measured_slug() {
        assert_eq!(
            crate::transcript_path("da08d5f3-844e-430e-8363-39b4882f437b"),
            "/chuggernaut/claude/projects/-workspace/da08d5f3-844e-430e-8363-39b4882f437b.jsonl"
        );
    }

    #[test]
    fn parses_result_text_for_triage() {
        // A success result object carries the agent's prose in `result`.
        let json = r#"{"type":"result","subtype":"success","is_error":false,"result":"The work task failed because the migration script referenced a dropped column. Recommend Revoke.","session_id":"x","usage":{"input_tokens":1,"output_tokens":2}}"#;
        assert_eq!(
            parse_result(json.as_bytes()).as_deref(),
            Some(
                "The work task failed because the migration script referenced a dropped column. Recommend Revoke."
            )
        );
        // Past leading container noise.
        let mut stdout = b"Cloning into '/workspace'...\n".to_vec();
        stdout.extend_from_slice(json.as_bytes());
        assert!(parse_result(&stdout).is_some());
        // Empty / missing result → None, never an empty assessment.
        assert!(parse_result(br#"{"type":"result","result":""}"#).is_none());
        assert!(parse_result(b"plain output").is_none());
    }

    #[test]
    fn mcp_config_shape() {
        let json = mcp_config_json(&config().mcp_servers);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["mcpServers"]["chuggernaut-channel"]["command"],
            "/usr/local/bin/chuggernaut-channel"
        );
    }
}
