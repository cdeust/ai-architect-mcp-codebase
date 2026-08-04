#!/usr/bin/env python3
"""Fail when public distribution manifests drift from the canonical identity."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CANONICAL = "ai-architect-mcp-codebase"
EXPECTED_REPOSITORY = "https://github.com/cdeust/ai-architect-mcp-codebase"
# Deliberately duplicated from the contract: this is a revocation freeze, not
# another derived source. Any history addition requires an explicit gate edit.
REVOKED_PREFIXES = [
    "mcp__plugin_automatised-pipeline_ai-architect__",
    "mcp__plugin_automatised-pipeline_automatised-pipeline__",
    "mcp__plugin_ai-architect-codebase_ai-architect__",
]


def load(path: str) -> dict:
    return json.loads((ROOT / path).read_text(encoding="utf-8"))


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def main() -> None:
    contract = load("mcp-contract.json")
    claude_plugin = load(".claude-plugin/plugin.json")
    claude_marketplace = load(".claude-plugin/marketplace.json")
    plugin_name = claude_plugin["name"]
    marketplace_name = claude_marketplace["name"]
    server_keys = list(load(".mcp.json")["mcpServers"])
    require(len(server_keys) == 1, f"expected one Claude MCP server key, got {server_keys}")
    server_key = server_keys[0]
    derived_prefix = f"mcp__plugin_{plugin_name}_{server_key}__"

    assertions = {
        "manifest.json": load("manifest.json")["name"],
        ".claude-plugin/plugin.json": plugin_name,
        ".claude-plugin/marketplace.json plugin": claude_marketplace["plugins"][0]["name"],
        ".agents/plugins/marketplace.json": load(".agents/plugins/marketplace.json")["plugins"][0]["name"],
        "Codex plugin.json": load("plugins/ai-architect-mcp-codebase/.codex-plugin/plugin.json")["name"],
        "gemini-extension.json": load("gemini-extension.json")["name"],
    }
    wrong = {where: value for where, value in assertions.items() if value != CANONICAL}
    require(not wrong, f"non-canonical public identities: {wrong}")
    require(marketplace_name == f"{CANONICAL}-marketplace", "wrong Claude marketplace name")
    require(contract["schema_version"] == 1, "unsupported MCP contract schema")
    require(claude_plugin["repository"] == EXPECTED_REPOSITORY, "Claude plugin repository trust anchor drifted")
    require(contract["distribution"] == CANONICAL, "contract distribution drifted")
    require(contract["claude_plugin"] == plugin_name, "contract plugin drifted")
    require(contract["claude_marketplace"] == marketplace_name, "contract marketplace drifted")
    require(contract["mcp_server"] == server_key, "contract server key drifted")
    require(contract["claude_tool_prefix"] == derived_prefix, "contract tool prefix drifted")
    require(contract["revoked_claude_tool_prefixes"] == REVOKED_PREFIXES, "revoked prefixes drifted")
    require(derived_prefix not in REVOKED_PREFIXES, "canonical prefix is marked revoked")
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    require(f'name = "{CANONICAL}"' in cargo, "Cargo package identity drifted")
    print(
        f"DISTRIBUTION IDENTITY OK: {CANONICAL} across "
        f"{len(assertions) + 2} declarations; Claude prefix {derived_prefix}"
    )


if __name__ == "__main__":
    main()
