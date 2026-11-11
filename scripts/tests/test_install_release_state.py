"""The `install` check states what it proves on a version with no release yet.

`smoke_claude_plugin.py --installed` asks GitHub whether the release of the
plugin's version is published. Published: the strict path, unchanged. Not
published: the launcher must refuse with its release-asset error and nothing
else. No network here: `urlopen` is a fixture, and the launcher either is a
recorded result or runs for real against a `curl` that answers 404.
"""

import contextlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError, URLError

SPEC = importlib.util.spec_from_file_location(
    "smoke_release_state",
    Path(__file__).resolve().parents[1] / "smoke_claude_plugin.py",
)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)

REPO = Path(__file__).resolve().parents[2]
VERSION = json.loads((REPO / ".claude-plugin/plugin.json").read_text())["version"]
API = "https://api.github.com/repos/cdeust/ai-architect-mcp-codebase/releases"
ASSET = (
    "https://github.com/cdeust/ai-architect-mcp-codebase/releases/download/"
    f"v{VERSION}/ai-architect-mcp-codebase-linux-x86_64.tar.gz"
)
CURL_404 = "curl: (22) The requested URL returned error: 404"
REFUSAL = (
    f"ai-architect-mcp-codebase: installing v{VERSION} for linux-x86_64\n"
    f"{CURL_404}\n"
    f"ai-architect-mcp-codebase: FATAL: release asset unavailable: {ASSET}\n"
)
CATALOG = b'const FULL_TOOL_NAMES: [&str; 2] = ["health_check", "released_tool"];'


def url_of(request):
    return request if isinstance(request, str) else request.full_url


def not_found(url):
    return HTTPError(url, 404, "Not Found", None, None)


def release(tag, *, draft=False):
    return json.dumps({"tag_name": tag, "draft": draft, "prerelease": False}).encode()


class Network:
    """`urlopen` fixture: URL -> bytes to answer, or an exception to raise.
    An URL it was not given is a 404; every request is recorded."""

    def __init__(self, answers):
        self.answers = answers
        self.requests = []

    def __call__(self, request, *args, **kwargs):
        self.requests.append(request)
        answer = self.answers.get(url_of(request), not_found(url_of(request)))
        if isinstance(answer, BaseException):
            raise answer
        return io.BytesIO(answer)

    def urls(self):
        return [url_of(request) for request in self.requests]


def launcher_result(returncode, stdout="", stderr=""):
    return subprocess.CompletedProcess(["bash"], returncode, stdout, stderr)


def healthy_answers(version, names):
    health = {"status": "ok", "tools_count": len(names)}
    lines = [
        {
            "id": 1,
            "result": {"serverInfo": {"name": "ai-architect", "version": version}},
        },
        {"id": 2, "result": {"tools": [{"name": name} for name in names]}},
        {"id": 3, "result": {"content": [{"text": json.dumps(health)}]}},
    ]
    return "\n".join(map(json.dumps, lines)) + "\n"


