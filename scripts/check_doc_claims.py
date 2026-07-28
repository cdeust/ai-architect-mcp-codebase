#!/usr/bin/env python3
"""README numeric-claim gate.

The README advertises numbers a reader takes as measured facts — coverage
percentage, test count, tool count. Every one of them was typed by hand, so
each was true on the day it was written and silently false afterwards. AP
#161 caught the test count reading 434 against a 947-test suite; AP #160
caught the coverage badge asserting a figure no CI job re-measured.

This gate makes the advertised number answerable to the produced one. It is
deliberately asymmetric:

  OVERSTATED          advertised > measured -> failure. This is the dishonesty
                      the issues are about: the badge claiming more than the
                      project delivers.
  UNDERSTATED         advertised < measured by more than COVERAGE_TOLERANCE_PP
                      -> failure, so the badge cannot be quietly abandoned at
                      an old low number and left to rot.
  NOT_A_WHOLE_PERCENT the badge carries decimals -> failure.

The badge is required to be a WHOLE percentage that floors the measurement,
and that requirement is load-bearing rather than cosmetic. The first CI run of
this gate measured 81.56% against a hand-typed 81.59% badge and failed, which
is the gate working; but pinning the badge at 81.56 would have made it fail
again the moment coverage moved to 81.55. Coverage shifts by hundredths on
almost every change, so an exact badge is correct for exactly one commit. A
gate that reddens for no reason gets disabled — the same failure mode
scripts/check_marketplace_pins.py documents for network flakiness, arrived at
from the other direction. A whole-percent badge is honest for a whole point of
movement and needs a human only when the true value leaves its bucket.

Exit codes: 0 claims consistent, 1 drift, 2 error.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
README = REPO_ROOT / "README.md"

# Percentage points the badge may sit BELOW the measured value before this
# gate fails. This is derived, not chosen: the badge is required to be a whole
# percentage (see check_coverage), so the widest honest gap between a badge and
# the value it floors is exactly one point. A badge reading 81% is correct for
# any measurement in [81.00, 82.00) and stale the moment the true value leaves
# that bucket — which is precisely what this constant expresses.
COVERAGE_TOLERANCE_PP = 1.0

# Matches the shields.io badge URL: .../badge/Coverage-81.59%25-brightgreen
_BADGE_COVERAGE = re.compile(r"/badge/Coverage-(\d+(?:\.\d+)?)%25")
# Matches the accessible-text twin: alt="81.59% line coverage"
_ALT_COVERAGE = re.compile(r'alt="(\d+(?:\.\d+)?)% line coverage"')


def measured_coverage(report_path: Path) -> float:
    """Total line-coverage percent from a `cargo llvm-cov report --json` export.

    The export is llvm-cov's own schema: a single-element ``data`` array whose
    ``totals.lines.percent`` is the workspace figure the gate compares against.
    """
    payload = json.loads(report_path.read_text())
    try:
        return float(payload["data"][0]["totals"]["lines"]["percent"])
    except (KeyError, IndexError, TypeError, ValueError) as exc:
        raise ValueError(
            f"{report_path} is not a recognisable llvm-cov JSON export "
            f"(expected data[0].totals.lines.percent): {exc}"
        ) from exc


def advertised_coverage(readme_text: str) -> list[tuple[str, float]]:
    """Every coverage percentage the README advertises, as (where, value)."""
    found = [("badge URL", float(m)) for m in _BADGE_COVERAGE.findall(readme_text)]
    found += [("badge alt text", float(m)) for m in _ALT_COVERAGE.findall(readme_text)]
    return found


def check_coverage(readme_text: str, measured: float) -> list[str]:
    """Failures comparing every advertised coverage figure to the measurement."""
    claims = advertised_coverage(readme_text)
    if not claims:
        return [
            "MISSING_CLAIM: README advertises no coverage percentage. The badge "
            "is the artifact this gate exists to keep honest; removing it "
            "removes the gate's subject. Restore it or drop this check."
        ]
    failures = []
    for where, value in claims:
        if value != int(value):
            failures.append(
                f"NOT_A_WHOLE_PERCENT: README {where} claims {value}% line "
                f"coverage. The badge must be a whole percentage it can floor. "
                f"A two-decimal badge is exact for one commit and OVERSTATED by "
                f"the next hundredth-point dip, which turns this gate into a "
                f"chore and gets it disabled. Use {int(measured)}%."
            )
            continue
        if value > measured:
            failures.append(
                f"OVERSTATED: README {where} claims {value:.2f}% line coverage, "
                f"measured {measured:.2f}%."
            )
        elif measured - value > COVERAGE_TOLERANCE_PP:
            failures.append(
                f"UNDERSTATED: README {where} claims {value:.2f}% line coverage, "
                f"measured {measured:.2f}% — more than {COVERAGE_TOLERANCE_PP:.1f} "
                f"point(s) stale. Update the badge."
            )
    values = {value for _, value in claims}
    if len(values) > 1:
        failures.append(
            f"INCONSISTENT: README advertises more than one coverage figure "
            f"({', '.join(f'{v:.2f}%' for v in sorted(values))}); the badge URL "
            f"and its alt text must agree, or a screen-reader user is told a "
            f"different number than a sighted one."
        )
    return failures


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--coverage-json",
        type=Path,
        required=True,
        help="path to a `cargo llvm-cov report --json` export",
    )
    parser.add_argument(
        "--readme", type=Path, default=README, help="README to check (default: repo)"
    )
    parser.add_argument(
        "--print-only",
        action="store_true",
        help="print the measured figure as markdown and exit 0, checking nothing",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        measured = measured_coverage(args.coverage_json)
    except (OSError, ValueError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    if args.print_only:
        print(f"**Line coverage: {measured:.2f}%** (floor: 80%)")
        return 0

    try:
        readme_text = args.readme.read_text()
    except OSError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    failures = check_coverage(readme_text, measured)
    for line in failures:
        print(line)
    if failures:
        print(
            f"\n{len(failures)} doc claim(s) disagree with the measurement "
            f"({measured:.2f}% line coverage). A number a reader takes as a "
            f"fact has to be one."
        )
        return 1
    print(f"README coverage claim agrees with the measurement ({measured:.2f}%).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
