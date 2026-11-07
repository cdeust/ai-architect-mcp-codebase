//! Phase 1 of `analyze_codebase`: build the index stage of the graph, reusing
//! the previous graph incrementally when it is safe to (issue #443).
//!
//! # Why
//!
//! `analyze_codebase` was documented as incremental ("re-run after edits;
//! indexing is incremental by default") but removed the graph directory and
//! re-indexed every file on every call: 60 s then 60 s on ripgrep, 480 s then
//! 486 s on django, in the 2026-10-10 audit. `index_codebase` already had the
//! changed-files-only path (issue #62); this module routes analyze through it.
//!
//! # When the previous graph is reused
//!
//! All four must hold, otherwise the graph is rebuilt in full:
//! 1. the caller did not pass `full: true`;
//! 2. `<output_dir>/graph` exists and `file_manifest.json` loads;
//! 3. the manifest records the walk/parse settings the graph was built with,
//!    and they equal this call's (`IndexOptions::scope`). The per-file states
//!    cannot see a changed `language`, `dependency_scope` or `exclude_dirs`,
//!    which change which files belong in the graph at all, so analyze — whose
//!    contract never asked the caller to pass `full` after such a change —
//!    must not reuse a graph built under other settings;
//! 4. the incremental pass itself succeeds. A failure is logged and falls back
//!    to the full rebuild, as `index_codebase` does (§13 — no silent path).
//!
//! The later phases (resolve, LSP, cluster, search index) run over the whole
//! graph either way; each is written to re-run on an incrementally updated
//! graph (they reset their own prior rows first — see `resolver::resolve_graph`
//! and `clustering::cluster_graph`).

use super::AnalyzeRequest;
use crate::indexer;
use crate::indexer::coverage::{self, CoverageReport};
use crate::query_handlers::{remove_stale_graph_artifact, write_graph_meta};
use serde_json::{json, Value};
use std::path::PathBuf;

/// What phase 1 produced, whichever path it took.
pub(super) struct IndexPhase {
    pub(super) graph_path: PathBuf,
    /// The response's `index` block: `mode` plus that mode's counts.
    pub(super) summary: Value,
    /// The coverage report now on disk, for the LSP fold and the response.
    pub(super) coverage: CoverageReport,
    /// A best-effort sidecar (manifest or meta) write that failed.
    pub(super) sidecar_err: Option<String>,
}

/// Runs phase 1. The sidecars (coverage, manifest, `meta.json`) are on disk
/// when this returns `Ok`, `meta.json` last, on both paths.
pub(super) fn run(req: &AnalyzeRequest) -> Result<IndexPhase, String> {
    if let Some(phase) = try_incremental(req)? {
        return Ok(phase);
    }
    full(req)
}

/// The settings check (condition 3 in the module doc), on a loaded manifest.
fn reusable_manifest(req: &AnalyzeRequest) -> Option<indexer::manifest::FileManifest> {
    if req.want_full || !req.graph_dir.exists() {
        return None;
    }
    let prior = indexer::manifest::load(&indexer::manifest::manifest_path(&req.output_dir))?;
    if prior.scope.as_ref() != Some(&req.options.scope()) {
        eprintln!(
            "[ap] analyze_codebase: the graph was built with other or unrecorded \
             language/dependency_scope/exclude_dirs settings; rebuilding in full"
        );
        return None;
    }
    Some(prior)
}

/// `Ok(None)`: not applicable, or the incremental pass failed (logged).
/// `Err`: the pass succeeded but its coverage sidecar cannot be read back, so
/// no coverage receipt can be returned for it.
fn try_incremental(req: &AnalyzeRequest) -> Result<Option<IndexPhase>, String> {
    let Some(prior) = reusable_manifest(req) else {
        return Ok(None);
    };
    let manifest_path = indexer::manifest::manifest_path(&req.output_dir);
    let inc = match indexer::index_incremental(
        &req.codebase,
        &req.graph_dir,
        &manifest_path,
        &req.options,
        &prior,
    ) {
        Ok(inc) => inc,
        Err(e) => {
            eprintln!("[ap] analyze_codebase: incremental index failed ({e}); rebuilding in full");
            return Ok(None);
        }
    };
    let coverage = coverage::load(&coverage::coverage_path(&req.output_dir))
        .ok_or("incremental index finished but its coverage sidecar could not be read back")?;
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
        coverage,
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
