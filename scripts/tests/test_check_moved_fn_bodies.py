"""Tests for scripts/check_moved_fn_bodies.py, the proof that a move changed no body.

The point of a proof script is that it can fail: each case below writes a
before and an after directory, and the mutated, lost and added cases must exit 1.
"""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

_SPEC = importlib.util.spec_from_file_location(
    "check_moved_fn_bodies",
    Path(__file__).resolve().parents[1] / "check_moved_fn_bodies.py",
)
moved = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(moved)

PRODUCTION = "pub fn open() -> u32 {\n    1\n}\n"
TESTS = (
    "#[test]\nfn keeps_a_row() {\n    assert_eq!(open(), 1);\n}\n\n"
    '#[test]\nfn names_the_brace() {\n    let s = "} {"; // } in a comment\n    assert_eq!(s.len(), 3);\n}\n'
)


def run(before_files, after_files, names):
    with TemporaryDirectory() as tmp:
        for label, files in (("before", before_files), ("after", after_files)):
            for name, text in files.items():
                target = Path(tmp) / label / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(text)
        return moved.main(
            [
                "x",
                f"dir:{Path(tmp) / 'before'}",
                f"dir:{Path(tmp) / 'after'}",
                *names,
            ]
        )


class MovedFnBodies(unittest.TestCase):
    def test_tests_moved_to_a_new_file_are_unchanged(self):
        before = {"a.rs": PRODUCTION + "mod tests {\n" + TESTS + "}\n"}
        after = {"a.rs": PRODUCTION, "a_tests.rs": TESTS}
        self.assertEqual(run(before, after, ["a.rs", "a_tests.rs"]), 0)

    def test_whitespace_and_indentation_do_not_count(self):
        before = {"a.rs": "fn f() {\n        1\n}\n"}
        after = {"a.rs": "fn f() { 1 }\n"}
        self.assertEqual(run(before, after, ["a.rs"]), 0)

    def test_one_mutated_test_body_fails(self):
        before = {"a.rs": PRODUCTION + TESTS}
        mutated = TESTS.replace("assert_eq!(open(), 1)", "assert_eq!(open(), 2)")
        self.assertNotEqual(mutated, TESTS)
        after = {"a.rs": PRODUCTION, "a_tests.rs": mutated}
        self.assertEqual(run(before, after, ["a.rs", "a_tests.rs"]), 1)

    def test_a_dropped_function_fails(self):
        before = {"a.rs": PRODUCTION + TESTS}
        after = {"a.rs": PRODUCTION, "a_tests.rs": TESTS.split("#[test]\nfn names")[0]}
        self.assertEqual(run(before, after, ["a.rs", "a_tests.rs"]), 1)

    def test_an_added_function_fails(self):
        before = {"a.rs": PRODUCTION}
        after = {"a.rs": PRODUCTION + "fn extra() {}\n"}
        self.assertEqual(run(before, after, ["a.rs"]), 1)

    def test_a_duplicated_helper_is_not_hidden(self):
        before = {"a.rs": "fn new() {\n    1\n}\n"}
        after = {"a.rs": "fn new() {\n    1\n}\nfn new() {\n    1\n}\n"}
        self.assertEqual(run(before, after, ["a.rs"]), 1)

    def test_a_brace_in_a_string_or_comment_does_not_end_a_function(self):
        bodies = moved.fn_bodies(TESTS)
        self.assertEqual(sorted(name for name, _ in bodies), ["keeps_a_row", "names_the_brace"])

    def test_a_missing_file_counts_as_empty(self):
        self.assertEqual(run({"a.rs": PRODUCTION}, {"a.rs": PRODUCTION}, ["a.rs", "gone.rs"]), 0)


if __name__ == "__main__":
    unittest.main()
