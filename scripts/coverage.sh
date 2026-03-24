#!/usr/bin/env bash
set -euo pipefail

# Generate code coverage report using cargo-llvm-cov.
#
# Prerequisites (one-time):
#   rustup component add llvm-tools-preview
#   cargo install cargo-llvm-cov
#
# Usage:
#   bash scripts/coverage.sh          # HTML report
#   bash scripts/coverage.sh --lcov   # LCOV for CI

if [[ "${1:-}" == "--lcov" ]]; then
    cargo llvm-cov --workspace --lcov --output-path target/coverage/lcov.info
    echo "LCOV report: target/coverage/lcov.info"
else
    cargo llvm-cov --workspace --html --output-dir target/coverage
    echo "HTML report: target/coverage/html/index.html"
fi
