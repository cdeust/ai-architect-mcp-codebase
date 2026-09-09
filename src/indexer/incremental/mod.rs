// incremental — changed-files-only re-indexing (issue #62).
//
// Layer: application/use-case within the indexer. This module owns the *policy*
// of "index only what changed": it classifies the current file tree against the
// persisted manifest (`super::manifest`), purges the graph nodes of the files
// that changed/vanished, re-parses only those files through the same
// walk→parse→persist machinery the full index uses, and re-derives the
// light-link edges out of them. It depends inward on `graph_store` (the store
// port), `parser`, and its sibling indexer submodules; nothing depends on it
// except the composition root (`do_index_codebase`) and its tests.
//
// Reference: DeusData/codebase-memory-mcp `src/pipeline/pipeline_incremental.c`.
// Their design, adapted to AP's store and improved:
//   * Classify by (mtime, size) against a stored per-file manifest — added /
//     modified / deleted / unchanged (their `classify_files` +
//     `find_deleted_files`). AP mirrors this on the hot path.
//   * The C reference purges a changed file's nodes INCLUDING its file node,
//     then snapshots inbound cross-file edges before the purge so cross-file
//     references survive (their `incr_capture_inbound_edge`). AP improves on
//     this for the common case: a *modified* file keeps its File node (its id —
//     the repo-relative path — is stable across an edit), so every inbound
//     File→File / Dir→File edge survives structurally with no snapshot needed.
//     Only the file's *symbols* are purged and re-parsed. AP still snapshots
//     inbound cross-file edges into a modified file's *symbols* (the case the C
//     reference targets) so a resolved graph's cross-file edges are preserved.
//   * Rename detection: the C reference has NONE (a rename is delete+add). AP's
//     documented improvement — the manifest stores a content hash per file, so
//     a (deleted, added) pair with an identical hash is reported as a rename.
//     The store makes an in-place primary-key rewrite unavailable (lbug/Kuzu
//     forbid SET on a PK), so a rename is executed as purge-old + parse-new
//     (equivalent to a full index of the renamed tree) but REPORTED as a
//     rename in the response counts.
//
// Submodules (issue #288 split — file was 3x the §4.1 500-line cap):
//   * `classify`  — discovery + manifest-diff classification (added/changed/
//     deleted/renamed) against the prior manifest.
//   * `coverage`  — the coverage-sidecar merge path: carry-forward of
//     unchanged files' gaps, overlay of fresh reparse gaps, and the
//     outside-build-targets recompute (issue #284).
//   * `bootstrap` — the artifact-bootstrap fill (issue #55): git-diff or
//     manifest-hash classification against an imported artifact's baseline.
//   * `mutate`    — the graph-mutation primitives: purge (symbols/File node/
//     FileContent/light-links), orphan-directory pruning, and re-parse of a
//     modified or new file.
//   * `edges`     — the inbound cross-file edge snapshot/re-link pair that
//     keeps a resolved graph's edges into a changed file from being orphaned
//     by its purge.
// This file keeps the two entry points (`index_incremental`,
// `write_full_manifest`) and `apply_changes`, the shared core both
// `index_incremental` and `bootstrap::fill_after_bootstrap` call.

use crate::graph_store::GraphStore;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::coverage::FileCoverage;
use super::manifest::{self, FileManifest, FileState};
use super::persist::ParseOutcome;
use super::walk::{DependencyScope, WalkOptions};
use super::{light_link, IncrementalResult, IndexOptions};
use std::collections::BTreeMap;

mod bootstrap;
mod classify;
mod coverage;
mod edges;
mod mutate;

pub use bootstrap::{fill_after_bootstrap, FillMethod, FillResult};
use classify::{classify, discover};
use coverage::save_incremental_coverage;
use edges::{relink_inbound_edges, snapshot_inbound_edges};
use mutate::{
    existing_directory_ids, prune_orphan_directories, purge_file_content, purge_file_node,
    purge_file_symbols, purge_outbound_light_links, reparse_modified_file, reparse_new_file,
};

// Every symbol the parser emits carries a file-scoped qualified-name id
// (`<rel_path>::…`): the top-level extraction scope is the file path and nested
// scopes recurse through it, while CallSite/Import/Module ids are all
// `caller_qn::…` or `qual(scope, …)` — verified across the language extractors.
// So `starts_with(id, "<rel>::")` selects exactly one file's symbols across
// every symbol label (Module, Function, Method, Struct, Enum, Variant, Trait,
// Field, Constant, TypeAlias, Import, CallSite). `File` and `Directory` are
// keyed by the bare path (no `::`), so the same predicate never matches them —
// which is why the purge can run label-agnostically (see `mutate::purge_file_symbols`).

