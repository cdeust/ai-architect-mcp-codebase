// indexer::persist::ast — full-AST persistence: one `FileContent` row plus
// one `AstNode` row per tree-sitter node (named and anonymous), wired
// straight into the store per file rather than through `SymbolBatch`.
//
// Deliberately NOT routed through `SymbolBatch`'s cross-file dedup: that
// accumulator exists because symbol ids can collide (a parser bug, two
// files defining the "same" qualified name) and a batch-wide `HashSet` catches
// it before one bad row can abort an entire flush. AST node ids can't
// collide by construction (`"{file_id}::ast::{counter}"`, and `file_id` is
// already unique per file), and at full-AST scale (~1 row per tree-sitter
// node, named + anonymous — the crate's own `src/` tree measures roughly 2x
// the pre-existing symbol-node count) accumulating every file's rows in one
// process-lifetime `Vec` before flushing would multiply peak RSS for no
// benefit. Each file's AST rows insert (chunked internally by
// `bulk_insert_nodes`/`bulk_insert_edges`, `BULK_BATCH_SIZE` = 500) and are
// then dropped.

use crate::graph_store::{cypher_str, GraphStore, PropEdge};
use crate::parser::ast_capture::{capture_full_ast, AstCapture};
use crate::parser::Language;

use super::{isolate, Isolated};

/// Captures and persists the full AST for one file: a `FileContent` row (the
/// zstd-compressed source), one `AstNode` row per tree-sitter node, a
/// `Defines_File_AstNode` edge from the file to the tree root, and one
/// `AstChild_AstNode_AstNode` edge per non-root node to its parent.
///
/// Best-effort, mirroring the IaC pass (`indexer::iac::run_iac_pass`): a
/// failure here (a capture panic, a store error) is logged and the file's
/// EXISTING `ParseOutcome` — already decided by `parser::parse_file`, which
/// this never touches — is unaffected. The full-AST layer is additive
/// enrichment, not a precondition for a file counting as indexed.
pub(super) fn persist_full_ast(store: &GraphStore, rel_path: &str, source: &str, lang: Language) {
    let capture = match isolate(|| capture_full_ast(source, rel_path, lang)) {
        Ok(c) => c,
        Err(Isolated::Failed(e)) => {
            eprintln!("indexer: ast_capture skipped for {rel_path}: {e}");
            return;
        }
        Err(Isolated::Panicked) => {
            eprintln!("indexer: ast_capture PANICKED for {rel_path} (isolated; index continues)");
            return;
        }
    };
    // Diagnostic only: `capture.parse_errors` mirrors `ParseResult::parse_errors`
    // from the SAME source under the SAME grammar (see `AstCapture`'s doc
    // comment), so it should already equal what `index_single_file` logged
    // via `set_file_parse_errors` moments ago. A divergence would mean the
    // two parses disagreed — worth a loud signal, not a silent field nobody
    // reads.
    if capture.parse_errors > 0 {
        eprintln!(
            "indexer: ast_capture {rel_path}: {} ERROR/MISSING nodes in the captured tree",
            capture.parse_errors
        );
    }
    if let Err(e) = persist_file_content(store, rel_path, source) {
        eprintln!("indexer: FileContent insert failed for {rel_path}: {e}");
    }
    if let Err(e) = persist_ast_nodes(store, rel_path, lang, &capture) {
        eprintln!("indexer: AstNode insert failed for {rel_path}: {e}");
    }
}

/// zstd-compresses `source` (crate-default level — `zstd::DEFAULT_COMPRESSION_LEVEL`,
/// the ratio/speed balance the zstd manual recommends as its default; the
/// artifact-export path's level-9 "best ratio" tier is reserved for its
/// one-shot whole-graph snapshot, not this per-file, every-indexed-file path)
/// and inserts the `FileContent` row.
fn persist_file_content(store: &GraphStore, rel_path: &str, source: &str) -> Result<(), String> {
    let bytes = source.as_bytes();
    let compressed = zstd::encode_all(bytes, zstd::DEFAULT_COMPRESSION_LEVEL)
        .map_err(|e| format!("zstd compress: {e}"))?;
    store.insert_file_content(rel_path, compressed, bytes.len() as i64)
}

/// Bulk-inserts the `AstNode` rows plus the `Defines_File_AstNode` root edge
/// and the `AstChild_AstNode_AstNode` parent->child edges.
fn persist_ast_nodes(
    store: &GraphStore,
    rel_path: &str,
    lang: Language,
    capture: &AstCapture,
) -> Result<(), String> {
    if capture.nodes.is_empty() {
        return Ok(());
    }
    let language = cypher_str(lang.as_str());
    let mut node_rows: Vec<Vec<(String, String)>> = Vec::with_capacity(capture.nodes.len());
    let mut child_edges: Vec<PropEdge> = Vec::with_capacity(capture.nodes.len().saturating_sub(1));
    for row in &capture.nodes {
        node_rows.push(vec![
            ("id".to_string(), cypher_str(&row.id)),
            ("file_id".to_string(), cypher_str(rel_path)),
            ("kind".to_string(), cypher_str(&row.kind)),
            ("is_named".to_string(), row.is_named.to_string()),
            ("start_byte".to_string(), row.start_byte.to_string()),
            ("end_byte".to_string(), row.end_byte.to_string()),
            ("start_line".to_string(), row.start_line.to_string()),
            ("start_col".to_string(), row.start_col.to_string()),
            ("end_line".to_string(), row.end_line.to_string()),
            ("end_col".to_string(), row.end_col.to_string()),
            ("field_name".to_string(), cypher_str(&row.field_name)),
            ("child_index".to_string(), row.child_index.to_string()),
            ("parent_id".to_string(), cypher_str(&row.parent_id)),
            ("language".to_string(), language.clone()),
        ]);
        if !row.parent_id.is_empty() {
            child_edges.push((
                row.parent_id.clone(),
                row.id.clone(),
                vec![
                    ("child_index".to_string(), row.child_index.to_string()),
                    ("field_name".to_string(), cypher_str(&row.field_name)),
                ],
            ));
        }
    }
    store.bulk_insert_nodes("AstNode", &node_rows)?;
    // Root's id is always `nodes[0].id` — `capture_full_ast`'s pre-order walk
    // emits the root first (see `AstCapture` doc contract).
    let root_id = &capture.nodes[0].id;
    // `Defines_File_AstNode` is a structural-provenance edge (ground-truth
    // AST fact, not a resolver guess) — carries (confidence=1.0,
    // "direct-ast") for the same reason every other `Defines_*` edge does.
    // source: Spike B' BUG #4 — see graph_store::is_structural_provenance_rel.
    let resolution_method = cypher_str("direct-ast");
    store.insert_edge(
        "Defines_File_AstNode",
        rel_path,
        root_id,
        &[
            ("confidence", "1.0"),
            ("resolution_method", &resolution_method),
        ],
    )?;
    store.bulk_insert_edges("AstChild_AstNode_AstNode", &child_edges)?;
    Ok(())
}
