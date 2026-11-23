"""check_index_cost.py holds the published django indexing cost to a measurement (#448)."""

import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

import check_index_cost as cost  # noqa: E402  (path fix above)

EVALUATION_TABLE = textwrap.dedent(
    """\
    Some prose.

    | `full_ast` | Wall time | Peak RSS | Graph on disk | Nodes | Edges |
    |---|---:|---:|---:|---:|---:|
    | `false` (default) | 53 s | 533 MiB | 111 MiB | 219,874 | 219,318 |
    | `true` | 747 s | 1,531 MiB | 1,091 MiB | 4,270,311 | 4,266,921 |

    More prose.
    """
)

FALSE_ROW = {
    "Wall time": 53.0,
    "Peak RSS": 533.0,
    "Graph on disk": 111.0,
    "Nodes": 219874.0,
    "Edges": 219318.0,
}


class ParseTable(unittest.TestCase):
    def test_reads_every_column_of_the_header(self) -> None:
        table = cost.parse_table(EVALUATION_TABLE)
        self.assertEqual(table["false"], FALSE_ROW)
        self.assertEqual(table["true"]["Nodes"], 4270311.0)
        self.assertEqual(table["true"]["Peak RSS"], 1531.0)

    def test_the_published_tables_parse_and_agree(self) -> None:
        readme = cost.parse_table((ROOT / "README.md").read_text(encoding="utf-8"))
        evaluation = cost.parse_table(
            (ROOT / "docs/evaluation.md").read_text(encoding="utf-8")
        )
        self.assertEqual(set(readme), {"false", "true"})
        self.assertNotIn("Edges", readme["false"])
        self.assertIn("Edges", evaluation["false"])
        self.assertEqual(cost.tables_disagree(readme, evaluation), [])

    def test_a_cell_in_the_wrong_unit_is_refused(self) -> None:
        text = EVALUATION_TABLE.replace("533 MiB", "533 MB")
        with self.assertRaisesRegex(ValueError, "Peak RSS cell '533 MB' is not in MiB"):
            cost.parse_table(text)

    def test_a_text_without_the_table_is_refused(self) -> None:
        with self.assertRaisesRegex(ValueError, "no table"):
            cost.parse_table("| other | table |\n|---|---|\n| a | b |\n")


class Compare(unittest.TestCase):
    def published(self) -> dict:
        return cost.parse_table(EVALUATION_TABLE)

    def test_the_published_figures_agree_with_themselves(self) -> None:
        self.assertEqual(cost.compare("t", self.published(), {"false": FALSE_ROW}), [])

    def test_a_node_count_off_by_one_disagrees(self) -> None:
        found = cost.compare(
            "t", self.published(), {"false": {**FALSE_ROW, "Nodes": 219875.0}}
        )
        self.assertEqual([item.column for item in found], ["Nodes"])

    def test_times_inside_the_band_agree_and_outside_disagree(self) -> None:
        inside = 53.0 * (1 + cost.TOLERANCE) - 0.5
        outside_up = 53.0 * (1 + cost.TOLERANCE) + 0.5
        outside_down = 53.0 * (1 - cost.TOLERANCE) - 0.5
        self.assertEqual(
            cost.compare("t", self.published(), {"false": {**FALSE_ROW, "Wall time": inside}}),
            [],
        )
        for value in (outside_up, outside_down):
            found = cost.compare(
                "t", self.published(), {"false": {**FALSE_ROW, "Wall time": value}}
            )
            self.assertEqual([item.column for item in found], ["Wall time"], value)

    def test_a_column_the_table_lacks_is_not_compared(self) -> None:
        readme = {"false": {k: v for k, v in FALSE_ROW.items() if k != "Edges"}}
        self.assertEqual(
            cost.compare("t", readme, {"false": {**FALSE_ROW, "Edges": 1.0}}), []
        )

    def test_a_row_the_table_does_not_publish_disagrees(self) -> None:
        found = cost.compare("t", {"true": FALSE_ROW}, {"false": FALSE_ROW})
        self.assertEqual(len(found), len(FALSE_ROW))

    def test_two_tables_that_differ_are_named(self) -> None:
        other = {"false": {**FALSE_ROW, "Peak RSS": 600.0}}
        self.assertEqual(
            cost.tables_disagree({"false": FALSE_ROW}, other),
            ["full_ast=false, Peak RSS: README says 533, docs/evaluation.md says 600"],
        )


# A stand-in for the server: answers `initialize`, then one `index_codebase`
# call from `behaviour.json` beside it, after creating `<output_dir>/graph`.
STAND_IN = textwrap.dedent(
    """\
    #!/usr/bin/env python3
    import json, sys
    from pathlib import Path
    b = json.loads((Path(__file__).resolve().parent / "behaviour.json").read_text())
    for line in sys.stdin:
        request = json.loads(line)
        if request["method"] == "initialize":
            answer = {"result": {"serverInfo": {"name": "ai-architect"}}}
        else:
            args = request["params"]["arguments"]
            graph = Path(args["output_dir"]) / "graph"
            graph.mkdir()
            (graph / "data").write_bytes(b"x" * 1024)
            payload = {"status": "ok", "node_count": b["nodes"], "edge_count": 7}
            if b.get("fail"):
                payload = {"status": "error", "message": "boom"}
            answer = {"result": {"content": [{"type": "text",
                                               "text": json.dumps(payload)}]}}
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], **answer}), flush=True)
    """
)


class MeasureRow(unittest.TestCase):
    def stand_in(self, directory: Path, behaviour: dict) -> Path:
        binary = directory / "server"
        binary.write_text(STAND_IN, encoding="utf-8")
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        (directory / "behaviour.json").write_text(json.dumps(behaviour), encoding="utf-8")
        return binary

    def test_reads_counts_from_the_answer_and_size_from_the_graph(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            binary = self.stand_in(Path(tmp), {"nodes": 42})
            figures = cost.measure_row(binary, Path(tmp), full_ast=False)
        self.assertEqual(figures["Nodes"], 42.0)
        self.assertEqual(figures["Edges"], 7.0)
        self.assertGreaterEqual(figures["Graph on disk"], 1.0)
        self.assertGreater(figures["Peak RSS"], 0.0)

    def test_a_failed_index_is_a_measurement_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            binary = self.stand_in(Path(tmp), {"nodes": 1, "fail": True})
            with self.assertRaisesRegex(cost.MeasurementError, "boom"):
                cost.measure_row(binary, Path(tmp), full_ast=False)


class Main(unittest.TestCase):
    def test_a_corpus_at_another_commit_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(["git", "init", "-q", tmp], check=True)
            subprocess.run(
                ["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@t",
                 "commit", "-q", "--allow-empty", "-m", "x"],
                check=True, env={**os.environ, "GIT_CONFIG_GLOBAL": os.devnull},
            )
            code = cost.main(["--binary", "/nonexistent", "--corpus", tmp])
        self.assertEqual(code, 2)

    def test_an_unknown_row_is_refused(self) -> None:
        self.assertEqual(
            cost.main(["--binary", "/x", "--corpus", "/x", "--rows", "maybe"]), 2
        )


if __name__ == "__main__":
    unittest.main()
