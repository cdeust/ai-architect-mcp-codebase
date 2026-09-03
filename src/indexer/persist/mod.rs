use super::SymbolBatch;
use crate::graph_store::{cypher_str, GraphStore};
use crate::parser::{self, Language};
use std::collections::HashMap;
use std::path::Path;

mod ast;
mod edges;
mod nodes;

// ---------------------------------------------------------------------------
// Directory and File node insertion
// ---------------------------------------------------------------------------

/// Inserts Directory nodes for all ancestor dirs of a file (relative to root).
/// `Contains_Dir_Dir` edges are staged into `batch` (flushed in large bulk
/// calls alongside the symbol edges) rather than inserted one at a time.
/// source: ADR-4253701 §Decision 2 (levier 2, persist.rs:89) — this was the
/// last per-edge insert loop for directory-tree structural edges.
pub(super) fn insert_ancestor_dirs(
    store: &GraphStore,
    batch: &mut SymbolBatch,
    root: &Path,
    file_path: &Path,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    label_by_qn: &mut HashMap<String, String>,
) -> Result<(), String> {
    let rel = super::relative_path(root, file_path);
    let mut current = std::path::PathBuf::new();
    // Walk each component except the filename itself.
    if let Some(parent) = rel.parent() {
        for component in parent.components() {
            let prev = current.clone();
            current.push(component);
            if seen.contains(&current) {
                continue;
            }
            seen.insert(current.clone());
            let dir_id = current.to_string_lossy();
            let dir_name = component.as_os_str().to_string_lossy();
            insert_directory_node(store, &dir_id, &dir_name)?;
            label_by_qn.insert(dir_id.to_string(), "Directory".into());
            if !prev.as_os_str().is_empty() {
                batch.push_edge(
                    "Contains_Dir_Dir",
                    prev.to_string_lossy().into_owned(),
                    dir_id.into_owned(),
                    Vec::new(),
                );
            }
        }
    }
    Ok(())
}

fn insert_directory_node(store: &GraphStore, id: &str, name: &str) -> Result<(), String> {
    store.insert_node(
        "Directory",
        &[
            ("id", &cypher_str(id)),
            ("path", &cypher_str(id)),
            ("name", &cypher_str(name)),
        ],
    )
}

pub(super) fn insert_file_node(
    store: &GraphStore,
    abs_path: &Path,
    rel_path: &str,
) -> Result<(), String> {
    let name = abs_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = abs_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let size = std::fs::metadata(abs_path).map(|m| m.len()).unwrap_or(0);
    store.insert_node(
        "File",
        &[
            ("id", &cypher_str(rel_path)),
            ("path", &cypher_str(rel_path)),
            ("name", &cypher_str(&name)),
            ("extension", &cypher_str(&ext)),
            ("size_bytes", &size.to_string()),
            // Inserted before the parse runs; index_single_file backfills the real
            // count via set_file_parse_errors once the file is parsed. 0 = "no
            // errors" and also the correct value for non-code files (never parsed).
            ("parse_errors", "0"),
        ],
    )
}

// ---------------------------------------------------------------------------
// Structural edges: Contains
// ---------------------------------------------------------------------------

/// Stages a `Contains_Dir_File` edge into `batch` instead of inserting it
/// directly. source: ADR-4253701 §Decision 2 (levier 2, persist.rs:85).
pub(super) fn insert_dir_file_edge(batch: &mut SymbolBatch, rel_path: &Path) {
    let file_id = rel_path.to_string_lossy().into_owned();
    let parent_id = rel_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    if parent_id.is_empty() {
        return; // file is at root level, no parent directory node
    }
    batch.push_edge("Contains_Dir_File", parent_id, file_id, Vec::new());
}

// ---------------------------------------------------------------------------
// Single-file indexing: parse → insert nodes → insert edges
// ---------------------------------------------------------------------------

/// The coverage-relevant outcome of indexing one file (issue #57). Infallible by
/// design: every failure mode is CLASSIFIED (not propagated as an error that
/// could abort the whole index), so one bad file can never kill the run.
pub(super) enum ParseOutcome {
    /// Parsed cleanly, or a non-code file that is a legitimate File node with no
    /// symbols. Fully covered — no gap.
    Indexed,
    /// Parsed, but the tree had ERROR/MISSING ranges (1-based line spans);
    /// constructs inside them may be missing from the graph.
    Partial(Vec<(u32, u32)>),
    /// Not indexed at all (unreadable / oversized / parse timeout). Reason inside.
    Skipped(String),
    /// The parser PANICKED; the panic was caught and isolated so it could not
    /// kill the index. The file is left uncovered. Reason inside.
    Quarantined(String),
}

