#!/usr/bin/env bash
# Source: windows-build.yml at 5c2e138f950f4cde34b0bd10c4c5b7ba6be89d34.
set -euo pipefail
SHIM="$RUNNER_TEMP/openssl-shim"
mkdir -p "$SHIM"
for d in \
  "/c/Program Files/OpenSSL/lib/VC/x64/MD" \
  "/c/Program Files/OpenSSL/lib/VC" \
  "/c/Program Files/OpenSSL/lib" \
  "/c/vcpkg/installed/x64-windows/lib"
do
  if [ -f "$d/libssl.lib" ] && [ -f "$d/libcrypto.lib" ]; then
    cp "$d/libssl.lib" "$SHIM/ssl.lib"
    cp "$d/libcrypto.lib" "$SHIM/crypto.lib"
    cp "$d/libssl.lib" "$SHIM/libssl.lib"
    cp "$d/libcrypto.lib" "$SHIM/libcrypto.lib"
    echo "shimmed OpenSSL import libs (both naming conventions) from: $d"
    ls -la "$SHIM"
    echo "RUSTFLAGS=-L native=$(cygpath -w "$SHIM")" >> "$GITHUB_ENV"
    exit 0
  fi
done
# Fail loudly with the evidence rather than letting the build reach a
# link error whose cause is one directory listing away.
echo "::error::no libssl.lib/libcrypto.lib found in any known location"
find "/c/Program Files/OpenSSL" -name '*.lib' 2>/dev/null | head -20 || true
exit 1
