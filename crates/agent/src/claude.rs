//! ClaudeProvider (spec §4.3).
//!
//! CMD (inside the workspace bootstrap): `claude -p "$(cat /chuggernaut/prompt.md)"
//! --model {model} --append-system-prompt {system_prompt} --mcp-config {json}`.
//! Prompt content is injected at `/chuggernaut/prompt.md` — never a path, never
//! inline in the shell command. Supports push notifications (`claude/channel`
//! experimental capability).

use crate::{AgentError, AgentOutput, AgentProvider, AgentRunConfig, McpServerConfig, PROMPT_PATH};
use async_trait::async_trait;
use container::{ContainerBackend, ContainerLaunchConfig, InjectedFile, bootstrap_cmd};
use std::sync::Arc;

pub struct ClaudeProvider {
    backend: Arc<dyn ContainerBackend>,
}

impl ClaudeProvider {
    pub fn new(backend: Arc<dyn ContainerBackend>) -> Self {
        Self { backend }
    }

    /// The shell line executed after the workspace bootstrap's clone+cd.
    /// `--dangerously-skip-permissions`: headless agents cannot answer
    /// permission prompts; the container is the sandbox (the agent image
    /// sets IS_SANDBOX=1 so the flag is accepted under root).
    ///
    /// `--session-id` makes the transcript filename deterministic so it can be
    /// harvested after exit. `--output-format json` makes stdout a single
    /// result object carrying real `usage` and the session id — the CLI's
    /// documented interface, as opposed to the transcript, whose format is
    /// internal and version-unstable. Nothing read agent stdout before this.
    fn claude_invocation(config: &AgentRunConfig) -> String {
        let mut cmd = format!(
            "claude -p \"$(cat {PROMPT_PATH})\" --dangerously-skip-permissions \
             --output-format json --session-id {}",
            shell_quote(&config.session_id)
        );
        if let Some(model) = &config.model {
            cmd.push_str(&format!(" --model {}", shell_quote(model)));
        }
        if let Some(system) = &config.system_prompt {
            cmd.push_str(&format!(" --append-system-prompt {}", shell_quote(system)));
        }
        if !config.mcp_servers.is_empty() {
            let json = mcp_config_json(&config.mcp_servers);
            cmd.push_str(&format!(" --mcp-config {}", shell_quote(&json)));
        }
        cmd
    }
}

#[async_trait]
impl AgentProvider for ClaudeProvider {
    async fn run(&self, config: AgentRunConfig) -> Result<AgentOutput, AgentError> {
        let mut files = vec![InjectedFile {
            container_path: PROMPT_PATH.to_string(),
            contents: config.prompt.clone().into_bytes(),
            mode: 0o644,
        }];
        files.extend(config.files.iter().cloned());

        let mut env = config.env.clone();
        env.insert("CLAUDE_CONFIG_DIR".into(), crate::CLAUDE_CONFIG_DIR.into());

        let launch = ContainerLaunchConfig {
            image: config.image.clone(),
            cmd: bootstrap_cmd(&["sh".into(), "-c".into(), Self::claude_invocation(&config)]),
            env,
            files,
            cpu_limit: None,    // resource limits ride on the dispatcher's
            memory_limit: None, // command-container path; provider adds none yet
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
        }
    }

    /// Recorded verbatim from `claude -p ... --output-format json` on CLI
    /// 2.1.211 (an auth-failure run, which still emits a full result object).
    /// This is an externally-owned contract: if the CLI changes shape, this
    /// fixture is where it should break.
    const RESULT_JSON: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":401,"duration_ms":499,"duration_api_ms":0,"num_turns":1,"result":"Invalid API key","stop_reason":"stop_sequence","session_id":"6f1db8aa-e7a0-465c-ad2c-492d6ef2cd86","total_cost_usd":0,"usage":{"input_tokens":12,"cache_creation_input_tokens":34,"cache_read_input_tokens":56,"output_tokens":78,"service_tier":"standard"},"modelUsage":{},"permission_denials":[],"terminal_reason":"api_error","uuid":"c5596749-3e8f-4175-8a7a-be175d506a9e"}"#;

    #[test]
    fn invocation_composes_all_flags() {
        let cmd = ClaudeProvider::claude_invocation(&config());
        assert!(cmd.starts_with(
            "claude -p \"$(cat /chuggernaut/prompt.md)\" --dangerously-skip-permissions"
        ));
        assert!(cmd.contains("--model 'claude-sonnet-4-6'"));
        assert!(cmd.contains("--append-system-prompt 'KO facts'"));
        assert!(cmd.contains("--mcp-config"));
        assert!(cmd.contains("chuggernaut-channel"));
    }

    /// Without these two the transcript is unaddressable and usage unmeasurable
    /// — the whole point of capture.
    #[test]
    fn invocation_pins_session_and_json_output() {
        let cmd = ClaudeProvider::claude_invocation(&config());
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
        let cmd = ClaudeProvider::claude_invocation(&c);
        assert_eq!(
            cmd,
            "claude -p \"$(cat /chuggernaut/prompt.md)\" --dangerously-skip-permissions \
             --output-format json --session-id 'da08d5f3-844e-430e-8363-39b4882f437b'"
        );
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
    fn mcp_config_shape() {
        let json = mcp_config_json(&config().mcp_servers);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["mcpServers"]["chuggernaut-channel"]["command"],
            "/usr/local/bin/chuggernaut-channel"
        );
    }
}