/// A file discovered on disk during the incremental scan, with the cheap change
/// signal (mtime, size) already read.
#[derive(Debug, Clone)]
pub(super) struct Discovered {
    /// Repo-relative, forward-slash id — identical to the File node's `id`.
    pub(super) rel: String,
    pub(super) abs: PathBuf,
    pub(super) mtime_ns: i64,
    pub(super) size: u64,
}

/// A rename: the old file's nodes are purged, the new file is parsed fresh.
/// The pair is keyed on an identical content hash at detection time; the hash
/// itself is not retained past classification.
#[derive(Debug, Clone)]
pub(super) struct Rename {
    pub(super) old_rel: String,
    pub(super) new_file: Discovered,
}

/// The set of files to act on, independent of HOW they were classified
/// (manifest diff or git diff). This is exactly what `apply_changes` consumes.
#[derive(Debug, Default)]
pub(super) struct ChangeSet {
    /// Modified files: File node kept, symbols re-parsed.
    pub(super) changed: Vec<Discovered>,
    /// New files: inserted from scratch.
    pub(super) added: Vec<Discovered>,
    /// Files gone from disk: File node + symbols purged.
    pub(super) deleted: Vec<String>,
    /// Rename pairs (old purged, new parsed), reported distinctly.
    pub(super) renamed: Vec<Rename>,
}

/// The classification of the current tree against the prior manifest: the
/// `ChangeSet` to apply, plus the unchanged set (for the count) and the manifest
/// to persist after the pass succeeds.
pub(super) struct Plan {
    pub(super) change_set: ChangeSet,
    /// Files whose (mtime, size) — or content hash — is unchanged: no work.
    pub(super) unchanged: Vec<Discovered>,
    /// The manifest for the current tree, written on success. Carries every
    /// current file's next state (hashes reused for unchanged files).
    pub(super) next_manifest: FileManifest,
}

