#!/usr/bin/env bash
# Local CRAP report. CI uses .cargo-crap.toml to fail on threshold/regression.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

cd "$REPO_ROOT/service"
cargo llvm-cov --workspace --lcov --output-path lcov.info
cargo crap --workspace --lcov lcov.info
cargo crap --workspace --lcov lcov.info --baseline crap_baseline.json --fail-regression || \
    echo "Warning: service CRAP regression detected"

cd "$REPO_ROOT/collectors"
cargo llvm-cov --lcov --output-path lcov.info
cargo crap --lcov lcov.info
cargo crap --lcov lcov.info --baseline crap_baseline.json --fail-regression || \
    echo "Warning: collectors CRAP regression detected"
