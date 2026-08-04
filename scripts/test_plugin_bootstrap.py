#!/usr/bin/env python3
"""Prove a missing release fails fast and never starts a cold plugin build."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="ai-architect-bootstrap-") as tmp:
        plugin = Path(tmp) / "plugin"
        fake_bin = Path(tmp) / "fake-bin"
        (plugin / "bin").mkdir(parents=True)
        (plugin / ".claude-plugin").mkdir()
        fake_bin.mkdir()
        shutil.copy2(ROOT / "bin/ensure-binary.sh", plugin / "bin/ensure-binary.sh")
        shutil.copy2(ROOT / ".claude-plugin/plugin.json", plugin / ".claude-plugin/plugin.json")
        (plugin / "Cargo.toml").write_text("[package]\nname='sentinel'\nversion='0.0.0'\n", encoding="utf-8")
        (plugin / "src").mkdir()

        # curl models a release 404. cargo is a sentinel: invoking it proves the
        # launcher entered the timeout-prone source build and fails this test.
        (fake_bin / "curl").write_text("#!/bin/sh\nexit 22\n", encoding="utf-8")
        (fake_bin / "cargo").write_text("#!/bin/sh\necho COLD_BUILD_STARTED >&2\nexit 99\n", encoding="utf-8")
        (fake_bin / "curl").chmod(0o755)
        (fake_bin / "cargo").chmod(0o755)

        run = subprocess.run(
            [str(plugin / "bin/ensure-binary.sh")],
            env={**os.environ, "CLAUDE_PLUGIN_ROOT": str(plugin), "PATH": f"{fake_bin}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
            timeout=10,
            check=False,
        )
        assert run.returncode == 1, run.stderr
        assert "no verified release binary" in run.stderr
        assert "COLD_BUILD_STARTED" not in run.stderr
    print("PLUGIN BOOTSTRAP OK: missing release fails fast without invoking cargo")


if __name__ == "__main__":
    main()
