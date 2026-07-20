# Chuggernaut — repo orientation

This repository holds two systems. Know which one you are touching before you edit.

- **`v2/`** — the **active** workspace. All current development happens here. It has
  its own Cargo workspace (`v2/Cargo.toml`), its own web app (`v2/web`), and its own
  `CLAUDE.md` with the real conventions. Start there.
- **repo root** (`crates/`, `action/`, `DESIGN.md`, `README.md`, `docker-compose.yml`,
  the `Dockerfile*`s) — the **legacy v1** system. Kept running and intact during the v2
  build-out. Do not modify v1 code unless a task explicitly targets v1. When v2 replaces
  v1, the v2 workspace moves to the root and these v1 crates are deleted.

If a request says "the app", "the platform", or "the UI" without qualification, it means
**v2**. See `v2/CLAUDE.md` and `v2/web/CLAUDE.md`.
