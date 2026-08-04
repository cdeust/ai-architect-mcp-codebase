#!/usr/bin/env bash
# Ensure the Claude plugin has the canonical ai-architect-mcp-codebase binary.
#
# Marketplace cache (CLAUDE_PLUGIN_ROOT set, no .git): download the release
# archive, verify its SHA-256 and offline Sigstore bundle against the fixed
# producer/workflow identity, then install it atomically. Persist the verified
# digest and re-check it on every launch.
#
# Source checkout (.git present, or no CLAUDE_PLUGIN_ROOT): never substitute a
# release binary for local source. Rebuild when sources are newer than target/.

set -euo pipefail

EXPECTED_REPO="cdeust/ai-architect-mcp-codebase"
EXPECTED_REPOSITORY_URL="https://github.com/${EXPECTED_REPO}"
EXPECTED_SIGNER_WORKFLOW="${EXPECTED_REPO}/.github/workflows/release.yml"

ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
BIN="$ROOT/target/release/ai-architect-mcp-codebase"
DIGEST_FILE="$BIN.sha256"
MANIFEST="$ROOT/Cargo.toml"
PLUGIN_MANIFEST="$ROOT/.claude-plugin/plugin.json"
SRC_DIR="$ROOT/src"
MODE="${1:-quiet}"

log() {
    if [ "$MODE" = "verbose" ]; then
        echo "ai-architect-mcp-codebase: $*" >&2
    fi
}

err() {
    echo "ai-architect-mcp-codebase: $*" >&2
}

fatal() {
    err "FATAL: $*"
    exit 1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fatal "neither sha256sum nor shasum is available"
    fi
}

download() {
    # 5 s connect bound; abort a stalled transfer below 1 KiB/s for 30 s.
    # The 120 s total permits the current ~10 MiB asset on slower links while
    # remaining bounded inside an actionable first-start failure.
    curl --fail --location --silent --show-error \
        --connect-timeout 5 --max-time 120 --speed-limit 1024 --speed-time 30 \
        --retry 2 --retry-connrefused "$1" -o "$2"
}

verify_provenance() {
    artifact=$1
    bundle=$2
    if command -v timeout >/dev/null 2>&1; then
        timeout 20 gh attestation verify "$artifact" \
            --repo "$EXPECTED_REPO" --signer-workflow "$EXPECTED_SIGNER_WORKFLOW" \
            --bundle "$bundle" >&2
    elif command -v gtimeout >/dev/null 2>&1; then
        gtimeout 20 gh attestation verify "$artifact" \
            --repo "$EXPECTED_REPO" --signer-workflow "$EXPECTED_SIGNER_WORKFLOW" \
            --bundle "$bundle" >&2
    elif command -v perl >/dev/null 2>&1; then
        # alarm survives exec, providing a portable macOS bound without GNU coreutils.
        perl -e '$seconds=shift @ARGV; alarm $seconds; exec @ARGV' 20 \
            gh attestation verify "$artifact" \
            --repo "$EXPECTED_REPO" --signer-workflow "$EXPECTED_SIGNER_WORKFLOW" \
            --bundle "$bundle" >&2
    else
        fatal "timeout, gtimeout, or perl is required to bound provenance verification"
    fi
}

[ -f "$MANIFEST" ] || fatal "Cargo.toml missing at $MANIFEST"
[ -f "$PLUGIN_MANIFEST" ] || fatal "plugin.json missing at $PLUGIN_MANIFEST"

marketplace_install="no"
if [ -n "${CLAUDE_PLUGIN_ROOT:-}" ] && [ ! -d "$ROOT/.git" ]; then
    marketplace_install="yes"
fi

