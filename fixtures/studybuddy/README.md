# StudyBuddy Fixture

Bootstrap files for the studybuddy project in chuggernaut.

## Contents

- `jobs.json` — 26-job seed fixture with dependency graph (5 phases)
- `CLAUDE.md` — Project context pushed to the Forgejo repo at bootstrap
- `terraform.tfvars.example` — Example terraform config for repo bootstrap

## Usage

### 1. Bootstrap the repo

```bash
./scripts/dev-up.sh \
  --repo sb/studybuddy \
  --initial-file sb/studybuddy:CLAUDE.md=fixtures/studybuddy/CLAUDE.md
```

Or add to your `terraform.tfvars`:

```hcl
managed_repos = [
  {
    org  = "sb"
    repo = "studybuddy"
    initial_files = {
      "CLAUDE.md" = file("../../fixtures/studybuddy/CLAUDE.md")
    }
  }
]
```

### 2. Seed the jobs

```bash
cargo run -p chuggernaut-cli -- seed sb/studybuddy fixtures/studybuddy/jobs.json
```

### 3. Configure the dispatcher

Set `CHUGGERNAUT_RUNNER_LABEL_MAP='{"flutter":"flutter"}'` so Flutter jobs route to the Flutter runner.

## Review Levels

- Tickets 1-23, 25: `review: "high"` (AI-reviewable)
- Tickets 24, 26: `review: "human"` (need Apple/Play Store manual actions)
