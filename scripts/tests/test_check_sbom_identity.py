"""The attested SBOM describes the released crate at the tag's version."""

import contextlib
import importlib.util
import io
import json
import re
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_sbom_identity", REPO / "scripts/check_sbom_identity.py"
)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)


def sbom(name="ai-architect-mcp-codebase", version="1.2.3"):
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.3",
        "metadata": {
            "component": {"type": "application", "name": name, "version": version}
        },
        "components": [{"name": "serde"}, {"name": "tokio"}],
    }


class SbomIdentity(unittest.TestCase):
    def run_check(self, document, tag):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sbom.cdx.json"
            path.write_text(
                document if isinstance(document, str) else json.dumps(document)
            )
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = check.main([str(path), "--tag", tag])
        return code, out.getvalue(), err.getvalue()

    def test_the_released_crate_at_the_tag_version_passes(self):
        code, out, err = self.run_check(sbom(), "v1.2.3")
        self.assertEqual((code, err), (0, ""))
        self.assertIn("describes ai-architect-mcp-codebase 1.2.3, 2 components", out)

    def test_a_release_candidate_tag_names_the_stable_version(self):
        self.assertEqual(self.run_check(sbom(), "v1.2.3-rc4")[0], 0)

    def test_the_sbom_of_another_workspace_crate_fails(self):
        """What v0.13.0 published: the SBOM of `zera 0.0.1`."""
        code, out, err = self.run_check(sbom("zera", "0.0.1"), "v1.2.3")
        self.assertEqual((code, out), (1, ""))
        self.assertIn("root component is 'zera'", err)
        self.assertIn("root component version is '0.0.1', not '1.2.3'", err)

    def test_the_right_crate_at_another_version_fails(self):
        code, _, err = self.run_check(sbom(version="1.2.2"), "v1.2.3")
        self.assertEqual(code, 1)
        self.assertIn("root component version is '1.2.2', not '1.2.3'", err)

    def test_a_document_that_names_no_root_crate_fails(self):
        without_component = sbom()
        del without_component["metadata"]["component"]
        not_cyclonedx = {**sbom(), "bomFormat": "SPDX"}
        for document in (without_component, {}, [], "not json", not_cyclonedx):
            with self.subTest(document=document):
                self.assertEqual(self.run_check(document, "v1.2.3")[0], 1)

    def test_a_tag_of_another_shape_fails(self):
        for tag in ("1.2.3", "v1.2", "v1.2.3-beta", "v1.2.3+build", "main", ""):
            with self.subTest(tag=tag):
                code, _, err = self.run_check(sbom(), tag)
                self.assertEqual(code, 1)
                self.assertIn("is not v<X.Y.Z>", err)

    def test_a_missing_file_fails(self):
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(
                check.main(["/nonexistent/sbom.json", "--tag", "v1.2.3"]), 1
            )


class ReleaseWorkflowUsesTheCheck(unittest.TestCase):
    """The rule only holds if release.yml copies the root file and runs the
    check before the SBOM is attested."""

    def test_the_sbom_step_copies_the_root_file_and_checks_it_before_attesting(self):
        workflow = (REPO / ".github/workflows/release.yml").read_text()
        step = re.search(
            r"- name: Generate SBOM\n(.*?)\n      - name: ", workflow, re.DOTALL
        ).group(1)
        code = "\n".join(
            line for line in step.splitlines() if not line.strip().startswith("#")
        )
        self.assertIn("cp sbom.json ai-architect-mcp-codebase.cdx.json", code)
        self.assertIn(
            'python3 scripts/check_sbom_identity.py ai-architect-mcp-codebase.cdx.json --tag "$TAG"',
            code,
        )
        self.assertIn("TAG: ${{ github.ref_name }}", code)
        self.assertNotIn("find ", code)
        self.assertLess(
            workflow.index("scripts/check_sbom_identity.py"),
            workflow.index("- name: Attest build provenance (SBOM)"),
        )


if __name__ == "__main__":
    unittest.main()
