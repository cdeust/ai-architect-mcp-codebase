#!/usr/bin/env bash
# Bare-runner environment probe. Sources: windows-build.yml at f3b51d3
# (OpenSSL import-library names/search paths), ci.yml at f3b51d3 (Linux ZSTD).
set -euo pipefail

prepare_windows_openssl() {
  local shim="$1" directory native_path
  shift
  mkdir -p "$shim"
  for directory in "$@"; do
    if [ -f "$directory/libssl.lib" ] && [ -f "$directory/libcrypto.lib" ]; then
      cp "$directory/libssl.lib" "$shim/ssl.lib"
      cp "$directory/libcrypto.lib" "$shim/crypto.lib"
      cp "$directory/libssl.lib" "$shim/libssl.lib"
      cp "$directory/libcrypto.lib" "$shim/libcrypto.lib"
      native_path="$(cygpath -w "$shim")"
      export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-L native=$native_path"
      printf 'OpenSSL import libraries from: %s\n' "$directory"
      return 0
    fi
  done
  echo '::error::no libssl.lib/libcrypto.lib found in the runner locations' >&2
  return 1
}

main() {
  : "${CI_OS:?CI_OS is unset}" "${CI_WORK:?CI_WORK is unset}"
  case "$CI_OS" in
    linux) export ZSTD_SYS_USE_PKG_CONFIG=1 ;;
    macos) ;;
    windows)
      prepare_windows_openssl "$CI_WORK/openssl-shim" \
        '/c/Program Files/OpenSSL/lib/VC/x64/MD' \
        '/c/Program Files/OpenSSL/lib/VC' \
        '/c/Program Files/OpenSSL/lib' \
        '/c/vcpkg/installed/x64-windows/lib'
      ;;
    *) printf 'Unsupported CI_OS: %s\n' "$CI_OS" >&2; return 1 ;;
  esac
  cargo run --release --locked --bin ai-architect-mcp-codebase -- install --dry-run
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
