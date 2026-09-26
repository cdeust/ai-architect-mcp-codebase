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


class Issue362(unittest.TestCase):
    """Each case below passed silently before issue #362."""

    def test_a_wrong_path_on_both_sides_fails(self):
        # Zero bodies against zero bodies proves nothing: a typo must not pass.
        self.assertNotEqual(run({"a.rs": PRODUCTION}, {"a.rs": PRODUCTION}, ["typo.rs"]), 0)

    def test_an_empty_side_fails(self):
        self.assertNotEqual(run({"a.rs": PRODUCTION}, {"b.rs": PRODUCTION}, ["a.rs"]), 0)

    def test_a_removed_test_attribute_fails(self):
        before = {"a.rs": "#[test]\nfn t() {\n    assert!(true);\n}\n"}
        after = {"a.rs": "fn t() {\n    assert!(true);\n}\n"}
        self.assertEqual(run(before, after, ["a.rs"]), 1)

    def test_a_changed_should_panic_attribute_fails(self):
        before = {"a.rs": '#[test]\n#[should_panic(expected = "x")]\nfn t() {}\n'}
        after = {"a.rs": '#[test]\n#[should_panic(expected = "y")]\nfn t() {}\n'}
        self.assertEqual(run(before, after, ["a.rs"]), 1)

    def test_a_multi_line_attribute_is_part_of_the_body(self):
        before = {"a.rs": "#[cfg(all(\n    test,\n    unix\n))]\nfn t() {}\n"}
        after = {"a.rs": "#[cfg(all(\n    test,\n    windows\n))]\nfn t() {}\n"}
        self.assertEqual(run(before, after, ["a.rs"]), 1)

    def test_a_removed_doc_comment_or_visibility_fails(self):
        base = "/// Opens it.\npub fn open() -> u32 {\n    1\n}\n"
        self.assertEqual(run({"a.rs": base}, {"a.rs": "pub fn open() -> u32 {\n    1\n}\n"}, ["a.rs"]), 1)
        self.assertEqual(run({"a.rs": base}, {"a.rs": "/// Opens it.\nfn open() -> u32 {\n    1\n}\n"}, ["a.rs"]), 1)

    def test_indenting_attributes_does_not_count(self):
        before = {"a.rs": "#[test]\nfn t() {}\n"}
        after = {"a.rs": "mod tests {\n    #[test]\n    fn t() {}\n}\n"}
        self.assertEqual(run(before, after, ["a.rs"]), 0)

    def test_whitespace_inside_a_string_literal_counts(self):
        before = {"a.rs": 'fn f() -> &\'static str {\n    "a  b"\n}\n'}
        after = {"a.rs": 'fn f() -> &\'static str {\n    "a b"\n}\n'}
        self.assertEqual(run(before, after, ["a.rs"]), 1)

    def test_whitespace_inside_a_raw_string_counts(self):
        before = {"a.rs": 'fn f() -> &\'static str {\n    r#"a  "b"#\n}\n'}
        after = {"a.rs": 'fn f() -> &\'static str {\n    r#"a "b"#\n}\n'}
        self.assertEqual(run(before, after, ["a.rs"]), 1)

    def test_a_raw_string_holding_a_brace_and_a_quote_does_not_end_a_function(self):
        text = 'fn f() -> &\'static str {\n    r##"} "# {"##\n}\nfn g() {}\n'
        self.assertEqual(sorted(name for name, _ in moved.fn_bodies(text)), ["f", "g"])

    def test_a_byte_raw_string_does_not_end_a_function(self):
        text = 'fn f() -> &\'static [u8] {\n    br#"}"#\n}\nfn g() {}\n'
        self.assertEqual(sorted(name for name, _ in moved.fn_bodies(text)), ["f", "g"])

    def test_an_escaped_quote_char_literal_does_not_end_a_function(self):
        text = "fn f() -> char {\n    let _ = '}';\n    '\\''\n}\nfn g() -> char { '\\\\' }\nfn h() {}\n"
        self.assertEqual(sorted(name for name, _ in moved.fn_bodies(text)), ["f", "g", "h"])

    def test_a_semicolon_in_an_array_type_does_not_end_the_signature(self):
        before = "fn f(x: [u8; 4]) -> [u32; 2] { [1, 2] }\nfn g() {}\n"
        after = "fn f(x: [u8; 4]) -> [u32; 2] { [9, 9] }\nfn g() {}\n"
        self.assertNotEqual(moved.fn_bodies(before), moved.fn_bodies(after))
        self.assertEqual(sorted(name for name, _ in moved.fn_bodies(before)), ["f", "g"])

    def test_a_declaration_without_a_body_still_ends_at_its_semicolon(self):
        text = "trait T {\n    fn f(x: [u8; 4]);\n}\nfn g() { 1 }\n"
        self.assertEqual(sorted(name for name, _ in moved.fn_bodies(text)), ["f", "g"])

    def test_lifetimes_are_not_char_literals(self):
        text = "fn f<'a>(x: &'a str) -> &'a str {\n    x\n}\nfn g() {}\n"
        self.assertEqual(sorted(name for name, _ in moved.fn_bodies(text)), ["f", "g"])


if __name__ == "__main__":
    unittest.main()
