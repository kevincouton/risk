#!/usr/bin/env bash
# Local CRAP report for the platform-templates service workspace.
# Install cargo-llvm-cov and cargo-crap first:
#   cargo install cargo-llvm-cov
#   cargo install cargo-crap
set -euo pipefail
cd "$(dirname "$0")/../service"
cargo llvm-cov --workspace --lcov --output-path lcov.info
cargo crap --workspace --lcov lcov.info --threshold 15
cd "$(dirname "$0")/../collectors"
cargo llvm-cov --lcov --output-path lcov.info
cargo crap --lcov lcov.info --threshold 15
