#!/usr/bin/env python3
"""Machine sources for every numeric claim the README makes.

One function per source, and none of them read the README — this module knows
only how to ask the repository what is true. `check_doc_claims.py` owns the
other half: what the README asserts, and how an assertion is compared to the
truth. Splitting them keeps each file answerable to one question (§1.1) and
keeps both under the size cap (§4.1).
"""

from __future__ import annotations

import json
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
README = REPO_ROOT / "README.md"
SRC = REPO_ROOT / "src"


# --------------------------------------------------------------------------


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


def measured_tests(log_path: Path) -> int:
    """Tests that passed, summed over every `test result:` line in a run log.

    Counting `#[test]` attributes statically is wrong — it undercounts by the
    tests our macros generate (691 attributes against 949 real tests, measured
    2026-07-28). The runner's own tally is the only honest source.
    """
    text = log_path.read_text(errors="replace")
    tallies = re.findall(r"^test result: \w+\. (\d+) passed", text, re.MULTILINE)
    if not tallies:
        raise ValueError(
            f"{log_path} contains no `test result:` line — it is not a cargo "
            f"test run log, or the run did not reach a summary."
        )
    return sum(int(n) for n in tallies)


def tool_names(source_path: Path, const_name: str) -> list[str]:
    """The string literals of a fixed-size Rust array constant."""
    text = source_path.read_text()
    body = re.search(rf"{const_name}: \[&str; \d+\] = \[(.*?)\];", text, re.DOTALL)
    if not body:
        raise ValueError(f"{source_path} declares no `{const_name}` array literal")
    return re.findall(r'"(\w+)"', body.group(1))


def rust_array_len(source_path: Path, const_name: str) -> int:
    """Length of a fixed-size Rust array constant: `NAME: [&str; N]`.

    The length is compiler-checked against the literal element list, and
    `tool_profile`'s own test asserts that list equals what `tools_list()`
    registers — so this number cannot drift from the shipped tool surface
    without failing `cargo test` first.
    """
    text = source_path.read_text()
    match = re.search(rf"{const_name}: \[&str; (\d+)\]", text)
    if not match:
        raise ValueError(f"{source_path} declares no `{const_name}: [&str; N]`")
    return int(match.group(1))


def measured_tools(src_root: Path = SRC) -> int:
    """Tools the `full` profile registers."""
    return rust_array_len(src_root / "tool_profile.rs", "FULL_TOOL_NAMES")


def measured_core_tools(src_root: Path = SRC) -> int:
    """Tools the `core` profile registers."""
    return rust_array_len(src_root / "tool_profile.rs", "CORE_TOOL_NAMES")


def measured_languages(src_root: Path = SRC) -> int:
    """Variants of the `Language` enum — the languages the parser dispatches.

    Counted from the enum rather than `spec/registry.rs`, because a shallow-path
    language (Ruby, ADR-0056) ships without a deep-extraction spec entry and
    counting the registry undercounts by one.
    """
    text = (src_root / "parser" / "language.rs").read_text()
    body = re.search(r"pub enum Language \{(.*?)\n\}", text, re.DOTALL)
    if not body:
        raise ValueError(f"{src_root}/parser/language.rs declares no `Language` enum")
    variants = re.findall(r"^\s{4}([A-Z]\w*),\s*$", body.group(1), re.MULTILINE)
    if not variants:
        raise ValueError("the `Language` enum parsed to zero variants")
    return len(variants)
