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
MINIMUM_VERSION="0.9.0"

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
    # gh is a Go binary and ignores SIGALRM on macOS. Node is already required
    # by Claude Code; this watchdog sends SIGTERM after 15 s (over 3x the
    # measured 4.5 s cold TUF-root lookup), then SIGKILL two seconds later.
    # The trust-anchor argv exists in one place and is exercised on every host.
    node -e '
      const { spawn } = require("child_process");
      const child = spawn(process.argv[1], process.argv.slice(2), { stdio: "inherit" });
      const hard = setTimeout(() => child.kill("SIGKILL"), 17000);
      const soft = setTimeout(() => child.kill("SIGTERM"), 15000);
      child.on("error", error => { clearTimeout(soft); clearTimeout(hard); console.error(error); process.exit(1); });
      child.on("exit", (code, signal) => {
        clearTimeout(soft); clearTimeout(hard);
        process.exit(code === 0 ? 0 : (signal ? 124 : code));
      });
    ' gh attestation verify "$artifact" \
        --repo "$EXPECTED_REPO" \
        --signer-workflow "$EXPECTED_SIGNER_WORKFLOW" \
        --source-ref "refs/tags/v$version" \
        --bundle "$bundle" >&2
}

[ -f "$MANIFEST" ] || fatal "Cargo.toml missing at $MANIFEST"
[ -f "$PLUGIN_MANIFEST" ] || fatal "plugin.json missing at $PLUGIN_MANIFEST"

marketplace_install="no"
if [ -n "${CLAUDE_PLUGIN_ROOT:-}" ] && [ "${AI_ARCHITECT_SOURCE_CHECKOUT:-0}" != "1" ]; then
    marketplace_install="yes"
fi

if [ "$marketplace_install" = "yes" ]; then
    command -v node >/dev/null 2>&1 || fatal "Node.js is required to parse plugin metadata"
    identity=$(node -e '
      const fs = require("fs");
      const plugin = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
      const cargo = fs.readFileSync(process.argv[2], "utf8");
      const match = cargo.match(/^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m);
      if (!match) throw new Error("Cargo package version missing");
      if (!/^\d+\.\d+\.\d+$/.test(plugin.version)) throw new Error("plugin version is not stable SemVer");
      if (plugin.version !== match[1]) throw new Error("plugin and Cargo versions differ");
      const current = plugin.version.split(".").map(Number);
      const minimum = process.argv[3].split(".").map(Number);
      for (let i = 0; i < 3; i++) {
        if (current[i] > minimum[i]) break;
        if (current[i] < minimum[i]) throw new Error("plugin version is below the supported floor");
      }
      process.stdout.write(plugin.version + "\t" + plugin.repository);
    ' "$PLUGIN_MANIFEST" "$MANIFEST" "$MINIMUM_VERSION") \
        || fatal "invalid plugin identity metadata"
    IFS=$'\t' read -r version manifest_repository <<< "$identity"
    [ "$manifest_repository" = "$EXPECTED_REPOSITORY_URL" ] \
        || fatal "plugin repository is not the trusted producer $EXPECTED_REPOSITORY_URL"

    if [ -x "$BIN" ] && [ -f "$DIGEST_FILE" ]; then
        read -r cached_version expected _ < "$DIGEST_FILE"
        [ "$cached_version" = "$version" ] \
            || fatal "cached binary version mismatch; reinstall the plugin"
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
