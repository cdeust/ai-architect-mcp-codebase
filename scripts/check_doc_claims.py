#!/usr/bin/env python3
"""README numeric-claim gate.

The README advertises numbers a reader takes as measured facts — coverage
percentage, test count, tool count, language count. Every one of them was
typed by hand, so each was true on the day it was written and silently false
afterwards. AP #161 caught the test count reading 434 against a 947-test
suite; AP #160 caught the coverage badge asserting a figure no CI job
re-measured; the #161 follow-up caught 24 advertised tools against a registry
of 26, 10 advertised languages against 11, and "16 hidden tools" against 18.

This gate makes each advertised number answerable to the produced one. Two
comparison policies are in play, and which one applies is decided by how often
the underlying quantity moves:

  EXACT     tool count, core-profile count, hidden-tool count, language count.
            Each changes only when someone deliberately adds a tool or a
            language — a reviewable interface change that already owes a doc
            update in the same PR (§13.1 E1/H4). The gate enforces that debt.

  FLOORED   coverage percentage, test count. Both move on nearly every commit,
            so an exact claim is correct for exactly one commit. The README
            advertises a round value that FLOORS the measurement, and the gate
            fails when the claim overstates, or has gone more than one bucket
            stale. A gate that reddens for no reason gets disabled — the same
            failure mode scripts/check_marketplace_pins.py documents for
            network flakiness, arrived at from the other direction.

Either way the asymmetry is the point:

  OVERSTATED      advertised > measured -> failure. This is the dishonesty the
                  issues are about: the badge claiming more than the project
                  delivers.
  UNDERSTATED     advertised < measured by more than one bucket -> failure, so
                  a badge cannot be quietly abandoned at an old low number and
                  left to rot.
  NOT_ROUND       a FLOORED claim carrying more precision than its bucket ->
                  failure (see above: precision it cannot keep).
  MISSING_CLAIM   the README stopped advertising a gated quantity at all ->
                  failure, so the gate's subject cannot be deleted to dodge it.
  INCONSISTENT    two places advertise different values for one quantity ->
                  failure; the badge URL and its alt text must agree, or a
                  screen-reader user is told a different number than a sighted
                  one.

Exit codes: 0 claims consistent, 1 drift, 2 error.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# The sibling half of this gate. Added explicitly because both suites load this
# file by path (CI runs `python3 scripts/check_doc_claims.py`, the tests load it
# through importlib), and neither route puts `scripts/` on the import path.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from doc_claim_sources import (  # noqa: E402  (import follows the path fix above)
    README,
    REPO_ROOT,
    SRC,
    measured_coverage,
    measured_languages,
    measured_tests,
    measured_tools,
    tool_names,
)
from doc_claim_targets import (  # noqa: E402  (same)
    EXACT_CLAIMS,
    MANIFEST_CLAIMS,
    ExactClaim,
)

# Percentage points the badge may sit BELOW the measured value before this
# gate fails. This is derived, not chosen: the badge is required to be a whole
# percentage (see check_coverage), so the widest honest gap between a badge and
# the value it floors is exactly one point. A badge reading 81% is correct for
# any measurement in [81.00, 82.00) and stale the moment the true value leaves
# that bucket — which is precisely what this constant expresses.
COVERAGE_TOLERANCE_PP = 1.0

# The same derivation applied to the test count. The badge advertises a round
# hundred that floors the suite size ("900+ tests"), so the widest honest gap
# is exactly one bucket: 900 is correct for any suite in [900, 1000) and stale
# the moment the true count leaves it. Picking a tighter bucket would put the
# README in the diff of every PR that adds a test, which is the chore that
# gets a gate disabled.
TEST_COUNT_BUCKET = 100

# Matches the shields.io badge URL: .../badge/Coverage-81%25
_BADGE_COVERAGE = re.compile(r"/badge/Coverage-(\d+(?:\.\d+)?)%25")
# Matches the accessible-text twin: alt="81% line coverage"
_ALT_COVERAGE = re.compile(r'alt="(\d+(?:\.\d+)?)% line coverage"')


# --------------------------------------------------------------------------
# Advertised claims — extraction from the README.
# --------------------------------------------------------------------------


def claims_for(readme_text: str, claim: ExactClaim) -> list[tuple[str, int]]:
    """Every place the README states `claim`, as (where, value)."""
    found: list[tuple[str, int]] = []
    for where, pattern in claim.patterns:
        found += [(where, int(value)) for value in pattern.findall(readme_text)]
    return found


def advertised_coverage(readme_text: str) -> list[tuple[str, float]]:
    """Every coverage percentage the README advertises, as (where, value)."""
    found = [("badge URL", float(m)) for m in _BADGE_COVERAGE.findall(readme_text)]
    found += [("badge alt text", float(m)) for m in _ALT_COVERAGE.findall(readme_text)]
    return found


def advertised_tests(readme_text: str) -> list[tuple[str, int]]:
    """Every test count the README advertises, as (where, value)."""
    patterns = (
        ("badge URL", re.compile(r"/badge/Tests-(\d+)\+?_passing")),
        ("badge alt text", re.compile(r'alt="(\d+)\+? tests')),
        ("prose", re.compile(r"(\d+)\+ tests")),
    )
    found: list[tuple[str, int]] = []
    for where, pattern in patterns:
        found += [(where, int(value)) for value in pattern.findall(readme_text)]
    return found


# --------------------------------------------------------------------------
# Comparison.
# --------------------------------------------------------------------------


def _missing(label: str, source: str) -> str:
    return (
        f"MISSING_CLAIM: README advertises no {label}. That claim is this "
        f"gate's subject; removing it removes the gate. Restore it, or drop "
        f"the {label} check and say why in the same PR. Measured from {source}."
    )


def _inconsistent(label: str, values: set[int] | set[float]) -> str:
    shown = ", ".join(str(v) for v in sorted(values))
    return (
        f"INCONSISTENT: README advertises more than one {label} ({shown}); "
        f"every place stating it must agree, or a screen-reader user is told a "
        f"different number than a sighted one."
    )


def check_exact(readme_text: str, claim: ExactClaim, measured: int) -> list[str]:
    """Failures comparing every stated `claim` to its machine-measured value."""
    stated = claims_for(readme_text, claim)
    if not stated:
        return [_missing(claim.label, claim.source)]
    failures = [
        f"DRIFTED: README {where} claims a {claim.label} of {value}, "
        f"measured {measured} from {claim.source}."
        for where, value in stated
        if value != measured
    ]
    values = {value for _, value in stated}
    if len(values) > 1:
        failures.append(_inconsistent(claim.label, values))
    return failures


def check_coverage(readme_text: str, measured: float) -> list[str]:
    """Failures comparing every advertised coverage figure to the measurement."""
    claims = advertised_coverage(readme_text)
    if not claims:
        return [_missing("coverage percentage", "the cargo llvm-cov job")]
    failures = []
    for where, value in claims:
        if value != int(value):
            failures.append(
                f"NOT_ROUND: README {where} claims {value}% line coverage. The "
                f"badge must be a whole percentage it can floor. A two-decimal "
                f"badge is exact for one commit and OVERSTATED by the next "
                f"hundredth-point dip, which turns this gate into a chore and "
                f"gets it disabled. Use {int(measured)}%."
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
                f"point(s) stale. Update the badge to {int(measured)}%."
            )
    values = {value for _, value in claims}
    if len(values) > 1:
        failures.append(_inconsistent("coverage percentage", values))
    return failures


def check_tests(readme_text: str, measured: int) -> list[str]:
    """Failures comparing every advertised test count to the suite's own tally."""
    claims = advertised_tests(readme_text)
    if not claims:
        return [_missing("test count", "the cargo test run log")]
    floor = measured - measured % TEST_COUNT_BUCKET
    failures = []
    for where, value in claims:
        if value % TEST_COUNT_BUCKET:
            failures.append(
                f"NOT_ROUND: README {where} claims {value} tests. The badge must "
                f"be a round {TEST_COUNT_BUCKET} it can floor — an exact count is "
                f"correct for one commit and puts the README in the diff of every "
                f"PR that adds a test. Use {floor}+."
            )
            continue
        if value > measured:
            failures.append(
                f"OVERSTATED: README {where} claims {value}+ tests, the suite "
                f"reports {measured} passing."
            )
        elif measured - value >= TEST_COUNT_BUCKET:
            failures.append(
                f"UNDERSTATED: README {where} claims {value}+ tests, the suite "
                f"reports {measured} passing — a full bucket of "
                f"{TEST_COUNT_BUCKET} stale. Update the badge to {floor}+."
            )
    values = {value for _, value in claims}
    if len(values) > 1:
        failures.append(_inconsistent("test count", values))
    return failures


