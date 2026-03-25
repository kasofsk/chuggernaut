# chuggernaut

NATS-first workflow orchestration for AI agents. Jobs live in NATS KV. Workers run inside Forgejo Action containers. A graph viewer is the primary human interface.

## Architecture

- **Dispatcher** — owns all workflow state in NATS KV, maintains an in-memory DAG (petgraph), serves an HTTP/SSE API, and dispatches Forgejo Actions workflows
- **Worker** — ephemeral binary that runs inside a Forgejo Action container; communicates via NATS (heartbeats, outcomes, MCP channel) and Forgejo (git, PRs, review)
- **CLI** — admin interface for creating jobs, requeuing, and inspecting state

## Crates

| Crate | Purpose |
|-------|---------|
| `dispatcher` | Core orchestrator, graph, HTTP API |
| `worker` | Action worker binary |
| `cli` | CLI tool |
| `types` | Shared domain types |
| `forgejo-api` | Forgejo REST API client |
| `nats` | NATS KV/stream helpers |
| `channel` | MCP channel over NATS |
| `test-utils` | Test harness utilities |

## Running

```
docker compose up        # NATS + Forgejo
cargo run -p dispatcher
cargo run -p cli -- <command>
```

## License

MIT
