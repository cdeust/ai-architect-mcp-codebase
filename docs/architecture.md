# Architecture

A Rust MCP server with a hand-written stdio JSON-RPC 2.0 transport (no MCP
SDK). The layer rules and coding standards are in
[CONTRIBUTING.md](../CONTRIBUTING.md); the repository layout is in the
[README](../README.md#repository-layout).

## Contents

- [Request path](#request-path)
- [Dependencies](#dependencies)
- [Storage](#storage)
- [Decision records](#decision-records)

## Request path

```
transport (stdio, JSON-RPC framing)
      ↓
main.rs  (request dispatch, tool registry, profile filter)
      ↓
handlers  (one module per tool group: analyze, indexing, query, symbol,
           search/context, process/impact, history, prd, verification)
      ↓
core modules:
    graph_store        LadybugDB port (Cypher, UNWIND, prepared statements)
    parser/            tree-sitter extraction, one spec per language
    indexer/           walk, parse, persist; coverage report
    resolver/          cross-file import, call, impl and use resolution
    lsp_client, lsp_resolver   optional language-server resolution
    clustering/        Louvain with C2 repair, process tracing, impact
    search/            hybrid search (Tantivy BM25, sparse TF-IDF, RRF)
    history/           git history as a version spine
    graph_freshness    staleness receipt on read tools
    prd_input/         graph-derived JSON bundle for a finding or feature
    prd_validator/     checks a PRD's claims against the graph
    security_gates/    graph-aware checks on a list of changed symbols
    semantic_diff      before/after graph comparison
    git_diff           diff parsing and symbol mapping
```

No tool calls a language model. Each tool takes structured JSON arguments,
validates them against its JSON Schema, and returns structured JSON with a
named reason code on error.

## Dependencies

Twenty direct dependencies, each with its justification as a comment in
[`Cargo.toml`](../Cargo.toml). Licenses were read from each crate's own
manifest at the locked version.

| Crate | Purpose | License |
|---|---|---|
| `serde`, `serde_json` | JSON-RPC wire format and artifact persistence | MIT or Apache-2.0 |
| `toml_edit` | Comment-preserving edits to Codex's `config.toml` in `install` | MIT or Apache-2.0 |
| `sha2` | Transcript digest in the clarification workflow | MIT or Apache-2.0 |
| `lbug` (LadybugDB) | Embedded property graph with Cypher | MIT |
| `tree-sitter` | Parser runtime | MIT |
| `tree-sitter-rust`, `-python`, `-typescript`, `-java`, `-kotlin-ng`, `-swift`, `-objc`, `-c`, `-cpp`, `-go`, `-ruby` | Language grammars (11) | MIT |
| `tantivy` | BM25 text search | MIT |
| `zstd` | Compression of the team-shared graph snapshot | BSD-3-Clause |
| `tar` | Archive of the graph directory for the snapshot | MIT or Apache-2.0 |

Not included on purpose: an async runtime (the server blocks on stdio), an
HTTP client, an LLM SDK, and an embedding-model runtime (sparse TF-IDF covers
the semantic half of search with no model).

## Storage

A graph is a LadybugDB database at `<output_dir>/graph/`, for whatever
`output_dir` the indexing call names, with its sidecars (`meta.json`,
`file_manifest.json`, the coverage report and the search index) beside it.
Nothing is shared between two output directories.

The decision record
[`graph-isolation.md`](../stages/decisions/graph-isolation.md) (Lamport
invariant analysis, 2026-04-11) chose one graph per finding, at
`<output_dir>/runs/<run_id>/findings/<finding_id>/graph/`. The server writes
to the `output_dir` its caller names, so a caller gets that isolation by giving
each finding its own output directory. Isolated graphs need no coordination
between concurrent runs, and cleanup is a directory delete. The cost is
redundant indexing when several findings target one codebase; the team-shared
artifact and incremental indexing reduce it.

## Decision records

The early design decisions were each worked through with an agent from
[zetetic-team-subagents](https://github.com/cdeust/zetetic-team-subagents),
where each agent applies one named reasoning method. The records are in
`stages/`.

| Decision | Method | Outcome | Record |
|---|---|---|---|
| Rust or C/C++ for the glue layer | Popper | The conjecture "Rust is the right language" was not refuted. `lbug` and `tree-sitter` already run native C/C++, and Rust is the glue where the borrow checker pays the most. | [`language-choice.md`](../stages/decisions/language-choice.md) |
| Graph per finding or per codebase | Lamport | Per finding. Isolation holds by construction with zero coordination; the redundant-indexing cost can be reduced by a cache later. | [`graph-isolation.md`](../stages/decisions/graph-isolation.md) |
| Decomposition of indexing | Simon | Five steps, satisficed against the growth rule; the first useful query arrives at step 4. | [`stage-3a-plan.md`](../stages/decisions/stage-3a-plan.md) |
| Database backend | dba | LadybugDB (evaluated at `lbug 0.15.3`, now on `0.20`): the only option that was maintained, embedded, native Cypher, with FTS, vector and algorithm extensions. | [`stage-3-db-evaluation.md`](../stages/stage-3-db-evaluation.md) |
| Clarification loop shape | Shannon | A four-tool state machine with an atomic single-file session (no crash window between separate files) and at least one round before finalize. | [`stage-2.md`](../stages/stage-2.md) |
| Bulk insert pattern | dba | UNWIND with `LogicalType::Struct { fields }` works; `LogicalType::Any` fails the binder. The original probe run reported a 38x speedup; the 2026-07-28 re-run measured 76x (see [evaluation](evaluation.md#bulk-insert-strategies)). | [`lbug_bulk_investigation.rs`](../tests/lbug_bulk_investigation.rs) |
