"""Tests for scripts/check_marketplace_pins.py — pin-staleness gate (#179).

Written unittest-style on purpose: the byte-identical AP copy runs them via
plain `python3 -m unittest` (no pytest there); Cortex's pytest collects
unittest classes natively.
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import unittest
import urllib.error
from pathlib import Path
from tempfile import TemporaryDirectory

_spec = importlib.util.spec_from_file_location(
    "check_marketplace_pins",
    Path(__file__).resolve().parents[2] / "scripts" / "check_marketplace_pins.py",
)
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)


def _git_repo_with_tags(root: str, tags: list[str]) -> Path:
    p = Path(root)
    subprocess.run(["git", "-C", root, "init", "-q"], check=True)
    subprocess.run(
        ["git", "-C", root, "commit", "-q", "--allow-empty", "-m", "x"],
        check=True,
        env={
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@t",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@t",
            "PATH": "/usr/bin:/bin:/usr/local/bin",
        },
    )
    for t in tags:
        subprocess.run(["git", "-C", root, "tag", t], check=True)
    return p


class TestParseSemver(unittest.TestCase):
    def test_v_prefix_and_bare(self):
        self.assertEqual(gate.parse_semver("v2.34.0"), (2, 34, 0))
        self.assertEqual(gate.parse_semver("2.7.1"), (2, 7, 1))

    def test_non_semver_none(self):
        self.assertIsNone(gate.parse_semver("nightly"))
        self.assertIsNone(gate.parse_semver("2.34"))

    def test_numeric_not_lexicographic(self):
        self.assertLess(gate.parse_semver("2.9.0"), gate.parse_semver("2.10.0"))


class TestPinBehindTag(unittest.TestCase):
    """The review's replay: both manifests agree AND both are stale (#67)."""

    def test_incident_replay_both_manifests_stale_tag_ahead(self):
        with TemporaryDirectory() as d:
            root = _git_repo_with_tags(d, ["v0.7.0", "v0.8.0", "v0.8.1", "v0.8.2"])
            plug = root / ".claude-plugin"
            plug.mkdir()
            (plug / "plugin.json").write_text(json.dumps({"version": "0.8.0"}))
            failures = gate.check_self_pin("ap", "./", "0.8.0", root)
            joined = "\n".join(failures)
            self.assertIn("PIN_BEHIND_TAG", joined)  # detection ORIGINATES here
            self.assertNotIn("SELF_PIN_MISMATCH", joined)  # manifests agree
            self.assertIn("2 release(s)", joined)  # v0.8.1 + v0.8.2 counted

    def test_current_pin_green(self):
        with TemporaryDirectory() as d:
            root = _git_repo_with_tags(d, ["v0.8.2"])
            plug = root / ".claude-plugin"
            plug.mkdir()
            (plug / "plugin.json").write_text(json.dumps({"version": "0.8.2"}))
            self.assertEqual(gate.check_self_pin("ap", "./", "0.8.2", root), [])

    def test_untagged_repo_no_crash_no_flag(self):
        with TemporaryDirectory() as d:
            root = _git_repo_with_tags(d, [])
            self.assertEqual(gate.check_self_pin("p", "./", "1.0.0", root), [])

    def test_frozen_pin_skips_tag_check_keeps_coherence(self):
        with TemporaryDirectory() as d:
            root = _git_repo_with_tags(d, ["v4.16.0"])
            shim = root / "shim" / ".claude-plugin"
            shim.mkdir(parents=True)
            (shim / "plugin.json").write_text(json.dumps({"version": "4.15.0"}))
            gate.FROZEN_PINS["frozen-test"] = "test"
            try:
                self.assertEqual(
                    gate.check_self_pin("frozen-test", "shim", "4.15.0", root), []
                )
                (shim / "plugin.json").write_text(json.dumps({"version": "9.9.9"}))
                failures = gate.check_self_pin("frozen-test", "shim", "4.15.0", root)
                self.assertIn("SELF_PIN_MISMATCH", "\n".join(failures))
            finally:
                gate.FROZEN_PINS.pop("frozen-test")


