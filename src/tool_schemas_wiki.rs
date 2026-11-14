//! `export_wiki_skeleton` tool schema: the deterministic wiki skeleton of a
//! resolved and clustered graph (`wiki::export`, issue #196).

use serde_json::{json, Value};

pub(super) fn export_wiki_skeleton_schema() -> Value {
    json!({
        "name": "export_wiki_skeleton",
        "description": "Export the deterministic wiki skeleton of a resolved and clustered graph: one page per community, per detected process and per god node (a symbol whose degree is strictly beyond Tukey's far-out fence Q3 + 3·IQR of the clustered degrees), each with its member symbols at file:line, its edges tagged extracted (read from the AST) or inferred (produced by the resolver), its edge counts to each other community, its source files and a coverage block that lists the parse_incomplete files and unresolved call sites of that page (present and empty when the page has none). The canonical JSON (schema ai-architect.wiki-skeleton/1, version embedded) is written to wiki_skeleton.json beside the graph directory, replacing an earlier export; the same graph gives byte-identical bytes. The response names the artifact with its size and SHA-256, the page count per kind, and what the skeleton could not hold (members and edges with no file:line, gaps no page lists, the god-node fence), never omitted. Needs the coverage record the index writes beside the graph (index_coverage.json): without it the call fails, since an empty coverage block would read as no gap. A graph with no community exports with a no_community warning: run cluster_graph (or analyze_codebase) first. No network call, no model client, no subprocess; the graph is only read.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase or analyze_codebase, resolved by resolve_graph, clustered by cluster_graph). The artifact is written to wiki_skeleton.json in its parent directory, beside index_coverage.json."
                }
            }
        }
    })
}
