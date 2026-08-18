#!/usr/bin/env bash
# Local CRAP report for the service workspace (and collectors if present).
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT/service"
cargo llvm-cov --workspace --lcov --output-path lcov.info
cargo crap --workspace --lcov lcov.info --threshold 15
if [ -d "$REPO_ROOT/collectors" ]; then
    cd "$REPO_ROOT/collectors"
    cargo llvm-cov --lcov --output-path lcov.info
    cargo crap --lcov lcov.info --threshold 15
fi
