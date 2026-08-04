#!/usr/bin/env bash
# ensure-binary.sh — guarantee target/release/ai-architect-mcp-codebase exists.
#
# Used by:
#   - session-start.sh hook (runs synchronously, BEFORE MCP servers
#     attempt to connect). Builds once on first install.
#   - .mcp.json launcher (re-invokes if binary disappeared between
#     sessions; fails fast with a clear stderr message rather than
#     silently calling `cargo run` which exceeds MCP startup timeout).
#
# Behaviour
# ---------
# 1. If `target/release/ai-architect-mcp-codebase` exists AND is newer than every
#    file under `src/` and `Cargo.{toml,lock}`, exit 0 immediately.
# 2. Otherwise, download the matching checksum-verified GitHub release asset.
# 3. Before a release exists, fall back to `cargo build --release --quiet`.
#
# Determinism: zero side effects beyond `cargo build`. Idempotent. No
# global state. Safe to invoke from any hook or launcher.

set -euo pipefail

ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
BIN="$ROOT/target/release/ai-architect-mcp-codebase"
MANIFEST="$ROOT/Cargo.toml"
SRC_DIR="$ROOT/src"

# Quiet mode: caller is the MCP launcher; suppress progress noise.
# Verbose mode: caller is a session-start hook or human; show progress.
MODE="${1:-quiet}"

log() {
    if [ "$MODE" = "verbose" ]; then
        echo "$@" >&2
    fi
}

err() {
    echo "ai-architect-mcp-codebase: $*" >&2
}

# Need cargo available in PATH.
if ! command -v cargo >/dev/null 2>&1; then
    err "FATAL: cargo not found in PATH. Install Rust (https://rustup.rs)"
    err "       and re-run this command. Plugin cannot start without"
    err "       a built binary."
    exit 127
fi

# Manifest sanity.
if [ ! -f "$MANIFEST" ]; then
    err "FATAL: Cargo.toml missing at $MANIFEST"
    exit 1
fi

# Fast path: binary exists and is fresh relative to all sources.
needs_build="no"
if [ ! -x "$BIN" ]; then
    needs_build="missing"
else
    # Compare mtimes: rebuild if any source file is newer than the binary.
    # `find -newer` returns the first match; head -1 to short-circuit.
    if [ -d "$SRC_DIR" ]; then
        newer=$(find "$SRC_DIR" "$MANIFEST" "$ROOT/Cargo.lock" \
                    -newer "$BIN" -print -quit 2>/dev/null || true)
        if [ -n "$newer" ]; then
            needs_build="stale"
        fi
    fi
fi

if [ "$needs_build" = "no" ]; then
    log "ai-architect-mcp-codebase: binary up-to-date at $BIN"
    exit 0
fi

# A marketplace install contains source, not target/. Compiling the complete
# graph stack during Claude's MCP handshake exceeds the host startup timeout on
# a cold machine, so released versions use the same prebuilt asset as the
# standalone installer. Failure is non-fatal here: an unreleased checkout can
# still use the source-build fallback below.
VERSION=$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$ROOT/.claude-plugin/plugin.json" | head -1)
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) release_target="macos-aarch64" ;;
    Linux-x86_64) release_target="linux-x86_64" ;;
    Linux-aarch64) release_target="linux-aarch64" ;;
    *) release_target="" ;;
esac

if [ "$needs_build" = "missing" ] && [ -n "$VERSION" ] && [ -n "$release_target" ] && command -v curl >/dev/null 2>&1; then
    asset="ai-architect-mcp-codebase-${release_target}.tar.gz"
    base="https://github.com/cdeust/ai-architect-mcp-codebase/releases/download/v${VERSION}"
    download_dir=$(mktemp -d)
    trap 'rm -rf "$download_dir"' EXIT
    err "Installing ai-architect-mcp-codebase v${VERSION} for ${release_target}…"
    if curl --fail --location --silent --show-error "$base/$asset" -o "$download_dir/$asset" \
        && curl --fail --location --silent --show-error "$base/$asset.sha256" -o "$download_dir/$asset.sha256"; then
        expected=$(awk '{print $1}' "$download_dir/$asset.sha256")
        if command -v sha256sum >/dev/null 2>&1; then
            actual=$(sha256sum "$download_dir/$asset" | awk '{print $1}')
        else
            actual=$(shasum -a 256 "$download_dir/$asset" | awk '{print $1}')
        fi
        if [ -n "$expected" ] && [ "$actual" = "$expected" ]; then
            mkdir -p "$(dirname "$BIN")"
            tar -xzf "$download_dir/$asset" -C "$(dirname "$BIN")"
            chmod +x "$BIN"
            err "ai-architect-mcp-codebase: verified release binary installed"
            exit 0
        fi
        err "release checksum verification failed; falling back to a source build"
    else
        err "release asset unavailable; falling back to a source build"
    fi
    rm -rf "$download_dir"
    trap - EXIT
fi

# Build path. Cargo writes its own progress to stderr; we add a
# bracketing message so the user knows what's happening.
err "Building ai-architect-mcp-codebase (reason: $needs_build)…"
err "  manifest: $MANIFEST"
err "  output:   $BIN"
err "  this can take 2–3 minutes on first install."

start_ts=$(date +%s)
if cargo build --release --quiet --manifest-path "$MANIFEST" >&2; then
    end_ts=$(date +%s)
    elapsed=$((end_ts - start_ts))
    err "ai-architect-mcp-codebase: build OK in ${elapsed}s"
    if [ ! -x "$BIN" ]; then
        err "FATAL: cargo build succeeded but $BIN is not executable."
        err "       Check Cargo.toml [[bin]] target name == 'ai-architect-mcp-codebase'."
        exit 1
    fi
    exit 0
else
    err "FATAL: cargo build failed. Run manually for full output:"
    err "       cargo build --release --manifest-path '$MANIFEST'"
    exit 1
fi