class TestGithubPin(unittest.TestCase):
    """check_github_pin(name, repo, pin, list_tags=..., pending=...).

    `list_tags` returns the FULL tag list (not just latest) because
    PIN_VERSION_UNPUBLISHED needs set-membership: a pin can be simultaneously
    "not the latest" AND "not published at all" (marketplace_pins_github.py
    ``list_release_tags`` docstring). Every test below supplies the full list.
    """

    def test_stale_flagged_with_count(self):
        tags = ["2.29.0", "2.30.0", "2.31.0", "2.32.0", "2.33.0", "2.34.0", "2.35.0"]
        failure, notice = gate.check_github_pin(
            "p", "o/r", "2.29.0", list_tags=lambda r: tags
        )
        self.assertIn("PIN_BEHIND_RELEASE", failure)
        # v2.30.0..v2.35.0: six releases strictly after the pin, up to latest.
        self.assertIn("6 release(s)", failure)
        self.assertIsNone(notice)

    def test_current_pin_passes(self):
        self.assertEqual(
            gate.check_github_pin(
                "p", "o/r", "2.34.0", list_tags=lambda r: ["2.34.0"]
            ),
            (None, None),
        )

    def test_ahead_pin_now_flagged_unpublished(self):
        """The bug PIN_VERSION_UNPUBLISHED exists to close: a pin naming a
        version nobody ever tagged used to compare `pin < latest` (false)
        and pass silently. It must now fail loudly instead — this is the
        cortex-viz "3.0.0" incident (marketplace_pins_github.py docstring):
        the pin sat ahead of every real tag and PIN_BEHIND_RELEASE could
        not see it because "ahead" is invisible to a "behind" comparison.
        """
        failure, notice = gate.check_github_pin(
            "p", "o/r", "3.0.0", list_tags=lambda r: ["2.6.0", "2.7.0", "2.8.0"]
        )
        self.assertIsNotNone(failure)
        self.assertIn("PIN_VERSION_UNPUBLISHED", failure)
        self.assertIn("2.8.0", failure)  # names the latest that DOES exist
        self.assertIsNone(notice)

    def test_pin_version_unpublished_and_behind_reports_unpublished_only(self):
        """A pin can be simultaneously unpublished AND numerically behind the
        latest tag. PIN_VERSION_UNPUBLISHED is the stronger, more specific
        statement (this version was never cut, full stop) and must win —
        check_github_pin checks it FIRST and returns before ever reaching
        the PIN_BEHIND_RELEASE comparison.
        """
        failure, notice = gate.check_github_pin(
            "p", "o/r", "2.29.5", list_tags=lambda r: ["2.30.0", "2.31.0"]
        )
        self.assertIn("PIN_VERSION_UNPUBLISHED", failure)
        self.assertNotIn("PIN_BEHIND_RELEASE", failure)
        self.assertIsNone(notice)

    def test_pin_version_unpublished_pending_degrades_to_notice(self):
        failure, notice = gate.check_github_pin(
            "p",
            "o/r",
            "3.0.0",
            list_tags=lambda r: ["2.8.0"],
            pending={"p": "https://github.com/o/r/pull/1"},
        )
        self.assertIsNone(failure)
        self.assertIn("PENDING", notice)
        self.assertIn("https://github.com/o/r/pull/1", notice)

    def test_network_failure_degrades_to_notice(self):
        def down(_repo):
            raise urllib.error.URLError("offline")

        failure, notice = gate.check_github_pin("p", "o/r", "2.29.0", list_tags=down)
        self.assertIsNone(failure)  # fail-open: no red run from an outage
        self.assertIn("network degraded", notice)

    def test_no_releases_repo_is_notice_not_keyerror(self):
        failure, notice = gate.check_github_pin(
            "p", "o/r", "1.0.0", list_tags=lambda r: None
        )
        self.assertIsNone(failure)
        self.assertIn("no published releases", notice)

    def test_unparseable_pin_reported(self):
        failure, _ = gate.check_github_pin(
            "p", "o/r", "nightly", list_tags=lambda r: ["1.0.0"]
        )
        self.assertIn("UNPARSEABLE", failure)
        self.assertIn("nightly", failure)

    def test_unparseable_tag_list_reported(self):
        failure, _ = gate.check_github_pin(
            "p", "o/r", "1.0.0", list_tags=lambda r: ["nightly"]
        )
        self.assertIn("UNPARSEABLE", failure)
        self.assertIn("nightly", failure)


