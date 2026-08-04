#!/usr/bin/env python3
"""Start the exact Codex/Gemini MCP commands and call a core tool."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORE_TOOLS = {
    "health_check",
    "analyze_codebase",
    "search_codebase",
    "get_context",
    "get_symbol",
    "get_impact",
    "query_graph",
    "detect_changes",
}


def _json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def _codex_server() -> tuple[dict, Path]:
    plugin_root = ROOT / "plugins" / "ai-architect-mcp-codebase"
    plugin = _json(plugin_root / ".codex-plugin" / "plugin.json")
    mcp_path = plugin_root / plugin["mcpServers"]
    return _json(mcp_path)["mcpServers"]["ai-architect"], plugin_root


def _gemini_server() -> tuple[dict, Path]:
    manifest = _json(ROOT / "gemini-extension.json")
    return manifest["mcpServers"]["ai-architect"], ROOT


def _response(stdout: str, request_id: int) -> dict:
    for line in stdout.splitlines():
        try:
            candidate = json.loads(line)
        except json.JSONDecodeError:
            continue
        if candidate.get("id") == request_id:
            return candidate
    raise AssertionError(f"missing MCP response {request_id}; stdout:\n{stdout}")


def _exercise(host: str, server: dict, cwd: Path) -> None:
    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": host, "version": "ci"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "health_check", "arguments": {}},
        },
    ]
    command = [server["command"], *server.get("args", [])]
    run = subprocess.run(
        command,
        cwd=cwd,
        env={**os.environ, "AP_SMOKE_HOST": host},
        input="\n".join(json.dumps(request) for request in requests) + "\n",
        text=True,
        capture_output=True,
        check=False,
    )
    assert run.returncode == 0, (
        f"{host}: shipped command {command!r} exited {run.returncode}\n"
        f"stderr:\n{run.stderr}\nstdout:\n{run.stdout}"
    )

    initialize = _response(run.stdout, 1)
    assert initialize.get("error") is None
    assert initialize["result"]["serverInfo"]["name"] == "ai-architect"
    assert "core" in initialize["result"]["instructions"].lower()

    listed = _response(run.stdout, 2)
    names = {tool["name"] for tool in listed["result"]["tools"]}
    assert names == CORE_TOOLS, f"{host}: wrong core tool surface: {sorted(names)}"

    called = _response(run.stdout, 3)
    assert called.get("error") is None
    assert called["result"].get("isError") is not True
    health = json.loads(called["result"]["content"][0]["text"])
    assert health["status"] == "ok"
    assert health["tools_count"] == len(CORE_TOOLS)
    print(
        f"PORTABLE HOST SMOKE OK: {host}, "
        f"{initialize['result']['serverInfo']['version']}, "
        f"{health['tools_count']} tools, health_check completed"
    )


def main() -> None:
    for host, loader in (("codex", _codex_server), ("gemini-cli", _gemini_server)):
        server, cwd = loader()
        _exercise(host, server, cwd)


if __name__ == "__main__":
    main()
