"""Fixtures for the marketplace bootstrap trust-path test (scripts/test_plugin_bootstrap.py).

A plugin tree with the real launcher and manifests, a release of fake assets, the
fake tools (uname, cargo, gh, curl, a hanging gh) the launcher calls, and the
checks every negative case shares. The cases themselves live in the test.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLUGIN = json.loads((ROOT / ".claude-plugin/plugin.json").read_text(encoding="utf-8"))
EXPECTED_REPO = "cdeust/ai-architect-mcp-codebase"
EXPECTED_REPOSITORY_URL = f"https://github.com/{EXPECTED_REPO}"
EXPECTED_SIGNER = f"{EXPECTED_REPO}/.github/workflows/release.yml"
ASSET = "ai-architect-mcp-codebase-macos-aarch64.tar.gz"
EXPECTED_BASE = f"{EXPECTED_REPOSITORY_URL}/releases/download/v{PLUGIN['version']}"


@dataclass(frozen=True)
class ReleaseFixture:
    archive: Path | None
    checksum: Path | None
    bundle: Path | None


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def fixture(
    tmp: Path,
    *,
    repository: str | None = None,
    version: str | None = None,
    cargo_version: str | None = None,
) -> tuple[Path, Path, Path, Path]:
    plugin = tmp / "plugin"
    fake_bin = tmp / "fake-bin"
    curl_calls = tmp / "curl-calls"
    gh_calls = tmp / "gh-calls"
    (plugin / "bin").mkdir(parents=True)
    (plugin / ".claude-plugin").mkdir()
    (plugin / "src").mkdir()
    fake_bin.mkdir()
    shutil.copy2(ROOT / "bin/ensure-binary.sh", plugin / "bin/ensure-binary.sh")
    if repository is None and version is None:
        shutil.copy2(ROOT / ".claude-plugin/plugin.json", plugin / ".claude-plugin/plugin.json")
    else:
        manifest = dict(PLUGIN)
        if repository is not None:
            manifest["repository"] = repository
        if version is not None:
            manifest["version"] = version
        (plugin / ".claude-plugin/plugin.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    if cargo_version is not None:
        cargo = cargo.replace(f'version = "{PLUGIN["version"]}"', f'version = "{cargo_version}"', 1)
    (plugin / "Cargo.toml").write_text(cargo, encoding="utf-8")
    shutil.copy2(ROOT / "Cargo.lock", plugin / "Cargo.lock")

    (fake_bin / "uname").write_text('#!/bin/sh\n[ "$1" = "-s" ] && echo Darwin || echo arm64\n', encoding="utf-8")
    (fake_bin / "cargo").write_text("#!/bin/sh\necho COLD_BUILD_STARTED >&2\nexit 99\n", encoding="utf-8")
    (fake_bin / "gh").write_text(
        f'''#!/bin/sh
if [ "$*" = "attestation verify --help" ]; then echo "  --source-ref string"; exit 0; fi
printf '%s\\n' "$*" >> {str(gh_calls)!r}
case "${{TEST_GH_MODE:-success}}" in
  success) exit 0 ;;
  fail) exit 1 ;;
esac
exit 1
''',
        encoding="utf-8",
    )
    for executable in ("uname", "cargo", "gh"):
        (fake_bin / executable).chmod(0o755)
    return plugin, fake_bin, curl_calls, gh_calls


def run(
    plugin: Path,
    fake_bin: Path,
    *,
    gh_mode: str = "success",
    source_checkout: bool = False,
    path: str | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(plugin / "bin/ensure-binary.sh")],
        env={
            **os.environ,
            "CLAUDE_PLUGIN_ROOT": str(plugin),
            "PATH": path or f"{fake_bin}:{os.environ['PATH']}",
            "TEST_GH_MODE": gh_mode,
            "AI_ARCHITECT_SOURCE_CHECKOUT": "1" if source_checkout else "0",
            **(env or {}),
        },
        text=True,
        errors="replace",
        capture_output=True,
        timeout=40,
        check=False,
    )


def make_release(
    tmp: Path,
    *,
    bad_sha: bool = False,
    symlink_target: Path | None = None,
) -> ReleaseFixture:
    tmp.mkdir(parents=True, exist_ok=True)
    payload = tmp / "ai-architect-mcp-codebase"
    payload.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    payload.chmod(0o755)
    archive = tmp / ASSET
    with tarfile.open(archive, "w:gz") as bundle:
        if symlink_target is not None:
            member = tarfile.TarInfo(payload.name)
            member.type = tarfile.SYMTYPE
            member.linkname = str(symlink_target)
            bundle.addfile(member)
        else:
            bundle.add(payload, arcname=payload.name)
    digest = "0" * 64 if bad_sha else hashlib.sha256(archive.read_bytes()).hexdigest()
    checksum = tmp / f"{ASSET}.sha256"
    checksum.write_text(f"{digest}  {ASSET}\n", encoding="utf-8")
    provenance = tmp / f"{ASSET}.sigstore.json"
    provenance.write_text("{}\n", encoding="utf-8")
    return ReleaseFixture(archive, checksum, provenance)


def install_curl(fake_bin: Path, calls: Path, release: ReleaseFixture) -> None:
    sources = {
        f"{EXPECTED_BASE}/{ASSET}": str(release.archive),
        f"{EXPECTED_BASE}/{ASSET}.sha256": str(release.checksum),
        f"{EXPECTED_BASE}/{ASSET}.sigstore.json": str(release.bundle),
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


def install_hanging_gh(fake_bin: Path) -> None:
    """Build a gh-shaped process that only SIGKILL can terminate."""
    compiler = shutil.which("cc")
    require(compiler is not None, "a C compiler is required for the watchdog test")
    source = fake_bin / "hanging-gh.c"
    source.write_text(
        r'''#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--help") == 0) {
            puts("  --source-ref string");
            return 0;
        }
    }
    signal(SIGALRM, SIG_IGN);
    signal(SIGTERM, SIG_IGN);
    signal(SIGINT, SIG_IGN);
    signal(SIGHUP, SIG_IGN);
    for (;;) pause();
}
''',
        encoding="utf-8",
    )
    compiled = subprocess.run(
        [compiler, str(source), "-o", str(fake_bin / "gh")],
        text=True,
        capture_output=True,
        check=False,
    )
    require(compiled.returncode == 0, f"failed to compile hanging gh fixture: {compiled.stderr}")


def absent_release() -> ReleaseFixture:
    return ReleaseFixture(None, None, None)


def without_bundle(release: ReleaseFixture) -> ReleaseFixture:
    return ReleaseFixture(release.archive, release.checksum, None)


def require_not_installed(plugin: Path, result: subprocess.CompletedProcess[str], label: str) -> None:
    require(result.returncode != 0, f"{label}: unexpectedly succeeded")
    require(
        not os.path.lexists(plugin / "target/release/ai-architect-mcp-codebase"),
        f"{label}: installed a binary or link",
    )
    require("COLD_BUILD_STARTED" not in result.stderr, f"{label}: invoked Cargo in marketplace mode")

