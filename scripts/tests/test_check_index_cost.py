"""check_index_cost.py holds the README's indexing-cost row to a re-measurement (#448)."""

import json
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

import check_index_cost as gate  # noqa: E402  (path fix above)

TABLE = textwrap.dedent(
    """\
    | `full_ast` | Wall time | Peak RSS | Graph on disk | Nodes |
    |---|---:|---:|---:|---:|
    | `false` (default) | 53 s | 533 MiB | 111 MiB | 219,874 |
    | `true` | 747 s | 1,531 MiB | 1,091 MiB | 4,270,311 |
    """
)
PUBLISHED = gate.Cost(53, 533, 111, 219_874)


class PublishedRow(unittest.TestCase):
    def test_reads_the_default_row_with_thousands_separators(self):
        self.assertEqual(gate.published_row(TABLE), PUBLISHED)

    def test_the_tree_readme_has_exactly_one_default_row(self):
        gate.published_row((ROOT / "README.md").read_text(encoding="utf-8"))

    def test_a_readme_without_the_row_fails(self):
        with self.assertRaises(SystemExit):
            gate.published_row(TABLE.replace("`false` (default)", "`false`"))

    def test_a_readme_with_two_rows_fails(self):
        row = TABLE.splitlines()[2]
        with self.assertRaises(SystemExit):
            gate.published_row(TABLE + row + "\n")


class Failures(unittest.TestCase):
    def test_the_published_figures_pass(self):
        self.assertEqual(gate.failures(PUBLISHED, PUBLISHED), [])

    def test_a_cheaper_index_passes(self):
        self.assertEqual(gate.failures(PUBLISHED, gate.Cost(20, 300, 90, 219_874)), [])

    def test_a_cost_up_to_the_ceiling_passes(self):
        at_ceiling = gate.Cost(106, 1066, 222, 219_874)
        self.assertEqual(gate.failures(PUBLISHED, at_ceiling), [])

    def test_a_node_count_that_moved_fails_either_way(self):
        for nodes in (219_873, 219_875):
            [problem] = gate.failures(PUBLISHED, gate.Cost(53, 533, 111, nodes))
            self.assertTrue(problem.startswith("Nodes:"), problem)

    def test_each_cost_above_the_ceiling_fails_on_its_own(self):
        for measured, label in (
            (gate.Cost(107, 533, 111, 219_874), "Wall time"),
            (gate.Cost(53, 1067, 111, 219_874), "Peak RSS"),
            (gate.Cost(53, 533, 223, 219_874), "Graph on disk"),
        ):
            [problem] = gate.failures(PUBLISHED, measured)
            self.assertTrue(problem.startswith(label), problem)

    def test_the_audited_cost_of_issue_448_fails(self):
        # 480 s, 1.7 GB resident, 1.2 GB graph: the release the issue audited.
        audited = gate.Cost(480, 1700 * 10**6 / 2**20, 1200 * 10**6 / 2**20, 4_270_311)
        self.assertEqual(len(gate.failures(PUBLISHED, audited)), 4)


class IndexResult(unittest.TestCase):
    def answer(self, message: dict) -> str:
        return "noise\n" + json.dumps({"jsonrpc": "2.0", "id": 1, "result": {}}) + "\n" + json.dumps(message)

    def test_reads_the_text_block_of_the_index_call(self):
        body = {"node_count": 5, "edge_count": 4, "files_indexed": 2}
        message = {"jsonrpc": "2.0", "id": 2, "result": {"content": [{"type": "text", "text": json.dumps(body)}]}}
        self.assertEqual(gate.index_result(self.answer(message)), body)

    def test_a_tool_error_fails(self):
        message = {"jsonrpc": "2.0", "id": 2, "result": {"isError": True, "content": []}}
        with self.assertRaises(SystemExit):
            gate.index_result(self.answer(message))

    def test_a_protocol_error_fails(self):
        message = {"jsonrpc": "2.0", "id": 2, "error": {"code": -32602, "message": "bad"}}
        with self.assertRaises(SystemExit):
            gate.index_result(self.answer(message))

    def test_no_answer_fails(self):
        with self.assertRaises(SystemExit):
            gate.index_result("")


class DiskMib(unittest.TestCase):
    def test_counts_every_file_of_a_directory_and_a_single_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            graph = Path(tmp) / "graph"
            graph.write_bytes(b"x" * 2**20)
            single = gate.disk_mib(graph)
            self.assertGreaterEqual(single, 1.0)
            (Path(tmp) / "graph.wal").write_bytes(b"y" * 2**20)
            self.assertGreaterEqual(gate.disk_mib(Path(tmp)), single + 1.0)


if __name__ == "__main__":
    unittest.main()
