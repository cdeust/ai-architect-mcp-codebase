#!/usr/bin/env python3
"""Fail when a CycloneDX SBOM does not describe the released crate.

`cargo cyclonedx --all` writes one `sbom.json` per workspace manifest. The
release attests one of them as `ai-architect-mcp-codebase.cdx.json`; picked by
directory order, it was the SBOM of `zera 0.0.1` on v0.13.0. release.yml now
copies the root manifest's file and runs this check before attesting it: the
SBOM's root component must be the released crate at the tag's version.

Usage: python3 scripts/check_sbom_identity.py <sbom.json> --tag v1.2.3
Exit 0 and one line on stdout when it matches; exit 1 and the reason otherwise.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

CRATE = "ai-architect-mcp-codebase"
# source: .github/workflows/release.yml, "Verify tag matches the plugin
# version": a release tag is v<X.Y.Z> or v<X.Y.Z>-rc<N>, both cut from a tree
# whose Cargo version is X.Y.Z.
TAG = re.compile(r"v(\d+\.\d+\.\d+)(-rc\d+)?")


def tag_version(tag: str) -> str:
    """The crate version a release tag is cut from; ValueError on another shape."""
    match = TAG.fullmatch(tag)
    if match is None:
        raise ValueError(f"tag {tag!r} is not v<X.Y.Z> or v<X.Y.Z>-rc<N>")
    return match.group(1)


def faults(sbom: object, version: str) -> list[str]:
    """Why `sbom` is not the CycloneDX SBOM of CRATE at `version`; empty when it is."""
    if not isinstance(sbom, dict):
        return ["the SBOM is not a JSON object"]
    found = []
    if sbom.get("bomFormat") != "CycloneDX":
        found.append(f"bomFormat is {sbom.get('bomFormat')!r}, not 'CycloneDX'")
    metadata = sbom.get("metadata")
    component = metadata.get("component") if isinstance(metadata, dict) else None
    if not isinstance(component, dict):
        return found + ["metadata.component is missing: the SBOM names no root crate"]
    if component.get("name") != CRATE:
        found.append(f"root component is {component.get('name')!r}, not {CRATE!r}")
    if component.get("version") != version:
        found.append(
            f"root component version is {component.get('version')!r}, not {version!r}"
        )
    return found


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sbom", type=Path, help="CycloneDX JSON file")
    parser.add_argument("--tag", required=True, help="the release tag, e.g. v1.2.3")
    args = parser.parse_args(argv)
    try:
        version = tag_version(args.tag)
        sbom = json.loads(args.sbom.read_text(encoding="utf-8"))
    except (ValueError, OSError) as exc:
        print(f"SBOM identity: {exc}", file=sys.stderr)
        return 1
    found = faults(sbom, version)
    for line in found:
        print(f"SBOM identity: {args.sbom}: {line}", file=sys.stderr)
    if found:
        return 1
    count = len(sbom.get("components") or [])
    print(
        f"SBOM IDENTITY OK: {args.sbom} describes {CRATE} {version}, {count} components"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
