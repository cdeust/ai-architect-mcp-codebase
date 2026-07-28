"""Fixtures shared by the doc-claim gate's two suites.

The gate measures a repository, so its tests need one: `fake_src` builds the
miniature source tree every machine source reads, and `complete_readme` builds
a README carrying one site for every claim the gate gates. Both live here
because the suite is split along the same seam as the code under test —
sources in one file, claim comparison in the other.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

_SCRIPTS = Path(__file__).resolve().parents[1]


def load(module: str):
    """Import one of the gate's modules by path.

    `scripts/` is not a package, and both modules are run as scripts in CI, so
    the suites reach them the same way CI does.
    """
    spec = importlib.util.spec_from_file_location(module, _SCRIPTS / f"{module}.py")
    loaded = importlib.util.module_from_spec(spec)
    # Registered before execution because `@dataclass` resolves its annotations
    # through `sys.modules[cls.__module__]`; a module loaded by path alone is
    # absent from that table and the decorator raises on Python 3.10.
    sys.modules[module] = loaded
    spec.loader.exec_module(loaded)
    return loaded


def readme_with(badge: str, alt: str) -> str:
    """A README fragment shaped like the real badge row."""
    return (
        "<p>\n"
        f'  <img src="https://img.shields.io/badge/Coverage-{badge}%25-brightgreen" '
        f'alt="{alt}% line coverage">\n'
        "</p>\n"
    )


FAKE_CORE = ("health_check", "get_symbol")
FAKE_FULL = ("health_check", "get_symbol", "index_status", "ingest_traces")
FAKE_LANGUAGES = ("Rust", "Python", "Ruby")


def fake_src(root: Path, full: tuple[str, ...] = FAKE_FULL) -> Path:
    """A miniature src/ carrying the two files every machine source reads.

    The gate measures the repository, so its tests need a repository — a
    fixture README alone would only prove the regexes fire.
    """

    def rust_array(name: str, names: tuple[str, ...]) -> str:
        body = "\n".join(f'    "{n}",' for n in names)
        return f"const {name}: [&str; {len(names)}] = [\n{body}\n];\n"

    src = root / "src"
    (src / "parser").mkdir(parents=True, exist_ok=True)
    (src / "tool_profile.rs").write_text(
        rust_array("CORE_TOOL_NAMES", FAKE_CORE) + rust_array("FULL_TOOL_NAMES", full)
    )
    variants = "\n".join(f"    {v}," for v in FAKE_LANGUAGES)
    (src / "parser" / "language.rs").write_text(
        f"pub enum Language {{\n{variants}\n}}\n"
    )
    return src


def fake_manifests(
    root: Path,
    *,
    tools: int = len(FAKE_FULL),
    languages: int = len(FAKE_LANGUAGES),
) -> None:
    """The distribution files that advertise counts outside the README.

    The phrasings are written out rather than derived from the gate's own
    registry: a fixture generated from the thing under test would pass no
    matter what the patterns said. Adding a newly gated manifest means adding
    it here too, which is the same doc-update-in-the-same-PR discipline the
    gate exists to enforce.
    """
    (root / ".claude-plugin").mkdir(parents=True, exist_ok=True)
    (root / "docs").mkdir(parents=True, exist_ok=True)
    graph = f"property graph; {tools} tools for agents."
    (root / "server.json").write_text(f'{{"description": "{graph}"}}\n')
    (root / "manifest.json").write_text(
        f'{{"description": "{graph}",\n'
        f' "long_description": "exposed through {tools} MCP tools."}}\n'
    )
    (root / "docs" / "ASSURANCE-CASE.md").write_text(
        f"A client starts it over stdio and calls its {tools} tools.\n"
    )
    (root / ".claude-plugin" / "plugin.json").write_text(
        f'{{"description": "source in {languages} languages with full AST; '
        f'exposes {tools} MCP tools to agents."}}\n'
    )
    (root / ".claude-plugin" / "marketplace.json").write_text(
        f'{{"description": "source in {languages} languages with full AST; '
        f'exposes {tools} tools",\n'
        f'  "tagline": "{tools} MCP tools · all-file indexing"}}\n'
    )


def complete_readme(
    *,
    coverage: str = "81",
    tools: int = len(FAKE_FULL),
    core: int = len(FAKE_CORE),
    hidden: int = len(FAKE_FULL) - len(FAKE_CORE),
    languages: int = len(FAKE_LANGUAGES),
    tests: str = "1000",
    tool_list: tuple[str, ...] = FAKE_FULL,
    layout: str = "automatised-pipeline/\n└── src/",
) -> str:
    """A README carrying one site for every claim the gate gates.

    Each keyword is a seam a test bends to make exactly one claim wrong, so a
    failure names the arm under test rather than a fixture that drifted.
    """
    listed = "\n".join(tool_list)
    return (
        f'<img src="https://img.shields.io/badge/Coverage-{coverage}%25-brightgreen" '
        f'alt="{coverage}% line coverage">\n'
        f'<img src="https://img.shields.io/badge/Tools-{tools}-orange" '
        f'alt="{tools} MCP tools">\n'
        f'<img src="https://img.shields.io/badge/Tests-{tests}+_passing" '
        f'alt="{tests}+ tests">\n'
        f'<img src="https://img.shields.io/badge/Languages-{languages}-blueviolet" '
        f'alt="{languages} languages">\n'
        f"\nThe `core` profile ({core} read-only tools) hides "
        f"the {hidden} hidden tools.\n"
        f"\n## {tools} MCP Tools\n\n```\n{listed}\n```\n"
        f"\n## Repository layout\n\n```\n{layout}\n```\n"
    )


def llvm_cov_json(percent: float) -> str:
    return json.dumps(
        {
            "type": "llvm.coverage.json.export",
            "version": "2.0.1",
            "data": [
                {"totals": {"lines": {"count": 100, "covered": 81, "percent": percent}}}
            ],
        }
    )
