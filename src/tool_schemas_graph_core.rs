//! Graph build/query core tool schemas (stages 3a-3c): `ingest_traces`,
//! `index_status`, `query_graph`, `get_symbol`, `resolve_graph`,
//! `cluster_graph`, `get_processes`, `index_history`.
//!
//! Split out of `tool_schemas.rs` (issue #289) to bring that file under
//! §4.1's 500-line cap. Pure code move, following the pattern established by
//! `tool_schemas_index_codebase.rs` / `tool_schemas_analyze_codebase.rs`.

use super::shared_params::{detail_param, format_param};
use serde_json::{json, Value};

pub(super) fn ingest_traces_schema() -> Value {
    json!({
        "name": "ingest_traces",
        "description": "Stage 3 — Fold RUNTIME caller→callee observations (from OTel spans, a profiler, or coverage traces) into the graph (issue #58): static analysis + runtime truth. For each trace it either ANNOTATES the matching static Calls edge with observed_count (the call happened, and static resolution already knew it — now weighted by real traffic), or CREATES an OBSERVED_CALLS edge where static resolution found NO edge (the divergence signal — a call that really happens but the static analyzer missed, e.g. via dynamic dispatch/reflection). Endpoints that are not Function/Method nodes are reported as unresolved. Enterprise uses: dead-code claims backed by production ABSENCE (a symbol with zero observed calls), and hot paths weighted by real traffic. Response: {matched, unmatched_created, unresolved_names} + a capped list of the created divergences and unresolved names. Query the results via query_graph: `MATCH ()-[r:OBSERVED_CALLS_Function_Function]->() RETURN r` for divergences, or Calls_* edges' observed_count for weighted hot paths.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "traces"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory (must be indexed + resolved so Calls edges exist to match against)."
                },
                "traces": {
                    "type": "array",
                    "description": "Runtime caller→callee observations. Each item is {caller, callee, count?} where caller/callee are qualified names ('file::symbol', e.g. 'src/main.rs::handle') and count is the observed call frequency (default 1). Repeated pairs are summed.",
                    "items": {
                        "type": "object",
                        "required": ["caller", "callee"],
                        "additionalProperties": false,
                        "properties": {
                            "caller": { "type": "string", "description": "Qualified name of the calling Function/Method." },
                            "callee": { "type": "string", "description": "Qualified name of the called Function/Method." },
                            "count": { "type": "integer", "minimum": 1, "default": 1, "description": "Observed call count (real traffic weight)." }
                        }
                    }
                }
            }
        }
    })
}

pub(super) fn index_status_schema() -> Value {
    json!({
        "name": "index_status",
        "description": "Stage 3a — Report an indexed graph's status and its indexing-COVERAGE (issue #57): node/edge counts, files indexed, and which files the indexer could NOT fully cover — 'parse_incomplete' (WERE indexed, but tree-sitter left ERROR/MISSING line ranges, so constructs there MAY be missing from the graph — grep those ranges), 'skipped' (not indexed at all: oversized/read-failure/parse-timeout), 'quarantined' (the parser panicked; isolated so it could not kill the index), and 'outside_build_targets' (issue #284, Rust only: WAS indexed — declarations are in the graph — but the file sits outside every compiled Cargo target per `cargo metadata --no-deps`, e.g. a Kani proof harness, so calls out of it cannot be resolved by the language server). Counts are exact; example lists are capped (full lists in the index_coverage.json sidecar). IMPORTANT: absence of a flag is NOT a completeness guarantee — the signal only marks what the indexer can detect. Use before trusting graph completeness on a file; for a structural view of the misses use query_graph(graph=\"missed\").",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory (the path returned by index_codebase)."
                }
            }
        }
    })
}

