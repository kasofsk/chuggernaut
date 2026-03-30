# chuggernaut

Orchestrate AI coding agents across repositories with dependency-aware job graphs.

## Overview

Chuggernaut turns structured work — PRDs, tickets, task lists — into a directed acyclic graph of jobs that AI agents execute autonomously. Each job is a discrete unit of work (implement a feature, write tests, update docs) with optional dependencies on other jobs. The system handles dispatch, code review, rework cycles, and dependency resolution automatically.

**The workflow:**

1. **Decompose** — Break a project into jobs with dependencies. A ticket like "add payments support" becomes a graph: `define schema → implement API → write tests → update docs`, where each arrow is a dependency.

2. **Seed** — Load the job graph via the CLI (`chuggernaut seed` from a fixture or `chuggernaut create` one at a time). Jobs with unmet dependencies start **Blocked**; jobs ready to go land **OnDeck**.

3. **Watch it run** — The dispatcher picks up OnDeck jobs, spins up CI containers with AI agents (Claude), and moves jobs through the pipeline: **OnTheStack** (agent working) → **InReview** (review agent checking the PR) → **Done** (merged). Failed reviews trigger automatic rework cycles.

4. **Steer from the graph** — The graph viewer is the primary human interface. It shows the live DAG with color-coded nodes, real-time status from running agents, PR links, rework counts, and token usage. Escalated or stuck jobs surface visually for human intervention.

The result: you define *what* needs to happen and *in what order*, and chuggernaut coordinates the agents to make it happen — one PR per job, reviewed and merged automatically where possible, escalated to you when not.

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

## Future work

### Continuous project management

The initial seed is just the starting point. After the first wave of jobs completes, new work arrives continuously — from humans via the CLI, or from **job factories** that create jobs in response to external events.

A job factory is any process that can write jobs to NATS KV. It doesn't know about the dispatcher, and the dispatcher doesn't know about it. Factories only need to understand the job schema. Examples:

- **Feature factory** — watches an issue tracker; when an issue is labeled `auto`, decomposes it into a job graph and writes the jobs to NATS
- **Incident factory** — listens for deployment failure alerts, creates "investigate and fix" jobs targeting the affected repo
- **Analytics factory** — processes user analytics to identify UX pain points, creates improvement jobs prioritised by impact
- **Follow-up factory** — triggered when a job completes, creates downstream work (update docs, cut a release, notify stakeholders)

Factories can be anything — a script on a cron, a webhook handler, a Lambda — as long as they write valid jobs with the right keys and optional dependencies. The dispatcher watches NATS KV for new jobs regardless of origin and handles dispatch, review, and dependency resolution the same way it always does. The graph view shows everything in one place.

This keeps the system open-ended: the dispatcher is a fixed coordination layer, and the surface area for *what work gets done* is defined entirely by whatever factories you connect.

## License

MIT
