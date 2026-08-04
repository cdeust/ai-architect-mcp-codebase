#!/usr/bin/env python3
"""Stage and execute the current-platform MCPB payload exactly as shipped."""

from __future__ import annotations

import json
import platform
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    ("Darwin", "arm64"): "macos-aarch64",
    ("Linux", "x86_64"): "linux-x86_64",
    ("Linux", "aarch64"): "linux-aarch64",
}


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
    target = TARGETS[(platform.system(), platform.machine())]
    binary = ROOT / "target/release/ai-architect-mcp-codebase"
    assert binary.is_file(), f"build release binary first: {binary}"
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mcpb-smoke", "version": "ci"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "health_check", "arguments": {}}},
    ]
    with tempfile.TemporaryDirectory(prefix="ai-architect-mcpb-") as tmp:
        stage = Path(tmp)
        for name in ("manifest.json", "launch.sh", "icon.png"):
            shutil.copy2(ROOT / name, stage / name)
        destination = stage / "bin" / target / binary.name
        destination.parent.mkdir(parents=True)
        shutil.copy2(binary, destination)
        run = subprocess.run([str(stage / "launch.sh")], input="\n".join(map(json.dumps, requests)) + "\n", text=True, capture_output=True, check=False)
        assert run.returncode == 0, run.stderr
        assert response(run.stdout, 1).get("error") is None
        tools = response(run.stdout, 2)["result"]["tools"]
        assert len(tools) == 26, f"expected full 26-tool MCPB surface, got {len(tools)}"
        assert response(run.stdout, 3)["result"].get("isError") is not True
    print("MCPB SMOKE OK: staged launcher initialized, listed 26 tools, and called health_check")


if __name__ == "__main__":
    main()
