#!/usr/bin/env bash
# FrankenManim native installer.
#
# Recommended invocation (the query parameter avoids stale proxy/CDN copies):
#   curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/franken_manim/main/scripts/install.sh?$(date +%s)" | bash
#
# The standalone `fmn` binary is CPython-free. This installer does not install
# the optional `franken-manim` Python wheel.

set -euo pipefail
shopt -s lastpipe 2>/dev/null || true
umask 022

REPOSITORY="Dicklesworthstone/franken_manim"
FALLBACK_VERSION="0.3.0"
QUIET=0
NO_GUM=0
FORCE=0
REQUESTED_VERSION=""
INSTALL_DIR="${FMN_INSTALL_DIR:-${HOME}/.local/bin}"
OFFLINE_ARCHIVE=""
OFFLINE_CHECKSUM=""
TEMP_DIR=""
LOCK_DIR=""
INSTALL_CANDIDATE=""
KEEP_STATE="${FMN_INSTALL_KEEP_STATE:-0}"
TMP_BASE="${TMPDIR:-/tmp}"
TMP_BASE="${TMP_BASE%/}"
OS=""
ARCH=""
ARTIFACT=""
BINARY_NAME="fmn"
ARCHIVE_KIND=""
PROXY_ARGS=()
HAS_GUM=0

if command -v gum >/dev/null 2>&1 && [[ -t 1 ]]; then
    HAS_GUM=1
fi

plain_output() {
    [[ ! -t 1 || "${NO_COLOR:-}" == "1" ]]
}

info() {
    [[ "$QUIET" -eq 1 ]] && return 0
    if [[ "$HAS_GUM" -eq 1 && "$NO_GUM" -eq 0 ]]; then
        gum style --foreground 39 "-> $*"
    elif plain_output; then
        printf '%s\n' "-> $*"
    else
        printf '\033[0;34m->\033[0m %s\n' "$*"
    fi
}

ok() {
    [[ "$QUIET" -eq 1 ]] && return 0
    if [[ "$HAS_GUM" -eq 1 && "$NO_GUM" -eq 0 ]]; then
        gum style --foreground 42 "✓ $*"
    elif plain_output; then
        printf '%s\n' "OK: $*"
    else
        printf '\033[0;32m✓\033[0m %s\n' "$*"
    fi
}

warn() {
    [[ "$QUIET" -eq 1 ]] && return 0
    if [[ "$HAS_GUM" -eq 1 && "$NO_GUM" -eq 0 ]]; then
        gum style --foreground 214 "! $*"
    elif plain_output; then
        printf '%s\n' "WARNING: $*"
    else
        printf '\033[1;33m!\033[0m %s\n' "$*"
    fi
}

err() {
    if [[ "$HAS_GUM" -eq 1 && "$NO_GUM" -eq 0 ]]; then
        gum style --foreground 196 "X $*" >&2
    elif [[ ! -t 2 || "${NO_COLOR:-}" == "1" ]]; then
        printf '%s\n' "ERROR: $*" >&2
    else
        printf '\033[0;31mX\033[0m %s\n' "$*" >&2
    fi
}

die() {
    err "$*"
    exit 1
}

