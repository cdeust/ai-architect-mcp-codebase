//! Graph analysis tool schemas (stages 3b-v2 and 3d-3e): `get_impact`,
//! `search_codebase`, `get_context`, `lsp_resolve`, `detect_changes`.
//!
//! Split out of `tool_schemas.rs` (issue #289) to bring that file under
//! §4.1's 500-line cap. Pure code move, following the pattern established by
//! `tool_schemas_index_codebase.rs` / `tool_schemas_analyze_codebase.rs`.

use super::shared_params::{detail_param, format_param};
use serde_json::{json, Value};

pub(super) fn get_impact_schema() -> Value {
    json!({
        "name": "get_impact",
        "description": "Stage 3c — Reverse-dependency blast radius for a symbol. USE THIS INSTEAD OF grepping a name across the repo to find who depends on it: it returns callers (reverse Calls), importers (reverse Imports), users (reverse Uses), and implementors (reverse Implements), each a re-queryable {id, qualified_name, label, confidence} handle you traverse further via get_symbol/get_context/query_graph — plus the communities and processes affected. Cursor: 'callers' is the PRIMARY paged list (page via next_offset, stable order); importers/users/implementors are byte-capped SUMMARIES from index 0 (secondary_lists_paged=false) — page one at scale via query_graph with ORDER BY. 'dependents_total' is the true pre-truncation size. TOKEN SURFACE (issue #56): detail='ids' → bare qualified names across all four sections; format='tabular' → rows-as-arrays under one 'columns' header. TEMPORAL (issue #58): 'cochange_partners' lists the files that historically change WITH this symbol's file (FILE_CHANGES_WITH, strongest coupling first) — impact candidates the static call graph cannot see (the architect agent's churning-pairs signal). EPISTEMIC HONESTY: 'epistemic'='lower-bound' when the target is reached via dynamic dispatch or heuristically-resolved edges — real impact may exceed what is shown; 'epistemic_reasons' names the carriers. 'unresolved_callsites_naming_target' (issue #283) is the structured count of unresolved CallSite nodes that name this target by its bare identifier but were never linked to a Calls edge — it is what tells an empty 'callers' list apart from a symbol with real callers the resolver failed to bind (e.g. Rust receiver calls without rust-analyzer); 'next_steps' carries a matching lsp_resolve/query_graph hint whenever this is >0 and 'callers' is empty. 'unresolved_callsites_outside_targets' (issue #284) is the subset of that count attributed to files the LSP pass proved sit outside every compiled Cargo target (a Kani proof harness is the common case) — unresolvable by the language server no matter how many times lsp_resolve runs; when >0, both the epistemic_reasons prose and the next_steps hint name the file(s) so a caller does not waste a retry on them. STALENESS (fleet-watch#112): the response's 'graph_freshness' object ('state': fresh|stale|unknown, plus dirty_files/checked_files/commits_behind/commits_ahead) tells you whether the working tree has moved since this graph was indexed — in EITHER direction: 'commits_ahead' > 0 means HEAD sits BEHIND the indexed commit (a checkout to an older revision), which makes the graph just as stale as a forward move. It rides on every response this tool returns, its not-found and error envelopes included — there it is what separates 'this does not exist' from 'this graph predates it'. Distinct key from index_codebase's string-valued 'graph_state'; do not type the two as one field — a 'stale' state means treat this blast radius as a lower-confidence snapshot. Prereqs: a resolved graph (cluster_graph adds the community/process fields).",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "qualified_name"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "qualified_name": {
                    "type": "string",
                    "description": "The qualified name of the symbol to analyze (e.g., 'src/main.rs::handle_tool_call')."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of CALLERS to skip before filling the byte budget (cursor pagination). 'callers' is the PRIMARY paged list, ordered by a stable total order (qualified_name, then id); page through every caller via next_offset. The other reverse-dependency lists (importers, users, implementors) are byte-capped SUMMARIES starting at index 0, not cursored (secondary_lists_paged=false) — to page one of those at scale, query it directly via query_graph with an explicit ORDER BY."
                },
                "detail": detail_param(),
                "format": format_param(),
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. Adds a foreign_callers section — callers of this symbol that live in OTHER repos, repo-tagged and kept separate from the local callers (and from dependents_total) so blast radius distinguishes local from cross-repo impact. Foreign callers are name-matched without a shared linker (confidence 0.50) and make the blast radius a lower bound. Omit for single-repo impact."
                }
            }
        }
    })
}

