#!/usr/bin/env python3
"""Fail when public distribution manifests drift from the canonical identity."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CANONICAL = "ai-architect-mcp-codebase"


def load(path: str) -> dict:
    return json.loads((ROOT / path).read_text(encoding="utf-8"))


def main() -> None:
    contract = load("mcp-contract.json")
    plugin_name = load(".claude-plugin/plugin.json")["name"]
    marketplace_name = load(".claude-plugin/marketplace.json")["name"]
    server_keys = list(load(".mcp.json")["mcpServers"])
    assert len(server_keys) == 1, f"expected one Claude MCP server key, got {server_keys}"
    server_key = server_keys[0]
    derived_prefix = f"mcp__plugin_{plugin_name}_{server_key}__"

    assertions = {
        "manifest.json": load("manifest.json")["name"],
        ".claude-plugin/plugin.json": load(".claude-plugin/plugin.json")["name"],
        ".claude-plugin/marketplace.json plugin": load(".claude-plugin/marketplace.json")["plugins"][0]["name"],
        ".agents/plugins/marketplace.json": load(".agents/plugins/marketplace.json")["plugins"][0]["name"],
        "Codex plugin.json": load("plugins/ai-architect-mcp-codebase/.codex-plugin/plugin.json")["name"],
        "gemini-extension.json": load("gemini-extension.json")["name"],
    }
    wrong = {where: value for where, value in assertions.items() if value != CANONICAL}
    assert not wrong, f"non-canonical public identities: {wrong}"
    assert marketplace_name == f"{CANONICAL}-marketplace"
    assert contract["distribution"] == CANONICAL
    assert contract["claude_plugin"] == plugin_name
    assert contract["claude_marketplace"] == marketplace_name
    assert contract["mcp_server"] == server_key
    assert contract["claude_tool_prefix"] == derived_prefix
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    assert f'name = "{CANONICAL}"' in cargo
    print(
        f"DISTRIBUTION IDENTITY OK: {CANONICAL} across "
        f"{len(assertions) + 2} declarations; Claude prefix {derived_prefix}"
    )


if __name__ == "__main__":
    main()
