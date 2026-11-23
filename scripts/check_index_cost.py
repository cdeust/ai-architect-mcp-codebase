#!/usr/bin/env python3
"""Re-measure the published cost of indexing django and hold the docs to it.

usage: check_index_cost.py --binary <server> --corpus <django checkout>
                           [--rows false,true] [--root <repository>]

Issue #448: `README.md` and `docs/evaluation.md` publish the wall time, peak
RSS, graph size, node count and edge count of one `index_codebase` call over
django 5.1.3, with and without `full_ast`. Those figures were measured once and
nothing re-ran them, so a change to the indexer could move them silently. This
script runs the procedure `docs/evaluation.md` describes on the pinned corpus,
once per requested row, and fails when the published figures and the
measurement disagree:

- `Nodes` and `Edges` must be equal: the graph of a fixed tree by a fixed
  binary is deterministic (the three runs of 2026-11-23 gave the counts of
  2026-11-10, see `docs/evaluation.md`), so any difference is a change of the indexer that the
  table no longer describes.
- `Wall time`, `Peak RSS` and `Graph on disk` must lie within `TOLERANCE` of
  the published value, in either direction: a table that understates the cost
  misleads a user sizing a machine, one that overstates it hides an
  improvement.

The two tables must also agree with each other, and the corpus checkout must be
the commit `docs/evaluation.md` names. Exit 0 when every row agrees, 1 when one
does not (each disagreement is printed), 2 on a usage or measurement error.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, IO

ROOT = Path(__file__).resolve().parents[1]

# source: docs/evaluation.md, "Indexing cost on a real repository": the corpus
# is django tag 5.1.3 at this commit. A table measured on another tree says
# nothing about this one.
CORPUS_COMMIT = "69bf08e3"

# source: docs/evaluation.md, re-measurement of 2026-11-23 (three runs of the
# `false` row, same machine class as the table): wall time 45 to 48 s against
# the published 53 s (a 15 % drift between two sessions, 7 % between runs),
# peak RSS 540 to 542 MiB against 533 (1.7 %), graph 115 MiB against 111
# (3.6 %). About twice the largest drift, so that another machine class (a
# hosted CI runner) is not read as a change. A move larger than this is a
# change the table must record, not noise.
TOLERANCE = 0.35

# source: the `true` row took 747 s on 2026-11-10; four times that bounds a
# hung server without cutting a slow but finished run.
CALL_TIMEOUT_S = 3000

ROW_PATTERN = re.compile(r"^\|\s*`(false|true)`[^|]*\|(.*)\|\s*$")
COLUMN_UNITS = {"Wall time": "s", "Peak RSS": "MiB", "Graph on disk": "MiB"}
TOLERATED = tuple(COLUMN_UNITS)
EXACT = ("Nodes", "Edges")


class MeasurementError(Exception):
    """The server could not be run or answered without the figures."""


@dataclass(frozen=True)
class Disagreement:
    source: str
    row: str
    column: str
    published: float
    measured: float

    def __str__(self) -> str:
        return (
            f"{self.source}, full_ast={self.row}, {self.column}: published "
            f"{self.published:g}, measured {self.measured:g}"
        )


def parse_number(cell: str, column: str) -> float:
    """Pre: `cell` is one cell of the table, e.g. `53 s` or `1,531 MiB`.

    Post: its number, after checking the unit the column expects.
    """
    text = cell.strip().replace(",", "")
    unit = COLUMN_UNITS.get(column)
    if unit is not None:
        if not text.endswith(f" {unit}"):
            raise ValueError(f"{column} cell {cell.strip()!r} is not in {unit}")
        text = text[: -len(unit) - 1]
    try:
        return float(text)
    except ValueError as exc:
        raise ValueError(f"{column} cell {cell.strip()!r} is not a number") from exc


def parse_table(text: str) -> dict[str, dict[str, float]]:
    """Post: the indexing-cost table of `text`, keyed by the `full_ast` value.

    The table is the one whose header starts with `| \\`full_ast\\` |`; its
    columns are read from that header, so README's table (no `Edges`) and
    docs/evaluation.md's table (with `Edges`) are both read.
    """
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if not line.startswith("| `full_ast` |"):
            continue
        header = [cell.strip() for cell in line.strip().strip("|").split("|")][1:]
        rows: dict[str, dict[str, float]] = {}
        for row_line in lines[index + 2 :]:
            match = ROW_PATTERN.match(row_line.strip())
            if match is None:
                break
            cells = match.group(2).split("|")
            if len(cells) != len(header):
                raise ValueError(f"row {row_line.strip()!r} has {len(cells)} cells")
            rows[match.group(1)] = {
                column: parse_number(cell, column)
                for column, cell in zip(header, cells)
            }
        if not rows:
            raise ValueError("the `full_ast` table has no row")
        return rows
    raise ValueError("no table whose header starts with `| `full_ast` |`")


def compare(
    source: str,
    published: dict[str, dict[str, float]],
    measured: dict[str, dict[str, float]],
    tolerance: float = TOLERANCE,
) -> list[Disagreement]:
    """Post: every column of a measured row that disagrees with `published`.

    A measured row the table does not publish is a disagreement on every
    column; a published column that is not measured is not compared.
    """
    found: list[Disagreement] = []
    for row, figures in sorted(measured.items()):
        table_row = published.get(row)
        for column, value in figures.items():
            if table_row is None:
                found.append(Disagreement(source, row, column, float("nan"), value))
                continue
            if column not in table_row:
                continue
            expected = table_row[column]
            if column in EXACT:
                agrees = value == expected
            else:
                agrees = abs(value - expected) <= tolerance * expected
            if not agrees:
                found.append(Disagreement(source, row, column, expected, value))
    return found


def tables_disagree(
    readme: dict[str, dict[str, float]], evaluation: dict[str, dict[str, float]]
) -> list[str]:
    """Post: the cells the two published tables give different values for."""
    found = []
    for row in sorted(set(readme) | set(evaluation)):
        if row not in readme or row not in evaluation:
            found.append(f"full_ast={row} is published in only one table")
            continue
        for column in sorted(set(readme[row]) & set(evaluation[row])):
            if readme[row][column] != evaluation[row][column]:
                found.append(
                    f"full_ast={row}, {column}: README says "
                    f"{readme[row][column]:g}, docs/evaluation.md says "
                    f"{evaluation[row][column]:g}"
                )
    return found


def read_answer(stdout: IO[str], request_id: int) -> dict[str, Any]:
    """Post: the JSON-RPC answer to `request_id`; notifications are skipped."""
    for line in stdout:
        line = line.strip()
        if not line:
            continue
        message = json.loads(line)
        if message.get("id") == request_id:
            return message
    raise MeasurementError(f"the server exited before answering request {request_id}")


def tool_figures(answer: dict[str, Any]) -> dict[str, Any]:
    """Post: the `index_codebase` response carried by a `tools/call` answer."""
    if answer.get("error") is not None:
        raise MeasurementError(f"index_codebase failed: {answer['error']}")
    try:
        text = answer["result"]["content"][0]["text"]
        payload = json.loads(text)
    except (KeyError, IndexError, TypeError, json.JSONDecodeError) as exc:
        raise MeasurementError(f"index_codebase answered {answer!r}") from exc
    if answer["result"].get("isError") or payload.get("status") != "ok":
        raise MeasurementError(f"index_codebase answered {text}")
    return payload


def directory_mib(path: Path) -> float:
    """Post: the size of `path` as `du -sm` reports it (MiB, rounded up)."""
    run = subprocess.run(
        ["du", "-sm", str(path)], capture_output=True, text=True, check=False
    )
    if run.returncode != 0:
        raise MeasurementError(f"du -sm {path} failed: {run.stderr.strip()}")
    return float(run.stdout.split()[0])


def peak_rss_mib(rusage: Any) -> float:
    """Post: `ru_maxrss` in MiB (KiB on Linux, bytes on macOS)."""
    scale = 1024 * 1024 if sys.platform == "darwin" else 1024
    return round(rusage.ru_maxrss / scale)


def measure_row(binary: Path, corpus: Path, full_ast: bool) -> dict[str, float]:
    """Post: the figures of one fresh `index_codebase` call, as documented.

    One server process per row, so its `ru_maxrss` is this call's peak.
    """
    with tempfile.TemporaryDirectory(prefix="index-cost-") as output_dir:
        server = subprocess.Popen(
            [str(binary), "--profile", "full"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
        )
        assert server.stdin is not None and server.stdout is not None
        try:
            send(server.stdin, 1, "initialize", {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "check_index_cost", "version": "1"},
            })
            read_answer(server.stdout, 1)
            started = time.monotonic()
            send(server.stdin, 2, "tools/call", {
                "name": "index_codebase",
                "arguments": {
                    "path": str(corpus),
                    "output_dir": output_dir,
                    "full": True,
                    "full_ast": full_ast,
                },
            })
            payload = tool_figures(read_answer(server.stdout, 2))
            wall = time.monotonic() - started
            server.stdin.close()
            _, status, rusage = os.wait4(server.pid, 0)
            server.returncode = os.waitstatus_to_exitcode(status)
        finally:
            if server.returncode is None:
                server.kill()
                server.wait()
            for stream in (server.stdin, server.stdout):
                if not stream.closed:
                    stream.close()
        return {
            "Wall time": round(wall),
            "Peak RSS": peak_rss_mib(rusage),
            "Graph on disk": directory_mib(Path(output_dir) / "graph"),
            "Nodes": float(payload["node_count"]),
            "Edges": float(payload["edge_count"]),
        }


def send(stdin: IO[str], request_id: int, method: str, params: dict[str, Any]) -> None:
    stdin.write(json.dumps({
        "jsonrpc": "2.0", "id": request_id, "method": method, "params": params,
    }) + "\n")
    stdin.flush()


def corpus_commit(corpus: Path) -> str:
    run = subprocess.run(
        ["git", "-C", str(corpus), "rev-parse", "HEAD"],
        capture_output=True, text=True, check=False,
    )
    if run.returncode != 0:
        raise MeasurementError(f"{corpus} is not a git checkout: {run.stderr.strip()}")
    return run.stdout.strip()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--rows", default="false", help="comma-separated full_ast values")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    rows = [row.strip() for row in args.rows.split(",") if row.strip()]
    if not rows or any(row not in ("false", "true") for row in rows):
        print(f"--rows takes false and/or true, not {args.rows!r}", file=sys.stderr)
        return 2
    try:
        readme = parse_table((args.root / "README.md").read_text(encoding="utf-8"))
        evaluation = parse_table(
            (args.root / "docs/evaluation.md").read_text(encoding="utf-8")
        )
        commit = corpus_commit(args.corpus)
        if not commit.startswith(CORPUS_COMMIT):
            print(f"{args.corpus} is at {commit}, the tables describe {CORPUS_COMMIT}",
                  file=sys.stderr)
            return 2
        measured = {
            row: measure_row(args.binary, args.corpus.resolve(), row == "true")
            for row in rows
        }
    except (OSError, ValueError, MeasurementError) as exc:
        print(f"INDEX COST: {exc}", file=sys.stderr)
        return 2
    for row, figures in measured.items():
        print(f"INDEX COST measured full_ast={row}: " + ", ".join(
            f"{column} {value:g}" for column, value in figures.items()))
    problems = tables_disagree(readme, evaluation) + [
        str(item)
        for source, table in (("README.md", readme), ("docs/evaluation.md", evaluation))
        for item in compare(source, table, measured)
    ]
    for problem in problems:
        print(f"INDEX COST MISMATCH: {problem}")
    if problems:
        return 1
    print(f"INDEX COST OK: {', '.join(rows)} within {TOLERANCE:.0%}, counts exact")
    return 0


if __name__ == "__main__":
    sys.exit(main())
