#!/usr/bin/env python3
"""Generate the Codex plugin skills from the canonical root skill sources."""

from __future__ import annotations

import argparse
import filecmp
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "skills"
GENERATED = ROOT / "plugins" / "ai-architect" / "skills"
SKILLS = ("understand-codebase", "impact-analysis", "validate-change-plan")


def _same_tree(source: Path, generated: Path) -> bool:
    comparison = filecmp.dircmp(source, generated)
    if comparison.left_only or comparison.right_only or comparison.funny_files:
        return False
    if any(not filecmp.cmp(source / name, generated / name, shallow=False) for name in comparison.common_files):
        return False
    return all(
        _same_tree(source / name, generated / name)
        for name in comparison.common_dirs
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    drifted = [
        skill
        for skill in SKILLS
        if not _same_tree(SOURCE / skill, GENERATED / skill)
    ]
    if args.check:
        if drifted:
            raise SystemExit(
                "generated Codex skills drifted: "
                + ", ".join(drifted)
                + "; run python3 scripts/sync_portable_skills.py"
            )
        print("portable skill generation is up to date")
        return

    for skill in SKILLS:
        target = GENERATED / skill
        shutil.rmtree(target, ignore_errors=True)
        shutil.copytree(SOURCE / skill, target)
    print(f"generated {len(SKILLS)} Codex skills from {SOURCE.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
