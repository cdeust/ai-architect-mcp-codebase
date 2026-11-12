// indexer::incremental::full_manifest: the manifest a full index leaves
// (moved from `incremental/mod.rs` unchanged, §4.1 file cap).

use super::{check_total_bytes, discover};
use crate::indexer::manifest::{self, FileManifest, FileState};
use crate::indexer::walk::WalkOptions;
use crate::indexer::IndexOptions;
use std::path::Path;

/// Builds the manifest for `codebase` and writes it to `manifest_path`. Since
/// #478 the full paths call `full_manifest` before indexing and save it after;
/// this one-step form builds fixtures for the tests and the baseline of the
/// `incremental_speed` benchmark (`benchmarks/incremental_speed`). Like any
/// manifest written outside a full index, it records no Cargo map, so an
/// `analyze_codebase` over it rebuilds.
///
/// Preconditions: `codebase` is the tree a full index under the same
/// `options` (`language_filter`/`dependency_scope`/`exclude_dirs`) built the
/// graph from, unchanged since; `manifest_path`'s parent exists. Postcondition on `Ok`: the manifest
/// records (mtime, size, content_hash) for every file the full index visited,
/// keyed by the File node id. Best-effort per file: a stat/read failure
/// records a zeroed/empty state rather than aborting the whole index (the
/// file simply re-classifies as changed next time).
///
/// Walk-level gaps (excluded/unreadable directories, issue #249) are
/// discarded here — the caller (a just-completed full index) already wrote
/// the coverage sidecar from its own walk; this re-walk exists only to build
/// the manifest baseline for the NEXT incremental call.
#[allow(dead_code)] // see the re-export in `indexer/mod.rs`
pub fn write_full_manifest(
    codebase: &Path,
    manifest_path: &Path,
    options: &IndexOptions,
) -> Result<(), String> {
    manifest::save(manifest_path, &full_manifest(codebase, options)?)
}

/// The manifest a full index of `codebase` under `options` leaves, built from
/// the tree as it is now. A full rebuild takes it BEFORE indexing and saves it
/// after (review of #478): a file added, edited or deleted while the index
/// runs then differs from this snapshot, so the next reuse check sees it,
/// instead of a manifest written after the index vouching for a file the
/// graph never parsed.
pub fn full_manifest(codebase: &Path, options: &IndexOptions) -> Result<FileManifest, String> {
    full_manifest_within(codebase, options, crate::indexer::MAX_TOTAL_BYTES)
}

/// `full_manifest` under an explicit byte cap, so a caller's test can reach
/// the refusal without a 2 GiB tree.
pub fn full_manifest_within(
    codebase: &Path,
    options: &IndexOptions,
    cap: u64,
) -> Result<FileManifest, String> {
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope: options.dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, _walk_gaps, _walk_pruned) = discover(codebase, walk_opts)?;
    // Taken before indexing, so the indexer's own cap has not run yet: refuse
    // a tree over it before hashing anything (Codex review of #478).
    check_total_bytes(&current, cap)?;
    let mut m = FileManifest::new();
    // A full index's manifest describes a graph built with exactly `options`.
    m.scope = Some(options.scope());
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
    Ok(m)
}

#[cfg(test)]
#[path = "full_manifest_tests.rs"]
mod full_manifest_tests;
