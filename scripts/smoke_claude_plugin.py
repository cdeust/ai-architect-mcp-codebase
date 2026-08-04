#!/usr/bin/env python3
"""Exercise the exact Claude plugin launcher and its full tool surface."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


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
    run = subprocess.run(
        [command, *server.get("args", [])],
        env={
            **os.environ,
            "CLAUDE_PLUGIN_ROOT": str(ROOT),
            "AI_ARCHITECT_SOURCE_CHECKOUT": "1",
        },
        input="\n".join(map(json.dumps, requests)) + "\n",
        text=True,
        capture_output=True,
        check=False,
    )
    require(run.returncode == 0, run.stderr)
    require(response(run.stdout, 1).get("error") is None, "Claude initialize failed")
    require(len(response(run.stdout, 2)["result"]["tools"]) == 26, "Claude tool surface is not 26")
    require(response(run.stdout, 3)["result"].get("isError") is not True, "Claude health_check failed")
    print("CLAUDE PLUGIN SMOKE OK: shipped launcher initialized, listed 26 tools, and called health_check")


if __name__ == "__main__":
    main()