class TestPinSha(unittest.TestCase):
    """check_pin_sha(name, repo, sha, branch=..., compare=...) —
    PIN_SHA_UNREACHABLE (Cortex #351: a pinned sha that is an unmerged PR
    head, invisible to any version check because the pin's *version* reads
    current while its *commit* does not resolve from the default branch).
    """

    def test_identical_or_behind_default_branch_passes(self):
        for status in ("identical", "behind"):
            failure, notice = gate.check_pin_sha(
                "p", "o/r", "deadbeef", branch=lambda r: "main", compare=lambda *a: status
            )
            self.assertIsNone(failure, status)
            self.assertIsNone(notice, status)

    def test_ahead_of_default_branch_flagged_unreachable(self):
        failure, notice = gate.check_pin_sha(
            "p", "o/r", "deadbeef", branch=lambda r: "main", compare=lambda *a: "ahead"
        )
        self.assertIn("PIN_SHA_UNREACHABLE", failure)
        self.assertIn("ahead", failure)
        self.assertIsNone(notice)

    def test_sha_not_found_flagged_unreachable(self):
        failure, notice = gate.check_pin_sha(
            "p", "o/r", "deadbeef", branch=lambda r: "main", compare=lambda *a: None
        )
        self.assertIn("PIN_SHA_UNREACHABLE", failure)
        self.assertIn("does not resolve commit", failure)
        self.assertIsNone(notice)

    def test_repo_not_found_degrades_to_notice(self):
        failure, notice = gate.check_pin_sha(
            "p", "o/r", "deadbeef", branch=lambda r: None, compare=lambda *a: "identical"
        )
        self.assertIsNone(failure)
        self.assertIn("does not resolve", notice)

    def test_network_failure_degrades_to_notice(self):
        def down(_repo):
            raise urllib.error.URLError("offline")

        failure, notice = gate.check_pin_sha("p", "o/r", "deadbeef", branch=down)
        self.assertIsNone(failure)
        self.assertIn("network degraded", notice)


class TestRootManifestSplit(unittest.TestCase):
    def test_three_way_split_third_leg_flagged(self):
        with TemporaryDirectory() as d:
            root = Path(d)
            (root / "server.json").write_text(json.dumps({"version": "0.8.2"}))
            issues = gate.check_root_manifests(root, "0.8.0")
            self.assertTrue(any("SERVER_JSON_SPLIT" in i for i in issues))

    def test_ap_172_incident_replay_manifest_json_two_releases_stale(self):
        """The exact tree that exited 0 before this check existed.

        automatised-pipeline carried manifest.json 0.8.0 while server.json and
        every marketplace pin read 0.8.2. The gate passed, and the wrong
        version shipped inside every .mcpb bundle for two releases.
        """
        with TemporaryDirectory() as d:
            root = Path(d)
            (root / "server.json").write_text(json.dumps({"version": "0.8.2"}))
            (root / "manifest.json").write_text(json.dumps({"version": "0.8.0"}))
            issues = gate.check_root_manifests(root, "0.8.2")
            self.assertEqual(len(issues), 1, issues)
            self.assertIn("MANIFEST_JSON_SPLIT", issues[0])
            self.assertIn("0.8.0", issues[0])

    def test_both_stale_are_reported_separately(self):
        with TemporaryDirectory() as d:
            root = Path(d)
            (root / "server.json").write_text(json.dumps({"version": "0.8.1"}))
            (root / "manifest.json").write_text(json.dumps({"version": "0.8.0"}))
            issues = gate.check_root_manifests(root, "0.8.2")
            self.assertEqual(len(issues), 2, issues)

    def test_aligned_passes_and_absent_passes(self):
        with TemporaryDirectory() as d:
            root = Path(d)
            # Absent: the canonical repo has neither file — not a failure.
            self.assertEqual(gate.check_root_manifests(root, "0.8.2"), [])
            (root / "server.json").write_text(json.dumps({"version": "0.8.2"}))
            (root / "manifest.json").write_text(json.dumps({"version": "0.8.2"}))
            self.assertEqual(gate.check_root_manifests(root, "0.8.2"), [])

    def test_missing_version_key_is_not_a_failure(self):
        with TemporaryDirectory() as d:
            root = Path(d)
            (root / "manifest.json").write_text(json.dumps({"name": "x"}))
            self.assertEqual(gate.check_root_manifests(root, "0.8.2"), [])


if __name__ == "__main__":
    unittest.main()
