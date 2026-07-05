//! ClaudeProvider (spec §4.3).
//!
//! CMD: `claude -p "$(cat /chuggernaut/prompt.md)" --model {model}
//!       --append-system-prompt {system_prompt} --mcp-config {json}`
//! Supports push notifications (`claude/channel` experimental capability).
//! TODO: implement over `container::ContainerBackend`.

pub struct ClaudeProvider;
