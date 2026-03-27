# chuggernaut

NATS-first workflow orchestration for AI agents. Jobs live in NATS KV. Workers run inside CI action containers (Forgejo Actions or GitHub Actions). A graph viewer is the primary human interface.

## Architecture

- **Dispatcher** — owns all workflow state in NATS KV, maintains an in-memory DAG (petgraph), serves an HTTP/SSE API, and dispatches CI workflows
- **Worker** — ephemeral binary that runs inside a CI action container; communicates via NATS (heartbeats, outcomes, MCP channel) and the git provider (git, PRs, review)
- **CLI** — admin interface for creating jobs, requeuing, and inspecting state

## Git Provider Support

Chuggernaut supports both **Forgejo** and **GitHub** as git providers. The provider is auto-detected from the URL at runtime, and selected via a `git_provider` Terraform variable for infrastructure provisioning.

| | Forgejo | GitHub |
|---|---|---|
| API client | `forgejo-api` crate | `github-api` crate |
| Workflow path | `.forgejo/workflows/` | `.github/workflows/` |
| Runner | `forgejo-runner` (self-hosted Docker) | `actions/runner` (self-hosted Docker) |
| Bootstrap | `scripts/dev-up.sh` | `scripts/github-setup.sh` |

## Crates

| Crate | Purpose |
|-------|---------|
| `dispatcher` | Core orchestrator, graph, HTTP API |
| `worker` | Action worker binary |
| `cli` | CLI tool |
| `types` | Shared domain types |
| `git-provider` | Provider-agnostic `GitProvider` trait |
| `forgejo-api` | Forgejo REST API client |
| `github-api` | GitHub REST API client |
| `nats` | NATS KV/stream helpers |
| `channel` | MCP channel over NATS |
| `test-utils` | Test harness utilities |

## Scripts

### `dev-up.sh`

Bootstrap a Forgejo-based dev environment (docker compose + Terraform + runners).

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

### `github-setup.sh`

Bootstrap a GitHub-based deployment. Requires pre-existing GitHub bot accounts with PATs.

```
export GITHUB_TOKEN=ghp_...
export GITHUB_WORKER_TOKEN=ghp_...
export GITHUB_REVIEWER_TOKEN=ghp_...
./scripts/github-setup.sh --owner myorg --repo myproject
```

| Flag | Description |
|------|-------------|
| `--owner org` | GitHub organization (required) |
| `--repo name` | Repo to create and configure (repeatable) |
| `--runners N` | Number of self-hosted runners per repo (default: 1) |

Requires: docker, terraform >= 1.4, gh CLI, nats CLI, curl, jq. Set `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` for worker credentials.

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

## License

MIT
