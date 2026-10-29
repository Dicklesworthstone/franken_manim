#!/usr/bin/env bash
# fm-ntp (ADR-0003 class=fuzz): the SCHEDULED coverage-guided fuzz job for
# the fmn-codec untrusted-input parsers. This is NEVER a merge gate —
# scripts/check.sh does not call it. CI invokes it from the weekly
# `fuzz-codec` job in .github/workflows/ci.yml (`schedule` events only):
#
#     cargo install cargo-fuzz --version 0.13.1
#     bash scripts/fuzz_scheduled.sh
#
# (Not --locked: cargo-fuzz 0.13.1's committed lockfile pins a rustix
# the pinned nightly refuses to compile; see the ci.yml install step.)
#
# Each target runs time-boxed. A crash (assertion failure = a budget
# violation, libFuzzer-detected panic, timeout-hang, or rss blow-out)
# fails this script loudly and drops the reproducer under fuzz/artifacts/:
# that is a FINDING — a real decoder bug to fix at the source in
# fmn-codec, never a harness to weaken.
#
# Environment knobs:
#   FMN_FUZZ_SECONDS    per-target wall-clock budget (default 60)
#   FMN_FUZZ_TIMEOUT    per-input hang timeout seconds (default 25)
#   FMN_FUZZ_RSS_MB     per-input rss cap in MiB (default 2048)
#   FMN_FUZZ_TARGET_DIR private build dir for the fallback (default fuzz/target)
#   FMN_FUZZ_TARGETS    comma- or space-separated subset of the targets
#                       (default: all)
#   FMN_FUZZ_CORPUS_ROOT  where each target's growing corpus lives (default
#                       fuzz/corpus); libFuzzer writes new units only here
#   FMN_FUZZ_SEED_ROOT  optional read-only seed root: <root>/<target>/ is
#                       passed after the corpus, so its files seed the run
#                       without being rewritten (the private TeX corpus is
#                       CC BY-NC-SA and must never land under fuzz/corpus)
#   FMN_FUZZ_MAX_LEN    libFuzzer -max_len (default: libFuzzer's own)
#   FMN_FUZZ_ARTIFACTS  crash/reproducer prefix (default fuzz/artifacts/)
#
# `-j N` caps the fallback harness build's jobs (cargo-fuzz's own `--jobs`
# means parallel fuzzing, so the primary path ignores it). rch reads it from
# the command line to size the worker it reserves:
#     rch exec --job --result-dir <dir> -- env FMN_FUZZ_SECONDS=3600 \
#         FMN_FUZZ_TARGETS=tex_layout bash scripts/fuzz_scheduled.sh -j 3
set -euo pipefail
cd "$(dirname "$0")/.."

build_jobs=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        -j)
            build_jobs=(-j "$2")
            shift 2
            ;;
        *)
            echo "==> unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

# Not SECONDS: bash counts that variable up from any value assigned to it,
# so every later "(Ns)" progress line reported a growing budget.
BUDGET_SECONDS="${FMN_FUZZ_SECONDS:-60}"
TIMEOUT="${FMN_FUZZ_TIMEOUT:-25}"
RSS_MB="${FMN_FUZZ_RSS_MB:-2048}"
FALLBACK_TARGET_DIR="${FMN_FUZZ_TARGET_DIR:-fuzz/target}"
CORPUS_ROOT="${FMN_FUZZ_CORPUS_ROOT:-fuzz/corpus}"
SEED_ROOT="${FMN_FUZZ_SEED_ROOT:-}"
ARTIFACTS="${FMN_FUZZ_ARTIFACTS:-fuzz/artifacts/}"
ALL_TARGETS=(inflate_bytes decode_png decode_jpeg decode_entry_envelope namespace_and_object_paths svg_document yaml_config tex_math tex_layout ttf_font_parse ttf_glyf_outlines ttf_cff path_boolean)
if [[ -n "${FMN_FUZZ_TARGETS:-}" ]]; then
    IFS=', ' read -r -a TARGETS <<<"${FMN_FUZZ_TARGETS}"
    for target in "${TARGETS[@]}"; do
        if [[ " ${ALL_TARGETS[*]} " != *" ${target} "* ]]; then
            echo "==> unknown fuzz target: ${target}" >&2
            exit 2
        fi
    done
