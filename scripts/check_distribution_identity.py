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
    assert load(".claude-plugin/marketplace.json")["name"] == f"{CANONICAL}-marketplace"
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    assert f'name = "{CANONICAL}"' in cargo
    print(f"DISTRIBUTION IDENTITY OK: {CANONICAL} across {len(assertions) + 2} declarations")


if __name__ == "__main__":
    main()
