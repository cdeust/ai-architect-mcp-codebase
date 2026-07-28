"""Tests for scripts/check_doc_claims.py — README numeric-claim gate (AP #160).

unittest-style to match the sibling gate's suite, which CI runs with plain
`python3 -m unittest`.

Every arm the gate can take is exercised, including the ones whose whole
observable effect is a message: a gate that detects drift but cannot say
which claim drifted sends a maintainer back to the diff to guess.
"""

from __future__ import annotations

import importlib.util
import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

_spec = importlib.util.spec_from_file_location(
    "check_doc_claims",
    Path(__file__).resolve().parents[2] / "scripts" / "check_doc_claims.py",
)
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)


def readme_with(badge: str, alt: str) -> str:
    """A README fragment shaped like the real badge row."""
    return (
        "<p>\n"
        f'  <img src="https://img.shields.io/badge/Coverage-{badge}%25-brightgreen" '
        f'alt="{alt}% line coverage">\n'
        "</p>\n"
    )


def llvm_cov_json(percent: float) -> str:
    return json.dumps(
        {
            "type": "llvm.coverage.json.export",
            "version": "2.0.1",
            "data": [{"totals": {"lines": {"count": 100, "covered": 81, "percent": percent}}}],
        }
    )


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


class AdvertisedCoverage(unittest.TestCase):
    def test_finds_both_the_badge_url_and_the_alt_text(self):
        found = gate.advertised_coverage(readme_with("81.59", "81.59"))
        self.assertEqual(
            sorted(found), [("badge URL", 81.59), ("badge alt text", 81.59)]
        )

    def test_finds_nothing_when_the_badge_is_absent(self):
        self.assertEqual(gate.advertised_coverage("# README\n\nno badges here\n"), [])

    def test_accepts_an_integer_percentage(self):
        self.assertEqual(
            gate.advertised_coverage(readme_with("82", "82")),
            [("badge URL", 82.0), ("badge alt text", 82.0)],
        )


class CheckCoverage(unittest.TestCase):
    def test_passes_when_the_badge_floors_the_measurement(self):
        # The real shape: CI measured 81.56%, the badge reads 81%.
        self.assertEqual(gate.check_coverage(readme_with("81", "81"), 81.56), [])

    def test_passes_at_the_bottom_of_the_badge_bucket(self):
        self.assertEqual(gate.check_coverage(readme_with("81", "81"), 81.00), [])

    def test_fails_when_the_badge_overstates_at_all(self):
        # One hundredth of a point over is still a claim the project does not
        # meet — overstatement has no tolerance band by design. This is the
        # arm that catches coverage slipping out of the badge's bucket.
        failures = gate.check_coverage(readme_with("82", "82"), 81.99)
        self.assertEqual(len(failures), 2)
        self.assertTrue(all(f.startswith("OVERSTATED") for f in failures))
        self.assertIn("82.00", failures[0])
        self.assertIn("81.99", failures[0])

    def test_fails_when_the_badge_understates_beyond_tolerance(self):
        # Coverage climbed a full point past the badge: bump it.
        failures = gate.check_coverage(readme_with("80", "80"), 81.01)
        self.assertEqual(len(failures), 2)
        self.assertTrue(all(f.startswith("UNDERSTATED") for f in failures))

    def test_tolerance_boundary_is_inclusive(self):
        # Exactly COVERAGE_TOLERANCE_PP below passes; a hundredth more fails.
        # Pins the comparison as `>` rather than `>=`, which no other test sees.
        self.assertEqual(gate.check_coverage(readme_with("81", "81"), 82.00), [])
        self.assertNotEqual(gate.check_coverage(readme_with("81", "81"), 82.01), [])

    def test_fails_when_the_badge_carries_decimals(self):
        # The brittleness guard. An exact badge is correct for one commit and
        # OVERSTATED by the next hundredth-point dip.
        failures = gate.check_coverage(readme_with("81.56", "81.56"), 81.56)
        self.assertEqual(len(failures), 2)
        self.assertTrue(all(f.startswith("NOT_A_WHOLE_PERCENT") for f in failures))
        # The message names the value that would be accepted.
        self.assertIn("81%", failures[0])

    def test_decimal_badge_fails_even_when_it_understates(self):
        # The whole-percent rule is not a proxy for overstatement — an
        # understating decimal badge is rejected too, or the rule has a hole.
        failures = gate.check_coverage(readme_with("81.10", "81.10"), 81.56)
        self.assertTrue(all(f.startswith("NOT_A_WHOLE_PERCENT") for f in failures))

    def test_fails_when_the_badge_is_missing_entirely(self):
        failures = gate.check_coverage("# README\n", 81.56)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("MISSING_CLAIM"))

    def test_fails_when_url_and_alt_text_disagree(self):
        # The accessibility arm: a screen-reader user must not be told a
        # different number than a sighted one.
        failures = gate.check_coverage(readme_with("81", "90"), 81.56)
        self.assertTrue(any(f.startswith("INCONSISTENT") for f in failures))
        self.assertTrue(any(f.startswith("OVERSTATED") for f in failures))


class MainExitCodes(unittest.TestCase):
    def _run(self, badge: str, percent: float, extra: list[str] | None = None) -> int:
        with TemporaryDirectory() as d:
            cov = Path(d) / "coverage.json"
            cov.write_text(llvm_cov_json(percent))
            readme = Path(d) / "README.md"
            readme.write_text(readme_with(badge, badge))
            return gate.main(
                ["--coverage-json", str(cov), "--readme", str(readme)] + (extra or [])
            )

    def test_exit_0_when_consistent(self):
        self.assertEqual(self._run("81", 81.56), 0)

    def test_exit_1_on_drift(self):
        self.assertEqual(self._run("95", 81.56), 1)

    def test_exit_2_when_the_report_is_unreadable(self):
        with TemporaryDirectory() as d:
            missing = Path(d) / "nope.json"
            self.assertEqual(gate.main(["--coverage-json", str(missing)]), 2)

    def test_exit_2_when_the_readme_is_unreadable(self):
        with TemporaryDirectory() as d:
            cov = Path(d) / "coverage.json"
            cov.write_text(llvm_cov_json(81.56))
            self.assertEqual(
                gate.main(
                    ["--coverage-json", str(cov), "--readme", str(Path(d) / "nope.md")]
                ),
                2,
            )

    def test_print_only_exits_0_without_checking_the_readme(self):
        # --print-only feeds the run summary and must not fail the job, even
        # when the badge is wildly wrong: the gate step is what fails.
        self.assertEqual(self._run("10", 81.56, ["--print-only"]), 0)


if __name__ == "__main__":
    unittest.main()
