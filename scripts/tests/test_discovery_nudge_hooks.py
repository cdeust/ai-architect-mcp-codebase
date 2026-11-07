"""The plugin's code-discovery nudge (issue #198): manifest, payloads, fail-open.

The nudge is paid on every session start and every subagent spawn, and a hook
that breaks session startup is worse than no nudge. These tests pin both: the
payload shape for each event, a byte ceiling on the injected text, and an exit
of 0 with no output on every failure path, under every POSIX shell present.
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HOOKS = ROOT / "hooks" / "hooks.json"
SCRIPT = ROOT / "hooks" / "discovery-nudge.sh"
TOOL_PROFILE = ROOT / "src" / "tool_profile.rs"

# Measured 2026-11-07: 711 bytes, 91 words. The ceiling leaves room for one
# more sentence; growing past it should be a deliberate, re-measured choice.
MAX_NUDGE_BYTES = 900

# hooks.json always runs `sh`, so it must be present; dash and bash are extra
# coverage where installed.
if not shutil.which("sh"):
    raise RuntimeError("these tests need `sh` on PATH, as hooks/hooks.json does")
SHELLS = [shell for shell in ("sh", "dash", "bash") if shutil.which(shell)]


def run(shell, *arguments, env=None, stdout=subprocess.PIPE):
    return subprocess.run(
        [shutil.which(shell), str(SCRIPT), *arguments],
        stdout=stdout,
        stderr=subprocess.PIPE,
        stdin=subprocess.DEVNULL,
        env=env,
        check=False,
    )


def core_tool_names():
    source = TOOL_PROFILE.read_text(encoding="utf-8")
    block = re.search(r"CORE_TOOL_NAMES: \[&str; \d+\] = \[(.*?)\];", source, re.S)
    if block is None:
        raise AssertionError("CORE_TOOL_NAMES declaration not found in src/tool_profile.rs")
    return set(re.findall(r'"([a-z_]+)"', block.group(1)))


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.hooks = json.loads(HOOKS.read_text(encoding="utf-8"))["hooks"]

    def commands(self, event):
        return [
            hook["command"]
            for group in self.hooks[event]
            for hook in group["hooks"]
            if hook["type"] == "command"
        ]

    def test_session_start_covers_every_session_source(self):
        (group,) = self.hooks["SessionStart"]
        self.assertEqual(
            set(group["matcher"].split("|")), {"startup", "resume", "clear", "compact"}
        )
        self.assertEqual(
            self.commands("SessionStart"),
            ['sh "${CLAUDE_PLUGIN_ROOT}/hooks/discovery-nudge.sh" session'],
        )

    def test_subagent_start_matches_every_subagent(self):
        (group,) = self.hooks["SubagentStart"]
        self.assertEqual(group["matcher"], "*")
        self.assertEqual(
            self.commands("SubagentStart"),
            ['sh "${CLAUDE_PLUGIN_ROOT}/hooks/discovery-nudge.sh" subagent'],
        )

    def test_only_the_two_nudge_events_are_registered(self):
        # No PreToolUse gate: the plugin never sits in front of Grep/Glob.
        self.assertEqual(set(self.hooks), {"SessionStart", "SubagentStart"})

    def test_every_hook_has_the_five_second_timeout(self):
        timeouts = {
            hook["timeout"]
            for event in self.hooks
            for group in self.hooks[event]
            for hook in group["hooks"]
        }
        self.assertEqual(timeouts, {5})

    def test_shipped_at_the_plugin_root(self):
        # Claude Code loads <plugin root>/hooks/hooks.json; the marketplace
        # source is the repository root, so this file is that one.
        marketplace = json.loads(
            (ROOT / ".claude-plugin" / "marketplace.json").read_text(encoding="utf-8")
        )
        self.assertEqual(marketplace["plugins"][0]["source"], "./")

    def test_running_the_hook_writes_nothing_under_home(self):
        # The nudge must never touch ~/.claude/settings.json or anything else:
        # run both modes with an empty HOME and require it to stay empty.
        for shell in SHELLS:
            for mode in ("session", "subagent"):
                with self.subTest(shell=shell, mode=mode), tempfile.TemporaryDirectory() as home:
                    result = run(shell, mode, env={**os.environ, "HOME": home})
                    self.assertEqual(result.returncode, 0)
                    self.assertEqual(list(Path(home).iterdir()), [])


class PayloadTests(unittest.TestCase):
    def session_text(self, shell):
        result = run(shell, "session")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, b"")
        return result.stdout.decode("utf-8")

    def test_session_start_is_plain_text(self):
        for shell in SHELLS:
            with self.subTest(shell=shell):
                text = self.session_text(shell)
                self.assertTrue(text.endswith("\n"))
                self.assertIn("ai-architect", text)
                with self.assertRaises(json.JSONDecodeError):
                    json.loads(text)

    def test_subagent_start_is_additional_context_json(self):
        for shell in SHELLS:
            with self.subTest(shell=shell):
                result = run(shell, "subagent")
                self.assertEqual(result.returncode, 0)
                self.assertEqual(result.stderr, b"")
                payload = json.loads(result.stdout)
                self.assertEqual(
                    payload,
                    {
                        "hookSpecificOutput": {
                            "hookEventName": "SubagentStart",
                            "additionalContext": self.session_text(shell).rstrip("\n"),
                        }
                    },
                )

    def test_text_points_at_the_missed_graph_instead_of_claiming_completeness(self):
        text = self.session_text("sh")
        self.assertIn('query_graph(graph="missed")', text)
        self.assertIn("absence from the graph is not proof of absence", text)

    def test_text_names_only_tools_both_profiles_register(self):
        contract = json.loads((ROOT / "mcp-contract.json").read_text(encoding="utf-8"))
        text = self.session_text("sh").replace(contract["claude_tool_prefix"], "")
        named = set(re.findall(r"\b[a-z]+(?:_[a-z]+)+\b", text))
        core = core_tool_names()
        self.assertTrue(named, "the nudge must name the tools it recommends")
        self.assertLessEqual(named, core)
        self.assertIn("analyze_codebase", named)
        self.assertIn("query_graph", named)

    def test_tool_prefix_matches_the_contract(self):
        contract = json.loads((ROOT / "mcp-contract.json").read_text(encoding="utf-8"))
        self.assertIn(contract["claude_tool_prefix"], self.session_text("sh"))

    def test_token_cost_stays_within_the_measured_budget(self):
        size = len(self.session_text("sh").rstrip("\n").encode("utf-8"))
        self.assertLessEqual(size, MAX_NUDGE_BYTES)


class FailOpenTests(unittest.TestCase):
    def assert_silent_success(self, result):
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout or b"", b"")
        self.assertEqual(result.stderr, b"")

    def test_missing_or_unknown_mode(self):
        for shell in SHELLS:
            for arguments in ((), ("",), ("SubagentStart",), ("extra",)):
                with self.subTest(shell=shell, arguments=arguments):
                    self.assert_silent_success(run(shell, *arguments))

    def test_both_modes_run_without_any_external_command(self):
        # An empty PATH leaves only shell builtins: the payloads are constants
        # written with printf, so no missing tool can break or truncate them.
        for shell in SHELLS:
            for mode in ("session", "subagent"):
                with self.subTest(shell=shell, mode=mode):
                    result = run(shell, mode, env={"PATH": ""})
                    self.assertEqual(result.returncode, 0)
                    self.assertEqual(result.stderr, b"")
                    self.assertEqual(result.stdout, run(shell, mode).stdout)

    def test_closed_stdout(self):
        for shell in SHELLS:
            for mode in ("session", "subagent"):
                with self.subTest(shell=shell, mode=mode):
                    path = shutil.which(shell)
                    result = subprocess.run(
                        [path, "-c", '"$0" "$1" "$2" >&-', path, str(SCRIPT), mode],
                        capture_output=True,
                        stdin=subprocess.DEVNULL,
                        check=False,
                    )
                    self.assert_silent_success(result)

    @unittest.skipUnless(os.path.exists("/dev/full"), "needs /dev/full (Linux)")
    def test_failed_write(self):
        # /dev/full accepts the open and fails every write with ENOSPC.
        for shell in SHELLS:
            for mode in ("session", "subagent"):
                with self.subTest(shell=shell, mode=mode), open("/dev/full", "wb") as full:
                    self.assert_silent_success(run(shell, mode, stdout=full))

    def test_reader_gone_away(self):
        # A pipe with no reader: without `trap '' PIPE` the shell dies of SIGPIPE.
        for shell in SHELLS:
            for mode in ("session", "subagent"):
                with self.subTest(shell=shell, mode=mode):
                    reader, writer = os.pipe()
                    os.close(reader)
                    try:
                        result = run(shell, mode, stdout=writer)
                    finally:
                        os.close(writer)
                    self.assert_silent_success(result)


if __name__ == "__main__":
    unittest.main()
