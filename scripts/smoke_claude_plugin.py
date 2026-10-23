#!/usr/bin/env python3
"""Exercise the shipped Claude launcher against its source or release catalog."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
from pathlib import Path
from urllib.request import urlopen

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
        if isinstance(value, dict) and value.get("id") == request_id:
            return value
    raise AssertionError(f"missing response {request_id}: {output}")


def expected_tools(root: Path, version: str, *, installed: bool) -> set[str]:
    # Release installers execute the attested tag's binary, not unreleased HEAD.
    # SemVer 2.0.0 identifiers; also keep the tag inside the fixed URL path.
    # https://semver.org/spec/v2.0.0.html#spec-item-9
    base, has_build, build = version.partition("+")
    core, has_pre, prerelease = base.partition("-")
    valid = bool(re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", core))
    for present, identifiers in ((has_pre, prerelease), (has_build, build)):
        if present:
            valid = valid and all(
                re.fullmatch(r"[0-9A-Za-z-]+", item) for item in identifiers.split(".")
            )
    if has_pre:
        valid = valid and all(
            not (item.isdigit() and len(item) > 1 and item.startswith("0"))
            for item in prerelease.split(".")
        )
    require(bool(valid), "invalid plugin version")
    if installed:
        url = f"https://raw.githubusercontent.com/cdeust/ai-architect-mcp-codebase/v{version}/src/tool_profile.rs"
        try:
            with urlopen(url) as source:
                text = source.read().decode("utf-8")
        except (OSError, UnicodeError) as exc:
            raise SystemExit(
                f"release catalog unavailable for v{version}: {exc}"
            ) from exc
    else:
        text = (root / "src/tool_profile.rs").read_text(encoding="utf-8")
    match = re.search(
        r"FULL_TOOL_NAMES:\s*\[&str;\s*(\d+)\]\s*=\s*\[(.*?)\];", text, re.DOTALL
    )
    require(match is not None, "canonical FULL_TOOL_NAMES catalog missing")
    names = re.findall(r'"(\w+)"', match.group(2))
    require(
        len(names) == int(match.group(1))
        and len(names) == len(set(names))
        and bool(names),
        "canonical tool catalog is inconsistent",
    )
    return set(names)


def launcher_command(server: dict, root: Path, *, windows: bool) -> list[str]:
    command = [
        value.replace("${CLAUDE_PLUGIN_ROOT}", root.as_posix())
        for value in [server["command"], *server.get("args", [])]
    ]
    if windows and command[0] == "bash":
        # Resolve the manifest's executable, never substitute a different launcher.
        bash = shutil.which("bash")
        require(bash is not None, "Bash is required by the shipped Claude manifest")
        command[0] = bash
    return command


def check_tools(tools: list[dict], expected: set[str]) -> None:
    names = [tool["name"] for tool in tools]
    require(
        len(names) == len(set(names)) and set(names) == expected,
        f"Claude tool surface mismatch: missing={sorted(expected - set(names))}, unexpected={sorted(set(names) - expected)}, duplicates={len(names) - len(set(names))}",
    )


def check_health(called: dict, count: int) -> None:
    require(called.get("error") is None, "Claude health_check RPC failed")
    result = called["result"]
    require(result.get("isError") is not True, "Claude health_check failed")
    health = json.loads(result["content"][0]["text"])
    require(health.get("status") == "ok", "Claude health_check is not healthy")
    require(
        health.get("tools_count") == count, "Claude health_check catalog count mismatch"
    )


def smoke_requests() -> list[dict]:
    return [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "claude-plugin-smoke", "version": "ci"},
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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--installed",
        action="store_true",
        help="verify the attested release installed by the plugin bootstrap",
    )
    args = parser.parse_args()
    version = json.loads(
        (ROOT / ".claude-plugin/plugin.json").read_text(encoding="utf-8")
    )["version"]
    expected = expected_tools(ROOT, version, installed=args.installed)
    server = json.loads((ROOT / ".mcp.json").read_text(encoding="utf-8"))["mcpServers"][
        "ai-architect"
    ]
    requests = smoke_requests()
    environment = {**os.environ, "CLAUDE_PLUGIN_ROOT": ROOT.as_posix()}
    environment.pop("AP_PROFILE", None)
    environment["AI_ARCHITECT_SOURCE_CHECKOUT"] = "0" if args.installed else "1"
    run = subprocess.run(
        launcher_command(server, ROOT, windows=os.name == "nt"),
        env=environment,
        cwd=ROOT,
        input="\n".join(map(json.dumps, requests)) + "\n",
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=False,
    )
    require(run.returncode == 0, run.stderr)
    initialize = response(run.stdout, 1)
    require(initialize.get("error") is None, "Claude initialize failed")
    require(
        initialize["result"]["serverInfo"]["name"] == "ai-architect",
        "Claude server identity mismatch",
    )
    require(
        initialize["result"]["serverInfo"]["version"] == version,
        "Claude server version mismatch",
    )
    check_tools(response(run.stdout, 2)["result"]["tools"], expected)
    check_health(response(run.stdout, 3), len(expected))
    print(
        f"CLAUDE PLUGIN SMOKE OK: shipped launcher, v{version}, exact {len(expected)}-tool catalog, health_check ok"
    )


if __name__ == "__main__":
    main()
