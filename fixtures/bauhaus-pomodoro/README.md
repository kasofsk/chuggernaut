# Bauhaus Pomodoro Fixture

Bootstrap files for the bauhaus-pomodoro project in chuggernaut.

## Contents

- `jobs.json` — 16-job seed fixture with dependency graph (5 phases)
- `CLAUDE.md` — Project context pushed to the Forgejo repo at bootstrap

## Usage

### 1. Bootstrap the repo

```bash
./scripts/dev-up.sh \
  --repo bp/bauhaus-pomodoro \
  --initial-file bp/bauhaus-pomodoro:CLAUDE.md=fixtures/bauhaus-pomodoro/CLAUDE.md
```

Or add to your `terraform.tfvars`:

```hcl
managed_repos = [
  {
    org  = "bp"
    repo = "bauhaus-pomodoro"
    initial_files = {
      "CLAUDE.md" = file("../../fixtures/bauhaus-pomodoro/CLAUDE.md")
    }
  }
]
```

### 2. Seed the jobs

```bash
cargo run -p chuggernaut-cli -- seed bp/bauhaus-pomodoro fixtures/bauhaus-pomodoro/jobs.json
```

### 3. Configure the dispatcher

Set `CHUGGERNAUT_RUNNER_LABEL_MAP='{"flutter":"flutter"}'` so Flutter jobs route to the Flutter runner.

## Job Graph

```
Phase 1 — Foundation
  01: Project Setup ──┬──→ 02: Design System ──┬──→ 04: App Shell + Nav ──→ ...
                      ├──→ 03: Data Models ─────┤
                      │                         ├──→ 05: Timer State ──→ ...
                      │                         └──→ 06: Task CRUD ──→ ...
                      └──→ 03 ──→ 13: Settings State ──→ ...

Phase 2 — Core Timer
  05: Timer State ──→ 08: Notifications
  06: Task CRUD

Phase 3 — Screens
  04 + 05 ──→ 07: Timer Screen
  04 + 06 ──→ 09: Tasks Screen
  05 + 06 ──→ 10: Task-Timer Integration

Phase 4 — Analytics
  03 + 05 ──→ 11: Statistics Service ──→ 12: Efficiency Report (+ 04)

Phase 5 — Settings + Polish
  13 + 04 ──→ 14: Settings Screen
  07 + 08 + 09 + 10 + 12 + 14 ──→ 15: Integration QA
  02 + 13 + 15 ──→ 16: Theme Variants
```

## Review Levels

All tickets default to AI review. No tickets require manual/human review.
