"""Proves the no-comparison-against-main guard detects what it forbids, and runs.

Owner rule 2026-10-29: no CI check may compare a pull request with main.
The scan itself lives in scripts/tests/test_no_main_comparison.py, Cortex's file.
This companion follows Cortex's tests_py/scripts/test_no_main_comparison_guard.py.
It sits under `scripts/`, which the guard scans, so each sample is written in two
parts: a whole marker here would be a violation of the guard itself.

Standard library only.
"""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
GUARD = ROOT / "scripts" / "tests" / "test_no_main_comparison.py"
WORKFLOWS = ROOT / ".github" / "workflows"
PIPELINES = ("pipeline.yml", "pipeline-post-merge.yml")
STEP = "python3 -m unittest discover -s scripts/tests -p test_no_main_comparison.py -v"
COMPANION = "python3 -m unittest discover -s scripts/tests -p test_no_main_comparison_guard.py"


def load_guard():
    spec = importlib.util.spec_from_file_location("no_main_comparison_guard", GUARD)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def job(name, workflow):
    """The text of one job of a workflow: from its key to the next job's key."""
    lines = (WORKFLOWS / workflow).read_text(encoding="utf-8").split("\n")
    start = lines.index(f"  {name}:")
    end = next(
        (
            index
            for index in range(start + 1, len(lines))
            if lines[index].startswith("  ") and lines[index][2:3] not in ("", " ", "#")
        ),
        len(lines),
    )
    return "\n".join(lines[start:end])


def found(guard, name, text, directory="scripts"):
    """The guard's violations on a tree that holds the single file `directory/name`."""
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / directory).mkdir(parents=True)
        (Path(tmp) / directory / name).write_text(text + "\n", encoding="utf-8")
        return guard.violations(Path(tmp), (directory,))


class GuardDetects(unittest.TestCase):
    def test_guard_passes_on_this_tree(self):
        self.assertEqual(load_guard().violations(ROOT), [])

    def test_guard_detects_every_marker(self):
        guard = load_guard()
        samples = [*guard.FORBIDDEN, "header + " + "blob(x)", "MAIN" + "_SHA=x", "trusted" + "_sha = x"]
        for sample in samples:
            with self.subTest(sample=sample):
                self.assertTrue(found(guard, "ci.sh", sample))

    def test_guard_detects_a_hashed_generated_header(self):
        guard = load_guard()
        self.assertEqual(found(guard, "pipeline.yml", guard.HEADER, ".github/workflows"), [])
        self.assertTrue(found(guard, "pipeline.yml", guard.HEADER + " 65105147", ".github/workflows"))

    def test_guard_detects_strict_only_in_a_protection_config(self):
        guard = load_guard()
        strict = "strict" + ": true"
        self.assertEqual(found(guard, "config.yml", strict), [])
        self.assertTrue(found(guard, "protect.yml", "required_status_checks:\n  " + strict))

    def test_guard_accepts_a_clean_file(self):
        self.assertEqual(found(load_guard(), "ci.sh", "git fetch origin \"$TESTED_SHA\""), [])


class GuardRuns(unittest.TestCase):
    def test_both_pipelines_run_the_guard_in_the_contract_job(self):
        for workflow in PIPELINES:
            with self.subTest(workflow=workflow):
                self.assertIn("run: " + STEP, job("contract", workflow))

    def test_ci_json_runs_this_companion(self):
        commands = json.loads((ROOT / "ci.json").read_text(encoding="utf-8"))
        self.assertIn(COMPANION, commands["regression"])


class TagJobReadsNoMain(unittest.TestCase):
    """The release tag goes on the tested commit, fetched by SHA, as in Cortex."""

    def test_tag_job_fetches_the_tested_commit_into_a_bare_repository(self):
        for workflow in PIPELINES:
            with self.subTest(workflow=workflow):
                tag = job("tag", workflow)
                for absent in ("uses: actions/checkout", "ref: main", "merge-base"):
                    self.assertFalse(absent in tag, absent + " in the tag job")
                self.assertIn('git init --quiet --bare "$repo"', tag)
                self.assertIn('fetch --quiet --depth=1 origin "$TESTED_SHA"', tag)
                self.assertIn("TESTED_SHA: ${{ needs.contract.outputs.tested_sha }}", tag)
                self.assertIn("working-directory: ${{ runner.temp }}/release-repo", tag)


if __name__ == "__main__":
    unittest.main()
