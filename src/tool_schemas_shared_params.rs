//! Parameters that more than one tool schema declares IDENTICALLY, or with a
//! common body and a per-tool suffix (`full_ast_param`, issue #448).
//!
//! `index_codebase` and `analyze_codebase` both accept these with byte-equal
//! definitions, apart from `full_ast_param`'s per-tool suffix — verified by
//! diffing the emitted `tools/list` documents, not by eye. Two copies of a
//! contract is how they drift: a clarification lands on one tool's
//! description and silently not the other's, and a caller reading the wrong
//! one is misled with nothing failing.
//!
//! `exclude_dirs` is deliberately NOT here. The two tools' texts genuinely
//! differ — `index_codebase` documents that changing it needs `full=true`,
//! which has no meaning for `analyze_codebase` — so they stay separate rather
//! than being forced into a false common definition.

use serde_json::{json, Value};

pub(super) fn dependency_scope_param() -> Value {
    json!({
        "type": "string",
        "enum": ["none", "public_api", "full"],
        "default": "none",
        "description": "Tri-tier control over dependency-directory ingestion. 'none': prune build/dependency dirs (node_modules, .venv, vendor, target, dist, …); only .git is always skipped. 'public_api': descend into those dirs but persist only publicly-visible symbols from files under them — project files are still indexed in full. 'full': descend and persist everything (equivalent to the deprecated include_dependencies=true). Supersedes 'include_dependencies'; if both are given, dependency_scope wins."
    })
}

/// Issue #448: what the full-AST layer is, common to both tools' `full_ast`
/// texts. Each tool appends its own rebuild semantics (`index_codebase` is
/// incremental, `analyze_codebase` always rebuilds), for the same reason
/// `exclude_dirs` is not shared.
const FULL_AST_LAYER: &str = "Also persist the full syntax tree: one AstNode row per tree-sitter node (named and anonymous), linked by Defines_File_AstNode (file -> root) and AstChild_AstNode_AstNode (parent -> child) edges, plus each parsed file's zstd-compressed source as a FileContent row. Default false: no typed tool reads this layer (raw query_graph Cypher can), and on django (7,008 files) AstNode rows were 94.7% of the graph's nodes (issue #448). Symbols, calls, imports, communities and processes are indexed either way.";

/// The `full_ast` parameter with `rebuild_note` appended to the shared text.
pub(super) fn full_ast_param(rebuild_note: &str) -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": format!("{FULL_AST_LAYER} {rebuild_note}")
    })
}

pub(super) fn include_dependencies_param() -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Deprecated — use 'dependency_scope' instead ('true' maps to 'full', 'false' maps to 'none'). Kept as a compatibility alias for one release; emits a deprecation warning."
    })
}

/// The `detail` token-surface parameter (issue #56), shared by list-returning
/// tools. `full` (default) is unchanged behavior; `ids` returns bare identifiers.
pub(super) fn detail_param() -> Value {
    json!({
        "type": "string",
        "enum": ["full", "ids"],
        "default": "full",
        "description": "Token surface (issue #56). 'full' (default): each result as a complete object. 'ids': return ONLY the bare identifiers (qualified names) plus the total — a cheap wide sweep to enumerate what exists before drilling into specific symbols with get_symbol/get_context. Overrides 'format' (an id list needs no table)."
    })
}

/// The `format` token-surface parameter (issue #56), shared by list-returning
/// tools. `json` (default) is objects; `tabular` streams rows as arrays.
pub(super) fn format_param() -> Value {
    json!({
        "type": "string",
        "enum": ["json", "tabular"],
        "default": "json",
        "description": "Token surface (issue #56). 'json' (default): results as objects. 'tabular': declare the columns ONCE (in the response's 'columns' header) and stream each result as an array of cells in that column order — homogeneous result sets stop repeating field names, cutting tokens. Read each row positionally against 'columns'. Ignored when detail='ids'."
    })
}
