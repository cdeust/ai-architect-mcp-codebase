#!/usr/bin/env python3
"""Re-pin the bootstrap manifest digests in bin/ensure-binary.sh.

`bin/ensure-binary.sh` pins the SHA-256 of `.claude-plugin/plugin.json` and of
the `[package]` table of `Cargo.toml` (what is hashed is defined in
`bootstrap_pins.py`) so a marketplace install can detect a tampered package.
`scripts/check_distribution_identity.py` enforces the pins in CI.

The Cargo pin covers the `[package]` table only, not the dependency tables: a
dependency bump (Dependabot's whole job) leaves the pin valid, so such a pull
request is green without a re-pin. A release (version bump, edited package
metadata) changes the table and needs this script.

Recomputing from the repository's own manifests is not a security bypass: on a
branch, the repository IS the source of truth for what the release will contain.
The pin protects an *installed* package against a swapped file at launch time,
which this never touches.

Usage:
    python3 scripts/repin_bootstrap_digests.py          # rewrite the pins
    python3 scripts/repin_bootstrap_digests.py --check  # exit 1 if they drifted
"""
import re
import sys
from pathlib import Path

from bootstrap_pins import PINS, digest

ROOT = Path(__file__).resolve().parent.parent
BOOTSTRAP = ROOT / "bin" / "ensure-binary.sh"


def main() -> int:
    check_only = "--check" in sys.argv[1:]
    text = BOOTSTRAP.read_text(encoding="utf-8")
    drifted = []

    for path, variable, scope in PINS:
        try:
            actual = digest(ROOT, path, scope)
        except ValueError as refused:
            print(f"repin: {variable} has nothing to pin: {refused}", file=sys.stderr)
            return 2
        pattern = rf'^{variable}="[0-9a-f]{{64}}"$'
        match = re.search(pattern, text, re.MULTILINE)
        if match is None:
            print(f"repin: {variable} not found in {BOOTSTRAP}", file=sys.stderr)
            return 2
        if match.group(0) != f'{variable}="{actual}"':
            drifted.append((path, variable, actual))
            text = re.sub(pattern, f'{variable}="{actual}"', text, count=1, flags=re.MULTILINE)

    if not drifted:
        print("repin: bootstrap digests already match")
        return 0

    if check_only:
        for path, variable, actual in drifted:
            print(f"repin: {variable} drifted ({path} -> {actual})", file=sys.stderr)
        print("repin: run `python3 scripts/repin_bootstrap_digests.py` to fix", file=sys.stderr)
        return 1

    BOOTSTRAP.write_text(text, encoding="utf-8")
    for path, variable, actual in drifted:
        print(f"repin: {variable} <- {actual}  ({path})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
