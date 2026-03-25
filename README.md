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

## Scripts

### `dev-up.sh`

Bootstrap the full dev environment (docker compose + Terraform + runners).

```
./scripts/dev-up.sh                                    # 1 runner, no repos
./scripts/dev-up.sh --runners 3                        # 3 runners
./scripts/dev-up.sh --repo acme/payments               # bootstrap a repo (repeatable)
./scripts/dev-up.sh --clean --repo test/repo           # wipe everything first
```

| Flag | Description |
|------|-------------|
| `--runners N` | Number of action runners to provision (default: 1) |
| `--repo org/name` | Repo to create and configure (repeatable) |
| `--clean` | Destroy all state (containers, Terraform, volumes) before starting |

Requires: docker, terraform >= 1.4, nats CLI, curl, jq. Set `CHUGGERNAUT_CLAUDE_CODE_OAUTH_TOKEN` or `CHUGGERNAUT_ANTHROPIC_API_KEY` for worker credentials.

### `dev-down.sh`

Tear down the dev environment. Stops compose services and removes Terraform-managed runners. Data volumes are preserved — use `docker compose down -v` to also wipe data.

```
./scripts/dev-down.sh
```

### `coverage.sh`

Generate code coverage via `cargo-llvm-cov`.

```
./scripts/coverage.sh              # HTML report (non-ignored tests)
./scripts/coverage.sh --full       # include E2E tests
./scripts/coverage.sh --lcov       # LCOV output for CI
./scripts/coverage.sh --full --lcov
```

| Flag | Description |
|------|-------------|
| `--full` | Include ignored (E2E) tests (requires Docker + runner-env image) |
| `--lcov` | Output LCOV format to `target/coverage/lcov.info` instead of HTML |

## Running

```
./scripts/dev-up.sh      # full dev environment
cargo run -p dispatcher
cargo run -p cli -- <command>
```

## License

MIT
