"""The Cargo bootstrap pin hashes the [package] table and nothing else."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from bootstrap_pins import package_table  # noqa: E402

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


if __name__ == "__main__":
    unittest.main()