else
    TARGETS=("${ALL_TARGETS[@]}")
fi

mkdir -p "${ARTIFACTS%/*}"

run_flags=(
    "-max_total_time=${BUDGET_SECONDS}"
    "-timeout=${TIMEOUT}"
    "-rss_limit_mb=${RSS_MB}"
    "-artifact_prefix=${ARTIFACTS}"
    "-print_final_stats=1"
)
if [[ -n "${FMN_FUZZ_MAX_LEN:-}" ]]; then
    run_flags+=("-max_len=${FMN_FUZZ_MAX_LEN}")
fi

# The corpus directories for one target: the growing corpus first (libFuzzer
# writes new units there), then the read-only seeds when they exist.
corpus_dirs() {
    local target="$1"
    mkdir -p "${CORPUS_ROOT}/${target}"
    printf '%s\n' "${CORPUS_ROOT}/${target}"
    if [[ -n "${SEED_ROOT}" && -d "${SEED_ROOT}/${target}" ]]; then
        printf '%s\n' "${SEED_ROOT}/${target}"
    fi
}

status=0

if cargo fuzz --version >/dev/null 2>&1; then
    # Primary path: cargo-fuzz builds with sanitizer-coverage
    # instrumentation (the coverage-guided mode this campaign is for).
    for target in "${TARGETS[@]}"; do
        echo "==> cargo fuzz run ${target} (${BUDGET_SECONDS}s)"
        mapfile -t dirs < <(corpus_dirs "${target}")
        if ! cargo fuzz run "${target}" "${dirs[@]}" -- "${run_flags[@]}"; then
            echo "==> FINDING: ${target} crashed — reproducer in fuzz/artifacts/" >&2
            status=1
        fi
    done
else
    # Fallback for machines without the cargo-fuzz subcommand (the rch
    # workers): build the harnesses with the flags cargo-fuzz 0.13 passes —
    # sanitizer-coverage instrumentation, the `fuzzing` cfg, AddressSanitizer
    # — for the host triple, so build scripts stay uninstrumented. Coverage
    # guidance and sanitizer checks match the primary path.
    host="$(rustc -vV | sed -n 's/^host: //p')"
    echo "==> cargo-fuzz not found; building instrumented harnesses for ${host}"
    RUSTFLAGS="-Cpasses=sancov-module \
-Cllvm-args=-sanitizer-coverage-level=4 \
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters \
-Cllvm-args=-sanitizer-coverage-pc-table \
-Cllvm-args=-sanitizer-coverage-trace-compares \
-Cllvm-args=-sanitizer-coverage-stack-depth \
--cfg fuzzing -Zsanitizer=address" \
        cargo build --release --manifest-path fuzz/Cargo.toml --target "${host}" \
        --target-dir "${FALLBACK_TARGET_DIR}" --bins "${build_jobs[@]}"
    for target in "${TARGETS[@]}"; do
        echo "==> ${target} (${BUDGET_SECONDS}s)"
        mapfile -t dirs < <(corpus_dirs "${target}")
        if ! "${FALLBACK_TARGET_DIR}/${host}/release/${target}" \
            "${dirs[@]}" "${run_flags[@]}"; then
            echo "==> FINDING: ${target} crashed — reproducer in fuzz/artifacts/" >&2
            status=1
        fi
    done
fi

if [[ "${status}" -ne 0 ]]; then
    echo "==> fuzz campaign found crashes; see fuzz/artifacts/ (fix at the source in fmn-codec)" >&2
    exit "${status}"
fi
echo "==> all ${#TARGETS[@]} targets clean within budget"
