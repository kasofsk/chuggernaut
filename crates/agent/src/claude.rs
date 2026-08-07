//! ClaudeProvider (spec §4.3).
//!
//! CMD (inside the workspace bootstrap): `claude -p "$(cat /chuggernaut/prompt.md)"
//! --output-format stream-json --verbose --model {model}
//! --append-system-prompt {system_prompt}
//! --settings /chuggernaut/agent-settings.json
//! --mcp-config /chuggernaut/mcp-config.json`.
//! Prompt content is injected at `/chuggernaut/prompt.md` — never a path, never
//! inline in the shell command. The `--mcp-config` payload carries NATS
//! credentials (the channel MCP server's `NATS_CREDS` env), so it is injected
//! as a mode-0600 file rather than passed inline in argv: argv leaks into `ps`,
//! `/proc/*/cmdline`, and crash reports (spec §4.3). Supports push notifications
//! (`claude/channel` experimental capability).

use crate::{
    AgentError, AgentOutput, AgentProvider, AgentRunConfig, LaunchReporter, McpServerConfig,
    PROMPT_PATH, PermissionProfile, SETTINGS_PATH,
};
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
    /// `--settings` carries the run's [`PermissionProfile`], replacing the
    /// blanket `--dangerously-skip-permissions` this used to pass. Headless
    /// agents still cannot answer permission prompts — but they do not need to:
    /// an unmatched tool call is *denied* and reported back to the model, which
    /// keeps working. So the allow list is a real control rather than advice,
    /// which is what lets an evaluator be read-only. The container remains the
    /// security boundary (spec §4.3); this is about what the agent should spend
    /// its turn on, not about containing an adversary.
    ///
    /// `--session-id` makes the transcript filename deterministic so it can be
    /// harvested after exit. `--output-format stream-json` (which `-p` requires
    /// be paired with `--verbose`) makes stdout a stream of JSONL events —
    /// assistant text, tool_use, and a final `type:"result"` object — emitted
    /// AS the agent works, so the live log viewer has something to show for the
    /// whole run instead of silence until exit. The final result event carries
    /// the same real `usage`/`result`/`session_id` fields the single-object
    /// `json` format did, so result parsing is unchanged. This is the CLI's
    /// documented interface, as opposed to the transcript, whose format is
    /// internal and version-unstable.
    ///
    /// The MCP config is written to [`MCP_CONFIG_PATH`] (mode 0600) and passed
    /// by path: the CLI accepts a file for `--mcp-config`, and the payload
    /// carries the NATS credential, which must never enter argv.
    fn claude_invocation(config: &AgentRunConfig) -> Invocation {
        let mut command = format!(
            "claude -p \"$(cat {PROMPT_PATH})\" --settings {SETTINGS_PATH} \
             --output-format stream-json --verbose --session-id {}",
            shell_quote(&config.session_id)
        );
        if let Some(model) = &config.model {
            command.push_str(&format!(" --model {}", shell_quote(model)));
        }
        if let Some(system) = &config.system_prompt {
            command.push_str(&format!(" --append-system-prompt {}", shell_quote(system)));
        }
        let mut files = vec![InjectedFile {
            container_path: SETTINGS_PATH.to_string(),
            contents: settings_json(config.permissions).into_bytes(),
            mode: 0o644,
            artifact: None,
        }];
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
    async fn run(
        &self,
        config: AgentRunConfig,
        on_launch: LaunchReporter,
    ) -> Result<AgentOutput, AgentError> {
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
            cmd: bootstrap_cmd(
                &["sh".into(), "-c".into(), invocation.command],
                config.runtime_env.as_deref(),
            ),
            env,
            files,
            cpu_limit: None,
            memory_limit: None,
            node: config.node.clone(),
            runtime_env: config.runtime_env.clone(),
        };
        let id = self.backend.launch(launch).await?;
        on_launch.report(&id);
        let out = |exit_code| AgentOutput {
            exit_code,
            container_id: Some(id.clone()),
            session_id: Some(config.session_id.clone()),
        };

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

/// The CLI's result object, the one documented interface to a finished run —
/// unlike the session transcript, whose format Anthropic documents as internal
/// and version-unstable. Every reader below goes through here, so the shape is
/// known in one place if the CLI's output changes.
///
/// Under `--output-format stream-json` the result object is the final
/// `type:"result"` event, and stdout is a stream of JSONL events preceding it
/// (assistant text, tool_use). Scanning for the last line that parses picks it
/// out regardless — and also skips anything the container printed before it (the
/// workspace clone, npm noise). Returns the owned value, not a borrow: the
/// lossy UTF-8 conversion it parses from does not outlive the call.
fn parse_result_object(stdout: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .rev()
        .find_map(|line| serde_json::from_str(line.trim()).ok())
}

/// Pull real token usage out of the CLI's result object.
///
/// Returns `None` rather than erroring: usage is reporting, and must never fail
/// a task that otherwise succeeded.
pub fn parse_usage(stdout: &[u8]) -> Option<types::TokenUsage> {
    let value = parse_result_object(stdout)?;
    let usage = value.get("usage")?;
    let n = |key: &str| usage.get(key).and_then(|v| v.as_u64());
    Some(types::TokenUsage {
        input_tokens: n("input_tokens").unwrap_or(0),
        output_tokens: n("output_tokens").unwrap_or(0),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
    })
}

/// Pull the agent's final result text out of the CLI's result object — its
/// `result` field. Used to capture a **triage assessment** (spec §1.2), which
/// runs without the channel MCP: there is no `submit_result` call, so the CLI's
/// own JSON result on stdout is the only channel for the written output.
///
/// Returns `None` when stdout carries no result object or the result is empty.
pub fn parse_result(stdout: &[u8]) -> Option<String> {
    parse_result_object(stdout)?
        .get("result")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// Tool calls the CLI refused under the run's [`PermissionProfile`], as compact
/// one-line summaries — the result object's `permission_denials` array.
///
/// This is the feedback loop for the permission profiles: a policy that is too
/// tight degrades an agent *quietly* (it is told "denied", shrugs, and carries
/// on with less), so without surfacing denials the only symptom is worse work
/// for reasons nobody can see. A reviewer silently unable to call `submit_eval`
/// is indistinguishable from a reviewer that just never reached a verdict.
///
/// Same tolerance as [`parse_usage`]: this is reporting, so a missing or
/// unrecognised field yields fewer entries, never an error.
pub fn parse_permission_denials(stdout: &[u8]) -> Vec<String> {
    let Some(value) = parse_result_object(stdout) else {
        return vec![];
    };
    let Some(denials) = value.get("permission_denials").and_then(|v| v.as_array()) else {
        return vec![];
    };
    denials
        .iter()
        .map(|d| {
            let tool = d
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match d
                .get("tool_input")
                .and_then(|i| i.get("command"))
                .and_then(|v| v.as_str())
            {
                Some(cmd) => format!("{tool}: {}", truncate(cmd, 120)),
                None => tool.to_string(),
            }
        })
        .collect()
}

/// Trim to `max` chars on a char boundary, marking that it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
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

/// The `--settings` payload for a [`PermissionProfile`] (spec §4.3).
///
/// Deny beats allow in the CLI's resolution, but the **allow list is the real
/// control** here: `Review` never allows a bare `Bash`, so a command that
/// matches no allowed prefix is denied outright. That closes the escapes a
/// denylist alone leaves open — `sh -c "cargo test"`, `make`, an `&&` chain
/// behind a permitted prefix. The explicit denies are belt-and-braces on the
/// two tools this exists to stop, and they document the intent at the point
/// someone would go looking for it.
///
/// `mcp__chuggernaut-channel` must be allowed in both profiles: it is how a run
/// reports (`update_status`) and how it terminates meaningfully
/// (`submit_result` / `submit_eval`, spec §4.2). A reviewer that cannot call
/// `submit_eval` looks exactly like a broken reviewer.
pub fn settings_json(profile: PermissionProfile) -> String {
    let permissions = match profile {
        PermissionProfile::Work => serde_json::json!({
            "defaultMode": "acceptEdits",
            "allow": [
                "Bash",
                "Edit",
                "Write",
                "Read",
                "Glob",
                "Grep",
                "NotebookEdit",
                "TodoWrite",
                "Task",
                "WebFetch",
                "WebSearch",
                "mcp__chuggernaut-channel",
            ],
        }),
        PermissionProfile::Review => serde_json::json!({
            "defaultMode": "default",
            "allow": [
                "Read",
                "Glob",
                "Grep",
                "TodoWrite",
                "mcp__chuggernaut-channel",
                "Bash(git diff:*)",
                "Bash(git log:*)",
                "Bash(git show:*)",
                "Bash(git status:*)",
                "Bash(git branch:*)",
                "Bash(git fetch:*)",
                "Bash(git merge-base:*)",
                "Bash(git rev-parse:*)",
                "Bash(git blame:*)",
                "Bash(git grep:*)",
                "Bash(rg:*)",
                "Bash(ls:*)",
                "Bash(cat:*)",
                "Bash(head:*)",
                "Bash(tail:*)",
                "Bash(wc:*)",
            ],
            "deny": [
                "Bash(cargo:*)",
                "Bash(npm:*)",
                "Bash(npx:*)",
                "Bash(make:*)",
                "Edit",
                "Write",
                "NotebookEdit",
            ],
        }),
    };
    serde_json::json!({ "permissions": permissions }).to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn config() -> AgentRunConfig {
        AgentRunConfig {
            image: Some("img:latest".into()),
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
            permissions: PermissionProfile::Work,
            runtime_env: None,
        }
    }

    /// Recorded verbatim from `claude -p ... --output-format json` on CLI
    /// 2.1.211 (an auth-failure run, which still emits a full result object).
    /// This is an externally-owned contract: if the CLI changes shape, this
    /// fixture is where it should break.
    const RESULT_JSON: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":401,"duration_ms":499,"duration_api_ms":0,"num_turns":1,"result":"Invalid API key","stop_reason":"stop_sequence","session_id":"6f1db8aa-e7a0-465c-ad2c-492d6ef2cd86","total_cost_usd":0,"usage":{"input_tokens":12,"cache_creation_input_tokens":34,"cache_read_input_tokens":56,"output_tokens":78,"service_tier":"standard"},"modelUsage":{},"permission_denials":[],"terminal_reason":"api_error","uuid":"c5596749-3e8f-4175-8a7a-be175d506a9e"}"#;

    /// Recorded verbatim from `claude -p "say hi in exactly one word"
    /// --output-format stream-json --verbose` on CLI 2.1.217 — the format #103
    /// switches to. JSONL: a `system`/`init` event, a streamed `assistant`
    /// event, a `rate_limit_event`, then the final `type:"result"` event that
    /// carries the same `usage`/`result`/`session_id` fields the single-object
    /// `json` format did. This is an externally-owned contract: result parsing
    /// scans for the last parseable line, which must be this result event.
    const STREAM_JSON: &str = concat!(
        r#"{"type":"system","subtype":"init","cwd":"/private/tmp","session_id":"4ceaa089-9e75-4c30-9d96-261158e741be","tools":["Bash","Read","Write"],"mcp_servers":[],"model":"claude-fable-5","permissionMode":"default","uuid":"c1b2eba6-41ef-4a3a-a8a4-d68a397306b6"}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-fable-5","id":"msg_011CdHF3Lk5Wr3aQA8UnvPRU","type":"message","role":"assistant","content":[{"type":"text","text":"Hi"}],"stop_reason":null,"usage":{"input_tokens":2,"cache_creation_input_tokens":7882,"cache_read_input_tokens":15109,"output_tokens":5,"service_tier":"standard"}},"parent_tool_use_id":null,"session_id":"4ceaa089-9e75-4c30-9d96-261158e741be","uuid":"0c27a503-1617-42ac-af5d-f77b15d098ab"}"#,
        "\n",
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1784740200,"rateLimitType":"five_hour"},"uuid":"ce57daaa-cdf8-4518-8fd9-a7808e20cc10","session_id":"4ceaa089-9e75-4c30-9d96-261158e741be"}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":false,"api_error_status":null,"duration_ms":1988,"duration_api_ms":1883,"num_turns":1,"result":"Hi","stop_reason":"end_turn","session_id":"4ceaa089-9e75-4c30-9d96-261158e741be","total_cost_usd":0.173019,"usage":{"input_tokens":2,"cache_creation_input_tokens":7882,"cache_read_input_tokens":15109,"output_tokens":5,"service_tier":"standard"},"permission_denials":[],"terminal_reason":"completed","uuid":"d1148bcb-66a2-4c19-8bc8-54e3d7fb392a"}"#,
    );

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
            "claude -p \"$(cat /chuggernaut/prompt.md)\" --settings /chuggernaut/agent-settings.json"
        ));
        assert!(inv.command.contains("--model 'claude-sonnet-4-6'"));
        assert!(inv.command.contains("--append-system-prompt 'KO facts'"));
        assert!(
            inv.command
                .contains("--mcp-config /chuggernaut/mcp-config.json")
        );
        assert!(!inv.command.contains("chuggernaut-channel"));
    }

    /// Without the session id the transcript is unaddressable; without
    /// stream-json (+ its mandatory `--verbose`) stdout stays silent until exit
    /// and the live log viewer has nothing to show — the whole point of #103.
    #[test]
    fn invocation_pins_session_and_streaming_output() {
        let cmd = ClaudeProvider::claude_invocation(&config()).command;
        assert!(
            cmd.contains("--session-id 'da08d5f3-844e-430e-8363-39b4882f437b'"),
            "{cmd}"
        );
        assert!(cmd.contains("--output-format stream-json"), "{cmd}");
        assert!(cmd.contains("--verbose"), "{cmd}");
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
            "claude -p \"$(cat /chuggernaut/prompt.md)\" --settings /chuggernaut/agent-settings.json \
             --output-format stream-json --verbose \
             --session-id 'da08d5f3-844e-430e-8363-39b4882f437b'"
        );
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.files[0].container_path, SETTINGS_PATH);
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
        let contents = String::from_utf8(file.contents.clone()).unwrap();
        assert!(
            contents.contains("SUAGC3DCT7DHY6TQKEPNXKVHTHULNVR7KE5G6QYWQ2Q4JW3AB2LG5UGXNU"),
            "creds must ride in the file"
        );
        assert!(contents.contains("NATS USER JWT"));
        assert!(file.artifact.is_none());
    }

    /// Parse the injected settings payload for a profile.
    fn settings_for(profile: PermissionProfile) -> serde_json::Value {
        let mut c = config();
        c.permissions = profile;
        let inv = ClaudeProvider::claude_invocation(&c);
        let file = inv
            .files
            .iter()
            .find(|f| f.container_path == SETTINGS_PATH)
            .expect("settings file injected");
        assert_eq!(file.mode, 0o644);
        assert!(file.artifact.is_none());
        serde_json::from_slice(&file.contents).expect("settings are valid JSON")
    }

    fn allow_list(profile: PermissionProfile) -> Vec<String> {
        settings_for(profile)["permissions"]["allow"]
            .as_array()
            .expect("allow list")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    /// The point of the whole change: an evaluator cannot build. Note this is
    /// carried by the ABSENCE of a bare `Bash` allow, not by the deny list —
    /// a denylist alone is defeated by `sh -c "cargo test"`.
    #[test]
    fn review_profile_cannot_reach_build_tooling() {
        let allow = allow_list(PermissionProfile::Review);
        assert!(
            !allow.iter().any(|r| r == "Bash"),
            "a bare Bash allow would let any command through: {allow:?}"
        );
        for rule in &allow {
            assert!(
                !rule.starts_with("Bash(cargo") && !rule.starts_with("Bash(npm"),
                "build tooling must not be allow-listed: {rule}"
            );
        }
        let settings = settings_for(PermissionProfile::Review);
        let deny: Vec<&str> = settings["permissions"]["deny"]
            .as_array()
            .expect("deny list")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for rule in ["Bash(cargo:*)", "Bash(npm:*)", "Bash(npx:*)"] {
            assert!(deny.contains(&rule), "{rule} must be denied: {deny:?}");
        }
    }

    /// A reviewer that cannot publish a verdict is indistinguishable from a
    /// broken reviewer — and it must still be able to read the diff it judges.
    #[test]
    fn review_profile_can_report_and_read_the_diff() {
        let allow = allow_list(PermissionProfile::Review);
        for rule in [
            "mcp__chuggernaut-channel",
            "Read",
            "Grep",
            "Bash(git diff:*)",
            "Bash(git log:*)",
            "Bash(git fetch:*)",
            "Bash(git merge-base:*)",
        ] {
            assert!(
                allow.iter().any(|r| r == rule),
                "{rule} must be allowed: {allow:?}"
            );
        }
    }

    /// Work is deliberately as permissive as the `--dangerously-skip-permissions`
    /// this replaced: the change makes policy expressible, it does not newly
    /// restrict authors. A regression here breaks every job.
    #[test]
    fn work_profile_stays_permissive() {
        let allow = allow_list(PermissionProfile::Work);
        for rule in ["Bash", "Edit", "Write", "mcp__chuggernaut-channel"] {
            assert!(
                allow.iter().any(|r| r == rule),
                "{rule} must be allowed for work: {allow:?}"
            );
        }
        assert!(
            settings_for(PermissionProfile::Work)["permissions"]
                .get("deny")
                .is_none(),
            "work denies nothing"
        );
    }

    /// The recorded fixtures carry `permission_denials: []` — the empty case
    /// must stay quiet rather than reporting a phantom denial.
    #[test]
    fn no_denials_reported_when_the_run_hit_none() {
        assert!(parse_permission_denials(RESULT_JSON.as_bytes()).is_empty());
        assert!(parse_permission_denials(STREAM_JSON.as_bytes()).is_empty());
        assert!(parse_permission_denials(b"not json at all").is_empty());
    }

    /// A denied Bash call must surface the *command*: that is what tells an
    /// operator whether the profile is too tight or the prompt asked wrongly.
    #[test]
    fn denials_report_the_denied_command() {
        let line = r#"{"type":"result","subtype":"success","result":"done","permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_1","tool_input":{"command":"cargo test --workspace","description":"run tests"}},{"tool_name":"Write","tool_use_id":"toolu_2","tool_input":{"file_path":"/workspace/x"}}]}"#;
        assert_eq!(
            parse_permission_denials(line.as_bytes()),
            vec![
                "Bash: cargo test --workspace".to_string(),
                "Write".to_string()
            ]
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

    /// #103: under `stream-json` stdout is a stream of JSONL events, and the
    /// final `type:"result"` event carries the same usage the single-object
    /// `json` format did. The last-parseable-line scan must land on it — not on
    /// an earlier `assistant` event, which also carries a (partial) `usage`.
    #[test]
    fn parses_usage_from_the_stream_json_result_event() {
        let usage = parse_usage(STREAM_JSON.as_bytes()).expect("usage");
        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_write_tokens, Some(7882));
        assert_eq!(usage.cache_read_tokens, Some(15109));
    }

    /// #103: the streamed transcript's final event carries the `result` text,
    /// just as the single-object `json` format did — triage keeps working.
    #[test]
    fn parses_result_from_the_stream_json_result_event() {
        assert_eq!(parse_result(STREAM_JSON.as_bytes()).as_deref(), Some("Hi"));
    }

    /// The whole streamed transcript rides in stdout, so parsing must still land
    /// on the result event after leading container noise precedes the stream.
    #[test]
    fn parses_stream_json_past_leading_container_noise() {
        let mut stdout = b"Cloning into '/workspace'...\nnpm notice\n".to_vec();
        stdout.extend_from_slice(STREAM_JSON.as_bytes());
        assert_eq!(parse_usage(&stdout).unwrap().input_tokens, 2);
        assert_eq!(parse_result(&stdout).as_deref(), Some("Hi"));
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
        let json = r#"{"type":"result","subtype":"success","is_error":false,"result":"The work task failed because the migration script referenced a dropped column. Recommend Revoke.","session_id":"x","usage":{"input_tokens":1,"output_tokens":2}}"#;
        assert_eq!(
            parse_result(json.as_bytes()).as_deref(),
            Some(
                "The work task failed because the migration script referenced a dropped column. Recommend Revoke."
            )
        );
        let mut stdout = b"Cloning into '/workspace'...\n".to_vec();
        stdout.extend_from_slice(json.as_bytes());
        assert!(parse_result(&stdout).is_some());
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
