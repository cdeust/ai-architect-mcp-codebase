#!/usr/bin/env python3
"""Exercise the release bootstrap success, failure, and cached-binary paths."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSET = "ai-architect-mcp-codebase-macos-aarch64.tar.gz"
EXPECTED_BASE = "https://github.com/cdeust/ai-architect-mcp-codebase/releases/download/v0.9.0"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def fixture(tmp: Path) -> tuple[Path, Path, Path]:
    plugin = tmp / "plugin"
    fake_bin = tmp / "fake-bin"
    calls = tmp / "curl-calls"
    (plugin / "bin").mkdir(parents=True)
    (plugin / ".claude-plugin").mkdir()
    (plugin / "src").mkdir()
    fake_bin.mkdir()
    shutil.copy2(ROOT / "bin/ensure-binary.sh", plugin / "bin/ensure-binary.sh")
    shutil.copy2(ROOT / ".claude-plugin/plugin.json", plugin / ".claude-plugin/plugin.json")
    shutil.copy2(ROOT / "Cargo.toml", plugin / "Cargo.toml")
    shutil.copy2(ROOT / "Cargo.lock", plugin / "Cargo.lock")
    (fake_bin / "uname").write_text('#!/bin/sh\n[ "$1" = "-s" ] && echo Darwin || echo arm64\n', encoding="utf-8")
    (fake_bin / "cargo").write_text("#!/bin/sh\necho COLD_BUILD_STARTED >&2\nexit 99\n", encoding="utf-8")
    (fake_bin / "gh").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    for executable in ("uname", "cargo", "gh"):
        (fake_bin / executable).chmod(0o755)
    return plugin, fake_bin, calls


def run(plugin: Path, fake_bin: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(plugin / "bin/ensure-binary.sh")],
        env={**os.environ, "CLAUDE_PLUGIN_ROOT": str(plugin), "PATH": f"{fake_bin}:{os.environ['PATH']}"},
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )


def make_asset(tmp: Path) -> tuple[Path, Path, Path]:
    payload = tmp / "ai-architect-mcp-codebase"
    payload.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    payload.chmod(0o755)
    archive = tmp / ASSET
    with tarfile.open(archive, "w:gz") as bundle:
        bundle.add(payload, arcname=payload.name)
    checksum = tmp / f"{ASSET}.sha256"
    checksum.write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {ASSET}\n", encoding="utf-8")
    bundle = tmp / f"{ASSET}.sigstore.json"
    bundle.write_text("{}\n", encoding="utf-8")
    return archive, checksum, bundle


def install_curl(
    fake_bin: Path,
    calls: Path,
    archive: Path | None,
    checksum: Path | None,
    bundle: Path | None,
) -> None:
    sources = {
        f"{EXPECTED_BASE}/{ASSET}": str(archive),
        f"{EXPECTED_BASE}/{ASSET}.sha256": str(checksum),
        f"{EXPECTED_BASE}/{ASSET}.sigstore.json": str(bundle),
    }
    script = f'''#!/usr/bin/env python3
import json, shutil, sys
args = sys.argv[1:]
url = next(arg for arg in args if arg.startswith("https://"))
out = args[args.index("-o") + 1]
with open({str(calls)!r}, "a", encoding="utf-8") as log: log.write(url + "\\n")
source = {json.dumps(sources)}.get(url)
if not source or source == "None": sys.exit(22)
shutil.copyfile(source, out)
'''
    (fake_bin / "curl").write_text(script, encoding="utf-8")
    (fake_bin / "curl").chmod(0o755)


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="ai-architect-bootstrap-") as raw_tmp:
        tmp = Path(raw_tmp)

        # Positive control: exact version/target URLs, valid SHA, installed executable.
        plugin, fake_bin, calls = fixture(tmp / "success")
        archive, checksum, bundle = make_asset(tmp / "success")
        install_curl(fake_bin, calls, archive, checksum, bundle)
        result = run(plugin, fake_bin)
        require(result.returncode == 0, result.stderr)
        require((plugin / "target/release/ai-architect-mcp-codebase").exists(), "binary not installed")
        requested = calls.read_text(encoding="utf-8").splitlines()
        require(
            requested
            == [
                f"{EXPECTED_BASE}/{ASSET}",
                f"{EXPECTED_BASE}/{ASSET}.sha256",
                f"{EXPECTED_BASE}/{ASSET}.sigstore.json",
            ],
            f"wrong URLs: {requested}",
        )
        require("COLD_BUILD_STARTED" not in result.stderr, "success path invoked cargo")

        # Negative control: a real attempted 404 fails fast without invoking Cargo.
        plugin, fake_bin, calls = fixture(tmp / "missing")
        install_curl(fake_bin, calls, None, None, None)
        result = run(plugin, fake_bin)
        require(result.returncode == 1, result.stderr)
        require(calls.read_text(encoding="utf-8").splitlines() == [f"{EXPECTED_BASE}/{ASSET}"], "download was not attempted")
        require("no verified release binary" in result.stderr, "missing actionable failure")
        require("COLD_BUILD_STARTED" not in result.stderr, "404 path invoked cargo")

        # Cached marketplace binary: source mtimes are irrelevant and no network/build runs.
        plugin, fake_bin, calls = fixture(tmp / "cached")
        binary = plugin / "target/release/ai-architect-mcp-codebase"
        binary.parent.mkdir(parents=True)
        binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        binary.chmod(0o755)
        time.sleep(0.01)
        (plugin / "src/newer.rs").write_text("// newer than archive binary\n", encoding="utf-8")
        install_curl(fake_bin, calls, None, None, None)
        result = run(plugin, fake_bin)
        require(result.returncode == 0, result.stderr)
        require(not calls.exists(), "cached binary unexpectedly hit the network")
        require("COLD_BUILD_STARTED" not in result.stderr, "cached path invoked cargo")

    print("PLUGIN BOOTSTRAP OK: verified install, attempted 404, and stale-cache reuse")


if __name__ == "__main__":
    main()
