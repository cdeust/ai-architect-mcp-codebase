#!/usr/bin/env python3
"""Exercise the exact Claude plugin launcher and its full tool surface."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def response(output: str, request_id: int) -> dict:
    for line in output.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if value.get("id") == request_id:
            return value
    raise AssertionError(f"missing response {request_id}: {output}")


def main() -> None:
    server = json.loads((ROOT / ".mcp.json").read_text())["mcpServers"]["ai-architect"]
    command = server["command"].replace("${CLAUDE_PLUGIN_ROOT}", str(ROOT))
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude-plugin-smoke", "version": "ci"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "health_check", "arguments": {}}},
    ]
    run = subprocess.run([command, *server.get("args", [])], env={**os.environ, "CLAUDE_PLUGIN_ROOT": str(ROOT)}, input="\n".join(map(json.dumps, requests)) + "\n", text=True, capture_output=True, check=False)
    assert run.returncode == 0, run.stderr
    assert response(run.stdout, 1).get("error") is None
    assert len(response(run.stdout, 2)["result"]["tools"]) == 26
    assert response(run.stdout, 3)["result"].get("isError") is not True
    print("CLAUDE PLUGIN SMOKE OK: shipped launcher initialized, listed 26 tools, and called health_check")


if __name__ == "__main__":
    main()
