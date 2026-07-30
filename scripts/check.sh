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

echo "==> cargo check -p fmn-cli --features batch --all-targets"
cargo check -p fmn-cli --features batch --all-targets

echo "==> cargo check -p fmn-output --features ffmpeg-test-fixture --all-targets"
cargo check -p fmn-output --features ffmpeg-test-fixture --all-targets

echo "==> cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> cargo clippy -p fmn-cli --features batch --all-targets -- -D warnings"
cargo clippy -p fmn-cli --features batch --all-targets -- -D warnings

echo "==> cargo clippy -p fmn-output --features ffmpeg-test-fixture --all-targets -- -D warnings"
cargo clippy -p fmn-output --features ffmpeg-test-fixture --all-targets -- -D warnings

if [[ "${FMN_SIMD_TIER_GATE:-0}" == "1" ]]; then
    # The weekly x86-64-v4 artifact runs under user-mode emulation. Compile and
    # lint the whole workspace above, then execute the crates that own SIMD
    # kernels plus the certified tier-equivalence corpus. Unrelated subprocess
    # suites would escape the Cargo target runner and execute on the host ISA.
    echo "==> cargo test (SIMD-owning crates)"
    cargo test -p fmn-render -p fmn-anim
    echo "==> cargo test (certified tier corpus)"
    cargo test -p fmn-conformance --test certified_engine
else
    echo "==> cargo test"
    cargo test
    echo "==> cargo test -p fmn-cli --features batch --all-targets"
    cargo test -p fmn-cli --features batch --all-targets
    echo "==> cargo test -p fmn-output --features ffmpeg-test-fixture --all-targets"
    cargo test -p fmn-output --features ffmpeg-test-fixture --all-targets
fi

echo "==> crate-DAG check (workspace graph vs plan §19)"
python3 scripts/check_crate_dag.py

echo "OK: all gates green"