draw_box() {
    [[ "$QUIET" -eq 1 ]] && return 0
    local width=0 line padding
    for line in "$@"; do
        ((${#line} > width)) && width=${#line}
    done
    printf '╔'
    repeat_character '═' $((width + 2))
    printf '╗\n'
    for line in "$@"; do
        padding=$((width - ${#line}))
        printf '║ %s' "$line"
        printf '%*s' $((padding + 1)) ''
        printf '║\n'
    done
    printf '╚'
    repeat_character '═' $((width + 2))
    printf '╝\n'
}

repeat_character() {
    local character=$1 count=$2
    while ((count > 0)); do
        printf '%s' "$character"
        count=$((count - 1))
    done
}

show_header() {
    if [[ "$HAS_GUM" -eq 1 && "$NO_GUM" -eq 0 && "$QUIET" -eq 0 ]]; then
        gum style --border normal --border-foreground 39 --padding "0 1" \
            "$(gum style --foreground 42 --bold 'FrankenManim installer')" \
            "$(gum style --foreground 245 'Checksummed native fmn binary')"
    else
        draw_box "FrankenManim installer" "Checksummed native fmn binary"
    fi
}

usage() {
    cat <<'EOF'
Usage: install.sh [OPTIONS]

Install the standalone CPython-free `fmn` binary from a FrankenManim release.
Every archive is SHA-256 verified before extraction.

Options:
  --version VERSION     Install an exact release (for example 0.3.0 or v0.3.0)
  --install-dir DIR     Destination directory (default: $HOME/.local/bin)
  --offline ARCHIVE     Install a local release archive without network access
  --checksum HASH|FILE  Required with --offline: 64-hex SHA-256 or checksum file
  --force               Reinstall even when the requested version is installed
  --quiet               Print only errors
  --no-gum              Disable optional gum presentation
  -h, --help            Show this help

Supported release archives:
  Linux x86-64   fmn-x86_64-unknown-linux-gnu.tar.xz
  macOS arm64    fmn-aarch64-apple-darwin.tar.xz
  Windows x86-64 fmn-x86_64-pc-windows-msvc.zip (MSYS2/Git Bash/Cygwin)

Linux arm64 and macOS x86-64 have no published native artifact yet. Build from
the exact source tag instead:
  git clone --branch v<VERSION> --depth 1 https://github.com/Dicklesworthstone/franken_manim
  cd franken_manim
  cargo build --release -p fmn-cli --features cli

Uninstall:
  rm "<install-dir>/fmn"   # use fmn.exe on Windows
EOF
}

cleanup() {
    local status=$?
    trap - EXIT
    if [[ "$KEEP_STATE" != "1" ]]; then
        if [[ -n "$INSTALL_CANDIDATE" && -f "$INSTALL_CANDIDATE" ]]; then
            case "$INSTALL_CANDIDATE" in
                "$INSTALL_DIR"/.fmn.install.*) rm -f -- "$INSTALL_CANDIDATE" ;;
            esac
        fi
        if [[ -n "$LOCK_DIR" && -d "$LOCK_DIR" && -f "$LOCK_DIR/pid" ]] \
            && [[ "$(cat "$LOCK_DIR/pid" 2>/dev/null || true)" == "$$" ]]; then
            rm -f -- "$LOCK_DIR/pid"
            rmdir -- "$LOCK_DIR" 2>/dev/null || true
        fi
        if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
            case "$TEMP_DIR" in
                "$TMP_BASE"/fmn-install.*) rm -rf -- "$TEMP_DIR" ;;
            esac
        fi
    fi
    exit "$status"
}

trap cleanup EXIT
trap 'exit 130' HUP INT TERM

parse_args() {
    while (($#)); do
        case "$1" in
            --version)
                (($# >= 2)) || die "--version requires a value"
                REQUESTED_VERSION=$2
                shift 2
                ;;
            --install-dir)
                (($# >= 2)) || die "--install-dir requires a value"
                INSTALL_DIR=$2
                shift 2
                ;;
            --offline)
                (($# >= 2)) || die "--offline requires an archive path"
                OFFLINE_ARCHIVE=$2
                shift 2
                ;;
            --checksum)
                (($# >= 2)) || die "--checksum requires a hash or file path"
                OFFLINE_CHECKSUM=$2
                shift 2
                ;;
            --force)
                FORCE=1
                shift
                ;;
            --quiet)
                QUIET=1
                shift
                ;;
            --no-gum)
                NO_GUM=1
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            --)
                shift
                (($# == 0)) || die "unexpected positional arguments: $*"
                ;;
            *) die "unknown option: $1 (use --help)" ;;
        esac
    done
}

setup_proxy() {
    local proxy="${HTTPS_PROXY:-${https_proxy:-${HTTP_PROXY:-${http_proxy:-}}}}"
    if [[ -n "$proxy" ]]; then
        PROXY_ARGS=(--proxy "$proxy")
    fi
}

detect_platform() {
    local uname_os uname_arch
    uname_os=$(uname -s)
    uname_arch=$(uname -m)
    case "$uname_os" in
        Linux) OS="linux" ;;
        Darwin) OS="darwin" ;;
        MINGW*|MSYS*|CYGWIN*) OS="windows" ;;
        *) die "unsupported operating system: $uname_os" ;;
    esac
    case "$uname_arch" in
        x86_64|amd64|AMD64) ARCH="x86_64" ;;
        arm64|aarch64) ARCH="aarch64" ;;
        *) die "unsupported CPU architecture: $uname_arch" ;;
    esac

    case "$OS/$ARCH" in
        linux/x86_64)
            ARTIFACT="fmn-x86_64-unknown-linux-gnu.tar.xz"
            ARCHIVE_KIND="tar.xz"
            ;;
        darwin/aarch64)
            ARTIFACT="fmn-aarch64-apple-darwin.tar.xz"
            ARCHIVE_KIND="tar.xz"
            ;;
        windows/x86_64)
            ARTIFACT="fmn-x86_64-pc-windows-msvc.zip"
            BINARY_NAME="fmn.exe"
            ARCHIVE_KIND="zip"
            ;;
        linux/aarch64)
            die "no native Linux arm64 release exists yet; use the exact-tag source-build commands shown by --help"
            ;;
        darwin/x86_64)
            die "no native macOS x86-64 release exists; use the exact-tag source-build commands shown by --help"
            ;;
        *) die "no native release exists for $OS/$ARCH" ;;
    esac

    if [[ "$OS" == "linux" && -r /proc/version ]] \
        && grep -qi microsoft /proc/version 2>/dev/null; then
        warn "WSL detected; installing the Linux x86-64 binary"
    fi
}