/// Runs a changed-files-only re-index of `codebase` into the existing graph at
/// `graph_dir`, using `prior` (the loaded manifest) as the change baseline.
///
/// Preconditions: `graph_dir` is an existing, openable graph previously built by
/// a full index of (an ancestor state of) `codebase`; `manifest_path` is where
/// the refreshed manifest is written; `prior` is the manifest that graph was
/// left with. Postconditions on `Ok`: the graph equals what a full index of the
/// current `codebase` tree would produce at the index stage (File/Directory +
/// symbol nodes, structural + light-link edges), the manifest at
/// `manifest_path` reflects the current tree, and the returned counts sum the
/// files by class. Invariant preserved across the pass: for every unchanged
/// file, none of its nodes or edges are touched (asserted by the integration
/// test's per-file count check).
pub fn index_incremental(
    codebase: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    options: &IndexOptions,
    prior: &FileManifest,
) -> Result<IncrementalResult, String> {
    let start = Instant::now();
    let store = GraphStore::open_or_create(graph_dir)?;
    store.require_entry_metadata()?;
    // No create_schema() here: the graph already exists (this path is reached
    // only when a prior full index built it with the current schema), and the
    // DDL pass is ~0.4s of pure fixed cost that would defeat the whole point of
    // an incremental re-index. A schema mismatch is instead the caller's cue to
    // pass `full: true` (documented on the tool). source: measured — skipping
    // the redundant CREATE TABLE IF NOT EXISTS pass is the single largest
    // incremental speedup on small change sets.

    let dependency_scope = options.dependency_scope;
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, walk_gaps) = discover(codebase, walk_opts)?;
    let plan = classify(prior, &current);

    let (reparsed, reparsed_gaps) = apply_changes(
        &store,
        codebase,
        &plan.change_set,
        &current,
        dependency_scope,
    )?;

    // Persist the refreshed manifest (built by classify, hashes reused).
    manifest::save(manifest_path, &plan.next_manifest)?;

    // Coverage (issue #57/#249): carry forward unchanged files' gaps, overlay
    // the freshly-reparsed files' gaps (clearing any that are now clean), plus
    // the walk-level excluded/unreadable directories discovered this pass,
    // save.
    let mut merged_gaps = reparsed_gaps;
    merged_gaps.extend(walk_gaps);
    save_incremental_coverage(
        codebase,
        graph_dir,
        &current,
        &plan.change_set,
        merged_gaps,
        "incremental",
    );

    // Intentionally NO node_count()/edge_count() here — see `IncrementalResult`.
    Ok(IncrementalResult {
        graph_path: graph_dir.to_path_buf(),
        changed: plan.change_set.changed.len() as u64,
        added: plan.change_set.added.len() as u64,
        deleted: plan.change_set.deleted.len() as u64,
        renamed: plan.change_set.renamed.len() as u64,
        unchanged: plan.unchanged.len() as u64,
        files_reparsed: reparsed,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

/// Applies a classified `ChangeSet` to the open graph: snapshot inbound
/// cross-file edges into modified files, purge the changed/deleted/renamed-old
/// nodes, re-parse the changed/added/renamed-new files, prune orphaned
/// directories, re-derive light-link edges out of the re-parsed files, and
/// re-link the snapshot. Returns the number of files re-parsed
/// (changed + added + renamed-new).
///
/// This is the shared core of both incremental entry points: the local
/// manifest-driven re-index (`index_incremental`) and the artifact bootstrap
/// fill (`bootstrap::fill_after_bootstrap`), which differ only in HOW they
/// classify. `all_current` is the full current file discovery, used so a
/// re-parsed file's light links resolve against every File node that exists,
/// not just the changed ones.
///
/// Postcondition: the graph equals what a full index of the current tree would
/// produce at the index stage, given a correct `changes` classification.
/// Invariant: unchanged files' nodes and edges are never touched.
pub(super) fn apply_changes(
    store: &GraphStore,
    codebase: &Path,
    changes: &ChangeSet,
    all_current: &[Discovered],
    dependency_scope: DependencyScope,
) -> Result<(u64, BTreeMap<String, FileCoverage>), String> {
    // Coverage gaps for the reparsed files only (issue #57). The caller merges
    // this with the carried-forward coverage of unchanged files.
    let mut collector = super::coverage::CoverageCollector::default();
    // The set of files whose nodes are being purged-and-reparsed. Used both to
    // scope the inbound-edge snapshot (source must be OUTSIDE this set) and to
    // re-run light-linking only for these sources.
    let mut reparsed_set: HashSet<String> = HashSet::new();
    for d in &changes.changed {
        reparsed_set.insert(d.rel.clone());
    }
    for d in &changes.added {
        reparsed_set.insert(d.rel.clone());
    }
    for r in &changes.renamed {
        reparsed_set.insert(r.new_file.rel.clone());
    }

    // ---- 1. Snapshot inbound cross-file edges into MODIFIED files' symbols --
    // (Deleted/renamed-old files are NOT snapshotted: their inbound edges must
    // die, exactly as a full re-index would drop an edge to a vanished target.)
    let changed_rels: Vec<&str> = changes.changed.iter().map(|d| d.rel.as_str()).collect();
    let saved_edges = snapshot_inbound_edges(store, &changed_rels, &reparsed_set)?;

    // ---- 2. Purge -----------------------------------------------------------
    for d in &changes.changed {
        purge_file_symbols(store, &d.rel)?;
        purge_outbound_light_links(store, &d.rel)?;
        purge_file_content(store, &d.rel)?;
    }
    for rel in &changes.deleted {
        purge_file_symbols(store, rel)?;
        purge_file_node(store, rel)?;
        purge_file_content(store, rel)?;
    }
    for r in &changes.renamed {
        purge_file_symbols(store, &r.old_rel)?;
        purge_file_node(store, &r.old_rel)?;
        purge_file_content(store, &r.old_rel)?;
    }

    // ---- 3. Re-parse changed/added/renamed-new files ------------------------
    // Seed the "already inserted" directory set with the Directory nodes the
    // graph already holds, so inserting a NEW file under an existing directory
    // does not try to re-CREATE that directory (a primary-key violation). Only
    // genuinely new directories are created, matching a full index.
    let mut dir_nodes_inserted: HashSet<PathBuf> = existing_directory_ids(store)?;
    for d in &changes.changed {
        let outcome = reparse_modified_file(store, codebase, d, dependency_scope)?;
        record_reparse_outcome(&mut collector, &d.rel, outcome);
    }
    for d in &changes.added {
        let outcome = reparse_new_file(
            store,
            codebase,
            d,
            dependency_scope,
            &mut dir_nodes_inserted,
        )?;
        record_reparse_outcome(&mut collector, &d.rel, outcome);
    }
    for r in &changes.renamed {
        let outcome = reparse_new_file(
            store,
            codebase,
            &r.new_file,
            dependency_scope,
            &mut dir_nodes_inserted,
        )?;
        record_reparse_outcome(&mut collector, &r.new_file.rel, outcome);
    }

    // A deletion or rename can empty a directory. A full index never creates a
    // Directory node with no descendant file, so prune any that were orphaned to
    // keep the graph identical to a from-scratch index of the current tree.
    if !changes.deleted.is_empty() || !changes.renamed.is_empty() {
        prune_orphan_directories(store)?;
    }

    // ---- 4. Re-derive light-link edges OUT of the re-parsed files -----------
    // Resolve against the full current file set so a changed file can still link
    // to an unchanged target's File node.
    let all_files: Vec<PathBuf> = all_current.iter().map(|d| d.abs.clone()).collect();
    let reparsed_files: Vec<PathBuf> = all_current
        .iter()
        .filter(|d| reparsed_set.contains(&d.rel))
        .map(|d| d.abs.clone())
        .collect();
    if !reparsed_files.is_empty() {
        match light_link::link_file_imports_for(store, codebase, &reparsed_files, &all_files) {
            Ok(_) => {}
            Err(e) => eprintln!("incremental: light-link pass skipped: {e}"),
        }
    }

    // ---- 4b. Re-derive IaC nodes/edges OUT of the re-parsed files -----------
    // (issue #63 incremental integration). The changed files' stale `<rel>::`
    // IaC nodes were already reclaimed by `purge_file_symbols` above (they share
    // the symbol id-prefix), so this re-emits from the current text and resolves
    // references against every File node — exactly like the light-link step. IaC
    // reference edges target stable File nodes, so an unchanged referencer's edge
    // into a reparsed file is not disturbed. Parse gaps fold into the collector
    // so the coverage sidecar carries the IaC honesty signal too.
    if !reparsed_files.is_empty() {
        match super::iac::run_iac_pass_for(store, codebase, &reparsed_files, &all_files) {
            Ok(gaps) => super::fold_iac_gaps(&mut collector, gaps),
            Err(e) => eprintln!("incremental: IaC pass skipped: {e}"),
        }
    }

    // ---- 5. Re-link the snapshotted inbound cross-file edges -----------------
    relink_inbound_edges(store, &saved_edges)?;

    Ok((reparsed_set.len() as u64, collector.into_files()))
}

/// Records one reparsed file's outcome into the coverage collector, logging the
/// non-clean cases. Unlike the full-index recorder, `files_indexed` is not
/// tracked here (the incremental caller counts the whole current tree).
fn record_reparse_outcome(
    collector: &mut super::coverage::CoverageCollector,
    rel: &str,
    outcome: ParseOutcome,
) {
    match outcome {
        ParseOutcome::Indexed => {}
        ParseOutcome::Partial(ranges) => collector.record_partial(rel, ranges),
        ParseOutcome::Skipped(reason) => {
            eprintln!("incremental: skipping {rel}: {reason}");
            collector.record_skipped(rel, reason);
        }
        ParseOutcome::Quarantined(reason) => {
            eprintln!("incremental: QUARANTINED {rel}: {reason} (isolated; index continues)");
            collector.record_quarantined(rel, reason);
        }
    }
}

/// Builds the manifest for a freshly full-indexed `codebase` and writes it to
/// `manifest_path`, so the NEXT `index_codebase` call can run incrementally.
///
/// Preconditions: `codebase` is the tree just indexed with the same
/// `options` (`language_filter`/`dependency_scope`/`exclude_dirs`);
/// `manifest_path`'s parent exists. Postcondition on `Ok`: the manifest
/// records (mtime, size, content_hash) for every file the full index visited,
/// keyed by the File node id. Best-effort per file: a stat/read failure
/// records a zeroed/empty state rather than aborting the whole index (the
/// file simply re-classifies as changed next time).
///
/// Walk-level gaps (excluded/unreadable directories, issue #249) are
/// discarded here — the caller (a just-completed full index) already wrote
/// the coverage sidecar from its own walk; this re-walk exists only to build
/// the manifest baseline for the NEXT incremental call.
pub fn write_full_manifest(
    codebase: &Path,
    manifest_path: &Path,
    options: &IndexOptions,
) -> Result<(), String> {
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope: options.dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, _walk_gaps) = discover(codebase, walk_opts)?;
    let mut m = FileManifest::new();
    for d in &current {
        m.files.insert(
            d.rel.clone(),
            FileState {
                mtime_ns: d.mtime_ns,
                size: d.size,
                content_hash: manifest::hash_file(&d.abs).unwrap_or_default(),
            },
        );
    }
    manifest::save(manifest_path, &m)
}
