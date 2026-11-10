# Evaluation and measurements

Each figure on this page names the test or file it comes from and the date
it was measured. Figures from different dates or engine versions are not
comparable with each other.

## Contents

- [Graph tools compared with a Grep/Glob/Read baseline](#graph-tools-compared-with-a-grepglobread-baseline)
- [Indexing cost on a real repository](#indexing-cost-on-a-real-repository)
- [Bulk insert strategies](#bulk-insert-strategies)
- [Search index size](#search-index-size)
- [Synthetic 500-file fixture](#synthetic-500-file-fixture)

## Graph tools compared with a Grep/Glob/Read baseline

This offline retrieval evaluation compares graph queries with a fixed
substring-search and full-file-read protocol on an authored four-language
corpus (Python, TypeScript, Go, Rust): 20 questions across five capability
dimensions. [`PRE_REGISTRATION.md`](../benchmarks/eval_headtohead/PRE_REGISTRATION.md)
records the hypotheses and protocol, committed before the first run. The
results below are the post-#92 run in
[`results.json`](../benchmarks/eval_headtohead/results.json); earlier runs are
saved beside it. [`MANIFEST.md`](../benchmarks/eval_headtohead/MANIFEST.md)
has the provenance, and [`reproduce.sh`](../benchmarks/eval_headtohead/reproduce.sh)
the command. The evaluation is deterministic and needs no API key or external
corpus; building it requires the Rust toolchain and dependencies.

| Metric (mean ± sample stdev, n=20) | Graph tools | Grep/Glob/Read baseline | Source field |
|---|---:|---:|---|
| Retrieval precision | 1.00 ± 0.00 | 0.65 ± 0.33 | `aggregate.{graph,explorer}.precision` |
| Retrieval recall | 1.00 ± 0.00 | 1.00 ± 0.00 | `aggregate.*.recall` |
| Payload token proxy | 43.14 ± 17.26 | 550.36 ± 330.28 | `aggregate.*.tokens` |
| Modeled tool calls | 1.00 ± 0.00 | 5.20 ± 1.64 | `aggregate.*.tool_calls` |
| Mean per-question token ratio (baseline / graph) | 14.26x | | `aggregate.token_ratio_explorer_over_graph.mean` |
| Mean per-question tool-call ratio | 5.20x | | `aggregate.toolcall_ratio_explorer_over_graph.mean` |

All four hypotheses, H1 to H4, are supported in the current run under this
protocol. The original run falsified H4: graph recall was 0.825 against 1.00.
Its five losses (`go-D3`, `go-D4`, `rs-D2`, `rs-D4`, `ts-D4`) remain in
`raw_results.2026-07-26-pre-fix-87.json`, and fixes #87 and #92 closed those
gaps. The corpus informed those fixes, so the current result is a regression
benchmark on the code the fixes were written against.

Costs are modeled from payload sizes; no AI-client bill or tool trace was
observed. The graph leg serializes a benchmark-specific compact envelope of symbol
identities, and the baseline counts its substring-hit transcript plus the full
matching files. Both use a payload-size / 4 token proxy. Each graph question
is assigned one call; each baseline question is assigned two calls plus one
per matching file. Indexing, client prompts, actual MCP response envelopes and
model reasoning are excluded. The 14.26x figure is a mean of per-question
ratios; dividing the aggregate payload volumes gives 12.76x, a different
statistic. The optional answer-quality judge (`AP_EVAL_JUDGE_CMD`) did not
run. These measurements establish file-retrieval results and protocol costs.
AI-agent success, hallucination reduction and real-world token savings were
not measured.

## Indexing cost on a real repository

Issue #448. Measured on 2026-11-10 at main `72bc505`, one run per row (n=1,
no variance estimate), on a Linux x86_64 cloud container (4 vCPU Intel Xeon
@ 2.10 GHz, 15 GiB RAM), rustc 1.95.0, release build with lbug 0.21.2 built
from source (`LBUG_BUILD_FROM_SOURCE=1 ZSTD_SYS_USE_PKG_CONFIG=1`, as in
`ci.json`). Corpus: django tag `5.1.3` (commit `69bf08e3`, 6,824 tracked
files, of which `index_codebase` reported 6,748 as indexed), shallow clone.

| `full_ast` | Wall time | Peak RSS | Graph on disk | Nodes | Edges |
|---|---:|---:|---:|---:|---:|
| `false` (default) | 53 s | 533 MiB | 111 MiB | 219,874 | 219,318 |
| `true` | 747 s | 1,531 MiB | 1,091 MiB | 4,270,311 | 4,266,921 |

The full-AST layer (`AstNode` and `FileContent` rows) adds 4,050,437 nodes,
94.9 % of the `true` graph, which matches the 94.7 % the 2026-10-10 audit
found on release v0.14.0 (macOS arm64: 480 s, 1.7 GB resident, 1.2 GB graph,
with the layer always on). The two machines differ, so only the ratios
between rows are comparable with the audit, not the absolute times.

Procedure: one MCP stdio session per row (`--profile full`), `initialize`,
then one `tools/call` of `index_codebase` with `path` the django checkout,
a fresh empty `output_dir`, `"full": true` and the row's `full_ast`. Wall
time is measured around that call; peak RSS is the server process's
`ru_maxrss` (`getrusage(RUSAGE_CHILDREN)` after it exits); disk is
`du -sm <output_dir>/graph`. Nothing in CI re-runs this measurement yet, so
it is correct for this commit and this machine only.

## Bulk insert strategies

Re-measured on 2026-07-28 with `lbug 0.18`, rustc 1.95.0, macOS 26.5.1 arm64,
by re-running the nine compile-and-run probes in
[`tests/lbug_bulk_investigation.rs`](../tests/lbug_bulk_investigation.rs)
(`cargo test --release --test lbug_bulk_investigation -- --nocapture`, 199
edges per strategy). The ranking matches the original run on `lbug 0.15.3`;
the absolute figures are not comparable across the two runs, because both the
engine version and the machine changed. The crate now depends on `lbug 0.20`,
and this table has not been re-measured on it.

| Strategy | ms/edge |
|---|---|
| Raw string per edge (naive) | 9.658 |
| Prepared statement, no transaction | 6.924 |
| `BEGIN TRANSACTION` + prepared + `COMMIT` | 0.328 |
| UNWIND + typed `LogicalType::Struct` | 0.127 |

The chosen path (UNWIND with a typed struct) is 76 times faster than the naive
one in this measurement. A first version used `LogicalType::Any`, which fails
the binder; the typed struct form works. Prepared statements are cached in a
`RefCell<HashMap<query, PreparedStatement>>` on the `GraphStore`. Clustering
fills one in-memory `HashMap<id, label>` in a single pass, which replaced a
per-node Cypher round-trip (`probe_node_label_for_process`).

## Search index size

The sparse TF-IDF index replaced a dense `N × V × 4B` matrix. The README of
April 2026 reported it 30.5 times smaller on this codebase (108 KB against
3.2 MB); its size grows with the number of non-zero terms instead of the
vocabulary size. The measuring test is
[`tests/tfidf_size_report.rs`](../tests/tfidf_size_report.rs), which indexes
the crate's own `src/` and prints the index size. Its output was not
committed, and the figure has not been re-measured since.

## Synthetic 500-file fixture

[`tests/scalability_bench.rs`](../tests/scalability_bench.rs) generates 500
Rust files (about 10K lines), indexes them end to end, and fails if the run
takes 60 seconds or more. The README of April 2026 reported about 38 seconds
for index, resolve, cluster and search index together; that figure was not
committed with its conditions and has not been re-measured.
