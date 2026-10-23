"""Execute bootstrap shell branches with disposable archives, no network or Rust.

OS detection and provenance CLI responses are fixtures; this is not native Windows
execution. The installed macOS release is exercised separately by the CI smoke.
"""

import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def make_archive(base, name, target, checksum_ok):
    assets = base / "assets"
    assets.mkdir()
    payload = b'#!/usr/bin/env bash\nprintf "fixture-binary:%s\\n" "$*"\n'
    asset = assets / f"ai-architect-mcp-codebase-{target}.tar.gz"
    with tarfile.open(asset, "w:gz") as archive:
        entry = tarfile.TarInfo(name)
        entry.size = len(payload)
        archive.addfile(entry, io.BytesIO(payload))
    digest = hashlib.sha256(asset.read_bytes()).hexdigest() if checksum_ok else "0" * 64
    Path(str(asset) + ".sha256").write_text(f"{digest}  {asset.name}\n")
    Path(str(asset) + ".sigstore.json").write_text("{}")
    return assets, asset, payload


def make_tools(base, system, machine):
    tools = base / "tools"
    tools.mkdir()
    (tools / "uname").write_text(
        f'#!/bin/sh\ncase "$1" in -s) echo "{system}";; -m) echo "{machine}";; esac\n'
    )
    (tools / "curl").write_text(f"""#!{sys.executable}
import os, pathlib, shutil, sys
args = sys.argv[1:]
i = args.index('-o')
url = args[i-1]
with open(os.environ['DOWNLOAD_LOG'], 'a') as out: out.write(url + '\\n')
shutil.copyfile(pathlib.Path(os.environ['FIXTURE_ASSETS']) / url.rsplit('/',1)[1], args[i+1])
""")
    (tools / "gh").write_text("""#!/bin/sh
case "$*" in *--help*) echo --source-ref; exit 0;; esac
printf '%s\n' "$*" >> "$PROVENANCE_LOG"
exit "$PROVENANCE_STATUS"
""")
    for tool in tools.iterdir():
        tool.chmod(0o755)
    return tools


class BootstrapPlatforms(unittest.TestCase):
    def exercise(
        self, system, machine, target, *, checksum_ok=True, provenance_ok=True
    ):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            plugin = base / "plugin"
            (plugin / "bin").mkdir(parents=True)
            (plugin / ".claude-plugin").mkdir()
            for name in (
                "Cargo.toml",
                "bin/ensure-binary.sh",
                "bin/launch-plugin.sh",
                ".claude-plugin/plugin.json",
            ):
                shutil.copy2(ROOT / name, plugin / name)
            windows = system.startswith(("MINGW", "MSYS", "CYGWIN"))
            name = "ai-architect-mcp-codebase" + (".exe" if windows else "")
            assets, asset, payload = make_archive(base, name, target, checksum_ok)
            tools = make_tools(base, system, machine)
            environment = {
                **os.environ,
                "CLAUDE_PLUGIN_ROOT": str(plugin),
                "AI_ARCHITECT_SOURCE_CHECKOUT": "0",
                "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}",
                "FIXTURE_ASSETS": str(assets),
                "DOWNLOAD_LOG": str(base / "downloads"),
                "PROVENANCE_LOG": str(base / "provenance"),
                "PROVENANCE_STATUS": "0" if provenance_ok else "1",
            }
            command = [
                "bash",
                str(plugin / "bin/launch-plugin.sh"),
                "argument with spaces",
            ]
            result = subprocess.run(
                command, env=environment, capture_output=True, text=True
            )
            if target == "unsupported" or not checksum_ok or not provenance_ok:
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((plugin / "target/release" / name).exists())
                return result.stderr
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "fixture-binary:argument with spaces\n")
            self.assertEqual((plugin / "target/release" / name).read_bytes(), payload)
            self.check_verified_cache(base, asset, command, environment)
            return result.stderr

    def check_verified_cache(self, base, asset, command, environment):
        log = (base / "downloads").read_text()
        self.assertIn(f"v0.14.0/{asset.name}", log)
        proof = (base / "provenance").read_text()
        self.assertIn("--source-ref refs/tags/v0.14.0", proof)
        self.assertIn(
            "--signer-workflow cdeust/ai-architect-mcp-codebase/.github/workflows/release.yml",
            proof,
        )
        self.assertIn("--repo cdeust/ai-architect-mcp-codebase", proof)
        cached = subprocess.run(
            command, env=environment, capture_output=True, text=True
        )
        self.assertEqual(cached.returncode, 0, cached.stderr)
        self.assertEqual(
            (base / "downloads").read_text(),
            log,
            "verified cache unexpectedly redownloaded",
        )

    def test_existing_macos_and_linux_and_windows_shell_mappings(self):
        cases = [
            ("Darwin", "arm64", "macos-aarch64"),
            ("Linux", "x86_64", "linux-x86_64"),
            ("Linux", "aarch64", "linux-aarch64"),
            ("MINGW64_NT-10.0", "x86_64", "windows-x86_64"),
            ("MSYS_NT-10.0", "x86_64", "windows-x86_64"),
            ("CYGWIN_NT-10.0", "x86_64", "windows-x86_64"),
        ]
        for system, machine, target in cases:
            with self.subTest(system=system, machine=machine):
                self.exercise(system, machine, target)

    def test_windows_checksum_and_provenance_remain_required(self):
        self.assertIn(
            "checksum verification failed",
            self.exercise(
                "MINGW64_NT-10.0", "x86_64", "windows-x86_64", checksum_ok=False
            ),
        )
        self.assertIn(
            "provenance verification failed",
            self.exercise(
                "MINGW64_NT-10.0", "x86_64", "windows-x86_64", provenance_ok=False
            ),
        )

    def test_unsupported_windows_architecture_does_not_install(self):
        self.assertIn(
            "unsupported plugin platform",
            self.exercise("MINGW64_NT-10.0", "aarch64", "unsupported"),
        )


if __name__ == "__main__":
    unittest.main()