class InstalledRun(unittest.TestCase):
    """`main --installed` over a copy of the manifests, a fixture network and a
    recorded launcher result."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / ".claude-plugin").mkdir()
        for name in (".claude-plugin/plugin.json", ".mcp.json"):
            shutil.copy2(REPO / name, self.root / name)

    def run_main(self, answers, launcher):
        """(stdout, network, launcher mock); SystemExit passes through."""
        network = Network(answers)
        out = io.StringIO()
        with (
            patch.object(smoke, "ROOT", self.root),
            patch.object(smoke, "urlopen", network),
            patch.object(smoke.subprocess, "run", return_value=launcher) as ran,
            patch.object(sys, "argv", ["smoke", "--installed"]),
            contextlib.redirect_stdout(out),
        ):
            try:
                smoke.main()
            finally:
                self.network, self.ran = network, ran
        return out.getvalue()

    def published(self, **more):
        raw = f"https://raw.githubusercontent.com/cdeust/ai-architect-mcp-codebase/v{VERSION}/src/tool_profile.rs"
        return {f"{API}/tags/v{VERSION}": release(f"v{VERSION}"), raw: CATALOG, **more}

    def test_published_release_takes_the_strict_path(self):
        good = healthy_answers(VERSION, ["health_check", "released_tool"])
        out = self.run_main(self.published(), launcher_result(0, good))
        self.assertIn(f"CLAUDE PLUGIN SMOKE OK: shipped launcher, v{VERSION}", out)
        self.assertNotIn("INSTALL:", out)
        self.assertEqual(
            self.network.urls(),
            [
                f"{API}/tags/v{VERSION}",
                f"https://raw.githubusercontent.com/cdeust/ai-architect-mcp-codebase/v{VERSION}/src/tool_profile.rs",
            ],
        )

    def test_published_release_never_takes_the_lenient_branch(self):
        """The refusal that passes on an unreleased version fails on a
        published one: a missing asset of a published release is a defect."""
        with self.assertRaises(SystemExit) as stopped:
            self.run_main(self.published(), launcher_result(1, "", REFUSAL))
        self.assertIn("release asset unavailable", str(stopped.exception))

    def test_published_release_with_a_missing_catalog_fails(self):
        answers = {f"{API}/tags/v{VERSION}": release(f"v{VERSION}")}
        with self.assertRaisesRegex(SystemExit, "release catalog unavailable"):
            self.run_main(answers, launcher_result(1, "", REFUSAL))
        self.ran.assert_not_called()

    def test_published_release_with_a_wrong_catalog_fails(self):
        wrong = healthy_answers(VERSION, ["health_check", "other_tool"])
        with self.assertRaisesRegex(SystemExit, "tool surface mismatch"):
            self.run_main(self.published(), launcher_result(0, wrong))

    def test_unreleased_version_verifies_the_refusal_and_says_so(self):
        answers = {f"{API}/latest": release("v0.0.1")}
        out = self.run_main(answers, launcher_result(1, "", REFUSAL))
        self.assertEqual(
            out,
            f"INSTALL: v{VERSION} is not released yet; verified that the plugin "
            "installs and that its launcher refuses with the release-asset error "
            "(exit 1, 404 on the asset, no binary installed, no fallback); the "
            "tool catalog and health_check are covered by the e2e job on this "
            "commit, the download path by release.yml's publish_verified_release "
            "and by the next run after the release\n",
        )
        self.assertEqual(
            self.network.urls(), [f"{API}/tags/v{VERSION}", f"{API}/latest"]
        )
        environment = self.ran.call_args.kwargs["env"]
        self.assertEqual(environment["AI_ARCHITECT_SOURCE_CHECKOUT"], "0")
        self.assertEqual(environment["CLAUDE_PLUGIN_ROOT"], self.root.as_posix())

    def test_draft_release_is_not_published(self):
        answers = {
            f"{API}/tags/v{VERSION}": release(f"v{VERSION}", draft=True),
            f"{API}/latest": release("v0.0.1"),
        }
        out = self.run_main(answers, launcher_result(1, "", REFUSAL))
        self.assertTrue(out.startswith(f"INSTALL: v{VERSION} is not released yet;"))

    def test_unknown_release_state_fails_before_the_launcher_runs(self):
        url = f"{API}/tags/v{VERSION}"
        for answer in (
            HTTPError(url, 403, "rate limit exceeded", None, None),
            HTTPError(url, 500, "Server Error", None, None),
            URLError("no route to host"),
            TimeoutError("timed out"),
            b"<html>not json</html>",
            b"[]",
            release("v9.9.9"),
            json.dumps({"tag_name": f"v{VERSION}"}).encode(),
        ):
            with (
                self.subTest(answer=answer),
                self.assertRaisesRegex(SystemExit, "release state un"),
            ):
                self.run_main({url: answer}, launcher_result(1, "", REFUSAL))
            self.ran.assert_not_called()

    def test_unreleased_version_must_be_above_the_latest_release(self):
        for latest in ("v99.0.0", f"v{VERSION}"):
            with (
                self.subTest(latest=latest),
                self.assertRaisesRegex(SystemExit, "not above the latest release"),
            ):
                self.run_main(
                    {f"{API}/latest": release(latest)},
                    launcher_result(1, "", REFUSAL),
                )
            self.ran.assert_not_called()
        for unreadable in (release("nightly"), b"{}", URLError("offline")):
            with self.subTest(latest=unreadable), self.assertRaises(SystemExit):
                self.run_main(
                    {f"{API}/latest": unreadable}, launcher_result(1, "", REFUSAL)
                )
        # A repository with no published release has nothing to be behind.
        self.run_main({}, launcher_result(1, "", REFUSAL))

    def test_anything_but_the_clean_refusal_fails_on_an_unreleased_version(self):
        good = healthy_answers(VERSION, ["health_check"])
        other = ASSET.replace(f"v{VERSION}", "v0.0.1")
        cases = {
            "launcher started a server": launcher_result(0, good, ""),
            "exit 0 with the refusal text": launcher_result(0, "", REFUSAL),
            "another exit code": launcher_result(2, "", REFUSAL),
            "answered on stdout": launcher_result(1, good, REFUSAL),
            "fell back to a source build": launcher_result(
                1,
                "",
                REFUSAL
                + "ai-architect-mcp-codebase: building from source (reason: missing; first build can take several minutes)\n",
            ),
            "connection refused, not a 404": launcher_result(
                1,
                "",
                REFUSAL.replace(CURL_404, "curl: (7) Failed to connect to github.com"),
            ),
            "server error, not a 404": launcher_result(
                1, "", REFUSAL.replace("error: 404", "error: 503")
            ),
            "asset of another version": launcher_result(
                1, "", REFUSAL.replace(ASSET, other)
            ),
            "another fatal error": launcher_result(
                1,
                "",
                "ai-architect-mcp-codebase: FATAL: curl is required for marketplace installation\n",
            ),
            "tampered manifest": launcher_result(
                1,
                "",
                "ai-architect-mcp-codebase: FATAL: plugin manifest does not match the reviewed release identity\n",
            ),
            "two fatal errors": launcher_result(
                1,
                "",
                REFUSAL
                + "ai-architect-mcp-codebase: FATAL: release checksum unavailable\n",
            ),
            "no output": launcher_result(1, "", ""),
        }
        for name, launcher in cases.items():
            with self.subTest(name), self.assertRaises(SystemExit):
                self.run_main({f"{API}/latest": release("v0.0.1")}, launcher)

    def test_a_binary_left_behind_fails_on_an_unreleased_version(self):
        (self.root / "target/release").mkdir(parents=True)
        (self.root / "target/release/ai-architect-mcp-codebase").write_bytes(b"x")
        with self.assertRaisesRegex(SystemExit, "left a binary"):
            self.run_main({}, launcher_result(1, "", REFUSAL))

    def test_a_token_in_the_environment_authenticates_the_state_request(self):
        for environment, expected in (
            ({"GH_TOKEN": "t1"}, "Bearer t1"),
            ({"GITHUB_TOKEN": "t2"}, "Bearer t2"),
            ({}, None),
        ):
            clean = {
                key: value
                for key, value in os.environ.items()
                if key not in ("GH_TOKEN", "GITHUB_TOKEN")
            }
            with (
                self.subTest(environment=environment),
                patch.dict(os.environ, {**clean, **environment}, clear=True),
            ):
                self.run_main({}, launcher_result(1, "", REFUSAL))
            self.assertEqual(
                self.network.requests[0].get_header("Authorization"), expected
            )


@unittest.skipIf(os.name == "nt", "the fixture tools are POSIX scripts")
class RealLauncherRefusal(unittest.TestCase):
    """The committed launcher, run as the plugin runs it, against a `curl` that
    answers what github.com answers for an asset that does not exist. Binds the
    asserted line and exit code to bin/ensure-binary.sh itself."""

    def launch(self, curl_exit, curl_message):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        base = Path(directory.name)
        plugin, tools = base / "plugin", base / "tools"
        (plugin / "bin").mkdir(parents=True)
        (plugin / ".claude-plugin").mkdir()
        tools.mkdir()
        for name in (
            "Cargo.toml",
            "bin/ensure-binary.sh",
            "bin/launch-plugin.sh",
            ".claude-plugin/plugin.json",
            ".mcp.json",
        ):
            shutil.copy2(REPO / name, plugin / name)
        (tools / "uname").write_text(
            '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n'
        )
        (tools / "curl").write_text(
            f"#!/bin/sh\necho '{curl_message}' >&2\nexit {curl_exit}\n"
        )
        (tools / "gh").write_text("#!/bin/sh\necho --source-ref\n")
        (tools / "cargo").write_text("#!/bin/sh\necho cargo-was-run >&2\nexit 0\n")
        for tool in tools.iterdir():
            tool.chmod(0o755)
        server = json.loads((plugin / ".mcp.json").read_text())["mcpServers"][
            "ai-architect"
        ]
        run = subprocess.run(
            smoke.launcher_command(server, plugin, windows=False),
            env={
                **os.environ,
                "CLAUDE_PLUGIN_ROOT": plugin.as_posix(),
                "AI_ARCHITECT_SOURCE_CHECKOUT": "0",
                "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}",
            },
            cwd=plugin,
            input="{}\n",
            text=True,
            capture_output=True,
            check=False,
        )
        return plugin, run

    def test_missing_asset_is_the_documented_refusal_and_nothing_else(self):
        plugin, run = self.launch(22, CURL_404)
        self.assertEqual(run.returncode, 1, run.stderr)
        self.assertEqual(run.stderr, REFUSAL)
        self.assertNotIn("cargo-was-run", run.stderr)
        self.assertFalse((plugin / "target").exists())
        with contextlib.redirect_stdout(io.StringIO()) as out:
            smoke.check_unreleased(plugin, VERSION, run)
        self.assertIn(f"INSTALL: v{VERSION} is not released yet;", out.getvalue())

    def test_unreachable_host_is_not_read_as_a_missing_release(self):
        plugin, run = self.launch(7, "curl: (7) Failed to connect to github.com")
        self.assertEqual(run.returncode, 1, run.stderr)
        with self.assertRaisesRegex(SystemExit, "not with a 404"):
            smoke.check_unreleased(plugin, VERSION, run)


if __name__ == "__main__":
    unittest.main()
