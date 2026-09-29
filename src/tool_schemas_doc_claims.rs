//! `check_doc_claims` tool schema: a document's claims about a codebase,
//! checked one anchored claim at a time against the graph (`doc_claims`).

use serde_json::{json, Value};

pub(super) fn check_doc_claims_schema() -> Value {
    json!({
        "name": "check_doc_claims",
        "description": "Check what a document (README, changelog, audit) claims about the codebase against the code graph, one anchored claim at a time. The caller reads the document and supplies each claim as {text, file, line, kind, subject, expected}: `text` must appear verbatim at `file:line` under repo_root, else the row is `rejected_anchor`; the tool never interprets prose, it applies the rule of the claim's kind. Kinds: `symbol_exists` / `module_exists` (subject: a qualified name `src/lib.rs::Type::f`, or `Type::f` as a README writes it), `file_exists` (subject: a path), `is_public` (the item is declared with a bare `pub`; Rust only; re-export from the crate root is not checked), `test_count` / `proof_count` (subject: `\"\"` for the whole repo, `dir/` or a file; expected: `\"N\"` exactly or `\">=N\"` at least; counts `#[test]` / `#[kani::proof]` functions), `enum_variant_count` (subject: the enum; expected as for counts). Any other kind is `not_verifiable` with reason `kind_not_supported_yet`. Verdicts: supported, contradicted, not_found, not_verifiable (with the reason), rejected_anchor. An absence never yields `contradicted` (`not_found` instead), and neither does a count the graph holds only as a lower bound: a count claim is contradicted only when the declarations the harness certainly compiles (the compiled floor: files of targets it runs, no `#[cfg]` but its own option, read from the source) already outnumber it. Each row carries its evidence: a Cypher query query_graph can replay, or a repository `file:line`. Deterministic: no clock, rows sorted by (file, line, id). By default only the rows that are not `supported` are returned, with the counts of every verdict; `detail: \"full\"` returns every row. repo_root must be the tree the graph was indexed from. Read-only.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "repo_root", "claims"],
            "additionalProperties": false,
            "properties": {
                "graph_path": { "type": "string", "pattern": "^/.+" },
                "repo_root": { "type": "string", "pattern": "^/.+", "description": "Absolute path of the repository the graph was indexed from. Every file a claim names is read under it; a path leaving it (absolute, `..`, or a symbolic link resolving outside) is refused." },
                "claims": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["text", "file", "line", "kind"],
                        "additionalProperties": false,
                        "properties": {
                            "id": { "type": "string", "description": "Optional; defaults to c<index> in input order." },
                            "text": { "type": "string", "description": "The exact text read, verbatim; may span lines starting at `line`." },
                            "file": { "type": "string", "description": "The document, relative to repo_root." },
                            "line": { "type": "integer", "minimum": 1 },
                            "kind": { "type": "string", "description": "One of the kinds listed in the tool description; another kind is answered not_verifiable (kind_not_supported_yet)." },
                            "subject": { "type": "string" },
                            "expected": { "type": ["string", "integer"] }
                        }
                    }
                },
                "detail": { "type": "string", "enum": ["compact", "full"], "description": "compact (default): only rows that are not supported; full: every row." },
                "format": { "type": "string", "enum": ["json", "tabular"], "description": "tabular: `columns` once, each row an array of cells." },
                "offset": { "type": "integer", "minimum": 0, "description": "Cursor from a previous page's next_offset." }
            }
        }
    })
}
