"""The `install` check states what it proves on a version with no release yet.

`smoke_claude_plugin.py --installed` asks GitHub whether the release of the
plugin's version is at or below the repository's latest release, read from
the redirect of github.com's `releases/latest` page (no REST call, no token).
At or below: the strict path, unchanged. Above: the launcher must refuse with
its release-asset error and nothing else. No network here: both openers are
fixtures, and the launcher either is a recorded result or runs for real
against a `curl` that answers 404.
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
from email.message import Message
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


LATEST = "https://github.com/cdeust/ai-architect-mcp-codebase/releases/latest"
TAG_PAGE = "https://github.com/cdeust/ai-architect-mcp-codebase/releases/tag/"
RAW = (
    "https://raw.githubusercontent.com/cdeust/ai-architect-mcp-codebase/"
    f"v{VERSION}/src/tool_profile.rs"
)
MAJOR, MINOR, PATCH = map(int, VERSION.split("."))
ABOVE = f"v{MAJOR}.{MINOR}.{PATCH + 1}"
BELOW = "v0.0.1"

# What github.com answered on 2026-11-11, redirects not followed, through this
# session's egress proxy (which may authenticate requests to this repository;
# an anonymous client was not available to compare):
#   .../releases/latest             302, Location .../releases/tag/v0.14.0
#   .../releases/tag/v0.14.0        200  (published release)
#   .../releases/tag/v0.9.0-rc1     200  (published prerelease; not the latest)
#   .../releases/tag/v0.15.0        404  (no such tag)
#   .../releases/tag/v99.98.97      404  (no such tag)
#   .../releases/download/v0.15.0/<asset>   404
# Not observed: a tag with no release (every tag of this repository has one,
# and the proxy refuses other repositories), a draft seen anonymously, and a
# repository with no release. The decision therefore rests on the one answer
# that was observed and that the documentation defines, `releases/latest`: a
# draft, a prerelease and a bare tag are never the latest release.


def redirect(location, code=302):
    headers = Message()
    if location is not None:
        headers["Location"] = location
    return HTTPError(LATEST, code, "Found", headers, None)


def latest_is(tag):
    return redirect(TAG_PAGE + tag)


class Page:
    """A reply that was not a redirect."""

    def __init__(self, status=200):
        self.status = status

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class Network:
    """Fixture for both openers. `latest` is what the no-redirect request to
    LATEST raises or returns; `catalog` is the tag's tool_profile.rs, or None
    for a 404. Every requested URL is recorded, in order."""

    def __init__(self, latest, catalog=None):
        self.latest, self.catalog, self.urls = latest, catalog, []

    def open(self, request, timeout=None):
        self.urls.append(request.full_url)
        assert timeout, "the state request must be bounded"
        assert not request.has_header("Authorization"), "no token is used"
        if isinstance(self.latest, BaseException):
            raise self.latest
        return self.latest

    def urlopen(self, url, *args, **kwargs):
        self.urls.append(url)
        if self.catalog is None:
            raise HTTPError(url, 404, "Not Found", Message(), None)
        return io.BytesIO(self.catalog)


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

    def run_main(self, network, launcher):
        """stdout of `main --installed`; SystemExit passes through."""
        out = io.StringIO()
        self.network = network
        with (
            patch.object(smoke, "ROOT", self.root),
            patch.object(smoke, "NO_REDIRECT", network),
            patch.object(smoke, "urlopen", network.urlopen),
            patch.object(smoke.subprocess, "run", return_value=launcher) as self.ran,
            patch.object(sys, "argv", ["smoke", "--installed"]),
            patch.dict(os.environ, {"GH_TOKEN": "t", "GITHUB_TOKEN": "t"}),
            contextlib.redirect_stdout(out),
        ):
            smoke.main()
        return out.getvalue()

    def test_the_latest_release_takes_the_strict_path(self):
        good = healthy_answers(VERSION, ["health_check", "released_tool"])
        network = Network(latest_is(f"v{VERSION}"), CATALOG)
        out = self.run_main(network, launcher_result(0, good))
        self.assertIn(f"CLAUDE PLUGIN SMOKE OK: shipped launcher, v{VERSION}", out)
        self.assertNotIn("INSTALL:", out)
        self.assertEqual(network.urls, [LATEST, RAW])

    def test_a_version_below_the_latest_release_takes_the_strict_path(self):
        """A branch cut before the last release still installs its own
        published release, as before."""
        good = healthy_answers(VERSION, ["health_check", "released_tool"])
        out = self.run_main(
            Network(latest_is(ABOVE), CATALOG), launcher_result(0, good)
        )
        self.assertIn("CLAUDE PLUGIN SMOKE OK", out)

    def test_published_release_never_takes_the_lenient_branch(self):
        """The refusal that passes on an unreleased version fails on the
        latest release and on an older one: a missing asset of a published
        release is a defect."""
        for latest in (f"v{VERSION}", ABOVE):
            with self.subTest(latest=latest), self.assertRaises(SystemExit) as stopped:
                self.run_main(
                    Network(latest_is(latest), CATALOG), launcher_result(1, "", REFUSAL)
                )
            self.assertIn("release asset unavailable", str(stopped.exception))

    def test_a_lower_or_equal_version_with_no_release_fails(self):
        """A version at or below the latest release that was never published
        (skipped, or lowered) has no catalog at its tag: failure, before the
        launcher runs."""
        for latest in (f"v{VERSION}", ABOVE, "v99.0.0"):
            with (
                self.subTest(latest=latest),
                self.assertRaisesRegex(SystemExit, "release catalog unavailable"),
            ):
                self.run_main(
                    Network(latest_is(latest)), launcher_result(1, "", REFUSAL)
                )
            self.ran.assert_not_called()

    def test_published_release_with_a_wrong_catalog_fails(self):
        wrong = healthy_answers(VERSION, ["health_check", "other_tool"])
        with self.assertRaisesRegex(SystemExit, "tool surface mismatch"):
            self.run_main(
                Network(latest_is(f"v{VERSION}"), CATALOG), launcher_result(0, wrong)
            )

    def test_unreleased_version_verifies_the_refusal_and_says_so(self):
        network = Network(latest_is(BELOW), CATALOG)
        out = self.run_main(network, launcher_result(1, "", REFUSAL))
        self.assertEqual(
            out,
            f"INSTALL: v{VERSION} is not released yet; verified that the plugin "
            "installs and that its launcher refuses with the release-asset error "
            "(exit 1, 404 on the asset, no binary installed, no fallback); the "
            "tool catalog and health_check are covered by the e2e job on this "
            "commit, the download path by release.yml's publish_verified_release "
            "and by the next run after the release\n",
        )
        self.assertEqual(network.urls, [LATEST])
        environment = self.ran.call_args.kwargs["env"]
        self.assertEqual(environment["AI_ARCHITECT_SOURCE_CHECKOUT"], "0")
        self.assertEqual(environment["CLAUDE_PLUGIN_ROOT"], self.root.as_posix())

    def test_the_404_is_read_from_curl_message_whatever_its_exit_code(self):
        # The macos runner's curl reports the same 404 under exit code 56.
        macos = REFUSAL.replace(CURL_404, CURL_404.replace("(22)", "(56)"))
        network = Network(latest_is(BELOW), CATALOG)
        out = self.run_main(network, launcher_result(1, "", macos))
        self.assertIn(f"INSTALL: v{VERSION} is not released yet", out)

    def test_draft_release_is_not_published(self):
        """A draft (and a prerelease, and a bare tag) of this version is never
        the latest release: `releases/latest` still names the previous one,
        whatever exists at the tag (here, its catalog)."""
        out = self.run_main(
            Network(latest_is(BELOW), CATALOG), launcher_result(1, "", REFUSAL)
        )
        self.assertTrue(out.startswith(f"INSTALL: v{VERSION} is not released yet;"))

    def test_unknown_release_state_fails_before_the_launcher_runs(self):
        other = "https://github.com/someone/else/releases/tag/v0.0.1"
        for name, answer in {
            "403": redirect(None, 403),
            "404 (a repository with no release, or a hidden one)": redirect(None, 404),
            "429": redirect(None, 429),
            "500": redirect(None, 500),
            "no route": URLError("no route to host"),
            "timeout": TimeoutError("timed out"),
            "a page where a redirect was expected": Page(200),
            "redirect with no Location": redirect(None),
            "redirect to the release list": redirect(TAG_PAGE[: -len("tag/")]),
            "redirect to another repository": redirect(other),
            "301 to the right place": redirect(TAG_PAGE + BELOW, 301),
            "prerelease tag": redirect(TAG_PAGE + "v0.0.1-rc1"),
            "tag that is not a version": redirect(TAG_PAGE + "nightly"),
            "tag with a suffix path": redirect(TAG_PAGE + "v0.0.1/evil"),
            "relative Location": redirect(
                "/cdeust/ai-architect-mcp-codebase/releases/tag/v0.0.1"
            ),
        }.items():
            with (
                self.subTest(name),
                self.assertRaisesRegex(SystemExit, "release state un"),
            ):
                self.run_main(Network(answer, CATALOG), launcher_result(1, "", REFUSAL))
            self.ran.assert_not_called()

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
            "a 404 only inside another message": launcher_result(
                1,
                "",
                REFUSAL.replace(CURL_404, "curl: (22) 404 seen in a redirect body"),
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
                self.run_main(Network(latest_is(BELOW), CATALOG), launcher)

    def test_a_binary_left_behind_fails_on_an_unreleased_version(self):
        (self.root / "target/release").mkdir(parents=True)
        (self.root / "target/release/ai-architect-mcp-codebase").write_bytes(b"x")
        with self.assertRaisesRegex(SystemExit, "left a binary"):
            self.run_main(Network(latest_is(BELOW)), launcher_result(1, "", REFUSAL))

    def test_the_state_request_uses_no_rest_call_and_no_token(self):
        """One github.com page, even with a token in the environment (the
        fixture refuses an Authorization header); no api.github.com anywhere."""
        network = Network(latest_is(BELOW))
        self.run_main(network, launcher_result(1, "", REFUSAL))
        self.assertEqual(network.urls, [LATEST])
        source = (REPO / "scripts/smoke_claude_plugin.py").read_text()
        for needle in ("api.github.com", "GH_TOKEN", "GITHUB_TOKEN", "Authorization"):
            self.assertNotIn(needle, source)

    def test_the_real_opener_does_not_follow_a_redirect(self):
        """The handler github.com's 302 goes through: it hands the redirect
        back, so the Location is read, never fetched."""
        handler = smoke._NoRedirect()
        self.assertIsNone(
            handler.redirect_request(None, None, 302, "Found", {}, TAG_PAGE + BELOW)
        )
        self.assertIn(smoke._NoRedirect, [type(h) for h in smoke.NO_REDIRECT.handlers])


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
