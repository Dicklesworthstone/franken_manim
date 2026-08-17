#!/usr/bin/env bash
# Hermetic integration checks for scripts/install.sh. No network is used.

set -euo pipefail
umask 022
cd "$(dirname "$0")/.."

TMP_BASE="${TMPDIR:-/tmp}"
TMP_BASE="${TMP_BASE%/}"
TEST_ROOT=$(mktemp -d "$TMP_BASE/fmn-install-test.XXXXXXXX")
KEEP_TEST_ROOT="${FMN_INSTALL_KEEP_TEST_ROOT:-0}"

cleanup() {
    local status=$?
    trap - EXIT
    if [[ "$KEEP_TEST_ROOT" != "1" ]]; then
        case "$TEST_ROOT" in
            "$TMP_BASE"/fmn-install-test.*) rm -rf -- "$TEST_ROOT" ;;
        esac
    else
        printf 'installer test artifacts preserved at %s\n' "$TEST_ROOT"
    fi
    exit "$status"
}
trap cleanup EXIT

fail() {
    printf 'installer test failed: %s\n' "$*" >&2
    exit 1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

expect_failure() {
    local label=$1
    shift
    if "$@" >"$TEST_ROOT/$label.stdout" 2>"$TEST_ROOT/$label.stderr"; then
        fail "$label unexpectedly succeeded"
    fi
}

bash -n scripts/install.sh
bash scripts/install.sh --help >"$TEST_ROOT/help.txt"
grep -q -- '--offline ARCHIVE' "$TEST_ROOT/help.txt" || fail "help omits offline mode"
grep -q -- '--checksum HASH|FILE' "$TEST_ROOT/help.txt" || fail "help omits checksum mode"

fixture="$TEST_ROOT/fixture"
mkdir -p "$fixture"
printf '%s\n' '#!/usr/bin/env bash' 'printf "fmn 9.8.7\\n"' >"$fixture/fmn"
chmod 0755 "$fixture/fmn"
archive="$TEST_ROOT/fmn-x86_64-unknown-linux-gnu.tar.xz"
tar -cJf "$archive" -C "$fixture" fmn
checksum=$(sha256_file "$archive")

success_dir="$TEST_ROOT/success/bin"
FMN_INSTALL_KEEP_STATE=1 bash scripts/install.sh --quiet --no-gum \
    --version 9.8.7 --install-dir "$success_dir" \
    --offline "$archive" --checksum "$checksum"
[[ -x "$success_dir/fmn" ]] || fail "offline install did not publish fmn"
[[ "$("$success_dir"/fmn --version)" == "fmn 9.8.7" ]] \
    || fail "installed binary reports the wrong version"

checksum_dir="$TEST_ROOT/checksum-failure/bin"
expect_failure checksum-mismatch env FMN_INSTALL_KEEP_STATE=1 \
    bash scripts/install.sh --quiet --no-gum --version 9.8.7 \
    --install-dir "$checksum_dir" --offline "$archive" \
    --checksum 0000000000000000000000000000000000000000000000000000000000000000
[[ ! -e "$checksum_dir/fmn" ]] || fail "checksum failure published a binary"
grep -q 'SHA-256 mismatch' "$TEST_ROOT/checksum-mismatch.stderr" \
    || fail "checksum failure was not precise"

bad_fixture="$TEST_ROOT/bad-fixture"
mkdir -p "$bad_fixture"
printf '%s\n' 'not a binary' >"$bad_fixture/not-fmn"
bad_archive="$TEST_ROOT/bad.tar.xz"
tar -cJf "$bad_archive" -C "$bad_fixture" not-fmn
bad_checksum=$(sha256_file "$bad_archive")
bad_dir="$TEST_ROOT/bad-archive/bin"
expect_failure bad-archive env FMN_INSTALL_KEEP_STATE=1 \
    bash scripts/install.sh --quiet --no-gum --version 9.8.7 \
    --install-dir "$bad_dir" --offline "$bad_archive" --checksum "$bad_checksum"
[[ ! -e "$bad_dir/fmn" ]] || fail "malformed archive published a binary"
grep -q 'exactly one top-level fmn binary' "$TEST_ROOT/bad-archive.stderr" \
    || fail "malformed archive refusal was not precise"

locked_dir="$TEST_ROOT/concurrent/bin"
mkdir -p "$locked_dir/.fmn-install.lock"
printf '%s\n' "$$" >"$locked_dir/.fmn-install.lock/pid"
expect_failure concurrent env FMN_INSTALL_KEEP_STATE=1 \
    bash scripts/install.sh --quiet --no-gum --version 9.8.7 \
    --install-dir "$locked_dir" --offline "$archive" --checksum "$checksum"
[[ ! -e "$locked_dir/fmn" ]] || fail "concurrent install published a binary"
grep -q 'another install may be active' "$TEST_ROOT/concurrent.stderr" \
    || fail "concurrent-install refusal was not precise"

printf 'installer smoke: success, checksum refusal, archive refusal, and lock refusal passed\n'