def _layout_block(readme_text: str) -> str | None:
    """The fenced tree under the `## Repository layout` heading, if present."""
    section = re.search(
        r"^## Repository layout\s*\n+```\n(.*?)\n```",
        readme_text,
        re.DOTALL | re.MULTILINE,
    )
    return section.group(1) if section else None


def _layout_paths(block: str) -> list[tuple[int, str]]:
    """Every (line number, repo-relative path) the tree claims exists.

    Depth comes from the tree glyphs: each nesting level is a 4-column prefix,
    so a stack of parent directories reconstructs the full path. A line may
    carry several siblings separated by ` · `, and everything after `←` is
    commentary rather than a path.
    """
    parents: list[str] = []
    found: list[tuple[int, str]] = []
    for lineno, raw in enumerate(block.splitlines(), start=1):
        entry = re.match(r"^((?:(?:│|\s)\s{3})*)(?:├── |└── )(.*)$", raw)
        if not entry:
            continue
        depth = len(entry.group(1)) // 4
        names = entry.group(2).split("←")[0].strip()
        del parents[depth:]
        siblings = [n.strip() for n in names.split("·") if n.strip()]
        for name in siblings:
            found.append((lineno, "/".join(parents + [name.rstrip("/")])))
        # Only an unambiguous single directory can parent the next level.
        if len(siblings) == 1 and siblings[0].endswith("/"):
            parents.append(siblings[0].rstrip("/"))
    return found


