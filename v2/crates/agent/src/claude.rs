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
    fn claude_invocation(config: &AgentRunConfig) -> String {
        let mut cmd = format!("claude -p \"$(cat {PROMPT_PATH})\"");
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

        let launch = ContainerLaunchConfig {
            image: config.image.clone(),
            cmd: bootstrap_cmd(&["sh".into(), "-c".into(), Self::claude_invocation(&config)]),
            env: config.env.clone(),
            files,
            cpu_limit: None,    // resource limits ride on the dispatcher's
            memory_limit: None, // command-container path; provider adds none yet
        };
        let id = self.backend.launch(launch).await?;

        // The provider owns the timeout for its own container: agent tasks
        // have no dispatcher-visible container id yet, so the §3.5 scan
        // cannot kill them — enforce here instead.
        match tokio::time::timeout(config.task_timeout, self.backend.wait(&id)).await {
            Ok(exit) => Ok(AgentOutput { exit_code: exit? }),
            Err(_elapsed) => {
                let _ = self.backend.kill(&id).await;
                Err(AgentError::Provider(format!(
                    "agent container {id} exceeded task_timeout {:?}",
                    config.task_timeout
                )))
            }
        }
    }

    fn supports_push_notifications(&self) -> bool {
        true
    }
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
        }
    }

    #[test]
    fn invocation_composes_all_flags() {
        let cmd = ClaudeProvider::claude_invocation(&config());
        assert!(cmd.starts_with("claude -p \"$(cat /chuggernaut/prompt.md)\""));
        assert!(cmd.contains("--model 'claude-sonnet-4-6'"));
        assert!(cmd.contains("--append-system-prompt 'KO facts'"));
        assert!(cmd.contains("--mcp-config"));
        assert!(cmd.contains("chuggernaut-channel"));
    }

    #[test]
    fn minimal_invocation_omits_optional_flags() {
        let mut c = config();
        c.model = None;
        c.system_prompt = None;
        c.mcp_servers = vec![];
        let cmd = ClaudeProvider::claude_invocation(&c);
        assert_eq!(cmd, "claude -p \"$(cat /chuggernaut/prompt.md)\"");
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
