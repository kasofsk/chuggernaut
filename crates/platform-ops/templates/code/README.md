# Chuggernaut project layout

This repository was seeded by chuggernaut. Everything the platform runs for
this project is defined *here*, versioned with the code it operates on:

- `.chug/jobs/*.yaml` — **job types**: what a job's work task runs, which
  evaluation tasks judge it, budgets, wrap-up mode. `.chug/jobs/code.yaml` is the
  starter: an agent implements the job ticket, a second agent reviews it.
- `.chug/jobs/_defaults.yaml` — (optional) evaluators appended to *every* job type,
  e.g. a project-wide CI gate.
- `.chug/tasks/` — **reusable tasks**. A command task is a script
  (`.chug/tasks/ci.sh`); an agent task is a markdown instructions file
  (`.chug/tasks/review-code.md`). Job types reference them by path via `run:` /
  `prompt:`.
- `.chug/prompts/` — work prompts for job types.
- `.chug/tags/*.md` — (optional) **knowledge tags**: `.chug/tags/backend.md` defines what
  the `backend` tag means; tag a job at creation to attach it.

Validate any of it offline with `chuggernaut validate .chug/jobs/*.yaml`, and get
in-editor validation via `chuggernaut schema job-type > .chug/jobs/.schema.json`
plus a `# yaml-language-server: $schema=.schema.json` modeline.