def check_layout(readme_text: str, repo_root: Path = REPO_ROOT) -> list[str]:
    """Failures for every path the Repository layout tree shows but does not have.

    AP #161: the tree listed `src/graph_store.rs`, `src/indexer.rs`,
    `src/resolver.rs` and `src/clustering.rs` as files months after each became
    a directory. Existence is checkable, so it is checked — a tree that names a
    path the repository does not have is a map of a territory that moved.
    """
    block = _layout_block(readme_text)
    if block is None:
        return [_missing("Repository layout tree", "the repository itself")]
    failures = []
    for lineno, rel in _layout_paths(block):
        target = repo_root / rel
        if target.exists():
            continue
        kind = "directory" if rel.endswith("/") else "path"
        failures.append(
            f"STALE_LAYOUT: Repository layout tree line {lineno} shows "
            f"`{rel}`, which the repository does not have. Either the {kind} "
            f"moved and the tree did not, or the entry is a typo."
        )
    return failures


def check_tool_inventory(readme_text: str, src_root: Path = SRC) -> list[str]:
    """Failures where the README's tool list and the registry disagree by NAME.

    Counting alone is not enough: the section listed 24 real tools while the
    registry held 26, and a count-only gate would have been satisfied by
    editing the heading to 26 without ever documenting `index_status` or
    `ingest_traces`. Names are what a reader looks up, so names are compared.
    """
    registry = tool_names(src_root / "tool_profile.rs", "FULL_TOOL_NAMES")
    # The intro between the heading and the fence is matched line-by-line
    # rather than with `.*?`: a dot-all wildcard happily swallows this
    # section's own fence and captures the NEXT one, which reports every tool
    # as undocumented while pointing at the wrong block.
    section = re.search(
        r"^## \d+ MCP Tools\n(?:(?!```)[^\n]*\n)*```\n(.*?)\n```",
        readme_text,
        re.DOTALL | re.MULTILINE,
    )
    if not section:
        return [_missing("MCP tool list", "FULL_TOOL_NAMES in src/tool_profile.rs")]
    listed = set(re.findall(r"\b([a-z][a-z0-9_]+)\b", section.group(1)))
    failures = []
    undocumented = sorted(name for name in registry if name not in listed)
    if undocumented:
        failures.append(
            f"UNDOCUMENTED_TOOL: the server registers "
            f"{', '.join(undocumented)} but the README's tool list does not "
            f"mention {'them' if len(undocumented) > 1 else 'it'}. Every "
            f"advertised tool is part of the documented interface."
        )
    phantom = sorted(name for name in listed if name not in registry and "_" in name)
    if phantom:
        failures.append(
            f"PHANTOM_TOOL: the README's tool list names "
            f"{', '.join(phantom)}, which the server does not register."
        )
    return failures