pub(super) fn query_graph_schema() -> Value {
    json!({
        "name": "query_graph",
        "description": "Stage 3a — Run a read-only Cypher query against the code graph: the ESCAPE HATCH for structural questions the typed tools do not cover (arbitrary MATCH patterns, aggregations, multi-hop traversals). USE THIS INSTEAD OF scripting grep across files for structural facts like 'which functions call X and live in module Y'. Read-only: mutation keywords are rejected. Response: 'columns' + 'rows' (rows as compact arrays), a human-readable 'result' table, 'total_count', 'order_stable', and 'next_offset' — page a large result by re-calling with offset=next_offset, but paging is cursor-safe ONLY when your query declares ORDER BY (order_stable reports whether it does). TOKEN SURFACE (issue #56): format='tabular' drops the redundant human 'result' string (rows+columns already carry the data); detail='ids' collapses to the first column's values for a cheap sweep. COVERAGE HONESTY (issue #57): pass graph=\"missed\" to enumerate what the index does NOT cover (parse-incomplete + skipped + quarantined + outside_build_targets files, issue #284 — Rust files outside every compiled Cargo target) so you know where to prefer grep. IMPORTANT: absence from graph results — or from the 'missed' list — is NOT a completeness guarantee; the signal only marks what the indexer can detect.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory (the path returned by index_codebase)."
                },
                "graph": {
                    "type": "string",
                    "enum": ["default", "missed"],
                    "default": "default",
                    "description": "Which view to query. 'default': run the Cypher 'query' against the code graph. 'missed' (issue #57): ignore 'query' and return the coverage report — the files the index could NOT fully cover (parse_incomplete with flagged line ranges, skipped, quarantined, outside_build_targets — issue #284, Rust files outside every compiled Cargo target), so an agent can pivot to grep. Absence from the missed list is NOT a completeness guarantee."
                },
                "query": {
                    "type": "string",
                    "description": "Cypher query to execute against the graph. Required unless graph=\"missed\"."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of result rows to skip before filling the byte budget (cursor pagination). Page through a large result by re-calling with offset = the previous response's next_offset until next_offset is absent. IMPORTANT: paging is only cursor-safe when your query declares an ORDER BY — the response reports order_stable; without ORDER BY the row order is unspecified and pages may skip or duplicate rows."
                },
                "detail": detail_param(),
                "format": {
                    "type": "string",
                    "enum": ["json", "tabular"],
                    "default": "json",
                    "description": "Token surface (issue #56). query_graph already returns 'rows' as compact arrays with 'columns' declared once. 'json' (default) additionally includes a human-readable 'result' text table. 'tabular' OMITS that 'result' string (the rows+columns already carry the data), cutting tokens. detail='ids' collapses to the first column's values."
                }
            }
        }
    })
}

pub(super) fn get_symbol_schema() -> Value {
    json!({
        "name": "get_symbol",
        "description": "Stage 3a — Exact symbol lookup by qualified name. USE THIS INSTEAD OF opening a file and reading around a definition: given a qualified name ('file_path::symbol_name', e.g. 'src/main.rs::handle_tool_call'), it returns the node's properties (kind, visibility, line span, language) plus EVERY incoming and outgoing edge (defines, calls, imports, implements, …) — the symbol's immediate neighborhood in one call. If you have a keyword but not the qualified name, run search_codebase first; for a grouped 360° view use get_context; for reverse-dependency blast radius use get_impact. Response: the node object + its edge lists. COVERAGE CAVEAT (issue #57): if the symbol is not found it may be UNINDEXED (its file parse-incomplete or skipped) rather than nonexistent — check index_status before concluding it does not exist. STALENESS (fleet-watch#112): the response's 'graph_freshness' object ('state': fresh|stale|unknown, plus dirty_files/checked_files/commits_behind/commits_ahead) tells you whether the working tree has moved since this graph was indexed — in EITHER direction: 'commits_ahead' > 0 means HEAD sits BEHIND the indexed commit (a checkout to an older revision), which makes the graph just as stale as a forward move. It rides on every response this tool returns, its not-found and error envelopes included — there it is what separates 'this does not exist' from 'this graph predates it'. Distinct key from index_codebase's string-valued 'graph_state'; do not type the two as one field.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "qualified_name"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory."
                },
                "qualified_name": {
                    "type": "string",
                    "description": "The qualified name of the symbol to look up (e.g., 'src/main.rs::handle_tool_call')."
                },
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. When the symbol is not defined in this graph, the bridge looks for its definition in these siblings and returns repo-tagged foreign_definitions (re-query the owning repo via the returned `repo`). Omit for single-repo lookups."
                }
            }
        }
    })
}

