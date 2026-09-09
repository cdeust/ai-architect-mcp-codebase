// mutate — graph-mutation primitives: purge and re-parse (issue #288 split).
//
// The purge half deletes a changed/deleted/renamed-old file's nodes/edges;
// the re-parse half re-inserts a modified or new file's symbols. Both halves
// are invoked from `apply_changes` (parent module), which sequences purge
// before re-parse per file class.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::graph_store::{cypher_str, GraphStore};

use super::super::persist::{self, ParseOutcome};
use super::super::walk::{is_dependency_path, DependencyScope};
use super::super::{relative_path, SymbolBatch};
use super::Discovered;

/// The cross-file link tables re-derived from a modified file's own text on
/// re-parse. Their OUTBOUND edges from a changed file are purged and rebuilt;
/// their INBOUND edges into a changed file survive because the File node is kept.
const LIGHT_LINK_TABLES: &[&str] = &["Imports_File_File", "References_File_File"];

// ---------------------------------------------------------------------------
// Graph mutation — purge
// ---------------------------------------------------------------------------

/// Deletes every symbol node of `rel` (id prefixed `"<rel>::"`), cascading their
/// edges, in a single label-agnostic pass.
///
/// A file-scoped symbol is exactly a node whose id begins with `"<rel>::"`; the
/// File node's id is the bare `"<rel>"` (no `::`) and Directory/Community/…/
/// Version ids never carry a `"<rel>::"` prefix, so this deletes precisely the
/// file's symbols and nothing else. Uses one unlabeled `MATCH (n) … DETACH
/// DELETE n` (verified supported by lbug 0.15) instead of one query per label —
/// ~12× fewer round-trips per changed file. source: `SYMBOL_LABELS` documents
/// which labels this covers; the unlabeled scan is the measured-fast equivalent.
pub(super) fn purge_file_symbols(store: &GraphStore, rel: &str) -> Result<(), String> {
    let prefix = cypher_str(&format!("{rel}::"));
    let cypher = format!("MATCH (n) WHERE starts_with(n.id, {prefix}) DETACH DELETE n");
    store.execute_query(&cypher)?;
    Ok(())
}

/// Deletes the File node for `rel` and every edge touching it. Used for deleted
/// and renamed-old files only (a modified file keeps its File node).
pub(super) fn purge_file_node(store: &GraphStore, rel: &str) -> Result<(), String> {
    let id = cypher_str(rel);
    let cypher = format!("MATCH (f:File) WHERE f.id = {id} DETACH DELETE f");
    store.execute_query(&cypher)?;
    Ok(())
}

/// Deletes the `FileContent` row for `rel`, when one exists.
///
/// `FileContent.id` equals the File node's bare `"<rel>"` id (by design —
/// `MATCH (fc:FileContent {id: file_id})` is a direct lookup), which is
/// exactly the shape `purge_file_symbols`'s `"<rel>::"`-PREFIX scan does NOT
/// cover (same reason `purge_file_node` needs its own exact-id delete rather
/// than reusing the prefix scan). Root cause of a real bug (found via CI,
/// not by inspection): a MODIFIED file keeps its File node across a reparse,
/// so without this call its stale FileContent row (from the file's PREVIOUS
/// content) was never purged; `persist_full_ast`'s
/// `insert_file_content` then hit lbug's primary-key uniqueness constraint
/// on the re-insert, the error was logged and swallowed (best-effort, by
/// design — a FileContent failure must not fail the file's indexing), and
/// the file was silently left with STALE content and a missing fresh
/// FileContent node — an incremental-fill graph that diverged from a
/// from-scratch full index by exactly one node per re-touched file.
/// Called for every purge site `purge_file_symbols`/`purge_file_node`
/// already runs (changed/deleted/renamed-old): safe as a no-op when no
/// FileContent row exists yet (added files, or a graph indexed before this
/// layer existed).
pub(super) fn purge_file_content(store: &GraphStore, rel: &str) -> Result<(), String> {
    let id = cypher_str(rel);
    let cypher = format!("MATCH (fc:FileContent) WHERE fc.id = {id} DETACH DELETE fc");
    store.execute_query(&cypher)?;
    Ok(())
}

/// Deletes the light-link edges OUT of `rel` so the re-parse can re-derive them
/// from the file's current text (an import removed by the edit must disappear;
/// one added must appear). Inbound light-link edges are untouched — the File
/// node survives, so edges from unchanged files into `rel` are preserved.
pub(super) fn purge_outbound_light_links(store: &GraphStore, rel: &str) -> Result<(), String> {
    let id = cypher_str(rel);
    for table in LIGHT_LINK_TABLES {
        let cypher = format!("MATCH (a:File)-[r:{table}]->(:File) WHERE a.id = {id} DELETE r");
        store.execute_query(&cypher)?;
    }
    Ok(())
}

