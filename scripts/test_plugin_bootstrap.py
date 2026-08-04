#!/usr/bin/env python3
"""Mutation-resistant tests for the Claude marketplace bootstrap trust path."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import time
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
if [ "$*" = "attestation verify --help" ]; then exit 0; fi
printf '%s\\n' "$*" >> {str(gh_calls)!r}
case "${{TEST_GH_MODE:-success}}" in
  success) exit 0 ;;
  fail) exit 1 ;;
  hang) exec python3 -c 'import signal,time; signal.signal(signal.SIGALRM, signal.SIG_IGN); time.sleep(60)' ;;
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
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(plugin / "bin/ensure-binary.sh")],
        env={
            **os.environ,
            "CLAUDE_PLUGIN_ROOT": str(plugin),
            "PATH": path or f"{fake_bin}:{os.environ['PATH']}",
            "TEST_GH_MODE": gh_mode,
            "AI_ARCHITECT_SOURCE_CHECKOUT": "1" if source_checkout else "0",
        },
        text=True,
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


def main() -> None:
    require(PLUGIN["repository"] == EXPECTED_REPOSITORY_URL, "fixture repository is not the fixed trust anchor")
    with tempfile.TemporaryDirectory(prefix="ai-architect-bootstrap-") as raw_tmp:
        tmp = Path(raw_tmp)

        # Positive control: exact URLs, fixed signer identity, valid SHA/bundle.
        plugin, fake_bin, curl_calls, gh_calls = fixture(tmp / "success")
        release = make_release(tmp / "success")
        install_curl(fake_bin, curl_calls, release)
        result = run(plugin, fake_bin)
        require(result.returncode == 0, result.stderr)
        binary = plugin / "target/release/ai-architect-mcp-codebase"
        require(binary.is_file() and not binary.is_symlink(), "installed binary is not a regular file")
        require((binary.parent / f"{binary.name}.sha256").is_file(), "verified digest was not persisted")
        requested = curl_calls.read_text(encoding="utf-8").splitlines()
        require(requested == [f"{EXPECTED_BASE}/{ASSET}", f"{EXPECTED_BASE}/{ASSET}.sha256", f"{EXPECTED_BASE}/{ASSET}.sigstore.json"], f"wrong URLs: {requested}")
        gh_args = gh_calls.read_text(encoding="utf-8")
        require(f"--repo {EXPECTED_REPO}" in gh_args, "gh verification lacks fixed repository")
        require(f"--signer-workflow {EXPECTED_SIGNER}" in gh_args, "gh verification lacks fixed signer workflow")
        require("--bundle " in gh_args, "gh verification lacks offline bundle")

        # Cached control: digest is rechecked; no network. Tampering fails closed.
        curl_calls.unlink()
        gh_calls.unlink()
        result = run(plugin, fake_bin)
        require(result.returncode == 0, result.stderr)
        require(not curl_calls.exists() and not gh_calls.exists(), "valid cache hit network")
        binary.write_text("tampered\n", encoding="utf-8")
        result = run(plugin, fake_bin)
        require(result.returncode != 0 and "digest mismatch" in result.stderr, "tampered cache was accepted")

        # Version is part of the cache record; a reused directory cannot mask a bump.
        shutil.copy2(release.archive, binary)
        binary.chmod(0o755)
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        (binary.parent / f"{binary.name}.sha256").write_text(f"0.8.4  {digest}  {binary}\n", encoding="utf-8")
        result = run(plugin, fake_bin)
        require(result.returncode != 0 and "version mismatch" in result.stderr, "stale cache version was accepted")

        # Negative controls protect each independent integrity boundary.
        for label, release, gh_mode in (
            ("404", absent_release(), "success"),
            ("badsha", make_release(tmp / "badsha", bad_sha=True), "success"),
            ("nobundle", without_bundle(make_release(tmp / "nobundle")), "success"),
            ("ghfail", make_release(tmp / "ghfail"), "fail"),
        ):
            plugin_case, fake_bin_case, curl_case, _ = fixture(tmp / label)
            install_curl(fake_bin_case, curl_case, release)
            result = run(plugin_case, fake_bin_case, gh_mode=gh_mode)
            require_not_installed(plugin_case, result, label)

        # Symlink case uses an existing non-executable victim and checks the
        # external side effect, not only the absence of a plugin-root binary.
        victim = tmp / "symlink/victim"
        victim.parent.mkdir(parents=True, exist_ok=True)
        victim.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        victim.chmod(0o644)
        plugin_case, fake_bin_case, curl_case, _ = fixture(tmp / "symlink/plugin-case")
        install_curl(fake_bin_case, curl_case, make_release(tmp / "symlink/release", symlink_target=victim))
        result = run(plugin_case, fake_bin_case)
        require_not_installed(plugin_case, result, "symlink")
        require(victim.stat().st_mode & 0o111 == 0, "archive symlink changed victim permissions")

        # A hostile fork cannot redirect either downloads or signer identity.
        plugin, fake_bin, curl_calls, _ = fixture(
            tmp / "hostile-repository", repository="https://github.com/attacker-org/evil-fork"
        )
        install_curl(fake_bin, curl_calls, make_release(tmp / "hostile-repository"))
        result = run(plugin, fake_bin)
        require_not_installed(plugin, result, "hostile repository")
        require(not curl_calls.exists(), "hostile repository reached download path")

        for label, malicious_version in (("downgrade", "0.8.4"), ("traversal", "../../attacker/repo")):
            plugin, fake_bin, curl_calls, _ = fixture(
                tmp / label, version=malicious_version, cargo_version=malicious_version
            )
            install_curl(fake_bin, curl_calls, make_release(tmp / f"{label}-release"))
            result = run(plugin, fake_bin)
            require_not_installed(plugin, result, label)
            require(not curl_calls.exists(), f"{label}: invalid version reached download path")

        # gh is mandatory and must expose the attestation verifier.
        plugin, fake_bin, curl_calls, _ = fixture(tmp / "oldgh")
        (fake_bin / "gh").write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
        (fake_bin / "gh").chmod(0o755)
        install_curl(fake_bin, curl_calls, make_release(tmp / "oldgh-release"))
        result = run(plugin, fake_bin)
        require_not_installed(plugin, result, "old gh")
        require(not curl_calls.exists(), "old gh reached download path")

        plugin, fake_bin, curl_calls, _ = fixture(tmp / "nogh")
        (fake_bin / "gh").unlink()
        node = shutil.which("node")
        require(node is not None, "node is required by the bootstrap test")
        (fake_bin / "node").symlink_to(node)
        install_curl(fake_bin, curl_calls, make_release(tmp / "nogh-release"))
        result = run(plugin, fake_bin, path=f"{fake_bin}:/usr/bin:/bin")
        require_not_installed(plugin, result, "missing gh")
        require(not curl_calls.exists(), "missing gh reached download path")

        # The watchdog must terminate a process that explicitly ignores SIGALRM.
        plugin, fake_bin, curl_calls, _ = fixture(tmp / "hanging-gh")
        install_curl(fake_bin, curl_calls, make_release(tmp / "hanging-gh-release"))
        started = time.monotonic()
        result = run(plugin, fake_bin, gh_mode="hang")
        elapsed = time.monotonic() - started
        require_not_installed(plugin, result, "hanging gh")
        require(elapsed < 25, f"provenance watchdog took {elapsed:.1f}s")

        # A source checkout never downloads upstream bytes and retains Cargo fallback.
        plugin, fake_bin, curl_calls, _ = fixture(tmp / "source")
        install_curl(fake_bin, curl_calls, absent_release())
        result = run(plugin, fake_bin, source_checkout=True)
        require(result.returncode == 1 and "COLD_BUILD_STARTED" in result.stderr, "source checkout did not invoke Cargo")
        require(not curl_calls.exists(), "source checkout downloaded a release")

    print("PLUGIN BOOTSTRAP OK: trust anchor, SHA, provenance, archive type, cache, and source paths")


if __name__ == "__main__":
    main()