pub(super) fn resolve_graph_schema() -> Value {
    json!({
        "name": "resolve_graph",
        "description": "Stage 3b — Resolve cross-file edges in the code graph. Runs AFTER index_codebase. Adds Imports, Calls, Implements, Extends, and Uses edges by matching string references to concrete target nodes. Returns resolution statistics including edge counts and resolution rate.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase)."
                },
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. Reports cross_repo_resolvable — how many of this graph's unresolved references a sibling repo can define (a cross-service edge rather than a true third-party dependency) — plus a sample. Omit for single-repo resolution."
                }
            }
        }
    })
}

pub(super) fn cluster_graph_schema() -> Value {
    json!({
        "name": "cluster_graph",
        "description": "Stage 3c — Run community detection and process tracing on an indexed+resolved graph. Groups symbols into functional communities via Louvain+C2 repair, detects entry points (main, test, proof, handler, lib_entry; proof denotes a source harness, not a verified result), and traces BFS call chains to create Process nodes. Requires resolve_graph to have been called first.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase, resolved by resolve_graph)."
                },
                "resolution_param": {
                    "type": "number",
                    "default": 1.0,
                    "description": "Resolution parameter gamma for community detection. Higher = more, smaller communities. Default 1.0."
                }
            }
        }
    })
}

pub(super) fn get_processes_schema() -> Value {
    json!({
        "name": "get_processes",
        "description": "Stage 3c — List all detected processes (execution flows from entry points). Each process has an entry point, entry kind (main/test/proof/handler/lib_entry; proof denotes a source harness, not a verified result), BFS depth, and symbol count. Requires cluster_graph to have been called first.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of processes to skip before filling the byte budget (cursor pagination). Processes are returned in a stable total order (name, then entry_point). Page through them all by re-calling with offset = the previous response's next_offset until next_offset is absent."
                },
                "detail": detail_param(),
                "format": format_param(),
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Accepted for cross-repo API symmetry but NOT acted on: a Process is an intra-graph execution flow (BFS over one graph's call edges), and the bridge topology forbids the super-graph merge that a cross-repo process would require. To trace a flow across repos, follow get_impact's foreign_callers / get_symbol's foreign_definitions into the sibling graph and call get_processes there."
                }
            }
        }
    })
}

pub(super) fn index_history_schema() -> Value {
    json!({
        "name": "index_history",
        "description": "History layer — ingests git commit history into an already-indexed graph as a traversable version spine (not a flat diff report). Creates Commit nodes with author/timestamp/message, PreviousVersion commit ancestry, and a Version node per (entity, commit) for every File and symbol a commit changed — linked by ChangedIn (version→commit) and VersionOf (version→entity), and chained by PreviousVersion (version→prior version). Lets a consumer walk: entity ← VersionOf ← Version → ChangedIn → Commit → PreviousVersion → Commit, and the reverse. File attribution is exact; symbol attribution maps changed lines onto the current graph's symbol ranges (best-effort on older commits). Call after index_codebase + resolve_graph on the same graph_path.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "codebase_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (must already be indexed)."
                },
                "codebase_path": {
                    "type": "string",
                    "description": "Path to the git working tree that was indexed. Must be inside a git repository."
                },
                "max_commits": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of commits to walk, newest first. Defaults to 200."
                }
            }
        }
    })
}