if [ "$marketplace_install" = "yes" ]; then
    manifest_repository=$(sed -n 's/^[[:space:]]*"repository"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PLUGIN_MANIFEST" | head -1)
    [ "$manifest_repository" = "$EXPECTED_REPOSITORY_URL" ] \
        || fatal "plugin repository is not the trusted producer $EXPECTED_REPOSITORY_URL"

    if [ -x "$BIN" ] && [ -f "$DIGEST_FILE" ]; then
        expected=$(awk '{print $1}' "$DIGEST_FILE")
        actual=$(sha256_file "$BIN")
        [ -n "$expected" ] && [ "$actual" = "$expected" ] \
            || fatal "cached binary digest mismatch; reinstall the plugin"
        log "cached binary SHA-256 verified at $BIN"
        exit 0
    fi

    command -v curl >/dev/null 2>&1 || fatal "curl is required for marketplace installation"
    command -v gh >/dev/null 2>&1 \
        || fatal "GitHub CLI 2.49+ is required to verify release provenance: https://cli.github.com/"
    gh attestation verify --help >/dev/null 2>&1 \
        || fatal "GitHub CLI lacks 'gh attestation verify'; upgrade to version 2.49 or newer"

    version=$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PLUGIN_MANIFEST" | head -1)
    [ -n "$version" ] || fatal "plugin version missing"
    case "$(uname -s)-$(uname -m)" in
        Darwin-arm64) release_target="macos-aarch64" ;;
        Linux-x86_64) release_target="linux-x86_64" ;;
        Linux-aarch64) release_target="linux-aarch64" ;;
        *)
            fatal "unsupported plugin platform $(uname -s)/$(uname -m); supported: macOS arm64, Linux x86_64, Linux aarch64"
            ;;
    esac

    asset="ai-architect-mcp-codebase-${release_target}.tar.gz"
    base="${EXPECTED_REPOSITORY_URL}/releases/download/v${version}"
    download_dir=$(mktemp -d)
    trap 'rm -rf "$download_dir"' EXIT
    err "installing v${version} for ${release_target}"

    download "$base/$asset" "$download_dir/$asset" \
        || fatal "release asset unavailable: $base/$asset"
    download "$base/$asset.sha256" "$download_dir/$asset.sha256" \
        || fatal "release checksum unavailable"
    expected=$(awk '{print $1}' "$download_dir/$asset.sha256")
    actual=$(sha256_file "$download_dir/$asset")
    [ -n "$expected" ] && [ "$actual" = "$expected" ] \
        || fatal "release checksum verification failed"

    download "$base/$asset.sigstore.json" "$download_dir/$asset.sigstore.json" \
        || fatal "release provenance bundle unavailable"
    verify_provenance "$download_dir/$asset" "$download_dir/$asset.sigstore.json" \
        || fatal "release provenance verification failed or exceeded 20 seconds"

    # Streaming the one expected member into a new regular file prevents tar
    # symlinks, hardlinks, devices, ownership and permission metadata from
    # reaching the plugin cache. mv publishes the verified binary atomically.
    staged_bin="$download_dir/ai-architect-mcp-codebase.verified"
    tar -xzOf "$download_dir/$asset" ai-architect-mcp-codebase > "$staged_bin" \
        || fatal "release archive does not contain the expected binary"
    [ -s "$staged_bin" ] || fatal "release binary is empty"
    chmod 0755 "$staged_bin"
    installed_digest=$(sha256_file "$staged_bin")
    mkdir -p "$(dirname "$BIN")"
    mv -f "$staged_bin" "$BIN"
    printf '%s  %s\n' "$installed_digest" "$BIN" > "$DIGEST_FILE"
    err "release binary installed; SHA-256 and Sigstore provenance verified"
    exit 0
fi

# Source checkout: preserve the developer's source/binary freshness contract.
needs_build="no"
if [ ! -x "$BIN" ]; then
    needs_build="missing"
elif [ -d "$SRC_DIR" ]; then
    newer=$(find "$SRC_DIR" "$MANIFEST" "$ROOT/Cargo.lock" \
                -newer "$BIN" -print -quit 2>/dev/null || true)
    if [ -n "$newer" ]; then
        needs_build="stale"
    fi
fi

if [ "$needs_build" = "no" ]; then
    log "source-checkout binary up-to-date at $BIN"
    exit 0
fi

command -v cargo >/dev/null 2>&1 \
    || fatal "cargo not found; install Rust from https://rustup.rs"
err "building from source (reason: $needs_build; first build can take several minutes)"
if cargo build --release --quiet --manifest-path "$MANIFEST" >&2; then
    [ -x "$BIN" ] || fatal "cargo succeeded but $BIN is not executable"
    err "source build complete"
else
    fatal "cargo build failed; rerun without --quiet for full output"
fi
