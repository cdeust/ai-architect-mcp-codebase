//! Parameters that more than one tool schema declares IDENTICALLY.
//!
//! `index_codebase` and `analyze_codebase` both accept these, with byte-equal
//! definitions — verified by diffing the emitted `tools/list` documents, not by
//! eye. Two copies of a contract is how they drift: a clarification lands on one
//! tool's description and silently not the other's, and a caller reading the
//! wrong one is misled with nothing failing.
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

pub(super) fn include_dependencies_param() -> Value {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Deprecated — use 'dependency_scope' instead ('true' maps to 'full', 'false' maps to 'none'). Kept as a compatibility alias for one release; emits a deprecation warning."
    })
}
