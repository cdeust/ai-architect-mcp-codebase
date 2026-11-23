#!/usr/bin/env python3
"""Re-measure the published cost of indexing django and hold the README to it.

usage: check_index_cost.py <path to the built release binary> [--corpus DIR]

Issue #448 asked for "a timing and size table for a real repository ...
published in the README and checked in CI against a fixed repository". The
table (README, "On the server side, indexing has a cost of its own") was
measured by hand once and nothing re-ran it. This script re-runs the default
row (`full_ast` false) the way docs/evaluation.md describes the procedure: one
MCP stdio session, `initialize`, one `tools/call` of `index_codebase` with
`"full": true`, `"full_ast": false` and a fresh empty `output_dir`, on django
5.1.3 (commit `DJANGO_COMMIT`), shallow-cloned. The `full_ast` true row takes
about twelve minutes and is not re-run.

Two policies, chosen by what the quantity depends on:

  EXACT    Nodes. The count depends only on the corpus, which is pinned, and on
           the extractor, which changes only in a reviewable pull request. A
           pull request that changes it owes the README the new count, the way
           `scripts/check_doc_claims.py` holds the tool and language counts.
  CEILING  Wall time, peak RSS and graph on disk. They depend on the machine as
           well, and the CI runner is not the machine of the table. A
           measurement above `CEILING_FACTOR` times the published figure fails:
           the README then understates the cost, which is what #448 reports. A
           measurement below it passes, since a cheaper index than advertised
           misleads no one.

Nothing here reads the network except the one shallow fetch of django.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import resource
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
README = ROOT / "README.md"

DJANGO_URL = "https://github.com/django/django"
# source: `git ls-remote https://github.com/django/django 'refs/tags/5.1.3^{}'`,
# the commit docs/evaluation.md names for the table.
DJANGO_TAG = "5.1.3"
DJANGO_COMMIT = "69bf08e3a32492998871eb91ad84b3c8d8117180"

# source: the smallest gap issue #448 reports between the audited cost and the
# cost without the AST layer is peak memory, 1.7 GB against 533 MiB (3.2x);
# time and disk are 9x and 10x apart. A ceiling of 2x fails on a regression of
# that size and leaves room for a runner slower than the table's machine.
CEILING_FACTOR = 2.0

# source: 4x the 747 s of the slowest published row, so a server that never
# answers fails the step instead of holding the runner until the job limit.
INDEX_TIMEOUT_S = 3000

ROW_PATTERN = re.compile(
    r"^\|\s*`false`\s*\(default\)\s*\|\s*(?P<wall>[\d,]+)\s*s\s*\|"
    r"\s*(?P<rss>[\d,]+)\s*MiB\s*\|\s*(?P<disk>[\d,]+)\s*MiB\s*\|"
    r"\s*(?P<nodes>[\d,]+)\s*\|\s*$",
    re.MULTILINE,
)


@dataclass(frozen=True)
class Cost:
    """One row of the table: seconds, MiB, MiB, count."""

    wall_s: float
    rss_mib: float
    disk_mib: float
    nodes: int


def published_row(readme_text: str) -> Cost:
    """Post: the README's single `full_ast` false row; exits when there is not
    exactly one, so the gate's subject cannot be deleted to dodge it."""
    rows = list(ROW_PATTERN.finditer(readme_text))
    if len(rows) != 1:
        raise SystemExit(
            f"README.md: expected one `false` (default) row in the indexing-cost "
            f"table, found {len(rows)}"
        )
    row = rows[0]

    def number(name: str) -> int:
        return int(row.group(name).replace(",", ""))

    return Cost(number("wall"), number("rss"), number("disk"), number("nodes"))


def failures(published: Cost, measured: Cost, factor: float = CEILING_FACTOR) -> list[str]:
    """Every way `measured` contradicts `published` under the two policies."""
    found = []
    if measured.nodes != published.nodes:
        found.append(
            f"Nodes: README states {published.nodes:,}, the index produced "
            f"{measured.nodes:,}; update the README and docs/evaluation.md"
        )
    for label, unit, claim, value in (
        ("Wall time", "s", published.wall_s, measured.wall_s),
        ("Peak RSS", "MiB", published.rss_mib, measured.rss_mib),
        ("Graph on disk", "MiB", published.disk_mib, measured.disk_mib),
    ):
        if value > claim * factor:
            found.append(
                f"{label}: measured {value:.0f} {unit}, above {factor:g}x the "
                f"{claim:.0f} {unit} the README states"
            )
    return found