/// The ids of every Directory node currently in the graph, as `PathBuf`s keyed
/// the same way `insert_ancestor_dirs` tracks them (the relative dir path).
pub(super) fn existing_directory_ids(store: &GraphStore) -> Result<HashSet<PathBuf>, String> {
    let qr = store.execute_query("MATCH (d:Directory) RETURN d.id")?;
    Ok(qr
        .rows
        .into_iter()
        .filter_map(|row| row.into_iter().next())
        .map(PathBuf::from)
        .collect())
}

/// Deletes Directory nodes left with no children (no `Contains_Dir_File` and no
/// `Contains_Dir_Dir` out-edges) after a purge, to a fixpoint — deleting a leaf
/// directory can orphan its parent. A Directory's only out-edges are the two
/// containment kinds, so "zero out-edges" is exactly "childless". Bounded by
/// `MAX_DEPTH` iterations (the walker's own directory-depth cap), so a
/// pathological tree cannot loop unbounded.
pub(super) fn prune_orphan_directories(store: &GraphStore) -> Result<(), String> {
    for _ in 0..super::super::MAX_DEPTH {
        let orphans = store.execute_query(
            "MATCH (d:Directory) OPTIONAL MATCH (d)-[e]->() \
             WITH d, count(e) AS c WHERE c = 0 RETURN d.id",
        )?;
        if orphans.rows.is_empty() {
            return Ok(());
        }
        for row in orphans.rows {
            if let Some(id) = row.into_iter().next() {
                let cypher = format!(
                    "MATCH (d:Directory) WHERE d.id = {} DETACH DELETE d",
                    cypher_str(&id)
                );
                store.execute_query(&cypher)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Graph mutation — re-parse
// ---------------------------------------------------------------------------

/// Re-parses a MODIFIED file into the existing graph. The File node is kept
/// (its id is stable); its symbols were already purged. Resets the File node's
/// mutable columns (size, parse_errors) then re-inserts the parsed symbols and
/// intra-file edges. Postcondition: the file's symbol subgraph equals a fresh
/// parse; inbound File-targeted edges are untouched throughout.
pub(super) fn reparse_modified_file(
    store: &GraphStore,
    codebase: &Path,
    d: &Discovered,
    dependency_scope: DependencyScope,
) -> Result<ParseOutcome, String> {
    // Reset the kept File node's mutable state. index_single_file re-bumps
    // parse_errors only when >0, so we clear it first (a fixed parse must drop
    // back to 0), and refresh size_bytes to the new length.
    let id = cypher_str(&d.rel);
    let reset = format!(
        "MATCH (f:File) WHERE f.id = {id} SET f.parse_errors = 0, f.size_bytes = {}",
        d.size
    );
    store.execute_query(&reset)?;

    let mut batch = SymbolBatch::default();
    let mut label_by_qn: HashMap<String, HashSet<String>> = HashMap::new();
    label_by_qn
        .entry(d.rel.clone())
        .or_default()
        .insert("File".into());
    let mut seen_node_ids: HashSet<(String, String)> = HashSet::new();
    let restrict =
        dependency_scope == DependencyScope::PublicApi && is_dependency_path(codebase, &d.abs);
    let outcome = persist::index_single_file(
        store,
        &mut batch,
        &d.abs,
        &d.rel,
        &mut label_by_qn,
        &mut seen_node_ids,
        restrict,
    );
    batch.flush(store)?;
    Ok(outcome)
}

/// Indexes a NEW (or renamed-new) file from scratch: ancestor Directory nodes,
/// the File node, the Dir→File containment edge, then the parsed symbols and
/// intra-file edges — exactly the per-file body of the full index.
pub(super) fn reparse_new_file(
    store: &GraphStore,
    codebase: &Path,
    d: &Discovered,
    dependency_scope: DependencyScope,
    dir_nodes_inserted: &mut HashSet<PathBuf>,
) -> Result<ParseOutcome, String> {
    let mut batch = SymbolBatch::default();
    let mut label_by_qn: HashMap<String, HashSet<String>> = HashMap::new();
    let mut seen_node_ids: HashSet<(String, String)> = HashSet::new();

    persist::insert_ancestor_dirs(
        store,
        &mut batch,
        codebase,
        &d.abs,
        dir_nodes_inserted,
        &mut label_by_qn,
    )?;
    persist::insert_file_node(store, &d.abs, &d.rel)?;
    label_by_qn
        .entry(d.rel.clone())
        .or_default()
        .insert("File".into());
    let rel_path = relative_path(codebase, &d.abs);
    persist::insert_dir_file_edge(&mut batch, &rel_path);

    let restrict =
        dependency_scope == DependencyScope::PublicApi && is_dependency_path(codebase, &d.abs);
    let outcome = persist::index_single_file(
        store,
        &mut batch,
        &d.abs,
        &d.rel,
        &mut label_by_qn,
        &mut seen_node_ids,
        restrict,
    );
    batch.flush(store)?;
    Ok(outcome)
}