normalize_version() {
    local value=${1#v}
    [[ "$value" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] \
        || die "invalid release version: $1"
    printf '%s\n' "$value"
}

curl_download() {
    local url=$1 destination=$2
    curl -fsSL --retry 3 --retry-delay 1 --connect-timeout 10 --max-time 300 \
        --max-filesize 67108864 \
        --proto '=https' --tlsv1.2 ${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"} \
        -o "$destination" "$url"
}

resolve_version() {
    if [[ -n "$REQUESTED_VERSION" ]]; then
        normalize_version "$REQUESTED_VERSION"
        return
    fi
    if [[ -n "$OFFLINE_ARCHIVE" ]]; then
        die "--offline requires --version so the installed binary identity can be verified"
    fi

    local response discovered
    response=""
    if response=$(curl -fsSL --retry 2 --connect-timeout 5 --max-time 20 \
        --max-filesize 1048576 \
        --proto '=https' --tlsv1.2 ${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"} \
        "https://api.github.com/repos/${REPOSITORY}/releases?per_page=10" 2>/dev/null); then
        discovered=$(printf '%s\n' "$response" \
            | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' \
            | head -n 1)
        if [[ -n "$discovered" ]]; then
            normalize_version "$discovered"
            return
        fi
    fi
    warn "could not resolve the newest published release; using installer fallback $FALLBACK_VERSION"
    printf '%s\n' "$FALLBACK_VERSION"
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

preflight() {
    [[ "$INSTALL_DIR" == /* ]] || die "--install-dir must be an absolute path"
    [[ -n "$TMP_BASE" && "$TMP_BASE" == /* ]] || die "TMPDIR must be an absolute path"
    [[ -d "$TMP_BASE" && -w "$TMP_BASE" ]] \
        || die "temporary directory is not writable: $TMP_BASE"
    [[ "$KEEP_STATE" == "0" || "$KEEP_STATE" == "1" ]] \
        || die "FMN_INSTALL_KEEP_STATE must be 0 or 1"
    require_command install
    require_command mktemp
    require_command uname
    if [[ -z "$OFFLINE_ARCHIVE" ]]; then
        require_command curl
    else
        [[ -f "$OFFLINE_ARCHIVE" && ! -L "$OFFLINE_ARCHIVE" ]] \
            || die "offline archive is not a regular file: $OFFLINE_ARCHIVE"
        [[ "$(wc -c < "$OFFLINE_ARCHIVE")" -le 67108864 ]] \
            || die "offline archive exceeds the 64 MiB installer limit"
        [[ -n "$OFFLINE_CHECKSUM" ]] \
            || die "--offline requires --checksum; checksum verification cannot be skipped"
    fi
    case "$ARCHIVE_KIND" in
        tar.xz) require_command tar ;;
        zip) require_command unzip ;;
    esac
    if ! command -v sha256sum >/dev/null 2>&1 \
        && ! command -v shasum >/dev/null 2>&1; then
        die "sha256sum or shasum is required for release verification"
    fi

    mkdir -p -- "$INSTALL_DIR"
    [[ -d "$INSTALL_DIR" && -w "$INSTALL_DIR" ]] \
        || die "installation directory is not writable: $INSTALL_DIR"
    local available_kb
    available_kb=$(df -Pk "$INSTALL_DIR" 2>/dev/null | awk 'NR == 2 {print $4}')
    if [[ "$available_kb" =~ ^[0-9]+$ ]] && ((available_kb < 20480)); then
        die "less than 20 MiB is available at $INSTALL_DIR"
    fi
}

acquire_lock() {
    LOCK_DIR="$INSTALL_DIR/.fmn-install.lock"
    if ! mkdir -- "$LOCK_DIR" 2>/dev/null; then
        local owner="unknown"
        [[ -r "$LOCK_DIR/pid" ]] && owner=$(cat "$LOCK_DIR/pid" 2>/dev/null || true)
        die "another install may be active (lock: $LOCK_DIR, pid: $owner); inspect and remove a stale lock manually"
    fi
    printf '%s\n' "$$" > "$LOCK_DIR/pid"
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

checksum_from_file() {
    local checksum_file=$1 artifact_name=$2 matches count
    matches=$(awk -v name="$artifact_name" '
        NF == 2 {
            file = $2
            sub(/^\*/, "", file)
            if (file == name) print tolower($1)
        }
    ' "$checksum_file")
    count=$(printf '%s\n' "$matches" | awk 'NF {count++} END {print count + 0}')
    [[ "$count" -eq 1 ]] \
        || die "checksum file must contain exactly one entry for $artifact_name"
    printf '%s\n' "$matches"
}

expected_checksum() {
    local archive=$1 checksum_input=$2 artifact_name=$3 value
    if [[ "$checksum_input" =~ ^[0-9A-Fa-f]{64}$ ]]; then
        value=$(printf '%s' "$checksum_input" | tr 'A-F' 'a-f')
    elif [[ -f "$checksum_input" && ! -L "$checksum_input" ]]; then
        value=$(checksum_from_file "$checksum_input" "$artifact_name")
    else
        die "--checksum must be a 64-hex hash or a regular checksum file"
    fi
    [[ "$value" =~ ^[0-9a-f]{64}$ ]] || die "invalid SHA-256 value for $archive"
    printf '%s\n' "$value"
}

verify_checksum() {
    local archive=$1 expected=$2 actual
    actual=$(sha256_file "$archive")
    [[ "$actual" == "$expected" ]] \
        || die "SHA-256 mismatch for $(basename "$archive"): expected $expected, got $actual"
    ok "SHA-256 verified"
}

validate_archive() {
    local archive=$1 listing expected_entry
    expected_entry=$BINARY_NAME
    case "$ARCHIVE_KIND" in
        tar.xz) listing=$(tar -tJf "$archive") ;;
        zip) listing=$(unzip -Z1 "$archive") ;;
    esac
    [[ "$listing" == "$expected_entry" || "$listing" == "./$expected_entry" ]] \
        || die "archive must contain exactly one top-level $expected_entry binary"
}

extract_archive() {
    local archive=$1 destination=$2
    case "$ARCHIVE_KIND" in
        tar.xz) tar -xJf "$archive" -C "$destination" ;;
        zip) unzip -qq "$archive" -d "$destination" ;;
    esac
}

binary_version() {
    local binary=$1 output
    if command -v timeout >/dev/null 2>&1; then
        output=$(timeout 5 "$binary" --version 2>/dev/null) || return 1
    elif command -v gtimeout >/dev/null 2>&1; then
        output=$(gtimeout 5 "$binary" --version 2>/dev/null) || return 1
    else
        output=$("$binary" --version 2>/dev/null) || return 1
    fi
    printf '%s\n' "$output" | sed -n 's/^fmn \([^[:space:]]*\)$/\1/p' | head -n 1
}

already_installed() {
    local destination=$1 version=$2 installed
    [[ -x "$destination" && ! -L "$destination" ]] || return 1
    installed=$(binary_version "$destination" || true)
    [[ "$installed" == "$version" ]]
}

download_release() {
    local version=$1 archive_path=$2 checksum_path=$3 base
    base="https://github.com/${REPOSITORY}/releases/download/v${version}"
    info "Downloading $ARTIFACT"
    curl_download "$base/$ARTIFACT" "$archive_path"
    [[ "$(wc -c < "$archive_path")" -le 67108864 ]] \
        || die "downloaded archive exceeds the 64 MiB installer limit"
    info "Downloading SHA256SUMS"
    curl_download "$base/SHA256SUMS" "$checksum_path"
    [[ "$(wc -c < "$checksum_path")" -le 1048576 ]] \
        || die "SHA256SUMS exceeds the 1 MiB installer limit"
}

install_binary() {
    local source=$1 destination=$2 version=$3 candidate_version
    INSTALL_CANDIDATE=$(mktemp "$INSTALL_DIR/.fmn.install.XXXXXX")
    install -m 0755 "$source" "$INSTALL_CANDIDATE"
    candidate_version=$(binary_version "$INSTALL_CANDIDATE" || true)
    [[ "$candidate_version" == "$version" ]] \
        || die "archive binary reports version ${candidate_version:-unknown}, expected $version"
    mv -f -- "$INSTALL_CANDIDATE" "$destination"
    INSTALL_CANDIDATE=""
    ok "Installed $destination"
}

main() {
    parse_args "$@"
    setup_proxy
    show_header
    detect_platform
    local version destination archive_path checksum_path checksum_value extract_dir source_binary
    version=$(resolve_version)
    destination="$INSTALL_DIR/$BINARY_NAME"
    preflight
    acquire_lock

    if [[ "$FORCE" -eq 0 ]] && already_installed "$destination" "$version"; then
        ok "fmn $version is already installed at $destination"
        info "Use --force to reinstall it"
        return 0
    fi

    TEMP_DIR=$(mktemp -d "$TMP_BASE/fmn-install.XXXXXXXX")
    extract_dir="$TEMP_DIR/extract"
    mkdir -- "$extract_dir"
    checksum_path="$TEMP_DIR/SHA256SUMS"
    if [[ -n "$OFFLINE_ARCHIVE" ]]; then
        archive_path=$OFFLINE_ARCHIVE
        checksum_value=$(expected_checksum "$archive_path" "$OFFLINE_CHECKSUM" \
            "$(basename "$archive_path")")
    else
        archive_path="$TEMP_DIR/$ARTIFACT"
        download_release "$version" "$archive_path" "$checksum_path"
        checksum_value=$(checksum_from_file "$checksum_path" "$ARTIFACT")
    fi

    verify_checksum "$archive_path" "$checksum_value"
    validate_archive "$archive_path"
    extract_archive "$archive_path" "$extract_dir"
    source_binary="$extract_dir/$BINARY_NAME"
    [[ -f "$source_binary" && ! -L "$source_binary" ]] \
        || die "archive did not produce a regular $BINARY_NAME"
    chmod 0755 "$source_binary"
    install_binary "$source_binary" "$destination" "$version"

    local installed
    installed=$(binary_version "$destination" || true)
    [[ "$installed" == "$version" ]] || die "post-install version check failed"
    if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
        warn "$INSTALL_DIR is not on PATH; add it to your shell configuration"
    fi
    draw_box "Installed fmn $version" "$destination" \
        "Uninstall: rm \"$destination\""
}

main "$@"
