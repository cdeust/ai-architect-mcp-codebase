"""Tests for scripts/check_doc_claims.py — the claim-comparison half (AP #160, #161).

unittest-style to match the sibling gate's suite, which CI runs with plain
`python3 -m unittest`.

Every arm the gate can take is exercised, including the ones whose whole
observable effect is a message: a gate that detects drift but cannot say
which claim drifted sends a maintainer back to the diff to guess.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

# This directory, so the shared fixtures import from any working directory:
# CI invokes the suite as `python3 -m unittest scripts/tests/<file>.py` from
# the repo root, which puts the root on the path but not this package-less dir.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from doc_claim_fixtures import (  # noqa: E402  (import follows the path fix)
    FAKE_FULL,
    complete_readme,
    fake_manifests,
    fake_src,
    llvm_cov_json,
    load,
    readme_with,
)

gate = load("check_doc_claims")


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
        self.assertTrue(all(f.startswith("NOT_ROUND") for f in failures))
        # The message names the value that would be accepted.
        self.assertIn("81%", failures[0])

    def test_decimal_badge_fails_even_when_it_understates(self):
        # The whole-percent rule is not a proxy for overstatement — an
        # understating decimal badge is rejected too, or the rule has a hole.
        failures = gate.check_coverage(readme_with("81.10", "81.10"), 81.56)
        self.assertTrue(all(f.startswith("NOT_ROUND") for f in failures))

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
            root = Path(d)
            src = fake_src(root)
            fake_manifests(root)
            cov = root / "coverage.json"
            cov.write_text(llvm_cov_json(percent))
            readme = root / "README.md"
            readme.write_text(complete_readme(coverage=badge))
            return gate.main(
                [
                    "--coverage-json",
                    str(cov),
                    "--readme",
                    str(readme),
                    "--src",
                    str(src),
                ]
                + (extra or [])
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

    def test_print_coverage_exits_0_without_checking_the_readme(self):
        # --print-coverage feeds the run summary and must not fail the job, even
        # when the badge is wildly wrong: the gate step is what fails.
        self.assertEqual(self._run("10", 81.56, ["--print-coverage"]), 0)


class CheckExact(unittest.TestCase):
    CLAIM = gate.EXACT_CLAIMS[0]

    def test_passes_when_every_site_matches(self):
        self.assertEqual(gate.check_exact(complete_readme(), self.CLAIM, 4), [])

    def test_reports_drift_naming_both_numbers_and_the_source(self):
        failures = gate.check_exact(complete_readme(tools=4), self.CLAIM, 26)
        self.assertTrue(failures)
        self.assertTrue(all(f.startswith("DRIFTED") for f in failures))
        self.assertIn("4", failures[0])
        self.assertIn("26", failures[0])
        self.assertIn("tool_profile.rs", failures[0])

    def test_reports_a_missing_claim_rather_than_passing_silently(self):
        # Deleting the badge must not be a way to make the gate green.
        failures = gate.check_exact("# README\n", self.CLAIM, 26)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("MISSING_CLAIM"))

    def test_reports_sites_that_disagree_with_each_other(self):
        readme = complete_readme(tools=4).replace(
            'alt="4 MCP tools"', 'alt="9 MCP tools"'
        )
        failures = gate.check_exact(readme, self.CLAIM, 4)
        self.assertTrue(any(f.startswith("INCONSISTENT") for f in failures))


class CheckTests(unittest.TestCase):
    def test_passes_when_the_badge_floors_the_suite(self):
        self.assertEqual(gate.check_tests(complete_readme(tests="1000"), 1035), [])

    def test_fails_when_the_badge_overstates_the_suite(self):
        failures = gate.check_tests(complete_readme(tests="1100"), 1035)
        self.assertTrue(failures)
        self.assertTrue(all(f.startswith("OVERSTATED") for f in failures))
        self.assertIn("1035", failures[0])

    def test_fails_when_the_badge_is_a_full_bucket_stale(self):
        failures = gate.check_tests(complete_readme(tests="900"), 1035)
        self.assertTrue(all(f.startswith("UNDERSTATED") for f in failures))
        self.assertIn("1000+", failures[0])

    def test_bucket_boundary_is_exclusive_at_the_top(self):
        # Exactly one bucket stale fails; one test short of it passes. Pins the
        # comparison as `>=`, which no other test distinguishes.
        self.assertNotEqual(gate.check_tests(complete_readme(tests="1000"), 1100), [])
        self.assertEqual(gate.check_tests(complete_readme(tests="1000"), 1099), [])

    def test_fails_when_the_badge_is_not_a_round_bucket(self):
        # An exact count is correct for one commit and puts the README in the
        # diff of every PR that adds a test.
        failures = gate.check_tests(complete_readme(tests="1035"), 1035)
        self.assertTrue(all(f.startswith("NOT_ROUND") for f in failures))
        self.assertIn("1000+", failures[0])

    def test_reports_a_missing_claim(self):
        failures = gate.check_tests("# README\n", 1035)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("MISSING_CLAIM"))

    def test_reports_sites_that_disagree_with_each_other(self):
        readme = complete_readme(tests="1000").replace(
            'alt="1000+ tests"', 'alt="900+ tests"'
        )
        failures = gate.check_tests(readme, 1035)
        self.assertTrue(any(f.startswith("INCONSISTENT") for f in failures))


class CheckLayout(unittest.TestCase):
    def _repo(self, tree: str) -> tuple[Path, str]:
        d = TemporaryDirectory()
        self.addCleanup(d.cleanup)
        root = Path(d.name)
        fake_src(root)
        fake_manifests(root)
        (root / "scripts").mkdir()
        (root / "scripts" / "check_doc_claims.py").touch()
        return root, complete_readme(layout=tree)

    def test_passes_when_every_path_exists(self):
        root, readme = self._repo(
            "automatised-pipeline/\n"
            "├── src/\n"
            "│   ├── tool_profile.rs\n"
            "│   └── parser/\n"
            "└── scripts/"
        )
        self.assertEqual(gate.check_layout(readme, root), [])

    def test_resolves_nested_entries_through_their_parent_directory(self):
        # The bug this whole check exists for: `src/graph_store.rs` shown as a
        # file long after it became `src/graph_store/`. Depth must come from
        # the tree glyphs, or a nested name is checked at the repo root and
        # wrongly reported missing.
        root, readme = self._repo(
            "automatised-pipeline/\n├── src/\n│   ├── parser/\n│   │   ├── language.rs"
        )
        self.assertEqual(gate.check_layout(readme, root), [])

    def test_reports_a_path_the_repository_does_not_have(self):
        root, readme = self._repo(
            "automatised-pipeline/\n├── src/\n│   ├── graph_store.rs"
        )
        failures = gate.check_layout(readme, root)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("STALE_LAYOUT"))
        self.assertIn("src/graph_store.rs", failures[0])
        self.assertIn("line 3", failures[0])

    def test_checks_every_sibling_on_a_shared_line(self):
        # Siblings are written `a.rs · b.rs` to keep the tree readable; a
        # checker that reads only the first would miss the rest.
        root, readme = self._repo(
            "automatised-pipeline/\n├── src/\n│   ├── tool_profile.rs · gone.rs"
        )
        failures = gate.check_layout(readme, root)
        self.assertEqual(len(failures), 1)
        self.assertIn("src/gone.rs", failures[0])

    def test_reports_a_missing_layout_section(self):
        root = Path(".")
        failures = gate.check_layout("# README\n", root)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("MISSING_CLAIM"))


class CheckToolInventory(unittest.TestCase):
    def _src(self, full: tuple[str, ...] = FAKE_FULL) -> Path:
        d = TemporaryDirectory()
        self.addCleanup(d.cleanup)
        return fake_src(Path(d.name), full=full)

    def test_passes_when_the_list_names_every_registered_tool(self):
        self.assertEqual(gate.check_tool_inventory(complete_readme(), self._src()), [])

    def test_reports_a_registered_tool_the_readme_never_documents(self):
        # The count-only hole: renaming the heading to 26 would satisfy a
        # counter while `index_status` stayed undocumented.
        readme = complete_readme(tool_list=("health_check", "get_symbol"))
        failures = gate.check_tool_inventory(readme, self._src())
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("UNDOCUMENTED_TOOL"))
        self.assertIn("index_status", failures[0])
        self.assertIn("ingest_traces", failures[0])

    def test_reports_a_documented_tool_the_server_does_not_register(self):
        readme = complete_readme(tool_list=FAKE_FULL + ("removed_tool",))
        failures = gate.check_tool_inventory(readme, self._src())
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("PHANTOM_TOOL"))
        self.assertIn("removed_tool", failures[0])

    def test_reads_its_own_fence_and_not_a_later_one(self):
        # Regression: a dot-all wildcard between the heading and the fence
        # swallowed this block and captured the Repository-layout fence
        # instead, reporting every tool as undocumented.
        readme = complete_readme()
        self.assertIn("## Repository layout", readme)
        self.assertEqual(gate.check_tool_inventory(readme, self._src()), [])

    def test_reports_a_missing_tool_section(self):
        failures = gate.check_tool_inventory("# README\n", self._src())
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("MISSING_CLAIM"))


class CheckManifests(unittest.TestCase):
    def _repo(self, **kwargs) -> Path:
        d = TemporaryDirectory()
        self.addCleanup(d.cleanup)
        root = Path(d.name)
        fake_src(root)
        fake_manifests(root, **kwargs)
        return root

    def test_passes_when_every_manifest_agrees_with_the_source(self):
        root = self._repo()
        self.assertEqual(gate.check_manifests(root, root / "src"), [])

    def test_reports_every_manifest_advertising_a_stale_tool_count(self):
        # All five were stale at 24 while the README was, because nothing read
        # them — a user installing from the marketplace never opens the README.
        root = self._repo(tools=24)
        failures = gate.check_manifests(root, root / "src")
        self.assertTrue(failures)
        self.assertTrue(all(f.startswith("DRIFTED") for f in failures))
        named = " ".join(failures)
        for manifest in ("server.json", "manifest.json", "marketplace.json"):
            self.assertIn(manifest, named)

    def test_reports_a_stale_language_count(self):
        root = self._repo(languages=10)
        failures = gate.check_manifests(root, root / "src")
        self.assertTrue(any("languages" in f for f in failures))

    def test_reports_a_manifest_that_no_longer_states_its_count(self):
        # A rephrase must fail loudly rather than silently un-gate the claim.
        root = self._repo()
        (root / "server.json").write_text('{"description": "no count here"}\n')
        failures = gate.check_manifests(root, root / "src")
        self.assertTrue(any(f.startswith("MISSING_CLAIM") for f in failures))
        self.assertTrue(any("server.json" in f for f in failures))

    def test_reports_a_gated_manifest_that_has_been_deleted(self):
        root = self._repo()
        (root / "server.json").unlink()
        failures = gate.check_manifests(root, root / "src")
        self.assertTrue(any(f.startswith("MISSING_FILE") for f in failures))


class MainInputArms(unittest.TestCase):
    def test_print_coverage_without_a_report_is_an_error(self):
        self.assertEqual(gate.main(["--print-coverage"]), 2)

    def test_exit_2_when_the_test_log_is_unreadable(self):
        with TemporaryDirectory() as d:
            root = Path(d)
            src = fake_src(root)
            fake_manifests(root)
            readme = root / "README.md"
            readme.write_text(complete_readme())
            self.assertEqual(
                gate.main(
                    [
                        "--test-log",
                        str(root / "nope.log"),
                        "--readme",
                        str(readme),
                        "--src",
                        str(src),
                    ]
                ),
                2,
            )

    def test_static_claims_are_checked_with_no_inputs_at_all(self):
        # Tool, language and layout claims need no run to verify, so they are
        # checked on every invocation — not only when a report is supplied.
        with TemporaryDirectory() as d:
            root = Path(d)
            src = fake_src(root)
            fake_manifests(root)
            readme = root / "README.md"
            readme.write_text(complete_readme(tools=99))
            self.assertEqual(gate.main(["--readme", str(readme), "--src", str(src)]), 1)


if __name__ == "__main__":
    unittest.main()
