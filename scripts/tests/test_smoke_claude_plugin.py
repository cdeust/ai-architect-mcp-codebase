"""Installation checks bind the exact binary release to its actual tool catalog."""

import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "smoke", Path(__file__).resolve().parents[1] / "smoke_claude_plugin.py"
)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


class SmokeTests(unittest.TestCase):
    def test_source_catalog_is_local_and_never_downloads(self):
        with patch.object(smoke, "urlopen", side_effect=AssertionError("network")):
            names = smoke.expected_tools(smoke.ROOT, "0.14.0", installed=False)
        self.assertIn("check_doc_claims", names)
        self.assertEqual(len(names), len(set(names)))

    def test_installed_catalog_uses_exact_release_not_head(self):
        source = (
            b'const FULL_TOOL_NAMES: [&str; 2] = ["health_check", "released_tool"];'
        )
        with patch.object(smoke, "urlopen", return_value=io.BytesIO(source)) as fetch:
            names = smoke.expected_tools(smoke.ROOT, "0.14.0", installed=True)
        self.assertEqual(names, {"health_check", "released_tool"})
        self.assertEqual(
            fetch.call_args.args,
            (
                "https://raw.githubusercontent.com/cdeust/ai-architect-mcp-codebase/v0.14.0/src/tool_profile.rs",
            ),
        )

    def test_release_catalog_network_failure_is_not_a_fallback(self):
        with patch.object(smoke, "urlopen", side_effect=OSError("offline")):
            with self.assertRaisesRegex(SystemExit, "release catalog.*offline"):
                smoke.expected_tools(smoke.ROOT, "0.14.0", installed=True)

    def test_bad_versions_never_reach_network(self):
        for version in (
            "../main",
            "0.14",
            "v0.14.0",
            "0.14.0/evil",
            "0.14.0-01",
            "0.14.0-rc..1",
            "0.14.0+",
            "01.14.0",
        ):
            with (
                self.subTest(version=version),
                patch.object(smoke, "urlopen") as fetch,
                self.assertRaisesRegex(SystemExit, "version"),
            ):
                smoke.expected_tools(smoke.ROOT, version, installed=True)
            fetch.assert_not_called()

    def test_catalog_requires_the_declared_unique_names(self):
        for source in (
            "not a catalog",
            'const FULL_TOOL_NAMES: [&str; 2] = ["a"];',
            'const FULL_TOOL_NAMES: [&str; 2] = ["a", "a"];',
        ):
            with (
                self.subTest(source=source),
                patch.object(
                    smoke, "urlopen", return_value=io.BytesIO(source.encode())
                ),
                self.assertRaises(SystemExit),
            ):
                smoke.expected_tools(smoke.ROOT, "0.14.0", installed=True)

    def test_same_count_wrong_tool_and_duplicates_fail(self):
        for names in (["health_check", "wrong"], ["health_check", "health_check"]):
            with (
                self.subTest(names=names),
                self.assertRaisesRegex(SystemExit, "tool surface"),
            ):
                smoke.check_tools(
                    [{"name": n} for n in names], {"health_check", "expected"}
                )
        smoke.check_tools(
            [{"name": "expected"}, {"name": "health_check"}],
            {"health_check", "expected"},
        )

    def test_windows_launches_shell_script_through_bash_without_shell_true(self):
        server = {
            "command": "bash",
            "args": ["${CLAUDE_PLUGIN_ROOT}/bin/launch-plugin.sh", "space argument"],
        }
        with patch.object(
            smoke.shutil, "which", return_value="C:/Program Files/Git/bin/bash.exe"
        ):
            command = smoke.launcher_command(
                server, Path("/plugin with spaces"), windows=True
            )
        self.assertEqual(
            command,
            [
                "C:/Program Files/Git/bin/bash.exe",
                "/plugin with spaces/bin/launch-plugin.sh",
                "space argument",
            ],
        )

    def test_missing_bash_fails_and_posix_preserves_manifest_command(self):
        server = {
            "command": "bash",
            "args": ["${CLAUDE_PLUGIN_ROOT}/bin/launch-plugin.sh"],
        }
        with (
            patch.object(smoke.shutil, "which", return_value=None),
            self.assertRaisesRegex(SystemExit, "Bash"),
        ):
            smoke.launcher_command(server, Path("/plugin"), windows=True)
        self.assertEqual(
            smoke.launcher_command(server, Path("/plugin"), windows=False),
            ["bash", "/plugin/bin/launch-plugin.sh"],
        )

    def test_health_requires_status_and_catalog_count(self):
        good = {
            "result": {
                "content": [{"text": json.dumps({"status": "ok", "tools_count": 2})}]
            }
        }
        smoke.check_health(good, 2)
        for result in (
            {"error": {"message": "failed"}},
            {"result": {"isError": True}},
            {"result": {"content": [{"text": '{"status":"bad","tools_count":2}'}]}},
            good,
        ):
            with self.subTest(result=result), self.assertRaises(SystemExit):
                smoke.check_health(result, 3)


if __name__ == "__main__":
    unittest.main()