pub(super) fn index_single_file(
    store: &GraphStore,
    batch: &mut SymbolBatch,
    abs_path: &Path,
    rel_path: &str,
    label_by_qn: &mut HashMap<String, String>,
    seen_node_ids: &mut std::collections::HashSet<(String, String)>,
    restrict_to_public_api: bool,
) -> ParseOutcome {
    // Detect language FIRST (cheap, no I/O). Under all-file indexing the
    // walker yields every file, so most non-code files reach here: they are
    // NOT parsed (no grammar) — their File node was already inserted by the
    // caller, and any lightweight cross-file links (e.g. .js import/require)
    // are emitted in a dedicated post-pass. A non-code file is fully covered as
    // a File node (no symbols expected), so it is `Indexed`, not a gap.
    // source: all-file indexing — every file is a File node; only supported
    // languages get a full AST.
    let ext = abs_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let lang = match Language::from_extension(ext) {
        Some(l) => l,
        None => return ParseOutcome::Indexed,
    };
    let source = match std::fs::read_to_string(abs_path) {
        Ok(s) => s,
        Err(e) => return ParseOutcome::Skipped(format!("read_error: {e}")),
    };
    // Defense-in-depth: even if the dir walker let a large file slip (e.g.
    // size changed between lstat and read), refuse to feed it to tree-sitter.
    // source: H2 fix — per-file parse cap, 1 MB is sufficient for all real code.
    if (source.len() as u64) > super::MAX_PARSE_BYTES {
        return ParseOutcome::Skipped(format!(
            "oversized: {} bytes > MAX_PARSE_BYTES {}",
            source.len(),
            super::MAX_PARSE_BYTES
        ));
    }
    // Crash quarantine (issue #57): a parser panic (a grammar bug, a char-boundary
    // slice on a pathological error tree, …) must not abort the whole index.
    // `isolate` runs the parse under `catch_unwind`; a panic becomes a quarantined
    // file and the loop continues. The batch is untouched until after this point,
    // so a caught panic leaves no partial state. A tree-sitter timeout/None is a
    // clean `Err` (not a panic) → Skipped.
    let parsed = match isolate(|| parser::parse_file(&source, rel_path, lang)) {
        Ok(p) => p,
        Err(Isolated::Failed(e)) => return ParseOutcome::Skipped(format!("parse_failed: {e}")),
        Err(Isolated::Panicked) => return ParseOutcome::Quarantined("parser panicked".to_string()),
    };
    // Backfill the File node's parse-error count (inserted as 0 by the caller).
    // Best-effort: a store hiccup here must not fail the file (the count is a
    // convenience; the authoritative gap signal is the coverage sidecar).
    if parsed.parse_errors > 0 {
        let _ = set_file_parse_errors(store, rel_path, parsed.parse_errors);
    }
    nodes::accumulate_parsed_nodes(
        batch,
        &parsed.nodes,
        label_by_qn,
        seen_node_ids,
        lang.as_str(),
        restrict_to_public_api,
    );
    edges::accumulate_parsed_edges(batch, &parsed.refs, label_by_qn);
    // Full-AST persistence layer (additive, best-effort — see
    // `ast::persist_full_ast`'s doc comment for why a failure here never
    // changes this file's `ParseOutcome`).
    ast::persist_full_ast(store, rel_path, &source, lang);
    if parsed.parse_errors > 0 {
        ParseOutcome::Partial(parsed.error_ranges)
    } else {
        ParseOutcome::Indexed
    }
}

/// Terminal outcome of an isolated parse that did not yield a tree.
pub(super) enum Isolated {
    /// The parse returned `Err(String)` (e.g. tree-sitter timeout/None) — no panic.
    Failed(String),
    /// The parse PANICKED and the panic was caught. The exact reason is not
    /// recoverable across the unwind boundary; the caller supplies a stable label.
    Panicked,
}

/// Runs `f` under `catch_unwind`, mapping its result to `Ok(value)`,
/// `Err(Failed(msg))`, or `Err(Panicked)`. This is the crash-quarantine seam
/// (issue #57): the exact construct the indexer uses to keep one file's parser
/// panic from killing the whole run — extracted so it is unit-testable with a
/// deliberately panicking closure (AP's own grammars are robust enough that no
/// natural input panics them). `f` must be unwind-safe; the indexer's parse
/// closure captures only `&str`/`Copy` values, which are.
pub(super) fn isolate<T, F>(f: F) -> Result<T, Isolated>
where
    F: FnOnce() -> Result<T, String> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(msg)) => Err(Isolated::Failed(msg)),
        Err(_) => Err(Isolated::Panicked),
    }
}

/// Records the tree-sitter parse-error count on an already-inserted File node.
/// The File node is created eagerly (before the parse) with parse_errors = 0;
/// this flips it to the real count so a degraded parse is visible downstream.
fn set_file_parse_errors(store: &GraphStore, rel_path: &str, errors: u32) -> Result<(), String> {
    let cypher = format!(
        "MATCH (f:File) WHERE f.id = {} SET f.parse_errors = {}",
        cypher_str(rel_path),
        errors
    );
    store.execute_query(&cypher).map(|_| ())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{isolate, Isolated};

    #[test]
    fn isolate_returns_value_on_success() {
        let r: Result<u32, Isolated> = isolate(|| Ok(42u32));
        assert!(matches!(r, Ok(42)));
    }

    #[test]
    fn isolate_maps_err_to_failed() {
        let r: Result<u32, Isolated> = isolate(|| Err("boom".to_string()));
        assert!(matches!(r, Err(Isolated::Failed(m)) if m == "boom"));
    }

    #[test]
    fn isolate_catches_panic_as_quarantine() {
        // The crash-quarantine guarantee (issue #57): a panic inside the parse
        // closure is CAUGHT — it becomes `Panicked` and the caller keeps going,
        // instead of unwinding through the whole index. Silence the default panic
        // hook so the deliberate panic does not spam the test output.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r: Result<u32, Isolated> = isolate(|| panic!("simulated parser crash"));
        std::panic::set_hook(prev);
        assert!(matches!(r, Err(Isolated::Panicked)));
    }
}
