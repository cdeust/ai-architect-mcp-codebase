#!/usr/bin/env bash
# source: https://github.com/cdeust/ai-architect-mcp-codebase/tree/5c2e138f950f4cde34b0bd10c4c5b7ba6be89d34
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
case "${1:-}" in
  setup)
    if [[ "${RUNNER_OS:-Linux}" == Windows ]]; then
      bash .github/ci/windows-openssl.sh
    else
      # source: ci.yml at 5c2e138, duplicate zstd symbols from lbug.
      export ZSTD_SYS_USE_PKG_CONFIG=1
      if [[ "$(cargo llvm-cov --version 2>/dev/null || true)" != 'cargo-llvm-cov 0.8.7' ]]; then
        cargo install cargo-llvm-cov --locked --version 0.8.7 --force
      fi
    fi
    ;;
  unit)
    # source: ci.yml at 5c2e138, duplicate zstd symbols from lbug.
    export ZSTD_SYS_USE_PKG_CONFIG=1
    cargo llvm-cov clean --workspace
    # Includes graph_accuracy exactly once, as part of workspace tests.
    cargo llvm-cov --workspace --locked --no-report
    cargo llvm-cov report --json --output-path coverage.json
    cargo llvm-cov report --summary-only --fail-under-lines 80
    python3 scripts/check_doc_claims.py --coverage-json coverage.json
    ;;
  regression)
    python3 -m unittest discover -s scripts/tests -p 'test_*.py' -v
    ;;
  e2e)
    # source: ci.yml at 5c2e138, duplicate zstd symbols from lbug.
    export ZSTD_SYS_USE_PKG_CONFIG=1
    cargo build --locked --release --bin ai-architect-mcp-codebase
    PATH="$PWD/target/release:$PATH" python3 scripts/smoke_portable_hosts.py
    CLAUDE_PLUGIN_ROOT="$PWD" python3 scripts/smoke_claude_plugin.py
    ;;
  environment)
    if [[ "${RUNNER_OS:-Linux}" == Windows ]]; then
      cargo build --locked --release --bin ai-architect-mcp-codebase --target x86_64-pc-windows-msvc
      test -s target/x86_64-pc-windows-msvc/release/ai-architect-mcp-codebase.exe
    else
      # source: ci.yml at 5c2e138, duplicate zstd symbols from lbug.
      export ZSTD_SYS_USE_PKG_CONFIG=1
      cargo check --locked --workspace --all-targets
    fi
    ;;
  installation)
    # source: ci.yml at 5c2e138, duplicate zstd symbols from lbug.
    export ZSTD_SYS_USE_PKG_CONFIG=1
    cargo build --locked --release --bin ai-architect-mcp-codebase
    python3 scripts/check_distribution_identity.py
    python3 scripts/test_plugin_bootstrap.py
    python3 scripts/smoke_mcpb.py
    ;;
  release-version)
    python3 -c 'import tomllib; print("tag=v"+tomllib.load(open("Cargo.toml","rb"))["package"]["version"])'
    ;;
  *) echo 'Usage: run.sh setup|unit|regression|e2e|environment|installation|release-version' >&2; exit 2 ;;
esac
