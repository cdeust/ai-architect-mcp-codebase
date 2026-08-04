#!/usr/bin/env python3
"""Fail when public distribution manifests drift from the canonical identity."""

from __future__ import annotations

import json
import re
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
    mcp_servers = load(".mcp.json")["mcpServers"]
    server_keys = list(mcp_servers)
    require(len(server_keys) == 1, f"expected one Claude MCP server key, got {server_keys}")
    server_key = server_keys[0]
    # An `env` block in the shipped manifest could set AI_ARCHITECT_SOURCE_CHECKOUT
    # and disable bootstrap verification without touching a single script, so the
    # declaration is restricted to the fields the launcher actually needs.
    declared_fields = set(mcp_servers[server_key])
    require(
        declared_fields <= {"command", "args"},
        f"MCP server declaration carries unexpected fields: {sorted(declared_fields - {'command', 'args'})}",
    )
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
    cargo_version = re.search(r'^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
    require(cargo_version is not None, "Cargo package version missing")
    plugin_version = claude_plugin["version"]
    require(re.fullmatch(r"\d+\.\d+\.\d+", plugin_version) is not None, "plugin version is not stable SemVer")
    require(plugin_version == cargo_version.group(1), "Claude plugin and Cargo versions differ")
    bootstrap = (ROOT / "bin/ensure-binary.sh").read_text(encoding="utf-8")
    minimum = re.search(r'^MINIMUM_VERSION="([^"]+)"$', bootstrap, re.MULTILINE)
    require(minimum is not None and minimum.group(1) == plugin_version, "bootstrap minimum version drifted")
    print(
        f"DISTRIBUTION IDENTITY OK: {CANONICAL} across "
        f"{len(assertions) + 2} declarations; Claude prefix {derived_prefix}"
    )


if __name__ == "__main__":
    main()
