#!/bin/sh
# Hygiene checks (acceptance test 12).
set -eu
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
