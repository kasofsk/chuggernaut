#!/usr/bin/env bash
set -euo pipefail

# Generate code coverage report using cargo-llvm-cov.
#
# Prerequisites (one-time):
#   rustup component add llvm-tools-preview
#   cargo install cargo-llvm-cov
#
# Usage:
#   bash scripts/coverage.sh              # HTML report (fast: non-ignored tests only)
#   bash scripts/coverage.sh --full       # HTML report (all tests including E2E)
#   bash scripts/coverage.sh --lcov       # LCOV for CI (non-ignored only)
#   bash scripts/coverage.sh --full --lcov

FULL=false
LCOV=false

for arg in "$@"; do
    case "$arg" in
        --full) FULL=true ;;
        --lcov) LCOV=true ;;
        *) echo "Unknown arg: $arg"; exit 1 ;;
    esac
done

# Clean previous profraw data
cargo llvm-cov clean --workspace

# Run non-ignored tests (parallel)
echo "==> Running integration tests..."
cargo llvm-cov --no-report -p chuggernaut-dispatcher --test integration

if $FULL; then
    # Run E2E tests (sequential, requires Docker + runner-env image)
    echo "==> Running E2E tests (--test-threads=1)..."
    cargo llvm-cov --no-report -p chuggernaut-dispatcher --test integration -- --ignored --test-threads=1
fi

# Generate report
if $LCOV; then
    cargo llvm-cov report --lcov --output-path target/coverage/lcov.info
    echo "LCOV report: target/coverage/lcov.info"
else
    cargo llvm-cov report --html --output-dir target/coverage
    echo "HTML report: target/coverage/html/index.html"
fi
