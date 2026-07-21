#!/bin/sh
# Project-wide CI gate (wired in jobs/_defaults.yaml). Mirrors
# .github/workflows/ci.yml. Runs inside the agent container: NATS/Docker
# integration tests self-skip there (require_nats! in test-utils).
set -eu
export CARGO_TERM_COLOR=always
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
