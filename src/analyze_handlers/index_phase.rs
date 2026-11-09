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
//! All five must hold, otherwise the graph is rebuilt in full:
//! 1. the caller did not pass `full: true`;
//! 2. `<output_dir>/graph` exists and `file_manifest.json` loads;
//! 3. the manifest records the server version and the walk/parse settings the
//!    graph was built with, and they equal this call's (`IndexOptions::scope`).
//!    The per-file states cannot see a changed `language`, `dependency_scope`
//!    or `exclude_dirs`, which change which files belong in the graph at all;
//! 4. no file changed, was added, deleted or renamed since that manifest
//!    (`indexer::tree_changed_since`);
//! 5. the incremental pass that refreshes the sidecars succeeds. A failure is
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
    if req.want_full || !req.graph_dir.exists() {
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

#[cfg(test)]
#[path = "index_phase_tests.rs"]
mod tests;
