"""The Cargo bootstrap pin hashes the [package] table and nothing else."""

import contextlib
import hashlib
import io
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from bootstrap_pins import digest, package_table  # noqa: E402
import check_distribution_identity as identity  # noqa: E402

MANIFEST = b"""[workspace]
members = ["."]

[package]
name = "x"
version = "1.0.0"
include = [
  "/src/**",
]

[dependencies]
lbug = "0.20"
"""


class PackageTable(unittest.TestCase):
    def test_runs_from_header_to_the_next_table_and_stops(self) -> None:
        self.assertEqual(
            package_table(MANIFEST),
            b'[package]\nname = "x"\nversion = "1.0.0"\ninclude = [\n  "/src/**",\n]\n\n',
        )

    def test_a_dependency_bump_leaves_it_unchanged(self) -> None:
        self.assertEqual(package_table(MANIFEST), package_table(MANIFEST.replace(b'"0.20"', b'"0.21"')))

    def test_a_package_edit_changes_it(self) -> None:
        self.assertNotEqual(package_table(MANIFEST), package_table(MANIFEST.replace(b'"1.0.0"', b'"1.0.1"')))

    def test_a_table_before_package_is_not_included(self) -> None:
        self.assertNotIn(b"workspace", package_table(MANIFEST))

    def test_no_package_table_is_empty(self) -> None:
        self.assertEqual(package_table(b"[workspace]\nmembers = []\n"), b"")

    def test_header_with_trailing_blanks_is_found(self) -> None:
        self.assertEqual(package_table(b"[package] \t\nname = 'x'\n"), b"[package] \t\nname = 'x'\n")

    def test_a_nul_byte_is_refused(self) -> None:
        # BSD awk drops the rest of a line after a NUL, so the shell side cannot
        # hash such a manifest as this function would; both refuse it instead.
        with self.assertRaises(ValueError):
            package_table(MANIFEST.replace(b'name = "x"', b'name = "x"\x00version = "9.9.9"'))

    def test_a_byte_that_is_not_utf8_is_hashed_like_any_other(self) -> None:
        self.assertIn(b'evil = "\xff"\n', package_table(MANIFEST.replace(b'name = "x"\n', b'evil = "\xff"\nname = "x"\n')))


class Digest(unittest.TestCase):
    def manifest(self, data: bytes) -> Path:
        root = Path(tempfile.mkdtemp(prefix="bootstrap-pins-"))
        self.addCleanup(lambda: [(root / "Cargo.toml").unlink(), root.rmdir()])
        (root / "Cargo.toml").write_bytes(data)
        return root

    def test_package_scope_hashes_the_table(self) -> None:
        root = self.manifest(MANIFEST)
        self.assertEqual(digest(root, "Cargo.toml", "package"), hashlib.sha256(package_table(MANIFEST)).hexdigest())

    def test_whole_scope_hashes_the_file(self) -> None:
        root = self.manifest(MANIFEST)
        self.assertEqual(digest(root, "Cargo.toml", "whole"), hashlib.sha256(MANIFEST).hexdigest())

    def test_an_empty_package_table_is_refused(self) -> None:
        # "[package] # comment" is not the header either side looks for; a pin of
        # sha256("") would pass the gate while binding nothing.
        for data in (b"[workspace]\nmembers = []\n", b"[package] # comment\nname = 'x'\n"):
            with self.assertRaises(ValueError):
                digest(self.manifest(data), "Cargo.toml", "package")


class NativeReleasePin(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="native-pin-contract-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        # Exercise the real entry point with all existing distribution checks.
        for name in (
            "mcp-contract.json",
            ".mcp.json",
            "manifest.json",
            "Cargo.toml",
            ".cargo/config.toml",
            ".claude-plugin/plugin.json",
            ".claude-plugin/marketplace.json",
            ".agents/plugins/marketplace.json",
            "plugins/ai-architect-mcp-codebase/.codex-plugin/plugin.json",
            "gemini-extension.json",
            "server.json",
            "bin/ensure-binary.sh",
        ):
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(identity.ROOT / name, target)
        # Regression fixture: locked wrapper 0.21.2 must not select native 0.22.0.
        (self.root / "Cargo.lock").write_text(
            'version = 4\n[[package]]\nname = "lbug"\nversion = "0.21.2"\n',
            encoding="utf-8",
        )

    def set_pin(self, value):
        (self.root / ".cargo/config.toml").write_text(
            "[env]\n" + (f'LBUG_VERSION = "{value}"\n' if value is not None else ""),
            encoding="utf-8",
        )

    def run_gate(self):
        with (
            patch.object(identity, "ROOT", self.root),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            identity.main()

    def test_matching_release_passes_the_complete_distribution_gate(self):
        self.set_pin("0.21.2")
        self.run_gate()

    def test_absent_native_pin_is_rejected(self):
        self.set_pin(None)
        with self.assertRaisesRegex(SystemExit, "LBUG_VERSION"):
            self.run_gate()

    def test_latest_native_release_is_rejected(self):
        self.set_pin("latest")
        with self.assertRaisesRegex(SystemExit, "LBUG_VERSION"):
            self.run_gate()

    def test_different_native_release_is_rejected(self):
        self.set_pin("0.22.0")
        with self.assertRaisesRegex(SystemExit, "LBUG_VERSION"):
            self.run_gate()

    def test_cargo_lock_upgrade_requires_updating_native_pin(self):
        self.set_pin("0.21.2")
        lock = self.root / "Cargo.lock"
        before = lock.read_text(encoding="utf-8")
        old = 'name = "lbug"\nversion = "0.21.2"'
        self.assertIn(old, before)
        lock.write_text(
            before.replace(old, 'name = "lbug"\nversion = "0.22.0"'), encoding="utf-8"
        )
        with self.assertRaisesRegex(SystemExit, "LBUG_VERSION"):
            self.run_gate()

    def test_matching_lock_and_native_upgrade_is_accepted(self):
        self.set_pin("0.22.0")
        lock = self.root / "Cargo.lock"
        lock.write_text(
            lock.read_text(encoding="utf-8").replace(
                'name = "lbug"\nversion = "0.21.2"',
                'name = "lbug"\nversion = "0.22.0"',
            ),
            encoding="utf-8",
        )
        self.run_gate()


if __name__ == "__main__":
    unittest.main()