pub(super) fn search_codebase_schema() -> Value {
    json!({
        "name": "search_codebase",
        "description": "Stage 3d — Ranked keyword search over the code graph. USE THIS INSTEAD OF grep/ripgrep when you know a name or concept but not the exact qualified name: it ranks symbols by hybrid lexical+structural relevance and returns structural context grep cannot — kind, file path, community, process participation, score. Response: 'results' (ranked, cursor-paged), 'total_count', 'by_process' (results grouped by execution flow), and 'next_offset' when more remain — page by re-calling with offset=next_offset until it is absent (stable order: score desc, then qualified_name). TOKEN SURFACE (issue #56): detail='ids' returns only qualified names for a cheap wide sweep before drilling in; format='tabular' streams rows as arrays under a one-line 'columns' header. Narrow with label_filter (Function/Method/Struct/…). COVERAGE CAVEAT (issue #57): a symbol absent from results may simply be UNINDEXED — the graph can be parse-incomplete; if a negative result matters, check index_status / query_graph(graph=\"missed\") and grep the flagged files. DOC CONTENT (fleet-watch#112): also searches the full text of markdown/plain-text/similar doc files (label 'File' in results, kind of hit rather than a symbol) — so a query whose only evidence lives in README/skill/doc prose still returns something, not just code symbols. Built by analyze_codebase's search-index phase; a graph built without it (bare index_codebase/resolve_graph/cluster_graph) falls back to substring search over symbols only. DOC RECALL IS LEXICAL ONLY: doc bodies are indexed by BM25 but NOT by the semantic vector half, so a doc is found by words it actually contains — a query that matches it in meaning but shares no term with it will not surface it, even though a symbol can be found that way STALENESS (fleet-watch#112): the response's 'graph_freshness' object ('state': fresh|stale|unknown, plus dirty_files/checked_files/commits_behind/commits_ahead) tells you whether the working tree has moved since this graph was indexed — in EITHER direction: 'commits_ahead' > 0 means HEAD sits BEHIND the indexed commit (a checkout to an older revision), which makes the graph just as stale as a forward move. It rides on every response this tool returns, its not-found and error envelopes included — there it is what separates 'this does not exist' from 'this graph predates it'. Distinct key from index_codebase's string-valued 'graph_state'; do not type the two as one field. Prereqs: index_codebase + resolve_graph + cluster_graph (or analyze_codebase for all-in-one).",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "query"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "query": {
                    "type": "string",
                    "description": "Search query — one or more keywords (e.g., 'handle_tool', 'GraphStore', 'search result')."
                },
                "limit": {
                    "type": "integer",
                    "default": 20,
                    "description": "Maximum number of ranked candidate results to consider. Bounds the universe the cursor pages within."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of ranked results to skip before filling the byte budget (cursor pagination). Results are in a stable total order: descending score, then ascending qualified_name. Page through them via next_offset until it is absent."
                },
                "label_filter": {
                    "type": "string",
                    "enum": ["Function", "Method", "Struct", "Enum", "Trait", "Module", "Constant", "TypeAlias", "File"],
                    "description": "Optional: only return symbols of this kind. 'File' (fleet-watch#112) restricts to doc/prose content hits — markdown, plain text, and similar files the parser does not turn into symbols — rather than code. 'File' REQUIRES the search index built by analyze_codebase: doc text lives only in that index, never in the graph, so on a graph built without it this value is refused with an explanatory error rather than returning an empty result you could mistake for 'no doc matched'."
                },
                "detail": detail_param(),
                "format": format_param(),
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. Federates the query across them and returns a repo-tagged foreign_results section, kept separate from the primary cursored results so local pagination stays exact. Omit for single-repo search."
                }
            }
        }
    })
}

