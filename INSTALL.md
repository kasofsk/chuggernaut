# Installing Chuggernaut — the 15-minute path

Chuggernaut is a NATS-backed job orchestrator you self-host: a **dispatcher**
drives jobs through a DAG in containers, an **api** bridges HTTP↔NATS, and a
worker fleet runs the work. This is the streamlined path to get an **existing
repo** (e.g. on GitHub) running on a fresh single host, mirroring `main` back to
GitHub. Multi-node/HA is out of scope here — see `deploy/prod/README.md`.

The fastest route is the **`/chug-install` Claude Code skill**: open this repo in
Claude Code and run `/chug-install`. It detects what already exists, asks the
model-choice question, runs the scripts below, and verifies each stage. The
scripts are equally runnable by hand — the skill just narrates them.

## The three phases

Everything is `deploy/prod/chug-install.sh` (idempotent; `--dry-run` previews
any step without changing anything). Configuration lives in
`deploy/prod/chuggernaut.env` (copy `deploy/prod/env.example`). <!-- runtime -->

```sh
# 0. Preflight — deps + config, non-destructive. Do this first.
deploy/prod/chug-install.sh preflight
#    Missing deps on macOS:  brew install colima docker node age

# 1. Platform — stand up dispatcher + api + NATS + ssh front on THIS host.
#    Composes boot.sh + `chuggernaut init` + install-launchd.sh, then health-gates.
deploy/prod/chug-install.sh --dry-run platform   # preview
deploy/prod/chug-install.sh platform             # for real

# 2. Import — bring an existing repo in as a PLATFORM-OWNED project and mirror
#    main back to GitHub (GitHub becomes a read-only mirror).
deploy/prod/chug-install.sh --dry-run project-import git@github.com:acme/widget.git
deploy/prod/chug-install.sh project-import git@github.com:acme/widget.git --owner acme --name widget
#    Then follow the printed deploy-key guidance (one out-of-band step).

# 3. Worker (optional) — provision a worker node's creds + images.
deploy/prod/chug-install.sh worker-join --node nuc --project acme/widget
```

## Two ownership models — pick one

- **Platform-owned + mirror (default, full dogfood).** The platform's bare repo
  owns `main`; GitHub is a **read-only mirror** force-pushed every 5 minutes.
  Land changes as jobs on the platform. This is how the Chuggernaut repo itself
  runs. → `project-import` (above).
- **Linked-origin.** GitHub stays the source of truth; the platform tracks it
  via `POST /api/v1/projects/link` + `CHUG_ORIGIN_*` secrets and opens
  `chug/release-*` PRs back. Choose this to keep GitHub-native PR review. →
  README §12 / §5.3.

⚠️ Under the default model, **direct pushes to GitHub `main` are overwritten** —
the platform owns `main`.

## Verify

- **Platform:** `chug-install.sh preflight` is clean and the health gate in
  `platform` passes (a live dispatcher answers `req.jobs.list`).
- **Import + mirror round trip:** create a trivial job that commits to `main`,
  let it merge, and confirm the commit appears on the GitHub mirror within
  ~5 min (or run the mirror agent once). This is the end-to-end proof.

## When something breaks

Every phase maps to a documented manual step in `deploy/prod/README.md`
(bootstrap §1, deployment §3, worker nodes §6, mirror §3). The scripts say which
underlying command they run; drop to the README section and run it by hand.
