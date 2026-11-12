"""smoke_binary.py starts one built binary and holds its answers to the tree (#449)."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

from smoke_claude_plugin import expected_tools  # noqa: E402  (path fix above)

VERSION = json.loads(
    (ROOT / ".claude-plugin/plugin.json").read_text(encoding="utf-8")
)["version"]

# A stand-in for the built binary: it reads the MCP requests on its standard
# input and answers from `behaviour.json`, the file beside it.
STAND_IN = textwrap.dedent(
    """\
    #!/usr/bin/env python3
    import json, sys
    from pathlib import Path
    b = json.loads((Path(__file__).resolve().parent / "behaviour.json").read_text())
    if b.get("exit"):
        sys.exit(b["exit"])
    for line in sys.stdin:
        request = json.loads(line)
        ident = request.get("id")
        if request["method"] == "initialize":
            result = {"serverInfo": {"name": b["name"], "version": b["version"]}}
        elif request["method"] == "tools/list":
            result = {"tools": [{"name": name} for name in b["tools"]]}
        elif request["method"] == "tools/call":
            health = {"status": b["status"], "tools_count": len(b["tools"])}
            result = {"content": [{"type": "text", "text": json.dumps(health)}]}
        else:
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": ident, "result": result}))
    """
)


@unittest.skipUnless(os.name == "posix", "the stand-in is started through its shebang")
class SmokeBinaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.binary = Path(self.tmp.name) / "ai-architect-mcp-codebase"
        self.binary.write_text(STAND_IN, encoding="utf-8")
        self.binary.chmod(0o755)
        self.behaviour = {
            "name": "ai-architect",
            "version": VERSION,
            "tools": sorted(expected_tools(ROOT, VERSION, installed=False)),
            "status": "ok",
        }

    def smoke(self, *arguments, **changed):
        (Path(self.tmp.name) / "behaviour.json").write_text(
            json.dumps({**self.behaviour, **changed}), encoding="utf-8"
        )
        return subprocess.run(
            [sys.executable, str(SCRIPTS / "smoke_binary.py"), *arguments],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_a_binary_that_answers_as_the_tree_says_passes(self):
        run = self.smoke(str(self.binary))
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn("BINARY SMOKE OK: ai-architect-mcp-codebase", run.stdout)
        self.assertIn(f"v{VERSION}", run.stdout)

    def test_a_binary_of_another_version_is_refused(self):
        run = self.smoke(str(self.binary), version="0.0.1")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn(f"server version is 0.0.1, the tree says {VERSION}", run.stderr)

    def test_a_binary_with_another_name_is_refused(self):
        run = self.smoke(str(self.binary), name="something-else")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("server name is something-else", run.stderr)

    def test_a_missing_tool_is_refused(self):
        run = self.smoke(str(self.binary), tools=self.behaviour["tools"][1:])
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("tool surface mismatch", run.stderr)
        self.assertIn(self.behaviour["tools"][0], run.stderr)

    def test_an_unhealthy_binary_is_refused(self):
        run = self.smoke(str(self.binary), status="degraded")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("health_check is not healthy", run.stderr)

    def test_a_binary_that_exits_with_an_error_is_refused(self):
        run = self.smoke(str(self.binary), exit=3)
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("exited 3", run.stderr)

    def test_a_path_with_no_binary_is_refused(self):
        run = self.smoke(str(Path(self.tmp.name) / "absent"))
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("no binary at", run.stderr)

    def test_the_path_is_required(self):
        run = self.smoke()
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("usage: smoke_binary.py", run.stderr)


if __name__ == "__main__":
    unittest.main()
