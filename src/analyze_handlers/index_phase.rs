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
//! 4. the manifest records no file it could not read (an empty hash):
//!    `chmod +r` moves no mtime, so the refresh would carry such a file
//!    forward unparsed, and the full rebuild retries it;
//! 5. the incremental pass, which re-parses the files changed, added, deleted
//!    or renamed since that manifest, succeeds. A failed pass is logged and
//!    falls back to the full rebuild (§13 — no silent path); a failed
//!    coverage-sidecar write fails the call;
//! 6. that pass keeps the manifest's scope (`IncrementalResult::scope_kept`):
//!    it computed the same Cargo map (`FileManifest::cargo_facts`) as the pass
//!    that wrote the manifest, and built its coverage report on the previous
//!    one. The map is read from manifests the walk may not list — `Cargo.toml`
//!    under `language: "rust"`, a path dependency outside the tree — so the
//!    per-file states cannot see it move. Nor does it keep the scope when
//!    the IaC references of a file it did not re-parse may have moved
//!    (`indexer::incremental::cross_links`, Codex review of #523).
//!
//! # Why an edited tree is reused
//!
//! Until the resolver could invalidate its own rows, an edited tree was
//! rebuilt in full: `resolver::resolve_graph` kept the rows an earlier run
//! wrote, so a call an added second `beta()` made ambiguous kept its
//! unique-match edge (Codex review of #478). Every pass now retracts the
//! static call rows (`resolver::retract`, #496) and the import, use,
//! implements and extends rows (`resolver::retract_edges`, #513) it does not
//! decide again, and reopens the sites and imports they alone resolved, so
//! resolving the refreshed graph gives the rows a fresh analysis gives.
//! The refresh also draws again the light links of unchanged files when the
//! file set moves, which resolve against it (`cross_links`).
//! `index_phase_change_tests.rs` and `index_phase_cross_link_tests.rs` check
//! every edit kind against a fresh analysis of the same tree.
//!
//! The later phases (resolve, LSP, cluster, search index) run over the whole
//! graph either way.
//!
//! # What a reused graph keeps that a fresh analysis lacks
//!
//! Rows other tools add to the graph and analyze never writes: the co-change
//! edges `index_codebase` mines (`FILE_CHANGES_WITH`) and the rows
//! `ingest_traces` records. A full analysis removes them with the graph; a
//! reuse keeps them, so after `analyze` → `index_codebase` → `analyze` on an
//! unchanged tree `get_impact` can read co-change edges a fresh analysis
//! would not have (Claude review of #478). They are extra data, not a wrong
//! resolution, so they do not block reuse; `full: true` drops them.

use super::AnalyzeRequest;
use crate::indexer;
use crate::indexer::coverage::CoverageReport;
use crate::query_handlers::{prepare_full_rebuild, write_graph_meta};
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

/// Runs phase 1. The coverage sidecar is on disk when this returns `Ok`, and
/// `meta.json` is written last, best-effort, on both paths. On the full path
/// the manifest is best-effort too (`sidecar_err` says why one is missing); on
/// reuse a manifest that cannot be saved fails the refresh, which falls back
/// to the full path.
pub(super) fn run(req: &AnalyzeRequest) -> Result<IndexPhase, String> {
    if let Some(phase) = try_reuse(req)? {
        return Ok(phase);
    }
    full(req)
}

