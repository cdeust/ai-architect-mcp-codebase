#!/usr/bin/env python3
"""Fail when public distribution manifests drift from the canonical identity."""

from __future__ import annotations

import hashlib
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
    server_config = mcp_servers[server_key]
    require(
        set(server_config) == {"command", "args"},
        f"unexpected Claude MCP server configuration keys: {sorted(server_config)}",
    )
    require(
        server_config["command"] == "${CLAUDE_PLUGIN_ROOT}/bin/launch-plugin.sh"
        and server_config["args"] == [],
        "Claude MCP launcher contract drifted",
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
    version_assertions = {
        "manifest.json": load("manifest.json")["version"],
        ".claude-plugin/marketplace.json metadata": claude_marketplace["metadata"]["version"],
        ".claude-plugin/marketplace.json plugin": claude_marketplace["plugins"][0]["version"],
        "Codex plugin.json": load("plugins/ai-architect-mcp-codebase/.codex-plugin/plugin.json")["version"],
        "gemini-extension.json": load("gemini-extension.json")["version"],
        "server.json": load("server.json")["version"],
        "server.json package": load("server.json")["packages"][0]["version"],
    }
    wrong_versions = {where: value for where, value in version_assertions.items() if value != plugin_version}
    require(not wrong_versions, f"public version declarations drifted: {wrong_versions}")
    bootstrap = (ROOT / "bin/ensure-binary.sh").read_text(encoding="utf-8")
    expected = re.search(r'^EXPECTED_VERSION="([^"]+)"$', bootstrap, re.MULTILINE)
    require(expected is not None and expected.group(1) == plugin_version, "bootstrap release pin drifted")
    for label, path, variable in (
        ("plugin", ".claude-plugin/plugin.json", "EXPECTED_PLUGIN_MANIFEST_SHA256"),
        ("Cargo", "Cargo.toml", "EXPECTED_CARGO_MANIFEST_SHA256"),
    ):
        pinned = re.search(rf'^{variable}="([0-9a-f]{{64}})"$', bootstrap, re.MULTILINE)
        actual = hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
        require(pinned is not None and pinned.group(1) == actual, f"bootstrap {label} manifest digest drifted")
    print(
        f"DISTRIBUTION IDENTITY OK: {CANONICAL} across "
        f"{len(assertions) + 2} declarations; Claude prefix {derived_prefix}"
    )


if __name__ == "__main__":
    main()
