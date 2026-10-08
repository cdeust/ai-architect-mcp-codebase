"""The Cargo bootstrap pin hashes the [package] table and nothing else."""

import hashlib
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from bootstrap_pins import digest, package_table  # noqa: E402

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


if __name__ == "__main__":
    unittest.main()
