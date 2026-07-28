"""Tests for scripts/doc_claim_sources.py — the measurement half (AP #161).

These suites never read a README. They pin what the gate believes the
repository produces: a broken source measurement makes every claim "agree"
with a number nobody produced.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

# This directory, so the shared fixtures import from any working directory:
# CI invokes the suite as `python3 -m unittest scripts/tests/<file>.py` from
# the repo root, which puts the root on the path but not this package-less dir.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from doc_claim_fixtures import (  # noqa: E402  (import follows the path fix)
    FAKE_CORE,
    FAKE_FULL,
    FAKE_LANGUAGES,
    fake_src,
    llvm_cov_json,
    load,
)

gate = load("doc_claim_sources")


class MeasuredCoverage(unittest.TestCase):
    def test_reads_percent_from_llvm_cov_export(self):
        with TemporaryDirectory() as d:
            p = Path(d) / "coverage.json"
            p.write_text(llvm_cov_json(81.56))
            self.assertAlmostEqual(gate.measured_coverage(p), 81.56)

    def test_rejects_json_of_the_wrong_shape(self):
        with TemporaryDirectory() as d:
            p = Path(d) / "coverage.json"
            p.write_text(json.dumps({"data": []}))
            with self.assertRaises(ValueError) as ctx:
                gate.measured_coverage(p)
            self.assertIn("llvm-cov JSON export", str(ctx.exception))

    def test_rejects_non_json(self):
        with TemporaryDirectory() as d:
            p = Path(d) / "coverage.json"
            p.write_text("not json at all")
            with self.assertRaises(ValueError):
                gate.measured_coverage(p)


class MeasuredTests(unittest.TestCase):
    def test_sums_every_test_result_line(self):
        # One `cargo test` run emits one summary per binary; the suite size is
        # their sum, not the largest.
        log = (
            "test result: ok. 12 passed; 0 failed; 0 ignored\n"
            "running 3 tests\n"
            "test result: ok. 3 passed; 0 failed; 1 ignored\n"
        )
        with TemporaryDirectory() as d:
            p = Path(d) / "test.log"
            p.write_text(log)
            self.assertEqual(gate.measured_tests(p), 15)

    def test_rejects_a_log_with_no_summary(self):
        # A run that never reached a summary (compile error, killed job) must
        # not silently measure zero tests and pass a badge claiming none.
        with TemporaryDirectory() as d:
            p = Path(d) / "test.log"
            p.write_text("error[E0432]: unresolved import\n")
            with self.assertRaises(ValueError) as ctx:
                gate.measured_tests(p)
            self.assertIn("test result:", str(ctx.exception))


class MachineSources(unittest.TestCase):
    def test_reads_tool_names_and_counts_from_the_rust_array(self):
        with TemporaryDirectory() as d:
            src = fake_src(Path(d))
            self.assertEqual(
                gate.tool_names(src / "tool_profile.rs", "FULL_TOOL_NAMES"),
                list(FAKE_FULL),
            )
            self.assertEqual(gate.measured_tools(src), len(FAKE_FULL))
            self.assertEqual(gate.measured_core_tools(src), len(FAKE_CORE))

    def test_rejects_a_source_missing_the_constant(self):
        # The gate must fail loudly when the source it measures is renamed —
        # a silent zero would let every count claim "agree" with nothing.
        with TemporaryDirectory() as d:
            src = fake_src(Path(d))
            (src / "tool_profile.rs").write_text("// the constant moved away\n")
            with self.assertRaises(ValueError) as ctx:
                gate.measured_tools(src)
            self.assertIn("FULL_TOOL_NAMES", str(ctx.exception))

    def test_counts_language_enum_variants(self):
        with TemporaryDirectory() as d:
            self.assertEqual(
                gate.measured_languages(fake_src(Path(d))), len(FAKE_LANGUAGES)
            )

    def test_rejects_a_parser_missing_the_language_enum(self):
        with TemporaryDirectory() as d:
            src = fake_src(Path(d))
            (src / "parser" / "language.rs").write_text("pub struct NotAnEnum;\n")
            with self.assertRaises(ValueError) as ctx:
                gate.measured_languages(src)
            self.assertIn("Language", str(ctx.exception))

    def test_rejects_an_empty_language_enum(self):
        # An enum that parses to zero variants means the shape changed, not
        # that the project supports no languages.
        with TemporaryDirectory() as d:
            src = fake_src(Path(d))
            (src / "parser" / "language.rs").write_text("pub enum Language {\n}\n")
            with self.assertRaises(ValueError) as ctx:
                gate.measured_languages(src)
            self.assertIn("zero variants", str(ctx.exception))