pub(super) fn get_context_schema() -> Value {
    json!({
        "name": "get_context",
        "description": "Stage 3d — 360° symbol view: the symbol plus ALL its relationships grouped and labeled by kind — imports / imported-by, calls / called-by, implements / implemented-by, community membership, process participation. USE THIS INSTEAD OF reading a file and manually tracing what a symbol touches; it is the richest single-symbol tool (get_symbol returns raw edges, this groups them). Ideal for PRD generation, review prep, and understanding a symbol before changing it. Input: a qualified name ('file_path::symbol_name'); run search_codebase first if you only have a keyword, or get_impact for the reverse-dependency blast radius. COVERAGE CAVEAT (issue #57): grouped relationships are only as complete as the graph — a caller in a parse-incomplete file will be absent; verify negatives with index_status or query_graph(graph=\"missed\").",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "qualified_name"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "qualified_name": {
                    "type": "string",
                    "description": "The qualified name of the symbol (e.g., 'src/main.rs::handle_tool_call')."
                }
            }
        }
    })
}

pub(super) fn lsp_resolve_schema() -> Value {
    json!({
        "name": "lsp_resolve",
        "description": "Stage 3b-v2 — LSP-enhanced resolution. Queries a Language Server Protocol server (rust-analyzer, pyright, typescript-language-server) to resolve method calls on inferred types that the static resolver cannot handle. Runs AFTER resolve_graph. Requires the LSP server to be installed; gracefully fails if not found. Issue #282: if the target sits under a parent workspace that does not list it as a member (or another condition the server reports as `health: \"error\"`), the phase fails loudly as an `lsp_workspace_load_failed` error instead of silently answering every request `[]`. On success, the response's `state` distinguishes `completed` (at least one site resolved, or nothing needed resolving) from `completed_unresolved` (the pass ran but resolved nothing); `server_health` carries the language server's own last-reported health. `outside_targets_count` (issue #284) is sites the pass proved sit outside every compiled Cargo target (see `query_graph(graph=\"missed\").coverage.outside_build_targets`) and therefore never got a `textDocument/definition` request — counted separately from `failed_count` (a real negative answer) and `skipped_count` (budget ran out); it does not by itself flip `state` to `completed_unresolved`.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "codebase_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase)."
                },
                "codebase_path": {
                    "type": "string",
                    "description": "Absolute path to the codebase root."
                },
                "language": {
                    "type": "string",
                    "enum": ["auto", "rust", "python", "typescript", "java", "kotlin", "swift", "objc", "c", "cpp", "go"],
                    "default": "auto",
                    "description": "Language for LSP server selection. 'auto' detects from file extensions."
                },
                "lsp_command": {
                    "type": "string",
                    "description": "Override the LSP server command. Default: auto-detect (rust-analyzer, pyright, typescript-language-server)."
                },
                "timeout_ms": {
                    "type": "integer",
                    "default": 30000,
                    "description": "Total timeout in milliseconds for LSP resolution."
                }
            }
        }
    })
}

pub(super) fn detect_changes_schema() -> Value {
    json!({
        "name": "detect_changes",
        "description": "Stage 3e — Git-diff impact analysis: maps changed lines to the affected symbols, communities, and processes, so you see a diff's blast radius without reading it line by line. USE THIS INSTEAD OF eyeballing a diff to guess what a change touches. Input: either raw unified-diff text, or base_ref/head_ref to run git diff internally. Returns affected symbols with change type (added/modified/deleted), community and process membership, and a heuristic risk score (0.0–1.0) per symbol. Follow up with get_impact on a high-risk symbol to expand its reverse-dependency blast radius. COVERAGE CAVEAT (issue #57): symbols in parse-incomplete files may be missing from the mapping — verify with index_status when a change lands in a flagged file.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase or analyze_codebase)."
                },
                "diff_text": {
                    "type": "string",
                    "description": "Raw unified diff text. Mutually exclusive with base_ref/head_ref."
                },
                "codebase_path": {
                    "type": "string",
                    "description": "Absolute path to the git repository. Required when using base_ref/head_ref."
                },
                "base_ref": {
                    "type": "string",
                    "default": "HEAD~1",
                    "description": "Git ref for the base (e.g., 'HEAD~1', 'main', a commit hash)."
                },
                "head_ref": {
                    "type": "string",
                    "default": "HEAD",
                    "description": "Git ref for the head (e.g., 'HEAD', a branch name)."
                }
            }
        }
    })
}
