//! Phase 1 of `analyze_codebase`: build the index stage of the graph, reusing
//! the previous graph when nothing it was built from has changed (issue #443).
//!
//! # Why
//!
//! `analyze_codebase` was documented as incremental ("re-run after edits;
//! indexing is incremental by default") but removed the graph directory and
//! re-indexed every file on every call: 62 s then 60 s on ripgrep, 480 s then
//! 486 s on django, in the 2026-10-10 audit.
//!
//! # When the previous graph is reused
//!
//! All of these must hold, otherwise the graph is rebuilt in full:
//! 1. the caller did not pass `full: true`;
//! 2. `<output_dir>/graph` exists and is not a symlink, `file_manifest.json`
//!    and the coverage sidecar both load, `meta.json` names the same codebase
//!    root, and neither this call nor the graph involves language-server rows
//!    (`lsp`);
//! 3. the manifest records the server version and the walk/parse settings the
//!    graph was built with, and they equal this call's (`IndexOptions::scope`).
//!    The per-file states cannot see a changed `language`, `dependency_scope`
//!    or `exclude_dirs`, which change which files belong in the graph at all;
//! 4. no file changed, was added, deleted or renamed since that manifest, and
//!    none was unreadable when it was written (`indexer::tree_changed_since`);
//! 5. the incremental pass that refreshes the sidecars succeeds and itself
//!    finds no change (a save in between rebuilds). A failure is
//!    logged and falls back to the full rebuild (§13 — no silent path).
//!
//! # Why an edited tree is still rebuilt in full
//!
//! Condition 4 is the resolver's limit, not the indexer's: `index_incremental`
//! re-parses only the changed files correctly, but `resolver::resolve_graph`
//! keeps the call edges an earlier run wrote and has no invalidation for an
//! UNCHANGED file whose candidate set another file's edit changed. Adding a
//! second `beta()` makes a call that resolved by unique match ambiguous; a
//! fresh analysis drops that edge, a reused graph kept it (Codex review of
//! #478, reproduced by `a_call_made_ambiguous_by_an_added_file_is_rebuilt`).
//! Reusing an edited graph waits for that invalidation (a follow-up of #443).
//!
//! The later phases (resolve, LSP, cluster, search index) run over the whole
//! graph either way.

use super::AnalyzeRequest;
use crate::indexer;
use crate::indexer::coverage::CoverageReport;
use crate::query_handlers::{remove_stale_graph_artifact, write_graph_meta};
use serde_json::{json, Value};
use std::path::PathBuf;

/// What phase 1 produced, whichever path it took.
pub(super) struct IndexPhase {
    pub(super) graph_path: PathBuf,
    /// The response's `index` block: `mode` plus that mode's counts.
    pub(super) summary: Value,
    /// The coverage report this call computed, for the LSP fold and the
    /// response; it is also the one on disk.
    pub(super) coverage: CoverageReport,
    /// A best-effort sidecar (manifest or meta) write that failed.
    pub(super) sidecar_err: Option<String>,
}

/// Runs phase 1. The sidecars (coverage, manifest, `meta.json`) are on disk
/// when this returns `Ok`, `meta.json` last, on both paths.
pub(super) fn run(req: &AnalyzeRequest) -> Result<IndexPhase, String> {
    if let Some(phase) = try_reuse(req)? {
        return Ok(phase);
    }
    full(req)
}

/// Conditions 1 to 4 of the module doc: the manifest to reuse, or `None`.
fn reusable_manifest(req: &AnalyzeRequest) -> Option<indexer::manifest::FileManifest> {
    if req.want_full || !is_own_graph(&req.graph_dir) {
        return None;
    }
    // The refresh carries the unchanged files' gaps forward from this report;
    // without it the reused graph would read as clean (Codex review of #478).
    if indexer::coverage::load(&indexer::coverage::coverage_path(&req.output_dir)).is_none() {
        eprintln!("[ap] analyze_codebase: no readable coverage sidecar; rebuilding in full");
        return None;
    }
    if !same_root(req) || req.enable_lsp || graph_has_lsp_rows(&req.graph_dir) {
        eprintln!(
            "[ap] analyze_codebase: another root, or language-server rows in play; rebuilding in full"
        );
        return None;
    }
    let prior = indexer::manifest::load(&indexer::manifest::manifest_path(&req.output_dir))?;
    if prior.scope.as_ref() != Some(&req.options.scope()) {
        eprintln!(
            "[ap] analyze_codebase: the graph was built by another version or with other \
             or unrecorded language/dependency_scope/exclude_dirs settings; rebuilding in full"
        );
        return None;
    }
    match indexer::tree_changed_since(&req.codebase, &req.options, &prior) {
        Ok(false) => Some(prior),
        Ok(true) => None,
        Err(e) => {
            eprintln!("[ap] analyze_codebase: change check failed ({e}); rebuilding in full");
            None
        }
    }
}

