#!/usr/bin/env bash
# The mandatory verification gate (AGENTS.md): fmt, check, clippy -D warnings,
# test, then the structural crate-DAG check — in order, stopping on first
# failure. CI wires this script rather than duplicating the commands.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo fmt --check"
cargo fmt --check

echo "==> cargo check --all-targets"
cargo check --all-targets

echo "==> cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> cargo test"
cargo test

echo "==> crate-DAG check (workspace graph vs plan §19)"
python3 scripts/check_crate_dag.py

echo "OK: all gates green"
