#!/usr/bin/env python3
"""Start one built binary and require it to answer as the plugin's server.

usage: smoke_binary.py <path to the built binary>

The binary is run the way `bin/launch-plugin.sh` runs it, with no argument and
the MCP requests on its standard input: `initialize`, `tools/list`,
`health_check`. The name and version it announces, its tool catalog and its
health are held to this tree (`.claude-plugin/plugin.json`,
`src/tool_profile.rs`), with the checks of `smoke_claude_plugin.py`.

Issue #449: `release.yml` and `windows-build.yml` built the Windows and Linux
aarch64 binaries and never started them. Each built artifact is now started
here, on a runner of its platform, before it is packaged.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from smoke_claude_plugin import (  # noqa: E402  (import follows the path fix above)
    ROOT,
    check_health,
    check_tools,
    expected_tools,
    require,
    response,
    smoke_requests,
)

# source: six times the 19.9 s first launch the audit of 2026-10-10 measured
# (issue #449), so a binary that never answers fails the step instead of
# holding the runner until the job's own limit.
START_TIMEOUT_S = 120


def answers(binary: Path) -> str:
    """Post: the standard output of `binary`, which exited 0 within the limit."""
    environment = {**os.environ}
    environment.pop("AP_PROFILE", None)
    try:
        run = subprocess.run(
            [str(binary)],
            env=environment,
            input="\n".join(map(json.dumps, smoke_requests())) + "\n",
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
            timeout=START_TIMEOUT_S,
        )
    except OSError as exc:
        raise SystemExit(f"{binary} could not be started: {exc}") from exc
    except subprocess.TimeoutExpired as exc:
        raise SystemExit(
            f"{binary} did not answer within {START_TIMEOUT_S} s"
        ) from exc
    require(run.returncode == 0, f"{binary} exited {run.returncode}: {run.stderr}")
    return run.stdout


def answer(output: str, request_id: int, *keys: str) -> Any:
    """Post: the value under `keys` in the answer to request `request_id`.

    An answer that is missing, or that lacks one of the keys, ends the run with
    what the binary printed, not with a traceback.
    """
    try:
        value: Any = response(output, request_id)
    except AssertionError as exc:
        raise SystemExit(str(exc)) from exc
    for key in keys:
        require(
            isinstance(value, dict) and key in value,
            f"answer {request_id} has no {'.'.join(keys)}: {output}",
        )
        value = value[key]
    return value


def main() -> None:
    require(len(sys.argv) == 2, "usage: smoke_binary.py <path to the built binary>")
    binary = Path(sys.argv[1])
    require(binary.is_file(), f"no binary at {binary}")
    version = json.loads(
        (ROOT / ".claude-plugin/plugin.json").read_text(encoding="utf-8")
    )["version"]
    expected = expected_tools(ROOT, version, installed=False)
    output = answers(binary)
    require(answer(output, 1).get("error") is None, "initialize failed")
    served = answer(output, 1, "result", "serverInfo")
    for field in ("name", "version"):
        require(field in served, f"serverInfo has no {field}: {output}")
    require(served["name"] == "ai-architect", f"server name is {served['name']}")
    require(
        served["version"] == version,
        f"server version is {served['version']}, the tree says {version}",
    )
    check_tools(answer(output, 2, "result", "tools"), expected)
    check_health(answer(output, 3), len(expected))
    print(
        f"BINARY SMOKE OK: {binary.name}, v{version}, exact {len(expected)}-tool "
        "catalog, health_check ok"
    )


if __name__ == "__main__":
    main()