def check_manifests(repo_root: Path = REPO_ROOT, src_root: Path = SRC) -> list[str]:
    """Failures where a distribution manifest advertises a stale count."""
    measured = {
        "tools": measured_tools(src_root),
        "languages": measured_languages(src_root),
    }
    failures = []
    for rel, quantity, pattern in MANIFEST_CLAIMS:
        path = repo_root / rel
        if not path.exists():
            failures.append(f"MISSING_FILE: {rel} is gated here but does not exist.")
            continue
        stated = pattern.findall(path.read_text())
        if not stated:
            failures.append(
                f"MISSING_CLAIM: {rel} no longer states its {quantity} count in "
                f"the form this gate reads (`{pattern.pattern}`). Restore the "
                f"phrasing or update the pattern in the same PR — a rephrase "
                f"must not silently un-gate the claim."
            )
            continue
        failures += [
            f"DRIFTED: {rel} advertises {value} {quantity}, measured "
            f"{measured[quantity]} from src/."
            for value in stated
            if int(value) != measured[quantity]
        ]
    return failures


def check_static_claims(readme_text: str, src_root: Path = SRC) -> list[str]:
    """Every claim measurable from the source tree alone, with no run needed."""
    failures = []
    for claim in EXACT_CLAIMS:
        failures += check_exact(readme_text, claim, claim.measure(src_root))
    return failures


# --------------------------------------------------------------------------
# CLI.
# --------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--coverage-json",
        type=Path,
        help="path to a `cargo llvm-cov report --json` export; enables the "
        "coverage check",
    )
    parser.add_argument(
        "--test-log",
        type=Path,
        help="path to a captured `cargo test` run log; enables the test-count check",
    )
    parser.add_argument(
        "--readme", type=Path, default=README, help="README to check (default: repo)"
    )
    parser.add_argument(
        "--src", type=Path, default=SRC, help="source tree to measure (default: repo)"
    )
    parser.add_argument(
        "--print-coverage",
        action="store_true",
        help="print the measured coverage as markdown and exit 0, checking nothing",
    )
    return parser


def _gather(args: argparse.Namespace, readme_text: str) -> list[str]:
    """Run every check the supplied inputs enable."""
    failures = check_static_claims(readme_text, args.src)
    failures += check_tool_inventory(readme_text, args.src)
    failures += check_layout(readme_text, args.src.parent)
    failures += check_manifests(args.src.parent, args.src)
    if args.coverage_json:
        failures += check_coverage(readme_text, measured_coverage(args.coverage_json))
    if args.test_log:
        failures += check_tests(readme_text, measured_tests(args.test_log))
    return failures


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    if args.print_coverage:
        if not args.coverage_json:
            print("ERROR: --print-coverage needs --coverage-json", file=sys.stderr)
            return 2
        try:
            measured = measured_coverage(args.coverage_json)
        except (OSError, ValueError) as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            return 2
        print(f"**Line coverage: {measured:.2f}%** (floor: 80%)")
        return 0

    try:
        readme_text = args.readme.read_text()
        failures = _gather(args, readme_text)
    except (OSError, ValueError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    for line in failures:
        print(line)
    if failures:
        print(
            f"\n{len(failures)} doc claim(s) disagree with what the repository "
            f"produces. A number a reader takes as a fact has to be one."
        )
        return 1
    print("README numeric claims agree with their machine sources.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
