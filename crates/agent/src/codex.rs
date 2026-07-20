//! CodexProvider (spec §4.3).
//!
//! MCP config serialized to a temp file mounted at `/repo/.codex/config.toml`.
//! CMD: `codex exec "$(cat /chuggernaut/prompt.md)" --model {model}` (system
//! prompt prepended; no native flag). Polling inbox mode only.
//! TODO: implement over `container::ContainerBackend`.

pub struct CodexProvider;