/// Conditions 1 to 4 of the module doc: the manifest to reuse, or `None`.
/// The checks that need no I/O or read a small sidecar come first, and only
/// then the one that opens the graph, so a re-scoped or `lsp: true` call
/// never opens it (Claude review of #478).
fn reusable_manifest(req: &AnalyzeRequest) -> Option<indexer::manifest::FileManifest> {
    if req.want_full || !is_own_graph(&req.graph_dir) {
        return None;
    }
    if req.enable_lsp {
        // The pass writes against the statically resolved graph.
        eprintln!("[ap] analyze_codebase: lsp=true; rebuilding in full");
        return None;
    }
    if !same_root(req) {
        eprintln!(
            "[ap] analyze_codebase: the graph was built for another root; rebuilding in full"
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
    // The refresh carries the unchanged files' gaps forward from this report;
    // without it the reused graph would read as clean (Codex review of #478).
    if indexer::coverage::load(&indexer::coverage::coverage_path(&req.output_dir)).is_none() {
        eprintln!("[ap] analyze_codebase: no readable coverage sidecar; rebuilding in full");
        return None;
    }
    if prior.files.values().any(|f| f.content_hash.is_empty()) {
        eprintln!(
            "[ap] analyze_codebase: the manifest records an unreadable file; rebuilding in full"
        );
        return None;
    }
    if graph_has_lsp_rows(&req.graph_dir) {
        eprintln!(
            "[ap] analyze_codebase: the graph holds language-server rows; rebuilding in full"
        );
        return None;
    }
    Some(prior)
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
/// read as unchanged to the refresh (review of #478). `write_graph_meta`
/// records the root exactly as this compares it.
fn same_root(req: &AnalyzeRequest) -> bool {
    // A regular file of a sane size only (Codex security review of #478): a
    // FIFO here would block, and a link to `/dev/zero` would never end.
    indexer::manifest::read_regular_capped(&req.output_dir.join("meta.json"), META_MAX_BYTES)
        .and_then(|(bytes, _)| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|m| m["root"].as_str() == Some(&*req.codebase.to_string_lossy()))
}

/// `meta.json` holds a root path, a commit sha and the manifest's identity: a
/// few kilobytes. 1 MiB is a fail-safe bound; over it the graph is rebuilt.
const META_MAX_BYTES: u64 = 1 << 20;

/// True when the graph holds language-server rows, or cannot say. A fresh
/// analysis without `lsp` has none, and `resolve_graph` does not drop them, so
/// such a graph is not reused (review of #478). With `lsp: true` the caller
/// rebuilds too: the pass writes against the statically resolved graph.
fn graph_has_lsp_rows(graph_dir: &std::path::Path) -> bool {
    crate::graph_store::GraphStore::open_or_create(graph_dir)
        .and_then(|s| s.count_edges_by_method("lsp-definition"))
        .map_or(true, |n| n > 0)
}

/// `Ok(None)`: not applicable, the refresh pass failed, or it dropped the
/// scope (each logged).
/// `Err`: the pass ran but its coverage report did not reach the sidecar, so
/// the response could not stand behind any coverage receipt (same rule as the
/// full path, `persist_analyze_sidecars`).
fn try_reuse(req: &AnalyzeRequest) -> Result<Option<IndexPhase>, String> {
    let Some(prior) = reusable_manifest(req) else {
        return Ok(None);
    };
    let manifest_path = indexer::manifest::manifest_path(&req.output_dir);
    // Re-parses the changed, added and renamed files, purges the deleted
    // ones, and refreshes the manifest, the coverage sidecar and the Cargo
    // facts; with nothing changed it re-parses nothing.
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
    // The pass's own verdict on whether its manifest still vouches for the
    // graph (Claude review of #478): a different or unrecorded Cargo map, or a
    // report built without the previous one, for instance because the sidecar
    // vanished after `reusable_manifest` loaded it. The Cargo map comes from
    // manifests the walk may not list (`Cargo.toml` under `language: "rust"`),
    // and `resolve_graph` keeps the rows an earlier map accepted.
    if !inc.scope_kept {
        eprintln!(
            "[ap] analyze_codebase: the refresh could not vouch for the graph; rebuilding in full"
        );
        return Ok(None);
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
    // Snapshot, release, forget the old sidecars, remove the old graph — in
    // that order (`prepare_full_rebuild`): a rebuild that fails or is killed
    // leaves no manifest vouching for a new or partial graph, and the snapshot
    // saved after the index makes a file saved during it read as changed.
    let snapshot =
        prepare_full_rebuild(&req.codebase, &req.output_dir, &req.graph_dir, &req.options)?;
    let result =
        indexer::index_codebase_with_language(&req.codebase, &req.graph_dir, &req.options)?;
    let snapshot = snapshot.with_cargo_facts(&result.cargo_facts_digest);
    let sidecar_err = super::persist_analyze_sidecars(req, &result.coverage, snapshot)?;
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

#[cfg(test)]
#[path = "index_phase_rebuild_tests.rs"]
mod rebuild_tests;

#[cfg(test)]
#[path = "index_phase_change_tests.rs"]
mod change_tests;

#[cfg(test)]
#[path = "index_phase_cross_link_tests.rs"]
mod cross_link_tests;
