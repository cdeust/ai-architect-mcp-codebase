#!/usr/bin/env python3
"""Where the docs make each numeric claim, and which source answers it.

Pure data: one registry of README claims, one of the distribution manifests
that advertise counts to a user who never opens the README. Adding a newly
gated number is a row here plus its measurement in `doc_claim_sources` — the
comparison logic in `check_doc_claims` never grows a branch per claim (§1.2).
"""

from __future__ import annotations

import re
import sys
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from doc_claim_sources import (  # noqa: E402  (import follows the path fix above)
    measured_core_tools,
    measured_languages,
    measured_tools,
)


@dataclass(frozen=True)
class ExactClaim:
    """A README quantity gated for exact agreement with its machine source.

    Each row carries its own measurement, so adding a newly gated count is one
    entry here and nothing else — the comparison logic never grows a branch
    per claim (§1.2).
    """

    label: str
    source: str
    measure: Callable[[Path], int]
    patterns: tuple[tuple[str, re.Pattern[str]], ...]


EXACT_CLAIMS: tuple[ExactClaim, ...] = (
    ExactClaim(
        label="tool count",
        source="FULL_TOOL_NAMES in src/tool_profile.rs",
        measure=lambda src: measured_tools(src),
        patterns=(
            ("badge URL", re.compile(r"/badge/Tools-(\d+)-")),
            ("badge alt text", re.compile(r'alt="(\d+) MCP tools"')),
            ("section heading", re.compile(r"^## (\d+) MCP Tools", re.MULTILINE)),
            ("nav anchor", re.compile(r"#(\d+)-mcp-tools")),
            ("intro sentence", re.compile(r"through (\d+) MCP tools")),
            ("summary line", re.compile(r"(\d+) tools\. [\d,]+\+? lines of Rust")),
            ("tool-profile note", re.compile(r"rarely need all (\d+)")),
            ("profile table", re.compile(r"\| `full` \| all (\d+) \|")),
            ("profile example", re.compile(r"default: full \(all (\d+)\)")),
        ),
    ),
    ExactClaim(
        label="core-profile tool count",
        source="CORE_TOOL_NAMES in src/tool_profile.rs",
        measure=lambda src: measured_core_tools(src),
        patterns=(
            (
                "core profile size",
                re.compile(r"`core` profile \((\d+) read-only tools\)"),
            ),
            (
                "core profile note",
                re.compile(r"just the (\d+) code-intelligence tools"),
            ),
        ),
    ),
    ExactClaim(
        label="hidden-tool count",
        source="FULL_TOOL_NAMES minus CORE_TOOL_NAMES in src/tool_profile.rs",
        measure=lambda src: measured_tools(src) - measured_core_tools(src),
        patterns=(("tool-profile note", re.compile(r"the (\d+) hidden tools")),),
    ),
    ExactClaim(
        label="language count",
        source="the `Language` enum in src/parser/language.rs",
        measure=lambda src: measured_languages(src),
        patterns=(
            ("badge URL", re.compile(r"/badge/Languages-(\d+)-")),
            ("badge alt text", re.compile(r'alt="(\d+) languages"')),
        ),
    ),
)


# Files outside the README that advertise the tool or language count to a user
# who never opens the README: the MCP registry entry, the desktop-extension
# manifest, the plugin marketplace listing, the assurance case. Every one of
# them was stale at 24 tools when the README was (AP #161), because nothing
# read them. Pattern per file, so a rephrase fails loudly instead of silently
# un-gating the claim.
MANIFEST_CLAIMS: tuple[tuple[str, str, re.Pattern[str]], ...] = (
    ("server.json", "tools", re.compile(r"property graph; (\d+) tools for agents")),
    ("manifest.json", "tools", re.compile(r"property graph; (\d+) tools for agents")),
    ("manifest.json", "tools", re.compile(r"through (\d+) MCP tools")),
    ("docs/ASSURANCE-CASE.md", "tools", re.compile(r"calls its (\d+) tools")),
    (".claude-plugin/plugin.json", "tools", re.compile(r"exposes (\d+) MCP tools")),
    (".claude-plugin/marketplace.json", "tools", re.compile(r"exposes (\d+) tools")),
    (".claude-plugin/marketplace.json", "tools", re.compile(r'"(\d+) MCP tools · ')),
    (
        ".claude-plugin/plugin.json",
        "languages",
        re.compile(r"source in (\d+) languages"),
    ),
    (
        ".claude-plugin/marketplace.json",
        "languages",
        re.compile(r"source in (\d+) languages"),
    ),
)
