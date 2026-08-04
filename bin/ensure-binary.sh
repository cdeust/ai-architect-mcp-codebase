#!/usr/bin/env bash
# Ensure the Claude plugin has the canonical ai-architect-mcp-codebase binary.
#
# Marketplace cache (CLAUDE_PLUGIN_ROOT set, unless an explicit source checkout
# is confirmed): download the release archive, verify its SHA-256 and bundled
# Sigstore provenance against the fixed
# producer/workflow identity, then install it atomically. Persist the verified
# digest and re-check it on every launch.
#
# Source checkout (no CLAUDE_PLUGIN_ROOT, or explicit opt-in plus .git): never
# substitute a release binary for local source. Rebuild when sources are newer
# than target/.

set -euo pipefail

EXPECTED_REPO="cdeust/ai-architect-mcp-codebase"
EXPECTED_REPOSITORY_URL="https://github.com/${EXPECTED_REPO}"
EXPECTED_SIGNER_WORKFLOW="${EXPECTED_REPO}/.github/workflows/release.yml"
# This is a release pin, not a floating minimum. The distribution identity gate
# requires it to match Cargo.toml and every public manifest before merge.
EXPECTED_VERSION="0.9.0"
EXPECTED_PLUGIN_MANIFEST_SHA256="f6123131585f7b56eb16d4d3cbb909dc7ae4678314f536dd38cde52ac1d6b4a3"
EXPECTED_CARGO_MANIFEST_SHA256="796000db3c3482058858b92e6552e64c5cb86f21a9b35f5a7f1a3eb3f5188945"

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
    local remaining
    remaining=$((DOWNLOAD_DEADLINE - $(date +%s)))
    [ "$remaining" -gt 0 ] || fatal "release download exceeded the 180-second global budget"
    # 5 s connect bound; abort a stalled transfer below 1 KiB/s for 30 s.
    # --retry-max-time shares the remaining global budget across retries.
    curl --fail --location --silent --show-error \
        --connect-timeout 5 --max-time "$remaining" --retry-max-time "$remaining" \
        --speed-limit 1024 --speed-time 30 --retry 2 --retry-connrefused \
        "$1" -o "$2"
}

verify_provenance() {
    local artifact=$1
    local bundle=$2
    local release_version=$3
    local -a gh_args=(
        attestation verify "$artifact"
        --repo "$EXPECTED_REPO"
        --signer-workflow "$EXPECTED_SIGNER_WORKFLOW"
        --source-ref "refs/tags/v$release_version"
        --bundle "$bundle"
    )
    # gh is a Go binary and ignores SIGALRM on macOS. When available, Perl
    # remains the parent, receives the alarm itself, then sends SIGTERM and
    # SIGKILL to gh. This is not the broken `alarm; exec gh` pattern: the alarm
    # never enters Go.
    # The 30 s bound is 6x the measured 5 s cold TUF-root lookup on 2026-08-04.
    # The trust-anchor argv exists in one place and is exercised on every host.
    if command -v perl >/dev/null 2>&1; then
        perl -e '
          use strict;
          use warnings;
          my $seconds = shift @ARGV;
          my $pid = fork();
          die "fork failed: $!\n" unless defined $pid;
          if ($pid == 0) { exec @ARGV; die "exec failed: $!\n"; }
          my $timed_out = 0;
          local $SIG{ALRM} = sub {
            $timed_out = 1;
            kill "TERM", $pid;
            select undef, undef, undef, 2;
            kill "KILL", $pid if kill 0, $pid;
          };
          alarm $seconds;
          while (waitpid($pid, 0) == -1) {
            next if $!{EINTR};
            die "waitpid failed: $!\n";
          }
          my $status = $?;
          alarm 0;
          exit(124) if $timed_out;
          exit(1) if $status == -1;
          exit(128 + ($status & 127)) if $status & 127;
          exit($status >> 8);
        ' 30 gh "${gh_args[@]}" >&2
    else
        err "WARNING: Perl unavailable; provenance verification has no local time bound"
        gh "${gh_args[@]}" >&2
    fi
}

[ -f "$MANIFEST" ] || fatal "Cargo.toml missing at $MANIFEST"
[ -f "$PLUGIN_MANIFEST" ] || fatal "plugin.json missing at $PLUGIN_MANIFEST"

marketplace_install="no"
if [ -n "${CLAUDE_PLUGIN_ROOT:-}" ]; then
    # This is a developer escape hatch, never a decision packaged metadata may
    # make: a marketplace cache has no .git, and every accepted bypass is
    # announced even when the launcher requested quiet mode.
    if [ "${AI_ARCHITECT_SOURCE_CHECKOUT:-0}" = "1" ] && [ -e "$ROOT/.git" ]; then
        err "bootstrap verification skipped (source-checkout mode)"
    else
        marketplace_install="yes"
    fi
fi

if [ "$marketplace_install" = "yes" ]; then
    [ "$(sha256_file "$PLUGIN_MANIFEST")" = "$EXPECTED_PLUGIN_MANIFEST_SHA256" ] \
        || fatal "plugin manifest does not match the reviewed release identity"
    [ "$(sha256_file "$MANIFEST")" = "$EXPECTED_CARGO_MANIFEST_SHA256" ] \
        || fatal "Cargo manifest does not match the reviewed release identity"
    version="$EXPECTED_VERSION"

    # An obsolete or unreadable cache record falls through to the stricter
    # download path, which verifies SHA-256 and provenance again. A digest
    # mismatch at the current version remains fatal because that is tampering.
    if [ -x "$BIN" ] && [ -f "$DIGEST_FILE" ]; then
        cached_version=""
        expected=""
        cached_path=""
        extra=""
        if read -r cached_version expected cached_path extra < "$DIGEST_FILE" \
            && [ -n "$cached_version" ] \
            && [[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] \
            && [ -n "$cached_path" ] \
            && [ -z "$extra" ]; then
            if [ "$cached_version" = "$version" ]; then
                actual=$(sha256_file "$BIN")
                [ "$actual" = "$expected" ] \
                    || fatal "cached binary digest mismatch; reinstall the plugin"
                log "cached binary SHA-256 verified at $BIN"
                exit 0
            fi
            err "cached binary version mismatch; refreshing verified release"
        else
            err "cached binary metadata is invalid or obsolete; refreshing verified release"
        fi
        rm -f "$DIGEST_FILE"
    fi

    command -v curl >/dev/null 2>&1 || fatal "curl is required for marketplace installation"
    command -v gh >/dev/null 2>&1 \
        || fatal "GitHub CLI 2.68+ is required to verify release provenance: https://cli.github.com/"
    gh attestation verify --help 2>&1 | grep -q -- '--source-ref' \
        || fatal "GitHub CLI lacks 'gh attestation verify --source-ref'; upgrade to version 2.68 or newer"

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
    DOWNLOAD_DEADLINE=$(($(date +%s) + 180))
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
    verify_provenance "$download_dir/$asset" "$download_dir/$asset.sigstore.json" "$version" \
        || fatal "release provenance verification failed or exceeded the 30-second budget"

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
    printf '%s  %s  %s\n' "$version" "$installed_digest" "$BIN" > "$DIGEST_FILE"
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