/// True when `graph_dir` is a real directory or file, not a symlink. A
/// symlinked graph is never reused: the later phases would rewrite the target,
/// whereas the full path's `remove_stale_graph_artifact` unlinks the link and
/// leaves its target alone (Codex review of #478).
fn is_own_graph(graph_dir: &std::path::Path) -> bool {
    std::fs::symlink_metadata(graph_dir).is_ok_and(|m| !m.file_type().is_symlink())
}

/// True when `meta.json` names this call's root. Another checkout whose files
/// all match by (mtime, size) — normalized mtimes, `cp -p` — would otherwise
/// pass the change check (review of #478). `write_graph_meta` records the root
/// exactly as this compares it.
fn same_root(req: &AnalyzeRequest) -> bool {
    std::fs::read_to_string(req.output_dir.join("meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|m| m["root"].as_str() == Some(&*req.codebase.to_string_lossy()))
}

/// True when the graph holds language-server rows, or cannot say. A fresh
/// analysis without `lsp` has none, and `resolve_graph` does not drop them, so
/// such a graph is not reused (review of #478). With `lsp: true` the caller
/// rebuilds too: the pass writes against the statically resolved graph.
fn graph_has_lsp_rows(graph_dir: &std::path::Path) -> bool {
    crate::graph_store::GraphStore::open_or_create(graph_dir)
        .and_then(|s| s.count_edges_by_method("lsp-definition"))
        .map_or(true, |n| n > 0)
}

/// `Ok(None)`: not applicable, or the refresh pass failed (logged).
/// `Err`: the pass ran but its coverage report did not reach the sidecar, so
/// the response could not stand behind any coverage receipt (same rule as the
/// full path, `persist_analyze_sidecars`).
fn try_reuse(req: &AnalyzeRequest) -> Result<Option<IndexPhase>, String> {
    let Some(prior) = reusable_manifest(req) else {
        return Ok(None);
    };
    let manifest_path = indexer::manifest::manifest_path(&req.output_dir);
    // With nothing changed this re-parses nothing; it refreshes the manifest
    // (moved mtimes), the coverage sidecar and the Cargo facts.
    let inc = match indexer::index_incremental(
        &req.codebase,
        &req.graph_dir,
        &manifest_path,
        &req.options,
        &prior,
    ) {
        Ok(inc) => inc,
        Err(e) => {
            eprintln!(
                "[ap] analyze_codebase: incremental refresh failed ({e}); rebuilding in full"
            );
            return Ok(None);
        }
    };
    // A file saved between the change check and this pass was re-parsed
    // (TOCTOU, review of #478): that graph is no longer one a reuse may keep.
    if inc.changed + inc.added + inc.deleted + inc.renamed > 0 {
        eprintln!("[ap] analyze_codebase: the tree changed during the refresh; rebuilding in full");
        return Ok(None);
    }
    if let Some(e) = inc.coverage_write_error {
        return Err(format!("coverage sidecar write failed: {e}"));
    }
    // The manifest was saved by the pass; `meta.json` lands last and names it.
    let sidecar_err = write_graph_meta(&req.output_dir, &req.codebase).err();
    Ok(Some(IndexPhase {
        summary: json!({
            "mode": "incremental",
            "changed": inc.changed,
            "added": inc.added,
            "deleted": inc.deleted,
            "renamed": inc.renamed,
            "unchanged": inc.unchanged,
            "files_reparsed": inc.files_reparsed,
            "elapsed_ms": inc.elapsed_ms,
        }),
        graph_path: inc.graph_path,
        coverage: inc.coverage,
        sidecar_err,
    }))
}

/// The from-scratch rebuild: remove whatever a prior run left, index, then
/// write the sidecars.
fn full(req: &AnalyzeRequest) -> Result<IndexPhase, String> {
    // The manifest and `meta.json` go first (review of #478): they vouch for
    // the graph about to be removed. A rebuild that fails or is killed before
    // `persist_analyze_sidecars` would otherwise leave the old manifest, and
    // its scope, beside a new or partial graph for the next call to trust.
    forget_sidecars(req)?;
    if req.graph_dir.exists() {
        // Prior run may have left a dir OR a single-file Kuzu db; remove either.
        remove_stale_graph_artifact(&req.graph_dir)?;
    }
    let result =
        indexer::index_codebase_with_language(&req.codebase, &req.graph_dir, &req.options)?;
    let sidecar_err = super::persist_analyze_sidecars(req, &result.coverage)?;
    Ok(IndexPhase {
        summary: json!({
            "mode": "full",
            "node_count": result.node_count,
            "edge_count": result.edge_count,
            "files_indexed": result.files_indexed,
        }),
        graph_path: result.graph_path,
        coverage: result.coverage,
        sidecar_err,
    })
}

/// Removes the sidecars that describe the previous graph. Absent is fine; a
/// file that cannot be removed fails the call, since it would still be read.
fn forget_sidecars(req: &AnalyzeRequest) -> Result<(), String> {
    for path in [
        indexer::manifest::manifest_path(&req.output_dir),
        req.output_dir.join("meta.json"),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("remove stale {}: {e}", path.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "index_phase_tests.rs"]
mod tests;
