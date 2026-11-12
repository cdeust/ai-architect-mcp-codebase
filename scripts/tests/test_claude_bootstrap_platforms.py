"""Execute bootstrap shell branches with disposable archives, no network or Rust.

OS detection and provenance CLI responses are fixtures; this is not native Windows
execution. The installed release is exercised separately by the CI smoke (the
pipeline's `install` job, on macOS, Linux and Windows runners).
"""

import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def require(condition, message):
    """An `assert` that `python -O` does not strip."""
    if not condition:
        raise AssertionError(message)


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


class BootstrapHarness(unittest.TestCase):
    """The bootstrap exercise, shared by the test classes; it holds no tests."""

    def exercise(
        self,
        system,
        machine,
        target,
        *,
        checksum_ok=True,
        provenance_ok=True,
        windows_checkout=False,
        manifest_edit=None,
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
            if windows_checkout:
                subprocess.run(
                    [
                        "git",
                        "-C",
                        str(ROOT),
                        "-c",
                        "core.autocrlf=true",
                        "checkout-index",
                        "--force",
                        f"--prefix={plugin.as_posix()}/",
                        "--",
                        "Cargo.toml",
                        ".claude-plugin/plugin.json",
                        "bin/ensure-binary.sh",
                        "bin/launch-plugin.sh",
                    ],
                    check=True,
                    capture_output=True,
                )
            if manifest_edit:
                manifest = plugin / manifest_edit
                manifest.write_bytes(
                    manifest.read_bytes().replace(b"0.15.0", b"0.15.1")
                )
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
            if (
                target == "unsupported"
                or not checksum_ok
                or not provenance_ok
                or manifest_edit
            ):
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
        self.assertIn(f"v0.15.0/{asset.name}", log)
        proof = (base / "provenance").read_text()
        self.assertIn("--source-ref refs/tags/v0.15.0", proof)
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


class BootstrapPlatforms(BootstrapHarness):
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

    def test_windows_checkout_preserves_reviewed_release_identity(self):
        # PR461: checkout with CRLF defaults changed pinned manifest bytes.
        self.exercise(
            "MINGW64_NT-10.0", "x86_64", "windows-x86_64", windows_checkout=True
        )

    def test_windows_checkout_still_rejects_modified_release_identity(self):
        for manifest, message in (
            (".claude-plugin/plugin.json", "plugin manifest does not match"),
            ("Cargo.toml", "Cargo package table does not match"),
        ):
            with self.subTest(manifest=manifest):
                self.assertIn(
                    message,
                    self.exercise(
                        "MINGW64_NT-10.0",
                        "x86_64",
                        "windows-x86_64",
                        windows_checkout=True,
                        manifest_edit=manifest,
                    ),
                )

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


# Issue #449: what each platform the README's table installs means to `uname`.
INSTALLED_UNAME = {
    "macOS arm64": ("Darwin", "arm64"),
    "Linux x86_64": ("Linux", "x86_64"),
    "Linux aarch64": ("Linux", "aarch64"),
    "Windows x86_64": ("MINGW64_NT-10.0", "x86_64"),
}

# Issue #449: the `uname` pair the launcher matches on each runner label of
# the `install` job. Inferred, not measured: `install` passed in run
# 42697366993 on images macos-26-arm64, ubuntu-26.04 and windows-2025-vs2026,
# and the launcher installs on these four platforms only. Git Bash reports
# `MINGW64_NT-10.0-<build>`; the launcher matches its `MINGW*` prefix.
RUNNER_UNAME = {
    "macos-latest": ("Darwin", "arm64"),
    "ubuntu-latest": ("Linux", "x86_64"),
    "windows-latest": ("MINGW64_NT-10.0", "x86_64"),
}

# Issue #449: what each name in the README's "refuses" row means to `uname`.
REFUSED_UNAME = {
    "macOS x86_64 (Intel)": [("Darwin", "x86_64")],
    "Windows arm64": [
        ("MINGW64_NT-10.0", "aarch64"),
        ("MSYS_NT-10.0", "aarch64"),
        ("CYGWIN_NT-10.0", "aarch64"),
    ],
    # Samples, each one architecture away from an installed row: a catch-all
    # such as `Linux-*)` in the launcher fails here.
    "any other": [
        ("FreeBSD", "amd64"),
        ("Linux", "riscv64"),
        ("Linux", "armv7l"),
        ("Darwin", "i386"),
    ],
}


def readme_platform_rows():
    readme = (ROOT / "README.md").read_text()
    start = readme.index("<!-- platforms:begin -->")
    end = readme.index("<!-- platforms:end -->")
    lines = [line for line in readme[start:end].splitlines() if line.startswith("|")]
    return [[cell.strip() for cell in line.strip("|").split("|")] for line in lines[2:]]


def release_runners():
    """`{target_tag: runner}` of the `build:` job's matrix in `release.yml`."""
    release = (ROOT / ".github/workflows/release.yml").read_text()
    job = re.search(r"^  build:\n(.*?)(?=^  [^\s#]|\Z)", release, re.M | re.S)
    require(job, "release.yml has no top-level `build:` job")
    tags = re.findall(r"^\s*-\s*target_tag:\s*([a-z0-9_-]+)", job.group(1), re.M)
    pairs = re.findall(
        r"^\s*-\s*target_tag:\s*([a-z0-9_-]+)\n"
        r"(?:\s+[a-z_]+:.*\n)*?"
        r"\s+runner:\s*([a-z0-9.-]+)",
        job.group(1),
        re.M,
    )
    require(
        len(pairs) == len(tags), "a `target_tag:` entry has no readable `runner:`"
    )
    require(len(set(tags)) == len(tags), "a `target_tag:` is listed twice")
    require(
        re.search(r"^    runs-on: \$\{\{ matrix\.runner \}\}$", job.group(1), re.M),
        "the `build` job does not run on `matrix.runner`",
    )
    # source: release.yml's own comment on #176 — with `continue-on-error`,
    # v0.8.3 shipped green with its windows asset missing.
    require(
        not re.search(r"^\s+continue-on-error:", job.group(1), re.M),
        "a `build` leg may fail without failing the release",
    )
    return dict(pairs)


def install_job_runners():
    """The systems the pipeline's `install` job runs on, in matrix order, for a
    change that is not documentation only. The job takes its `os` matrix from the
    `contract` job, which selects it from `RUNNERS` (Cortex ADR-1133)."""
    pipeline = (ROOT / ".github/workflows/pipeline.yml").read_text()
    job = re.search(r"^  install:\n(.*?)(?=^  [^\s#]|\Z)", pipeline, re.M | re.S)
    require(job, "pipeline.yml has no top-level `install:` job")
    body = job.group(1)
    selected = r"^\s+os: \$\{\{ fromJSON\(needs\.contract\.outputs\.platforms\) \}\}$"
    require(
        re.search(selected, body, re.M),
        "the `install` job does not take its `os` matrix from the `contract` job",
    )
    runners = re.findall(r"^\s+RUNNERS = \[([^\]]*)\]$", pipeline, re.M)
    require(len(runners) == 1, "pipeline.yml does not define `RUNNERS` exactly once")
    require(
        not re.search(r"^\s+(include|exclude):", body, re.M),
        "`include:`/`exclude:` matrix entries are not read",
    )
    require(
        re.search(r"^    runs-on: \$\{\{ matrix\.os \}\}$", body, re.M),
        "the `install` job does not run on `matrix.os`",
    )
    labels = [runner.strip().strip('"') for runner in runners[0].split(",")]
    require(len(set(labels)) == len(labels), "an `install` runner is listed twice")
    return labels


class ReadmePlatformTable(BootstrapHarness):
    """Issue #449: the README's supported-platforms table states what the
    launcher installs or refuses, what `release.yml` builds, and which runners
    of the pipeline's `install` job start the published asset (`ci.json`)."""

    def test_release_assets_match_the_launcher_and_the_release_matrix(self):
        listed = {
            row[1].strip("`") for row in readme_platform_rows() if row[2] == "installs"
        }
        launcher = (ROOT / "bin/ensure-binary.sh").read_text()
        accepted = set(
            re.findall(r'^[^#\n]*release_target="([a-z0-9_-]+)"', launcher, re.M)
        )
        built = set(release_runners())
        self.assertEqual(listed, accepted)
        self.assertEqual(listed, built)

    def test_every_refused_platform_is_refused_by_the_launcher(self):
        names = [
            name.strip()
            for row in readme_platform_rows()
            if row[2] == "refuses"
            for name in row[0].split(",")
        ]
        self.assertTrue(names, "the table names no refused platform")
        for row in readme_platform_rows():
            self.assertIn(row[2], {"installs", "refuses"}, row)
            if row[2] == "refuses":
                self.assertEqual(row[1], "none", "a refused platform has no asset")
                self.assertEqual(row[3], "no", "a refused platform is not started")
        for name in names:
            with self.subTest(name=name):
                self.assertIn(name, REFUSED_UNAME, "no uname pair for this row")
                for system, machine in REFUSED_UNAME[name]:
                    self.assertIn(
                        "unsupported plugin platform",
                        self.exercise(system, machine, "unsupported"),
                    )

    def test_each_installed_row_is_its_own_asset_and_runner(self):
        """Row by row, not as sets. A row is started by the `install` runner
        whose `uname` picks that row's asset, not by the runner that builds it."""
        built_on = release_runners()
        started_by = {}
        for label in install_job_runners():
            self.assertIn(label, RUNNER_UNAME, f"no uname pair for runner {label}")
            started_by.setdefault(RUNNER_UNAME[label], []).append(label)
        seen_platforms, seen_assets = set(), set()
        for row in readme_platform_rows():
            if row[2] != "installs":
                continue
            # The name before any parenthetical: a wording edit is not a new row.
            platform = row[0].split(" (", 1)[0]
            asset, ci = row[1].strip("`"), row[3]
            with self.subTest(platform=platform):
                self.assertNotIn(platform, seen_platforms, "platform listed twice")
                self.assertNotIn(asset, seen_assets, "asset listed twice")
                seen_platforms.add(platform)
                seen_assets.add(asset)
                self.assertIn(platform, INSTALLED_UNAME, "no uname pair for this row")
                pair = INSTALLED_UNAME[platform]
                # Popped first, so a failure below does not also fail the
                # unmatched-runner check after the loop.
                labels = started_by.pop(pair, [])
                self.exercise(*pair, asset)
                self.assertIn(asset, built_on, "asset not built by release.yml")
                if labels:
                    expected = ", ".join(f"yes, on `{label}`" for label in labels)
                    self.assertEqual(ci, expected)
                else:
                    self.assertEqual(
                        ci,
                        f"**no**: built on `{built_on[asset]}` and started there "
                        "by `release.yml` before it is packaged",
                    )
        self.assertFalse(
            started_by, f"install runners that start no listed platform: {started_by}"
        )

    def test_release_and_windows_build_start_what_they_build(self):
        """Issue #449: the row above says `release.yml` starts the binary it
        builds. Every leg of its matrix runs `scripts/smoke_binary.py` on its
        own artifact, after the build and before the packaging, with no
        condition; `windows-build.yml` starts the executable it builds."""
        name = "      - name: Start the built binary (initialize, tools/list, health_check)\n"
        release = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertEqual(release.count(name), 1)
        step = release.split(name, 1)[1].split("      - name:", 1)[0]
        self.assertNotIn("if:", step, "a leg of the matrix would skip the smoke")
        self.assertIn("python3 scripts/smoke_binary.py", step)
        self.assertIn(
            "target/${{ matrix.rust_target }}/release/"
            "ai-architect-mcp-codebase${{ matrix.bin_suffix }}",
            step,
        )
        last_build = release.rindex("run: cargo build --locked --release")
        self.assertLess(last_build, release.index(name))
        self.assertLess(
            release.index(name), release.index("      - name: Package tarball + sha256")
        )
        windows = (ROOT / ".github/workflows/windows-build.yml").read_text()
        self.assertEqual(windows.count(name), 1)
        self.assertIn(
            "python3 scripts/smoke_binary.py "
            "target/x86_64-pc-windows-msvc/release/ai-architect-mcp-codebase.exe",
            windows.split(name, 1)[1],
        )

    def test_started_by_ci_means_the_installed_smoke(self):
        """"Started by CI" is the `install` command running the installed
        copy's smoke with `--installed`, as the README says."""
        install = json.loads((ROOT / "ci.json").read_text())["install"]
        self.assertIn("claude plugin install", install)
        # The installed copy (`$root`, from `claude plugin list`), last, with
        # nothing that could swallow its failure.
        self.assertTrue(
            install.endswith(
                '&& test -n "$root" '
                '&& python3 "$root/scripts/smoke_claude_plugin.py" --installed'
            ),
            install,
        )
        self.assertNotIn("||", install)


if __name__ == "__main__":
    unittest.main()