def fetch_django(destination: Path) -> Path:
    """Post: `destination` is a checkout of `DJANGO_COMMIT`."""
    subprocess.run(
        ["git", "clone", "--quiet", "--depth", "1", "--branch", DJANGO_TAG, DJANGO_URL, str(destination)],
        check=True,
    )
    head = subprocess.run(
        ["git", "-C", str(destination), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if head != DJANGO_COMMIT:
        raise SystemExit(f"django {DJANGO_TAG} resolved to {head}, expected {DJANGO_COMMIT}")
    return destination


def requests(corpus: Path, output_dir: Path) -> list[dict]:
    return [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "check-index-cost", "version": "ci"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "index_codebase",
                "arguments": {
                    "path": str(corpus),
                    "output_dir": str(output_dir),
                    "full": True,
                    "full_ast": False,
                },
            },
        },
    ]


def index_result(output: str) -> dict:
    """Post: the JSON body `index_codebase` answered with; exits on an error."""
    for line in output.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(message, dict) or message.get("id") != 2:
            continue
        result = message.get("result") or {}
        if "error" in message or result.get("isError"):
            raise SystemExit(f"index_codebase failed: {line}")
        for block in result.get("content", []):
            if block.get("type") == "text":
                return json.loads(block["text"])
    raise SystemExit(f"index_codebase gave no answer:\n{output[-4000:]}")


def disk_mib(path: Path) -> float:
    """The blocks `path` occupies, in MiB, as `du -sm` (docs/evaluation.md)
    counts them, without its rounding up."""
    files = [path] if path.is_file() else [f for f in path.rglob("*") if f.is_file()]
    return sum(f.stat().st_blocks * 512 for f in files) / 2**20


def measure(binary: Path, corpus: Path, work: Path) -> Cost:
    """Post: the cost of one fresh default index of `corpus` by `binary`.

    Peak RSS is `ru_maxrss` of RUSAGE_CHILDREN; the server is this process's
    only child that has exited when it is read (the git clone ran before and
    is counted too, but it is far smaller than the index)."""
    output_dir = work / "out"
    output_dir.mkdir()
    environment = {**os.environ}
    environment.pop("AP_PROFILE", None)
    payload = "\n".join(map(json.dumps, requests(corpus, output_dir))) + "\n"
    started = time.monotonic()
    run = subprocess.run(
        [str(binary), "--profile", "full"],
        env=environment,
        input=payload,
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=False,
        timeout=INDEX_TIMEOUT_S,
    )
    wall = time.monotonic() - started
    if run.returncode != 0:
        raise SystemExit(f"{binary} exited {run.returncode}:\n{run.stderr[-4000:]}")
    body = index_result(run.stdout)
    # ru_maxrss is KiB on Linux, the only platform the e2e job runs on.
    rss_mib = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024
    graph = Path(body.get("graph_path") or output_dir / "graph")
    disk = disk_mib(graph)
    print(
        f"django {DJANGO_TAG}: {body.get('files_indexed')} files, "
        f"{body.get('node_count')} nodes, {body.get('edge_count')} edges, "
        f"{wall:.0f} s, {rss_mib:.0f} MiB peak RSS, {disk:.0f} MiB on disk"
    )
    return Cost(wall, rss_mib, disk, int(body["node_count"]))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--corpus", type=Path, help="an existing django checkout (default: shallow clone)")
    args = parser.parse_args()
    published = published_row(README.read_text(encoding="utf-8"))
    work = Path(tempfile.mkdtemp(prefix="index-cost-"))
    try:
        corpus = args.corpus or fetch_django(work / "django")
        measured = measure(args.binary.resolve(), corpus.resolve(), work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    problems = failures(published, measured)
    for problem in problems:
        print(f"FAIL {problem}", file=sys.stderr)
    if problems:
        raise SystemExit(1)
    print("indexing cost: README table holds")


if __name__ == "__main__":
    main()
