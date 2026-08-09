#!/usr/bin/env python3
"""Re-pin the bootstrap manifest digests in bin/ensure-binary.sh.

`bin/ensure-binary.sh` pins the SHA-256 of `.claude-plugin/plugin.json` and
`Cargo.toml` so a marketplace install can detect a tampered package.
`scripts/check_distribution_identity.py` enforces the pins in CI.

Consequence nobody automated: **every dependency bump changes `Cargo.toml`, so
every Dependabot pull request fails CI on "bootstrap Cargo manifest digest
drifted"** until a human re-pins by hand. Six such pull requests were open and
red at once on 2026-08-09, all on this single cause. This script makes the fix
one command, and the pre-commit hook applies it automatically.

Recomputing from the repository's own manifests is not a security bypass: on a
branch, the repository IS the source of truth for what the release will contain.
The pin protects an *installed* package against a swapped file at launch time,
which this never touches.

Usage:
    python3 scripts/repin_bootstrap_digests.py          # rewrite the pins
    python3 scripts/repin_bootstrap_digests.py --check  # exit 1 if they drifted
"""
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BOOTSTRAP = ROOT / "bin" / "ensure-binary.sh"

# (manifest path, the shell variable pinning its digest)
PINS = (
    (".claude-plugin/plugin.json", "EXPECTED_PLUGIN_MANIFEST_SHA256"),
    ("Cargo.toml", "EXPECTED_CARGO_MANIFEST_SHA256"),
)


def main() -> int:
    check_only = "--check" in sys.argv[1:]
    text = BOOTSTRAP.read_text(encoding="utf-8")
    drifted = []

    for path, variable in PINS:
        actual = hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
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
